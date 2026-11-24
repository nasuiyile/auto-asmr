//! 实时引擎的处理核心（不依赖声卡驱动）：试听文件、多音频合成的试听、主播模式（麦克风）共用。
//!
//! 输出回调每次调用 [`Pump::fill`]：从 [`Source`]（内存里的音频，或麦克风的环形缓冲）取 48 kHz 的输入，
//! 交给 [`Proc`]（一个引擎，或多音频合成的混音器）处理，重采样到输出设备的采样率。界面和音频线程之间的状态在 [`Shared`] 里
//! （参数、合成配置、跳转、播放位置、位置读数……都是原子量或短锁，音频线程从不阻塞）。
//! 桌面版（`src/realtime.rs`，cpal）和安卓版（文件转换页的试听，AAudio）各自打开输出设备，处理是同一份代码。

use crate::dsp::dynamics::DryOut;
use crate::dsp::engine::{Engine, EngineConfig, Telemetry};
use crate::dsp::mix::{self, MixSetup, Mixer, MAX_VOICES};
use crate::dsp::resample::Resampler;
use crate::dsp::{FS, FS_USIZE};
use crate::spsc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 音频线程与界面之间共享的状态（都是原子量，音频线程从不阻塞）。
pub struct Shared {
    cfg: Mutex<Arc<EngineConfig>>,
    cfg_ver: AtomicU64,
    /// true = 监听原声（不经过效果），false = 输出效果。
    pub monitor_dry: AtomicBool,
    tele: [AtomicU32; 15],
    /// 环形缓冲里平均积压的时间（微秒）。
    pub fill_us: AtomicU32,
    /// 最近每次输出回调的处理时间（微秒，平滑 / 最大）。
    pub proc_us: AtomicU32,
    pub proc_max_us: AtomicU32,
    /// 断流次数（设备报告的短暂断流，或输入来不及 / 积压过多）。
    pub xruns: AtomicU32,
    /// 引擎算法延时（样本，48 kHz）。
    pub algo_samples: AtomicU32,
    /// 输入 / 输出回调一次的帧数。
    pub in_frames: AtomicU32,
    pub out_frames: AtomicU32,
    /// 试听文件时：播放位置和总长（48 kHz 样本）；播完后 `finished` 置位。
    pub play_pos: AtomicU64,
    pub play_len: AtomicU64,
    pub finished: AtomicBool,
    /// 待执行的文件跳转；音频回调取走最新请求，不重新打开输出设备。
    seek_sample: AtomicU64,
    error: Mutex<Option<String>>,
    /// 多音频合成试听：当前的合成配置（与 `cfg` 共用版本号 `cfg_ver`）。
    pub mix: Mutex<Option<Arc<MixSetup>>>,
    /// 多音频合成试听时各音源此刻的位置：每个音源 [方位角, 仰角, 距离, 说话活动]。
    vtele: [AtomicU32; MAX_VOICES * 4],
    n_voices: AtomicU32,
}

impl Shared {
    pub fn new(cfg: Arc<EngineConfig>) -> Self {
        Shared {
            mix: Mutex::new(None),
            vtele: Default::default(),
            n_voices: AtomicU32::new(0),
            cfg: Mutex::new(cfg),
            cfg_ver: AtomicU64::new(1),
            monitor_dry: AtomicBool::new(false),
            tele: Default::default(),
            fill_us: AtomicU32::new(0),
            proc_us: AtomicU32::new(0),
            proc_max_us: AtomicU32::new(0),
            xruns: AtomicU32::new(0),
            algo_samples: AtomicU32::new(0),
            in_frames: AtomicU32::new(0),
            out_frames: AtomicU32::new(0),
            play_pos: AtomicU64::new(0),
            play_len: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            seek_sample: AtomicU64::new(u64::MAX),
            error: Mutex::new(None),
        }
    }

    fn store_tele(&self, t: &Telemetry) {
        let v = [t.az, t.r, t.closeness, t.act, t.in_db, t.out_db, t.gr_db, t.level_gain_db, t.n_moves as f32, t.n_sways as f32, t.el, t.n_tops as f32, t.n_updowns as f32, t.depth_cm, t.n_strokes as f32];
        for (a, x) in self.tele.iter().zip(v) {
            a.store(x.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn telemetry(&self) -> Telemetry {
        let g = |i: usize| f32::from_bits(self.tele[i].load(Ordering::Relaxed));
        Telemetry {
            az: g(0),
            r: g(1),
            closeness: g(2),
            act: g(3),
            in_db: g(4),
            out_db: g(5),
            gr_db: g(6),
            level_gain_db: g(7),
            n_moves: g(8) as u32,
            n_sways: g(9) as u32,
            el: g(10),
            n_tops: g(11) as u32,
            n_updowns: g(12) as u32,
            depth_cm: g(13),
            n_strokes: g(14) as u32,
        }
    }

    /// 设备出错（拔掉、被占用等）时的说明；读一次即清空。
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut e| e.take())
    }

    /// 设备出错（拔掉、被占用等）：记下说明，界面读到后停掉。
    pub fn record_error(&self, message: String) {
        if let Ok(mut pending) = self.error.lock() {
            *pending = Some(message);
        }
    }

    pub fn seek_preview(&self, sample: u64) {
        self.seek_sample.store(sample.min(self.play_len.load(Ordering::Relaxed)), Ordering::Release);
    }

    pub fn preview_finished(&self) -> bool {
        self.seek_sample.load(Ordering::Acquire) == u64::MAX && self.finished.load(Ordering::Relaxed)
    }

    pub fn set_config(&self, cfg: Arc<EngineConfig>) {
        if let Ok(mut g) = self.cfg.lock() {
            *g = cfg;
            self.cfg_ver.fetch_add(1, Ordering::Release);
        }
    }

    pub fn set_mix(&self, setup: Arc<MixSetup>) {
        if let Ok(mut g) = self.mix.lock() {
            *g = Some(setup);
            self.cfg_ver.fetch_add(1, Ordering::Release);
        }
    }

    fn store_voice_tele(&self, mx: &Mixer) {
        let n = mx.voices().min(MAX_VOICES);
        for k in 0..n {
            let t = mx.engine(k).tele;
            for (j, x) in [t.az, t.el, t.r, t.act].into_iter().enumerate() {
                self.vtele[k * 4 + j].store(x.to_bits(), Ordering::Relaxed);
            }
        }
        self.n_voices.store(n as u32, Ordering::Relaxed);
    }

    /// 多音频合成试听时各音源此刻的 [方位角, 仰角, 距离, 说话活动]；不是合成试听时为空。
    pub fn voice_telemetry(&self) -> Vec<[f32; 4]> {
        let n = (self.n_voices.load(Ordering::Relaxed) as usize).min(MAX_VOICES);
        (0..n).map(|k| std::array::from_fn(|j| f32::from_bits(self.vtele[k * 4 + j].load(Ordering::Relaxed)))).collect()
    }
}


/// 引擎的输入来源。
pub enum Source {
    /// 麦克风：输入回调写进环形缓冲。
    Ring(spsc::Consumer),
    /// 试听文件：整段已解码成 48 kHz 单声道。
    Memory { data: Arc<Vec<f32>>, pos: usize },
    /// 多音频合成的试听：几段音频（各自已解码成 48 kHz 单声道）同时播放，总长按最长的那段。
    Multi { data: Vec<Arc<Vec<f32>>>, pos: usize, len: usize },
}

/// 输出回调里的处理器：一个引擎（麦克风 / 试听一个文件），或多音频合成（每个音源一个引擎，相加后统一收尾）。
pub enum Proc {
    One(Engine),
    Mix(Mixer),
}

impl Proc {
    fn latency_samples(&self) -> usize {
        match self {
            Proc::One(e) => e.latency_samples(),
            Proc::Mix(m) => m.latency_samples(),
        }
    }

