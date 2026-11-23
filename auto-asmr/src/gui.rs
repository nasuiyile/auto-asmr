//! 图形界面（egui）：实时页 + 文件转换页，右侧是两种模式共用的效果参数。

use crate::config::{ConfigError, Saved, Tab};
use crate::dsp::engine::{EngineConfig, Telemetry};
use crate::dsp::mix::{self, MixSetup, Voice, DUO_MAX_MS, MAX_VOICES, PRESETS};
use crate::dsp::params::{MotionMode, Params, Playback, Via, DENOISE_MAX_DB};
use crate::dsp::tables::tables;
use crate::dsp::motion::{lift_tilt, up_angle};
use crate::dsp::{FS, HEAD_A, R_MIN};
mod view3d;

use crate::batch;
use crate::dsp::align::Alignment;
use crate::dsp::balance::{self, BalanceData};
use crate::offline::{align_tracks, balance_data, convert_mix, default_mix_output, default_output, load_mono, Channel, ConvertOptions, ConvertReport, Duo, MixInput, MixOptions};
use crate::realtime::{self, DeviceInfo, DeviceList, Realtime, RtOptions};
use eframe::egui;
use egui::{Color32, RichText};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// 参数表第一列（文字）的最小宽度，各分组的滑块因此对齐。
const COL: f32 = 104.0;

const CONFIG_FILE: &str = "auto-asmr.ron";

/// 项目主页（窗口右下角的"给个star"）。
const GITHUB_URL: &str = "https://github.com/nasuiyile/auto-asmr";

/// 多音频合成时各音源的颜色：文件列表、位置图、3D 视图里一致。
const VOICE_COLORS: [Color32; MAX_VOICES] = [Color32::from_rgb(255, 120, 40), Color32::from_rgb(70, 150, 240), Color32::from_rgb(70, 180, 100), Color32::from_rgb(190, 100, 225)];

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 左右移动方式的名字。
fn motion_name(m: MotionMode) -> &'static str {
    match m {
        MotionMode::Sweep => t("穿梭", "sweep"),
        MotionMode::Random => t("随机走位", "random"),
        MotionMode::Orbit => t("环绕", "orbit"),
        MotionMode::Static => t("固定", "static"),
        MotionMode::Manual => t("手动", "manual"),
    }
}

/// 合成音源的位置名：预设位置用它的名字，拖出来的位置用方位角。
fn voice_name(v: &Voice) -> String {
    match v.preset_index() {
        Some(k) => t(PRESETS[k].0, PRESETS[k].1).to_string(),
        None => format!("{:.0}°", v.az),
    }
}

/// 位置图的半径刻度：到头中心的距离（米）→ 离图中心的比例（0–1）。对数压缩：2 cm 在中心，80 cm 在边上。
fn radar_map(r: f32) -> f32 {
    ((r / 0.02).max(1.0).ln() / (RADAR_MAX_R / 0.02).ln()).clamp(0.0, 1.0)
}

/// 位置图上的一点（相对图中心的偏移；`rmax` = 满刻度对应的像素数）→（方位角 度, 到头中心的距离 米）。
/// 距离限制在头外面（不会拖进头里）到图的边缘之间。
fn radar_pick(d: egui::Vec2, rmax: f32) -> (f32, f32) {
    let az = d.x.atan2(-d.y).to_degrees().rem_euclid(360.0);
    let r = 0.02 * ((RADAR_MAX_R / 0.02).ln() * (d.length() / rmax).clamp(0.0, 1.0)).exp();
    (az, r.clamp(R_MIN, RADAR_MAX_R))
}

/// 位置图边缘对应的距离（米）。
const RADAR_MAX_R: f32 = 0.8;

/// 输出增益的说明（左下角降噪下面那一行）。
const OUT_GAIN_TIP_ZH: &str = "在自动响度之后、峰值限制器之前整体加减音量，相当于耳机音量旋钮。只在实时 / 试听时起作用；转换文件不用它，文件的响度精确归一化到「目标响度」。往上推到峰值上限后只会被限制器压扁，不会更响";
const OUT_GAIN_TIP_EN: &str = "Overall gain after auto level and before the peak limiter, like a headphone volume knob. Real-time / preview only; file conversion ignores it and normalizes exactly to the target loudness. Above the ceiling the limiter only squashes it, it won't get louder";

/// 参数表的一行：滑轨用常用范围，手输或拖动数值可以超出该范围。
fn row(ui: &mut egui::Ui, label: &str, tip: &str, w: egui::Slider<'_>) -> bool {
    let l = ui.label(label);
    if !tip.is_empty() {
        l.on_hover_text(tip);
    }
    let changed = ui.add(w.clamping(egui::SliderClamping::Never))
        .on_hover_text(t("点击数字可手动输入，按 Enter 确认；允许超出滑块范围", "Click the number to type a value, then press Enter; values may exceed the slider range"))
        .changed();
    ui.end_row();
    changed
}

/// 界面文字：中文 / 英文二选一。
fn t(zh: &'static str, en: &'static str) -> &'static str {
    if ENGLISH.load(Ordering::Relaxed) {
        en
    } else {
        zh
    }
}

/// 列表里一个文件的转换状态。
#[derive(Clone)]
enum ItemStatus {
    Waiting,
    Running(f32, String),
    Done(Box<ConvertReport>, PathBuf),
    Failed(String),
}

/// 一次转换（单个文件或一批文件）。工作线程更新 `status`，界面每帧读一次。
struct Job {
    paths: Vec<PathBuf>,
    status: Arc<Vec<Mutex<ItemStatus>>>,
    /// 每个文件的估计时长（秒），用来给总进度加权；后台线程估计好之前为空（按等权算）。
    est: Arc<Mutex<Vec<f64>>>,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    workers: usize,
    t0: std::time::Instant,
    /// 多音频合成的任务：参与合成的音频和位置（写进完成后的说明）；普通转换为 None。
    mix: Option<String>,
}

type Decoded = Result<(PathBuf, u8, Arc<Vec<f32>>), String>;

/// 合成试听用的解码结果：参与合成的文件、取的声道、各段 48 kHz 单声道、各段的响度（对齐用），
/// 以及自动对齐的结果（打开"自动对齐"后才算；对不上时是错误说明，按没对齐的原样播放）。
struct MixAudio {
    paths: Vec<PathBuf>,
    ch: u8,
    data: Vec<Arc<Vec<f32>>>,
    lufs: Vec<Option<f32>>,
    /// 各段每 10 ms 的 K 加权均方（自动平衡用）。
    levels: Vec<Arc<Vec<f32>>>,
    aligned: Option<Result<AlignedMix, String>>,
}

/// 自动对齐、裁剪之后的各段（和"合成并保存"用的是同一个函数算的），及其响度。
struct AlignedMix {
    al: Alignment,
    data: Vec<Arc<Vec<f32>>>,
    lufs: Vec<Option<f32>>,
    levels: Vec<Arc<Vec<f32>>>,
}

/// 试听用的一组音频：各段、响度、10 ms 均方（都是共享的，复制很便宜）。
struct MixTracks {
    data: Vec<Arc<Vec<f32>>>,
    lufs: Vec<Option<f32>>,
    levels: Vec<Arc<Vec<f32>>>,
}

impl AlignedMix {
    fn new(paths: &[PathBuf], data: &[Arc<Vec<f32>>]) -> Result<Self, String> {
        let refs: Vec<&[f32]> = data.iter().map(|d| d.as_slice()).collect();
        let names: Vec<String> = paths.iter().map(|p| file_name(p)).collect();
        let (out, al) = align_tracks(&refs, &names)?;
        let lufs = out.iter().map(|m| mix::loudness(m)).collect();
        let levels = out.iter().map(|m| Arc::new(balance::levels(m))).collect();
        Ok(AlignedMix { al, data: out.into_iter().map(Arc::new).collect(), lufs, levels })
    }
}

impl MixAudio {
    /// 试听用的各段和响度：`align` 时用对齐后的（还没算好时为 None；对不上时用原样的），否则用原样的。
    fn tracks(&self, align: bool) -> Option<MixTracks> {
        match (&self.aligned, align) {
            (None, true) => None,
            (Some(Ok(a)), true) => Some(MixTracks { data: a.data.clone(), lufs: a.lufs.clone(), levels: a.levels.clone() }),
            _ => Some(MixTracks { data: self.data.clone(), lufs: self.lufs.clone(), levels: self.levels.clone() }),
        }
    }
}

/// 合成解码失败时带上是哪一组文件、哪个声道，免得界面对同一组反复重试。
type MixDecoded = Result<MixAudio, (Vec<PathBuf>, u8, String)>;

pub struct App {
    s: Saved,
    cfg: Arc<EngineConfig>,
    sent: Params,
    hosts: Vec<(cpal::HostId, String)>,
    devices: DeviceList,
    rt: Option<Realtime>,
    rt_preview: bool,
    status: String,
    /// 文件页的待转换列表；`file_in` 是其中选中用来试听的那个。
    files: Vec<PathBuf>,
    statuses: HashMap<PathBuf, ItemStatus>,
    file_in: Option<PathBuf>,
    file_out: Option<PathBuf>,
    start_s: f32,
    dur_s: f32,
    preview_s: f64,
    preview_dragging: bool,
    preview_pending: bool,
    job: Option<Job>,
    report: String,
    decoded: Option<(PathBuf, u8, Arc<Vec<f32>>)>,
    decoding: Option<Receiver<Decoded>>,
    tele: Telemetry,
    trail: Vec<(f32, f32)>,
    view3d: view3d::View3d,
    /// 最近一次用的自动模式（关掉"手动摆位"时回到它）、上一帧的模式。
    auto_mode: MotionMode,
    /// 进手动摆位之前的"响度随位置变化"（风格预设可能定过自己的值），取消手动摆位时恢复。
    auto_level_follow: f32,
    last_mode: MotionMode,
    shot: Option<(PathBuf, u32)>,
    /// 截图自检时模拟的一次拖动：按下点和松开点（窗口坐标），以及是否用右键。
    shot_drag: Option<[f32; 4]>,
    shot_drag_right: bool,
    shot_drag_frame: u32,
    demo: Option<PathBuf>,
    /// 截图自检要等多少帧（AUTO_ASMR_SHOT_WAIT 可改）。
    shot_wait: Option<u32>,
    // ---- 多音频合成
    /// 每个音频的位置和音量（按文件记：删掉别的文件、列表顺序变了都不影响）。
    voices: HashMap<PathBuf, Voice>,
    /// 左右双人的两个声音的位置和音量（默认左耳、右耳）。
    duo_voices: [Voice; 2],
    mix_audio: Option<MixAudio>,
    /// 勾上自动对齐时对齐还没算好：算好后把播放位置换到对齐后的时间线（停在同一处音乐）。
    mix_shift: bool,
    /// 自动平衡的输入响度数据，以及它是按哪一组音频（第 1 段数据的地址）算的。
    mix_bal: Option<(usize, Arc<BalanceData>)>,
    mix_decoding: Option<Receiver<MixDecoded>>,
    /// 解码失败过的那一组（文件, 声道）：不自动重试，点"试听"时再试。
    mix_failed: Option<(Vec<PathBuf>, u8)>,
    /// 合成的输出文件；None = 默认（第一个音频旁边，名字加 _mix_asmr）。
    mix_out: Option<PathBuf>,
    /// 正在运行的试听是合成试听。
    rt_mix: bool,
    /// 最近一次交给音频线程的各音源位置和合成配置（参数或位置变了才重发）。
    mix_sent: Option<(Vec<Voice>, Arc<MixSetup>)>,
    /// 合成试听时各音源此刻的 [方位角, 仰角, 距离, 说话活动]，以及位置图上的轨迹。
    mix_tele: Vec<[f32; 4]>,
    mix_trails: Vec<Vec<(f32, f32)>>,
    /// 位置图上正在拖动的音源、最近选中的音源（读数显示它）。
    mix_drag: Option<usize>,
    mix_sel: Option<usize>,
}

/// 窗口和任务栏图标：戴耳机的白色小猫（assets/icon.png，由 icon/make_icons.py 生成）。读不出来时用 egui 默认图标。
fn window_icon() -> std::sync::Arc<egui::IconData> {
    let decode = || -> Option<egui::IconData> {
        let mut dec = png::Decoder::new(std::io::Cursor::new(include_bytes!("../assets/icon.png").as_slice()));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
        let mut reader = dec.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return None;
        }
        buf.truncate(info.buffer_size());
        Some(egui::IconData { rgba: buf, width: info.width, height: info.height })
    };
    std::sync::Arc::new(decode().unwrap_or_default())
}

