//! 文件模式：解码任意常见格式 → 取一个声道 → 48 kHz → 引擎（带约 1 秒预读）→ 精确响度归一化和真峰值限制 → WAV。
//! 多音频合成（[`convert_mix`]）：几个文件各过一个引擎、各在一个位置，相加之后同样两遍处理；
//! 可以先自动对齐（[`align_tracks`]：同一首歌的几个版本，从一起开始唱的地方叠加）。
//! 左右双人（[`ConvertOptions::duo`]）：一个文件当成两个声音合成，第 2 个晚几十毫秒，走的也是 [`convert_mix`]。

use crate::dsp::align::{self, Alignment};
use crate::dsp::analysis::prescan;
use crate::dsp::balance::{self, BalanceData};
use crate::dsp::dynamics::{Limiter, LoudnessMeter, TruePeak};
use crate::dsp::engine::{Engine, EngineConfig, PostMode};
use crate::dsp::mix::{self, Layout, MixSetup, Mixer, Voice, MAX_VOICES};
use crate::dsp::params::Params;
use crate::dsp::rain::{self, Rain};
use crate::dsp::resample::Resampler;
use crate::dsp::{db_to_lin, FS, FS_USIZE};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// 立体声输入取哪个声道。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Auto,
    Left,
    Right,
    Mix,
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub input: PathBuf,
    pub output: PathBuf,
    pub start: f64,
    pub duration: Option<f64>,
    pub channel: Channel,
    pub bits: u16,
    pub lufs: f32,
    pub peak_db: f32,
    pub max_gr_db: f32,
    /// 把实际走位写成 CSV（时间, 方位角, 距离, 不含忽远忽近的距离, 说话活动, 仰角, 入耳深度）。
    pub save_trajectory: Option<PathBuf>,
    /// 左右双人：同一个声音一左一右两个人说，第 2 个晚一点（None = 普通转换）。批量转换时每个文件都这样做。
    pub duo: Option<Duo>,
}

impl ConvertOptions {
    pub fn new(input: PathBuf) -> Self {
        let output = default_output(&input);
        ConvertOptions { input, output, start: 0.0, duration: None, channel: Channel::Auto, bits: 24, lufs: -16.0, peak_db: -1.0, max_gr_db: 8.0, save_trajectory: None, duo: None }
    }
}

/// 左右双人：把一个音频当成两个声音合成（和导入两个音频的多音频合成一样：各在一个位置、各自忽远忽近），
/// 第 2 个声音整体晚 `delay_ms`，听起来是两个人一前一后地说。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Duo {
    /// 第 2 个声音晚多少毫秒（0–[`mix::DUO_MAX_MS`]）。
    pub delay_ms: f32,
    /// 两个声音的位置和音量（默认左耳、右耳）。
    pub voices: [Voice; 2],
    /// 排布：各自固定、一起移动（按移动方式一起走、彼此相对）或镜像（左右对称，各守一边）。
    pub layout: Layout,
    /// 自动平衡的强度（0–1，0 = 关）。
    pub balance: f32,
}

impl Duo {
    /// 默认：一个在左耳、一个在右耳，各自固定，不平衡。
    pub fn new(delay_ms: f32) -> Self {
        Duo { delay_ms, voices: [Voice::preset(0), Voice::preset(1)], layout: Layout::Fixed, balance: 0.0 }
    }

    /// 一句话说明（写进转换完成后的报告）。
    pub fn describe(&self, english: bool) -> String {
        let name = |v: &Voice| match v.preset_index() {
            Some(i) => if english { mix::PRESETS[i].1 } else { mix::PRESETS[i].0 }.to_string(),
            None => format!("{:.0}°", v.az),
        };
        let pos = match self.layout {
            Layout::Follow => if english { "moving together, opposite each other" } else { "一起移动，彼此相对" }.to_string(),
            Layout::Mirror => if english { "mirrored, left and right" } else { "镜像，左右对称" }.to_string(),
            Layout::Fixed => format!("{} / {}", name(&self.voices[0]), name(&self.voices[1])),
        };
        if english {
            format!("Two voices: the second {:.0} ms later ({pos})", self.delay_ms)
        } else {
            format!("左右双人：第 2 个声音晚 {:.0} ms（{pos}）", self.delay_ms)
        }
    }
}

/// 默认输出文件名：输入名_asmr.wav。
pub fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "output".into());
    input.with_file_name(format!("{stem}_asmr.wav"))
}

/// 多音频合成的默认输出文件名：放在第一个音频旁边，名字是"第一个音频名_mix_asmr.wav"
/// （以 _asmr 结尾，添加文件夹时会被当作本程序的输出跳过）。
pub fn default_mix_output(first: &Path) -> PathBuf {
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "output".into());
    first.with_file_name(format!("{stem}_mix_asmr.wav"))
}

/// 多音频合成的选项（位置和音量在 [`MixInput`] 里）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MixOptions {
    /// 排布：各自固定、一起移动（按移动方式一起走、彼此错开 360°/N）或镜像（只用于两个音源，其余按一起移动）。
    pub layout: Layout,
    /// 合成前自动对齐（同一首歌的几个版本，从一起开始唱的地方叠加）。
    pub align: bool,
    /// 自动平衡的强度（0–1，0 = 关）：按短时响度让几段每一段时间都差不多一样响。
    pub balance: f32,
    /// 各音源比时间线晚多少个样本（左右双人：[0, 延迟, …]；多音频合成全是 0）。
    pub delays: [usize; MAX_VOICES],
}

/// 由各段的 10 ms 均方和整段响度算出自动平衡要用的输入响度数据（与强度无关）。合成试听和保存共用。
pub fn balance_data(levels: &[&[f32]], lufs: &[Option<f32>]) -> Arc<BalanceData> {
    Arc::new(BalanceData::new(levels, &mix::align_db(lufs)))
}

/// 多音频合成的一个音源：哪个文件、放在哪里、音量增减。
#[derive(Clone, Debug)]
pub struct MixInput {
    pub path: PathBuf,
    pub voice: Voice,
}

#[derive(Clone, Debug, Default)]
pub struct ConvertReport {
    pub seconds: f64,
    pub lufs: f32,
    pub true_peak_db: f32,
    pub max_gr_db: f32,
    pub channel_note: String,
    /// 多音频合成·自动对齐的说明：每段从哪里开始用、裁掉多少（没对齐时为空）。
    pub align_note: String,
    /// 多音频合成·自动平衡的说明：平衡前后两边的短时响度差（没平衡时为空）。
    pub balance_note: String,
    /// 左右双人的说明：第 2 个声音晚多少、两个声音在哪里（不是左右双人时为空）。
    pub duo_note: String,
    /// 背景音的说明（没开背景音时为空）。
    pub bg_note: String,
    pub n_moves: u32,
    pub n_sways: u32,
    /// 从头顶绕的次数、上下换位的次数。
    pub n_tops: u32,
    pub n_updowns: u32,
    /// 伸进耳道的次数（入耳深度大于 0 时）。
    pub n_strokes: u32,
    pub speech_pct: f32,
    pub elapsed_s: f64,
    /// 镜像排布：`n_moves` 数的是"靠向脸前 / 脑后"，不是换耳。
    pub mirrored: bool,
}