    fn tele(&self) -> &Telemetry {
        match self {
            Proc::One(e) => &e.tele,
            Proc::Mix(m) => &m.tele,
        }
    }
}

/// 输出回调里做的全部工作（不依赖声卡驱动，可以单独测试）。
pub struct Pump {
    src: Source,
    proc: Proc,
    /// 多音频合成时每个音源的一块输入。
    inbs: Vec<Vec<f32>>,
    shared: Arc<Shared>,
    cfg_ver: u64,
    rs: [Resampler; 2],
    out_rate: f64,
    q: [Vec<f32>; 2], // 已经是输出采样率的样本，等着被取走
    qpos: usize,
    primed: bool,
    fill_avg: f32,
    in_max: f32,
    proc_avg: f32,
    inb: Vec<f32>,
    el: Vec<f32>,
    er: Vec<f32>,
    last_output: [f32; 2],
    seek_fade: usize,
    /// 合成试听各段的延迟（样本，左右双人）：现在用的、淡出中的旧值，以及淡变还剩多少样本。
    dl: [usize; MAX_VOICES],
    dl_old: [usize; MAX_VOICES],
    dl_fade: usize,
    /// 主播模式的左右双人：麦克风输入的历史（环形，[`HIST`] 个样本）和已经写进去多少个样本。其余情况为空。
    hist: Vec<f32>,
    hist_pos: usize,
    /// 监听原声时的输出级（输出增益 + 峰值限制器），以及上一块是不是原声（刚切到原声时清掉它的旧状态）。
    dry_out: DryOut,
    was_dry: bool,
}

pub const N: usize = 64;
/// 试听中改了延迟：从旧的位置淡到新的位置用多少样本（20 ms），免得一下跳过去咔嗒一声。
pub const DL_FADE: usize = mix::DELAY_FADE;
/// 主播模式左右双人的输入历史（样本，2 的幂）：要装得下最长的延迟（300 ms = 14400 个样本）再加一块。
pub const HIST: usize = 1 << 15;
const _: () = assert!(HIST >= (mix::DUO_MAX_MS as usize) * FS_USIZE / 1000 + N);


impl Pump {
    pub fn new(src: Source, eng: Engine, shared: Arc<Shared>, out_rate: f64) -> Self {
        Self::with_proc(src, Proc::One(eng), shared, out_rate)
    }

    pub fn with_proc(src: Source, proc: Proc, shared: Arc<Shared>, out_rate: f64) -> Self {
        shared.algo_samples.store(proc.latency_samples() as u32, Ordering::Relaxed);
        match &src {
            Source::Memory { data, pos } => {
                shared.play_len.store(data.len() as u64, Ordering::Relaxed);
                shared.play_pos.store((*pos).min(data.len()) as u64, Ordering::Relaxed);
            }
            Source::Multi { pos, len, .. } => {
                shared.play_len.store(*len as u64, Ordering::Relaxed);
                shared.play_pos.store((*pos).min(*len) as u64, Ordering::Relaxed);
            }
            Source::Ring(_) => {}
        }
        // 合成时每个声音一块输入：试听是几段音频，主播模式是同一个麦克风按各自的延迟取
        let n_in = match (&src, &proc) {
            (Source::Multi { data, .. }, _) => data.len(),
            (Source::Ring(_), Proc::Mix(m)) => m.voices(),
            _ => 0,
        };
        let mic_mix = matches!((&src, &proc), (Source::Ring(_), Proc::Mix(_)));
        let dl = if let Proc::Mix(m) = &proc { m.delays() } else { [0; MAX_VOICES] };
        Pump {
            src,
            proc,
            inbs: (0..n_in).map(|_| vec![0.0; N]).collect(),
            shared,
            cfg_ver: 1,
            rs: [Resampler::new(FS as f64, out_rate), Resampler::new(FS as f64, out_rate)],
            out_rate,
            q: [Vec::with_capacity(65536), Vec::with_capacity(65536)],
            qpos: 0,
            primed: false,
            fill_avg: 0.0,
            in_max: 0.0,
            proc_avg: 0.0,
            inb: vec![0.0; N],
            el: vec![0.0; N],
            er: vec![0.0; N],
            last_output: [0.0; 2],
            seek_fade: 0,
            dl,
            dl_old: dl,
            dl_fade: 0,
            hist: if mic_mix { vec![0.0; HIST] } else { Vec::new() },
            hist_pos: 0,
            dry_out: DryOut::new(FS),
            was_dry: false,
        }
    }