/// `screenshot`：(PNG 路径, 是否切到文件页, 演示文件)——自检用：打开界面、可选地试听演示文件几秒、截图后退出。
pub fn run(screenshot: Option<(PathBuf, bool, Option<PathBuf>)>) -> Result<(), String> {
    let config_path = std::env::current_dir().map_err(|e| format!("无法获取当前运行目录：{e}"))?.join(CONFIG_FILE);
    let opts = eframe::NativeOptions {
        persistence_path: Some(config_path),
        viewport: egui::ViewportBuilder::default().with_inner_size([1260.0, 840.0]).with_min_inner_size([1040.0, 640.0]).with_title("auto-asmr · KU100 双耳 ASMR").with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native("auto-asmr", opts, Box::new(move |cc| Ok(Box::new(App::new(cc, screenshot))))).map_err(|e| e.to_string())
}

impl App {
    fn export_config(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(t("配置文件", "Configuration"), &["ron"])
            .set_file_name("auto-asmr-export.ron")
            .save_file() else { return };
        // 和自动保存使用相同的 eframe 键值格式，可直接替换运行目录中的配置文件；安卓版也能导入。
        let result = crate::config::to_text(&self.s).and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
        self.status = match result {
            Ok(()) => format!("{}：{}", t("配置已导出", "Configuration exported"), path.display()),
            Err(e) => format!("{}：{e}", t("配置导出失败", "Configuration export failed")),
        };
    }

    fn import_config(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(t("配置文件", "Configuration"), &["ron"])
            .pick_file() else { return };
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| parse_config(&text)) {
            Ok(s) => {
                // 先停掉正在运行的实时 / 试听（设备、缓冲可能都变了），再整体换成导入的设置
                self.stop();
                let mode = s.params.mode;
                self.s = s;
                self.last_mode = mode;
                self.auto_mode = if mode == MotionMode::Manual { MotionMode::Sweep } else { mode };
                self.auto_level_follow = if mode == MotionMode::Manual { Params::default_level_follow(MotionMode::Sweep) } else { self.s.params.level_follow };
                self.reload_devices();
                // 参数由 sync_params 和 self.sent 比较后提交给引擎
                self.status = format!("{}：{}", t("配置已导入", "Configuration imported"), path.display());
            }
            Err(e) => self.status = format!("{}：{e}", t("配置导入失败", "Configuration import failed")),
        }
    }

    fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<(PathBuf, bool, Option<PathBuf>)>) -> Self {
        let mut s: Saved = cc.storage.and_then(|st| eframe::get_value(st, eframe::APP_KEY)).unwrap_or_default();
        let mut demo = None;
        let shot = screenshot.map(|(p, file_tab, demo_file)| {
            // 截图自检：用默认设置，保证每次画面一致；要看别的状态时，用环境变量 AUTO_ASMR_SHOT_PARAMS
            // 给出参数（RON，只写要改的项，如 "(denoise:true,denoise_db:12.0)"）
            s = Saved::default();
            if let Some(p) = std::env::var("AUTO_ASMR_SHOT_PARAMS").ok().and_then(|v| ron::from_str::<Params>(&v).ok()) {
                s.params = p;
            }
            if file_tab {
                s.tab = Tab::File;
            }
            if demo_file.is_some() {
                // 演示时不出声：输出压到 −60 dB
                s.params.out_gain_db = -60.0;
                s.params.auto_level = false;
            }
            demo = demo_file;
            (p, 0)
        });
        let hosts = realtime::hosts();
        let host_id = hosts.iter().find(|h| h.1 == s.host).map(|h| h.0);
        let devices = realtime::list_devices(host_id);
        let cfg = Arc::new(EngineConfig::new(s.params.clone(), None));
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.slider_width = 150.0;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        cc.egui_ctx.set_global_style(style);
        let s_mode = s.params.mode;
        let s_level_follow = s.params.level_follow;
        let is_shot = shot.is_some();
        let mut app = App {
            sent: s.params.clone(),
            s,
            cfg,
            hosts,
            devices,
            rt: None,
            rt_preview: false,
            status: String::new(),
            files: Vec::new(),
            statuses: HashMap::new(),
            file_in: None,
            file_out: None,
            start_s: 0.0,
            dur_s: 0.0,
            preview_s: 0.0,
            preview_dragging: false,
            preview_pending: false,
            job: None,
            report: String::new(),
            decoded: None,
            decoding: None,
            tele: Telemetry { az: 270.0, r: 0.105, in_db: -120.0, out_db: -120.0, ..Default::default() },
            trail: Vec::new(),
            view3d: view3d::View3d::default(),
            auto_mode: if s_mode == MotionMode::Manual { MotionMode::Sweep } else { s_mode },
            auto_level_follow: if s_mode == MotionMode::Manual { Params::default_level_follow(MotionMode::Sweep) } else { s_level_follow },
            last_mode: s_mode,
            shot,
            shot_drag: std::env::var("AUTO_ASMR_SHOT_DRAG").ok().and_then(|v| {
                let f: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (f.len() == 4).then(|| [f[0], f[1], f[2], f[3]])
            }),
            // AUTO_ASMR_SHOT_DRAG 后面加 ",right"：用右键拖
            shot_drag_right: std::env::var("AUTO_ASMR_SHOT_DRAG").map_or(false, |v| v.trim_end().ends_with("right")),
            shot_drag_frame: 0,
            demo,
            shot_wait: std::env::var("AUTO_ASMR_SHOT_WAIT").ok().and_then(|v| v.trim().parse().ok()),
            voices: HashMap::new(),
            duo_voices: [Voice::preset(0), Voice::preset(1)],
            mix_audio: None,
            mix_shift: false,
            mix_bal: None,
            mix_decoding: None,
            mix_failed: None,
            mix_out: None,
            rt_mix: false,
            mix_sent: None,
            mix_tele: Vec::new(),
            mix_trails: Vec::new(),
            mix_drag: None,
            mix_sel: None,
        };
        if is_shot {
            // AUTO_ASMR_SHOT_MIX=1：打开多音频合成（配合 AUTO_ASMR_SHOT_FILES 预先放进的文件）；AUTO_ASMR_SHOT_EN=1：英文界面
            if std::env::var("AUTO_ASMR_SHOT_MIX").is_ok() {
                app.s.mix = true;
            }
            // AUTO_ASMR_SHOT_FOLLOW=1：合成用"一起移动"
            if std::env::var("AUTO_ASMR_SHOT_FOLLOW").is_ok() {
                app.s.mix_follow = true;
            }
            // AUTO_ASMR_SHOT_ALIGN=1：合成前自动对齐
            if std::env::var("AUTO_ASMR_SHOT_ALIGN").is_ok() {
                app.s.mix_align = true;
            }
            // AUTO_ASMR_SHOT_BG=<0–100>：背景音按雨夜打开，音量取这个值
            if let Some(v) = std::env::var("AUTO_ASMR_SHOT_BG").ok().and_then(|v| v.trim().parse::<f32>().ok()) {
                app.s.params = app.s.params.clone().bg_rain_night();
                app.s.params.bg_level = v.clamp(0.0, 100.0);
            }
            // AUTO_ASMR_SHOT_DUO=<毫秒>：左右双人
            if let Some(v) = std::env::var("AUTO_ASMR_SHOT_DUO").ok().and_then(|v| v.trim().parse::<f32>().ok()) {
                app.s.duo_ms = v.clamp(0.0, DUO_MAX_MS);
            }
            // AUTO_ASMR_SHOT_RT_DUO=<毫秒>：实时（主播）页的左右双人
            if let Some(v) = std::env::var("AUTO_ASMR_SHOT_RT_DUO").ok().and_then(|v| v.trim().parse::<f32>().ok()) {
                app.s.rt_duo_ms = v.clamp(0.0, DUO_MAX_MS);
            }
            // AUTO_ASMR_SHOT_BALANCE=<0–100>：合成的自动平衡强度
            if let Some(v) = std::env::var("AUTO_ASMR_SHOT_BALANCE").ok().and_then(|v| v.trim().parse::<f32>().ok()) {
                app.s.mix_balance = v.clamp(0.0, 100.0);
            }
            if std::env::var("AUTO_ASMR_SHOT_EN").is_ok() {
                app.s.english = true;
            }
            // AUTO_ASMR_SHOT_3D="偏航,俯仰"（可留空用默认视角）：打开 3D 观察窗
            if let Ok(v) = std::env::var("AUTO_ASMR_SHOT_3D") {
                app.s.view3d = true;
                let f: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if f.len() == 2 {
                    (app.view3d.yaw, app.view3d.pitch) = (f[0], f[1]);
                }
            }
            // 截图自检批量转换：AUTO_ASMR_SHOT_FILES="a;b;c" 预先放进文件列表；
            // AUTO_ASMR_SHOT_CONVERT=<输出文件夹> 再以每个文件 600 秒开始全部转换，截下转换中的画面（退出时取消）
            if let Ok(v) = std::env::var("AUTO_ASMR_SHOT_FILES") {
                app.add_files(v.split(';').filter(|x| !x.is_empty()).map(PathBuf::from).collect(), false);
            }
            if let Ok(d) = std::env::var("AUTO_ASMR_SHOT_CONVERT") {
                app.s.out_dir = Some(PathBuf::from(d));
                app.dur_s = 600.0;
                app.start_convert();
            }
        }
        app
    }

    fn host_id(&self) -> Option<cpal::HostId> {
        self.hosts.iter().find(|h| h.1 == self.s.host).map(|h| h.0)
    }

    fn rt_options(&self) -> RtOptions {
        RtOptions { host: self.host_id(), input: self.s.input.clone(), output: self.s.output.clone(), buffer_frames: self.s.buffer, input_channel: self.s.in_channel }
    }

    fn reload_devices(&mut self) {
        self.hosts = realtime::hosts();
        self.devices = realtime::list_devices(self.host_id());
    }

    fn stop(&mut self) {
        if self.rt_preview && !self.preview_dragging {
            if let Some(rt) = &self.rt {
                self.preview_s = rt.shared.play_pos.load(Ordering::Relaxed) as f64 / FS as f64;
            }
        }
        self.rt = None;
        self.rt_preview = false;
        self.rt_mix = false;
        self.mix_sent = None;
        self.mix_tele.clear();
        self.mix_trails.clear();
        self.preview_pending = false;
    }

    // ------------------------------------------------------------------ 多音频合成

    /// 多音频合成是否生效：勾上了，并且列表里至少有两个音频。
    fn multi(&self) -> bool {
        self.s.mix && self.files.len() >= 2
    }

    /// 左右双人是否生效：没在多音频合成、滑条不为 0、选中了一个音频。
    fn duo(&self) -> bool {
        !self.multi() && self.s.duo_ms > 0.0 && self.file_in.is_some()
    }

    /// 实时（主播）页的左右双人是否打开：麦克风的声音一左一右两个人说。
    fn mic_duo(&self) -> bool {
        self.s.tab == Tab::Realtime && self.s.rt_duo_ms > 0.0
    }

    /// 试听、位置图按"几个声音合成"来：多音频合成，或左右双人（同一个音频当成两个声音）。
    fn mixing(&self) -> bool {
        self.multi() || self.duo()
    }

    /// 自动对齐只用于多音频合成（左右双人的两个声音本来就是同一段）。
    fn align_on(&self) -> bool {
        self.multi() && self.s.mix_align
    }

    /// 各声音比时间线晚多少个样本：左右双人的第 2 个声音晚 `duo_ms`（实时页是 `rt_duo_ms`），其余都是 0。
    fn mix_delays(&self) -> [usize; MAX_VOICES] {
        let mut d = [0; MAX_VOICES];
        if self.mic_duo() {
            d[1] = mix::delay_samples(self.s.rt_duo_ms);
        } else if self.duo() {
            d[1] = mix::delay_samples(self.s.duo_ms);
        }
        d
    }

    /// 转换时的左右双人设置（不是左右双人时 None）。
    fn duo_options(&self) -> Option<Duo> {
        (!self.multi() && self.s.duo_ms > 0.0).then(|| Duo { delay_ms: self.s.duo_ms, voices: self.duo_voices, follow: self.s.mix_follow, balance: self.s.mix_balance / 100.0 })
    }

    /// 改第 k 个声音的位置 / 音量：多音频合成按文件记，左右双人（文件和实时共用）记在自己的两个位置里。
    fn set_mix_voice(&mut self, k: usize, v: Voice) {
        if self.mic_duo() || self.duo() {
            if let Some(d) = self.duo_voices.get_mut(k) {
                *d = v;
            }
        } else if let Some(p) = self.files.get(k).cloned() {
            self.voices.insert(p, v);
        }
    }

    /// 改左右双人的延迟（0 = 关）。试听中开 / 关时按新的方式从当前位置重新开始；只是改延迟时实时生效（淡过去）。
    fn set_duo(&mut self, ms: f32) {
        let was = self.duo();
        self.s.duo_ms = ms.clamp(0.0, DUO_MAX_MS);
        if self.duo() == was {
            return;
        }
        self.mix_trails.clear();
        self.mix_sel = None;
        if self.rt.is_some() && self.rt_preview {
            if let Some(rt) = &self.rt {
                self.preview_s = rt.shared.play_pos.load(Ordering::Relaxed) as f64 / FS as f64;
            }
            self.start_preview();
        }
    }

    /// 改实时（主播）页左右双人的延迟（0 = 关）。运行中开 / 关时按新的方式重新开始（单个引擎 ↔ 两个声音）；
    /// 只是改延迟时实时生效（淡过去）。
    fn set_rt_duo(&mut self, ms: f32) {
        let was = self.mic_duo();
        self.s.rt_duo_ms = ms.clamp(0.0, DUO_MAX_MS);
        if self.mic_duo() == was {
            return;
        }
        self.mix_trails.clear();
        self.mix_sel = None;
        if self.rt.is_some() && !self.rt_preview {
            self.start_mic();
        }
    }

    /// 位置图、右侧参数是否按合成显示：文件页的多音频合成 / 左右双人，或实时页的左右双人。
    fn mix_view(&self) -> bool {
        (self.s.tab == Tab::File && self.mixing()) || self.mic_duo()
    }

    /// 参与合成的音频：列表里的前 [`MAX_VOICES`] 个；左右双人是选中的那个音频两次。
    fn mix_paths(&self) -> Vec<PathBuf> {
        if let Some(p) = self.file_in.as_ref().filter(|_| self.duo()) {
            return vec![p.clone(), p.clone()];
        }
        self.files.iter().take(MAX_VOICES).cloned().collect()
    }

    /// 参与合成的每个音频的位置和音量。
    fn mix_voices(&self) -> Vec<Voice> {
        if self.mic_duo() || self.duo() {
            return self.duo_voices.to_vec();
        }
        let mut used: Vec<Voice> = Vec::new();
        self.files
            .iter()
            .take(MAX_VOICES)
            .map(|p| {
                let v = self.voices.get(p).copied().unwrap_or_else(|| mix::default_voice(&used));
                used.push(v);
                v
            })
            .collect()
    }

    /// 给新进入合成的音频分配位置：按"左耳、右耳、正前、正后"取第一个空着的方向（两个音频时一左一右）。
    fn ensure_voices(&mut self) {
        let paths: Vec<PathBuf> = self.files.iter().take(MAX_VOICES).cloned().collect();
        let mut used: Vec<Voice> = paths.iter().filter_map(|p| self.voices.get(p).copied()).collect();
        for p in paths {
            if !self.voices.contains_key(&p) {
                let v = mix::default_voice(&used);
                used.push(v);
                self.voices.insert(p, v);
            }
        }
    }

    /// 合成的输出文件。
    fn mix_output(&self) -> PathBuf {
        self.mix_out.clone().unwrap_or_else(|| self.files.first().map(|p| default_mix_output(p)).unwrap_or_default())
    }

    /// 由当前参数和各音源的位置、音量、排布算出交给引擎的合成配置。`lufs`：各音源的响度（对齐用）。
    fn mix_setup(&self, lufs: &[Option<f32>]) -> (Vec<Voice>, Arc<MixSetup>) {
        let voices = self.mix_voices();
        let prev = self.mix_sent.as_ref().map(|(_, s)| s.as_ref());
        let setup = MixSetup::new(&self.cfg, &voices, lufs, self.s.mix_follow, prev).with_balance(self.cur_balance(), self.s.mix_balance / 100.0).with_delays(&self.mix_delays());
        (voices, Arc::new(setup))
    }

    /// 当前合成用的自动平衡数据（强度为 0、或音频还没准备好时没有）。
    fn cur_balance(&self) -> Option<Arc<BalanceData>> {
        // 实时页的左右双人：输入响度由混音器边听边算，不用这份事先算好的数据
        if self.s.mix_balance <= 0.0 || self.mic_duo() {
            return None;
        }
        let t = self.mix_audio.as_ref().filter(|m| m.paths == self.mix_paths() && m.ch == self.s.file_channel)?.tracks(self.align_on())?;
        let key = Arc::as_ptr(t.data.first()?) as usize;
        self.mix_bal.as_ref().filter(|(k, _)| *k == key).map(|(_, b)| b.clone())
    }

    /// 每帧：自动平衡的输入响度数据跟当前这组音频（对齐与否）不符时重算（由 10 ms 均方算，很快）。强度不影响它。
    fn sync_balance(&mut self) {
        if !self.mixing() || self.s.mix_balance <= 0.0 || self.mic_duo() {
            return;
        }
        let Some(t) = self.mix_audio.as_ref().filter(|m| m.paths == self.mix_paths() && m.ch == self.s.file_channel).and_then(|m| m.tracks(self.align_on())) else { return };
        let Some(first) = t.data.first() else { return };
        let key = Arc::as_ptr(first) as usize;
        if self.mix_bal.as_ref().map_or(false, |(k, _)| *k == key) {
            return;
        }
        let lv: Vec<&[f32]> = t.levels.iter().map(|l| l.as_slice()).collect();
        self.mix_bal = Some((key, balance_data(&lv, &t.lufs)));
    }

    /// 合成时各音源此刻的位置 [方位角, 仰角, 距离, 说话活动]：试听中用引擎报告的；没在试听时，各自固定是各自摆好的
    /// 位置（带上"高度"），一起移动是开始时的队形（走位的起点，再错开 360°/N）。
    fn mix_positions(&self) -> Vec<[f32; 4]> {
        let active = self.rt.is_some() && self.rt_mix;
        let p = &self.s.params;
        let voices = self.mix_voices();
        let start = self.s.mix_follow.then(|| crate::dsp::motion::Motion::new(p.seed).step(p, 0.0, 1.0, 0.0));
        let offsets = mix::follow_offsets(voices.len(), self.s.mix_follow);
        voices
            .iter()
            .enumerate()
            .map(|(k, v)| match (self.mix_tele.get(k).filter(|_| active), start) {
                (Some(q), _) => *q,
                (None, Some(s)) => [(s.az + offsets[k]).rem_euclid(360.0), s.el, s.r, 0.0],
                (None, None) => {
                    let (a, e, r) = lift_tilt(v.az, v.distance(p.ear_dist), 0.0, p.height.clamp(-80.0, 85.0));
                    [a, e, r, 0.0]
                }
            })
            .collect()
    }

    /// 切换合成的排布（各自固定 / 一起移动）。合成试听中切换时从当前位置重新开始（几个引擎的走位要从头对齐）；
    /// 主播模式的左右双人也重新开始。
    fn set_mix_follow(&mut self, follow: bool) {
        if follow == self.s.mix_follow {
            return;
        }
        self.s.mix_follow = follow;
        self.mix_trails.clear();
        if self.rt_mix && self.rt.is_some() {
            if self.rt_preview {
                self.start_mix_preview();
            } else {
                self.start_mic();
            }
        }
    }

    /// 打开 / 关闭多音频合成。
    fn set_mix_mode(&mut self, on: bool) {
        self.s.mix = on;
        self.stop();
        if on {
            self.ensure_voices();
        } else if self.file_in.is_some() && self.decoded.is_none() && self.decoding.is_none() {
            self.decode_file();
        }
    }

    /// 后台解码参与合成的几个文件（同时解码），并测各自的响度。
    fn decode_mix(&mut self) {
        let paths = self.mix_paths();
        let ch = self.s.file_channel;
        if self.duo() {
            // 左右双人：两个声音是同一个文件，直接用单个文件的解码结果（还没有就先解码它，好了再来），只补算响度
            let Some(p) = paths.first().cloned() else { return };
            match self.decoded.as_ref().filter(|(q, c, _)| *q == p && *c == ch) {
                Some((_, _, a)) => {
                    let a = a.clone();
                    let (tx, rx) = channel();
                    std::thread::spawn(move || {
                        let l = mix::loudness(&a);
                        let lv = Arc::new(balance::levels(&a));
                        let _ = tx.send(Ok(MixAudio { paths, ch, data: vec![a.clone(), a], lufs: vec![l, l], levels: vec![lv.clone(), lv], aligned: None }));
                    });
                    self.mix_decoding = Some(rx);
                }
                None if self.decoding.is_none() => self.decode_file(),
                None => {}
            }
            return;
        }
        let chan = self.file_channel();
        let align = self.align_on();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let res: Vec<Result<(Arc<Vec<f32>>, Option<f32>, Arc<Vec<f32>>), String>> = std::thread::scope(|s| {
                let hs: Vec<_> = paths
                    .iter()
                    .map(|p| {
                        s.spawn(move || {
                            let mut o = ConvertOptions::new(p.clone());
                            o.channel = chan;
                            let (m, _) = load_mono(&o, &AtomicBool::new(false)).map_err(|e| format!("{}：{e}", file_name(p)))?;
                            let l = mix::loudness(&m);
                            let lv = Arc::new(balance::levels(&m));
                            Ok((Arc::new(m), l, lv))
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("解码出错".to_string()))).collect()
            });
            let r: MixDecoded = match res.into_iter().collect::<Result<Vec<_>, String>>() {
                Ok(v) => {
                    let mut data = Vec::new();
                    let mut lufs = Vec::new();
                    let mut levels = Vec::new();
                    for (d, l, lv) in v {
                        data.push(d);
                        lufs.push(l);
                        levels.push(lv);
                    }
                    let aligned = align.then(|| AlignedMix::new(&paths, &data));
                    Ok(MixAudio { paths, ch, data, lufs, levels, aligned })
                }
                Err(e) => Err((paths, ch, e)),
            };
            let _ = tx.send(r);
        });
        self.mix_decoding = Some(rx);
        self.status = if align { t("正在解码要合成的音频并自动对齐…", "Decoding and aligning the files to mix…") } else { t("正在解码要合成的音频…", "Decoding the files to mix…") }.to_string();
    }

    /// 已经解码好、还没对齐过：在后台对齐（不用重新解码），结果和解码结果走同一个通道。
    fn align_mix(&mut self) {
        let Some(m) = &self.mix_audio else { return };
        let (paths, ch, data, lufs, levels) = (m.paths.clone(), m.ch, m.data.clone(), m.lufs.clone(), m.levels.clone());
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let aligned = Some(AlignedMix::new(&paths, &data));
            let _ = tx.send(Ok(MixAudio { paths, ch, data, lufs, levels, aligned }));
        });
        self.mix_decoding = Some(rx);
        self.status = t("正在自动对齐…", "Aligning…").to_string();
    }

    /// 打开 / 关闭合成前的自动对齐。合成试听中切换时按新的时间线重新开始（对齐要算的话算完接着播）。
    fn set_mix_align(&mut self, on: bool) {
        if on == self.s.mix_align {
            return;
        }
        self.s.mix_align = on;
        self.mix_trails.clear();
        // 播放条换成另一条时间线：按第 1 段停在同一处音乐（对齐后的时间 = 原来的时间 − 第 1 段裁掉的开头）
        if self.rt_mix && self.rt.is_some() {
            if let Some(rt) = &self.rt {
                self.preview_s = rt.shared.play_pos.load(Ordering::Relaxed) as f64 / FS as f64;
            }
        }
        self.mix_shift = false;
        match self.mix_audio.as_ref().map(|m| &m.aligned) {
            Some(Some(Ok(a))) => {
                let cut = a.al.starts[0] as f64 / FS as f64;
                self.preview_s = if on { (self.preview_s - cut).max(0.0) } else { self.preview_s + cut };
            }
            Some(None) if on => self.mix_shift = true,
            _ => {}
        }
        if self.rt_mix && self.rt.is_some() {
            self.start_mix_preview();
        }
    }

    /// 每帧检查：合成的文件组或声道变了就停掉旧的合成试听、重新解码（之前在播的话解码完接着播）；
    /// 不在合成时释放解码结果。普通试听在合成生效时停掉。
    fn sync_mix(&mut self) {
        if !self.mixing() {
            if self.rt_mix {
                self.stop();
            }
            self.mix_audio = None;
            return;
        }
        if self.rt.is_some() && self.rt_preview && !self.rt_mix {
            self.stop();
        }
        let paths = self.mix_paths();
        let ch = self.s.file_channel;
        if self.mix_audio.as_ref().map_or(false, |m| m.paths == paths && m.ch == ch) {
            // 打开了自动对齐但还没对齐过：在后台先算好（列表下面马上能看到从哪里裁）
            if self.align_on() && self.mix_decoding.is_none() && self.mix_audio.as_ref().map_or(false, |m| m.aligned.is_none()) {
                self.align_mix();
            }
            return;
        }
        let resume = self.rt_mix || self.preview_pending;
        if self.rt_mix {
            self.stop();
        }
        self.preview_pending = resume;
        self.mix_audio = None;
        if self.mix_decoding.is_none() && self.mix_failed.as_ref() != Some(&(paths, ch)) {
            self.decode_mix();
        }
    }

    /// 合成试听：几个音频同时播放，各在自己的位置；先在后台解码（解码过同一组就直接用）。
    fn start_mix_preview(&mut self) {
        let selected_s = self.preview_s;
        self.stop();
        self.preview_s = selected_s;
        self.sync_params();
        let paths = self.mix_paths();
        let ch = self.s.file_channel;
        let decoded = self.mix_audio.as_ref().filter(|m| m.paths == paths && m.ch == ch);
        let have = decoded.is_some();
        let ready = decoded.and_then(|m| m.tracks(self.align_on())).map(|t| {
            let len = t.data.iter().map(|x| x.len()).max().unwrap_or(0);
            (t.data, t.lufs, len)
        });
        self.sync_balance();
        if let Some((data, lufs, len)) = ready {
            let start = ((self.preview_s.max(0.0) * FS as f64) as usize).min(len);
            let (voices, setup) = self.mix_setup(&lufs);
            match Realtime::start_preview_mix(&self.rt_options(), setup.clone(), data, start) {
                Ok(rt) => {
                    self.status = if self.duo() {
                        format!("{}：{}（{} Hz），{} {:.0} ms", t("左右双人试听中", "Previewing two voices"), rt.info.output_name, rt.info.out_rate, t("第 2 个声音晚", "the second one later by"), self.s.duo_ms)
                    } else {
                        format!("{}：{}（{} Hz），{} {}", t("合成试听中", "Previewing the mix"), rt.info.output_name, rt.info.out_rate, voices.len(), t("个音频同时播放", "files at once"))
                    };
                    self.rt = Some(rt);
                    self.rt_preview = true;
                    self.rt_mix = true;
                    self.mix_sent = Some((voices, setup));
                }
                Err(e) => self.status = e,
            }
            return;
        }
        self.preview_pending = true;
        if self.mix_decoding.is_none() {
            if have {
                self.align_mix(); // 解码过了，只差对齐
            } else {
                self.mix_failed = None; // 点了试听：解码失败过的也再试一次
                self.decode_mix();
            }
        }
    }

    /// 合成并保存：参与合成的音频各放各的位置，输出一个文件。
    fn start_mix_convert(&mut self) {
        if self.job.is_some() || !self.multi() {
            return;
        }
        let paths = self.mix_paths();
        let out = self.mix_output();
        let voices = self.mix_voices();
        let inputs: Vec<MixInput> = paths.iter().cloned().zip(voices.iter().copied()).map(|(path, voice)| MixInput { path, voice }).collect();
        let follow = self.s.mix_follow;
        let align = self.s.mix_align;
        let mo = MixOptions { follow, align, balance: self.s.mix_balance / 100.0, ..Default::default() };
        let offsets = mix::follow_offsets(inputs.len(), follow);
        let list = inputs
            .iter()
            .enumerate()
            .map(|(k, i)| {
                let g = if i.voice.gain_db != 0.0 { format!("，{:+.1} dB", i.voice.gain_db) } else { String::new() };
                let pos = if follow { format!("+{:.0}°", offsets[k]) } else { voice_name(&i.voice) };
                format!("{}. {}（{pos}{g}）", k + 1, file_name(&i.path))
            })
            .collect::<Vec<_>>()
            .join("  ");
        let desc = if follow { format!("{}（{}）  {list}", t("一起移动", "moving together"), motion_name(self.s.params.mode)) } else { list };
        let desc = if align { format!("{}  {desc}", t("自动对齐", "auto-aligned")) } else { desc };
        let desc = if mo.balance > 0.0 { format!("{} {:.0}%  {desc}", t("自动平衡", "auto-balance"), self.s.mix_balance) } else { desc };
        let opts = self.convert_options(paths[0].clone(), out.clone());
        let job = Job {
            paths: vec![out.clone()],
            status: Arc::new(vec![Mutex::new(ItemStatus::Waiting)]),
            est: Arc::new(Mutex::new(vec![1.0])),
            cancel: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            workers: inputs.len(),
            t0: std::time::Instant::now(),
            mix: Some(desc),
        };
        let (st, ca, fin, params) = (job.status.clone(), job.cancel.clone(), job.finished.clone(), self.s.params.clone());
        std::thread::spawn(move || {
            let r = convert_mix(&opts, &inputs, &params, mo, ca, |f, stage| {
                if let Ok(mut g) = st[0].lock() {
                    *g = ItemStatus::Running(f, stage.to_string());
                }
            });
            if let Ok(mut g) = st[0].lock() {
                *g = match r {
                    Ok(rep) => ItemStatus::Done(Box::new(rep), out),
                    Err(e) => ItemStatus::Failed(e),
                };
            }
            fin.store(true, Ordering::Release);
        });
        self.report.clear();
        self.job = Some(job);
    }

    fn start_mic(&mut self) {
        self.stop();
        self.sync_params();
        // 左右双人：两个声音都来自麦克风，本来就是同一个声音，不用做响度对齐
        let duo = self.mic_duo().then(|| self.mix_setup(&[None, None]));
        let started = match &duo {
            Some((_, setup)) => Realtime::start_duo(&self.rt_options(), setup.clone()),
            None => Realtime::start(&self.rt_options(), self.cfg.clone()),
        };
        match started {
            Ok(rt) => {
                rt.shared.monitor_dry.store(self.s.dry, Ordering::Relaxed);
                let note = if duo.is_some() { format!("；{} {:.0} ms", t("左右双人，第 2 个声音晚", "two voices, the second one later by"), self.s.rt_duo_ms) } else { String::new() };
                self.status = format!("{}：{} → {}（{} Hz → {} Hz）{note}", t("运行中", "Running"), rt.info.input_name, rt.info.output_name, rt.info.in_rate, rt.info.out_rate);
                self.rt = Some(rt);
                if let Some(d) = duo {
                    self.rt_mix = true;
                    self.mix_sent = Some(d);
                }
            }
            Err(e) => self.status = e,
        }
    }

    fn file_channel(&self) -> Channel {
        match self.s.file_channel {
            1 => Channel::Left,
            2 => Channel::Right,
            3 => Channel::Mix,
            _ => Channel::Auto,
        }
    }

    fn convert_options(&self, input: PathBuf, output: PathBuf) -> ConvertOptions {
        let mut o = ConvertOptions::new(input);
        o.output = output;
        o.start = self.start_s.max(0.0) as f64;
        o.duration = if self.dur_s > 0.0 { Some(self.dur_s as f64) } else { None };
        o.channel = self.file_channel();
        o.bits = self.s.bits;
        o.lufs = self.s.file_lufs;
        o.peak_db = self.s.params.ceiling_db;
        o.duo = self.duo_options();
        o
    }

    /// 同时转换几个文件。
    fn threads(&self) -> usize {
        if self.s.jobs == 0 {
            batch::default_threads()
        } else {
            self.s.jobs
        }
    }

    /// 转换列表里的全部文件：一个文件时按"输出到…"选的位置保存；多个文件时放进输出文件夹（或各自源文件旁边），
    /// 后台按 CPU 核数并行，长的文件先做。
    fn start_convert(&mut self) {
        if self.multi() {
            self.start_mix_convert();
            return;
        }
        let paths = self.files.clone();
        if paths.is_empty() || self.job.is_some() {
            return;
        }
        let outs = if paths.len() == 1 {
            vec![self.file_out.clone().unwrap_or_else(|| default_output(&paths[0]))]
        } else {
            if let Some(d) = &self.s.out_dir {
                if let Err(e) = std::fs::create_dir_all(d) {
                    self.status = format!("{} {}：{e}", t("无法创建输出文件夹", "Cannot create output folder"), d.display());
                    return;
                }
            }
            batch::plan_outputs(&paths, self.s.out_dir.as_deref())
        };
        let items: Vec<ConvertOptions> = paths.iter().cloned().zip(outs).map(|(i, o)| self.convert_options(i, o)).collect();
        let n = items.len();
        let workers = self.threads().clamp(1, n);
        let job = Job {
            paths: paths.clone(),
            status: Arc::new((0..n).map(|_| Mutex::new(ItemStatus::Waiting)).collect()),
            est: Arc::new(Mutex::new(Vec::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            workers,
            t0: std::time::Instant::now(),
            mix: None,
        };
        let (st, est, ca, fin, params) = (job.status.clone(), job.est.clone(), job.cancel.clone(), job.finished.clone(), self.s.params.clone());
        std::thread::spawn(move || {
            let e = batch::estimates(&items);
            if let Ok(mut g) = est.lock() {
                *g = e.clone();
            }
            batch::run(
                &items,
                &e,
                &params,
                workers,
                &ca,
                |i, f, stage| {
                    if let Ok(mut g) = st[i].lock() {
                        *g = ItemStatus::Running(f, stage.to_string());
                    }
                },
                |i, r| {
                    if let Ok(mut g) = st[i].lock() {
                        *g = match r {
                            Ok(rep) => ItemStatus::Done(Box::new(rep), items[i].output.clone()),
                            Err(e) => ItemStatus::Failed(e),
                        };
                    }
                },
            );
            fin.store(true, Ordering::Release);
        });
        for p in &paths {
            self.statuses.insert(p.clone(), ItemStatus::Waiting);
        }
        self.report.clear();
        self.job = Some(job);
    }

    /// 试听：先在后台解码（已解码过同一个文件就直接用），再用实时引擎播放。合成时几个音频一起播。
    fn start_preview(&mut self) {
        if self.mixing() {
            self.start_mix_preview();
            return;
        }
        let Some(input) = self.file_in.clone() else { return };
        let selected_s = self.preview_s;
        self.stop();
        self.preview_s = selected_s;
        self.sync_params();
        let ch = self.s.file_channel;
        if let Some((p, c, audio)) = &self.decoded {
            if *p == input && *c == ch {
                let start = ((self.preview_s.max(0.0) * FS as f64) as usize).min(audio.len());
                match Realtime::start_preview(&self.rt_options(), self.cfg.clone(), audio.clone(), start) {
                    Ok(rt) => {
                        self.status = format!("{}：{}（{} Hz）", t("试听中", "Previewing"), rt.info.output_name, rt.info.out_rate);
                        self.rt = Some(rt);
                        self.rt_preview = true;
                    }
                    Err(e) => self.status = e,
                }
                return;
            }
        }
        self.preview_pending = true;
        if self.decoding.is_none() {
            self.decode_file();
        }
    }

    fn decode_file(&mut self) {
        let Some(input) = self.file_in.clone() else { return };
        let ch = self.s.file_channel;
        let (tx, rx) = channel();
        let mut o = ConvertOptions::new(input.clone());
        o.channel = self.file_channel();
        std::thread::spawn(move || {
            let r = load_mono(&o, &AtomicBool::new(false)).map(|(m, _)| (input, ch, Arc::new(m)));
            let _ = tx.send(r);
        });
        self.decoding = Some(rx);
        self.status = t("正在解码…", "Decoding…").to_string();
    }

    /// 把文件 / 文件夹加进列表（文件夹展开为其中的音频文件），已在列表里的不重复加。
    /// 列表原来是空的、或 `select` 为真时，选中新加的第一个用来试听。
    fn add_files(&mut self, paths: Vec<PathBuf>, select: bool) {
        if self.job.is_some() {
            self.status = t("正在转换，完成后才能修改列表", "Converting; the list can be changed when it finishes").to_string();
            return;
        }
        let mut first = None;
        for p in batch::expand_inputs(&paths) {
            if !self.files.contains(&p) {
                first.get_or_insert(p.clone());
                self.files.push(p);
            }
        }
        self.ensure_voices();
        if let Some(p) = first {
            if select || self.file_in.is_none() {
                self.set_file(p);
            }
        } else if !paths.is_empty() && paths.iter().any(|p| p.is_dir()) {
            self.status = t("文件夹里没有找到新的音频文件", "No new audio files in the folder").to_string();
        }
    }

    fn remove_file(&mut self, i: usize) {
        let p = self.files.remove(i);
        self.statuses.remove(&p);
        // 它的位置空出来，留给补进合成的下一个音频
        self.voices.remove(&p);
        self.ensure_voices();
        self.mix_sel = None;
        if self.file_in.as_ref() == Some(&p) {
            self.stop();
            self.file_in = None;
            self.file_out = None;
            self.decoded = None;
            self.decoding = None;
            self.preview_pending = false;
            if let Some(q) = self.files.get(i.min(self.files.len().saturating_sub(1))).cloned() {
                self.set_file(q);
            }
        }
    }

    fn clear_files(&mut self) {
        self.stop();
        self.files.clear();
        self.statuses.clear();
        self.file_in = None;
        self.file_out = None;
        self.decoded = None;
        self.decoding = None;
        self.preview_pending = false;
        self.report.clear();
        self.voices.clear();
        self.mix_audio = None;
        self.mix_decoding = None;
        self.mix_failed = None;
        self.mix_out = None;
        self.mix_sel = None;
    }

    /// 选中一个文件用来试听（不在列表里就加进去）。合成时不单独解码它（试听的是几个音频的合成）。
    fn set_file(&mut self, p: PathBuf) {
        if !self.files.contains(&p) {
            self.files.push(p.clone());
            self.ensure_voices();
        }
        if self.file_in.as_ref() == Some(&p) {
            return;
        }
        self.stop();
        self.file_out = Some(default_output(&p));
        self.file_in = Some(p);
        self.decoded = None;
        self.preview_s = self.start_s.max(0.0) as f64;
        self.preview_dragging = false;
        if !self.multi() {
            self.decode_file(); // 左右双人也用它（两个声音共用这份解码结果）
        }
    }

    // ------------------------------------------------------------------ 各块界面

    fn device_combo(ui: &mut egui::Ui, id: &str, list: &[DeviceInfo], sel: &mut Option<String>) {
        let def_name = list.iter().find(|d| d.is_default).map(|d| d.name.as_str()).unwrap_or("—");
        let text = match sel {
            None => format!("{}（{}）", t("系统默认", "System default"), def_name),
            Some(id) => list.iter().find(|d| &d.id == id).map(|d| d.name.clone()).unwrap_or_else(|| t("（设备未找到）", "(device not found)").to_string()),
        };
        egui::ComboBox::from_id_salt(id).width(300.0).selected_text(text).show_ui(ui, |ui| {
            ui.selectable_value(sel, None, format!("{}（{}）", t("系统默认", "System default"), def_name));
            for d in list {
                ui.selectable_value(sel, Some(d.id.clone()), &d.name);
            }
        });
    }

    fn ui_devices(&mut self, ui: &mut egui::Ui, with_input: bool) {
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(t("音频设备", "Audio devices")).strong());
            egui::Grid::new("dev").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(t("设备类型", "Host"));
                let mut host = self.s.host.clone();
                let shown = if host.is_empty() { self.hosts.first().map(|h| h.1.clone()).unwrap_or_default() } else { host.clone() };
                egui::ComboBox::from_id_salt("host").width(300.0).selected_text(shown).show_ui(ui, |ui| {
                    for (_, name) in &self.hosts {
                        ui.selectable_value(&mut host, name.clone(), name);
                    }
                });
                if host != self.s.host {
                    self.s.host = host;
                    self.devices = realtime::list_devices(self.host_id());
                }
                ui.end_row();
                if with_input {
                    ui.label(t("输入设备", "Input"));
                    Self::device_combo(ui, "in", &self.devices.inputs, &mut self.s.input);
                    ui.end_row();
                }
                ui.label(t("输出设备", "Output"));
                Self::device_combo(ui, "out", &self.devices.outputs, &mut self.s.output);
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if ui.button(t("重载设备列表", "Reload devices")).clicked() {
                    self.reload_devices();
                }
                ui.label(t("缓冲", "Buffer"));
                egui::ComboBox::from_id_salt("buf").width(70.0).selected_text(if self.s.buffer == 0 { t("默认", "Default").to_string() } else { self.s.buffer.to_string() }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.s.buffer, 0, t("默认", "Default"));
                    for n in [64u32, 128, 256, 512, 1024] {
                        ui.selectable_value(&mut self.s.buffer, n, n.to_string());
                    }
                });
                ui.label(t("帧", "frames")).on_hover_text(t("越小延迟越低；出现爆音就调大", "Smaller = lower latency; increase if you hear crackles"));
                if with_input {
                    ui.label(t("输入声道", "Input ch"));
                    let txt = if self.s.in_channel == usize::MAX { t("混合", "Mix").to_string() } else { (self.s.in_channel + 1).to_string() };
                    egui::ComboBox::from_id_salt("inch").width(60.0).selected_text(txt).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.s.in_channel, 0, "1");
                        ui.selectable_value(&mut self.s.in_channel, 1, "2");
                        ui.selectable_value(&mut self.s.in_channel, usize::MAX, t("混合", "Mix"));
                    });
                }
            });
            if with_input && self.devices.inputs.is_empty() {
                ui.colored_label(Color32::from_rgb(220, 150, 60), t("没有找到输入设备（麦克风）", "No input device (microphone) found"));
            }
        });
    }

    /// 头顶俯视图：声源相对头的位置（上 = 正前方），半径按对数压缩，贴耳的细节看得清。
    /// 右侧的竖条是高度：从耳朵（声源在正前 / 正后方时从头中心）看过去，嘴在上方还是下方多少度。
    /// 多音频合成时每个音源一个彩色圆点：各自固定时各自可以拖；一起移动时跟着走位动（手动时拖任意一个就是挪整组）。
    fn ui_radar(&mut self, ui: &mut egui::Ui, size: f32) {
        const GAUGE_W: f32 = 20.0;
        let mixing = self.mix_view();
        let follow = mixing && self.s.mix_follow;
        let fixed_mix = mixing && !follow;
        let manual = !fixed_mix && self.s.params.mode == MotionMode::Manual;
        let sense = if manual || fixed_mix { egui::Sense::click_and_drag() } else { egui::Sense::hover() };
        let (resp, p) = ui.allocate_painter(egui::vec2(size + GAUGE_W, size), sense);
        let rect = egui::Rect::from_min_size(resp.rect.min, egui::vec2(size, size));
        let c = rect.center();
        let rmax = size * 0.47;
        let map = |r: f32| -> f32 { radar_map(r) * rmax };
        // （方位角, 水平距离）→ 图上的点
        let pos = |az: f32, r: f32| {
            let a = az.to_radians();
            c + egui::vec2(a.sin(), -a.cos()) * map(r)
        };
        // 右侧的高度标尺
        let (gx, g0, g1) = (rect.right() + 0.5 * GAUGE_W - 2.0, rect.top() + 30.0, rect.bottom() - 18.0);
        let gm = 0.5 * (g0 + g1);
        if self.shot.as_ref().map_or(false, |s| s.1 == 2) {
            eprintln!("位置图：中心 ({:.0}, {:.0})，满刻度 {:.0} px；高度标尺 x={:.0}，y={:.0}…{:.0}", c.x, c.y, rmax, gx, g0, g1);
        }
        let on_gauge = |pt: egui::Pos2| pt.x > rect.right();
        if manual {
            // 手动摆位：在俯视图里点 / 拖改方位和距离，在右侧竖条上点 / 拖改高度（双击竖条回到与耳朵齐平）。
            // 合成"一起移动"时抓住离按下点最近的那个音源，拖它就是挪整组（其余音源保持错开的角度）
            let origin = ui.input(|i| i.pointer.press_origin());
            let offsets = if follow { mix::follow_offsets(self.mix_voices().len(), true) } else { Vec::new() };
            if resp.is_pointer_button_down_on() {
                if follow && self.mix_drag.is_none() {
                    if let Some(o) = origin.filter(|&o| !on_gauge(o)) {
                        let cur = self.mix_positions();
                        let d = |q: &[f32; 4]| (pos(q[0], q[2] * q[1].to_radians().cos()) - o).length();
                        self.mix_drag = cur.iter().enumerate().min_by(|a, b| d(a.1).total_cmp(&d(b.1))).map(|(k, _)| k);
                    }
                }
                if let (Some(pt), Some(o)) = (resp.interact_pointer_pos(), origin) {
                    let off = self.mix_drag.and_then(|k| offsets.get(k).copied()).unwrap_or(0.0);
                    let pr = &mut self.s.params;
                    if on_gauge(o) {
                        pr.manual_tilt = ((gm - pt.y) / (0.5 * (g1 - g0)) * 90.0).clamp(-80.0, 85.0);
                    } else {
                        let (az, r) = radar_pick(pt - c, rmax);
                        (pr.manual_az, pr.manual_dist) = ((az - off).rem_euclid(360.0), r);
                    }
                }
            } else if follow {
                self.mix_drag = None;
            }
            if resp.double_clicked() && resp.interact_pointer_pos().map_or(false, on_gauge) {
                self.s.params.manual_tilt = 0.0;
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(if resp.is_pointer_button_down_on() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
            }
            if self.rt.is_none() && !mixing {
                // 没在运行：圆点直接显示摆好的位置
                let pr = &self.s.params;
                let (az, el, r) = lift_tilt(pr.manual_az, pr.manual_dist, 0.0, pr.manual_tilt);
                (self.tele.az, self.tele.el, self.tele.r) = (az, el, r);
            }
        }
        if fixed_mix {
            // 多音频合成（各自固定）：按下时抓住离按下点最近的那个音源，拖到哪里它就摆到哪里（方位和距离）
            let voices = self.mix_voices();
            let ear = self.s.params.ear_dist;
            let home = |v: &Voice| {
                let a = v.az.to_radians();
                c + egui::vec2(a.sin(), -a.cos()) * map(v.distance(ear))
            };
            if resp.is_pointer_button_down_on() {
                if self.mix_drag.is_none() {
                    if let Some(o) = ui.input(|i| i.pointer.press_origin()).filter(|&o| !on_gauge(o)) {
                        self.mix_drag = voices.iter().enumerate().min_by(|a, b| (home(a.1) - o).length().total_cmp(&(home(b.1) - o).length())).map(|(k, _)| k);
                        self.mix_sel = self.mix_drag.or(self.mix_sel);
                    }
                }
                if let (Some(k), Some(pt)) = (self.mix_drag, resp.interact_pointer_pos()) {
                    if let Some(&v0) = voices.get(k) {
                        let (az, r) = radar_pick(pt - c, rmax);
                        self.set_mix_voice(k, Voice { az, dist: Some(r), ..v0 });
                    }
                }
            } else {
                self.mix_drag = None;
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(if resp.is_pointer_button_down_on() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
            }
        }
        let dark = ui.visuals().dark_mode;
        let grid = if dark { Color32::from_gray(70) } else { Color32::from_gray(190) };
        let fg = if dark { Color32::from_gray(200) } else { Color32::from_gray(60) };
        p.rect_filled(resp.rect, 8.0, if dark { Color32::from_gray(24) } else { Color32::from_gray(245) });
        for (rr, label) in [(0.1875, "10 cm"), (0.3875, "30 cm"), (0.6875, "60 cm")] {
            p.circle_stroke(c, map(rr), egui::Stroke::new(1.0, grid));
            let a = 0.25 * std::f32::consts::PI; // 距离刻度写在右下方，不挡左上角的读数
            p.text(c + egui::vec2(a.cos(), a.sin()) * (map(rr) + 2.0), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(10.0), grid);
        }
        // 头、鼻子、耳朵
        let hr = map(HEAD_A);
        p.circle_filled(c, hr, if dark { Color32::from_gray(55) } else { Color32::from_gray(215) });
        p.circle_stroke(c, hr, egui::Stroke::new(1.5, fg));
        p.line_segment([c + egui::vec2(-hr * 0.22, -hr * 0.97), c + egui::vec2(0.0, -hr * 1.28)], egui::Stroke::new(1.5, fg));
        p.line_segment([c + egui::vec2(hr * 0.22, -hr * 0.97), c + egui::vec2(0.0, -hr * 1.28)], egui::Stroke::new(1.5, fg));
        for sx in [-1.0f32, 1.0] {
            p.rect_filled(egui::Rect::from_center_size(c + egui::vec2(sx * hr * 1.06, 0.0), egui::vec2(hr * 0.2, hr * 0.5)), 2.0, fg);
        }
        p.text(c + egui::vec2(-hr * 1.5, hr * 0.9), egui::Align2::RIGHT_TOP, t("左", "L"), egui::FontId::proportional(11.0), grid);
        p.text(c + egui::vec2(hr * 1.5, hr * 0.9), egui::Align2::LEFT_TOP, t("右", "R"), egui::FontId::proportional(11.0), grid);
        // 声源（嘴）和它的轨迹
        let accent = Color32::from_rgb(70, 150, 240);
        // 高度标尺：竖线、与耳朵齐平的刻度、上 / 下
        p.line_segment([egui::pos2(gx, g0), egui::pos2(gx, g1)], egui::Stroke::new(2.0, grid));
        p.line_segment([egui::pos2(gx - 4.0, gm), egui::pos2(gx + 4.0, gm)], egui::Stroke::new(1.5, grid));
        p.text(egui::pos2(gx, g0 - 3.0), egui::Align2::CENTER_BOTTOM, t("上", "up"), egui::FontId::proportional(10.0), grid);
        p.text(egui::pos2(gx, g1 + 2.0), egui::Align2::CENTER_TOP, t("下", "dn"), egui::FontId::proportional(10.0), grid);
        let gauge_y = |up: f32| gm - (up / 90.0).clamp(-1.0, 1.0) * 0.5 * (g1 - g0);
        let hgt_text = |up: f32| if up.abs() < 1.0 { String::new() } else { format!("   {} {:.0}°", if up > 0.0 { t("上", "up") } else { t("下", "down") }, up.abs()) };

        if mixing {
            // 每个音源：彩色实心点 = 此刻的位置（俯视投影），同色的圈 = 摆好的位置（拖它；一起移动时只有手动才有圈），
            // 右侧竖条上是各自的高度
            let active = self.rt.is_some() && self.rt_mix;
            let pr = &self.s.params;
            let voices = self.mix_voices();
            let offsets = mix::follow_offsets(voices.len(), follow);
            let cur = self.mix_positions();
            let ear = self.s.params.ear_dist;
            if active {
                self.mix_trails.resize(voices.len(), Vec::new());
                for (tr, q) in self.mix_trails.iter_mut().zip(&cur) {
                    tr.push((q[0], q[2] * q[1].to_radians().cos()));
                    if tr.len() > 90 {
                        tr.remove(0);
                    }
                }
            }
            for (k, tr) in self.mix_trails.iter().enumerate().take(voices.len()) {
                let col = VOICE_COLORS[k];
                let n = tr.len();
                for (i, w) in tr.windows(2).enumerate() {
                    let a = (i as f32 / n.max(1) as f32 * 150.0) as u8;
                    p.line_segment([pos(w[0].0, w[0].1), pos(w[1].0, w[1].1)], egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a)));
                }
            }
            let dot_at = |q: &[f32; 4]| pos(q[0], q[2] * q[1].to_radians().cos());
            for (k, (v, q)) in voices.iter().zip(&cur).enumerate() {
                let col = VOICE_COLORS[k];
                let speaking = active && q[3] > 0.5;
                let sel = self.mix_sel == Some(k) || self.mix_drag == Some(k);
                let ring = if !follow {
                    Some(pos(v.az, v.distance(ear)))
                } else if manual {
                    Some(pos(pr.manual_az + offsets[k], pr.manual_dist))
                } else {
                    None
                };
                if let Some(h) = ring {
                    p.circle_stroke(h, 10.0, egui::Stroke::new(if sel { 2.5 } else { 1.5 }, col));
                }
                let label_at = ring.unwrap_or_else(|| dot_at(q));
                p.text(label_at + egui::vec2(8.0, -8.0), egui::Align2::LEFT_BOTTOM, (k + 1).to_string(), egui::FontId::proportional(11.0), col);
                let dot = if active { col } else { col.gamma_multiply(0.75) };
                p.circle_filled(dot_at(q), if speaking { 7.0 } else { 5.5 }, dot);
                let (up, _, _) = up_angle(q[0], q[1], q[2]);
                p.circle_filled(egui::pos2(gx, gauge_y(up)), if speaking { 5.0 } else { 4.0 }, dot);
            }
            if follow && manual {
                p.circle_stroke(egui::pos2(gx, gauge_y(pr.manual_tilt)), 7.0, egui::Stroke::new(1.5, accent));
            }
            // 读数：拖动时显示被拖的那个，鼠标停在图上时显示离鼠标最近的那个；平时不显示，免得挡住正前方的音源
            let d = |k: usize, h: egui::Pos2| match (follow, voices.get(k)) {
                (false, Some(v)) => (pos(v.az, v.distance(ear)) - h).length(),
                _ => cur.get(k).map_or(f32::MAX, |q| (dot_at(q) - h).length()),
            };
            let hover_k = resp.hover_pos().and_then(|h| (0..cur.len()).min_by(|&a, &b| d(a, h).total_cmp(&d(b, h))));
            if let Some((k, q)) = self.mix_drag.or(hover_k).and_then(|k| cur.get(k).map(|q| (k, q))) {
                let (up, _, plan_r) = up_angle(q[0], q[1], q[2]);
                p.text(rect.left_top() + egui::vec2(8.0, 6.0), egui::Align2::LEFT_TOP, format!("{} {}  {} {:.1} cm   {:.0}°{}", t("音源", "Source"), k + 1, t("离耳", "from ear"), ((plan_r - HEAD_A) * 100.0).max(0.0), q[0], hgt_text(up)), egui::FontId::proportional(12.0), VOICE_COLORS[k]);
            }
            let hint = if !follow && (self.mic_duo() || self.duo()) {
                t("拖动圆圈，改两个声音的位置", "drag a ring to move that voice")
            } else if !follow {
                t("拖动圆圈，改各音频的位置", "drag a ring to move that file")
            } else if manual {
                t("拖任意一个，整组一起挪 · 右侧拖高度", "drag any dot to move the group · height at right")
            } else {
                t("一起移动：按右侧「左右移动」走", "moving together: see Left / right motion")
            };
            p.text(rect.left_bottom() + egui::vec2(8.0, -5.0), egui::Align2::LEFT_BOTTOM, hint, egui::FontId::proportional(10.5), accent);
            return;
        }

        let active = self.rt.is_some();
        // 俯视图里画的是水平投影：声源升高时向头中心靠拢，从头顶绕过去时会穿过头的上方
        let (el, r3) = (self.tele.el.to_radians(), self.tele.r);
        let r_h = r3 * el.cos();
        if active {
            self.trail.push((self.tele.az, r_h));
            if self.trail.len() > 90 {
                self.trail.remove(0);
            }
        }
        let n = self.trail.len();
        for (i, w) in self.trail.windows(2).enumerate() {
            let a = (i as f32 / n.max(1) as f32 * 150.0) as u8;
            p.line_segment([pos(w[0].0, w[0].1), pos(w[1].0, w[1].1)], egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(255, 140, 60, a)));
        }
        let speaking = self.tele.act > 0.5;
        let col = if !active {
            Color32::from_gray(130)
        } else if speaking {
            Color32::from_rgb(255, 120, 40)
        } else {
            Color32::from_rgb(170, 120, 90)
        };
        let sp = pos(self.tele.az, r_h);
        p.circle_filled(sp, if speaking && active { 7.0 } else { 5.5 }, col);
        if manual {
            // 手动的目标位置：一个可以拖的圈（声音稍微滞后一点跟过来）
            let pr = &self.s.params;
            p.circle_stroke(pos(pr.manual_az, pr.manual_dist), 10.0, egui::Stroke::new(1.5, accent));
            p.circle_stroke(egui::pos2(gx, gauge_y(pr.manual_tilt)), 7.0, egui::Stroke::new(1.5, accent));
            p.text(rect.left_bottom() + egui::vec2(8.0, -5.0), egui::Align2::LEFT_BOTTOM, t("拖动圆点 · 右侧拖高度", "drag the dot · height at right"), egui::FontId::proportional(10.5), accent);
        }
        // 高度角：以"耳朵"为支点（与运动里倾斜的支点相同：正侧方时是那只耳朵，正前 / 正后方时是头中心）
        let (up, _, plan_r) = up_angle(self.tele.az, self.tele.el, r3);
        p.circle_filled(egui::pos2(gx, gauge_y(up)), if speaking && active { 5.0 } else { 4.0 }, col);
        // 伸进耳道时显示入耳深度，否则显示离耳多远
        let near = if self.tele.depth_cm > 0.05 { format!("{} {:.1} cm", t("入耳", "in ear"), self.tele.depth_cm) } else { format!("{} {:.1} cm", t("离耳", "from ear"), ((plan_r - HEAD_A) * 100.0).max(0.0)) };
        p.text(rect.left_top() + egui::vec2(8.0, 6.0), egui::Align2::LEFT_TOP, format!("{near}   {:.0}°{}", self.tele.az, hgt_text(up)), egui::FontId::proportional(12.0), fg);
    }

    /// 电平条：`frac` 0–1 是填充比例，`hot` 时变红。
    fn meter(ui: &mut egui::Ui, label: &str, frac: f32, hot: bool, value: String) {
        ui.horizontal(|ui| {
            ui.add_sized([44.0, 16.0], egui::Label::new(label));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(118.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
            let col = if hot { Color32::from_rgb(230, 90, 60) } else { Color32::from_rgb(90, 190, 120) };
            ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * frac.clamp(0.0, 1.0), rect.height())), 3.0, col);
            ui.label(value);
        });
    }

    fn ui_status(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                self.ui_radar(ui, 192.0);
                ui.vertical(|ui| {
                    let tl = self.tele;
                    let dbs = |v: f32| if v < -100.0 { "—".to_string() } else { format!("{v:.0} dB") };
                    Self::meter(ui, t("输入", "In"), (tl.in_db + 60.0) / 60.0, tl.in_db > -3.0, dbs(tl.in_db));
                    Self::meter(ui, t("输出", "Out"), (tl.out_db + 60.0) / 60.0, tl.out_db > -1.5, dbs(tl.out_db));
                    Self::meter(ui, t("限幅", "Limit"), tl.gr_db / 12.0, tl.gr_db > 6.0, format!("{:.1} dB", -tl.gr_db.max(0.0) + 0.0));
                    ui.label(format!("{} {:+.1} dB    {} {}", t("自动响度", "Auto level"), tl.level_gain_db, t("说话", "Speech"), if tl.act > 0.5 { "●" } else { "○" }));
                    ui.add_space(4.0);
                    if let Some(rt) = &self.rt {
                        let sh = &rt.shared;
                        ui.label(format!("{} {:.1} ms", t("算法延迟", "Algorithm latency"), rt.algo_ms()));
                        if !self.rt_preview {
                            ui.label(format!("{} {:.1} ms", t("总延迟约", "Total latency ≈"), rt.latency_ms())).on_hover_text(t("输入缓冲 + 环形缓冲 + 算法 + 输出缓冲，不含声卡驱动自身的延迟", "Input buffer + ring + algorithm + output buffer; excludes the driver's own latency"));
                        }
                        ui.label(format!("{} {} µs（{} {}）", t("处理时间", "Processing"), sh.proc_us.load(Ordering::Relaxed), t("最大", "max"), sh.proc_max_us.load(Ordering::Relaxed)));
                        ui.label(format!("{} {}", t("断流次数", "Dropouts"), sh.xruns.load(Ordering::Relaxed)));
                    } else {
                        let mut latency = if self.s.params.denoise { 22.4 } else { 2.4 };
                        if self.s.params.static_ambience {
                            latency += 1000.0 * crate::dsp::ambience::AMBIENCE_DELAY as f32 / FS;
                        }
                        ui.label(format!("{} {:.1} ms", t("算法延迟", "Algorithm latency"), latency));
                        ui.label(RichText::new(t("未运行", "Stopped")).weak());
                    }
                });
            });
            let mut manual = self.s.params.mode == MotionMode::Manual;
            let tip = t(
                "打开后，穿梭、忽远忽近、自动上下、自然晃动全部停掉，声源停在你摆的位置：在位置图里点或拖改方位和距离，在右侧竖条上拖改高度（双击竖条回到与耳朵齐平）。运行中打开时，声源就停在此刻所在的位置。文件转换时相当于固定在这个位置",
                "When on, all automatic motion stops (sweeps, near / far sway, auto up / down, micro motion) and the source stays where you put it: click or drag on the map for direction and distance, drag the bar at the right for height (double-click it for ear level). Turned on while running, the source stays where it is at that moment. For file conversion this is a fixed position",
            );
            // 合成·各自固定时没有手动摆位（直接拖各音频的圆圈）；一起移动时手动摆位就是拖整组
            let fixed_mix = self.mix_view() && !self.s.mix_follow;
            let r = ui.add_enabled(!fixed_mix, egui::Checkbox::new(&mut manual, t("手动摆位：关闭自动移动，拖动位置图里的声源", "Manual: stop automatic motion and drag the source on the map")));
            if r.on_hover_text(tip).on_disabled_hover_text(t("几个声音各自固定时，直接在位置图上拖各自的圆圈", "With voices at fixed positions, drag each one's ring on the map")).changed() {
                let p = &mut self.s.params;
                p.mode = if manual { MotionMode::Manual } else { self.auto_mode };
                // 取消手动摆位：回到原来的自动模式，"响度随位置变化"也恢复成进手动之前的值（进手动时在 sync_params 里记下）
                p.level_follow = if manual { Params::default_level_follow(p.mode) } else { self.auto_level_follow };
            }
            ui.checkbox(&mut self.s.view3d, t("3D 视图：立体观察人头和音源的相对位置", "3D view: see the source around the head in 3D"))
                .on_hover_text(t("打开一个小窗口：左键拖动音源（自动切到手动摆位），右键拖动旋转视角；关掉时不绘制，不占资源", "Opens a small window: left-drag moves the source (switches to manual), right-drag rotates the view; nothing is drawn while it is off"));
        });
    }

    fn ui_realtime(&mut self, ui: &mut egui::Ui) {
        // 左右双人和文件页一样放在设备、位置图上面，三行（延迟、两个声音、排布和自动平衡）也和文件页一模一样
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            self.ui_duo_block(ui, false, true);
        });
        self.ui_devices(ui, true);
        self.ui_status(ui);
        ui.horizontal(|ui| {
            let running = self.rt.is_some() && !self.rt_preview;
            if ui.add_enabled(!running, egui::Button::new(RichText::new(t("开始", "Start")).strong()).min_size(egui::vec2(96.0, 30.0))).clicked() {
                self.start_mic();
            }
            if ui.add_enabled(self.rt.is_some(), egui::Button::new(t("停止", "Stop")).min_size(egui::vec2(96.0, 30.0))).clicked() {
                self.stop();
                self.status = t("已停止", "Stopped").to_string();
            }
            let before = self.s.dry;
            ui.radio_value(&mut self.s.dry, false, t("输出效果", "Effect"));
            ui.radio_value(&mut self.s.dry, true, t("监听原声", "Monitor dry"));
            if before != self.s.dry {
                if let Some(rt) = &self.rt {
                    rt.shared.monitor_dry.store(self.s.dry, Ordering::Relaxed);
                }
            }
        });
        ui.label(
            RichText::new(t(
                "直播时把输出设备选成虚拟声卡（如 VB-Cable / BlackHole），再在直播软件里把它当作麦克风。自己监听请戴耳机，外放听不出效果。",
                "For streaming, pick a virtual cable (VB-Cable / BlackHole) as output and use it as the microphone in your streaming app. Use headphones to monitor.",
            ))
            .small()
            .weak(),
        );
    }

    /// 文件页上方：待转换的文件列表（可多选添加、拖进窗口、点选试听），以及输出位置。
    fn ui_file_list(&mut self, ui: &mut egui::Ui) {
        let busy = self.job.is_some();
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("音频文件", "Audio files")).strong());
            if !self.files.is_empty() {
                ui.label(RichText::new(format!("{} {}", self.files.len(), t("个", "files"))).weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!busy && !self.files.is_empty(), egui::Button::new(t("清空", "Clear"))).clicked() {
                    self.clear_files();
                }
                if ui.add_enabled(!busy, egui::Button::new(t("添加文件夹…", "Add folder…"))).on_hover_text(t("加入文件夹里的全部音频文件（不含子文件夹，跳过已转换的 _asmr 文件）", "Adds every audio file in the folder (not subfolders; skips _asmr outputs)")).clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.add_files(vec![d], false);
                    }
                }
                if ui.add_enabled(!busy, egui::Button::new(t("添加文件…", "Add files…"))).on_hover_text(t("可以多选（按住 ⌘ / Ctrl 或 Shift）", "Multiple selection allowed (hold ⌘ / Ctrl or Shift)")).clicked() {
                    if let Some(v) = rfd::FileDialog::new().add_filter(t("音频", "Audio"), batch::AUDIO_EXTS).pick_files() {
                        self.add_files(v, false);
                    }
                }
            });
        });
        if self.files.is_empty() {
            ui.label(RichText::new(t("点「添加文件…」可以一次多选，也可以把文件或文件夹拖进窗口", "Use “Add files…” to pick several at once, or drop files / folders onto the window")).weak());
        } else {
            let (mut select, mut remove) = (None, None);
            let mut edits: Vec<(PathBuf, Voice)> = Vec::new();
            let row_h = 22.0;
            let mixing = self.multi();
            let follow = mixing && self.s.mix_follow;
            let offsets = mix::follow_offsets(self.files.len().min(MAX_VOICES), true);
            let files = self.files.clone();
            egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(4.0).inner_margin(4.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("filelist").max_height(row_h * 5.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, p) in files.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.set_min_height(row_h - 4.0);
                            if ui.add_enabled(!busy, egui::Button::new("×").small().frame(false)).on_hover_text(t("从列表移除", "Remove from list")).clicked() {
                                remove = Some(i);
                            }
                            let in_mix = mixing && i < MAX_VOICES;
                            if in_mix {
                                // 合成：行首是这个音源在位置图里的颜色和编号
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                                ui.painter().circle_filled(rect.center(), 8.0, VOICE_COLORS[i]);
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, (i + 1).to_string(), egui::FontId::proportional(11.0), Color32::WHITE);
                            }
                            let status_w = 150.0;
                            // 合成时右边是位置下拉框（宽度按界面语言里最长的位置名）和音量
                            let combo_w = if ENGLISH.load(Ordering::Relaxed) { 104.0 } else { 64.0 };
                            let ctl_w = if in_mix { combo_w + 70.0 + ui.spacing().item_spacing.x } else { status_w };
                            let name_w = (ui.available_width() - ctl_w - 8.0).max(60.0);
                            let name = file_name(p);
                            let sel = if in_mix { self.mix_sel == Some(i) } else { self.file_in.as_ref() == Some(p) };
                            let tip = if in_mix {
                                t("合成时几个音频一起播放；点击选中它，位置图左上角显示它的位置", "Mixed files play together; click to show this one's position on the map")
                            } else if mixing {
                                t("合成最多用前 4 个音频，这个不参与", "Only the first 4 files are mixed; this one is not")
                            } else {
                                t("点击选中，用来试听", "Click to select for preview")
                            };
                            let text = if mixing && !in_mix { RichText::new(name).weak() } else { RichText::new(name) };
                            let r = ui.add_sized([name_w, row_h - 4.0], egui::Button::selectable(sel, text).truncate().right_text(""));
                            if r.on_hover_text(format!("{}\n{tip}", p.display())).clicked() && (!mixing || in_mix) {
                                select = Some(i);
                            }
                            if in_mix {
                                let mut v = self.voices.get(p).copied().unwrap_or_else(|| Voice::preset(i.min(1)));
                                let before = v;
                                // 一起移动：位置由走位决定，这里显示它比 1 号转过多少度（只有音量可调）
                                let follow_off = follow.then(|| offsets[i]);
                                ui.add_enabled_ui(!busy, |ui| Self::voice_controls(ui, i, &mut v, combo_w, follow_off));
                                if v != before {
                                    edits.push((p.clone(), v));
                                }
                            } else if mixing {
                                ui.add_sized([status_w, 16.0], egui::Label::new(RichText::new(t("不参与合成", "not mixed")).small().weak()));
                            } else {
                                Self::status_cell(ui, status_w, self.statuses.get(p));
                            }
                        });
                    }
                });
            });
            for (p, v) in edits {
                self.voices.insert(p, v);
            }
            if let Some(i) = remove {
                self.remove_file(i);
            } else if let Some(i) = select {
                let p = self.files[i].clone();
                if mixing {
                    // 合成时点一行只是选中它（位置图显示它的读数），不打断正在播放的合成试听
                    self.mix_sel = Some(i);
                    self.file_out = Some(default_output(&p));
                    self.file_in = Some(p);
                } else {
                    self.set_file(p);
                }
            }
        }
        // 多音频合成开关
        ui.horizontal(|ui| {
            let mut on = self.s.mix;
            let tip = t(
                "勾上后，列表里的前 4 个音频各放一个位置（默认一个在左耳、一个在右耳，再多就放正前、正后），「试听」时一起播放，「合成并保存」输出一个文件。下面的「排布」可以选各自固定（位置在列表里选或在位置图上拖）或一起移动（按右侧「左右移动」的方式一起走，彼此始终相对）。各音频先自动对齐到一样响，列表里的 dB 是在这之上的加减",
                "When on, the first 4 files each get a position (left ear and right ear by default, then front and back), play together in Preview, and Mix & save writes one file. Below, Layout picks Fixed (positions from the list or dragged on the map) or Together (they follow Left / right motion and always stay opposite). The files are matched in loudness first; the dB in the list is added on top",
            );
            if ui.add_enabled(!busy, egui::Checkbox::new(&mut on, t("多音频合成：几个音频各在一个位置，同时播放", "Multi-audio mix: files at different positions, played together"))).on_hover_text(tip).changed() {
                self.set_mix_mode(on);
            }
            if self.s.mix {
                let n = self.files.len();
                let note = if n < 2 {
                    t("再添加一个音频", "add another file").to_string()
                } else if n > MAX_VOICES {
                    format!("{} {MAX_VOICES} {}", t("只用前", "first"), t("个", "only"))
                } else {
                    format!("{} {MAX_VOICES} {}", t("最多", "up to"), t("个", "files"))
                };
                ui.label(RichText::new(note).small().weak());
            }
        });
        // 合成的排布：各自固定 / 一起移动
        if self.s.mix {
            self.ui_layout_row(ui, busy);
        }
        // 合成前自动对齐：同一首歌的几个版本（前奏长短不同、编曲不同），从一起开始唱的地方叠加
        if self.s.mix {
            ui.horizontal(|ui| {
                let mut on = self.s.mix_align;
                let tip = t(
                    "同一首歌的几个版本（比如一段前奏长、一段前奏短，或者不同的编曲）：按旋律、和弦和节奏自动找到它们一起开始唱的地方，从那里叠加；对不上的开头和结尾裁掉（裁开的地方淡入淡出），下面会写出每段从哪里开始用、裁掉多少。试听和合成并保存用的是同一份对齐结果；「从第几秒 / 处理多少秒」和播放条都按对齐之后的时间算。几段对不上（不像同一首歌）时不裁剪，按原样合成",
                    "Versions of the same song (one with a long intro and one with a short one, or different arrangements): finds where they sing together from melody, chords and rhythm, mixes them from there and cuts the parts that don't match at the start and end (with short fades). Below it shows where each file starts and how much is cut. Preview and Mix & save use the same alignment; Start, Duration and the timeline count from the aligned start. If the files don't match (not the same song), nothing is cut",
                );
                if ui.add_enabled(!busy, egui::Checkbox::new(&mut on, t("自动对齐：同一首歌的几个版本，从一起开始唱的地方叠加", "Auto-align: versions of one song, mixed from where they sing together"))).on_hover_text(tip).changed() {
                    self.set_mix_align(on);
                }
            });
            if self.s.mix_align && self.multi() {
                let paths = self.mix_paths();
                let ch = self.s.file_channel;
                let warn = ui.visuals().warn_fg_color;
                let info = self.mix_audio.as_ref().filter(|m| m.paths == paths && m.ch == ch).and_then(|m| m.aligned.as_ref());
                match info {
                    Some(Ok(a)) => {
                        let names: Vec<String> = paths.iter().map(|p| file_name(p)).collect();
                        for line in a.al.describe(&names, ENGLISH.load(Ordering::Relaxed)).lines() {
                            let text = RichText::new(line.trim_start()).small();
                            let text = if line.contains('!') || line.contains('！') { text.color(warn) } else { text };
                            ui.add(egui::Label::new(text).wrap());
                        }
                    }
                    Some(Err(e)) => {
                        ui.add(egui::Label::new(RichText::new(e.as_str()).small().color(warn)).wrap());
                    }
                    None => {
                        let note = if self.mix_decoding.is_some() { t("正在对齐…", "Aligning…") } else { t("解码后自动对齐", "Aligned after decoding") };
                        ui.label(RichText::new(note).small().weak());
                    }
                }
            }
        }
        // 左右双人：一个音频当成两个人
        self.ui_duo(ui, busy);
        // 输出位置：合成时是一个文件；一个文件时可以指定文件名；多个文件时指定文件夹（左右双人也一样）
        if self.multi() {
            let out = self.mix_output();
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new(t("输出到…", "Save as…"))).clicked() {
                    let mut dlg = rfd::FileDialog::new().add_filter("WAV", &["wav"]).set_file_name(file_name(&out));
                    if let Some(d) = out.parent().filter(|d| d.is_dir()) {
                        dlg = dlg.set_directory(d);
                    }
                    if let Some(p) = dlg.save_file() {
                        self.mix_out = Some(p);
                    }
                }
                ui.add(egui::Label::new(file_name(&out)).truncate()).on_hover_text(out.display().to_string());
                if self.mix_out.is_some() && ui.add_enabled(!busy, egui::Button::new(t("默认名", "Default")).small()).on_hover_text(t("放回第一个音频旁边，名字加 _mix_asmr", "Back to the default: beside the first file, named …_mix_asmr")).clicked() {
                    self.mix_out = None;
                }
            });
        } else if self.files.len() == 1 {
            if let Some(out) = self.file_out.clone() {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Button::new(t("输出到…", "Save as…"))).clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("WAV", &["wav"]).set_file_name(out.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()).save_file() {
                            self.file_out = Some(p);
                        }
                    }
                    ui.label(out.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()).on_hover_text(out.display().to_string());
                });
            }
        } else if self.files.len() > 1 {
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new(t("输出文件夹…", "Output folder…"))).clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.s.out_dir = Some(d);
                    }
                }
                match self.s.out_dir.clone() {
                    Some(d) => {
                        ui.add(egui::Label::new(d.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| d.display().to_string())).truncate()).on_hover_text(d.display().to_string());
                        if ui.add_enabled(!busy, egui::Button::new(t("放回源文件旁", "Beside sources")).small()).clicked() {
                            self.s.out_dir = None;
                        }
                    }
                    None => {
                        ui.label(RichText::new(t("各自源文件旁边，名字加 _asmr", "beside each source, named …_asmr")).weak());
                    }
                }
            });
        }
        // 总进度和完成后的说明放在列表下面，不用滚动就能看到
        if let Some(j) = &self.job {
            let (f, n_done, n_run) = job_progress(j);
            let n = j.paths.len();
            let el = j.t0.elapsed().as_secs_f64();
            let eta = if f > 0.02 { format!("，{} {}", t("预计还需", "about"), playback_time(el / f as f64 * (1.0 - f as f64))) } else { String::new() };
            let text = if n == 1 {
                match j.status[0].lock().map(|g| g.clone()) {
                    Ok(ItemStatus::Running(_, st)) => format!("{st} {:.0}%{eta}", f * 100.0),
                    _ => format!("{:.0}%", f * 100.0),
                }
            } else {
                format!("{n_done} / {n} {}，{n_run} {}，{:.0}%{eta}", t("完成", "done"), t("个在转换", "running"), f * 100.0)
            };
            ui.add(egui::ProgressBar::new(f).text(text));
            if n > 1 {
                ui.label(RichText::new(format!("{} {} {}", t("后台", "Using"), j.workers, t("个线程并行，长的文件先处理", "threads in parallel, longest files first"))).small().weak());
            }
        }
        if !self.report.is_empty() {
            ui.label(&self.report);
        }
    }

    /// 合成的排布（各自固定 / 一起移动）和自动平衡：多音频合成和左右双人（文件页、实时页）共用。
    fn ui_layout_row(&mut self, ui: &mut egui::Ui, busy: bool) {
        ui.horizontal(|ui| {
            ui.label(t("排布", "Layout"));
            let mut follow = self.s.mix_follow;
            ui.selectable_value(&mut follow, false, t("各自固定", "Fixed")).on_hover_text(t(
                "每个音频固定在自己的位置（在列表里选，或在位置图上拖），只做忽远忽近、上下、自然晃动，不会跑到别的音频那边去",
                "Each file stays at its own position (pick it in the list or drag it on the map), with only near / far sway, up / down and micro motion",
            ));
            ui.selectable_value(&mut follow, true, t("一起移动", "Together")).on_hover_text(t(
                "几个音频按右侧「左右移动」的方式（穿梭、随机走位、环绕、固定、手动）一起走，彼此始终相隔 360°/个数：两个时正好在头的两侧相对，一个贴左耳时另一个贴右耳，换边时一个从脸前、一个从脑后绕过去，不会撞在一起。说话时钟按任何一个音频在说话来走",
                "The files follow Left / right motion (sweep, random, orbit, static, manual) together and always stay 360° / count apart: two files sit on opposite sides, one at the left ear while the other is at the right, and when they switch sides one passes in front and the other behind, so they never meet. Motion runs while any of the files is speaking",
            ));
            if follow != self.s.mix_follow {
                self.set_mix_follow(follow);
            }
            // 自动平衡（0 = 关）：和排布放在一行，不多占高度
            ui.add_space(8.0);
            ui.label(t("自动平衡", "Balance")).on_hover_text(t(
                "让几个声音每一段时间听起来都差不多一样响（比如一左一右时左右两边差不多大）。合成本来就按整段平均响度对齐，但两个声音各自忽远忽近，常常一个退开了另一个正贴着耳朵，内容本身也有响有轻。这里按每个声音此刻听起来多响（空间化之后：输入的 3 秒短时响度 + 它此刻位置带来的增益）往平均拉，最多 ±15 dB，0.25 秒平滑；停顿里的声音不往上拉。数值 = 强度：100% = 一样响，50% = 差距减半，0 = 关闭。一个声音单独退开时也会被补回一部分，所以忽远忽近的响度变化会变小（方向、远耳、音色这些远近线索照常）。试听时实时生效，合成完成后的说明里有平衡前后的响度差。主播模式的左右双人也一样，只是响度边听边算（只能往回看），运行中实时生效",
                "Keeps the voices about equally loud at every moment (e.g. left and right sides about the same). The mix already matches their overall loudness, but each voice sways near and far on its own, so often one has pulled back while the other is at the ear, and the content itself gets louder and softer. This pulls each voice toward the average of how loud they sound right now (after spatialization: 3-second input loudness plus the gain of its current position), at most ±15 dB, smoothed over 0.25 s; voices in a pause are not lifted. Value = strength: 100% = equal, 50% = half the gap, 0 = off. A voice that pulls back on its own is partly made up for, so near / far loudness changes get smaller (direction, far-ear and tone cues remain). Works live in Preview; the report after Mix & save shows the gap before and after. The real-time two voices work the same way, with loudness measured as it comes in (looking back only), live while running",
            ));
            ui.spacing_mut().slider_width = 90.0;
            let slider = egui::Slider::new(&mut self.s.mix_balance, 0.0..=100.0)
                .step_by(1.0)
                .fixed_decimals(0)
                .custom_formatter(|x, _| if x < 0.5 { t("0 关", "0 off").to_string() } else { format!("{x:.0}%") })
                .custom_parser(|s| s.trim().split(|c: char| !(c.is_ascii_digit() || c == '.')).find(|w| !w.is_empty()).and_then(|w| w.parse::<f64>().ok()));
            ui.add_enabled(!busy, slider);
        });
    }

    /// 文件页的左右双人：一个音频当成两个人。多音频合成生效时不显示。
    fn ui_duo(&mut self, ui: &mut egui::Ui, busy: bool) {
        if self.multi() {
            return; // 多音频合成时用不上，也不占地方
        }
        self.ui_duo_block(ui, busy, false);
    }

    /// 左右双人这一块，文件页和实时（主播）页共用、一模一样：延迟滑条和提示；打开后下面一行是两个声音的位置 / 音量，
    /// 再下面是排布和自动平衡（和多音频合成共用）。`live`：实时页（麦克风的声音，延迟单独记在 `rt_duo_ms`；
    /// 运行中从 0 拖上来、拖回 0 时重新开始一下）。
    fn ui_duo_block(&mut self, ui: &mut egui::Ui, busy: bool, live: bool) {
        let tip = if live {
            t(
                "把你的声音变成两个人：同一个声音一个在左耳、一个在右耳（各自忽远忽近），第 2 个整体晚这么多毫秒，听起来是两个人一前一后地说。30 ms 以下两边容易融成一个声音（听起来偏在先到的那一边），50–120 ms 像两个人，150 ms 以上像回声。只有第 2 个声音晚，第 1 个和平时一样及时。打开后下面可以改两个声音的位置、音量、排布和自动平衡；运行中拖这个滑条实时生效，从 0 拖上来或拖回 0 时会重新开始一下。处理量约是平时的两倍。0 = 关闭",
                "Turns your voice into two people: the same voice at the left ear and at the right ear (each sways near and far on its own), the second one this many milliseconds later, so it sounds like two people speaking one after the other. Below about 30 ms the two tend to fuse into one voice (heard on the earlier side); 50–120 ms sounds like two people; above 150 ms it sounds like an echo. Only the second voice is delayed; the first is as immediate as usual. When on, the two voices' positions, volumes, layout and balance can be set below; dragging this slider while running takes effect live, and turning it on or off restarts briefly. Uses about twice the processing. 0 = off",
            )
        } else {
            t(
                "把一个音频当成两个人：同一个声音一个在左耳、一个在右耳（和导入两个音频的多音频合成一样，各自忽远忽近），第 2 个整体晚这么多毫秒，听得出是两个人一前一后地说。30 ms 以下两边容易融成一个声音（听起来偏在先到的那一边），50–120 ms 像两个人，150 ms 以上像回声。打开后下面可以改两个声音的位置、音量、排布和自动平衡；试听中拖这个滑条实时生效。输出比原音频长出这段延迟。列表里有几个音频时，试听的是选中的那个，「全部转换」时每个文件都这样做。0 = 关闭",
                "Turns one file into two people: the same voice at the left ear and at the right ear (like mixing two files: each sways near and far on its own), the second one this many milliseconds later, so you hear two people speaking one after the other. Below about 30 ms the two tend to fuse into one voice (heard on the earlier side); 50–120 ms sounds like two people; above 150 ms it sounds like an echo. When on, the two voices' positions, volumes, layout and balance can be set below; dragging this slider during Preview takes effect live. The output is longer by the delay. With several files in the list, Preview plays the selected one and Convert all does this to every file. 0 = off",
            )
        };
        let ms = if live { self.s.rt_duo_ms } else { self.s.duo_ms };
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                ui.label(t("左右双人", "Two voices")).on_hover_text(tip);
                ui.spacing_mut().slider_width = 140.0;
                let mut v = ms;
                if ui.add(duo_slider(&mut v)).on_hover_text(tip).changed() {
                    if live {
                        self.set_rt_duo(v);
                    } else {
                        self.set_duo(v);
                    }
                }
            });
            let note = if ms <= 0.0 {
                t("同一个声音一左一右两个人说", "same voice on both sides")
            } else if !live && self.files.len() > 1 {
                t("转换时每个文件都这样", "applies to every file")
            } else {
                t("50–120 ms 最像两个人", "50–120 ms: most like two people")
            };
            ui.label(RichText::new(note).small().weak());
        });
        if !(if live { self.mic_duo() } else { self.duo() }) {
            return;
        }
        self.ui_duo_voices(ui, busy, ms);
        // 文件页勾着多音频合成（但只有一个音频）时，排布已经在合成开关下面了
        if live || !self.s.mix {
            self.ui_layout_row(ui, busy);
        }
    }

    /// 左右双人的两个声音放在一行：颜色和编号（和位置图一致）、位置、音量；一起移动时位置由走位决定。
    /// 文件页和实时页共用这两个位置。`ms`：第 2 个声音晚多少毫秒（写在提示里）。
    fn ui_duo_voices(&mut self, ui: &mut egui::Ui, busy: bool, ms: f32) {
        let combo_w = if ENGLISH.load(Ordering::Relaxed) { 104.0 } else { 64.0 };
        let follow = self.s.mix_follow;
        let offsets = mix::follow_offsets(2, true);
        let names = [t("1 号：原声（准时）", "1: the original (on time)").to_string(), format!("{} {:.0} ms", t("2 号：晚", "2: later by"), ms)];
        ui.horizontal(|ui| {
            for k in 0..2 {
                if k == 1 {
                    ui.add_space(10.0);
                }
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                let sel = self.mix_sel == Some(k);
                ui.painter().circle_filled(rect.center(), 8.0, VOICE_COLORS[k]);
                if sel {
                    ui.painter().circle_stroke(rect.center(), 9.5, egui::Stroke::new(1.5, ui.visuals().strong_text_color()));
                }
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, (k + 1).to_string(), egui::FontId::proportional(11.0), Color32::WHITE);
                if resp.on_hover_text(format!("{}\n{}", names[k], t("点击选中它，位置图左上角显示它的位置", "Click to show its position on the map"))).clicked() {
                    self.mix_sel = Some(k);
                }
                let mut v = self.duo_voices[k];
                ui.add_enabled_ui(!busy, |ui| Self::voice_controls(ui, k, &mut v, combo_w, follow.then(|| offsets[k])));
                self.duo_voices[k] = v;
            }
        });
    }

    /// 文件列表右侧的状态：等待 / 进度条 / 完成 / 失败。
    fn status_cell(ui: &mut egui::Ui, w: f32, st: Option<&ItemStatus>) {
        let size = [w, 16.0];
        match st {
            None => {
                ui.allocate_exact_size(egui::vec2(w, 16.0), egui::Sense::hover());
            }
            Some(ItemStatus::Waiting) => {
                ui.add_sized(size, egui::Label::new(RichText::new(t("等待中", "Waiting")).small().weak()));
            }
            Some(ItemStatus::Running(f, stage)) => {
                ui.add_sized(size, egui::ProgressBar::new(*f).text(RichText::new(format!("{stage} {:.0}%", f * 100.0)).small()));
            }
            Some(ItemStatus::Done(rep, out)) => {
                ui.add_sized(size, egui::Label::new(RichText::new(format!("✓ {} · {:.0} s", t("完成", "Done"), rep.elapsed_s)).small().color(Color32::from_rgb(90, 190, 120))))
                    .on_hover_text(format!("{}\n{:.1} s，{:.1} LUFS，{} {:.1} dBTP{}", out.display(), rep.seconds, rep.lufs, t("真峰值", "true peak"), rep.true_peak_db, if rep.channel_note.is_empty() { String::new() } else { format!("\n{}", rep.channel_note) }));
            }
            Some(ItemStatus::Failed(e)) => {
                ui.add_sized(size, egui::Label::new(RichText::new(format!("× {}", t("失败", "Failed"))).small().color(Color32::from_rgb(230, 90, 60)))).on_hover_text(e.as_str());
            }
        }
    }

    /// 文件列表里一个合成音源的位置下拉框（固定宽度 `combo_w`，放不下的字截断）和音量。
    /// `follow_off`：一起移动时它比 1 号转过的角度（位置由走位决定，下拉框换成这个角度）。
    fn voice_controls(ui: &mut egui::Ui, i: usize, v: &mut Voice, combo_w: f32, follow_off: Option<f32>) {
        ui.allocate_ui_with_layout(egui::vec2(combo_w, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_max_width(combo_w);
            if let Some(off) = follow_off {
                let text = if i == 0 { t("领头", "lead").to_string() } else { format!("+{off:.0}°") };
                ui.add_sized([combo_w, 18.0], egui::Label::new(RichText::new(text).weak())).on_hover_text(t(
                    "一起移动：几个音频按同一套走位一起走，这个音频的位置比 1 号绕头转过这么多度（两个时 +180° 就是正对面）",
                    "Moving together: the files share one path; this one is rotated around the head by this much from file 1 (+180° = directly opposite)",
                ));
                return;
            }
            egui::ComboBox::from_id_salt(("voice_pos", i))
                .width(combo_w)
                .truncate()
                .selected_text(voice_name(v))
                .show_ui(ui, |ui| {
                    for (k, &(zh, en, _, _)) in PRESETS.iter().enumerate() {
                        if ui.selectable_label(v.preset_index() == Some(k), t(zh, en)).clicked() {
                            *v = Voice { gain_db: v.gain_db, ..Voice::preset(k) };
                        }
                    }
                })
                .response
                .on_hover_text(t("这个音频放在哪里。也可以在位置图上直接拖它的圆圈", "Where this file sits. You can also drag its ring on the map"));
        });
        ui.add_sized([70.0, 18.0], egui::DragValue::new(&mut v.gain_db).range(-30.0..=12.0).speed(0.1).fixed_decimals(1).suffix(" dB"))
            .on_hover_text(t("音量增减：几个音频先自动对齐到一样响，再按这个数加减（0 = 不变）。拖动或点击输入", "Volume offset: the files are first matched in loudness, then this is added (0 = unchanged). Drag or click to type"));
    }

    fn ui_file(&mut self, ui: &mut egui::Ui) {
        let channel_before = self.s.file_channel;
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            self.ui_file_list(ui);
            egui::Grid::new("fileopts").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(t("取哪个声道", "Channel"));
                let names = [t("自动（更响的）", "Auto (louder)"), t("左", "Left"), t("右", "Right"), t("混合", "Mix")];
                egui::ComboBox::from_id_salt("fch").width(120.0).selected_text(names[self.s.file_channel.min(3) as usize]).show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut self.s.file_channel, i as u8, *n);
                    }
                });
                ui.label(t("位深", "Bit depth"));
                egui::ComboBox::from_id_salt("bits").width(70.0).selected_text(format!("{} bit", self.s.bits)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.s.bits, 24, "24 bit");
                    ui.selectable_value(&mut self.s.bits, 16, "16 bit");
                });
                ui.end_row();
                ui.label(t("从第几秒", "Start (s)"));
                if ui.add(egui::DragValue::new(&mut self.start_s).range(0.0..=36000.0).speed(1.0)).changed() {
                    self.preview_s = self.start_s as f64;
                }
                ui.label(t("处理多少秒", "Duration (s)"));
                ui.add(egui::DragValue::new(&mut self.dur_s).range(0.0..=36000.0).speed(1.0)).on_hover_text(t("0 = 到结尾。先试 60 秒听听效果很方便", "0 = to the end. Try 60 s first to audition"));
                ui.end_row();
                ui.label(t("目标响度", "Loudness"));
                ui.add(egui::Slider::new(&mut self.s.file_lufs, -24.0..=-10.0).clamping(egui::SliderClamping::Never).suffix(" LUFS").fixed_decimals(0));
                ui.end_row();
                if self.multi() {
                    return; // 合成只输出一个文件（几个音源的引擎已经各用一个核并行），没有"同时转换几个文件"
                }
                ui.label(t("同时转换", "Parallel"));
                let cores = batch::default_threads();
                let mut n = self.threads();
                let resp = ui.add_enabled(self.job.is_none(), egui::DragValue::new(&mut n).range(1..=cores.max(64)).suffix(t(" 个文件", " files"))).on_hover_text(format!(
                    "{}{cores}{}",
                    t("批量转换时同时处理几个文件，每个文件占一个核。默认 = 本机逻辑核数 ", "How many files are converted at once, one core each. Default = logical cores on this machine: "),
                    t("。整段读进内存，长音频每小时约占 0.7 GB，内存紧张时调小", ". Each file is held in memory (≈0.7 GB per hour of audio); lower this if memory is tight")
                ));
                if resp.changed() {
                    self.s.jobs = if n == cores { 0 } else { n };
                }
                if self.s.jobs != 0 && ui.small_button(t("自动", "Auto")).on_hover_text(format!("{} {cores}", t("恢复为本机逻辑核数", "Back to the number of logical cores:"))).clicked() {
                    self.s.jobs = 0;
                }
                ui.end_row();
            });
        });
        if channel_before != self.s.file_channel && self.file_in.is_some() {
            let resume = self.rt_preview || self.preview_pending;
            self.stop();
            self.decoded = None;
            self.mix_audio = None;
            self.preview_pending = resume;
            if self.mixing() {
                self.decode_mix();
            } else {
                self.decode_file();
            }
        }
        self.ui_devices(ui, false);
        self.ui_status(ui);
        let has = self.file_in.is_some();
        let n_files = self.files.len();
        let mixing = self.mixing();
        let multi = self.multi();
        ui.horizontal(|ui| {
            let previewing = self.rt.is_some() && self.rt_preview;
            let (can, tip) = if multi {
                (self.mix_decoding.is_none(), t("几个音频同时播放，各在自己的位置；可以边听边调右边的参数、在位置图上拖位置、改列表里的音量", "Plays the files together, each at its position; tweak parameters, drag positions on the map and change volumes while listening"))
            } else if mixing {
                (has && self.mix_decoding.is_none(), t("左右双人：同一个声音一左一右两个人说，第 2 个晚一点；可以边听边调延迟、位置、音量和右边的参数", "Two voices: the same voice on both sides, the second slightly later; tweak the delay, positions, volumes and parameters while listening"))
            } else {
                (has && self.decoding.is_none(), t("用实时引擎直接播放，可以边听边调右边的参数", "Plays through the real-time engine; tweak the parameters while listening"))
            };
            if ui.add_enabled(can && !previewing, egui::Button::new(t("试听", "Preview")).min_size(egui::vec2(96.0, 30.0))).on_hover_text(tip).clicked() {
                self.start_preview();
            }
            if ui.add_enabled(previewing, egui::Button::new(t("停止", "Stop")).min_size(egui::vec2(72.0, 30.0))).clicked() {
                self.stop();
                self.status = t("已停止", "Stopped").to_string();
            }
            let busy = self.job.is_some();
            let label = if multi {
                format!("{}（{}）", t("合成并保存", "Mix & save"), n_files.min(MAX_VOICES))
            } else if n_files > 1 {
                format!("{}（{n_files}）", t("全部转换并保存", "Convert all"))
            } else {
                t("转换并保存", "Convert & save").to_string()
            };
            let mut r = ui.add_enabled(n_files > 0 && !busy, egui::Button::new(RichText::new(label).strong()).min_size(egui::vec2(120.0, 30.0)));
            if multi {
                r = r.on_hover_text(if self.s.mix_align {
                    t("先自动对齐，再把参与合成的音频合成一个双耳 WAV（长度是对齐后的共同部分；起点和时长按对齐之后的时间算）", "Aligns the files, then mixes them into one binaural WAV (as long as their common part; Start and Duration count from the aligned start)")
                } else {
                    t("把参与合成的音频合成一个双耳 WAV（长度按最长的那个）", "Mixes the files into one binaural WAV (as long as the longest file)")
                });
            }
            if r.clicked() {
                self.start_convert();
            }
            if busy && ui.button(t("取消", "Cancel")).clicked() {
                if let Some(j) = &self.job {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        // 播放条的总长：合成时是各段（左右双人的第 2 个声音晚一点）都放完为止；左右双人还在算响度时先按单个文件的长度
        let delays = self.mix_delays();
        let single = self.decoded.as_ref().filter(|(p, _, _)| Some(p) == self.file_in.as_ref()).map(|(_, _, a)| a.len());
        let preview_len = if mixing {
            self.mix_audio
                .as_ref()
                .and_then(|m| m.tracks(self.align_on()))
                .map(|t| mix::timeline_len(&t.data.iter().map(|x| x.len()).collect::<Vec<_>>(), &delays))
                .or_else(|| single.filter(|_| !multi).map(|l| l + delays[1]))
        } else {
            single
        };
        if let Some(len) = preview_len {
            let duration = len as f64 / FS as f64;
            self.preview_s = self.preview_s.clamp(0.0, duration);
            let aligned = self.align_on() && self.mix_audio.as_ref().map_or(false, |m| matches!(m.aligned, Some(Ok(_))));
            let label = if aligned { t("试听位置（对齐后）", "Preview position (aligned)") } else { t("试听位置", "Preview position") };
            ui.label(format!("{label}  {} / {}", playback_time(self.preview_s), playback_time(duration)));
            let response = ui.scope(|ui| {
                ui.spacing_mut().slider_width = ui.available_width().max(1.0);
                ui.add_enabled(duration > 0.0, egui::Slider::new(&mut self.preview_s, 0.0..=duration).show_value(false))
            }).inner.on_hover_text(t("点击或拖动，立即试听该位置的处理效果；右侧参数边调边生效", "Click or drag to hear the processed audio here; effect parameters update while playing"));
            self.preview_dragging = response.dragged();
            if response.changed() || response.clicked() || response.drag_started() {
                if let Some(rt) = self.rt.as_ref().filter(|_| self.rt_preview) {
                    rt.shared.seek_preview((self.preview_s * FS as f64) as u64);
                } else {
                    self.start_preview();
                }
            }
        }
    }

    /// 左下角：输入降噪（一个 0–100 的滑条：数值 = 底噪最多压低多少分贝，0 = 关闭，100 = 不设上限），下面是输出增益。
    fn ui_denoise(ui: &mut egui::Ui, p: &mut Params) {
        ui.add_space(5.0);
        let mut v = p.denoise_slider();
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("降噪", "Denoise")).strong()).on_hover_text(t(
                "输入降噪（RNNoise，纯 CPU）：在空间化之前去掉麦克风 / 录音里的底噪。数值 = 底噪最多压低多少分贝，0 = 关闭，100 = 不设上限；60 以上基本没有区别。人声的音量不变",
                "Input denoise (RNNoise, CPU only): removes background noise before spatialization. The value is how many dB the noise is lowered by at most; 0 = off, 100 = no limit, little changes above 60. The voice keeps its level",
            ));
            ui.spacing_mut().slider_width = 300.0;
            let slider = egui::Slider::new(&mut v, 0.0..=DENOISE_MAX_DB)
                .step_by(1.0)
                .fixed_decimals(0)
                .custom_formatter(|x, _| {
                    if x < 0.5 {
                        t("0 关", "0 off").to_string()
                    } else if x >= DENOISE_MAX_DB as f64 - 0.5 {
                        t("100 最大", "100 max").to_string()
                    } else {
                        format!("{x:.0} dB")
                    }
                })
                .custom_parser(|s| s.trim().split(|c: char| !(c.is_ascii_digit() || c == '.')).find(|w| !w.is_empty()).and_then(|w| w.parse::<f64>().ok()));
            if ui.add(slider).changed() {
                p.set_denoise_slider(v);
            }
        });
        let note = if !p.denoise {
            t("0 = 关闭。麦克风或录音有底噪时往右拖：数值 = 底噪最多压低多少分贝", "0 = off. Drag right for a noisy microphone or recording: the value is how many dB the noise is lowered by at most").to_string()
        } else if p.denoise_db >= DENOISE_MAX_DB {
            t("不设上限，降到最干净；气声的高频会被削掉一些，很轻的尾音可能被压掉。延迟增加 20 ms", "No limit, as clean as it gets; breathy highs lose a little and very soft tails may be cut. Adds 20 ms of latency").to_string()
        } else {
            format!("{} {:.0} dB{}", t("底噪最多压低", "Noise lowered by at most"), p.denoise_db, t("，人声音量不变。数值越大越干净，气声的高频也会被削掉一点。延迟增加 20 ms", "; the voice keeps its level. Higher = cleaner, but breathy highs lose a little. Adds 20 ms of latency"))
        };
        ui.add(egui::Label::new(RichText::new(note).small().weak()).wrap());
        ui.add_space(4.0);
        // 输出增益：只是监听音量（实时 / 试听），放在降噪旁边随手可调；文件转换不用它，响度由目标响度决定
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("输出增益", "Output gain")).strong()).on_hover_text(t(OUT_GAIN_TIP_ZH, OUT_GAIN_TIP_EN));
            ui.spacing_mut().slider_width = 260.0;
            ui.add(egui::Slider::new(&mut p.out_gain_db, -60.0..=12.0).clamping(egui::SliderClamping::Never).suffix(" dB").fixed_decimals(1))
                .on_hover_text(t(OUT_GAIN_TIP_ZH, OUT_GAIN_TIP_EN));
        });
        ui.add(egui::Label::new(RichText::new(t("只在实时 / 试听时起作用（相当于耳机音量）；转换文件不用它，文件的响度由「目标响度」决定", "Real-time / preview only (like a headphone volume); file conversion ignores it, the file loudness follows the target")).small().weak()).wrap());
        ui.add_space(3.0);
    }

    /// 右侧第一栏：风格、贴耳感、左右移动。`mix`：多音频合成中时是 Some(是否一起移动)——各自固定时左右移动不起作用，
    /// 一起移动时几个音频按这里的方式一起走。
    ///
    /// 按下风格按钮或「恢复默认」时返回 Some(需同步的文件转换目标响度)：调用方据此同步目标响度、关掉左右双人
    /// （各风格的默认状态都是一个声音）。
    fn params_left(ui: &mut egui::Ui, p: &mut Params, mix: Option<bool>) -> Option<Option<f32>> {
        let mut file_lufs = None;
        let mut pressed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t("风格", "Style")).strong());
            // 从掏耳切回人声的风格：文件转换的目标响度也恢复成 −16
            let back = (p.inear_cm > 0.0 || p.target_lufs == crate::dsp::params::BREATH_LUFS).then_some(Params::default().target_lufs);
            // 选中的风格（参数和它一致）按钮亮起来
            let active = p.active_style();
            let style_button = |ui: &mut egui::Ui, name: &str, label: &str| ui.add(egui::Button::selectable(active == Some(name), label));
            if style_button(ui, "style_ear", t("贴耳加强", "Close to ear")).on_hover_text(t("嘴离耳约 1.7 cm，另一只耳朵几乎听不到，不加房间，左右耳慢慢穿梭", "Mouth ≈1.7 cm from the ear, far ear almost silent, no room, slow sweeps between the ears")).clicked() {
                *p = p.clone().style_ear();
                file_lufs = back;
                pressed = true;
            }
            if style_button(ui, "style_ku100", t("KU100 实录", "KU100 natural")).on_hover_text(t("模拟 KU100 近耳轻语。贴近时带出原声里细微的呼吸，控制响齿音和低频气流。声源在耳边和身前随机停留，远耳保留实测房间声。", "Models close whispers on a KU100. Brings out quiet breath as the voice draws near, with controlled sibilance and low-frequency air pulses. The voice pauses near either ear or in front, with measured room sound in the far ear.")).clicked() {
                *p = p.clone().style_ku100();
                file_lufs = back;
                pressed = true;
            }
            if style_button(ui, "style_breath", t("呼吸音", "Breathing")).on_hover_text(t("处理录音中的吸气和呼气。用带自然停顿的单声道呼吸录音试听。", "Use a recording of breathing with its natural pauses.")).clicked() {
                *p = p.clone().style_breath();
                file_lufs = Some(crate::dsp::params::BREATH_LUFS);
                pressed = true;
            }
            if style_button(ui, "style_earclean", t("掏耳", "Ear cleaning")).on_hover_text(t("给棉签、挖耳勺、刮擦这类触发音用（不是给人声用的）：声源伸进耳道进进出出，带耳道共鸣、耳内低频和堵耳的闷感，另一只耳朵基本听不到，每只耳朵约 40 秒才换边。按两段 KU100 掏耳实录标定。降噪会把这类声音当噪声压掉，请把降噪关掉或调低", "For trigger sounds such as cotton swabs, ear picks and scratching (not for voice): the source moves in and out of the ear canal with canal resonance, in-ear rumble and an occluded tone; the other ear is almost silent and the ear changes about every 40 s. Calibrated to two KU100 ear-cleaning recordings. Denoise treats such sounds as noise: turn it off or down")).clicked() {
                *p = p.clone().style_earclean();
                file_lufs = Some(crate::dsp::params::EARCLEAN_LUFS);
                pressed = true;
            }
            if ui.button(t("恢复默认", "Reset")).clicked() {
                *p = Params::default();
                file_lufs = back;
                pressed = true;
            }
        });
        egui::CollapsingHeader::new(RichText::new(t("贴耳感", "Closeness")).strong()).default_open(true).show(ui, |ui| {
            egui::Grid::new("g1").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                let mut cm = p.ear_cm();
                if row(ui, t("贴耳距离", "Distance from ear"), t("嘴离耳朵多远。1.7 cm = 贴着耳朵", "How far the mouth is from the ear. 1.7 cm = touching"), egui::Slider::new(&mut cm, 0.8..=25.0).logarithmic(true).suffix(" cm").fixed_decimals(1)) {
                    p.set_ear_cm(cm);
                }
                row(ui, t("远耳压低", "Far-ear cut"), t("贴耳时另一只耳朵额外压低多少（0 = 纯物理，像真实 KU100）", "Extra attenuation of the far ear when close (0 = pure physics)"), egui::Slider::new(&mut p.isolation_db, 0.0..=40.0).suffix(" dB").fixed_decimals(0));
                row(ui, t("临场感", "Presence"), t("贴耳时近耳 3.5 kHz 和空气感", "Near-ear 3.5 kHz presence and air"), egui::Slider::new(&mut p.presence_db, 0.0..=8.0).suffix(" dB").fixed_decimals(1));
                row(ui, t("暗度", "Darkness"), t("贴耳时近耳 5 kHz 附近压低，音色更暗、更柔", "Dip around 5 kHz at the near ear: darker, softer"), egui::Slider::new(&mut p.dark_db, 0.0..=8.0).suffix(" dB").fixed_decimals(1));
                row(ui, t("温暖感", "Warmth"), t("凑近时两耳共同的低频", "Low-frequency warmth in both ears when close"), egui::Slider::new(&mut p.warmth_db, 0.0..=12.0).suffix(" dB").fixed_decimals(1));
                row(ui, t("近耳低频", "Near-ear bass"), t("贴耳时只给近耳加的低频", "Bass added only to the near ear"), egui::Slider::new(&mut p.ear_warmth_db, 0.0..=10.0).suffix(" dB").fixed_decimals(1));
                row(ui, t("远耳低频削减", "Far-ear bass cut"), t("贴耳时远耳 320 Hz 以下再压低", "Extra cut below 320 Hz at the far ear"), egui::Slider::new(&mut p.far_lf_cut_db, 0.0..=18.0).suffix(" dB").fixed_decimals(0));
                row(ui, t("气声增强", "Breath lift"), t("贴耳时把嗓音里轻的送气声 / 气息提上来（最多这么多），响的齿音不提；只用嗓音自己的气声，不加噪声。0 = 关闭", "Lifts the soft breathy part of the voice when close (by at most this much); loud sibilants are left alone. No noise is added. 0 = off"), egui::Slider::new(&mut p.breath_db, 0.0..=12.0).suffix(" dB").fixed_decimals(1));
                row(ui, t("近处气声范围", "Breath lift around the head"), t("把部分气声增强延伸到身前等近处方位。数值越大，面前轻声越清楚。到头中心 20 cm 内最强，65 cm 处淡出。0 = 只在贴耳时增强；气流扑耳单独调节。", "Extends a fraction of the breath lift to nearby positions, including in front. Full reach within 20 cm of the head center, fading out by 65 cm. At 0, lift applies only near the ear. Breath puff has its own control."), egui::Slider::new(&mut p.breath_reach, 0.0..=1.0).fixed_decimals(2));
                row(ui, t("气流扑耳", "Breath puff"), t("送气时气流扑到耳朵上的低频脉冲（耳朵痒的感觉），由嗓音自己的送气声导出。0 = 关闭，1 倍 = 标准；滑块到 8 倍，手输可更高", "Low-frequency pulses of breath hitting the ear, derived from the voice's own aspiration. 0 = off, 1x = standard; slider up to 8x, type for higher values"), egui::Slider::new(&mut p.puff, 0.0..=8.0).suffix(t(" 倍", "x")).fixed_decimals(2));
                row(ui, t("呼吸去闷", "Breath de-boom"), t("100–300 Hz 比呼吸的中频突出时，动态收低 180 Hz 附近。这里调最大衰减。", "Maximum cut around 180 Hz when low mids dominate the breath."), egui::Slider::new(&mut p.breath_deboom_db, 0.0..=12.0).suffix(" dB"));
                row(ui, t("薄声补偿", "Thin breath compensation"), t("呼吸低频偏少时补气流，3 kHz 以上过亮时柔化。0 = 关闭。", "Fills in missing low-frequency airflow and softens excess brightness above 3 kHz. 0 = off."), egui::Slider::new(&mut p.breath_tone, 0.0..=1.0).fixed_decimals(2));
                row(ui, t("耳甲共鸣", "Concha resonance"), t("贴耳时近耳 4.3 kHz 的耳甲腔共振（KU100 贴耳实录的共有特征）。0 = 关闭", "4.3 kHz concha resonance at the near ear when close. 0 = off"), egui::Slider::new(&mut p.concha_db, 0.0..=8.0).suffix(" dB").fixed_decimals(1));
                row(ui, t("贴耳轻声", "Soft when close"), t("凑到耳边时把嗓音均衡成「放轻声音」，退开时恢复（只做均衡）。0 = 关闭", "EQ towards a lowered voice when at the ear, back to normal when away (EQ only). 0 = off"), egui::Slider::new(&mut p.soft_near, 0.0..=1.5).fixed_decimals(2));
                row(ui, t("嘴的大小", "Mouth size"), t("嘴是一个开口而不是一个点：贴耳时耳廓的窄谷被平均变浅，离远了没有区别。0 = 点声源", "The mouth is an opening, not a point: narrow pinna notches smooth out when close. 0 = point source"), egui::Slider::new(&mut p.mouth_cm, 0.0..=5.0).suffix(" cm").fixed_decimals(1));
                row(ui, t("远耳高频遮挡", "Far-ear shadow"), t("贴耳时另一只耳朵 6 kHz 以上再压低（近处声源的头影更深）。0 = 关闭", "Extra cut above 6 kHz at the far ear when close (deeper head shadow for near sources). 0 = off"), egui::Slider::new(&mut p.far_shadow_db, 0.0..=12.0).suffix(" dB").fixed_decimals(1));
            });
            ui.checkbox(&mut p.ku100_eq, t("近耳用 KU100 实测校正曲线（高频更收）", "KU100-calibrated near-ear EQ (softer highs)"));
        });
        // 没启用（入耳深度为 0）时默认折叠，启用后默认展开；两种状态各记各的展开 / 折叠
        let inear_on = p.inear_cm > 0.0;
        egui::CollapsingHeader::new(RichText::new(t("入耳（掏耳）", "In the ear (ear cleaning)")).strong()).id_salt(if inear_on { "inear_on" } else { "inear_off" }).default_open(inear_on).show(ui, |ui| {
            egui::Grid::new("g1b").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                row(ui, t("入耳深度", "Depth in ear"), t("声源贴到耳朵上之后，最深伸进耳道多少。0 = 不进耳道，这一组都不起作用。给棉签、挖耳勺、刮擦这类触发音用；只有贴在耳朵上时才会伸进去，换边前先退出来", "How far the source goes into the ear canal once it is at the ear. 0 = never enters, and this whole group is off. Meant for trigger sounds (cotton swab, ear pick, scratching); it only goes in while at the ear and comes out before changing ears"), egui::Slider::new(&mut p.inear_cm, 0.0..=crate::dsp::INEAR_MAX_CM).suffix(" cm").fixed_decimals(1));
            });
            ui.add_enabled_ui(p.inear_cm > 0.0, |ui| {
                egui::Grid::new("g1c").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                    row(ui, t("进出节奏", "In / out period"), t("有声音时，一次「伸进去—退出来」大约几秒（KU100 掏耳实录 2–6 秒）", "Seconds of sound per in-and-out stroke (KU100 ear-cleaning recordings: 2–6 s)"), egui::Slider::new(&mut p.inear_period, 1.5..=12.0).suffix(" s").fixed_decimals(1));
                    row(ui, t("随机程度", "Randomness"), t("0 = 进出的节奏和深浅固定，1 = 都随机，偶尔只伸到一半、偶尔整个拿出来", "0 = fixed rhythm and depth, 1 = random, sometimes only halfway in, sometimes fully out"), egui::Slider::new(&mut p.inear_random, 0.0..=1.0).fixed_decimals(2));
                    row(ui, t("耳道共鸣", "Canal resonance"), t("伸进耳道后，物体与话筒（鼓膜）之间那段耳道的共振峰：伸得越深共振越高（约 3 → 11 kHz），听得出进出的深浅。0 = 关闭", "Resonance of the part of the canal left between the object and the microphone (eardrum): the deeper, the higher (≈3 → 11 kHz), which is what makes the depth audible. 0 = off"), egui::Slider::new(&mut p.canal_db, 0.0..=16.0).suffix(" dB").fixed_decimals(1));
                    row(ui, t("耳内低频", "In-ear rumble"), t("摩擦 / 触碰在耳道里形成的低频压力（隆隆声），由输入自己的包络导出，不加噪声。0 = 关闭，1 倍 = 标准（比摩擦声高约 15 dB，实录 10–20 dB）。输入本身低频就重（直接摩擦话筒录的）时调低", "Low-frequency pressure (rumble) that rubbing and touching build up inside the canal, derived from the input's own envelope, no noise added. 0 = off, 1x = standard (≈15 dB above the friction sound; recordings: 10–20 dB). Turn it down if the input is already bass-heavy (recorded by rubbing the microphone itself)"), egui::Slider::new(&mut p.rumble, 0.0..=3.0).suffix(t(" 倍", "x")).fixed_decimals(2));
                    row(ui, t("堵耳闷感", "Occluded tone"), t("伸进耳道时近耳 2.5 kHz 以上压低多少：物体堵住了耳道口，声音更闷。0 = 关闭", "Cut above 2.5 kHz at the near ear while inside the canal: the object blocks the entrance, so it sounds more muffled. 0 = off"), egui::Slider::new(&mut p.muffle_db, 0.0..=12.0).suffix(" dB").fixed_decimals(1));
                });
            });
            ui.add(egui::Label::new(RichText::new(t("入耳深度为 0 时这一组不起作用。人声用的气声增强、气流扑耳、忽远忽近等，掏耳时建议关掉（点上面的「掏耳」一键设置）", "This group is inactive while the depth is 0. For ear cleaning, switch off the voice-only effects (breath lift, puff, near / far sway…): the Ear cleaning style button does it in one click")).weak().small()).wrap());
        });
        egui::CollapsingHeader::new(RichText::new(t("左右移动", "Left / right motion")).strong()).default_open(true).show(ui, |ui| {
            let accent = Color32::from_rgb(70, 150, 240);
            match mix {
                Some(false) => {
                    ui.add(egui::Label::new(RichText::new(t("几个声音各自固定（多音频合成 / 左右双人）：每个声音固定在自己的位置（在左边选，或在位置图上拖），下面的移动方式暂不起作用；忽远忽近、上下、自然晃动照常。想让它们一起动，把左边的「排布」改成「一起移动」", "Voices fixed (multi-audio mix / two voices): each voice stays at its own position (pick it on the left or drag it on the map), so the motion modes below are inactive; near / far sway, up / down and micro motion still apply. Set Layout to Together on the left to move them")).small().color(accent)).wrap());
                }
                Some(true) => {
                    ui.add(egui::Label::new(RichText::new(t("几个声音一起移动（多音频合成 / 左右双人）：按下面的方式一起走，彼此始终相隔 360°/个数（两个时正好相对：一个贴左耳时另一个贴右耳）", "Voices moving together (multi-audio mix / two voices): they follow the motion below together and always stay 360° / count apart (two voices: one at the left ear while the other is at the right)")).small().color(accent)).wrap());
                }
                None => {}
            }
            ui.add_enabled_ui(mix != Some(false), |ui| Self::motion_controls(ui, p));
            egui::Grid::new("g2b").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                row(ui, t("响度随位置变化", "Level follows position"), t("1 = 物理真实（贴耳很响、到脸前会弱很多），0 = 处处一样响", "1 = physical (loud at the ear, much quieter in front), 0 = constant"), egui::Slider::new(&mut p.level_follow, 0.0..=1.0).fixed_decimals(2));
            });
            if p.mode == MotionMode::Manual && mix != Some(false) {
                ui.add(egui::Label::new(RichText::new(t("手动：声源停在你摆的位置，在左边的位置图上拖动。忽远忽近、上下、自然晃动此时不起作用", "Manual: the source stays where you put it; drag it on the map at the left. Near / far sway, up / down and micro motion are inactive")).weak().small()).wrap());
            }
        });
        pressed.then_some(file_lufs)
    }

    /// "左右移动"里的模式按钮和各模式的参数（合成时整块变灰）。
    fn motion_controls(ui: &mut egui::Ui, p: &mut Params) {
        {
            ui.horizontal(|ui| {
                let before = p.mode;
                for (m, zh, en) in [(MotionMode::Sweep, "穿梭", "Sweep"), (MotionMode::Random, "随机走位", "Random"), (MotionMode::Orbit, "环绕", "Orbit"), (MotionMode::Static, "固定", "Static"), (MotionMode::Manual, "手动", "Manual")] {
                    ui.selectable_value(&mut p.mode, m, t(zh, en));
                }
                if p.mode != before {
                    p.level_follow = Params::default_level_follow(p.mode);
                }
            });
            if p.mode == MotionMode::Sweep {
                ui.checkbox(&mut p.breath_pause_switch, t("换气时换边", "Switch ears between breaths"))
                    .on_hover_text(t("在一侧停够设定时间后，等换气间隙再移动。", "After the hold time has elapsed, wait for a gap between breaths to move."));
            }
            egui::Grid::new("g2").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                match p.mode {
                    MotionMode::Sweep | MotionMode::Random => {
                        let mut freq = p.moves_per_min();
                        if row(ui, t("移动频率", "Moves per minute"), t("说话时每分钟换几次耳朵（同时缩放停留和移动时间）", "Ear switches per minute of speech (scales hold and move time)"), egui::Slider::new(&mut freq, 2.0..=40.0).suffix(t(" 次/分", " /min")).fixed_decimals(0)) {
                            p.set_moves_per_min(freq);
                        }
                        row(ui, t("停留时间", "Hold time"), t("每只耳朵边停留多久（掏耳时一般几十秒）", "How long it stays at each ear (tens of seconds for ear cleaning)"), egui::Slider::new(&mut p.hold_s, 0.3..=60.0).logarithmic(true).suffix(" s").fixed_decimals(1));
                        row(ui, t("移动时间", "Move time"), t("从一只耳朵移到另一只耳朵用多久", "How long a crossing takes"), egui::Slider::new(&mut p.move_s, 0.3..=12.0).suffix(" s").fixed_decimals(1));
                        row(ui, t("随机程度", "Randomness"), t("时长和落点的随机程度：0 = 完全规律", "Randomness of timing and landing spots: 0 = strictly regular"), egui::Slider::new(&mut p.randomness, 0.0..=1.0).fixed_decimals(2));
                        if p.mode == MotionMode::Random {
                            row(ui, t("正面比例", "Stops in front"), t("每次停下来，有多大比例停在正前方（两只耳朵听到的差不多），其余停在耳边、侧前、侧后。KU100 轻语实录里约三成的时间两耳差不多", "Share of the stops that land in front (both ears about equal); the rest land at an ear, front-side or back-side. In KU100 whisper recordings both ears are about equal for roughly 30% of the time"), egui::Slider::new(&mut p.front, 0.0..=0.7).fixed_decimals(2));
                        }
                        if p.mode == MotionMode::Sweep {
                            row(ui, t("从远处凑近", "Approach"), t("换边时先退到约 0.5 m 再慢慢凑近的概率", "Chance that a switch backs off to ≈0.5 m and then looms in"), egui::Slider::new(&mut p.approach, 0.0..=1.0).fixed_decimals(2));
                            row(ui, t("贴耳角度", "Ear angle"), t("90 = 正侧方，100 = 略靠后", "90 = straight to the side, 100 = slightly behind"), egui::Slider::new(&mut p.ear_angle, 60.0..=140.0).suffix("°").fixed_decimals(0));
                            row(ui, t("穿过时的距离", "Pass distance"), t("经过脸前 / 脑后时离头中心多远", "Distance from the head centre while crossing"), egui::Slider::new(&mut p.pass_dist, 0.12..=0.6).suffix(" m").fixed_decimals(2));
                        }
                    }
                    MotionMode::Orbit => {
                        row(ui, t("一圈用时", "Period"), "", egui::Slider::new(&mut p.orbit_period, 3.0..=60.0).suffix(" s").fixed_decimals(0));
                        row(ui, t("半径", "Radius"), "", egui::Slider::new(&mut p.orbit_radius, 0.1..=1.5).logarithmic(true).suffix(" m").fixed_decimals(2));
                    }
                    MotionMode::Static => {
                        row(ui, t("方位角", "Azimuth"), t("0 前，90 右，180 后，270 左", "0 front, 90 right, 180 back, 270 left"), egui::Slider::new(&mut p.static_az, 0.0..=359.0).suffix("°").fixed_decimals(0));
                    }
                    MotionMode::Manual => {
                        row(ui, t("方位角", "Azimuth"), t("0 前，90 右，180 后，270 左。也可以直接在左边的位置图上拖", "0 front, 90 right, 180 back, 270 left. Or drag on the map at the left"), egui::Slider::new(&mut p.manual_az, 0.0..=359.9).suffix("°").fixed_decimals(0));
                        let mut cm = p.manual_cm();
                        if row(ui, t("离头距离", "Distance from head"), t("声源离头表面多远；在耳朵正侧方时就是离耳朵多远", "Distance from the head surface; at the side this is the distance from the ear"), egui::Slider::new(&mut cm, 0.8..=71.0).logarithmic(true).suffix(" cm").fixed_decimals(1)) {
                            p.set_manual_cm(cm);
                        }
                        row(ui, t("高度", "Height"), t("以耳朵为支点向上（正）/ 向下（负）偏多少度。也可以拖位置图右侧的竖条", "Above (+) / below (−) the ear, in degrees. Or drag the bar at the right of the map"), egui::Slider::new(&mut p.manual_tilt, -80.0..=85.0).suffix("°").fixed_decimals(0));
                    }
                }
            });
            if matches!(p.mode, MotionMode::Sweep) {
                ui.horizontal(|ui| {
                    ui.label(t("换边时从哪里绕", "Cross via"));
                    for (v, zh, en) in [(Via::Front, "脸前", "Front"), (Via::Back, "脑后", "Back"), (Via::Alternate, "交替", "Alternate"), (Via::Random, "随机", "Random")] {
                        ui.selectable_value(&mut p.via, v, t(zh, en));
                    }
                });
            }
        }
    }

    /// 背景音（实时生成的雨声）：关着时折叠成一行；"雨夜"一键打开。7 个 0–100 的滑条，音量为 0 就是关闭。
    fn params_bg(ui: &mut egui::Ui, p: &mut Params) {
        let on = p.bg_level > 0.0;
        let title = match p.bg_db() {
            Some(db) if ENGLISH.load(Ordering::Relaxed) => format!("Background (rain)  ·  {:.0} dB below the voice", -db),
            Some(db) => format!("背景音（雨声）  ·  比人声低 {:.0} dB", -db),
            None => format!("{}  ·  {}", t("背景音（雨声）", "Background (rain)"), t("关", "off")),
        };
        egui::CollapsingHeader::new(RichText::new(title).strong()).id_salt(if on { "bg_on" } else { "bg_off" }).default_open(on).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button(t("雨夜", "Rainy night")).on_hover_text(t(
                    "四面八方的小到中雨（约 2 mm/h），重心绕着头慢慢转（8D 环绕）；身边地上的落地声比参考的 8D 雨声轻一点；不隔窗、没有屋檐滴水；雨声比人声低约 18 dB。只改背景音这几项。频谱、包围感、流动和落地声都对着一段 8D 环绕雨声校过",
                    "Light-to-moderate rain (about 2 mm/h) all around you, its centre slowly circling your head (8D surround); the drops landing nearby are a little softer than in the 8D reference; no window, no eave drips; 18 dB below the voice. Only changes the background settings. Spectrum, surround, flow and landing drops are calibrated against an 8D surround rain recording",
                )).clicked() {
                    *p = p.clone().bg_rain_night();
                }
                if on && ui.button(t("关", "Off")).clicked() {
                    p.bg_level = 0.0;
                }
                ui.add(egui::Label::new(RichText::new(t("实时生成，不是录音；人声不受影响", "Generated live, not a recording; the voice is untouched")).small().weak()).wrap());
            });
            let slider = |ui: &mut egui::Ui, label: &str, tip: &str, v: &mut f32, lo: f32, fmt: &dyn Fn(f64) -> String| {
                ui.label(label).on_hover_text(tip);
                let s = egui::Slider::new(v, lo..=100.0)
                    .step_by(1.0)
                    .fixed_decimals(0)
                    .custom_formatter(|x, _| fmt(x))
                    .custom_parser(|s| s.trim().split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '−')).find(|w| !w.is_empty() && *w != "-").and_then(|w| w.replace('−', "-").parse::<f64>().ok()));
                ui.add(s).on_hover_text(tip);
                ui.end_row();
            };
            egui::Grid::new("gbg").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                slider(ui, t("音量", "Volume"), t("雨声比人声的目标响度低多少：1 = 低 40 dB，100 = 一样响；0 = 关闭。整片雨声按这个定响度，落地声和滴水在它之上", "How far below the voice's target loudness the rain sits: 1 = 40 dB below, 100 = as loud; 0 = off. The rain bed follows this; landing drops and drips sit on top of it"), &mut p.bg_level, 0.0, &|x| if x < 0.5 { t("0 关", "0 off").to_string() } else { format!("{x:.0} · −{:.0} dB", 40.0 - 0.4 * x) });
                slider(ui, t("雨量", "Rain"), t("毛毛雨（0.3 mm/h）… 暴雨（60 mm/h）。雨越大，大雨滴越多、越密、越低沉，落地声也越多；也会响一点（±5 dB）", "Drizzle (0.3 mm/h) … downpour (60 mm/h). Heavier rain has more, denser, deeper drops and more landing drops, and is a little louder (±5 dB)"), &mut p.rain, 0.0, &|x| format!("{x:.0} · {:.1} mm/h", 0.3 * 200f64.powf(x / 100.0)));
                slider(ui, t("左右", "Left / right"), t("雨从哪边来：−100 = 都在左边，0 = 四周均匀，100 = 都在右边（比如窗户在右边）", "Where the rain comes from: −100 = all on the left, 0 = evenly around, 100 = all on the right (e.g. a window on your right)"), &mut p.bg_pan, -100.0, &|x| if x.abs() < 0.5 { t("中", "center").to_string() } else if x < 0.0 { format!("{} {:.0}", t("左", "L"), -x) } else { format!("{} {x:.0}", t("右", "R")) });
                slider(ui, t("包围感", "Surround"), t("0 = 只从一个方向来（「左右」定的那个方向），100 = 从四面八方来", "0 = from a single direction (set by Left / right), 100 = from all around"), &mut p.bg_width, 0.0, &|x| format!("{x:.0}"));
                slider(ui, t("流动", "Flow"), t("雨声的重心绕着头慢慢转（8D 环绕，平均 26 秒一圈，快慢随机）：那个方向的雨落得密一些。0 = 不动，50 = 和参考的 8D 雨声一样，100 = 更明显", "The rain's centre slowly circles your head (8D surround, about 26 s per turn, speed varies): more drops fall on that side. 0 = still, 50 = like the 8D reference, 100 = stronger"), &mut p.bg_flow, 0.0, &|x| if x < 0.5 { t("0 不动", "0 still").to_string() } else { format!("{x:.0}") });
                slider(ui, t("隔窗", "Window"), t("0 = 站在雨里，100 = 关着窗在屋里听：墙和玻璃挡掉高频（频率越高挡得越多），只剩闷闷的、偏低沉的雨声；落地声一起闷掉，屋檐滴水挡一半", "0 = standing in the rain, 100 = indoors with the window shut: walls and glass block the highs, leaving a muffled, deeper rain; landing drops are muffled too, eave drips half"), &mut p.bg_indoor, 0.0, &|x| format!("{x:.0}"));
                slider(ui, t("落地声", "Landing drops"), t("身边地上一个个雨点「噗」的一声（低沉、短促，按 HRTF 摆在四周地上）有多明显：0 = 没有，60 ≈ 参考的 8D 雨声，每格 0.2 dB；雨越大落得越多。屋檐滴水的响度也跟着它", "How noticeable the single drops landing on the ground around you are (a low, short thud placed around you with the HRTF): 0 = none, 60 ≈ the 8D reference, 0.2 dB per step; heavier rain brings more of them. Eave drips follow this loudness too"), &mut p.rain_near, 0.0, &|x| if x < 0.5 { t("0 无", "0 none").to_string() } else { format!("{x:.0} · {:+.0} dB", 0.2 * (x - 60.0)) });
                let land = p.rain_near;
                slider(ui, t("屋檐滴水", "Eave drips"), t("屋檐往石阶 / 积水的地上滴的「嗒」：1–3 处固定的位置，各有各的节奏，溅水声高低不同。越大滴得越快、地方越多；0 = 没有。多响跟着「落地声」，落地声为 0 时听不到", "Drips from the eaves onto wet stone: 1–3 fixed spots, each with its own rhythm and splash pitch. Higher = faster and more spots; 0 = none. Their loudness follows Landing drops, so none are heard while that is 0"), &mut p.rain_drip, 0.0, &|x| if x < 0.5 { t("0 无", "0 none").to_string() } else if land < 0.5 { format!("{x:.0} · {}", t("落地声为 0", "landing off")) } else { format!("{x:.0} · {} {}", if x < 34.0 { 1 } else if x < 67.0 { 2 } else { 3 }, t("处", "spots")) });
                slider(ui, t("雷声", "Thunder"), t("远处的雷（可选，默认没有）：一道曲折的闪电通道上每一段发出一个激波，按距离先后到达、按 HRTF 从天上各个方向来，远的只剩低沉的隆隆声，还有地形的回声。0 = 没有；越大越近、越频繁、越响：30 ≈ 两分钟一次、5 km 外，50 ≈ 77 秒、3.5 km，100 ≈ 25 秒、1.5 km。拉上来以后 3–10 秒内来第一声", "Distant thunder (optional, off by default): every segment of a tortuous lightning channel sends out a shock wave that arrives in order of distance, from its own direction in the sky (HRTF); far thunder keeps only the low rumble, plus terrain echoes. 0 = none; higher = closer, more frequent and louder: 30 ≈ every 2 min at 5 km, 50 ≈ 77 s at 3.5 km, 100 ≈ 25 s at 1.5 km. The first one comes 3–10 s after you turn it up"), &mut p.bg_thunder, 0.0, &|x| if x < 0.5 { t("0 无", "0 none").to_string() } else { let (i, d) = (crate::dsp::thunder::interval_s(x as f32 / 100.0), crate::dsp::thunder::distance_km(x as f32 / 100.0)); if i >= 90.0 { format!("{x:.0} · {:.0} {} · {d:.1} km", i / 60.0, t("分钟", "min")) } else { format!("{x:.0} · {i:.0} s · {d:.1} km") } });
            });
        });
    }

    /// 右侧第二栏：忽远忽近、KU100 临场感、人声处理、输出。`fixed_mix`：多音频合成·各自固定中（各音源用"固定"，
    /// 手动摆位不起作用，忽远忽近、上下照常）。
    fn params_right(ui: &mut egui::Ui, p: &mut Params, fixed_mix: bool) {
        Self::params_bg(ui, p);
        egui::CollapsingHeader::new(RichText::new(t("忽远忽近", "Near / far sway")).strong()).default_open(true).show(ui, |ui| {
            if p.mode == MotionMode::Manual && !fixed_mix {
                ui.disable(); // 手动摆位时不起作用（多音频合成·各自固定时各音源照常忽远忽近）
            }
            egui::Grid::new("g3").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                row(ui, t("幅度", "Amount"), t("随机地凑近、往后让的幅度（0 = 关闭）", "How far it leans in and out (0 = off)"), egui::Slider::new(&mut p.sway, 0.0..=2.0).fixed_decimals(2));
                row(ui, t("节奏", "Period"), t("说话时一次凑近—退开大约几秒", "Seconds of speech per lean-out / lean-in cycle"), egui::Slider::new(&mut p.sway_period, 2.0..=14.0).suffix(" s").fixed_decimals(1));
                row(ui, t("随机程度", "Randomness"), t("0 = 固定节奏和深浅，1 = 时长和深浅都随机", "0 = fixed rhythm and depth, 1 = random timing and depth"), egui::Slider::new(&mut p.sway_random, 0.0..=1.0).fixed_decimals(2));
                row(ui, t("跟随音量", "Follow loudness"), t("声音大时往后让、轻声时更贴近，听着更舒服", "Louder → leans back, softer → comes closer; more comfortable"), egui::Slider::new(&mut p.sway_follow, 0.0..=2.0).fixed_decimals(2));
                row(ui, t("退开时的音量", "Level when back"), t("退开时耳边音量保留物理衰减的比例（真人退开会说大声一点；KU100 实录 0.4–0.56）", "Share of the physical level drop kept when leaning back (talkers raise their voice; KU100 recordings 0.4–0.56)"), egui::Slider::new(&mut p.sway_level, 0.0..=1.0).fixed_decimals(2));
                row(ui, t("停顿时的动作", "Motion in pauses"), t("停顿时动作的速度：0.13 = 几乎不动，下一句从原位置接着说；1 = 一直在动。实时（主播）模式下，停顿超过 1 秒会恢复正常速度，话少时声音也一直在动", "Motion speed during pauses: 0.13 = almost still, 1 = always moving. In real-time mode motion resumes at full speed after 1 s of pause, so it keeps moving when you talk little"), egui::Slider::new(&mut p.pause_motion, 0.0..=1.0).fixed_decimals(2));
                row(ui, t("自然晃动", "Micro motion"), t("人体自然的微小晃动（1 约 1.2 cm）", "Natural small body sway (1 ≈ 1.2 cm)"), egui::Slider::new(&mut p.micro, 0.0..=2.0).fixed_decimals(2));
            });
        });
        egui::CollapsingHeader::new(RichText::new(t("上下", "Up / down")).strong()).default_open(true).show(ui, |ui| {
            if p.mode == MotionMode::Manual && !fixed_mix {
                ui.disable(); // 手动摆位时不起作用（高度在"左右移动"里调，或拖位置图右侧的竖条）
            }
            egui::Grid::new("g7").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                row(ui, t("高度", "Height"), t("嘴相对耳朵向上（正）/ 向下（负）偏多少度，离耳朵的距离不变；0 = 与耳朵齐平", "Mouth above (+) / below (−) the ear, in degrees, at the same distance from the ear; 0 = level with the ear"), egui::Slider::new(&mut p.height, -60.0..=75.0).suffix("°").fixed_decimals(0));
                row(ui, t("自动上下", "Auto up / down"), t("嘴在耳朵上方 / 下方之间慢慢换位置的幅度（0 = 关闭）", "How far the mouth drifts above / below the ear on its own (0 = off)"), egui::Slider::new(&mut p.updown, 0.0..=60.0).suffix("°").fixed_decimals(0));
                row(ui, t("节奏", "Period"), t("说话时上下一个来回大约几秒", "Seconds of speech per up / down cycle"), egui::Slider::new(&mut p.updown_period, 3.0..=30.0).suffix(" s").fixed_decimals(1));
                row(ui, t("随机程度", "Randomness"), t("0 = 节奏和高低固定，1 = 时长和高低都随机", "0 = fixed rhythm and height, 1 = random timing and height"), egui::Slider::new(&mut p.updown_random, 0.0..=1.0).fixed_decimals(2));
                row(ui, t("从头顶绕", "Over the head"), t("换耳时改成从头顶绕过去的概率（穿梭 / 随机走位）：0 = 只从脸前 / 脑后绕，1 = 每次都从头顶绕", "Chance that an ear switch goes over the top of the head (sweep / random): 0 = front / back only, 1 = always over the top"), egui::Slider::new(&mut p.over_top, 0.0..=1.0).fixed_decimals(2));
            });
            ui.add(egui::Label::new(RichText::new(t("上下比左右、远近弱，因人而异；气声、轻语更明显", "Height is a weaker cue than left / right or distance and varies between listeners; clearer on breathy voices")).weak().small()).wrap());
        });
        egui::CollapsingHeader::new(RichText::new(t("KU100 临场感", "KU100 presence")).strong()).default_open(true).show(ui, |ui| {
            ui.checkbox(&mut p.room, t("真实房间（KU100 实测，远耳听到的是房间声而不是寂静）", "Real room (measured with a KU100: the far ear hears the room, not silence)"));
            ui.checkbox(&mut p.static_ambience, t("固定原声底噪", "Keep recorded ambience stationary"))
                .on_hover_text(t("持续的房间底声留在固定位置，说话和短促呼吸随声源移动。", "Keep steady room tone in place while speech and short breaths move with the source."));
            egui::Grid::new("g4").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.add_enabled_ui(p.room, |ui| ui.label(t("房间声大小", "Room level")));
                ui.add_enabled(p.room, egui::Slider::new(&mut p.wet_db, -12.0..=9.0).clamping(egui::SliderClamping::Never).suffix(" dB").fixed_decimals(0));
                ui.end_row();
                ui.label(t("耳廓", "Pinna")).on_hover_text(t("KU100：人工头自己的耳廓（按平均人耳做的，给别人听选这个最稳）；真人 A–G：HUTUBS 实测的真人耳廓，自己听时挑前后、上下最分得清的那只", "KU100: the dummy head's own ear (an average ear, safest for an audience). Human A–G: measured human ears; for your own listening pick the one that gives the clearest front/back and up/down"));
                let cur = match crate::dsp::pinna::pinnas().index(&p.pinna) {
                    Some(i) => format!("{} {}", t("真人", "Human"), crate::dsp::pinna::pinnas().items[i].name),
                    None => t("KU100（人工头）", "KU100 (dummy head)").to_string(),
                };
                egui::ComboBox::from_id_salt("pinna").width(190.0).selected_text(cur).show_ui(ui, |ui| {
                    ui.selectable_value(&mut p.pinna, "KU100".to_string(), t("KU100（人工头）", "KU100 (dummy head)"));
                    for q in &crate::dsp::pinna::pinnas().items {
                        ui.selectable_value(&mut p.pinna, q.name.clone(), format!("{} {}", t("真人", "Human"), q.name));
                    }
                });
                ui.end_row();
                ui.label(t("回放补偿", "Playback EQ")).on_hover_text(t("扩散场：通用；KU100 原机：和真实 KU100 录音一样（高频略暗）；或按你的耳机型号补偿", "Diffuse: generic; Native: like a real KU100 recording (slightly darker); or pick your headphone model"));
                let cur = match &p.playback {
                    Playback::Diffuse => t("通用扩散场", "Diffuse field").to_string(),
                    Playback::Native => t("KU100 原机音色", "KU100 native").to_string(),
                    Playback::Headphone(n) => n.clone(),
                };
                egui::ComboBox::from_id_salt("pb").width(190.0).selected_text(cur).show_ui(ui, |ui| {
                    ui.selectable_value(&mut p.playback, Playback::Diffuse, t("通用扩散场", "Diffuse field"));
                    ui.selectable_value(&mut p.playback, Playback::Native, t("KU100 原机音色", "KU100 native"));
                    for (n, _) in &tables().headphones {
                        ui.selectable_value(&mut p.playback, Playback::Headphone(n.clone()), n);
                    }
                });
                ui.end_row();
            });
        });
        egui::CollapsingHeader::new(RichText::new(t("人声处理", "Voice")).strong()).default_open(true).show(ui, |ui| {
            egui::Grid::new("g5").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                row(ui, t("去齿音", "De-ess"), t("只在 s / sh / x 出现时压低 4.5 kHz 以上，最多压这么多", "Ducks >4.5 kHz only on sibilants, by at most this much"), egui::Slider::new(&mut p.deess_db, 0.0..=10.0).suffix(" dB").fixed_decimals(1));
                row(ui, t("强辅音柔化", "Soften sharp consonants"), t("只在强辅音突出时收轻 5 kHz 附近的窄频带。调高会让刺耳的字音更柔，0 = 关闭。", "Softens a band around 5 kHz on prominent consonants. Increase to tame sharp syllables. 0 = off."), egui::Slider::new(&mut p.sharp_soften_db, 0.0..=8.0).suffix(" dB").fixed_decimals(1));
                row(ui, t("轻声化", "Soft voice"), t("把嗓音均衡成「压低声音说话」（只做均衡，不改嗓音）", "EQ towards a lowered-voice spectrum (EQ only)"), egui::Slider::new(&mut p.soft_voice, 0.0..=1.5).fixed_decimals(2));
            });
        });
        egui::CollapsingHeader::new(RichText::new(t("输出", "Output")).strong()).default_open(true).show(ui, |ui| {
            ui.checkbox(&mut p.auto_level, t("自动响度（实时 / 试听：慢慢把音量拉到目标）", "Auto level (real-time / preview: slowly rides to the target)"));
            egui::Grid::new("g6").num_columns(2).min_col_width(COL).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.add_enabled_ui(p.auto_level, |ui| ui.label(t("目标响度", "Target")));
                ui.add_enabled(p.auto_level, egui::Slider::new(&mut p.target_lufs, -30.0..=-10.0).clamping(egui::SliderClamping::Never).suffix(" LUFS").fixed_decimals(0));
                ui.end_row();
                row(ui, t("峰值上限", "Ceiling"), t("限制器的真峰值上限", "True-peak ceiling of the limiter"), egui::Slider::new(&mut p.ceiling_db, -6.0..=-0.1).suffix(" dBTP").fixed_decimals(1));
                ui.label(t("随机种子", "Seed")).on_hover_text(t("换一个数字就换一套走位（下次开始时生效）", "A different number gives a different path (applies on next start)"));
                ui.add(egui::DragValue::new(&mut p.seed).speed(0.1));
                ui.end_row();
            });
        });
    }

    /// 在控件修改参数的当帧提交，音频线程下一次回调直接使用最新快照。
    fn sync_params(&mut self) {
        let mode = self.s.params.mode;
        if mode != self.last_mode {
            if mode == MotionMode::Manual {
                self.auto_mode = self.last_mode;
                self.auto_level_follow = self.sent.level_follow;
                if self.rt.is_some()
                    && self.s.params.manual_az == self.sent.manual_az
                    && self.s.params.manual_dist == self.sent.manual_dist
                    && self.s.params.manual_tilt == self.sent.manual_tilt
                {
                    // 运行中切到手动：声源就停在此刻所在的位置，从这里开始拖
                    let (up, az, dist) = up_angle(self.tele.az, self.tele.el, self.tele.r);
                    let p = &mut self.s.params;
                    p.manual_az = az;
                    p.manual_dist = dist.clamp(R_MIN, RADAR_MAX_R);
                    p.manual_tilt = up.clamp(-80.0, 85.0);
                }
            }
            self.last_mode = mode;
        }
        let changed = self.s.params != self.sent;
        if changed {
            let cfg = Arc::new(EngineConfig::new(self.s.params.clone(), Some(&self.cfg)));
            if let Some(rt) = self.rt.as_ref().filter(|_| !self.rt_mix) {
                rt.set_config(cfg.clone());
            }
            self.cfg = cfg;
            self.sent = self.s.params.clone();
        }
        // 合成试听：参数、位置或音量变了，重新算各音源的配置交给音频线程（文件组变了时 sync_mix 会停掉重来）
        if self.rt_mix && self.rt.is_some() {
            // 主播模式的左右双人：两个声音都是麦克风，不做响度对齐
            let lufs = if self.mic_duo() { Some(vec![None; 2]) } else { self.mix_audio.as_ref().filter(|m| m.paths == self.mix_paths()).and_then(|m| m.tracks(self.align_on())).map(|t| t.lufs) };
            self.sync_balance();
            let bal = self.cur_balance();
            if let Some(lufs) = lufs {
                let strength = self.s.mix_balance / 100.0;
                // 强度单独比：实时页的左右双人没有事先算好的数据（混音器边听边算），只看强度
                let bal_changed = self.mix_sent.as_ref().map_or(true, |(_, st)| {
                    st.balance_strength != strength
                        || match (&st.balance, &bal) {
                            (None, None) => false,
                            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                            _ => true,
                        }
                });
                let delays_changed = self.mix_sent.as_ref().map_or(true, |(_, st)| st.delays[..] != self.mix_delays()[..st.delays.len()]);
                if changed || bal_changed || delays_changed || self.mix_sent.as_ref().map_or(true, |(v, _)| *v != self.mix_voices()) {
                    let (voices, setup) = self.mix_setup(&lufs);
                    if let Some(rt) = &self.rt {
                        rt.set_mix(setup.clone());
                    }
                    self.mix_sent = Some((voices, setup));
                }
            }
        }
    }

    /// 每帧的后台事务：试听解码完成、转换完成、设备出错。
    fn poll(&mut self, ctx: &egui::Context) {
        ENGLISH.store(self.s.english, Ordering::Relaxed);
        if let Some(rx) = &self.mix_decoding {
            if let Ok(r) = rx.try_recv() {
                self.mix_decoding = None;
                match r {
                    Ok(m) => {
                        if self.mixing() && m.paths == self.mix_paths() && m.ch == self.s.file_channel {
                            if std::mem::take(&mut self.mix_shift) {
                                if let Some(Ok(a)) = &m.aligned {
                                    self.preview_s = (self.preview_s - a.al.starts[0] as f64 / FS as f64).max(0.0);
                                }
                            }
                            let align_err = match &m.aligned {
                                Some(Err(e)) if self.s.mix_align => Some(e.clone()),
                                _ => None,
                            };
                            self.mix_audio = Some(m);
                            if self.preview_pending {
                                self.start_preview();
                            } else if self.duo() {
                                self.status = t("左右双人已就绪：点「试听」或拖动播放条，同一个声音一左一右两个人说", "Two voices ready: press Preview or drag the timeline to hear the same voice on both sides").to_string();
                            } else {
                                self.status = t("要合成的音频已就绪：点「试听」或拖动播放条，几个音频会同时播放", "Files ready to mix: press Preview or drag the timeline to hear them together").to_string();
                            }
                            if let Some(e) = align_err {
                                self.status = format!("{}：{e}", t("自动对齐", "Auto-align"));
                            }
                        }
                        // 否则解码期间文件组或声道变了：丢掉，sync_mix 会按新的重新解码
                    }
                    Err((paths, ch, e)) => {
                        self.mix_failed = Some((paths, ch));
                        self.preview_pending = false;
                        self.status = e;
                    }
                }
            }
        }
        if let Some(rx) = &self.decoding {
            if let Ok(r) = rx.try_recv() {
                self.decoding = None;
                match r {
                    Ok(d) => {
                        self.decoded = Some(d);
                        if self.preview_pending {
                            self.start_preview();
                        } else {
                            self.status = t("音频已就绪，点击或拖动播放条即可试听", "Audio ready; click or drag the timeline to preview").to_string();
                        }
                    }
                    Err(e) => {
                        if self.duo() {
                            self.mix_failed = Some((self.mix_paths(), self.s.file_channel)); // 左右双人不要每帧重试
                        }
                        self.preview_pending = false;
                        self.status = e;
                    }
                }
            }
        }
        // 转换任务：把各文件的状态抄到列表上；全部结束后写总结
        if let Some(j) = &self.job {
            for (p, m) in j.paths.iter().zip(j.status.iter()) {
                if let Ok(g) = m.lock() {
                    self.statuses.insert(p.clone(), g.clone());
                }
            }
        }
        if self.job.as_ref().map_or(false, |j| j.finished.load(Ordering::Acquire)) {
            let j = self.job.take().unwrap();
            let all: Vec<ItemStatus> = j.status.iter().map(|m| m.lock().map(|g| g.clone()).unwrap_or(ItemStatus::Failed(String::new()))).collect();
            let ok = all.iter().filter(|s| matches!(s, ItemStatus::Done(..))).count();
            let n = all.len();
            self.report = match (n, all.first()) {
                (1, Some(ItemStatus::Done(rep, out))) => match &j.mix {
                    Some(desc) => mix_report_text(rep, out, desc),
                    None => report_text(rep, out),
                },
                (1, Some(ItemStatus::Failed(e))) => format!("{}：{e}", t("失败", "Failed")),
                _ => {
                    let cancelled = all.iter().filter(|s| matches!(s, ItemStatus::Failed(e) if e == "已取消")).count();
                    let mut r = format!("{} {ok} / {n} {}，{} {}", t("已转换", "Converted"), t("个文件", "files"), t("用时", "took"), playback_time(j.t0.elapsed().as_secs_f64()));
                    if n - ok - cancelled > 0 {
                        r.push_str(&format!("；{} {}（{}）", n - ok - cancelled, t("个失败", "failed"), t("鼠标移到列表里的「失败」上看原因", "hover “Failed” in the list for the reason")));
                    }
                    if cancelled > 0 {
                        r.push_str(&format!("；{} {}", cancelled, t("个已取消", "cancelled")));
                    }
                    r.push_str(&format!("\n{}：{}", t("输出位置", "Output"), match &self.s.out_dir {
                        Some(d) => d.display().to_string(),
                        None => t("各自源文件旁边", "beside each source").to_string(),
                    }));
                    r
                }
            };
        }
        let mut stop = false;
        if let Some(rt) = &self.rt {
            self.tele = rt.shared.telemetry();
            if self.rt_mix {
                self.mix_tele = rt.shared.voice_telemetry();
            }
            if let Some(e) = rt.shared.take_error() {
                self.status = e;
                stop = true;
            }
            if self.rt_preview && !self.preview_dragging {
                self.preview_s = rt.shared.play_pos.load(Ordering::Relaxed) as f64 / FS as f64;
            }
            if self.rt_preview && rt.shared.preview_finished() {
                self.status = t("试听结束", "Preview finished").to_string();
                stop = true;
            }
        } else {
            self.tele.act = 0.0;
            self.tele.in_db = -120.0;
            self.tele.out_db = -120.0;
            self.tele.gr_db = 0.0;
        }
        if stop {
            self.stop();
        }
        // 拖进窗口的文件
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            // 拖进来的文件（可多个、可以是文件夹）加进列表，选中第一个用来试听
            self.add_files(dropped, true);
            self.s.tab = Tab::File;
        }
        if self.rt.is_some() || self.job.is_some() || self.decoding.is_some() || self.mix_decoding.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
}