/// 单个文件转换完成后的说明（桌面版界面和安卓版文件转换页共用）。`out`：输出文件的位置。
pub fn report_text(rep: &ConvertReport, out: &str, english: bool) -> String {
    let t = |zh: &'static str, en: &'static str| if english { en } else { zh };
    format!(
        "{}：{}\n{:.1} s，{:.1} LUFS，{} {:.1} dBTP，{} {:.1} dB；{} {:.0}%，{} {}（{} {}），{} {}，{} {}{}；{} {:.1} s{}",
        t("已保存", "Saved"),
        out,
        rep.seconds,
        rep.lufs,
        t("真峰值", "true peak"),
        rep.true_peak_db,
        t("限制器最多压", "limiter max"),
        rep.max_gr_db,
        t("说话占", "speech"),
        rep.speech_pct,
        if rep.mirrored { t("靠向脸前 / 脑后", "lean-ins") } else { t("换耳", "switches") },
        rep.n_moves,
        t("从头顶绕", "over the head"),
        rep.n_tops,
        t("往后让", "lean-backs"),
        rep.n_sways,
        t("上下换位", "up / down moves"),
        rep.n_updowns,
        if rep.n_strokes > 0 { format!("，{} {}", t("伸进耳道", "strokes into the ear"), rep.n_strokes) } else { String::new() },
        t("用时", "took"),
        rep.elapsed_s,
        if rep.channel_note.is_empty() { String::new() } else { format!("\n{}", rep.channel_note) }
    ) + &if rep.duo_note.is_empty() { String::new() } else { format!("\n{}", rep.duo_note) }
        + &if rep.balance_note.is_empty() { String::new() } else { format!("\n{}", rep.balance_note) }
        + &if rep.bg_note.is_empty() { String::new() } else { format!("\n{}", rep.bg_note) }
}

/// 合成完成后的说明（同上）。`desc`：参与合成的音频和各自的位置。
pub fn mix_report_text(rep: &ConvertReport, out: &str, desc: &str, english: bool) -> String {
    let t = |zh: &'static str, en: &'static str| if english { en } else { zh };
    format!(
        "{}：{}\n{:.1} s，{:.1} LUFS，{} {:.1} dBTP，{} {:.1} dB；{} {:.1} s\n{}：{}{}",
        t("已保存", "Saved"),
        out,
        rep.seconds,
        rep.lufs,
        t("真峰值", "true peak"),
        rep.true_peak_db,
        t("限制器最多压", "limiter max"),
        rep.max_gr_db,
        t("用时", "took"),
        rep.elapsed_s,
        t("合成", "Mixed"),
        desc,
        if rep.channel_note.is_empty() { String::new() } else { format!("\n{}", rep.channel_note) }
    ) + &if rep.align_note.is_empty() { String::new() } else { format!("\n{}", rep.align_note) }
        + &if rep.balance_note.is_empty() { String::new() } else { format!("\n{}", rep.balance_note) }
        + &if rep.bg_note.is_empty() { String::new() } else { format!("\n{}", rep.bg_note) }
}

/// 逐包解码，把每个包交给 `sink(通道数, 采样率, 交织样本)`；返回 false 时提前停止。
fn decode(path: &Path, mut sink: impl FnMut(usize, u32, &[f32]) -> bool) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("打不开 {}：{e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("不认识的音频格式：{e}"))?;
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or("文件里没有音轨")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| format!("不支持的编码：{e}"))?;
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("读取失败：{e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                let need = buf.capacity() as u64;
                if sbuf.as_ref().map_or(true, |s| (s.capacity() as u64) < need * spec.channels.count() as u64) {
                    sbuf = Some(SampleBuffer::<f32>::new(need, spec));
                }
                let sb = sbuf.as_mut().unwrap();
                sb.copy_interleaved_ref(buf);
                if !sink(spec.channels.count(), spec.rate, sb.samples()) {
                    break;
                }
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("解码失败：{e}")),
        }
    }
    Ok(())
}