    /// 取下一块 64 个输入样本；麦克风还没送到时返回 false。合成时再按各声音的延迟给每个声音取一块（`inbs`）。
    fn next_block(&mut self) -> bool {
        let pos = match &mut self.src {
            Source::Ring(rx) => {
                if !(rx.len() >= N && rx.pop_slice(&mut self.inb[..N]) == N) {
                    return false;
                }
                if self.inbs.is_empty() {
                    return true;
                }
                // 主播模式的左右双人：麦克风的输入记进历史，各声音从里面按自己的延迟取
                let p = self.hist_pos;
                for (i, &v) in self.inb[..N].iter().enumerate() {
                    self.hist[(p + i) & (HIST - 1)] = v;
                }
                self.hist_pos += N;
                p
            }
            Source::Memory { data, pos } => {
                let n = data.len();
                for (i, v) in self.inb[..N].iter_mut().enumerate() {
                    *v = if *pos + i < n { data[*pos + i] } else { 0.0 };
                }
                *pos += N;
                self.shared.play_pos.store((*pos).min(n) as u64, Ordering::Relaxed);
                // 播完再多放 1 秒（房间尾音），然后停
                if *pos > n + FS_USIZE {
                    self.shared.finished.store(true, Ordering::Relaxed);
                }
                return true;
            }
            Source::Multi { pos, len, .. } => {
                let p = *pos;
                *pos += N;
                self.shared.play_pos.store((*pos).min(*len) as u64, Ordering::Relaxed);
                if *pos > *len + FS_USIZE {
                    self.shared.finished.store(true, Ordering::Relaxed);
                }
                p
            }
        };
        // 第 k 段按"现在位置 − 它的延迟"取（左右双人的第 2 个声音晚一点）。改了延迟：先把这次的淡变走完，
        // 再在 20 ms 里从旧位置淡到新位置
        if self.dl_fade == 0 {
            if let Proc::Mix(m) = &self.proc {
                let want = m.delays();
                if want != self.dl {
                    self.dl_old = self.dl;
                    self.dl = want;
                    self.dl_fade = DL_FADE;
                }
            }
        }
        let fading = (self.dl_fade > 0).then(|| DL_FADE - self.dl_fade);
        match &self.src {
            Source::Multi { data, .. } => mix::read_delayed(&mut self.inbs, pos, N, &self.dl, &self.dl_old, fading, |k, s| data[k].get(s).copied().unwrap_or(0.0)),
            _ => {
                // 麦克风的历史：只留最近 HIST 个样本（延迟最多 300 ms，远在这之内）
                let (h, end) = (&self.hist, self.hist_pos);
                mix::read_delayed(&mut self.inbs, pos, N, &self.dl, &self.dl_old, fading, |_, s| if end - s <= HIST { h[s & (HIST - 1)] } else { 0.0 })
            }
        }
        self.dl_fade = self.dl_fade.saturating_sub(N);
        true
    }

