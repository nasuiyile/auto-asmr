//! 文件转换页：照搬桌面版界面（`auto-asmr/src/gui.rs`）文件页的逻辑，函数名、行为一一对应——
//! 文件列表、试听（试听位置、边听边调参数）、转换并保存、批量（按文件并行、长的先做）、多音频合成（位置、音量、
//! 排布、自动平衡、自动对齐）、左右双人。转换、合成、对齐、平衡、试听引擎都直接用共用库（`offline`、`batch`、`live`），
//! 和电脑版是同一份代码；这里只是把桌面版界面里的"编排"搬过来。
//!
//! 和桌面版不同的地方（安卓的限制）：
//! - 文件来自系统的文件选择器。Kotlin 把每个文件做成缓存目录里一个带原文件名的符号链接（指向打开的文件描述符），
//!   这里照样按路径处理。
//! - 输出先写进 App 的暂存目录（`convert` 命令给的 `staging`），转换完成后由 Kotlin 存到用户选的位置
//!   （默认「音乐/auto-asmr」，或输出文件夹、源文件所在的文件夹、「输出到…」选的文件），再用 `delivered` 告诉这里
//!   最终位置，说明和列表里显示最终位置。
//! - 试听用 AAudio（[`crate::aout`]），回调里是共用库的 `Pump`。
//! - 界面状态：Kotlin 每 0.1–0.3 秒发一次 `poll`（相当于桌面版的一帧），每个命令都返回整页的状态（JSON）。
//!   设置（声道、位深、目标响度、同时转换、多音频合成、排布、对齐、平衡、左右双人）都在共用配置里（[`settings`]），
//!   Kotlin 按字段改；这里每次比较前后两份配置，照桌面版各个设置函数的做法处理（试听中开 / 关左右双人重新开始等）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use auto_asmr::batch;
use auto_asmr::config::Saved;
use auto_asmr::dsp::align::Alignment;
use auto_asmr::dsp::balance::{self, BalanceData};
use auto_asmr::dsp::engine::{Engine, EngineConfig, PostMode, Telemetry};
use auto_asmr::dsp::mix::{self, MixSetup, Mixer, Voice, MAX_VOICES, PRESETS};
use auto_asmr::dsp::motion::{lift_tilt, Motion};
use auto_asmr::dsp::params::MotionMode;
use auto_asmr::dsp::solo::Solo;
use auto_asmr::dsp::FS;
use auto_asmr::live::{Proc, Pump, Shared, Source};
use auto_asmr::offline::{
    align_tracks, auto_pick, balance_data, convert_mix, default_mix_output, default_output, load_audio, mix_report_text, report_text, Channel, ConvertOptions, ConvertReport, Duo, MixInput, MixOptions,
};
use serde_json::{json, Value};

use crate::aout::Output;
use crate::settings;

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn motion_name(m: MotionMode) -> &'static str {
    match m {
        MotionMode::Sweep => "穿梭",
        MotionMode::Random => "随机走位",
        MotionMode::Orbit => "环绕",
        MotionMode::Static => "固定",
        MotionMode::Manual => "手动",
    }
}

/// 多音频合成的错开延迟写成一句话（同桌面版）：两个音频时"错开：第 2 个晚 80 ms"，多个时依次列出。
fn stagger_text(ms: f32, n: usize) -> String {
    let parts: Vec<String> = (1..n).map(|k| format!("第 {} 个晚 {:.0} ms", k + 1, ms * k as f32)).collect();
    format!("错开：{}", parts.join("，"))
}

/// 位置名（预设的名字，摆到别处时是方位角）。
fn voice_name(v: &Voice) -> String {
    match v.preset_index() {
        Some(i) => PRESETS[i].0.to_string(),
        None => format!("{:.0}°", v.az),
    }
}

fn playback_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// 列表里一个文件的转换状态。
#[derive(Clone)]
enum ItemStatus {
    Waiting,
    Running(f32, String),
    Done(Box<ConvertReport>, PathBuf),
    Failed(String),
}

/// 一次转换（单个文件或一批文件）。工作线程更新 `status`，每次轮询读一次。
struct Job {
    paths: Vec<PathBuf>,
    status: Arc<Vec<Mutex<ItemStatus>>>,
    est: Arc<Mutex<Vec<f64>>>,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    workers: usize,
    t0: Instant,
    mix: Option<String>,
    /// 每一项的输入（多音频合成时是参与合成的第一个音频）：Kotlin 决定输出放哪里时要知道它来自哪个文件。
    inputs: Vec<PathBuf>,
    /// 这一次的暂存子目录（结束时空了就删掉：取消、失败的输出不会留下来）。
    dir: PathBuf,
}

/// 试听用的解码结果：文件、声道（[`Files::ch_key`]）、48 kHz 的音频，以及右声道（左右声道都要时才有，这时前面那段是左声道）。
type Decoded = Result<(PathBuf, u8, Arc<Vec<f32>>, Option<Arc<Vec<f32>>>), String>;

struct MixAudio {
    paths: Vec<PathBuf>,
    ch: u8,
    data: Vec<Arc<Vec<f32>>>,
    lufs: Vec<Option<f32>>,
    levels: Vec<Arc<Vec<f32>>>,
    aligned: Option<Result<AlignedMix, String>>,
}

struct AlignedMix {
    al: Alignment,
    data: Vec<Arc<Vec<f32>>>,
    lufs: Vec<Option<f32>>,
    levels: Vec<Arc<Vec<f32>>>,
}

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
    fn tracks(&self, align: bool) -> Option<MixTracks> {
        match (&self.aligned, align) {
            (None, true) => None,
            (Some(Ok(a)), true) => Some(MixTracks { data: a.data.clone(), lufs: a.lufs.clone(), levels: a.levels.clone() }),
            _ => Some(MixTracks { data: self.data.clone(), lufs: self.lufs.clone(), levels: self.levels.clone() }),
        }
    }
}

type MixDecoded = Result<MixAudio, (Vec<PathBuf>, u8, String)>;

/// 正在播放的试听（相当于桌面版的 `Realtime`）。
struct Preview {
    out: Output,
    shared: Arc<Shared>,
}

impl Preview {
    fn file(cfg: Arc<EngineConfig>, audio: Arc<Vec<f32>>, start: usize) -> Result<Preview, String> {
        let shared = Arc::new(Shared::new(cfg.clone()));
        let sh = shared.clone();
        // 试听不开主播模式的"话少时也一直在动"：动作严格跟着说话走，与文件转换一致（同桌面版）
        let out = Output::start(shared.clone(), move |rate| Pump::new(Source::Memory { data: audio, pos: start }, Engine::new(cfg, PostMode::Realtime, 0.0), sh, rate))?;
        Ok(Preview { out, shared })
    }

