//! 实时模式：麦克风 → 引擎 → 耳机 / 虚拟声卡。
//!
//! 输入回调把麦克风的声音（重采样到 48 kHz）写进无锁环形缓冲；输出回调取出来、过引擎、再重采样到
//! 输出设备的采样率。两个设备的时钟不会完全一致，用输出端重采样比例的微调（±0.5%）把缓冲量稳在目标值。
//!
//! 主播模式的左右双人：麦克风的声音同时交给两个引擎（[`Mixer`]，一个在左耳、一个在右耳），输出回调把麦克风的输入
//! 记进一小段历史（[`HIST`]），第 2 个声音从里面晚几十毫秒取——和试听文件时的左右双人是同一套做法。

use crate::dsp::engine::{Engine, EngineConfig, PostMode};
use crate::dsp::mix::{self, MixSetup, Mixer, MAX_VOICES};
use crate::dsp::solo::Solo;
use crate::dsp::{FS, FS_USIZE};
use crate::live::{mic_sink, Dropout, Proc, Pump, Shared, Source};
use crate::spsc;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, FromSample, Sample, SampleFormat, SizedSample, StreamConfig, SupportedBufferSize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceList {
    pub inputs: Vec<DeviceInfo>,
    pub outputs: Vec<DeviceInfo>,
}

/// 本机可用的音频后端（Windows：WASAPI；macOS：CoreAudio；Linux：ALSA 等）。
pub fn hosts() -> Vec<(cpal::HostId, String)> {
    cpal::available_hosts().into_iter().map(|h| (h, h.name().to_string())).collect()
}

fn host_of(id: Option<cpal::HostId>) -> cpal::Host {
    id.and_then(|h| cpal::host_from_id(h).ok()).unwrap_or_else(cpal::default_host)
}

/// 列出某个后端下的输入、输出设备（自动查找，给下拉框用）。
pub fn list_devices(host: Option<cpal::HostId>) -> DeviceList {
    let host = host_of(host);
    let def_in = host.default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let def_out = host.default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let mut list = DeviceList::default();
    let info = |d: &cpal::Device, def: &Option<String>| -> Option<DeviceInfo> {
        let id = d.id().ok()?.to_string();
        let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| id.clone());
        Some(DeviceInfo { is_default: def.as_deref() == Some(id.as_str()), id, name })
    };
    if let Ok(devs) = host.input_devices() {
        list.inputs = devs.filter_map(|d| info(&d, &def_in)).collect();
    }
    if let Ok(devs) = host.output_devices() {
        list.outputs = devs.filter_map(|d| info(&d, &def_out)).collect();
    }
    list
}

pub fn print_devices() {
    for (h, name) in hosts() {
        println!("音频后端：{name}");
        let l = list_devices(Some(h));
        println!("  输入设备：");
        for d in &l.inputs {
            println!("    {}{}    [{}]", d.name, if d.is_default { "（默认）" } else { "" }, d.id);
        }
        println!("  输出设备：");
        for d in &l.outputs {
            println!("    {}{}    [{}]", d.name, if d.is_default { "（默认）" } else { "" }, d.id);
        }
    }
}

#[derive(Clone, Debug)]
pub struct RtOptions {
    pub host: Option<cpal::HostId>,
    /// 设备 id；None = 系统默认设备。
    pub input: Option<String>,
    pub output: Option<String>,
    /// 希望的缓冲帧数（0 = 设备默认）。越小延迟越低，太小会爆音。
    pub buffer_frames: u32,
    /// 输入取哪个声道：0、1…；usize::MAX = 所有声道取平均。
    pub input_channel: usize,
}

impl Default for RtOptions {
    fn default() -> Self {
        RtOptions { host: None, input: None, output: None, buffer_frames: 256, input_channel: 0 }
    }
}

/// cpal 报来的设备错误：短暂断流只计数（后端仍在运行，不能因此停掉音频流）；音频线程没能提到实时优先级时照常运行
/// （只是别的程序占满 CPU 时更容易断流）；其余的记下来让界面停掉。
trait StreamError {
    fn stream_error(&self, device: &str, error: cpal::Error);
}

impl StreamError for Shared {
    fn stream_error(&self, device: &str, error: cpal::Error) {
        if error.kind() == cpal::ErrorKind::Xrun {
            self.dropout(if device == "输入" { Dropout::InputDevice } else { Dropout::OutputDevice });
            return;
        }
        if realtime_denied(&error) {
            return;
        }
        self.record_error(format!("{device}设备出错：{error}"));
    }
}

/// cpal 的 realtime 特性在音频线程开始时提升优先级（Windows：MMCSS，失败时改用 TIME_CRITICAL）；系统不让提升时报的错。
/// Windows 两步都失败时报的是 BackendError，只能按说明文字认出来。
fn realtime_denied(error: &cpal::Error) -> bool {
    error.kind() == cpal::ErrorKind::RealtimeDenied || error.message().is_some_and(|m| m.contains("promote audio thread to real-time priority"))
}

#[derive(Clone, Debug, Default)]
pub struct RtInfo {
    pub input_name: String,
    pub output_name: String,
    pub in_rate: u32,
    pub out_rate: u32,
    pub in_channels: u16,
    pub out_channels: u16,
}