impl eframe::App for App {
    /// 截图自检时模拟一次鼠标拖动（环境变量 AUTO_ASMR_SHOT_DRAG="x0,y0,x1,y1"，窗口坐标）：
    /// 第 5 帧在 (x0, y0) 按下，之后几帧移到 (x1, y1)，第 13 帧松开；截图在第 20 帧。
    /// 文件试听自检从解码完成后开始计数，确保播放条已经出现。
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let (Some((_, frame)), Some([x0, y0, x1, y1])) = (&self.shot, self.shot_drag) {
            let frame = if self.file_in.is_some() {
                let ready = if self.mixing() { self.mix_audio.as_ref().map_or(false, |m| m.tracks(self.align_on()).is_some()) } else { self.decoded.is_some() };
                if !ready {
                    return;
                }
                self.shot_drag_frame += 1;
                self.shot_drag_frame
            } else {
                *frame
            };
            let (a, b) = (egui::pos2(x0, y0), egui::pos2(x1, y1));
            let which = if self.shot_drag_right { egui::PointerButton::Secondary } else { egui::PointerButton::Primary };
            let button = |pos, pressed| egui::Event::PointerButton { pos, button: which, pressed, modifiers: Default::default() };
            match frame {
                4 => raw.events.push(egui::Event::PointerMoved(a)),
                5 => raw.events.push(button(a, true)),
                6..=12 => raw.events.push(egui::Event::PointerMoved(a + (b - a) * ((frame - 5) as f32 / 7.0))),
                13 => raw.events.push(button(b, false)),
                _ => {}
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("auto-asmr");
                ui.add_space(12.0);
                let before = self.s.tab;
                ui.selectable_value(&mut self.s.tab, Tab::Realtime, RichText::new(t("  实时（主播）  ", "  Real-time  ")).size(15.0));
                ui.selectable_value(&mut self.s.tab, Tab::File, RichText::new(t("  文件转换  ", "  File  ")).size(15.0));
                if before != self.s.tab {
                    self.stop();
                    self.status.clear();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut self.s.english, true, "English");
                    ui.selectable_value(&mut self.s.english, false, "中文");
                    egui::widgets::global_theme_preference_buttons(ui);
                    if ui.button(t("导出配置", "Export config")).clicked() {
                        self.export_config();
                    }
                    if ui.button(t("导入配置", "Import config")).clicked() {
                        self.import_config();
                    }
                });
            });
            ui.add_space(4.0);
        });
        if self.s.tab == Tab::File {
            self.sync_mix();
        self.sync_balance();
        }
        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // 最右下角：项目的 GitHub
                ui.hyperlink_to(RichText::new(t("给个star", "Give it a star")).strong().small(), GITHUB_URL)
                    .on_hover_text(format!("{}\n{GITHUB_URL}", t("打开本项目的 GitHub 页面，觉得好用就点个 Star", "Open this project on GitHub; star it if you find it useful")));
                ui.label(RichText::new("GitHub").small());
                ui.separator();
                ui.hyperlink_to(RichText::new("随意Official").strong().small(), "https://space.bilibili.com/79356601")
                    .on_hover_text(t("打开研发人的 B 站主页", "Open the developer's Bilibili profile"));
                ui.label(RichText::new("研发人 B站").small());
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(if self.status.is_empty() { t("纯 CPU 处理 · KU100 人工头实测 HRIR（TH Köln，CC BY 4.0）· 请戴耳机", "CPU only · KU100 dummy-head HRIRs (TH Köln, CC BY 4.0) · use headphones").to_string() } else { self.status.clone() }).small()).truncate());
                });
            });
        });
        egui::Panel::left("left").resizable(false).exact_size(470.0).show(ui, |ui| {
            egui::Panel::bottom("left_bottom").resizable(false).show(ui, |ui| {
                Self::ui_denoise(ui, &mut self.s.params);
            });
            egui::ScrollArea::vertical().id_salt("leftscroll").show(ui, |ui| {
                ui.add_space(6.0);
                match self.s.tab {
                    Tab::Realtime => self.ui_realtime(ui),
                    Tab::File => self.ui_file(ui),
                }
            });
        });
        let mix = self.mix_view();
        let layout = mix.then_some(self.s.mix_follow);
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("rightscroll").show(ui, |ui| {
                let p = &mut self.s.params;
                let mut style_lufs = None;
                ui.columns(2, |c| {
                    style_lufs = Self::params_left(&mut c[0], p, layout);
                    Self::params_right(&mut c[1], p, layout == Some(false));
                });
                if let Some(l) = style_lufs {
                    if let Some(l) = l {
                        self.s.file_lufs = l;
                    }
                    // 各风格的默认状态都是一个声音：按风格按钮 / 恢复默认时关掉左右双人（文件转换页和实时页的都关）
                    self.set_duo(0.0);
                    self.set_rt_duo(0.0);
                }
            });
        });

        // 3D 观察窗：只有打开时才绘制。合成时画每个音源（各自的颜色和编号）
        if self.s.view3d {
            let mut open = true;
            let (tele, active) = (self.tele, self.rt.is_some());
            let dots: Vec<view3d::Dot> = if mix {
                let live = active && self.rt_mix;
                self.mix_positions().iter().enumerate().map(|(k, q)| view3d::Dot { xyz: view3d::to_xyz(q[0], q[1], q[2]), col: VOICE_COLORS[k], label: (k + 1).to_string(), speaking: live && q[3] > 0.5 }).collect()
            } else {
                Vec::new()
            };
            let (v, params) = (&mut self.view3d, &mut self.s.params);
            egui::Window::new(t("3D 视图", "3D view"))
                .id(egui::Id::unique("view3d_window"))
                .open(&mut open)
                .default_pos([480.0, 300.0])
                .default_size([360.0, 400.0])
                .min_size([240.0, 280.0])
                .resizable(true)
                .show(&ctx, |ui| v.ui(ui, &tele, active, params, &dots));
            if !open {
                self.s.view3d = false;
            }
        } else {
            self.view3d.clear_trail();
        }

        self.sync_params();

        if self.rt.is_some() || self.job.is_some() || self.decoding.is_some() || self.mix_decoding.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        // 截图自检：等界面稳定后截一张图存成 PNG，然后退出
        if let Some(f) = self.demo.take() {
            self.set_file(f);
            self.start_s = 4.0;
            self.preview_s = 4.0;
            self.start_preview();
        }
        let wait = self.shot_wait.unwrap_or(if self.file_in.is_some() { 150 } else { 20 });
        let drag_note = self.shot_drag.map(|_| {
            let mut s = format!("手动位置：方位角 {:.1}°，到头中心 {:.3} m，高度 {:.1}°", self.s.params.manual_az, self.s.params.manual_dist, self.s.params.manual_tilt);
            if self.mixing() || self.mic_duo() {
                for (k, v) in self.mix_voices().iter().enumerate() {
                    s.push_str(&format!("\n合成音源 {}：方位角 {:.1}°，到头中心 {}", k + 1, v.az, v.dist.map_or("贴耳".to_string(), |d| format!("{d:.3} m"))));
                }
            }
            s
        });
        if let Some((path, frames)) = &mut self.shot {
            *frames += 1;
            if *frames == wait {
                if let Some(n) = &drag_note {
                    eprintln!("{n}");
                }
                let path = path.clone();
                let c2 = ctx.clone();
                ctx.request_screenshot(move |img| {
                    let _ = save_png(&path, &img);
                    c2.send_viewport_cmd(egui::ViewportCommand::Close);
                });
            }
            ctx.request_repaint();
        }
    }

    /// 关闭窗口时还在转换：取消，等工作线程收尾（删掉临时文件和写了一半的输出），最多等 3 秒。
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Relaxed);
            let t0 = std::time::Instant::now();
            while !j.finished.load(Ordering::Acquire) && t0.elapsed().as_secs_f32() < 3.0 {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.shot.is_none() {
            eframe::set_value(storage, eframe::APP_KEY, &self.s);
        }
    }
}