/// 读入音频并取成 48 kHz 单声道。返回 (样本, 关于声道选择的说明)。
pub fn load_mono(opts: &ConvertOptions, cancel: &AtomicBool) -> Result<(Vec<f32>, String), String> {
    // 第一遍：只统计左右声道的能量和相关性，决定取哪个声道
    let mut note = String::new();
    let mut pick = opts.channel;
    let (mut ll, mut rr, mut lr, mut nch) = (0f64, 0f64, 0f64, 0usize);
    if opts.channel == Channel::Auto {
        // 只统计要处理的那一段（与 Python 版一致：截取之后再比较两个声道）
        let mut seen = 0u64;
        decode(&opts.input, |ch, sr, s| {
            nch = ch;
            if ch < 2 {
                return false;
            }
            let i0 = (opts.start * sr as f64) as u64;
            let i1 = opts.duration.map(|d| i0 + (d * sr as f64) as u64);
            for f in s.chunks_exact(ch) {
                if seen >= i0 && i1.map_or(true, |e| seen < e) {
                    ll += (f[0] as f64) * (f[0] as f64);
                    rr += (f[1] as f64) * (f[1] as f64);
                    lr += (f[0] as f64) * (f[1] as f64);
                }
                seen += 1;
            }
            !cancel.load(Ordering::Relaxed) && i1.map_or(true, |e| seen < e)
        })?;
        if nch >= 2 {
            let corr = lr / ((ll * rr).sqrt() + 1e-30);
            let left = ll >= rr;
            pick = if left { Channel::Left } else { Channel::Right };
            note = format!("立体声输入，相关 {:.2}，取{}声道", corr, if left { "左" } else { "右" });
            let (big, small) = if left { (ll, rr) } else { (rr, ll) };
            if small > 0.25 * big && corr < 0.5 {
                note.push_str("（两个声道内容差别较大，只取一个会丢掉另一侧的内容，可改选「混合」）");
            }
        }
    }
    let mut mono: Vec<f32> = Vec::new();
    let mut rs: Option<Resampler> = None;
    let mut rate = 0u32;
    let mut seen = 0u64;
    let mut tmp: Vec<f32> = Vec::new();
    decode(&opts.input, |ch, sr, s| {
        if rate == 0 {
            rate = sr;
            if sr as usize != FS_USIZE {
                rs = Some(Resampler::new(sr as f64, FS as f64));
            }
        }
        let i0 = (opts.start * sr as f64) as u64;
        let i1 = opts.duration.map(|d| i0 + (d * sr as f64) as u64 + 64);
        tmp.clear();
        for f in s.chunks_exact(ch.max(1)) {
            let keep = seen >= i0 && i1.map_or(true, |e| seen < e);
            seen += 1;
            if !keep {
                continue;
            }
            let v = if ch < 2 {
                f[0]
            } else {
                match pick {
                    Channel::Right => f[1],
                    Channel::Mix => 0.5 * (f[0] + f[1]),
                    _ => f[0],
                }
            };
            tmp.push(v);
        }
        match &mut rs {
            Some(r) => r.process(&tmp, &mut mono),
            None => mono.extend_from_slice(&tmp),
        }
        !cancel.load(Ordering::Relaxed) && i1.map_or(true, |e| seen < e)
    })?;
    if let Some(d) = opts.duration {
        mono.truncate((d * FS as f64) as usize);
    }
    if mono.len() < FS_USIZE / 10 {
        return Err("输入太短".into());
    }
    Ok((mono, note))
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 第一遍每次处理多少样本。
const CH: usize = 4096;

/// 第一遍的产物：临时文件里是去掉开头时延之后的交错 f32 立体声，外加测好的积分响度和采样峰值。
struct Rendered {
    tmp: TempFile,
    written: usize,
    peak: f32,
    meter: LoudnessMeter,
}

/// 第一遍：从第 0 个样本起逐块调用 `render(起点, 左, 右)`，一共渲染 `total` 个样本；去掉开头 `skip` 个
/// （预读和降噪带来的时延）后写进临时文件，同时测响度和峰值。
fn render_to_temp(total: usize, skip: usize, cancel: &AtomicBool, progress: &mut impl FnMut(f32, &str), mut render: impl FnMut(usize, &mut [f32], &mut [f32])) -> Result<Rendered, String> {
    // 批量转换时多个线程同时转换：用进程内递增的序号保证临时文件名不重复
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp_path = std::env::temp_dir().join(format!("auto_asmr_{}_{}.f32", std::process::id(), TMP_SEQ.fetch_add(1, Ordering::Relaxed)));
    let tmp = TempFile(tmp_path.clone());
    let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp_path).map_err(|e| format!("无法创建临时文件：{e}"))?);
    let mut meter = LoudnessMeter::new();
    let mut peak = 0f32;
    let (mut ol, mut or) = (vec![0f32; CH], vec![0f32; CH]);
    let mut done = 0usize;
    let mut written = 0usize;
    let mut bytes = Vec::with_capacity(CH * 8);
    while done < total {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        let m = CH.min(total - done);
        render(done, &mut ol[..m], &mut or[..m]);
        bytes.clear();
        for i in 0..m {
            if done + i < skip {
                continue;
            }
            let (l, r) = (ol[i], or[i]);
            meter.push(l, r);
            peak = peak.max(l.abs()).max(r.abs());
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&r.to_le_bytes());
            written += 1;
        }
        w.write_all(&bytes).map_err(|e| format!("写临时文件失败：{e}"))?;
        done += m;
        progress(0.05 + 0.75 * done as f32 / total as f32, "空间化");
    }
    w.flush().map_err(|e| e.to_string())?;
    drop(w);
    Ok(Rendered { tmp, written, peak, meter })
}

/// 第二遍的结果。
struct Finished {
    seconds: f64,
    lufs: f32,
    true_peak_db: f32,
    max_gr_db: f32,
}

/// 去掉尾部多余的静音，第二遍精确响度归一化 + 真峰值限制，写出 WAV。`n_sig`：输入（最长的那个）的样本数。
/// 第二遍。`cfg`：背景音（雨声）在这里加——人声归一化之后、限制器之前，比人声低"音量"那么多，开头 1 秒淡入、结尾 2 秒淡出。
fn finish(opts: &ConvertOptions, r: Rendered, n_sig: usize, cfg: &EngineConfig, cancel: &AtomicBool, progress: &mut impl FnMut(f32, &str)) -> Result<Finished, String> {
    let Rendered { tmp, written, peak, meter } = r;
    let cancelled = || cancel.load(Ordering::Relaxed);
    // 去掉尾部多余的静音：保留到最后一个高于最大值 −100 dB 的样本
    let mut rd = BufReader::with_capacity(1 << 20, File::open(&tmp.0).map_err(|e| e.to_string())?);
    let mut n_out = n_sig.min(written);
    if written > n_sig {
        rd.seek(SeekFrom::Start((n_sig * 8) as u64)).map_err(|e| e.to_string())?;
        let mut b = vec![0u8; (written - n_sig) * 8];
        rd.read_exact(&mut b).map_err(|e| e.to_string())?;
        for (i, f) in b.chunks_exact(4).enumerate() {
            if f32::from_le_bytes(f.try_into().unwrap()).abs() > 1e-5 * peak {
                n_out = n_sig + i / 2 + 1;
            }
        }
        rd.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    }

    // 第二遍：响度归一化；峰值太高时用前瞻限制器最多压 max_gr dB，再不够就整体降低
    let g0 = db_to_lin(meter.gain_db_for(opts.lufs));
    let mut gain = g0;
    let lim = db_to_lin(opts.peak_db);
    if peak * gain > lim {
        let need = 20.0 * (peak * gain / lim).log10();
        if need > opts.max_gr_db {
            gain *= db_to_lin(-(need - opts.max_gr_db));
        }
    }
    // 背景音按人声实际的响度定（峰值太高、人声整体降低了时跟着降）
    let rain_ref = opts.lufs + 20.0 * (gain / g0).max(1e-9).log10();
    let mut rain = cfg.derived.rain.as_ref().map(|s| {
        let mut r = Rain::new(s.clone(), cfg.params.seed, cfg.derived.playback.as_ref());
        r.prime_gain(rain_ref);
        r
    });
    let (mut rain_l, mut rain_r) = if rain.is_some() { (vec![0f32; CH], vec![0f32; CH]) } else { (Vec::new(), Vec::new()) };
    let fade_in = (FS_USIZE).min(n_out / 4).max(1);
    let fade_out = (2 * FS_USIZE).min(n_out / 3).max(1);
    let fade = |i: usize| -> f32 {
        let a = if i < fade_in { 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / fade_in as f32).cos() } else { 1.0 };
        let left = n_out.saturating_sub(i);
        let b = if left < fade_out { 0.5 - 0.5 * (std::f32::consts::PI * left as f32 / fade_out as f32).cos() } else { 1.0 };
        a * b
    };
    let spec = hound::WavSpec { channels: 2, sample_rate: FS_USIZE as u32, bits_per_sample: opts.bits, sample_format: hound::SampleFormat::Int };
    let mut wav = hound::WavWriter::create(&opts.output, spec).map_err(|e| format!("无法写入 {}：{e}", opts.output.display()))?;
    let mut limiter = Limiter::new(6.0, 40.0);
    let ldelay = limiter.delay();
    let mut tp = TruePeak::new();
    let mut out_meter = LoudnessMeter::new();
    let full = if opts.bits == 16 { 32767.0 } else { 8_388_607.0 };
    let mut buf = vec![0u8; CH * 8];
    let mut fed = 0usize;
    let mut emitted = 0usize;
    let ceiling = lim * 0.97;
    let total2 = n_out + ldelay; // 末尾多送一段 0，把限制器里延时的样本推出来
    let pass2 = (|| -> Result<(), String> {
        while fed < total2 {
            if cancelled() {
                return Err("已取消".into());
            }
            let m = CH.min(total2 - fed);
            let avail = m.min(n_out.saturating_sub(fed));
            if avail > 0 {
                rd.read_exact(&mut buf[..avail * 8]).map_err(|e| e.to_string())?;
            }
            if let Some(r) = &mut rain {
                if avail > 0 {
                    r.render(rain_ref, &mut rain_l[..avail], &mut rain_r[..avail]);
                    for i in 0..avail {
                        let f = fade(fed + i);
                        rain_l[i] *= f;
                        rain_r[i] *= f;
                    }
                }
            }
            for i in 0..m {
                let (l, r) = if i < avail {
                    let o = i * 8;
                    let (l, r) = (f32::from_le_bytes(buf[o..o + 4].try_into().unwrap()) * gain, f32::from_le_bytes(buf[o + 4..o + 8].try_into().unwrap()) * gain);
                    if rain.is_some() {
                        (l + rain_l[i], r + rain_r[i])
                    } else {
                        (l, r)
                    }
                } else {
                    (0.0, 0.0)
                };
                let (a, b) = limiter.process(l, r, ceiling);
                if fed + i >= ldelay {
                    tp.push(a, b);
                    out_meter.push(a, b);
                    wav.write_sample((a.clamp(-1.0, 1.0) * full).round() as i32).map_err(|e| e.to_string())?;
                    wav.write_sample((b.clamp(-1.0, 1.0) * full).round() as i32).map_err(|e| e.to_string())?;
                    emitted += 1;
                }
            }
            fed += m;
            progress(0.8 + 0.2 * emitted as f32 / n_out.max(1) as f32, "响度归一化");
        }
        wav.finalize().map_err(|e| e.to_string())
    })();
    if let Err(e) = pass2 {
        // 取消或出错：不留下写了一半的输出文件
        let _ = std::fs::remove_file(&opts.output);
        return Err(e);
    }
    drop(tmp);
    progress(1.0, "完成");
    Ok(Finished { seconds: n_out as f64 / FS as f64, lufs: out_meter.integrated(), true_peak_db: 20.0 * tp.peak.max(1e-9).log10(), max_gr_db: limiter.max_gr_db })
}