    pub fn fill(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        crate::dsp::flush_denormals();
        let t0 = std::time::Instant::now();
        // 界面改了参数：换一份配置（拿不到锁就下次再说，绝不等待）
        let ver = self.shared.cfg_ver.load(Ordering::Acquire);
        if ver != self.cfg_ver {
            let applied = match &mut self.proc {
                Proc::One(e) => match self.shared.cfg.try_lock() {
                    Ok(g) => {
                        e.set_config(g.clone());
                        true
                    }
                    Err(_) => false,
                },
                // 合成：各音源的配置和增益（界面线程里已经算好，这里只换指针、抄增益，不分配内存）
                Proc::Mix(m) => match self.shared.mix.try_lock() {
                    Ok(g) => {
                        if let Some(s) = g.as_ref() {
                            m.set_setup(s);
                        }
                        true
                    }
                    Err(_) => false,
                },
            };
            if applied {
                self.cfg_ver = ver;
                self.shared.algo_samples.store(self.proc.latency_samples() as u32, Ordering::Relaxed);
            }
        }
        let seek = self.shared.seek_sample.load(Ordering::Acquire);
        if seek != u64::MAX {
            let len = match &self.src {
                Source::Memory { data, .. } => Some(data.len()),
                Source::Multi { len, .. } => Some(*len),
                Source::Ring(_) => None,
            };
            if let (Some(len), Source::Memory { pos, .. } | Source::Multi { pos, .. }) = (len, &mut self.src) {
                *pos = seek.min(len as u64) as usize;
                self.shared.play_pos.store(*pos as u64, Ordering::Relaxed);
                self.shared.finished.store(false, Ordering::Relaxed);
                for q in &mut self.q {
                    q.clear();
                }
                self.qpos = 0;
                for rs in &mut self.rs {
                    rs.reset();
                }
                self.seek_fade = (self.out_rate * 0.005).round().max(1.0) as usize;
                // 跳转本身有淡入：延迟直接用新的，不用再淡
                if let Proc::Mix(m) = &self.proc {
                    self.dl = m.delays();
                }
                self.dl_old = self.dl;
                self.dl_fade = 0;
            }
            // 先清除播完状态，再确认跳转完成；期间的新拖动请求留到下一回调。
            let _ = self.shared.seek_sample.compare_exchange(seek, u64::MAX, Ordering::AcqRel, Ordering::Acquire);
        }
        let frames = out_l.len();
        // 麦克风：目标积压量 = 一次输入回调的量 + 2 ms 余量；攒够了才开始
        let mut target = 0.0;
        if let Source::Ring(rx) = &self.src {
            let in48 = self.shared.in_frames.load(Ordering::Relaxed) as f32;
            self.in_max = (self.in_max * 0.999).max(in48);
            target = self.in_max + 0.002 * FS;
            if !self.primed {
                if (rx.len() as f32) < target + N as f32 {
                    out_l.fill(0.0);
                    out_r.fill(0.0);
                    return;
                }
                self.primed = true;
                self.fill_avg = rx.len() as f32;
            }
        }
        let dry = self.shared.monitor_dry.load(Ordering::Relaxed);
        let mut stalled = false;
        while self.q[0].len() - self.qpos < frames {
            if !self.next_block() {
                stalled = true; // 输入还没到：这次先输出静音，引擎不往前走
                break;
            }
            match &mut self.proc {
                Proc::One(eng) => {
                    if dry {
                        self.el[..N].copy_from_slice(&self.inb[..N]);
                        self.er[..N].copy_from_slice(&self.inb[..N]);
                    } else {
                        eng.process(&self.inb[..N], &mut self.el[..N], &mut self.er[..N]);
                    }
                }
                Proc::Mix(mx) => {
                    if dry {
                        // 监听原声：麦克风就是麦克风本身（左右双人不叠出回声）；试听文件是几段原声相加
                        let mic = matches!(self.src, Source::Ring(_));
                        for i in 0..N {
                            let s: f32 = if mic { self.inb[i] } else { self.inbs.iter().map(|b| b[i]).sum() };
                            self.el[i] = s;
                            self.er[i] = s;
                        }
                    } else {
                        let ins: [&[f32]; MAX_VOICES] = std::array::from_fn(|k| self.inbs.get(k).map_or(&[][..], |b| &b[..N]));
                        // 自动平衡按这一块在各音频里的位置取增益（拖动播放条后也对得上）
                        if let Source::Multi { pos, .. } = &self.src {
                            mx.seek(pos.saturating_sub(N));
                        }
                        mx.process(&ins[..self.inbs.len().min(MAX_VOICES)], &mut self.el[..N], &mut self.er[..N]);
                    }
                }
            }
            if dry {
                // 原声也乘输出增益、过峰值限制器（同效果那一路的收尾），音量滑条照样起作用
                let p = &match &self.proc {
                    Proc::One(e) => e.config(),
                    Proc::Mix(m) => m.config(),
                }
                .params;
                let (gain, ceiling) = (p.out_gain_db, p.ceiling_db);
                if !self.was_dry {
                    self.dry_out.reset();
                }
                self.dry_out.process(&mut self.el[..N], &mut self.er[..N], gain, ceiling);
            }
            self.was_dry = dry;
            if self.qpos > 0 && self.qpos == self.q[0].len() {
                self.q[0].clear();
                self.q[1].clear();
                self.qpos = 0;
            }
            self.rs[0].process(&self.el[..N], &mut self.q[0]);
            self.rs[1].process(&self.er[..N], &mut self.q[1]);
        }
        let have = (self.q[0].len() - self.qpos).min(self.q[1].len() - self.qpos).min(frames);
        out_l[..have].copy_from_slice(&self.q[0][self.qpos..self.qpos + have]);
        out_r[..have].copy_from_slice(&self.q[1][self.qpos..self.qpos + have]);
        out_l[have..].fill(0.0);
        out_r[have..].fill(0.0);
        // 用 5 ms 淡入衔接跳转，避免源波形突变产生咔嗒声。
        let fade_len = (self.out_rate * 0.005).round().max(1.0) as usize;
        for i in 0..frames.min(self.seek_fade) {
            let mix = 1.0 - self.seek_fade as f32 / fade_len as f32;
            out_l[i] = self.last_output[0] * (1.0 - mix) + out_l[i] * mix;
            out_r[i] = self.last_output[1] * (1.0 - mix) + out_r[i] * mix;
            self.seek_fade -= 1;
        }
        if let (Some(&l), Some(&r)) = (out_l.last(), out_r.last()) {
            // 淡入跨多个回调时保留同一衔接起点。
            if self.seek_fade == 0 {
                self.last_output = [l, r];
            }
        }
        self.qpos += have;
        if self.qpos > 8192 {
            self.q[0].drain(..self.qpos);
            self.q[1].drain(..self.qpos);
            self.qpos = 0;
        }
        if stalled {
            self.shared.xruns.fetch_add(1, Ordering::Relaxed);
        }
        if let Source::Ring(rx) = &mut self.src {
            // 时钟漂移：积压偏多就稍微快一点消耗，偏少就慢一点；积压太多（> 目标 + 150 ms）直接丢掉旧的
            let fill = rx.len() as f32;
            self.fill_avg += 0.05 * (fill - self.fill_avg);
            if fill > target + 0.15 * FS {
                rx.skip((fill - target) as usize);
                self.fill_avg = target;
                self.shared.xruns.fetch_add(1, Ordering::Relaxed);
            }
            let trim = 1.0 + (((self.fill_avg - target) / FS) as f64 * 0.5).clamp(-0.005, 0.005);
            for r in self.rs.iter_mut() {
                r.set_trim(FS as f64, self.out_rate, trim);
            }
            self.shared.fill_us.store((self.fill_avg / FS * 1e6) as u32, Ordering::Relaxed);
        }
        self.shared.store_tele(self.proc.tele());
        if let Proc::Mix(mx) = &self.proc {
            self.shared.store_voice_tele(mx);
        }
        let us = t0.elapsed().as_secs_f32() * 1e6;
        self.proc_avg += 0.05 * (us - self.proc_avg);
        self.shared.proc_us.store(self.proc_avg as u32, Ordering::Relaxed);
        self.shared.proc_max_us.fetch_max(us as u32, Ordering::Relaxed);
    }
}


/// 麦克风一侧：把设备采样率的输入重采样到 48 kHz，写进环形缓冲。
pub fn mic_sink(in_rate: u32, mut tx: spsc::Producer, shared: Arc<Shared>) -> impl FnMut(&[f32]) + Send + 'static {
    let mut rs = if in_rate as usize != FS_USIZE { Some(Resampler::new(in_rate as f64, FS as f64)) } else { None };
    let mut conv: Vec<f32> = Vec::with_capacity(32768);
    move |x: &[f32]| {
        let n = match &mut rs {
            Some(r) => {
                conv.clear();
                r.process(x, &mut conv);
                tx.push_slice(&conv);
                conv.len()
            }
            None => {
                tx.push_slice(x);
                x.len()
            }
        };
        shared.in_frames.store(n as u32, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::engine::PostMode;
    use crate::dsp::params::Params;

    fn preview_pump(audio: Vec<f32>, out_rate: f64, dry: bool) -> (Pump, Arc<Shared>) {
        let params = Params { auto_level: false, ..Params::default() };
        let cfg = Arc::new(EngineConfig::new(params, None));
        let shared = Arc::new(Shared::new(cfg.clone()));
        shared.monitor_dry.store(dry, Ordering::Relaxed);
        let pump = Pump::new(Source::Memory { data: Arc::new(audio), pos: 0 }, Engine::new(cfg, PostMode::Realtime, 0.0), shared.clone(), out_rate);
        (pump, shared)
    }

    #[test]
    fn preview_seeks_both_directions_and_restarts_after_eof() {
        for rate in [48_000.0, 44_100.0] {
            let mut audio = vec![0.2; FS_USIZE];
            audio.extend(vec![-0.4; FS_USIZE]);
            let (mut pump, shared) = preview_pump(audio, rate, true);
            // 非整块长度使队列残留旧音频，跳转时必须清掉。
            pump.fill(&mut [0.0; 137], &mut [0.0; 137]);
            shared.seek_preview((FS_USIZE / 2) as u64);
            shared.seek_preview(FS_USIZE as u64); // 连续拖动只执行最新位置。
            let (mut l, mut r) = ([0.0; 1024], [0.0; 1024]);
            pump.fill(&mut l, &mut r);
            assert!(l[512..].iter().chain(&r[512..]).all(|x| (*x + 0.4).abs() < 1e-4));
            assert!((FS_USIZE as u64..FS_USIZE as u64 + 2048).contains(&shared.play_pos.load(Ordering::Relaxed)));

            shared.seek_preview(100);
            pump.fill(&mut l, &mut r);
            assert!(l[512..].iter().all(|x| (*x - 0.2).abs() < 1e-4));
            assert!(shared.play_pos.load(Ordering::Relaxed) < 2048);

            shared.seek_preview(u64::MAX); // 超出长度的跳转截到结尾。
            for _ in 0..60 {
                pump.fill(&mut l, &mut r);
            }
            assert!(shared.preview_finished());
            shared.seek_preview(0);
            assert!(!shared.preview_finished());
            pump.fill(&mut l, &mut r);
            assert!(!shared.preview_finished());
            assert!(l[512..].iter().all(|x| (*x - 0.2).abs() < 1e-4));
        }
    }

    #[test]
    fn effect_parameters_change_during_preview_without_restarting() {
        let audio: Vec<f32> = (0..FS_USIZE * 3).map(|n| 0.01 * (n as f32 * std::f32::consts::TAU * 440.0 / FS).sin()).collect();
        let (mut pump, shared) = preview_pump(audio, 48_000.0, false);
        let (mut l, mut r) = ([0.0; 1024], [0.0; 1024]);
        for _ in 0..30 {
            pump.fill(&mut l, &mut r);
        }
        let before = l.iter().chain(&r).map(|x| x * x).sum::<f32>();
        let position = shared.play_pos.load(Ordering::Relaxed);
        let cfg = shared.cfg.lock().unwrap().clone();
        let mut params = cfg.params.clone();
        params.out_gain_db -= 24.0;
        shared.set_config(Arc::new(EngineConfig::new(params, Some(&cfg))));
        for _ in 0..4 {
            pump.fill(&mut l, &mut r);
        }
        let after = l.iter().chain(&r).map(|x| x * x).sum::<f32>();
        assert!(after < before * 0.02, "gain did not change: {before} -> {after}");
        assert!(shared.play_pos.load(Ordering::Relaxed) > position);
        assert!(!shared.preview_finished());
    }

    #[test]
    fn dry_monitor_follows_output_gain() {
        // 监听原声时输出增益照样起作用：−12 dB 后响度降 12 dB，+12 dB 加在满幅附近的声音上不削波
        let audio: Vec<f32> = (0..FS_USIZE * 4).map(|n| 0.5 * (n as f32 * std::f32::consts::TAU * 440.0 / FS).sin()).collect();
        let (mut pump, shared) = preview_pump(audio, 48_000.0, true);
        let (mut l, mut r) = ([0.0; 1024], [0.0; 1024]);
        let mut energy = |pump: &mut Pump| {
            let mut e = 0.0f32;
            let mut pk = 0.0f32;
            for _ in 0..20 {
                pump.fill(&mut l, &mut r);
                e += l.iter().chain(&r).map(|x| x * x).sum::<f32>();
                pk = l.iter().chain(&r).fold(pk, |m, x| m.max(x.abs()));
                assert_eq!(l, r);
            }
            (e, pk)
        };
        energy(&mut pump);
        let (before, pk0) = energy(&mut pump);
        assert!((pk0 - 0.5).abs() < 0.01, "0 dB 时原样：峰值 {pk0}");
        let set_gain = |db: f32| {
            let cfg = shared.cfg.lock().unwrap().clone();
            let params = Params { out_gain_db: db, ..cfg.params.clone() };
            shared.set_config(Arc::new(EngineConfig::new(params, Some(&cfg))));
        };
        set_gain(-12.0);
        energy(&mut pump);
        let (after, _) = energy(&mut pump);
        let db = 10.0 * (after / before).log10();
        assert!((db + 12.0).abs() < 0.2, "原声的输出增益没起作用：{db:.2} dB");
        set_gain(12.0);
        energy(&mut pump);
        let (_, pk) = energy(&mut pump);
        assert!(pk <= 0.9 && pk > 0.85, "调大后应被峰值上限（−1 dB）压住：峰值 {pk}");
    }

    #[test]
    fn mix_preview_keeps_sources_apart_follows_changes_and_seeks() {
        use crate::dsp::mix::{voice_configs, Voice};
        let energy = |v: &[f32]| v.iter().map(|x| (x * x) as f64).sum::<f64>() + 1e-30;
        // 左耳的音源比右耳的短、而且是静音；右耳的在说话：输出几乎都在右耳，总长按长的那段
        let x = crate::dsp::testsig::test_voice(8.0);
        let len = x.len();
        let base = EngineConfig::new(Params { auto_level: false, ..Params::default() }, None);
        let setup = |v: [Voice; 2]| Arc::new(MixSetup { cfgs: voice_configs(&base, &v, None), gains: vec![1.0, 1.0], offsets: vec![0.0; 2], follow: false, balance: None, balance_strength: 0.0, delays: vec![0; 2] });
        let first = setup([Voice::preset(0), Voice::preset(1)]);
        let shared = Arc::new(Shared::new(first.cfgs[0].clone()));
        *shared.mix.lock().unwrap() = Some(first.clone());
        let src = Source::Multi { data: vec![Arc::new(vec![0.0; len / 2]), Arc::new(x)], pos: 0, len };
        let mut pump = Pump::with_proc(src, Proc::Mix(Mixer::new(&first, PostMode::Realtime, 0.0)), shared.clone(), 48_000.0);
        assert_eq!(shared.play_len.load(Ordering::Relaxed), len as u64);
        let (mut l, mut r) = (vec![0f32; 1024], vec![0f32; 1024]);
        let mut run = |pump: &mut Pump, blocks: usize| {
            let (mut el, mut er) = (0f64, 0f64);
            for _ in 0..blocks {
                pump.fill(&mut l, &mut r);
                el += energy(&l);
                er += energy(&r);
            }
            10.0 * (er / el).log10()
        };
        let right_over_left = run(&mut pump, 140); // 约 3 秒
        assert!(right_over_left > 20.0, "右耳应比左耳响得多：{right_over_left:.1} dB");
        let vt = shared.voice_telemetry();
        assert_eq!(vt.len(), 2);
        assert!(vt[0][0] > 180.0 && vt[1][0] < 180.0, "{vt:?}");
        // 播放中把两个音源对调：说话的那个挪到左耳，不用重新开始；跳回开头对两个音源同时生效
        shared.set_mix(setup([Voice::preset(1), Voice::preset(0)]));
        shared.seek_preview(0);
        run(&mut pump, 1);
        assert!(shared.play_pos.load(Ordering::Relaxed) < 2048);
        run(&mut pump, 90); // 等它挪过去
        let after = run(&mut pump, 90);
        assert!(after < -10.0, "挪到左耳后应左耳更响：右比左 {after:.1} dB");
        assert!(shared.voice_telemetry()[1][0] > 180.0);
        // 播到最长那段的结尾（再加 1 秒尾音）就结束
        shared.seek_preview(u64::MAX);
        run(&mut pump, 60);
        assert!(shared.preview_finished());
    }

    #[test]
    fn duo_preview_reads_the_second_voice_later_and_changes_delay_smoothly() {
        use crate::dsp::mix::Voice;
        // 左右双人的试听：同一段音频给两个声音，第 2 个按"位置 − 延迟"取；拖延迟滑条时 20 ms 淡过去；跳转后立刻对上
        let x = crate::dsp::testsig::test_voice(6.0);
        let len = x.len();
        let base = Arc::new(EngineConfig::new(Params { auto_level: false, ..Params::default() }, None));
        let voices = [Voice::preset(0), Voice::preset(1)];
        let st = |d: usize| Arc::new(MixSetup::new(&base, &voices, &[None, None], false, None).with_delays(&[0, d]));
        let first = st(4800);
        let shared = Arc::new(Shared::new(base.clone()));
        *shared.mix.lock().unwrap() = Some(first.clone());
        let audio = Arc::new(x.clone());
        let src = Source::Multi { data: vec![audio.clone(), audio], pos: 0, len: len + 4800 };
        let mut pump = Pump::with_proc(src, Proc::Mix(Mixer::new(&first, PostMode::Realtime, 0.0)), shared.clone(), 48_000.0);
        let at = |t: usize, d: usize| if t >= d { x.get(t - d).copied().unwrap_or(0.0) } else { 0.0 };
        let pos = |p: &Pump| if let Source::Multi { pos, .. } = &p.src { *pos } else { 0 };
        for _ in 0..200 {
            let p0 = pos(&pump);
            assert!(pump.next_block());
            for i in 0..N {
                assert_eq!(pump.inbs[0][i], at(p0 + i, 0));
                assert_eq!(pump.inbs[1][i], at(p0 + i, 4800));
            }
        }
        // 改成 50 ms：淡变中是新旧两个位置的混合（连续、没有跳变），20 ms 之后完全是新的
        if let Proc::Mix(m) = &mut pump.proc {
            m.set_setup(&st(2400));
        }
        let mut max_step = 0f32;
        let mut prev = pump.inbs[1][N - 1];
        for b in 0..30 {
            let p0 = pos(&pump);
            assert!(pump.next_block());
            for i in 0..N {
                let v = pump.inbs[1][i];
                if b < DL_FADE / N {
                    max_step = max_step.max((v - prev).abs());
                } else {
                    assert_eq!(v, at(p0 + i, 2400));
                }
                prev = v;
            }
        }
        let own_step = x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        assert!(max_step <= own_step * 1.01 + 1e-6, "淡变中一步跳了 {max_step}（原音频自己最多 {own_step}）");
        // 跳转：新位置上第 2 个声音直接从 50 ms 之前开始
        shared.seek_preview(96_000);
        let (mut l, mut r) = (vec![0f32; 64], vec![0f32; 64]);
        pump.fill(&mut l, &mut r);
        let p0 = pos(&pump);
        assert!(pump.next_block());
        for i in 0..N {
            assert_eq!(pump.inbs[1][i], at(p0 + i, 2400));
        }
    }

    #[test]
    fn mix_preview_follow_mode_keeps_voices_opposite() {
        use crate::dsp::mix::Voice;
        // 一起移动的试听：两个音源始终在头的两侧相对；中途改移动方式（穿梭 → 环绕）也一样
        let x = crate::dsp::testsig::test_voice(16.0);
        let len = x.len();
        let base = Arc::new(EngineConfig::new(Params { auto_level: false, ..Params::default() }, None));
        let voices = [Voice::preset(0), Voice::preset(1)];
        let st = Arc::new(MixSetup::new(&base, &voices, &[None, None], true, None));
        let shared = Arc::new(Shared::new(base.clone()));
        *shared.mix.lock().unwrap() = Some(st.clone());
        let audio = Arc::new(x);
        let src = Source::Multi { data: vec![audio.clone(), audio], pos: 0, len };
        let mut pump = Pump::with_proc(src, Proc::Mix(Mixer::new(&st, PostMode::Realtime, 0.0)), shared.clone(), 44_100.0);
        let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
        let mut min_gap = 180.0f32;
        let gap = |v: &[[f32; 4]]| ((v[0][0] - v[1][0] + 180.0).rem_euclid(360.0) - 180.0).abs();
        for i in 0..1200 {
            if i == 600 {
                let p = Params { mode: crate::dsp::params::MotionMode::Orbit, auto_level: false, ..Params::default() };
                let orbit = Arc::new(EngineConfig::new(p, Some(&base)));
                shared.set_mix(Arc::new(MixSetup::new(&orbit, &voices, &[None, None], true, Some(&st))));
            }
            pump.fill(&mut l, &mut r);
            let vt = shared.voice_telemetry();
            if i > 5 {
                min_gap = min_gap.min(gap(&vt));
            }
        }
        assert!(min_gap > 150.0, "最近时只相隔 {min_gap:.0}°");
    }

    /// 主播模式的左右双人：一个环形缓冲当麦克风，输出回调里的合成器按主播模式（停顿 1 秒后恢复动作）。
    fn mic_duo_pump(setup: &Arc<MixSetup>, live: bool) -> (Pump, spsc::Producer, Arc<Shared>) {
        let shared = Arc::new(Shared::new(setup.cfgs[0].clone()));
        *shared.mix.lock().unwrap() = Some(setup.clone());
        let (tx, rx) = spsc::channel(FS_USIZE);
        let mut m = Mixer::new(setup, PostMode::Realtime, 0.0);
        m.set_live(live);
        (Pump::with_proc(Source::Ring(rx), Proc::Mix(m), shared.clone(), 48_000.0), tx, shared)
    }

    fn duo_setup(base: &Arc<EngineConfig>, follow: bool, delay: usize) -> Arc<MixSetup> {
        use crate::dsp::mix::Voice;
        Arc::new(MixSetup::new(base, &[Voice::preset(0), Voice::preset(1)], &[None, None], follow, None).with_delays(&[0, delay]))
    }

    #[test]
    fn mic_duo_reads_the_second_voice_later_from_the_mic_history() {
        // 两个声音都来自麦克风：1 号就是麦克风这一块，2 号是麦克风晚 100 ms；改成 50 ms 时 20 ms 淡过去、没有跳变；
        // 历史是环形的，跑过好几圈（3 秒 > HIST）之后照样逐样本对得上
        let x = crate::dsp::testsig::test_voice(3.0);
        let base = Arc::new(EngineConfig::new(Params::default(), None));
        let (mut pump, mut tx, _) = mic_duo_pump(&duo_setup(&base, false, 4800), true);
        assert_eq!(pump.inbs.len(), 2);
        let at = |t: usize, d: usize| if t >= d { x[t - d] } else { 0.0 };
        let mut t = 0;
        let mut feed = |pump: &mut Pump, t: &mut usize| {
            assert_eq!(tx.push_slice(&x[*t..*t + N]), N);
            assert!(pump.next_block());
            *t += N;
            *t - N
        };
        // 还没送到时不往前走
        assert!(!pump.next_block());
        while t + N <= FS_USIZE * 2 {
            let p0 = feed(&mut pump, &mut t);
            for i in 0..N {
                assert_eq!(pump.inb[i], x[p0 + i]);
                assert_eq!(pump.inbs[0][i], at(p0 + i, 0));
                assert_eq!(pump.inbs[1][i], at(p0 + i, 4800));
            }
        }
        assert!(t > HIST);
        if let Proc::Mix(m) = &mut pump.proc {
            m.set_setup(&duo_setup(&base, false, 2400));
        }
        let mut max_step = 0f32;
        let mut prev = pump.inbs[1][N - 1];
        for b in 0..40 {
            let p0 = feed(&mut pump, &mut t);
            for i in 0..N {
                let v = pump.inbs[1][i];
                if b < DL_FADE / N {
                    max_step = max_step.max((v - prev).abs());
                } else {
                    assert_eq!(v, at(p0 + i, 2400));
                }
                assert_eq!(pump.inbs[0][i], x[p0 + i]);
                prev = v;
            }
        }
        let own_step = x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        assert!(max_step <= own_step * 1.01 + 1e-6, "淡变中一步跳了 {max_step}（原音频自己最多 {own_step}）");
    }

    #[test]
    fn mic_duo_puts_one_voice_at_each_ear_and_monitors_the_plain_mic() {
        // 各自固定（左耳、右耳）：两个声音各在一边，左右两边差不多一样响；位置图拿到两个声音的位置；
        // 监听原声时输出就是麦克风本身（不叠出第 2 个声音）
        let x = crate::dsp::testsig::test_voice(8.0);
        let base = Arc::new(EngineConfig::new(Params { auto_level: false, ..Params::default() }, None));
        let (mut pump, tx, shared) = mic_duo_pump(&duo_setup(&base, false, 3840), true);
        let mut mic = mic_sink(FS_USIZE as u32, tx, shared.clone());
        let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
        let (mut el, mut er) = (0f64, 0f64);
        for (j, c) in x.chunks_exact(256).enumerate() {
            mic(c);
            pump.fill(&mut l, &mut r);
            if j > 100 {
                el += l.iter().map(|v| (v * v) as f64).sum::<f64>();
                er += r.iter().map(|v| (v * v) as f64).sum::<f64>();
            }
        }
        let lr = 10.0 * (el / er).log10();
        println!("主播模式左右双人（各自固定）：左比右 {lr:.2} dB");
        assert!(lr.abs() < 3.0, "左右两边应差不多一样响：左比右 {lr:.1} dB");
        let vt = shared.voice_telemetry();
        assert_eq!(vt.len(), 2);
        assert!(vt[0][0] > 180.0 && vt[1][0] < 180.0, "{vt:?}");
        assert_eq!(shared.xruns.load(Ordering::Relaxed), 0);
        // 监听原声：输出就是麦克风本身——送进去一下脉冲，出来只有一下，没有晚 80 ms 的第 2 下
        shared.monitor_dry.store(true, Ordering::Relaxed);
        let mut click = vec![0f32; FS_USIZE];
        click[FS_USIZE / 2] = 1.0;
        let mut got = Vec::new();
        for (j, c) in click.chunks_exact(256).enumerate() {
            mic(c);
            pump.fill(&mut l, &mut r);
            // 头几次回调还在放切换前已经算好排着队的效果声
            if j > 4 {
                assert_eq!(l, r);
                got.extend_from_slice(&l);
            }
        }
        let peaks: Vec<usize> = got.iter().enumerate().filter(|(_, v)| v.abs() > 0.3).map(|(i, _)| i).collect();
        assert!(!peaks.is_empty() && peaks[peaks.len() - 1] - peaks[0] < 8, "监听原声应只有麦克风这一下：{peaks:?}");
    }

    #[test]
    fn mic_duo_balance_evens_out_the_two_voices_live() {
        // 一左一右、2 号放远（离头 0.6 m）：不平衡时右边明显轻。自动平衡 100%（输入响度边听边算）后两个声音差不多一样响，
        // 左右差也小了很多；运行中把强度调回 0，增益慢慢回到 0 dB，之后不再处理（和从没打开过的输出逐样本相同）
        use crate::dsp::mix::Voice;
        let x = crate::dsp::testsig::test_voice(16.0);
        let base = Arc::new(EngineConfig::new(Params { auto_level: false, ..Params::default() }, None));
        let voices = [Voice::preset(0), Voice { dist: Some(0.6), ..Voice::preset(1) }];
        let setup = |s: f32| Arc::new(MixSetup::new(&base, &voices, &[None, None], false, None).with_delays(&[0, 3840]).with_balance(None, s));
        let run = |strength: f32, off_at: Option<usize>| {
            let (mut pump, mut tx, _) = mic_duo_pump(&setup(strength), true);
            let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
            let (mut el, mut er) = (0f64, 0f64);
            let mut out: Vec<f32> = Vec::new();
            for (j, c) in x.chunks_exact(256).enumerate() {
                if Some(j) == off_at {
                    if let Proc::Mix(m) = &mut pump.proc {
                        m.set_setup(&setup(0.0));
                    }
                }
                tx.push_slice(c);
                pump.fill(&mut l, &mut r);
                if j > 4 * FS_USIZE / 256 {
                    el += l.iter().map(|v| (v * v) as f64).sum::<f64>();
                    er += r.iter().map(|v| (v * v) as f64).sum::<f64>();
                }
                out.extend(l.iter().chain(&r));
            }
            let Proc::Mix(m) = &pump.proc else { unreachable!() };
            (10.0 * (el / er).log10(), m.balance_stats(), out)
        };
        let (lr_off, _, plain) = run(0.0, None);
        let (lr_on, stats, _) = run(1.0, None);
        let ((b5, b9), (a5, a9)) = stats.expect("两个声音同时出声，应有统计");
        println!("主播模式左右双人（2 号离头 0.6 m）：左比右 不平衡 {lr_off:.1} dB → 平衡 {lr_on:.1} dB；两个声音的响度差 中位 / 90% {b5:.1} / {b9:.1} → {a5:.1} / {a9:.1} dB");
        assert!(b5 > 4.0 && a5 < 1.5 && a9 < b9 / 3.0, "响度差 {b5:.1} / {b9:.1} → {a5:.1} / {a9:.1} dB");
        assert!(lr_off > 3.0 && lr_on.abs() < lr_off / 2.0, "左比右 {lr_off:.1} → {lr_on:.1} dB");
        // 第 6 秒把强度调回 0：几秒后增益回到 0 dB（±0.05 dB 以内就停下不再处理），输出和从没打开过的一样
        let (_, _, out) = run(1.0, Some(6 * FS_USIZE / 256));
        let tail = 12 * FS_USIZE * 2;
        let max_d = out[tail..].iter().zip(&plain[tail..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        let peak = plain[tail..].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(max_d < peak * 0.01, "调回 0 后与从没打开过的输出相差 {max_d}（峰值 {peak}）");
    }

    #[test]
    fn mic_duo_keeps_moving_when_speech_is_sparse_and_stays_opposite() {
        // 一起移动 + 话很少（每说 1 秒停 8 秒）：主播模式的两个声音照常换边，而且始终在头的两侧相对；
        // 严格跟着说话走（试听的做法）时几乎不动
        let voice = crate::dsp::testsig::test_voice(1.0);
        let x: Vec<f32> = (0..FS_USIZE * 54).map(|i| if i % (FS_USIZE * 9) < voice.len() { voice[i % (FS_USIZE * 9)] } else { 0.0 }).collect();
        let base = Arc::new(EngineConfig::new(Params::default(), None));
        let run = |live: bool| {
            let (mut pump, mut tx, shared) = mic_duo_pump(&duo_setup(&base, true, 3840), live);
            let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
            let mut min_gap = 180f32;
            for (j, c) in x.chunks_exact(512).enumerate() {
                tx.push_slice(c);
                pump.fill(&mut l, &mut r);
                let v = shared.voice_telemetry();
                if j > 10 {
                    min_gap = min_gap.min(((v[0][0] - v[1][0] + 180.0).rem_euclid(360.0) - 180.0).abs());
                }
            }
            let moves = if let Proc::Mix(m) = &pump.proc { [m.engine(0).tele.n_moves, m.engine(1).tele.n_moves] } else { [0; 2] };
            (moves, min_gap)
        };
        let (strict, _) = run(false);
        let (live, gap) = run(true);
        println!("每说 1 秒停 8 秒、54 秒：主播模式换边 {live:?} 次（严格跟着说话 {strict:?} 次），两个声音最近时相隔 {gap:.0}°");
        assert_eq!(live[0], live[1], "一起移动：同一套走位");
        assert!(live[0] >= 6 && live[0] >= strict[0] + 3, "主播模式换边 {live:?} 次，严格跟着说话 {strict:?} 次");
        assert!(gap > 150.0, "最近时只相隔 {gap:.0}°");
    }
}