    /// 试听一个立体声文件（听歌）：「立体声宽度」大于 0 时左右声道各当一个声源，为 0 时合成单声道过一个引擎；
    /// 边听边改参数时随时切换（同桌面版 `Realtime::start_preview_stereo`）。
    fn stereo(cfg: Arc<EngineConfig>, left: Arc<Vec<f32>>, right: Arc<Vec<f32>>, start: usize) -> Result<Preview, String> {
        let shared = Arc::new(Shared::new(cfg.clone()));
        let sh = shared.clone();
        let out = Output::start(shared.clone(), move |rate| {
            // 立体声对事先建好：播放中改「立体声宽度」时音频线程里不分配内存
            let solo = Solo::new(cfg, PostMode::Realtime, 0.0).with_pair();
            Pump::with_proc(Source::Stereo { l: left, r: right, pos: start }, Proc::Solo(solo), sh, rate)
        })?;
        Ok(Preview { out, shared })
    }

    fn mix(setup: Arc<MixSetup>, audio: Vec<Arc<Vec<f32>>>, start: usize) -> Result<Preview, String> {
        if audio.is_empty() || audio.len() != setup.cfgs.len() || audio.len() > MAX_VOICES {
            return Err(format!("合成试听需要 1–{MAX_VOICES} 段音频，且与位置一一对应"));
        }
        let shared = Arc::new(Shared::new(setup.cfgs[0].clone()));
        if let Ok(mut g) = shared.mix.lock() {
            *g = Some(setup.clone());
        }
        let lens: Vec<usize> = audio.iter().map(|a| a.len()).collect();
        let len = mix::timeline_len(&lens, &setup.delays);
        let sh = shared.clone();
        let out = Output::start(shared.clone(), move |rate| Pump::with_proc(Source::Multi { data: audio, pos: start, len }, Proc::Mix(Mixer::new(&setup, PostMode::Realtime, 0.0)), sh, rate))?;
        Ok(Preview { out, shared })
    }
}

pub struct Files {
    s: Saved,
    cfg: Arc<EngineConfig>,
    sent: auto_asmr::dsp::params::Params,
    rt: Option<Preview>,
    rt_preview: bool,
    status: String,
    files: Vec<PathBuf>,
    statuses: HashMap<PathBuf, ItemStatus>,
    file_in: Option<PathBuf>,
    start_s: f32,
    dur_s: f32,
    preview_s: f64,
    preview_dragging: bool,
    preview_pending: bool,
    job: Option<Job>,
    report: String,
    decoded: Option<(PathBuf, u8, Arc<Vec<f32>>)>,
    /// 解码结果的右声道：听歌（立体声宽度大于 0）、单个立体声音频时才有，这时 `decoded` 里那段是左声道。
    decoded_r: Option<Arc<Vec<f32>>>,
    /// 试听单个立体声文件时左右声道两个声源此刻的 [方位角, 仰角, 距离, 说话活动]（没在用立体声对时为空）。
    pair_tele: Vec<[f32; 4]>,
    decoding: Option<Receiver<Decoded>>,
    tele: Telemetry,
    // ---- 多音频合成
    voices: HashMap<PathBuf, Voice>,
    duo_voices: [Voice; 2],
    mix_audio: Option<MixAudio>,
    mix_shift: bool,
    mix_bal: Option<(usize, Arc<BalanceData>)>,
    mix_decoding: Option<Receiver<MixDecoded>>,
    mix_failed: Option<(Vec<PathBuf>, u8)>,
    rt_mix: bool,
    mix_sent: Option<(Vec<Voice>, Arc<MixSetup>)>,
    mix_tele: Vec<[f32; 4]>,
    mix_sel: Option<usize>,
    // ---- 安卓：输出交给 Kotlin 存到最终位置
    /// 完成了、还没存到最终位置的输出：(暂存路径, 输入, 是不是合成)。
    pending_out: Vec<(PathBuf, PathBuf, bool)>,
    /// 存好了的输出：暂存路径 → 最终位置（显示用）。
    delivered: HashMap<PathBuf, String>,
    /// 已经交出去过的暂存路径（避免重复交）。
    handed: std::collections::HashSet<PathBuf>,
    /// 第几次转换：每次的输出放进暂存目录下单独的子目录，路径不会和上一次的重复（上一次的可能还没存完）。
    job_seq: u32,
}

pub static FILES: Mutex<Option<Files>> = Mutex::new(None);

fn saved_now() -> Saved {
    settings::with_global(|g| g.saved.clone())
}

impl Files {
    fn new() -> Files {
        let s = saved_now();
        let cfg = Arc::new(EngineConfig::new(s.params.clone(), None));
        Files {
            sent: s.params.clone(),
            cfg,
            s,
            rt: None,
            rt_preview: false,
            status: String::new(),
            files: Vec::new(),
            statuses: HashMap::new(),
            file_in: None,
            start_s: 0.0,
            dur_s: 0.0,
            preview_s: 0.0,
            preview_dragging: false,
            preview_pending: false,
            job: None,
            report: String::new(),
            decoded: None,
            decoded_r: None,
            pair_tele: Vec::new(),
            decoding: None,
            tele: Telemetry::default(),
            voices: HashMap::new(),
            duo_voices: [Voice::preset(0), Voice::preset(1)],
            mix_audio: None,
            mix_shift: false,
            mix_bal: None,
            mix_decoding: None,
            mix_failed: None,
            rt_mix: false,
            mix_sent: None,
            mix_tele: Vec::new(),
            mix_sel: None,
            pending_out: Vec::new(),
            delivered: HashMap::new(),
            handed: Default::default(),
            job_seq: 0,
        }
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
        self.pair_tele.clear();
        self.preview_pending = false;
    }

    // ------------------------------------------------------------------ 多音频合成（同桌面版）

    fn multi(&self) -> bool {
        self.s.mix && self.files.len() >= 2
    }

    fn duo_with(&self, s: &Saved) -> bool {
        !(s.mix && self.files.len() >= 2) && s.duo_ms > 0.0 && self.file_in.is_some()
    }

    fn duo(&self) -> bool {
        self.duo_with(&self.s)
    }

    fn mixing(&self) -> bool {
        self.multi() || self.duo()
    }

    fn align_on(&self) -> bool {
        self.multi() && self.s.mix_align
    }

    /// 同桌面版：左右双人的第 2 个声音晚 `duo_ms`；多音频合成时同一个滑条是错开延迟，第 k 个音频晚 k × `duo_ms`（在自动对齐之后）。
    fn mix_delays(&self) -> [usize; MAX_VOICES] {
        let mut d = [0; MAX_VOICES];
        if self.multi() {
            d = mix::stagger_delays(self.s.duo_ms, self.mix_paths().len());
        } else if self.duo() {
            d[1] = mix::delay_samples(self.s.duo_ms);
        }
        d
    }

    fn duo_options(&self) -> Option<Duo> {
        (!self.multi() && self.s.duo_ms > 0.0).then(|| Duo { delay_ms: self.s.duo_ms, voices: self.duo_voices, follow: self.s.mix_follow, balance: self.s.mix_balance / 100.0 })
    }