/// 转换一个文件。`progress(0–1, 阶段说明)`；`cancel` 置位时中止。
pub fn convert(opts: &ConvertOptions, params: &Params, cancel: Arc<AtomicBool>, mut progress: impl FnMut(f32, &str)) -> Result<ConvertReport, String> {
    if let Some(d) = opts.duo.filter(|d| d.delay_ms > 0.0) {
        return convert_duo(opts, d, params, cancel, progress);
    }
    let t_start = std::time::Instant::now();
    crate::dsp::flush_denormals();
    progress(0.0, "读取音频");
    let (mono, note) = load_mono(opts, &cancel)?;
    let n_sig = mono.len();
    if cancel.load(Ordering::Relaxed) {
        return Err("已取消".into());
    }

    // 文件模式：响度和限幅放到第二遍，引擎里不做自动响度
    let cfg = Arc::new(EngineConfig::new(params.clone(), None));
    let eng_cfg = cfg.clone();
    let lookahead = 1.0;
    let mut eng = Engine::new(cfg, PostMode::Offline, lookahead);
    if let Some((thr, med)) = prescan(&mono) {
        eng.set_fixed_analysis(thr, med);
    }
    if opts.save_trajectory.is_some() {
        eng.trace = Some(Vec::new());
    }
    let skip = eng.latency_samples(); // 预读（和降噪）带来的时延，输出时去掉
    let tail = if params.room { (0.65 * FS) as usize } else { 512 };
    let total = n_sig + skip + tail;
    let mut inp = vec![0f32; CH];
    let rendered = render_to_temp(total, skip, &cancel, &mut progress, |done, ol, or| {
        let m = ol.len();
        for (i, v) in inp[..m].iter_mut().enumerate() {
            *v = if done + i < n_sig { mono[done + i] } else { 0.0 };
        }
        eng.process(&inp[..m], ol, or);
    })?;
    if let (Some(path), Some(tr)) = (&opts.save_trajectory, &eng.trace) {
        let mut f = BufWriter::new(File::create(path).map_err(|e| format!("无法写入轨迹文件：{e}"))?);
        writeln!(f, "t_sec,az_deg,dist_m,base_dist_m,speech,el_deg,inear_depth_m").map_err(|e| e.to_string())?;
        for row in tr.iter().step_by(8) {
            writeln!(f, "{:.4},{:.2},{:.5},{:.5},{:.3},{:.2},{:.5}", row[0], row[1], row[2], row[3], row[4], row[5], row[6]).map_err(|e| e.to_string())?;
        }
    }
    let fin = finish(opts, rendered, n_sig, &eng_cfg, &cancel, &mut progress)?;
    Ok(ConvertReport {
        seconds: fin.seconds,
        lufs: fin.lufs,
        true_peak_db: fin.true_peak_db,
        max_gr_db: fin.max_gr_db,
        bg_note: rain::describe(params, false),
        channel_note: note,
        align_note: String::new(),
        balance_note: String::new(),
        duo_note: String::new(),
        n_moves: eng.tele.n_moves,
        n_sways: eng.tele.n_sways,
        n_tops: eng.tele.n_tops,
        n_updowns: eng.tele.n_updowns,
        n_strokes: eng.tele.n_strokes,
        speech_pct: 100.0 * eng.n_speech as f32 / eng.n_blocks.max(1) as f32,
        elapsed_s: t_start.elapsed().as_secs_f64(),
        mirrored: false,
    })
}