/// 录音设备连续丢数据时该不该重新开始。
///
/// 变声器（比如 RVC）往虚拟声卡里写、本程序从虚拟声卡的录音端读：录音端在本程序开始录音时才启动，两头的相对节奏在这一刻
/// 定下来。节奏不巧时读的一头会追上写的一头，虚拟声卡一直丢数据（Windows 报"数据不连续"），声音在交给本程序之前就已经断了，
/// 听起来从头到尾滋滋响，监听原声也一样；停掉重新开始一次，大多就落在好的节奏上。所以 [`GLITCH_WINDOW`] 秒内录音设备报告
/// 丢数据达到 [`GLITCH_COUNT`] 次就该重新开始；偶尔一次（真的麦克风也可能有）不算。
pub struct GlitchWatch {
    t0: std::time::Instant,
    seen: u32,
    recent: std::collections::VecDeque<f64>,
}

pub const GLITCH_WINDOW: f64 = 2.0;
pub const GLITCH_COUNT: usize = 3;
/// 每次手动点开始之后最多自动重新开始几次（都不行就停下来，提示去调大变声器 / 虚拟声卡的缓冲）。
pub const GLITCH_RESTARTS: u32 = 5;

impl GlitchWatch {
    pub fn new() -> Self {
        GlitchWatch { t0: std::time::Instant::now(), seen: 0, recent: Default::default() }
    }

    /// `input_device_dropouts`：录音设备报告丢数据的累计次数（[`Dropout::InputDevice`]）。返回 true = 该重新开始了。
    pub fn update(&mut self, input_device_dropouts: u32) -> bool {
        self.update_at(self.t0.elapsed().as_secs_f64(), input_device_dropouts)
    }

    fn update_at(&mut self, now: f64, input_device_dropouts: u32) -> bool {
        for _ in self.seen..input_device_dropouts {
            self.recent.push_back(now);
        }
        self.seen = self.seen.max(input_device_dropouts);
        while self.recent.front().is_some_and(|&t| t < now - GLITCH_WINDOW) {
            self.recent.pop_front();
        }
        self.recent.len() >= GLITCH_COUNT
    }
}

pub struct Realtime {
    _input: Option<cpal::Stream>,
    _output: cpal::Stream,
    pub shared: Arc<Shared>,
    pub info: RtInfo,
}

fn err_text(what: &str, e: cpal::Error) -> String {
    format!("{what}：{e}")
}

fn pick_config(dev: &cpal::Device, input: bool, want_frames: u32) -> Result<(StreamConfig, SampleFormat), String> {
    let def = if input { dev.default_input_config() } else { dev.default_output_config() }.map_err(|e| err_text("读取设备格式失败", e))?;
    let mut cfg = def.config();
    cfg.buffer_size = buffer_size(want_frames, def.buffer_size(), input, cfg.sample_rate, cfg!(windows));
    Ok((cfg, def.sample_format()))
}

/// Windows（WASAPI 共享模式）的录音缓冲至少留多少秒。录音是事件驱动的：每个包一到就交给我们，缓冲大只是来不及读时
/// 能多装，不增加延迟。太小了，虚拟声卡（比如接 RVC 变声器）一次交出好几个包、或者线程晚醒一下，装不下的就被系统丢掉，
/// 在交给我们之前就没了（cpal 报断流），后面留多少积压都补不回来。
const WASAPI_INPUT_MIN_S: f64 = 0.1;

/// 设备缓冲要多大。`want_frames` 是界面上的「缓冲」（0 = 默认）。驱动报了范围时夹在范围里；Windows 的普通（软件）音频栈
/// 不报范围（`Unknown`），但共享模式什么长度都接受，也按设置要（以前这时一律用系统默认，界面上的设置在 Windows 上不起作用）。
/// Windows 的录音缓冲另外至少留 [`WASAPI_INPUT_MIN_S`]。`wasapi`：是不是 Windows。
fn buffer_size(want_frames: u32, supported: &SupportedBufferSize, input: bool, rate: u32, wasapi: bool) -> BufferSize {
    let want = if wasapi && input { want_frames.max((rate as f64 * WASAPI_INPUT_MIN_S).round() as u32) } else { want_frames };
    match supported {
        _ if want == 0 => BufferSize::Default,
        SupportedBufferSize::Range { min, max } => BufferSize::Fixed(want.clamp(*min, *max)),
        SupportedBufferSize::Unknown if wasapi => BufferSize::Fixed(want),
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

fn build_in<T>(dev: &cpal::Device, cfg: StreamConfig, sel: usize, mut sink: impl FnMut(&[f32]) + Send + 'static, shared: Arc<Shared>) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = cfg.channels.max(1) as usize;
    let mut tmp: Vec<f32> = Vec::with_capacity(16384);
    let sh = shared.clone();
    dev.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            tmp.clear();
            for f in data.chunks_exact(ch) {
                let v = if sel < ch {
                    f32::from_sample(f[sel])
                } else {
                    f.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / ch as f32
                };
                tmp.push(v);
            }
            sink(&tmp);
        },
        move |e| sh.stream_error("输入", e),
        None,
    )
}

fn build_out<T>(dev: &cpal::Device, cfg: StreamConfig, mut fill: impl FnMut(&mut [f32], &mut [f32]) + Send + 'static, shared: Arc<Shared>) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = cfg.channels.max(1) as usize;
    let (mut l, mut r): (Vec<f32>, Vec<f32>) = (vec![0.0; 16384], vec![0.0; 16384]);
    let sh = shared.clone();
    dev.build_output_stream(
        cfg,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / ch;
            if l.len() < frames {
                l.resize(frames, 0.0);
                r.resize(frames, 0.0);
            }
            shared.out_frames.store(frames as u32, Ordering::Relaxed);
            fill(&mut l[..frames], &mut r[..frames]);
            for (i, f) in data.chunks_exact_mut(ch).enumerate() {
                if ch == 1 {
                    f[0] = T::from_sample(0.5 * (l[i] + r[i]));
                } else {
                    f[0] = T::from_sample(l[i]);
                    f[1] = T::from_sample(r[i]);
                    for o in f[2..].iter_mut() {
                        *o = T::EQUILIBRIUM;
                    }
                }
            }
        },
        move |e| sh.stream_error("输出", e),
        None,
    )
}