    fn set_mix_voice(&mut self, k: usize, v: Voice) {
        if self.duo() {
            if let Some(d) = self.duo_voices.get_mut(k) {
                *d = v;
            }
        } else if let Some(p) = self.files.get(k).cloned() {
            self.voices.insert(p, v);
        }
    }

    /// 左右双人的延迟变了（配置里的 `duo_ms`）：试听中开 / 关时按新的方式从当前位置重新开始；只是改延迟时实时生效。
    fn on_duo_changed(&mut self, was: bool) {
        if self.duo() == was {
            return;
        }
        self.mix_sel = None;
        if self.rt.is_some() && self.rt_preview {
            if let Some(rt) = &self.rt {
                self.preview_s = rt.shared.play_pos.load(Ordering::Relaxed) as f64 / FS as f64;
            }
            self.start_preview();
        }
    }

    fn mix_paths(&self) -> Vec<PathBuf> {
        if let Some(p) = self.file_in.as_ref().filter(|_| self.duo()) {
            return vec![p.clone(), p.clone()];
        }
        self.files.iter().take(MAX_VOICES).cloned().collect()
    }

    fn mix_voices(&self) -> Vec<Voice> {
        if self.duo() {
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

    fn mix_setup(&self, lufs: &[Option<f32>]) -> (Vec<Voice>, Arc<MixSetup>) {
        let voices = self.mix_voices();
        let prev = self.mix_sent.as_ref().map(|(_, s)| s.as_ref());
        let setup = MixSetup::new(&self.cfg, &voices, lufs, self.s.mix_follow, prev).with_balance(self.cur_balance(), self.s.mix_balance / 100.0).with_delays(&self.mix_delays());
        (voices, Arc::new(setup))
    }

    fn cur_balance(&self) -> Option<Arc<BalanceData>> {
        if self.s.mix_balance <= 0.0 {
            return None;
        }
        let t = self.mix_audio.as_ref().filter(|m| m.paths == self.mix_paths() && m.ch == self.ch_key())?.tracks(self.align_on())?;
        let key = Arc::as_ptr(t.data.first()?) as usize;
        self.mix_bal.as_ref().filter(|(k, _)| *k == key).map(|(_, b)| b.clone())
    }

    fn sync_balance(&mut self) {
        if !self.mixing() || self.s.mix_balance <= 0.0 {
            return;
        }
        let Some(t) = self.mix_audio.as_ref().filter(|m| m.paths == self.mix_paths() && m.ch == self.ch_key()).and_then(|m| m.tracks(self.align_on())) else { return };
        let Some(first) = t.data.first() else { return };
        let key = Arc::as_ptr(first) as usize;
        if self.mix_bal.as_ref().map_or(false, |(k, _)| *k == key) {
            return;
        }
        let lv: Vec<&[f32]> = t.levels.iter().map(|l| l.as_slice()).collect();
        self.mix_bal = Some((key, balance_data(&lv, &t.lufs)));
    }

    fn mix_positions(&self) -> Vec<[f32; 4]> {
        let active = self.rt.is_some() && self.rt_mix;
        let p = &self.s.params;
        let voices = self.mix_voices();
        let start = self.s.mix_follow.then(|| Motion::new(p.seed).step(p, 0.0, 1.0, 0.0));
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

    /// 排布变了（各自固定 / 一起移动）：合成试听中从当前位置重新开始。
    fn on_follow_changed(&mut self) {
        if self.rt_mix && self.rt.is_some() {
            self.start_mix_preview();
        }
    }

    /// 多音频合成开 / 关了。
    fn on_mix_mode_changed(&mut self) {
        let on = self.s.mix;
        self.stop();
        if on {
            self.ensure_voices();
        } else if self.file_in.is_some() && self.decoded.is_none() && self.decoding.is_none() {
            self.decode_file();
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

    /// 解码结果按它区分（变了就要重新解码，同桌面版）：1–3 是「取哪个声道」选的左 / 右 / 混合；「自动」平时是 0
    /// （更响的那个声道），听歌（立体声宽度大于 0）时单个音频是 4（左右声道都要，各当一个声源），多音频合成、
    /// 左右双人是 3（左右混合）。
    fn ch_key(&self) -> u8 {
        match (self.s.file_channel, self.s.params.stereo_width > 0.0) {
            (0, true) => {
                if self.mixing() {
                    3
                } else {
                    4
                }
            }
            (c, _) => c.min(3),
        }
    }

    fn decode_mix(&mut self) {
        let paths = self.mix_paths();
        let ch = self.ch_key();
        if self.duo() {
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
        // 合成里每个音频是一个声源：听歌时「自动」取左右混合
        let pick = auto_pick(&self.s.params, false);
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
                            let m = load_audio(&o, &AtomicBool::new(false), pick).map(|(mut c, _)| c.swap_remove(0)).map_err(|e| format!("{}：{e}", file_name(p)))?;
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
        self.status = if align { "正在解码要合成的音频并自动对齐…" } else { "正在解码要合成的音频…" }.to_string();
    }

    fn align_mix(&mut self) {
        let Some(m) = &self.mix_audio else { return };
        let (paths, ch, data, lufs, levels) = (m.paths.clone(), m.ch, m.data.clone(), m.lufs.clone(), m.levels.clone());
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let aligned = Some(AlignedMix::new(&paths, &data));
            let _ = tx.send(Ok(MixAudio { paths, ch, data, lufs, levels, aligned }));
        });
        self.mix_decoding = Some(rx);
        self.status = "正在自动对齐…".to_string();
    }

    /// 合成前的自动对齐开 / 关了（配置里的 `mix_align` 已经是新值）。
    fn on_align_changed(&mut self) {
        let on = self.s.mix_align;
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
        let ch = self.ch_key();
        if self.mix_audio.as_ref().map_or(false, |m| m.paths == paths && m.ch == ch) {
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

    fn start_mix_preview(&mut self) {
        let selected_s = self.preview_s;
        self.stop();
        self.preview_s = selected_s;
        self.sync_params();
        let paths = self.mix_paths();
        let ch = self.ch_key();
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
            match Preview::mix(setup.clone(), data, start) {
                Ok(rt) => {
                    self.status = if self.duo() {
                        format!("左右双人试听中：{}（{} Hz），第 2 个声音晚 {:.0} ms", rt.out.name, rt.out.out_rate, self.s.duo_ms)
                    } else {
                        format!("合成试听中：{}（{} Hz），{} 个音频同时播放", rt.out.name, rt.out.out_rate, voices.len())
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
                self.align_mix();
            } else {
                self.mix_failed = None;
                self.decode_mix();
            }
        }
    }

    fn start_mix_convert(&mut self, staging: &Path) {
        if self.job.is_some() || !self.multi() {
            return;
        }
        let paths = self.mix_paths();
        let out = staging.join(default_mix_output(&paths[0]).file_name().unwrap_or_default());
        let voices = self.mix_voices();
        let inputs: Vec<MixInput> = paths.iter().cloned().zip(voices.iter().copied()).map(|(path, voice)| MixInput { path, voice }).collect();
        let follow = self.s.mix_follow;
        let align = self.s.mix_align;
        let delays = mix::stagger_delays(self.s.duo_ms, inputs.len());
        let mo = MixOptions { follow, align, balance: self.s.mix_balance / 100.0, delays };
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
        let desc = if follow { format!("一起移动（{}）  {list}", motion_name(self.s.params.mode)) } else { list };
        let desc = if align { format!("自动对齐  {desc}") } else { desc };
        let desc = if mo.balance > 0.0 { format!("自动平衡 {:.0}%  {desc}", self.s.mix_balance) } else { desc };
        let desc = if self.s.duo_ms > 0.0 { format!("{}  {desc}", stagger_text(self.s.duo_ms, inputs.len())) } else { desc };
        let opts = self.convert_options(paths[0].clone(), out.clone());
        let job = Job {
            paths: vec![out.clone()],
            status: Arc::new(vec![Mutex::new(ItemStatus::Waiting)]),
            est: Arc::new(Mutex::new(vec![1.0])),
            cancel: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            workers: inputs.len(),
            t0: Instant::now(),
            mix: Some(desc),
            inputs: vec![paths[0].clone()],
            dir: staging.to_path_buf(),
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

    fn threads(&self) -> usize {
        if self.s.jobs == 0 {
            batch::default_threads()
        } else {
            self.s.jobs
        }
    }

    /// 转换列表里的全部文件（输出先写进 `staging` 下这一次的子目录）。多个文件时按 CPU 核数并行，长的文件先做。
    fn start_convert(&mut self, staging: &Path) {
        if self.job.is_some() {
            return;
        }
        self.job_seq += 1;
        let staging = &staging.join(self.job_seq.to_string());
        if let Err(e) = std::fs::create_dir_all(staging) {
            self.status = format!("无法创建暂存目录 {}：{e}", staging.display());
            return;
        }
        if self.multi() {
            self.start_mix_convert(staging);
            return;
        }
        let paths = self.files.clone();
        if paths.is_empty() || self.job.is_some() {
            return;
        }
        // 输出名同桌面版（原名_asmr.wav，重名加 _2、_3），先都放进暂存目录
        let outs = batch::plan_outputs(&paths, Some(staging));
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
            t0: Instant::now(),
            mix: None,
            inputs: paths.clone(),
            dir: staging.to_path_buf(),
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
        let ch = self.ch_key();
        if let Some((p, c, audio)) = &self.decoded {
            if *p == input && *c == ch {
                let start = ((self.preview_s.max(0.0) * FS as f64) as usize).min(audio.len());
                // 左右声道都解码了（听歌、立体声的音频）：左右声道各当一个声源
                let started = match &self.decoded_r {
                    Some(right) => Preview::stereo(self.cfg.clone(), audio.clone(), right.clone(), start),
                    None => Preview::file(self.cfg.clone(), audio.clone(), start),
                };
                match started {
                    Ok(rt) => {
                        self.status = format!("试听中：{}（{} Hz）{}", rt.out.name, rt.out.out_rate, if self.decoded_r.is_some() { "，立体声：左右声道各一个声源" } else { "" });
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
        let ch = self.ch_key();
        let (tx, rx) = channel();
        let mut o = ConvertOptions::new(input.clone());
        o.channel = self.file_channel();
        // 听歌时「自动」：单个音频左右声道都要；左右双人（当成一个声源再变成两个人）左右混合
        let pick = auto_pick(&self.s.params, !self.mixing());
        std::thread::spawn(move || {
            let r = load_audio(&o, &AtomicBool::new(false), pick).map(|(c, _)| {
                let mut it = c.into_iter().map(Arc::new);
                (input, ch, it.next().unwrap_or_default(), it.next())
            });
            let _ = tx.send(r);
        });
        self.decoding = Some(rx);
        self.status = "正在解码…".to_string();
    }

    /// 把文件加进列表（Kotlin 已经展开了文件夹），已在列表里的不重复加。列表原来是空的、或 `select` 时选中新加的第一个。
    fn add_files(&mut self, paths: Vec<PathBuf>, select: bool) {
        if self.job.is_some() {
            self.status = "正在转换，完成后才能修改列表".to_string();
            return;
        }
        let mut first = None;
        for p in paths {
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
        }
    }

    fn remove_file(&mut self, i: usize) {
        if i >= self.files.len() || self.job.is_some() {
            return;
        }
        let p = self.files.remove(i);
        self.statuses.remove(&p);
        self.voices.remove(&p);
        self.ensure_voices();
        self.mix_sel = None;
        if self.file_in.as_ref() == Some(&p) {
            self.stop();
            self.file_in = None;
            self.decoded = None;
            self.decoded_r = None;
            self.decoding = None;
            self.preview_pending = false;
            if let Some(q) = self.files.get(i.min(self.files.len().saturating_sub(1))).cloned() {
                self.set_file(q);
            }
        }
    }

    fn clear_files(&mut self) {
        if self.job.is_some() {
            return;
        }
        self.stop();
        self.files.clear();
        self.statuses.clear();
        self.file_in = None;
        self.decoded = None;
        self.decoded_r = None;
        self.decoding = None;
        self.preview_pending = false;
        self.report.clear();
        self.voices.clear();
        self.mix_audio = None;
        self.mix_decoding = None;
        self.mix_failed = None;
        self.mix_sel = None;
    }

    fn set_file(&mut self, p: PathBuf) {
        if !self.files.contains(&p) {
            self.files.push(p.clone());
            self.ensure_voices();
        }
        if self.file_in.as_ref() == Some(&p) {
            return;
        }
        self.stop();
        self.file_in = Some(p);
        self.decoded = None;
        self.decoded_r = None;
        self.preview_s = self.start_s.max(0.0) as f64;
        self.preview_dragging = false;
        if !self.multi() {
            self.decode_file();
        }
    }

    /// 列表里点了第 i 行：合成时只是选中它（不打断合成试听），否则选它来试听。
    fn select(&mut self, i: usize) {
        let Some(p) = self.files.get(i).cloned() else { return };
        if self.multi() {
            if i < MAX_VOICES {
                self.mix_sel = Some(i);
                self.file_in = Some(p);
            }
        } else {
            self.set_file(p);
        }
    }

    /// 取的声道变了（下拉框换了，或者「立体声宽度」在 0 和大于 0 之间变了——比如按了「听歌」）：重新解码，
    /// 之前在试听的话解码完从原来的位置接着放（同桌面版 ui_file 里的处理）。
    fn on_channel_changed(&mut self) {
        if self.file_in.is_none() {
            return;
        }
        let resume = self.rt_preview || self.preview_pending;
        self.stop();
        self.decoded = None;
        self.decoded_r = None;
        self.mix_audio = None;
        self.preview_pending = resume;
        if self.mixing() {
            self.decode_mix();
        } else {
            self.decode_file();
        }
    }

    /// 参数变了：交给试听的引擎（同桌面版 sync_params；手动摆位的进出由 Kotlin 的 Settings 处理）。
    fn sync_params(&mut self) {
        let changed = self.s.params != self.sent;
        if changed {
            let cfg = Arc::new(EngineConfig::new(self.s.params.clone(), Some(&self.cfg)));
            if let Some(rt) = self.rt.as_ref().filter(|_| !self.rt_mix) {
                rt.shared.set_config(cfg.clone());
            }
            self.cfg = cfg;
            self.sent = self.s.params.clone();
        }
        if self.rt_mix && self.rt.is_some() {
            let lufs = self.mix_audio.as_ref().filter(|m| m.paths == self.mix_paths()).and_then(|m| m.tracks(self.align_on())).map(|t| t.lufs);
            self.sync_balance();
            let bal = self.cur_balance();
            if let Some(lufs) = lufs {
                let strength = self.s.mix_balance / 100.0;
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
                        rt.shared.set_mix(setup.clone());
                    }
                    self.mix_sent = Some((voices, setup));
                }
            }
        }
    }

    /// 配置里的设置变了（Kotlin 改的）：照桌面版各个设置函数的做法处理。
    fn sync_saved(&mut self) {
        let new = saved_now();
        let was_duo = self.duo();
        let key_before = self.ch_key();
        let old = std::mem::replace(&mut self.s, new);
        if old.mix != self.s.mix {
            self.on_mix_mode_changed();
        }
        if old.duo_ms != self.s.duo_ms {
            self.on_duo_changed(was_duo);
        }
        if old.mix_follow != self.s.mix_follow {
            self.on_follow_changed();
        }
        if old.mix_align != self.s.mix_align {
            self.on_align_changed();
        }
        // 解码结果按 ch_key 区分：声道选择变了，或者听歌的开关（立体声宽度）变了、合成 / 左右双人开关让「自动」的取法变了
        let key = self.ch_key();
        let stale = self.decoded.as_ref().map_or(false, |(_, c, _)| *c != key) || self.mix_audio.as_ref().map_or(false, |m| m.ch != key);
        if old.file_channel != self.s.file_channel || (key != key_before && (stale || self.decoding.is_some() || self.mix_decoding.is_some())) {
            self.on_channel_changed();
        }
    }

    /// 后台事务：解码完成、转换完成、试听出错 / 播完（同桌面版 poll）。
    fn poll(&mut self) {
        if let Some(rx) = &self.mix_decoding {
            if let Ok(r) = rx.try_recv() {
                self.mix_decoding = None;
                match r {
                    Ok(m) => {
                        if self.mixing() && m.paths == self.mix_paths() && m.ch == self.ch_key() {
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
                                self.status = "左右双人已就绪：点「试听」或拖动播放条，同一个声音一左一右两个人说".to_string();
                            } else {
                                self.status = "要合成的音频已就绪：点「试听」或拖动播放条，几个音频会同时播放".to_string();
                            }
                            if let Some(e) = align_err {
                                self.status = format!("自动对齐：{e}");
                            }
                        }
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
                    Ok((path, ch, audio, right)) => {
                        self.decoded = Some((path, ch, audio));
                        self.decoded_r = right;
                        if ch != self.ch_key() {
                            // 解码期间取的声道变了：这份用不上，按新的再解一次
                            self.decoded = None;
                            self.decoded_r = None;
                            self.decode_file();
                        } else if self.preview_pending {
                            self.start_preview();
                        } else {
                            self.status = "音频已就绪，点击或拖动播放条即可试听".to_string();
                        }
                    }
                    Err(e) => {
                        if self.duo() {
                            self.mix_failed = Some((self.mix_paths(), self.ch_key()));
                        }
                        self.preview_pending = false;
                        self.status = e;
                    }
                }
            }
        }
        if let Some(j) = &self.job {
            for (k, (p, m)) in j.paths.iter().zip(j.status.iter()).enumerate() {
                if let Ok(g) = m.lock() {
                    // 完成了的输出交给 Kotlin 存到最终位置（每个只交一次）
                    if let ItemStatus::Done(_, out) = &*g {
                        if self.handed.insert(out.clone()) {
                            self.pending_out.push((out.clone(), j.inputs.get(k).cloned().unwrap_or_default(), j.mix.is_some()));
                        }
                    }
                    self.statuses.insert(p.clone(), g.clone());
                }
            }
        }
        if self.job.as_ref().map_or(false, |j| j.finished.load(Ordering::Acquire)) {
            let j = self.job.take().unwrap();
            let _ = std::fs::remove_dir(&j.dir); // 只有空了才删得掉
            let all: Vec<ItemStatus> = j.status.iter().map(|m| m.lock().map(|g| g.clone()).unwrap_or(ItemStatus::Failed(String::new()))).collect();
            let ok = all.iter().filter(|s| matches!(s, ItemStatus::Done(..))).count();
            let n = all.len();
            self.report = match (n, all.first()) {
                (1, Some(ItemStatus::Done(rep, out))) => match &j.mix {
                    Some(desc) => mix_report_text(rep, &out.display().to_string(), desc, false),
                    None => report_text(rep, &out.display().to_string(), false),
                },
                (1, Some(ItemStatus::Failed(e))) => format!("失败：{e}"),
                _ => {
                    let cancelled = all.iter().filter(|s| matches!(s, ItemStatus::Failed(e) if e == "已取消")).count();
                    let mut r = format!("已转换 {ok} / {n} 个文件，用时 {}", playback_time(j.t0.elapsed().as_secs_f64()));
                    if n - ok - cancelled > 0 {
                        r.push_str(&format!("；{} 个失败（点列表里的「失败」看原因）", n - ok - cancelled));
                    }
                    if cancelled > 0 {
                        r.push_str(&format!("；{cancelled} 个已取消"));
                    }
                    r
                }
            };
        }
        let mut stop = false;
        if let Some(rt) = &self.rt {
            self.tele = rt.shared.telemetry();
            if self.rt_mix {
                self.mix_tele = rt.shared.voice_telemetry();
            } else {
                // 试听单个立体声文件：左右声道两个声源的位置（没在用立体声对时是空的）
                self.pair_tele = rt.shared.voice_telemetry();
            }
            if let Some(e) = rt.shared.take_error() {
                self.status = e;
                stop = true;
            }
            if self.rt_preview && !self.preview_dragging {
                self.preview_s = rt.shared.play_pos.load(Ordering::Relaxed) as f64 / FS as f64;
            }
            if self.rt_preview && rt.shared.preview_finished() {
                self.status = "试听结束".to_string();
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
    }

    /// 播放条的总长（样本）：合成时各段（左右双人的第 2 个声音晚一点）都放完为止。
    fn preview_len(&self) -> Option<usize> {
        let delays = self.mix_delays();
        let single = self.decoded.as_ref().filter(|(p, _, _)| Some(p) == self.file_in.as_ref()).map(|(_, _, a)| a.len());
        if self.mixing() {
            self.mix_audio
                .as_ref()
                .and_then(|m| m.tracks(self.align_on()))
                .map(|t| mix::timeline_len(&t.data.iter().map(|x| x.len()).collect::<Vec<_>>(), &delays))
                .or_else(|| single.filter(|_| !self.multi()).map(|l| l + delays[1]))
        } else {
            single
        }
    }

    /// 播放条点了 / 拖到 `s` 秒：试听中直接跳过去，否则从这里开始试听（同桌面版）。
    fn seek(&mut self, s: f64) {
        let Some(len) = self.preview_len() else { return };
        self.preview_s = s.clamp(0.0, len as f64 / FS as f64);
        if let Some(rt) = self.rt.as_ref().filter(|_| self.rt_preview) {
            rt.shared.seek_preview((self.preview_s * FS as f64) as u64);
        } else {
            self.start_preview();
        }
    }

    // ------------------------------------------------------------------ 给界面的状态

    fn text_with_labels(&self, s: &str) -> String {
        let mut s = s.to_string();
        for (p, label) in &self.delivered {
            s = s.replace(&p.display().to_string(), label);
        }
        s
    }

    fn status_json(&self, st: Option<&ItemStatus>) -> Value {
        match st {
            None => Value::Null,
            Some(ItemStatus::Waiting) => json!({"kind": "waiting"}),
            Some(ItemStatus::Running(f, stage)) => json!({"kind": "running", "f": f, "stage": stage}),
            Some(ItemStatus::Done(rep, out)) => json!({
                "kind": "done",
                "elapsed": rep.elapsed_s,
                "seconds": rep.seconds,
                "lufs": rep.lufs,
                "peak": rep.true_peak_db,
                "channel_note": rep.channel_note,
                "out": self.delivered.get(out).cloned().unwrap_or_else(|| "正在保存…".to_string()),
            }),
            Some(ItemStatus::Failed(e)) => json!({"kind": "failed", "error": e}),
        }
    }

    /// 和网页模式 `NativeHook.telemetry()` 同一排列（16 个数），给位置图和状态行用。
    fn tele_array(&self) -> Vec<f32> {
        let mut t = vec![0f32; 16];
        let Some(rt) = &self.rt else { return t };
        let sh = &rt.shared;
        let tl = &self.tele;
        t[0] = 1.0;
        t[1] = tl.az;
        t[2] = tl.el;
        t[3] = tl.r;
        t[4] = tl.act;
        let frames = sh.out_frames.load(Ordering::Relaxed) as f32;
        let block_us = if frames > 0.0 { frames / rt.out.out_rate.max(1) as f32 * 1e6 } else { 0.0 };
        t[5] = if block_us > 0.0 { sh.proc_us.load(Ordering::Relaxed) as f32 / block_us * 100.0 } else { 0.0 };
        t[6] = sh.algo_samples.load(Ordering::Relaxed) as f32 / FS * 1000.0 + block_us / 1000.0;
        t[7] = sh.xruns.load(Ordering::Relaxed) as f32;
        t[8] = FS;
        if self.rt_mix && self.mix_tele.len() >= 2 {
            let (a, b) = (self.mix_tele[0], self.mix_tele[1]);
            t[1..5].copy_from_slice(&a);
            t[11] = 1.0;
            t[12..16].copy_from_slice(&b);
        } else if !self.rt_mix && self.pair_tele.len() == 2 {
            // 立体声（听歌）：左右声道两个声源，记 2（同网页模式）
            t[1..5].copy_from_slice(&self.pair_tele[0]);
            t[11] = 2.0;
            t[12..16].copy_from_slice(&self.pair_tele[1]);
        }
        t
    }

    fn state(&self) -> Value {
        let multi = self.multi();
        let mixing = self.mixing();
        let files: Vec<Value> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!({
                    "path": p.display().to_string(),
                    "name": file_name(p),
                    "in_mix": multi && i < MAX_VOICES,
                    "status": self.status_json(self.statuses.get(p)),
                })
            })
            .collect();
        let selected = self.file_in.as_ref().and_then(|f| self.files.iter().position(|p| p == f));
        let voices: Vec<Value> = if mixing {
            self.mix_voices().iter().map(|v| json!({"az": v.az, "dist": v.distance(self.s.params.ear_dist), "custom_dist": v.dist, "gain": v.gain_db, "preset": v.preset_index(), "name": voice_name(v)})).collect()
        } else {
            Vec::new()
        };
        let n_voices = if mixing { self.mix_voices().len() } else { 0 };
        let align = if self.s.mix_align && multi {
            let paths = self.mix_paths();
            let ch = self.ch_key();
            match self.mix_audio.as_ref().filter(|m| m.paths == paths && m.ch == ch).and_then(|m| m.aligned.as_ref()) {
                Some(Ok(a)) => {
                    let names: Vec<String> = paths.iter().map(|p| file_name(p)).collect();
                    json!({"state": "ok", "lines": a.al.describe(&names, false).lines().map(|l| l.trim_start().to_string()).collect::<Vec<_>>()})
                }
                Some(Err(e)) => json!({"state": "err", "error": e}),
                None => json!({"state": "pending", "note": if self.mix_decoding.is_some() { "正在对齐…" } else { "解码后自动对齐" }}),
            }
        } else {
            Value::Null
        };
        let job = self.job.as_ref().map(|j| {
            let (f, n_done, n_run) = job_progress(j);
            let n = j.paths.len();
            let el = j.t0.elapsed().as_secs_f64();
            let eta = if f > 0.02 { format!("，预计还需 {}", playback_time(el / f as f64 * (1.0 - f as f64))) } else { String::new() };
            let text = if n == 1 {
                match j.status[0].lock().map(|g| g.clone()) {
                    Ok(ItemStatus::Running(_, st)) => format!("{st} {:.0}%{eta}", f * 100.0),
                    _ => format!("{:.0}%", f * 100.0),
                }
            } else {
                format!("{n_done} / {n} 完成，{n_run} 个在转换，{:.0}%{eta}", f * 100.0)
            };
            json!({"n": n, "frac": f, "text": text, "workers": j.workers})
        });
        let preview = self.preview_len().map(|len| {
            let aligned = self.align_on() && self.mix_audio.as_ref().map_or(false, |m| matches!(m.aligned, Some(Ok(_))));
            json!({"pos": self.preview_s, "len": len as f64 / FS as f64, "aligned": aligned})
        });
        let previewing = self.rt.is_some() && self.rt_preview;
        let can_preview = if multi {
            self.mix_decoding.is_none()
        } else if mixing {
            self.file_in.is_some() && self.mix_decoding.is_none()
        } else {
            self.file_in.is_some() && self.decoding.is_none()
        };
        let out_name = if multi {
            self.files.first().map(|p| file_name(&default_mix_output(p))).unwrap_or_default()
        } else {
            self.file_in.as_ref().or(self.files.first()).map(|p| file_name(&default_output(p))).unwrap_or_default()
        };
        let pending: Vec<Value> = self.pending_out.iter().map(|(o, i, m)| json!({"out": o.display().to_string(), "input": i.display().to_string(), "mix": m})).collect();
        json!({
            "files": files,
            "selected": selected,
            "mix_sel": self.mix_sel,
            "multi": multi,
            "duo": self.duo(),
            "stagger": if multi && self.s.duo_ms > 0.0 { stagger_text(self.s.duo_ms, n_voices) } else { String::new() },
            "mixing": mixing,
            "align_on": self.align_on(),
            "n_voices": n_voices,
            "voices": voices,
            "follow_offsets": mix::follow_offsets(n_voices.max(1), true),
            "positions": if mixing { self.mix_positions().iter().map(|q| q.to_vec()).collect::<Vec<_>>() } else { Vec::new() },
            "align": align,
            "job": job,
            "busy": self.job.is_some(),
            "report": self.text_with_labels(&self.report),
            "status": self.status,
            "previewing": previewing,
            "rt_mix": self.rt_mix,
            "can_preview": can_preview,
            "preview": preview,
            "tele": self.tele_array(),
            "start_s": self.start_s,
            "dur_s": self.dur_s,
            "threads": self.threads(),
            "cores": batch::default_threads(),
            "out_name": out_name,
            "decoding": self.decoding.is_some() || self.mix_decoding.is_some(),
            "pending_out": pending,
        })
    }

    /// 处理一条命令（JSON），返回整页状态。
    fn handle(&mut self, cmd: &Value) -> Value {
        // 先跟上配置（Kotlin 刚改过的设置），再照桌面版一帧的顺序：后台事务 → 合成同步 → 平衡 → 这次的操作 → 参数
        self.sync_saved();
        self.poll();
        self.sync_mix();
        self.sync_balance();
        let num = |k: &str| cmd.get(k).and_then(|v| v.as_f64());
        let idx = |k: &str| cmd.get(k).and_then(|v| v.as_u64()).map(|v| v as usize);
        match cmd.get("op").and_then(|v| v.as_str()).unwrap_or("poll") {
            "add" => {
                let paths: Vec<PathBuf> = cmd.get("paths").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(PathBuf::from).collect()).unwrap_or_default();
                let select = cmd.get("select").and_then(|v| v.as_bool()).unwrap_or(false);
                self.add_files(paths, select);
                self.sync_mix();
            }
            "remove" => {
                if let Some(i) = idx("index") {
                    self.remove_file(i);
                    self.sync_mix();
                }
            }
            "clear" => self.clear_files(),
            "select" => {
                if let Some(i) = idx("index") {
                    self.select(i);
                    self.sync_mix();
                }
            }
            "mix_sel" => self.mix_sel = idx("index"),
            "voice" => {
                if let Some(k) = idx("index") {
                    let voices = self.mix_voices();
                    if let Some(&v0) = voices.get(k) {
                        let mut v = v0;
                        if let Some(p) = idx("preset").filter(|&p| p < PRESETS.len()) {
                            v = Voice { gain_db: v.gain_db, ..Voice::preset(p) };
                        }
                        if let Some(g) = num("gain") {
                            v.gain_db = (g as f32).clamp(-30.0, 12.0);
                        }
                        if let (Some(az), Some(r)) = (num("az"), num("dist")) {
                            v = Voice { az: (az as f32).rem_euclid(360.0), dist: Some(r as f32), ..v };
                        }
                        self.set_mix_voice(k, v);
                    }
                }
            }
            "range" => {
                if let Some(s) = num("start") {
                    self.start_s = (s as f32).clamp(0.0, 36000.0);
                    self.preview_s = self.start_s as f64;
                }
                if let Some(d) = num("dur") {
                    self.dur_s = (d as f32).clamp(0.0, 36000.0);
                }
            }
            "preview" => self.start_preview(),
            "stop" => {
                self.stop();
                self.status = "已停止".to_string();
            }
            "seek" => {
                if let Some(s) = num("s") {
                    self.seek(s);
                }
            }
            "drag" => self.preview_dragging = cmd.get("on").and_then(|v| v.as_bool()).unwrap_or(false),
            "convert" => {
                let staging = PathBuf::from(cmd.get("staging").and_then(|v| v.as_str()).unwrap_or(""));
                self.start_convert(&staging);
            }
            "cancel" => {
                if let Some(j) = &self.job {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
            "delivered" => {
                let out = PathBuf::from(cmd.get("out").and_then(|v| v.as_str()).unwrap_or(""));
                let label = cmd.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();
                self.pending_out.retain(|(o, _, _)| *o != out);
                self.delivered.insert(out, label);
            }
            "release" => {
                // 离开文件页、App 退到后台：停掉试听（不影响正在转换的任务）
                self.stop();
            }
            _ => {}
        }
        self.sync_params();
        self.state()
    }
}

/// 总进度（按估计时长加权，0–1）、已结束的文件数、正在转换的文件数（同桌面版）。
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

/// 处理一条命令（JSON 文本），返回整页状态（JSON 文本）。
pub fn handle(cmd: &str) -> String {
    let v: Value = serde_json::from_str(cmd).unwrap_or_else(|_| json!({"op": "poll"}));
    let mut g = FILES.lock().unwrap_or_else(|e| e.into_inner());
    let f = g.get_or_insert_with(Files::new);
    f.handle(&v).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_asmr::dsp::testsig::test_voice;

    fn write_wav(path: &Path, x: &[f32]) {
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &v in x {
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
    }

    fn wait(f: &mut Files, cond: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..600 {
            let st = f.handle(&json!({"op": "poll"}));
            if cond(&st) {
                return st;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("等太久了");
    }

    /// 两个文件：单个转换（批量，暂存目录里各一个 _asmr.wav）、打开多音频合成后合成并保存，
    /// 各步的状态、完成后要交给 Kotlin 的输出和说明都对。
    #[test]
    fn convert_batch_and_mix_through_the_command_interface() {
        let _lock = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::settings::provide_repo_data();
        for (k, v) in [("duo_ms", json!(0.0)), ("mix", json!(false)), ("mix_align", json!(false)), ("mix_follow", json!(false)), ("mix_balance", json!(0.0))] {
            crate::settings::update_doc(k, v).unwrap();
        }
        let dir = std::env::temp_dir().join(format!("asmr-files-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("甲.wav"), dir.join("乙.wav"));
        write_wav(&a, &test_voice(4.0));
        write_wav(&b, &test_voice(3.0));
        let staging = dir.join("out");
        let mut f = Files::new();
        let st = f.handle(&json!({"op": "add", "paths": [a.display().to_string(), b.display().to_string()]}));
        assert_eq!(st["files"].as_array().unwrap().len(), 2);
        assert_eq!(st["selected"], json!(0));
        assert_eq!(st["out_name"], json!("甲_asmr.wav"));
        // 选中的第一个解码好之后播放条出现（4 秒）
        let st = wait(&mut f, |s| s["preview"].is_object());
        assert!((st["preview"]["len"].as_f64().unwrap() - 4.0).abs() < 0.05);
        // 全部转换：暂存目录里两个输出，两个都要交给 Kotlin
        f.handle(&json!({"op": "convert", "staging": staging.display().to_string()}));
        let st = wait(&mut f, |s| !s["busy"].as_bool().unwrap());
        assert_eq!(st["files"][0]["status"]["kind"], json!("done"), "{st}");
        assert_eq!(st["files"][1]["status"]["kind"], json!("done"));
        assert!(staging.join("1/甲_asmr.wav").is_file() && staging.join("1/乙_asmr.wav").is_file());
        assert_eq!(st["pending_out"].as_array().unwrap().len(), 2);
        assert!(st["report"].as_str().unwrap().starts_with("已转换 2 / 2"), "{}", st["report"]);
        // 交好了：列表里显示最终位置，不再要交
        let out = staging.join("1/甲_asmr.wav").display().to_string();
        let st = f.handle(&json!({"op": "delivered", "out": out, "label": "音乐/auto-asmr/甲_asmr.wav"}));
        assert_eq!(st["files"][0]["status"]["out"], json!("音乐/auto-asmr/甲_asmr.wav"));
        assert_eq!(st["pending_out"].as_array().unwrap().len(), 1);
        // 多音频合成：一左一右，合成并保存一个文件，说明里写着两个音频和位置
        crate::settings::update_doc("mix", json!(true)).unwrap();
        let st = f.handle(&json!({"op": "poll"}));
        assert!(st["multi"].as_bool().unwrap() && st["mixing"].as_bool().unwrap());
        assert_eq!(st["voices"][0]["name"], json!("左耳"));
        assert_eq!(st["voices"][1]["name"], json!("右耳"));
        assert_eq!(st["out_name"], json!("甲_mix_asmr.wav"));
        f.handle(&json!({"op": "voice", "index": 1, "gain": -3.0}));
        f.handle(&json!({"op": "convert", "staging": staging.display().to_string()}));
        let st = wait(&mut f, |s| !s["busy"].as_bool().unwrap());
        let report = st["report"].as_str().unwrap();
        assert!(report.contains("1. 甲.wav（左耳）") && report.contains("2. 乙.wav（右耳，-3.0 dB）"), "{report}");
        assert!(staging.join("2/甲_mix_asmr.wav").is_file());
        // 合成时左右双人的滑条是错开延迟：第 2 个音频晚这么多，不动合成的排布；合成说明里写着错开了多少
        crate::settings::update_doc("duo_ms", json!(100.0)).unwrap();
        let st = f.handle(&json!({"op": "poll"}));
        assert!(st["multi"].as_bool().unwrap() && !st["duo"].as_bool().unwrap());
        assert_eq!(st["stagger"], json!("错开：第 2 个晚 100 ms"));
        assert!(!crate::settings::with_global(|g| g.saved.mix_follow), "合成的排布不变");
        assert_eq!(f.mix_delays()[..2], [0, 4800]);
        f.handle(&json!({"op": "convert", "staging": staging.display().to_string()}));
        let st = wait(&mut f, |s| !s["busy"].as_bool().unwrap());
        let report = st["report"].as_str().unwrap();
        assert!(report.contains("错开：第 2 个晚 100 ms"), "{report}");
        crate::settings::update_doc("duo_ms", json!(0.0)).unwrap();
        crate::settings::update_doc("mix", json!(false)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 听歌：按了「听歌」之后，选中的立体声文件左右声道都解码（各当一个声源），转换的说明里写着立体声和响度；
    /// 目标响度跟着换成 −14；切回人声的风格后重新按"更响的那个声道"解码成一个声源。
    #[test]
    fn music_style_decodes_both_channels_and_converts_as_stereo() {
        let _lock = crate::settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::settings::provide_repo_data();
        crate::settings::load_config("()").unwrap();
        let dir = std::env::temp_dir().join(format!("asmr-files-music-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("歌.wav");
        {
            let (l, r) = (auto_asmr::dsp::testsig::test_song(4.0, 51), auto_asmr::dsp::testsig::test_song(4.0, 52));
            let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
            let mut w = hound::WavWriter::create(&song, spec).unwrap();
            for (a, b) in l.iter().zip(&r) {
                w.write_sample(*a).unwrap();
                w.write_sample(*b).unwrap();
            }
            w.finalize().unwrap();
        }
        let mut f = Files::new();
        f.handle(&json!({"op": "add", "paths": [song.display().to_string()]}));
        wait(&mut f, |s| s["preview"].is_object());
        assert!(f.decoded_r.is_none() && f.ch_key() == 0, "人声的风格：取更响的那个声道，一个声源");
        // 按「听歌」：下一次轮询发现取法变了，重新解码成左右两段
        crate::settings::run_action("style_music").unwrap();
        f.handle(&json!({"op": "poll"}));
        assert_eq!(f.ch_key(), 4);
        wait(&mut f, |s| s["preview"].is_object() && !s["decoding"].as_bool().unwrap());
        assert!(f.decoded_r.is_some() && f.decoded.as_ref().is_some_and(|(_, c, _)| *c == 4));
        assert_eq!(crate::settings::with_global(|g| g.saved.file_lufs), -14.0);
        let staging = dir.join("out");
        f.handle(&json!({"op": "convert", "staging": staging.display().to_string()}));
        let st = wait(&mut f, |s| !s["busy"].as_bool().unwrap());
        let report = st["report"].as_str().unwrap();
        assert!(report.contains("左右声道各当一个声源") && report.contains("和原声一样响"), "{report}");
        // 多音频合成 / 左右双人里是左右混合成一个声源
        crate::settings::update_doc("duo_ms", json!(80.0)).unwrap();
        f.handle(&json!({"op": "poll"}));
        assert_eq!(f.ch_key(), 3);
        crate::settings::update_doc("duo_ms", json!(0.0)).unwrap();
        // 切回贴耳加强：重新解码成一个声源
        crate::settings::run_action("style_ear").unwrap();
        f.handle(&json!({"op": "poll"}));
        wait(&mut f, |s| s["preview"].is_object() && !s["decoding"].as_bool().unwrap());
        assert!(f.decoded_r.is_none() && f.decoded.as_ref().is_some_and(|(_, c, _)| *c == 0));
        crate::settings::load_config("()").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