/// 左右双人：同一个文件当成两个声音（`d.voices`），第 2 个晚 `d.delay_ms`，按多音频合成做（文件只解码一次）。
/// 输出比原音频长出这段延迟（第 2 个声音说完为止）；起点和时长按第 1 个声音（原音频）的时间算。
fn convert_duo(opts: &ConvertOptions, d: Duo, params: &Params, cancel: Arc<AtomicBool>, progress: impl FnMut(f32, &str)) -> Result<ConvertReport, String> {
    if opts.save_trajectory.is_some() {
        return Err("左右双人不支持导出走位".into());
    }
    let mut o = opts.clone();
    o.duo = None;
    let inputs = [MixInput { path: opts.input.clone(), voice: d.voices[0] }, MixInput { path: opts.input.clone(), voice: d.voices[1] }];
    let mut delays = [0; MAX_VOICES];
    delays[1] = mix::delay_samples(d.delay_ms);
    let mo = MixOptions { layout: d.layout, align: false, balance: d.balance, delays };
    let mut rep = convert_mix(&o, &inputs, params, mo, cancel, progress)?;
    rep.duo_note = d.describe(false);
    Ok(rep)
}

/// 自动对齐几段整段解码好的单声道音频（见 [`crate::dsp::align`]）：返回对齐、裁剪（裁开处淡入淡出）之后的各段和对齐结果。
/// 合成试听和"合成并保存"都用它，所以两边听到的是同一份音频。
/// 有一段和第 1 段对不上（不像同一首歌）时不裁剪，返回错误说明——两边都按没对齐的原样合成，免得错裁掉一大段。
pub fn align_tracks(monos: &[&[f32]], names: &[String]) -> Result<(Vec<Vec<f32>>, Alignment), String> {
    let al = align::align(monos)?;
    if let Some(k) = al.pairs.iter().position(|p| !p.reliable) {
        let name = |i: usize| names.get(i).cloned().unwrap_or_else(|| format!("第 {} 段", i + 1));
        return Err(format!("「{}」和「{}」没有明显对得上的地方（可能不是同一首歌），没有对齐、没有裁剪", name(0), name(k + 1)));
    }
    let out = monos.iter().enumerate().map(|(k, x)| al.apply(k, x)).collect();
    Ok((out, al))
}