macro_rules! by_format {
    ($fmt:expr, $f:ident, $($arg:expr),*) => {
        match $fmt {
            SampleFormat::F32 => $f::<f32>($($arg),*),
            SampleFormat::I16 => $f::<i16>($($arg),*),
            SampleFormat::I32 => $f::<i32>($($arg),*),
            SampleFormat::U16 => $f::<u16>($($arg),*),
            SampleFormat::I8 => $f::<i8>($($arg),*),
            SampleFormat::U8 => $f::<u8>($($arg),*),
            SampleFormat::F64 => $f::<f64>($($arg),*),
            other => return Err(format!("不支持的采样格式：{other}")),
        }
    };
}

/// 实时引擎的输入从哪里来。
enum Feed {
    /// 麦克风；带着合成配置时是主播模式的左右双人（同一个麦克风交给几个声音，按各自的延迟取）。
    Mic(Option<Arc<MixSetup>>),
    /// 试听一个文件：（48 kHz 单声道, 从第几个样本开始）。
    File(Arc<Vec<f32>>, usize),
    /// 多音频合成的试听：（各段 48 kHz 单声道, 合成配置, 从第几个样本开始）。
    Mix(Vec<Arc<Vec<f32>>>, Arc<MixSetup>, usize),
    /// 试听一个立体声文件：（左声道, 右声道, 从第几个样本开始）。
    Stereo(Arc<Vec<f32>>, Arc<Vec<f32>>, usize),
}

impl Realtime {
    /// 麦克风 → 引擎 → 输出设备。
    pub fn start(opts: &RtOptions, cfg: Arc<EngineConfig>) -> Result<Realtime, String> {
        Self::start_inner(opts, cfg, Feed::Mic(None))
    }

    /// 主播模式的左右双人：麦克风 → 几个声音（`setup` 里每个音源一个引擎，一般是左耳、右耳两个）→ 相加 → 输出设备。
    /// 第 k 个声音的输入是麦克风晚 `setup.delays[k]` 个样本（最多 300 ms）；动作按主播模式（停顿超过 1 秒恢复正常速度）。
    /// 运行中用 [`Realtime::set_mix`] 改参数、位置、音量和延迟（延迟在 20 ms 里淡过去）；音源数、排布方式要重新开始才能改。
    pub fn start_duo(opts: &RtOptions, setup: Arc<MixSetup>) -> Result<Realtime, String> {
        let max = mix::delay_samples(mix::DUO_MAX_MS);
        if setup.cfgs.is_empty() || setup.cfgs.len() > MAX_VOICES || setup.delays.iter().any(|&d| d > max) {
            return Err(format!("左右双人需要 1–{MAX_VOICES} 个声音，延迟不超过 {:.0} ms", mix::DUO_MAX_MS));
        }
        let cfg = setup.cfgs[0].clone();
        Self::start_inner(opts, cfg, Feed::Mic(Some(setup)))
    }

    /// 试听：用实时引擎把一段已解码的音频（48 kHz 单声道）播到输出设备，可以边听边调参数。
    pub fn start_preview(opts: &RtOptions, cfg: Arc<EngineConfig>, audio: Arc<Vec<f32>>, start_sample: usize) -> Result<Realtime, String> {
        Self::start_inner(opts, cfg, Feed::File(audio, start_sample))
    }

    /// 试听一个立体声文件（左右声道各一段，48 kHz）：「立体声宽度」大于 0 时左右声道各当一个声源，为 0 时合成单声道
    /// 过一个引擎；边听边改参数（[`Realtime::set_config`]）时随时切换，不用重新开始。
    pub fn start_preview_stereo(opts: &RtOptions, cfg: Arc<EngineConfig>, left: Arc<Vec<f32>>, right: Arc<Vec<f32>>, start_sample: usize) -> Result<Realtime, String> {
        Self::start_inner(opts, cfg, Feed::Stereo(left, right, start_sample))
    }

    /// 多音频合成的试听：几段已解码的音频（48 kHz 单声道，与 `setup` 里的音源一一对应）各在自己的位置，同时播放。
    /// 播放中可以用 [`Realtime::set_mix`] 换参数、挪位置、改音量，不用重新开始。
    pub fn start_preview_mix(opts: &RtOptions, setup: Arc<MixSetup>, audio: Vec<Arc<Vec<f32>>>, start_sample: usize) -> Result<Realtime, String> {
        if audio.is_empty() || audio.len() != setup.cfgs.len() || audio.len() > MAX_VOICES {
            return Err(format!("合成试听需要 1–{MAX_VOICES} 段音频，且与位置一一对应"));
        }
        let cfg = setup.cfgs[0].clone();
        Self::start_inner(opts, cfg, Feed::Mix(audio, setup, start_sample))
    }