/// 读取配置文件（格式见 [`crate::config`]，安卓版也用同一个格式）。缺的项用默认值。
fn parse_config(text: &str) -> Result<Saved, String> {
    crate::config::parse(text).map_err(|e| match e {
        ConfigError::NotAutoAsmr => t("文件里没有 auto-asmr 的配置", "No auto-asmr configuration in this file").to_string(),
        ConfigError::Parse(e) => e,
    })
}

/// 总进度（按估计时长加权，0–1）、已结束的文件数、正在转换的文件数。
fn job_progress(j: &Job) -> (f32, usize, usize) {
    let est = j.est.lock().map(|g| g.clone()).unwrap_or_default();
    let (mut sum, mut tot, mut done, mut run) = (0f64, 0f64, 0, 0);
    for (i, m) in j.status.iter().enumerate() {
        let w = est.get(i).copied().unwrap_or(1.0);
        let f = match m.lock().map(|g| g.clone()) {
            Ok(ItemStatus::Running(f, _)) => {
                run += 1;
                f as f64
            }
            Ok(ItemStatus::Done(..)) | Ok(ItemStatus::Failed(_)) => {
                done += 1;
                1.0
            }
            _ => 0.0,
        };
        sum += w * f;
        tot += w;
    }
    ((sum / tot.max(1e-9)) as f32, done, run)
}