/// 多音频合成：`inputs` 里的 2–[`MAX_VOICES`] 个文件各放一个位置（见 [`crate::dsp::mix`]），同时播放，合成一个
/// 双耳 WAV 写到 `opts.output`。`opts.input` 不用；起点、时长、取哪个声道、位深、目标响度、峰值上限对每个音源都一样。
/// 输出的长度按最长的那个音源；各音源先按响度对齐，再加各自的音量增减，相加之后与单个文件一样两遍处理。
/// `mo.layout`：一起移动（按 `params` 的移动方式一起走、彼此错开 360°/N）、镜像（两个音源左右对称）；
/// 各自固定时在 `inputs` 给的位置。
/// `mo.align`：先自动对齐（整段解码后对齐、裁掉对不上的开头和结尾），起点和时长按对齐之后的时间算；
/// 对齐的说明（每段从哪里开始用、裁掉多少）放在报告的 `align_note` 里。
/// `mo.balance`：自动平衡（整段算增益曲线，再截起点和时长），说明放在 `balance_note` 里。
/// `mo.delays`：各音源比时间线晚多少个样本（左右双人），截取各段时直接错开，时间线随之变长。
/// 对齐、平衡或有延迟时整段解码（同一个文件只解码一次）、按整段测响度，和合成试听完全一样；
/// 都没有时和以前一样只解码要处理的那一段。
pub fn convert_mix(opts: &ConvertOptions, inputs: &[MixInput], params: &Params, mo: MixOptions, cancel: Arc<AtomicBool>, mut progress: impl FnMut(f32, &str)) -> Result<ConvertReport, String> {
    let (follow, align) = (mo.layout.moves(), mo.align);
    let n = inputs.len();
    let delays = &mo.delays[..n.min(MAX_VOICES)];
    let full = mo.align || mo.balance > 0.0 || delays.iter().any(|&d| d > 0);
    if n < 2 {
        return Err("多音频合成至少需要两个音频".into());
    }
    if n > MAX_VOICES {
        return Err(format!("多音频合成最多 {MAX_VOICES} 个音频"));
    }
    let out = crate::batch::normalize(&opts.output);
    if let Some(i) = inputs.iter().find(|i| crate::batch::normalize(&i.path) == out) {
        return Err(format!("输出文件不能是输入文件：{}", i.path.display()));
    }
    let t_start = std::time::Instant::now();
    crate::dsp::flush_denormals();
    let mut monos = Vec::with_capacity(n);
    let mut notes = Vec::new();
    // 整段处理时同一个文件只解码一次（左右双人）：src[k] = 第 k 个音源用的是第几段解码结果
    let mut src: Vec<usize> = (0..n).map(|k| if full { inputs[..k].iter().position(|i| i.path == inputs[k].path).unwrap_or(k) } else { k }).collect();
    let unique = src.iter().enumerate().filter(|&(k, &j)| k == j).count();
    for (k, inp) in inputs.iter().enumerate() {
        if src[k] != k {
            monos.push(Vec::new());
            continue;
        }
        progress(0.05 * k as f32 / n as f32, "读取音频");
        let mut o = opts.clone();
        o.input = inp.path.clone();
        if full {
            // 对齐、平衡要看整段（和合成试听解码的一样），起点和时长在之后再截
            o.start = 0.0;
            o.duration = None;
        }
        let name = inp.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (m, note) = load_mono(&o, &cancel).map_err(|e| format!("{name}：{e}"))?;
        if !note.is_empty() {
            notes.push(if unique > 1 { format!("{}. {name}：{note}", k + 1) } else { format!("{name}：{note}") });
        }
        monos.push(m);
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
    }
    let (mut align_note, mut balance_note) = (String::new(), String::new());
    let lufs: Vec<Option<f32>>;
    let mut bal: Option<Arc<BalanceData>> = None;
    let mut s0 = 0usize;
    if full {
        let names: Vec<String> = inputs.iter().map(|i| i.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        if align {
            progress(0.05, "自动对齐");
            let refs: Vec<&[f32]> = (0..n).map(|k| monos[src[k]].as_slice()).collect();
            match align_tracks(&refs, &names) {
                Ok((aligned, al)) => {
                    align_note = al.describe(&names, false);
                    monos = aligned;
                    src = (0..n).collect();
                }
                // 对不上：和合成试听一样，按没对齐的原样合成
                Err(e) => align_note = format!("自动对齐：{e}"),
            }
        }
        // 响度和平衡曲线按整段算（和合成试听一样），之后再截起点和时长；同一段只算一次
        let per: Vec<Option<f32>> = (0..n).map(|k| if src[k] == k { mix::loudness(&monos[k]) } else { None }).collect();
        lufs = (0..n).map(|k| per[src[k]]).collect();
        if mo.balance > 0.0 {
            progress(0.05, "自动平衡");
            let lv: Vec<Vec<f32>> = (0..n).map(|k| if src[k] == k { balance::levels(&monos[k]) } else { Vec::new() }).collect();
            let refs: Vec<&[f32]> = (0..n).map(|k| lv[src[k]].as_slice()).collect();
            bal = Some(balance_data(&refs, &lufs));
        }
        // 起点和时长按对齐之后的时间算（没对齐时就是原来的时间）；有延迟的音源整体往后挪，时间线随之变长
        let lens: Vec<usize> = (0..n).map(|k| monos[src[k]].len()).collect();
        let len = mix::timeline_len(&lens, delays);
        s0 = (opts.start.max(0.0) * FS as f64) as usize;
        if s0 >= len {
            return Err(format!("起点 {:.1} s 超出了长度 {:.1} s", opts.start, len as f64 / FS as f64));
        }
        let s1 = opts.duration.map_or(len, |d| (s0 + (d * FS as f64) as usize).min(len));
        monos = (0..n).map(|k| mix::delayed(&monos[src[k]], delays.get(k).copied().unwrap_or(0), s0, s1)).collect();
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
    } else {
        lufs = monos.iter().map(|m| mix::loudness(m)).collect();
    }
    let voices: Vec<Voice> = inputs.iter().map(|i| i.voice).collect();
    let base = Arc::new(EngineConfig::new(params.clone(), None));
    let setup = MixSetup::new(&base, &voices, &lufs, mo.layout, None).with_balance(bal, mo.balance).with_delays(delays);
    let mut mixer = Mixer::new(&setup, PostMode::Offline, 1.0);
    mixer.seek(s0); // 平衡曲线按整段的位置取
    for (k, m) in monos.iter().enumerate() {
        if let Some((thr, med)) = prescan(m) {
            mixer.engine_mut(k).set_fixed_analysis(thr, med);
        }
    }
    let n_sig = monos.iter().map(|m| m.len()).max().unwrap_or(0);
    let skip = mixer.latency_samples();
    let tail = if params.room { (0.65 * FS) as usize } else { 512 };
    let total = n_sig + skip + tail;
    let mut bufs = vec![vec![0f32; CH]; n];
    let rendered = render_to_temp(total, skip, &cancel, &mut progress, |done, ol, or| {
        let m = ol.len();
        for (b, x) in bufs.iter_mut().zip(&monos) {
            for (i, v) in b[..m].iter_mut().enumerate() {
                *v = x.get(done + i).copied().unwrap_or(0.0);
            }
        }
        let ins: Vec<&[f32]> = bufs.iter().map(|b| &b[..m]).collect();
        mixer.process_parallel(&ins, ol, or);
    })?;
    let fin = finish(opts, rendered, n_sig, &base, &cancel, &mut progress)?;
    if mo.balance > 0.0 {
        balance_note = balance::describe(mo.balance, mixer.balance_stats(), false);
    }
    let speech = (0..n).map(|k| 100.0 * mixer.engine(k).n_speech as f32 / mixer.engine(k).n_blocks.max(1) as f32).sum::<f32>() / n as f32;
    // 各自固定：各音源的往后让、上下换位次数加起来；一起移动、镜像：大家走同一套，报告一份
    let t = if follow { mixer.engine(0).tele } else { mixer.tele };
    Ok(ConvertReport {
        seconds: fin.seconds,
        lufs: fin.lufs,
        true_peak_db: fin.true_peak_db,
        max_gr_db: fin.max_gr_db,
        channel_note: notes.join("\n"),
        align_note,
        balance_note,
        duo_note: String::new(),
        bg_note: rain::describe(params, false),
        n_moves: t.n_moves,
        n_sways: t.n_sways,
        n_tops: t.n_tops,
        n_updowns: t.n_updowns,
        n_strokes: t.n_strokes,
        speech_pct: speech,
        elapsed_s: t_start.elapsed().as_secs_f64(),
        mirrored: setup.mirror,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testsig::test_voice;

    #[test]
    fn aligned_mix_trims_intro_and_preview_uses_the_same_audio() {
        use crate::dsp::testsig::test_song;
        let dir = std::env::temp_dir().join(format!("auto_asmr_align_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A：6 秒前奏 + 歌；B：同一首歌（没有前奏）+ 2 秒别的结尾
        let song = test_song(20.0, 21);
        let a: Vec<f32> = test_song(6.0, 22).into_iter().chain(song.iter().copied()).collect();
        let b: Vec<f32> = song.iter().copied().chain(test_song(2.0, 23)).collect();
        let (pa, pb) = (dir.join("a.wav"), dir.join("b.wav"));
        write_wav(&pa, &a);
        write_wav(&pb, &b);
        let inputs = [MixInput { path: pa.clone(), voice: Voice::preset(0) }, MixInput { path: pb.clone(), voice: Voice::preset(1) }];
        let mut opts = ConvertOptions::new(pa.clone());
        opts.output = dir.join("mix.wav");
        let run = |o: &ConvertOptions| convert_mix(o, &inputs, &Params::default(), MixOptions { align: true, ..Default::default() }, Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
        // 长度 = 共同部分（20 秒）；说明里写出 A 从 6 秒开始、B 的结尾裁掉 2 秒
        let rep = run(&opts);
        assert!((rep.seconds - 20.0).abs() < 0.02, "{}", rep.seconds);
        assert!(rep.align_note.contains("a.wav：从 0:06.00 开始（裁掉开头 6.00 s）"), "{}", rep.align_note);
        assert!(rep.align_note.contains("b.wav：开头不裁，用到 0:20.00（裁掉结尾 2.00 s）"), "{}", rep.align_note);
        // 起点和时长按对齐之后的时间算
        let mut part = opts.clone();
        part.start = 2.0;
        part.duration = Some(5.0);
        assert!((run(&part).seconds - 5.0).abs() < 0.02);
        // 试听：整段解码（和界面的 decode_mix 一样）再用同一个函数对齐，结果与合成时用的一致
        let ma = load_mono(&ConvertOptions::new(pa.clone()), &AtomicBool::new(false)).unwrap().0;
        let mb = load_mono(&ConvertOptions::new(pb.clone()), &AtomicBool::new(false)).unwrap().0;
        let (tr, al) = align_tracks(&[&ma, &mb], &["a.wav".into(), "b.wav".into()]).unwrap();
        assert_eq!(al.starts, vec![6 * FS_USIZE, 0]);
        assert_eq!((tr[0].len(), tr[1].len()), (20 * FS_USIZE, 20 * FS_USIZE));
        assert_eq!(al.describe(&["a.wav".into(), "b.wav".into()], false), rep.align_note);
        // 对不上的两段：不裁剪，按原样合成（长度按最长的），说明里写出原因
        let pc = dir.join("c.wav");
        write_wav(&pc, &test_song(15.0, 99));
        let other = [inputs[0].clone(), MixInput { path: pc.clone(), voice: Voice::preset(1) }];
        let r2 = convert_mix(&opts, &other, &Params::default(), MixOptions { align: true, ..Default::default() }, Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
        assert!((r2.seconds - 26.0).abs() < 0.02 && r2.align_note.contains("没有对齐"), "{} {}", r2.seconds, r2.align_note);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn balanced_mix_keeps_left_and_right_about_equal() {
        let dir = std::env::temp_dir().join(format!("auto_asmr_balance_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A（左耳）一直正常；B（右耳）同样在说，但后一半轻 12 dB：整段对齐只能各补一半，每一半里还是一边响一边轻
        let v = test_voice(24.0);
        let half = v.len() / 2;
        let b: Vec<f32> = v.iter().enumerate().map(|(i, &x)| if i < half { x } else { 0.25 * x }).collect();
        let (pa, pb) = (dir.join("a.wav"), dir.join("b.wav"));
        write_wav(&pa, &v);
        write_wav(&pb, &b);
        let inputs = [MixInput { path: pa.clone(), voice: Voice::preset(0) }, MixInput { path: pb.clone(), voice: Voice::preset(1) }];
        let gap = |strength: f32| {
            let mut opts = ConvertOptions::new(pa.clone());
            opts.output = dir.join(format!("mix_{strength}.wav"));
            let rep = convert_mix(&opts, &inputs, &Params::default(), MixOptions { balance: strength, ..Default::default() }, Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
            let (l, r) = read_stereo(&opts.output);
            // 每 2 秒左右两边的响度差（跳过开头 2 秒）
            let s = 2 * FS_USIZE;
            let e = |x: &[f32], i: usize| 10.0 * (x[i * s..(i + 1) * s].iter().map(|v| (v * v) as f64).sum::<f64>() + 1e-12).log10();
            let mut d: Vec<f64> = (1..l.len() / s).map(|i| (e(&l, i) - e(&r, i)).abs()).collect();
            d.sort_by(f64::total_cmp);
            (d[d.len() / 2], rep)
        };
        let (off, r0) = gap(0.0);
        let (on, r1) = gap(1.0);
        assert!(off > 4.0, "不平衡时左右中位差 {off:.1} dB");
        assert!(on < 2.0, "平衡后左右中位差 {on:.1} dB");
        assert!(r0.balance_note.is_empty() && r1.balance_note.starts_with("自动平衡 100%"), "{}", r1.balance_note);
        assert!((r1.lufs - r0.lufs).abs() < 0.5 && r1.seconds == r0.seconds);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 左右两路的 1 ms 对数包络，算右路比左路晚多少毫秒（互相关峰，±200 ms 内）。
    fn envelope_lag_ms(l: &[f32], r: &[f32]) -> i64 {
        let env = |x: &[f32]| -> Vec<f64> {
            let mut e = 0f64;
            let a = (-1.0 / (0.004 * FS as f64)).exp();
            let v: Vec<f64> = x.iter().map(|&s| { e = a * e + (1.0 - a) * (s * s) as f64; (e + 1e-9).ln() }).step_by(48).collect();
            let m = v.iter().sum::<f64>() / v.len() as f64;
            v.into_iter().map(|x| x - m).collect()
        };
        let (a, b) = (env(l), env(r));
        let corr = |lag: i64| {
            let n = a.len() as i64 - lag.abs();
            let (i0, j0) = if lag >= 0 { (0, lag) } else { (-lag, 0) };
            (0..n).map(|i| a[(i0 + i) as usize] * b[(j0 + i) as usize]).sum::<f64>() / n as f64
        };
        (-200..=200).max_by(|&x, &y| corr(x).total_cmp(&corr(y))).unwrap()
    }

    #[test]
    fn duo_puts_the_same_voice_on_both_sides_one_slightly_later() {
        let dir = std::env::temp_dir().join(format!("auto_asmr_duo_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = test_voice(12.0);
        let pa = dir.join("a.wav");
        write_wav(&pa, &v);
        let run = |start: f64, duration: Option<f64>, ms: f32| {
            let mut opts = ConvertOptions::new(pa.clone());
            opts.output = dir.join(format!("duo_{start}_{ms}.wav"));
            opts.start = start;
            opts.duration = duration;
            opts.duo = Some(Duo::new(ms));
            let rep = convert(&opts, &Params::default(), Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
            let (l, r) = read_stereo(&opts.output);
            (rep, l, r)
        };
        // 整段：输出比原音频长出这段延迟；右耳（第 2 个声音）比左耳正好晚 100 ms；两边差不多一样响
        let (rep, l, r) = run(0.0, None, 100.0);
        // （输出末尾和普通转换一样保留最多 512 个样本的滤波器余尾）
        assert!(l.len() >= v.len() + 4800 && l.len() <= v.len() + 4800 + 512, "{} 样本，原音频 {}", l.len(), v.len());
        assert_eq!(rep.duo_note, "左右双人：第 2 个声音晚 100 ms（左耳 / 右耳）");
        let lag = envelope_lag_ms(&l, &r);
        assert!((lag - 100).abs() <= 2, "右耳比左耳晚 {lag} ms");
        let db = |x: &[f32]| 10.0 * x.iter().map(|v| (v * v) as f64).sum::<f64>().log10();
        assert!((db(&l) - db(&r)).abs() < 3.0, "左 {:.1} dB，右 {:.1} dB", db(&l), db(&r));
        // 换一个延迟，错开的时间跟着变
        let (_, l, r) = run(0.0, None, 50.0);
        assert!((envelope_lag_ms(&l, &r) - 50).abs() <= 2);
        // 只处理一段：长度按要求；第 2 个声音从原音频起点之前 100 ms 开始（和试听拖到这里时听到的一样），不是从静音开始
        let (rep, l, r) = run(3.0, Some(5.0), 100.0);
        assert!(rep.seconds >= 5.0 && rep.seconds < 5.011, "{}", rep.seconds);
        assert!((envelope_lag_ms(&l, &r) - 100).abs() <= 2);
        let lead = |x: &[f32]| x[..2400].iter().map(|v| (v * v) as f64).sum::<f64>();
        assert!(lead(&r) > 1e-6 * lead(&l).max(1e-9), "右耳开头是静音");
        // 关掉（0 ms）就是普通转换
        let mut plain = ConvertOptions::new(pa.clone());
        plain.output = dir.join("plain.wav");
        plain.duration = Some(4.0);
        let mut zero = plain.clone();
        zero.output = dir.join("zero.wav");
        zero.duo = Some(Duo::new(0.0));
        let a = convert(&plain, &Params::default(), Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
        let b = convert(&zero, &Params::default(), Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
        assert!(b.duo_note.is_empty() && a.seconds == b.seconds);
        assert_eq!(std::fs::read(&plain.output).unwrap(), std::fs::read(&zero.output).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn background_rain_fills_the_pauses_and_off_changes_nothing() {
        let dir = std::env::temp_dir().join(format!("auto_asmr_rain_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 第 1–4 秒说话，前 1 秒和后 4 秒静音
        let v = test_voice(8.0);
        let x: Vec<f32> = v.iter().enumerate().map(|(i, &s)| if (FS_USIZE..4 * FS_USIZE).contains(&i) { s } else { 0.0 }).collect();
        let pa = dir.join("a.wav");
        write_wav(&pa, &x);
        let run = |p: &Params, name: &str| {
            let mut opts = ConvertOptions::new(pa.clone());
            opts.output = dir.join(name);
            let rep = convert(&opts, p, Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
            (rep, read_stereo(&opts.output), opts.output)
        };
        // 音量为 0：其它背景音设置怎么改，输出都逐字节相同，报告里没有背景音
        let (r0, _, o0) = run(&Params::default(), "plain.wav");
        let (r1, _, o1) = run(&Params { rain: 90.0, bg_indoor: 0.0, bg_pan: -100.0, rain_near: 100.0, ..Params::default() }, "off.wav");
        assert_eq!(std::fs::read(&o0).unwrap(), std::fs::read(&o1).unwrap());
        assert!(r0.bg_note.is_empty() && r1.bg_note.is_empty());
        // 打开雨夜：静音的那几秒里有雨声（比人声低约 18 dB 上下），开头是淡入的
        let night = Params::default().bg_rain_night();
        let (rep, (l, r), _) = run(&night, "night.wav");
        assert!(rep.bg_note.starts_with("背景音：雨声，比人声低 18 dB"), "{}", rep.bg_note);
        let (_, (l0, r0s), _) = run(&Params::default(), "plain2.wav");
        let tail_rain = crate::dsp::testsig::lufs(&l[5 * FS_USIZE..6 * FS_USIZE + FS_USIZE / 2], &r[5 * FS_USIZE..6 * FS_USIZE + FS_USIZE / 2]);
        let voice = crate::dsp::testsig::lufs(&l0[FS_USIZE..4 * FS_USIZE], &r0s[FS_USIZE..4 * FS_USIZE]);
        assert!(tail_rain > voice - 26.0 && tail_rain < voice - 12.0, "人声 {voice:.1}，静音处的雨声 {tail_rain:.1} LUFS");
        let e = |a: &[f32]| a.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / a.len() as f64;
        // 开头 1 秒淡入：前 50 ms 的雨声比 0.75–0.95 s 轻得多（这两段都没有人声）
        assert!(e(&l[..2400]) < 0.05 * e(&l[36_000..45_600]), "开头没有淡入");
        assert!(e(&l0[..45_600]) == 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_wav(path: &Path, x: &[f32]) {
        let spec = hound::WavSpec { channels: 1, sample_rate: FS_USIZE as u32, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &v in x {
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
    }

    fn read_stereo(path: &Path) -> (Vec<f32>, Vec<f32>) {
        let mut r = hound::WavReader::open(path).unwrap();
        let s: Vec<f32> = r.samples::<i32>().map(|v| v.unwrap() as f32 / 8_388_607.0).collect();
        (s.iter().step_by(2).copied().collect(), s.iter().skip(1).step_by(2).copied().collect())
    }

    #[test]
    fn mix_puts_each_file_on_its_own_side() {
        let dir = std::env::temp_dir().join(format!("auto_asmr_mix_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A：前 5 秒说话，之后静音；B：比 A 长、轻 12 dB，前 5 秒静音、之后说话
        let v = test_voice(12.0);
        let a: Vec<f32> = v.iter().take(10 * FS_USIZE).enumerate().map(|(i, &x)| if i < 5 * FS_USIZE { x } else { 0.0 }).collect();
        let b: Vec<f32> = v.iter().enumerate().map(|(i, &x)| if i >= 5 * FS_USIZE { 0.25 * x } else { 0.0 }).collect();
        let (pa, pb, out) = (dir.join("a.wav"), dir.join("b.wav"), dir.join("mix.wav"));
        write_wav(&pa, &a);
        write_wav(&pb, &b);
        let mut opts = ConvertOptions::new(pa.clone());
        opts.output = out.clone();
        let inputs = [MixInput { path: pa.clone(), voice: Voice::preset(0) }, MixInput { path: pb.clone(), voice: Voice::preset(1) }];
        let rep = convert_mix(&opts, &inputs, &Params::default(), MixOptions::default(), Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
        // 长度按最长的 B；响度精确归一化到目标
        assert!((rep.seconds - 12.0).abs() < 0.05, "{}", rep.seconds);
        assert!((rep.lufs - opts.lufs).abs() < 0.5, "{}", rep.lufs);
        let (l, r) = read_stereo(&out);
        let e = |v: &[f32]| v.iter().map(|x| (x * x) as f64).sum::<f64>() + 1e-30;
        let s = |t: f32| (t * FS) as usize;
        let first = 10.0 * (e(&l[s(0.2)..s(4.8)]) / e(&r[s(0.2)..s(4.8)])).log10();
        let second = 10.0 * (e(&r[s(5.2)..s(11.8)]) / e(&l[s(5.2)..s(11.8)])).log10();
        assert!(first > 20.0 && second > 20.0, "A 在左 {first:.1} dB，B 在右 {second:.1} dB");
        // 两个音源响度已对齐：B 的输入轻 12 dB，输出里两段一样响（相差 2 dB 以内）
        let ld = 10.0 * ((e(&l[s(0.2)..s(4.8)]) + e(&r[s(0.2)..s(4.8)])) / 4.6 / ((e(&l[s(5.2)..s(11.8)]) + e(&r[s(5.2)..s(11.8)])) / 6.6)).log10();
        assert!(ld.abs() < 2.0, "两段响度差 {ld:.1} dB");
        // 一起移动：同样长度和响度；两个音源走同一套，所以报告的换耳次数就是一个音源的
        let mut fo = opts.clone();
        fo.output = dir.join("mix_follow.wav");
        let rf = convert_mix(&fo, &inputs, &Params::default(), MixOptions { layout: Layout::Follow, ..Default::default() }, Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
        assert!((rf.seconds - 12.0).abs() < 0.05 && (rf.lufs - opts.lufs).abs() < 0.5, "{} s {} LUFS", rf.seconds, rf.lufs);
        assert!(rf.n_moves >= 1, "一起移动应当换过边：{}", rf.n_moves);
        assert_eq!(rep.n_moves, 0, "各自固定不换边");
        // 只给一个音源、输出覆盖输入：拒绝
        assert!(convert_mix(&opts, &inputs[..1], &Params::default(), MixOptions::default(), Arc::new(AtomicBool::new(false)), |_, _| {}).is_err());
        let mut bad = opts.clone();
        bad.output = pb.clone();
        assert!(convert_mix(&bad, &inputs, &Params::default(), MixOptions::default(), Arc::new(AtomicBool::new(false)), |_, _| {}).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