    fn start_inner(opts: &RtOptions, cfg: Arc<EngineConfig>, feed: Feed) -> Result<Realtime, String> {
        let host = host_of(opts.host);
        let find = |id: &Option<String>, input: bool| -> Result<cpal::Device, String> {
            let none = if input { "没有可用的输入设备（麦克风）" } else { "没有可用的输出设备" };
            match id {
                Some(s) => {
                    let did = s.parse().map_err(|_| format!("设备 id 无法识别：{s}"))?;
                    host.device_by_id(&did).ok_or_else(|| "找不到所选设备（可能已拔出），请重载设备列表".to_string())
                }
                None => if input { host.default_input_device() } else { host.default_output_device() }.ok_or_else(|| none.to_string()),
            }
        };
        let name = |d: &cpal::Device| d.description().map(|x| x.name().to_string()).unwrap_or_default();
        let out_dev = find(&opts.output, false)?;
        let (out_cfg, out_fmt) = pick_config(&out_dev, false, opts.buffer_frames)?;
        let sh = Shared::new(cfg.clone());
        if let Feed::Mix(_, setup, _) | Feed::Mic(Some(setup)) = &feed {
            if let Ok(mut g) = sh.mix.lock() {
                *g = Some(setup.clone());
            }
        }
        let shared = Arc::new(sh);
        let out_rate = out_cfg.sample_rate as f64;
        let mut info = RtInfo { output_name: name(&out_dev), out_rate: out_cfg.sample_rate, out_channels: out_cfg.channels, ..Default::default() };

        let (input, src, proc) = match feed {
            Feed::File(audio, pos) => {
                info.input_name = "文件".into();
                info.in_rate = FS_USIZE as u32;
                info.in_channels = 1;
                // 试听不开主播模式的"话少时也一直在动"：动作严格跟着说话走，与文件转换一致
                let eng = Engine::new(cfg, PostMode::Realtime, 0.0);
                (None, Source::Memory { data: audio, pos }, Proc::One(eng))
            }
            Feed::Stereo(left, right, pos) => {
                info.input_name = "文件".into();
                info.in_rate = FS_USIZE as u32;
                info.in_channels = 2;
                // 立体声对事先建好：播放中改「立体声宽度」时音频线程里不分配内存
                let solo = Solo::new(cfg, PostMode::Realtime, 0.0).with_pair();
                (None, Source::Stereo { l: left, r: right, pos }, Proc::Solo(solo))
            }
            Feed::Mix(audio, setup, pos) => {
                info.input_name = format!("{} 个文件合成", audio.len());
                info.in_rate = FS_USIZE as u32;
                info.in_channels = audio.len() as u16;
                let lens: Vec<usize> = audio.iter().map(|a| a.len()).collect();
                let len = mix::timeline_len(&lens, &setup.delays);
                (None, Source::Multi { data: audio, pos, len }, Proc::Mix(Mixer::new(&setup, PostMode::Realtime, 0.0)))
            }
            Feed::Mic(duo) => {
                let in_dev = find(&opts.input, true)?;
                let (in_cfg, in_fmt) = pick_config(&in_dev, true, opts.buffer_frames).map_err(|e| format!("{e}（可能没有麦克风，或没有授予麦克风权限）"))?;
                info.input_name = name(&in_dev);
                info.in_rate = in_cfg.sample_rate;
                info.in_channels = in_cfg.channels;
                let (tx, rx) = spsc::channel(FS_USIZE); // 最多积压 1 秒
                let sink = mic_sink(in_cfg.sample_rate, tx, shared.clone());
                let st = by_format!(in_fmt, build_in, &in_dev, in_cfg, opts.input_channel, sink, shared.clone()).map_err(|e| err_text("打开输入设备失败（检查麦克风权限，或换一个缓冲大小）", e))?;
                // 主播模式：停顿超过 1 秒后动作恢复正常速度（左右双人时每个声音都这样）
                let proc = match duo {
                    Some(setup) => {
                        let mut m = Mixer::new(&setup, PostMode::Realtime, 0.0);
                        m.set_live(true);
                        Proc::Mix(m)
                    }
                    None => {
                        let mut eng = Engine::new(cfg, PostMode::Realtime, 0.0);
                        eng.set_live(true);
                        Proc::One(eng)
                    }
                };
                (Some(st), Source::Ring(rx), proc)
            }
        };
        let mut pump = Pump::with_proc(src, proc, shared.clone(), out_rate);
        let fill = move |l: &mut [f32], r: &mut [f32]| pump.fill(l, r);
        let output = by_format!(out_fmt, build_out, &out_dev, out_cfg, fill, shared.clone()).map_err(|e| err_text("打开输出设备失败（换一个缓冲大小试试）", e))?;
        if let Some(i) = &input {
            i.play().map_err(|e| err_text("启动输入失败", e))?;
        }
        output.play().map_err(|e| err_text("启动输出失败", e))?;
        Ok(Realtime { _input: input, _output: output, shared, info })
    }

    /// 界面改了参数后调用。
    pub fn set_config(&self, cfg: Arc<EngineConfig>) {
        self.shared.set_config(cfg);
    }

    /// 合成试听中改了参数、位置或音量后调用（音源数要与开始时相同）。
    pub fn set_mix(&self, setup: Arc<MixSetup>) {
        self.shared.set_mix(setup);
    }

    /// 估计的总延迟（毫秒）：输入缓冲 + 环形缓冲积压 + 算法 + 重采样 + 输出缓冲。不含声卡驱动自身的延迟。
    pub fn latency_ms(&self) -> f32 {
        let s = &self.shared;
        let inb = s.in_frames.load(Ordering::Relaxed) as f32 / FS * 1000.0;
        let ring = s.fill_us.load(Ordering::Relaxed) as f32 / 1000.0;
        let algo = s.algo_samples.load(Ordering::Relaxed) as f32 / FS * 1000.0;
        let outb = s.out_frames.load(Ordering::Relaxed) as f32 / self.info.out_rate.max(1) as f32 * 1000.0;
        inb + ring + algo + 0.7 + outb
    }