/// 单个文件转换完成后的说明（文字在库里，安卓版用同一份）。
fn report_text(rep: &ConvertReport, out: &Path) -> String {
    crate::offline::report_text(rep, &out.display().to_string(), ENGLISH.load(Ordering::Relaxed))
}

/// 合成完成后的说明。`desc`：参与合成的音频和各自的位置。
fn mix_report_text(rep: &ConvertReport, out: &Path, desc: &str) -> String {
    crate::offline::mix_report_text(rep, &out.display().to_string(), desc, ENGLISH.load(Ordering::Relaxed))
}

/// 左右双人的延迟滑条（0–300 ms，0 = 关），文件页和实时页共用。
fn duo_slider(v: &mut f32) -> egui::Slider<'_> {
    egui::Slider::new(v, 0.0..=DUO_MAX_MS)
        .step_by(1.0)
        .fixed_decimals(0)
        .custom_formatter(|x, _| if x < 0.5 { t("0 关", "0 off").to_string() } else { format!("{x:.0} ms") })
        .custom_parser(|s| s.trim().split(|c: char| !(c.is_ascii_digit() || c == '.')).find(|w| !w.is_empty()).and_then(|w| w.parse::<f64>().ok()))
}

fn playback_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn save_png(path: &PathBuf, img: &egui::ColorImage) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    w.write_image_data(&bytes).map_err(|e| e.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_parameter_values_survive_redraw_and_serialization() {
        for mode in [MotionMode::Sweep, MotionMode::Manual, MotionMode::Random] {
            let mut params = Params {
                mode,
                inear_cm: 3.5,
                rumble: 5.0,
                canal_db: 20.0,
                front: 0.9,
                puff: 10.0,
                warmth_db: 18.0,
                hold_s: 30.0,
                move_s: 25.0,
                ear_dist: 1.2,
                manual_dist: 1.5,
                wet_db: 15.0,
                target_lufs: -8.0,
                ..Params::default()
            };
            let expected = params.clone();
            let ctx = egui::Context::default();
            // 普通、合成·各自固定、合成·一起移动三种画法都画几遍
            for k in 0..9 {
                let _ = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(2000.0, 3000.0))),
                    ..Default::default()
                }, |ui| {
                    ui.columns(2, |columns| {
                        let layout = [None, Some(false), Some(true)][k % 3];
                        App::params_left(&mut columns[0], &mut params, layout);
                        App::params_right(&mut columns[1], &mut params, layout == Some(false));
                    });
                });
                assert_eq!(params, expected, "drawing controls changed a manually entered value");
            }
            let saved = Saved { params, ..Saved::default() };
            let restored: Saved = ron::from_str(&ron::to_string(&saved).unwrap()).unwrap();
            assert_eq!(restored.params, expected);
        }
    }

    #[test]
    fn mix_setting_and_voices_survive_serialization() {
        // 合成开关随配置保存；Voice（位置、贴耳 / 固定距离、音量）可以序列化
        let saved = Saved { mix: true, mix_follow: true, ..Saved::default() };
        let restored: Saved = ron::from_str(&ron::to_string(&saved).unwrap()).unwrap();
        assert!(restored.mix && restored.mix_follow);
        // 旧配置文件里没有这两项：默认关、各自固定
        let old: Saved = ron::from_str("(english:false)").unwrap();
        assert!(!old.mix && !old.mix_follow);
        let v = Voice { az: 45.0, dist: Some(0.3), gain_db: -3.0 };
        assert_eq!(ron::from_str::<Voice>(&ron::to_string(&v).unwrap()).unwrap(), v);
        assert_eq!(voice_name(&Voice::preset(0)), "左耳");
        assert_eq!(voice_name(&Voice { az: 33.0, dist: Some(0.5), gain_db: 0.0 }), "33°");
    }

    #[test]
    fn exported_config_can_be_imported() {
        let saved = Saved { english: true, mix: true, buffer: 512, params: Params { puff: 2.5, ..Params::default() }, ..Saved::default() };
        // 库里的键就是 eframe 自动保存用的键：自动保存的文件、导出的文件、安卓版的文件是同一个格式
        assert_eq!(crate::config::APP_KEY, eframe::APP_KEY);
        // 和 export_config 一样的写法
        let text = crate::config::to_text(&saved).unwrap();
        let back = parse_config(&text).unwrap();
        assert!(back.english && back.mix && back.buffer == 512);
        assert_eq!(back.params, saved.params);
        // 直接写的 Saved，只写几项
        let bare = parse_config("(english:true,bits:16)").unwrap();
        assert!(bare.english && bare.bits == 16);
        assert_eq!(bare.params, Params::default());
        // 别的程序的键值文件、乱写的文件：报错
        assert!(parse_config("{\"other\": \"()\"}").is_err());
        assert!(parse_config("not ron").is_err());
    }

    #[test]
    fn radar_pick_is_the_inverse_of_the_drawing_scale() {
        let rmax = 90.0;
        for &(az, r) in &[(0.0f32, 0.3f32), (90.0, 0.105), (200.0, 0.5), (315.0, 0.8), (270.0, R_MIN)] {
            // 画的时候：方位角顺时针、上 = 正前方，半径 = radar_map(r) * rmax
            let a = az.to_radians();
            let d = egui::vec2(a.sin(), -a.cos()) * (radar_map(r) * rmax);
            let (az2, r2) = radar_pick(d, rmax);
            assert!(((az2 - az + 180.0).rem_euclid(360.0) - 180.0).abs() < 0.05, "az {az} -> {az2}");
            assert!((r2 / r - 1.0).abs() < 1e-3, "r {r} -> {r2}");
        }
        // 拖进头里：停在头外面；拖出图外：停在图的边缘
        assert_eq!(radar_pick(egui::vec2(1.0, 0.0), rmax).1, R_MIN);
        assert!((radar_pick(egui::vec2(0.0, -500.0), rmax).1 - RADAR_MAX_R).abs() < 1e-5);
    }

    #[test]
    fn up_angle_undoes_lift_tilt() {
        // 手动的三个量（方位角, 距离, 高度）→ 三维位置 → 再换回来
        for &(az, dist, tilt) in &[(90.0f32, 0.105f32, 45.0f32), (260.0, 0.15, -30.0), (0.0, 0.5, 30.0), (135.0, 0.3, 60.0), (300.0, 0.12, 0.0)] {
            let (a3, e3, r3) = lift_tilt(az, dist, 0.0, tilt);
            let (up, az2, dist2) = up_angle(a3, e3, r3);
            assert!((up - tilt).abs() < 0.2, "tilt {tilt} -> {up}");
            assert!(((az2 - az + 180.0).rem_euclid(360.0) - 180.0).abs() < 0.2, "az {az} -> {az2}");
            assert!((dist2 - dist).abs() < 5e-4, "dist {dist} -> {dist2}");
        }
    }

    /// 窗口图标（assets/icon.png）要能解出 256×256 的 RGBA，不然会退回 egui 的默认图标
    #[test]
    fn window_icon_decodes() {
        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        assert_eq!(icon.rgba[3], 0, "圆形图标的角落应当透明");
        assert_eq!(icon.rgba[(128 * 256 + 128) * 4 + 3], 255);
    }
}
