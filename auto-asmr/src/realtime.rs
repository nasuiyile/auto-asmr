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
use crate::live::{mic_sink, Proc, Pump, Shared, Source};
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

/// cpal 报来的设备错误：短暂断流只计数（后端仍在运行，不能因此停掉音频流），其余的记下来让界面停掉。
trait StreamError {
    fn stream_error(&self, device: &str, error: cpal::Error);
}

impl StreamError for Shared {
    fn stream_error(&self, device: &str, error: cpal::Error) {
        if error.kind() == cpal::ErrorKind::Xrun {
            self.xruns.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.record_error(format!("{device}设备出错：{error}"));
    }
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
    cfg.buffer_size = match (want_frames, def.buffer_size()) {
        (0, _) => BufferSize::Default,
        (n, SupportedBufferSize::Range { min, max }) => BufferSize::Fixed(n.clamp(*min, *max)),
        (_, _) => BufferSize::Default,
    };
    Ok((cfg, def.sample_format()))
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