    /// 只算算法本身的延时（毫秒）。
    pub fn algo_ms(&self) -> f32 {
        self.shared.algo_samples.load(Ordering::Relaxed) as f32 / FS * 1000.0
    }
}

/// 命令行自检：用默认设备跑几秒，打印延迟和处理时间。
pub fn selftest(seconds: f32, opts: &RtOptions, cfg: Arc<EngineConfig>, file: Option<std::path::PathBuf>) -> Result<(), String> {
    let rt = match file {
        Some(f) => {
            let mut o = crate::offline::ConvertOptions::new(f);
            o.duration = Some(seconds as f64 + 2.0);
            let (mono, _) = crate::offline::load_mono(&o, &AtomicBool::new(false))?;
            Realtime::start_preview(opts, cfg, Arc::new(mono), 0)?
        }
        None => Realtime::start(opts, cfg)?,
    };
    println!("输入：{}（{} Hz，{} 声道）\n输出：{}（{} Hz，{} 声道）", rt.info.input_name, rt.info.in_rate, rt.info.in_channels, rt.info.output_name, rt.info.out_rate, rt.info.out_channels);
    let steps = (seconds / 0.5).ceil() as usize;
    for _ in 0..steps {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let t = rt.shared.telemetry();
        println!(
            "  延迟约 {:.1} ms（算法 {:.1} ms，缓冲积压 {:.1} ms，输入 {} 帧 / 输出 {} 帧）；每次回调处理 {} µs（最大 {} µs）；断流 {} 次；输入 {:.0} dB，输出 {:.0} dB，方位 {:.0}°，距离 {:.2} m",
            rt.latency_ms(),
            rt.algo_ms(),
            rt.shared.fill_us.load(Ordering::Relaxed) as f32 / 1000.0,
            rt.shared.in_frames.load(Ordering::Relaxed),
            rt.shared.out_frames.load(Ordering::Relaxed),
            rt.shared.proc_us.load(Ordering::Relaxed),
            rt.shared.proc_max_us.load(Ordering::Relaxed),
            rt.shared.xruns.load(Ordering::Relaxed),
            t.in_db,
            t.out_db,
            t.az,
            t.r
        );
        if let Some(e) = rt.shared.take_error() {
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::params::Params;

    #[test]
    fn stream_xruns_are_counted_without_stopping_audio() {
        let shared = Shared::new(Arc::new(EngineConfig::new(Params::default(), None)));
        for device in ["输入", "输出"] {
            shared.stream_error(device, cpal::ErrorKind::Xrun.into());
        }
        assert_eq!(shared.xruns.load(Ordering::Relaxed), 2);
        assert!(shared.take_error().is_none());
    }

    /// Windows 上界面的「缓冲」真的生效（驱动不报范围时也按设置要），录音缓冲至少 100 ms；别的系统驱动不报范围时用默认，
    /// 报了范围就夹在范围里。
    #[test]
    fn buffer_sizes_follow_the_setting_and_keep_a_roomy_input_on_windows() {
        let unknown = SupportedBufferSize::Unknown;
        let range = SupportedBufferSize::Range { min: 128, max: 2048 };
        // Windows：输出按设置，录音至少 100 ms（48 kHz 4800 帧，44.1 kHz 4410 帧），设置更大就按设置
        assert_eq!(buffer_size(256, &unknown, false, 48_000, true), BufferSize::Fixed(256));
        assert_eq!(buffer_size(1024, &unknown, false, 48_000, true), BufferSize::Fixed(1024));
        assert_eq!(buffer_size(0, &unknown, false, 48_000, true), BufferSize::Default);
        assert_eq!(buffer_size(256, &unknown, true, 48_000, true), BufferSize::Fixed(4800));
        assert_eq!(buffer_size(0, &unknown, true, 44_100, true), BufferSize::Fixed(4410));
        assert_eq!(buffer_size(8192, &unknown, true, 48_000, true), BufferSize::Fixed(8192));
        assert_eq!(buffer_size(256, &range, true, 48_000, true), BufferSize::Fixed(2048));
        // 别的系统：录音和输出一样按设置；不报范围时用默认
        assert_eq!(buffer_size(256, &range, true, 48_000, false), BufferSize::Fixed(256));
        assert_eq!(buffer_size(64, &range, false, 48_000, false), BufferSize::Fixed(128));
        assert_eq!(buffer_size(256, &unknown, true, 48_000, false), BufferSize::Default);
        assert_eq!(buffer_size(0, &range, false, 48_000, false), BufferSize::Default);
    }

    /// 录音设备 2 秒内丢 3 次数据就该重新开始；隔得远的零星几次不算；一次报好几次也算。
    #[test]
    fn repeated_input_glitches_ask_for_a_restart() {
        let mut w = GlitchWatch::new();
        // 零星的：每 1.5 秒一次，2 秒窗口里最多两次
        for k in 1..=10 {
            assert!(!w.update_at(k as f64 * 1.5, k), "第 {k} 次");
        }
        // 接连的：半秒一次，第三次就该重新开始
        assert!(!w.update_at(20.0, 11));
        assert!(!w.update_at(20.5, 12));
        assert!(w.update_at(21.0, 13));
        // 两帧之间一下子多了 3 次
        let mut w = GlitchWatch::new();
        assert!(!w.update_at(1.0, 0));
        assert!(w.update_at(1.1, 3));
        // 计数没变就不算
        let mut w = GlitchWatch::new();
        for k in 0..100 {
            assert!(!w.update_at(k as f64 * 0.1, 2));
        }
    }

    /// 设备报的断流按输入 / 输出分开记。
    #[test]
    fn device_dropouts_are_counted_by_direction() {
        let shared = Shared::new(Arc::new(EngineConfig::new(Params::default(), None)));
        shared.stream_error("输入", cpal::ErrorKind::Xrun.into());
        shared.stream_error("输入", cpal::ErrorKind::Xrun.into());
        shared.stream_error("输出", cpal::ErrorKind::Xrun.into());
        assert_eq!(shared.dropouts(), [0, 0, 2, 1]);
        assert_eq!(shared.xruns.load(Ordering::Relaxed), 3);
    }

    /// 音频线程没能提到实时优先级（cpal 的 realtime 特性报的错）：照常运行，不停掉、不算断流。
    #[test]
    fn realtime_priority_refusal_keeps_audio_running() {
        let shared = Shared::new(Arc::new(EngineConfig::new(Params::default(), None)));
        for device in ["输入", "输出"] {
            shared.stream_error(device, cpal::ErrorKind::RealtimeDenied.into());
            // Windows：MMCSS 和 TIME_CRITICAL 都失败时，cpal 报的是带这句说明的 BackendError
            shared.stream_error(device, cpal::Error::with_message(cpal::ErrorKind::BackendError, "Failed to promote audio thread to real-time priority: Access is denied."));
        }
        assert!(shared.take_error().is_none());
        assert_eq!(shared.xruns.load(Ordering::Relaxed), 0);
        // 别的后端错误照样停掉
        shared.stream_error("输出", cpal::Error::with_message(cpal::ErrorKind::BackendError, "Failed to get current padding"));
        assert!(shared.take_error().is_some());
    }

    #[test]
    fn stream_xruns_do_not_hide_a_fatal_device_error() {
        let shared = Shared::new(Arc::new(EngineConfig::new(Params::default(), None)));
        for device in ["输入", "输出"] {
            shared.stream_error(device, cpal::ErrorKind::DeviceNotAvailable.into());
            shared.stream_error(device, cpal::ErrorKind::Xrun.into());
            let error = shared.take_error().expect("disconnected devices must still stop audio");
            assert!(error.starts_with(&format!("{device}设备出错：")));
            assert!(error.contains(&cpal::ErrorKind::DeviceNotAvailable.to_string()));
            assert!(shared.take_error().is_none());
        }
        assert_eq!(shared.xruns.load(Ordering::Relaxed), 2);
    }

    /// 用虚拟时钟模拟"输入回调每次 `n_in` 帧、输出回调每次 `n_out` 帧、输出设备时钟快 `drift`"，
    /// 返回 (断流次数, 稳定后的平均积压毫秒, 监听原声时测到的端到端延迟样本数)。
    fn simulate(n_in: usize, n_out: usize, drift: f64, seconds: f64, out_rate: f64) -> (u32, f32, i64) {
        let cfg = Arc::new(EngineConfig::new(Params::default(), None));
        let shared = Arc::new(Shared::new(cfg.clone()));
        shared.monitor_dry.store(true, Ordering::Relaxed);
        let (tx, rx) = spsc::channel(FS_USIZE);
        let mut sink = mic_sink(FS_USIZE as u32, tx, shared.clone());
        let mut pump = Pump::new(Source::Ring(rx), Engine::new(cfg, PostMode::Realtime, 0.0), shared.clone(), out_rate);
        let (t_in, t_out) = (n_in as f64 / FS as f64, n_out as f64 / (out_rate * (1.0 + drift)));
        let (mut next_in, mut next_out) = (0.0f64, 0.0005f64);
        let (mut n_fed, mut n_got) = (0usize, 0usize);
        let mut inb = vec![0f32; n_in];
        let (mut ol, mut or) = (vec![0f32; n_out], vec![0f32; n_out]);
        let click_in = (FS as f64 * seconds * 0.8) as usize; // 输入里放一个脉冲，测延迟
        let mut click_out: i64 = -1;
        let mut xr_after_start = 0;
        let mut fills = Vec::new();
        while next_in < seconds {
            if next_in <= next_out {
                for (i, v) in inb.iter_mut().enumerate() {
                    *v = if n_fed + i == click_in { 1.0 } else { 0.0 };
                }
                sink(&inb);
                n_fed += n_in;
                next_in += t_in;
            } else {
                pump.fill(&mut ol, &mut or);
                if click_out < 0 {
                    if let Some(i) = ol.iter().position(|v| v.abs() > 0.5) {
                        click_out = (n_got + i) as i64;
                    }
                }
                n_got += n_out;
                next_out += t_out;
                if next_out > 1.0 {
                    fills.push(shared.fill_us.load(Ordering::Relaxed) as f32 / 1000.0);
                    if next_out > 2.0 && xr_after_start == 0 {
                        xr_after_start = shared.xruns.load(Ordering::Relaxed);
                    }
                }
            }
        }
        let xr = shared.xruns.load(Ordering::Relaxed) - xr_after_start;
        let avg = fills.iter().rev().take(200).sum::<f32>() / 200.0;
        // 输出脉冲的位置换算回 48 kHz 的输入时间轴
        let delay = if click_out >= 0 { (click_out as f64 * FS as f64 / (out_rate * (1.0 + drift))) as i64 - click_in as i64 + (0.0005 * FS as f64) as i64 } else { -1 };
        (xr, avg, delay)
    }

    /// 输入的到达方式：每个包 [`PACKET`] 帧，第 k 个包本该在 k × 10 ms 到，实际由 `deliver(本该到的时间)` 给出什么时候交给
    /// 我们（一阵一阵地来、晚到）；输出回调每次 `n_out` 帧，比输入晚 `phase` 秒开始；两边每次醒来的时间再随机晚 0–`jitter` 秒
    /// （线程调度）。两个设备同一个时钟。`tone`：输入是 0.5 幅度的 1 kHz 正弦，否则是静音。监听原声。
    /// 返回每次输出回调之后的（时间，累计断流次数，积压毫秒）、整段左声道输出，以及按原因分的断流次数。
    fn simulate_arrivals(n_out: usize, phase: f64, jitter: f64, seconds: f64, deliver: impl Fn(f64) -> f64, tone: bool) -> (Vec<(f64, u32, f32)>, Vec<f32>, [u32; 4]) {
        let cfg = Arc::new(EngineConfig::new(Params::default(), None));
        let shared = Arc::new(Shared::new(cfg.clone()));
        shared.monitor_dry.store(true, Ordering::Relaxed);
        let (tx, rx) = spsc::channel(FS_USIZE);
        let mut sink = mic_sink(FS_USIZE as u32, tx, shared.clone());
        let mut pump = Pump::new(Source::Ring(rx), Engine::new(cfg, PostMode::Realtime, 0.0), shared.clone(), FS as f64);
        let mut rng = crate::dsp::rng::Rng::new(7);
        let (t_in, t_out) = (PACKET as f64 / FS as f64, n_out as f64 / FS as f64);
        // (醒来的时间，输出回调 = true / 第几个输入包)
        let mut ev: Vec<(f64, Option<usize>)> = Vec::new();
        let mut k = 0;
        while k as f64 * t_in < seconds {
            ev.push((deliver(k as f64 * t_in) + jitter * rng.unit() as f64, Some(k)));
            k += 1;
        }
        k = 0;
        while k as f64 * t_out + phase < seconds {
            ev.push((k as f64 * t_out + phase + jitter * rng.unit() as f64, None));
            k += 1;
        }
        // 同一时刻先交输入；同一阵里的包按顺序
        ev.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.is_some().cmp(&a.1.is_some())).then(a.1.cmp(&b.1)));
        let mut inb = vec![0f32; PACKET];
        let (mut ol, mut or) = (vec![0f32; n_out], vec![0f32; n_out]);
        let (mut log, mut out) = (Vec::new(), Vec::new());
        for (t, packet) in ev {
            match packet {
                Some(k) => {
                    for (i, v) in inb.iter_mut().enumerate() {
                        let n = (k * PACKET + i) as f32;
                        *v = if tone { 0.5 * (std::f32::consts::TAU * 1000.0 * n / FS).sin() } else { 0.0 };
                    }
                    sink(&inb);
                }
                None => {
                    pump.fill(&mut ol, &mut or);
                    out.extend_from_slice(&ol);
                    log.push((t, shared.xruns.load(Ordering::Relaxed), shared.fill_us.load(Ordering::Relaxed) as f32 / 1000.0));
                }
            }
        }
        (log, out, shared.dropouts())
    }

    /// 输入包的大小（Windows 共享模式常见的 10 ms）。
    const PACKET: usize = 480;

    /// 每 `n` 个包攒成一阵一起交出来（最后一个包该到的时候）。
    fn bursts(n: usize) -> impl Fn(f64) -> f64 {
        let t_in = PACKET as f64 / FS as f64;
        move |t: f64| ((t / t_in).round() as usize / n * n + n - 1) as f64 * t_in
    }

    /// 从 `log` 里取 [a, b) 秒之间的断流次数和平均积压毫秒。
    fn span(log: &[(f64, u32, f32)], a: f64, b: f64) -> (u32, f32) {
        let s: Vec<_> = log.iter().filter(|e| e.0 >= a && e.0 < b).collect();
        let before = log.iter().take_while(|e| e.0 < a).last().map_or(0, |e| e.1);
        (s.last().map_or(before, |e| e.1) - before, s.iter().map(|e| e.2).sum::<f32>() / s.len().max(1) as f32)
    }

    /// 虚拟声卡把输入攒成 30 / 40 ms 一阵交出来（比如接 RVC 变声器）：以前只按一个包留余量，平均积压够、低谷不够，
    /// 启动时相位不巧就每一阵都见底、从头断到尾（60 秒几百次）。现在开头断几次、多留出这一阵的量之后就不再断。
    #[test]
    fn bursty_input_settles_after_a_few_dropouts() {
        for n_out in [256, 480, 1024] {
            for n in [3, 4] {
                for phase_ms in [0.0, 2.0, 5.0, 9.0, 25.0] {
                    let (log, _, _) = simulate_arrivals(n_out, phase_ms / 1000.0, 0.001, 60.0, bursts(n), false);
                    let (early, _) = span(&log, 0.0, 5.0);
                    let (late, _) = span(&log, 5.0, 60.0);
                    let (_, fill) = span(&log, 50.0, 60.0);
                    let at = format!("输出 {n_out} 帧，{n} 个包一阵，相位 {phase_ms} ms");
                    assert!(early <= 10, "{at}：开头 5 秒断流 {early} 次");
                    assert!(late <= 2, "{at}：之后 55 秒又断 {late} 次");
                    assert!(fill < 40.0, "{at}：积压 {fill} ms");
                }
            }
        }
    }

    /// 输入按时一个包一个包地来（真的麦克风），两边醒来的时间各有 0–1 ms 的随机早晚：一次都不断，积压不比以前多。
    #[test]
    fn regular_input_keeps_its_low_latency() {
        for n_out in [256, 480, 1024] {
            for phase_ms in [0.0, 0.5, 5.0, 9.5] {
                let (log, _, _) = simulate_arrivals(n_out, phase_ms / 1000.0, 0.001, 30.0, |t| t, false);
                let (xr, fill) = span(&log, 0.0, 30.0);
                assert_eq!(xr, 0, "输出 {n_out} 帧，相位 {phase_ms} ms");
                assert!(fill < 14.0, "输出 {n_out} 帧，相位 {phase_ms} ms：积压 {fill} ms");
            }
        }
    }

    /// 一阵一阵的输入停了、恢复按时来：多留的积压慢慢减回去，回到和一直按时来时差不多的延迟，期间不断流。
    #[test]
    fn extra_backlog_shrinks_once_input_is_regular_again() {
        let t_in = PACKET as f64 / FS as f64;
        let four = bursts(4);
        let (log, _, _) = simulate_arrivals(256, 0.0, 0.001, 120.0, move |t| if t < 20.0 - 4.0 * t_in { four(t) } else { t }, false);
        let (_, bursty) = span(&log, 15.0, 20.0);
        let (xr, _) = span(&log, 20.0, 120.0);
        let (_, end) = span(&log, 110.0, 120.0);
        assert!(bursty > 18.0, "一阵一阵时积压 {bursty} ms");
        assert_eq!(xr, 0, "恢复按时后又断流");
        assert!(end < 14.0, "恢复按时 90 秒后积压还有 {end} ms");
    }

    /// 输入中途卡住 40 ms（之后一起补上）：断流的地方先淡出、攒够了再淡入，波形没有突然的跳变（"啪"）。
    #[test]
    fn a_dropout_fades_instead_of_clicking() {
        let (log, out, kinds) = simulate_arrivals(256, 0.002, 0.0, 6.0, |t| if (3.0..3.04).contains(&t) { 3.04 } else { t }, true);
        let (xr, _) = span(&log, 0.0, 6.0);
        assert!(xr >= 1, "这个卡顿应当造成断流");
        assert_eq!(kinds, [xr, 0, 0, 0], "记成输入没跟上");
        // 0.5 幅度的 1 kHz 正弦相邻两个样本最多差 0.065
        let step = out[FS_USIZE..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        assert!(step < 0.1, "最大跳变 {step}");
    }

    /// 输入卡住 0.4 秒后一下子全到：先记"输入没跟上"，积压超过目标 150 ms 后丢掉旧的，记"积压丢弃"，之后不再断。
    #[test]
    fn a_flood_after_a_long_gap_is_dropped_as_backlog() {
        let (log, _, kinds) = simulate_arrivals(480, 0.002, 0.0, 10.0, |t| if (3.0..3.4).contains(&t) { 3.4 } else { t }, false);
        assert!(kinds[0] >= 1 && kinds[1] >= 1 && kinds[2] == 0 && kinds[3] == 0, "{kinds:?}");
        let (late, fill) = span(&log, 4.0, 10.0);
        assert_eq!(late, 0);
        assert!(fill < 60.0, "积压 {fill} ms");
    }

    #[test]
    fn steady_when_clocks_match() {
        for (n_in, n_out) in [(128, 128), (256, 256), (480, 512), (512, 128), (441, 480)] {
            let (xr, fill_ms, delay) = simulate(n_in, n_out, 0.0, 12.0, 48_000.0);
            assert_eq!(xr, 0, "in {n_in} out {n_out}: xruns {xr}");
            println!("输入 {n_in} 帧 / 输出 {n_out} 帧：缓冲积压 {fill_ms:.1} ms，监听原声的端到端延迟 {:.1} ms", delay as f32 / 48.0);
            let target = n_in as f32 / 48.0 + 2.0;
            assert!(fill_ms > 0.0 && fill_ms < target + 4.0 + n_out as f32 / 48.0, "in {n_in} out {n_out}: fill {fill_ms} ms");
            assert!(delay > 0 && (delay as f32) < (fill_ms + 2.0 * n_out as f32 / 48.0 + n_in as f32 / 48.0 + 3.0) * 48.0, "in {n_in} out {n_out}: delay {delay}");
        }
    }

    #[test]
    fn drift_is_absorbed() {
        // 输出设备的时钟快 / 慢 0.1%（比真实声卡之间的偏差大得多），跑 60 秒不应断流、积压不应越来越多
        for drift in [0.001, -0.001] {
            let (xr, fill_ms, _) = simulate(256, 256, drift, 60.0, 48_000.0);
            assert_eq!(xr, 0, "drift {drift}: xruns {xr}");
            assert!(fill_ms > 1.0 && fill_ms < 20.0, "drift {drift}: fill {fill_ms} ms");
        }
    }

    #[test]
    fn other_output_rates_work() {
        let (xr, fill_ms, delay) = simulate(256, 256, 0.0, 12.0, 44_100.0);
        assert_eq!(xr, 0);
        assert!(fill_ms < 15.0 && delay > 0, "fill {fill_ms} delay {delay}");
    }
}
