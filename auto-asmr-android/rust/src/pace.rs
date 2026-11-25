//! 全局模式的输出节拍：把处理好的声音按输出流里的积压量微调速度后交出去，让"我们自己攒着的声音"尽量少。
//!
//! 全局模式是两个时钟之间的一座桥：截下来的声音按系统时钟一阵一阵地来（remote submix），输出流按声卡 / 蓝牙的
//! 时钟取走。中间攒着的声音（输出流里还没放出去的）就是我们多出来的延迟。以前的做法是给输出流定一个上限、
//! 写满就等：音频线程卡一下（输出设备刚启动、系统忙）之后，多出来的声音挪到录音缓冲里，再也回不去；两个时钟
//! 有偏差时也只能等它断流或丢数据；断流一次上限加一截，从不减回去，听久了延迟越来越大。
//!
//! 这里照桌面版实时模式（`auto_asmr::live::Pump`）的做法：
//! - 每次写之前看输出流里还剩多少（[`Pacer::push`] 的 `fill`），取 0.5 秒里的最低点；最低点比目标多，就稍微
//!   快一点（少产出一点样本），比目标少就慢一点，最多 ±0.5%（听不出音调变化）。差一两毫秒以内不动，原样交出去。
//! - 目标 = [`SLACK`] + 多留的量（一开始 5 ms）。断流一次多留 [`STEP`]；之后一直不断，再慢慢减回去（一次性的
//!   卡顿不会把延迟永久垫高），不比一开始留得少。减到上次留得不够的那个量附近就停住，隔很久才往下试一次。
//! - 断流的那一下马上垫静音把积压补回目标（声音反正已经断了一下），不靠慢 0.5% 一点点攒，免得接着连断几次。
//!   输出流刚开始放的头一秒，下游（混音线程、声卡）的缓冲还空着，会一口气取走好几块：这时的断流只垫静音，
//!   不算"留得不够"——不然每次停了再出声都要多留一截。
//! - 积压比目标多出 [`DROP`] 以上：直接丢掉旧的追上实时，之后 5 ms 淡入。
//! - 音频线程被卡住（进程被系统冻了一下）：输出先断流，恢复后卡住期间的声音一下子全到。这时声音已经断过了，
//!   不值得再用几十秒慢慢追：断流后的 2 秒里，一下子到的那一大块只留最新的一阵，多出一阵多就丢，马上回到实时。
//!
//! 引擎的输出不改：这一层只决定"什么时候、以多快的速度交给输出流"，速度不变时样本原样通过（整数延时）。

use auto_asmr::dsp::resample::Resampler;
use auto_asmr::dsp::{FS, FS_USIZE};

/// 一次最多送进来多少帧（调用方一次读到的量不会比它多）。
pub const MAX_FRAMES: usize = 4096;
/// 速度调到最快时输出比输入多出的量，加上插值器攒着的样本：输出缓冲至少比输入多这么多帧。
pub const OUT_PAD: usize = 64;
/// 断流后一次最多垫多少帧静音（100 ms）；输出缓冲再多出这么多帧才垫得下。
pub const PAD_MAX: usize = FS_USIZE / 10;

/// 最低点至少留这么多（样本，3 ms）：音频线程醒来的早晚。
const SLACK: f32 = 0.003 * FS;
/// 一开始多留的量（5 ms）。
const EXTRA_START: f32 = 0.005 * FS;
/// 断流一次多留多少（4 ms），最多留到 [`EXTRA_MAX`]（200 ms）。
const STEP: f32 = 0.004 * FS;
const EXTRA_MAX: f32 = 0.2 * FS;
/// 多久看一次最低点（0.5 秒的输入）。
const WINDOW: usize = FS_USIZE / 2;
/// 最低点比目标多这么多以内（2 ms）、少一半以内（1 ms）不调速度。
const DEADBAND: f32 = 0.002 * FS;
/// 速度最多调这么多（±0.5%）。
const TRIM_MAX: f64 = 0.005;
/// 积压比目标多出这么多（150 ms）就丢掉旧的。
const DROP: f32 = 0.15 * FS;
/// 断流之后、输出流刚开始放的这么久（2 秒的输入）里按更小的量（多出 40 ms）丢：卡住之后一下子到的旧声音不留着慢慢追。
const RECOVER: usize = 2 * FS_USIZE;
const RECOVER_DROP: f32 = 0.04 * FS;
/// 断流后这么久（60 秒的输入）不再断，多留的才开始往回减：每 [`DECAY_EVERY`]（20 秒）减 [`DECAY`]（1 ms）。
const HOLD: u64 = 60 * FS_USIZE as u64;
const DECAY_EVERY: u64 = 20 * FS_USIZE as u64;
const DECAY: f32 = 0.001 * FS;
/// 两次断流隔得比这近（30 秒），或者刚往回减完就断：说明是留得不够，不是偶然卡了一下，记下这个量。
const RECENT: u64 = 30 * FS_USIZE as u64;
/// 减到"上次留得不够的量 + 2 ms"就停住；要再往下试，得隔这么久没断（30 分钟），每试失败一次翻倍，最多 4 小时。
const BAD_MARGIN: f32 = 0.002 * FS;
const PROBE_HOLD: u64 = 30 * 60 * FS_USIZE as u64;
const PROBE_HOLD_MAX: u64 = 4 * 60 * 60 * FS_USIZE as u64;
/// 丢掉旧的之后淡入多少帧（5 ms）。
const FADE: usize = FS_USIZE / 200;
/// 输出流开始放的头这么久（1 秒的输入）里断流不算数：下游的缓冲还空着，会一口气取走好几块。
const GRACE: u64 = FS_USIZE as u64;
/// 多留过一截之后这么久（1 秒）里再断不重复加：先等这一截起作用。
const COOLDOWN: u64 = FS_USIZE as u64;

pub struct Pacer {
    rs: [Resampler; 2],
    l: Vec<f32>,
    r: Vec<f32>,
    ol: Vec<f32>,
    or: Vec<f32>,
    trim: f64,
    /// 在 [`SLACK`] 之外多留的量（样本）。
    extra: f32,
    /// 这一个窗口里写之前积压的最低点，以及窗口里已经送进来多少帧。
    low: f32,
    low_frames: usize,
    /// 输入一阵的量（慢慢衰减的最大值）：开始放时要先攒这么多。
    burst: f32,
    /// 上次看到的断流次数；刚开始 / 输出流重新开始后为空（第一次只记下来）。
    seen_underruns: Option<i32>,
    since_underrun: u64,
    since_decay: u64,
    since_begin: u64,
    since_step: u64,
    /// 留得不够的量（多留到这么多时还断过）；没有时是负数。
    bad: f32,
    probe_hold: u64,
    dropping: bool,
    drops: u32,
    fade: usize,
    /// 断流后的恢复期还剩多少帧。
    recover: usize,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    pub fn new() -> Self {
        let fs = FS as f64;
        let cap = MAX_FRAMES + OUT_PAD;
        Pacer {
            rs: [Resampler::new(fs, fs), Resampler::new(fs, fs)],
            l: Vec::with_capacity(MAX_FRAMES),
            r: Vec::with_capacity(MAX_FRAMES),
            ol: Vec::with_capacity(cap),
            or: Vec::with_capacity(cap),
            trim: 1.0,
            extra: EXTRA_START,
            low: f32::MAX,
            low_frames: 0,
            burst: 1024.0,
            seen_underruns: None,
            // 一开始没有"刚断过 / 刚减过"
            since_underrun: PROBE_HOLD_MAX,
            since_decay: PROBE_HOLD_MAX,
            since_begin: 0,
            since_step: PROBE_HOLD_MAX,
            bad: -1.0,
            probe_hold: PROBE_HOLD,
            dropping: false,
            drops: 0,
            fade: 0,
            recover: 0,
        }
    }

    /// 写之前输出流里至少要剩多少（样本）。
    pub fn target(&self) -> f32 {
        SLACK + self.extra
    }

    /// 输出流（重新）开始放：第一块是 `first` 帧时，前面要先垫多少帧静音，放起来之后积压的最低点才正好在目标上。
    /// 同时忘掉之前看到的断流次数（新的输出流从头数）和调过的速度。
    pub fn begin(&mut self, first: usize) -> usize {
        self.seen_underruns = None;
        self.set_trim(1.0);
        self.low = f32::MAX;
        self.low_frames = 0;
        self.dropping = false;
        // 输出设备启动得慢（蓝牙耳机要几百毫秒）时，等它的这段时间里来的声音会一下子全到：和断流后一样处理，不留着慢慢追
        self.recover = RECOVER;
        self.since_begin = 0;
        (self.target() + (self.burst - first as f32).max(0.0)).round() as usize
    }

    /// 开始放之前输出流里要攒够多少帧（垫的静音 + 第一块）。
    pub fn start_fill(&self) -> usize {
        (self.target() + self.burst).round() as usize
    }

    /// 送进一块处理好的交错立体声（`data.len() / 2` 帧，不超过 [`MAX_FRAMES`]）。`fill` 是写这一块之前输出流里
    /// 还没放出去的帧数，`underruns` 是输出流累计的断流次数。要写进输出流的帧写到 `out`（至少比输入多 [`OUT_PAD`] 帧；
    /// 再多出 [`PAD_MAX`] 帧，断流后才垫得下静音），返回帧数；0 = 这一块丢掉（正在追上实时）。
    pub fn push(&mut self, data: &[f32], fill: i32, underruns: i32, out: &mut [f32]) -> usize {
        let n = (data.len() / 2).min(MAX_FRAMES);
        if n == 0 {
            return 0;
        }
        let fill = fill.max(0) as f32;
        self.since_decay += n as u64;
        self.since_begin += n as u64;
        self.since_step += n as u64;

        let underrun = matches!(self.seen_underruns, Some(seen) if underruns > seen);
        if underrun {
            self.on_underrun();
        }
        self.since_underrun += n as u64;
        self.seen_underruns = Some(underruns);
        let target = self.target();

        // 输入一阵的量：平时跟着最大的走（慢慢衰减）；偶尔一大块只算四分之一，恢复期里的不算（那是卡住之后攒的）
        let mut skip = 0;
        if underrun {
            self.recover = RECOVER;
            // 断流的同时来了一大块：卡住期间攒的，只留最新的一阵
            let keep = self.burst as usize;
            if fill < target && n > keep + keep / 2 {
                skip = n - keep;
                self.cut();
            }
        } else if self.recover > 0 {
            self.recover = self.recover.saturating_sub(n);
        } else if n as f32 > self.burst {
            self.burst += 0.25 * (n as f32 - self.burst);
        } else {
            self.burst = (self.burst * 0.999).max(n as f32);
        }

        // 积压太多：丢掉旧的（这一块不写），直到输出流里的量回到"目标 + 一阵"
        let limit = if self.recover > 0 { RECOVER_DROP } else { DROP };
        if !self.dropping && fill > target + self.burst + limit {
            self.dropping = true;
            self.drops += 1;
        }
        if self.dropping {
            if fill > target + self.burst {
                return 0;
            }
            self.dropping = false;
            self.cut();
        }

        if !underrun {
            self.low = self.low.min(fill);
            self.low_frames += n;
            if self.low_frames >= WINDOW {
                self.end_window();
            }
        }

        self.l.clear();
        self.r.clear();
        for f in data[skip * 2..n * 2].chunks_exact(2) {
            self.l.push(f[0]);
            self.r.push(f[1]);
        }
        self.ol.clear();
        self.or.clear();
        self.rs[0].process(&self.l, &mut self.ol);
        self.rs[1].process(&self.r, &mut self.or);
        let m = self.ol.len().min(self.or.len()).min(out.len() / 2);
        // 刚断过：垫静音把积压补回目标（这一块写进去之后，到下一阵来之前还剩目标那么多）
        let pad = if underrun { ((self.target() - fill).max(0.0) as usize).min(PAD_MAX).min(out.len() / 2 - m) } else { 0 };
        out[..pad * 2].fill(0.0);
        for (i, f) in out[pad * 2..(pad + m) * 2].chunks_exact_mut(2).enumerate() {
            let g = if self.fade > 0 {
                self.fade -= 1;
                1.0 - self.fade as f32 / FADE as f32
            } else {
                1.0
            };
            f[0] = self.ol[i] * g;
            f[1] = self.or[i] * g;
        }
        pad + m
    }

    /// 声音接不上了（丢过一段）：清掉插值历史，接下来的 5 ms 淡入，速度和窗口从头来。
    fn cut(&mut self) {
        self.fade = FADE;
        for r in self.rs.iter_mut() {
            r.reset();
        }
        self.set_trim(1.0);
        self.low = f32::MAX;
        self.low_frames = 0;
    }

    fn set_trim(&mut self, trim: f64) {
        if trim != self.trim {
            self.trim = trim;
            let fs = FS as f64;
            for r in self.rs.iter_mut() {
                r.set_trim(fs, fs, trim);
            }
        }
    }

    /// 断流了：多留一截。两次断流隔得近、或者刚往回减完就断，说明是留得不够，记下这个量（以后减到它附近就停住）。
    /// 刚开始放的头一秒、刚多留过一截的一秒里不算（调用方照样垫静音补回目标）。
    fn on_underrun(&mut self) {
        self.low = f32::MAX;
        self.low_frames = 0;
        if self.since_begin < GRACE || self.since_step < COOLDOWN {
            return;
        }
        self.since_step = 0;
        let before = self.extra;
        if self.since_underrun < RECENT || self.since_decay < RECENT {
            if self.bad >= 0.0 && before < self.bad + BAD_MARGIN {
                // 往下试（比上次留得不够的量还少）又断了：下次隔更久再试
                self.probe_hold = (self.probe_hold * 2).min(PROBE_HOLD_MAX);
            }
            self.bad = self.bad.max(before);
        }
        self.extra = (before + STEP).min(EXTRA_MAX);
        self.since_underrun = 0;
    }

    /// 一个窗口结束：按最低点调速度；很久没断流时把多留的往回减。
    fn end_window(&mut self) {
        let err = self.low - self.target();
        // 偏少时更早动手（见底就断流），偏多只是多一点延迟
        let err = if err > DEADBAND {
            err - DEADBAND
        } else if err < -DEADBAND / 2.0 {
            err + DEADBAND / 2.0
        } else {
            0.0
        };
        // 每个窗口（0.5 秒）追回差的四分之一
        self.set_trim(1.0 + ((err / FS) as f64 * 0.5).clamp(-TRIM_MAX, TRIM_MAX));
        self.low = f32::MAX;
        self.low_frames = 0;

        if self.since_underrun >= HOLD && self.since_decay >= DECAY_EVERY {
            // 不比一开始留得少；留得不够的量附近停住，隔很久没断才往下试
            let floor = if self.bad >= 0.0 && self.since_underrun < self.probe_hold { self.bad + BAD_MARGIN } else { 0.0 };
            let floor = floor.max(EXTRA_START);
            if self.extra > floor {
                self.extra = (self.extra - DECAY).max(floor);
                self.since_decay = 0;
            }
        }
    }

    /// 状态：out[0] 目标（写之前至少剩多少，毫秒）、[1] 速度偏差（ppm，正 = 在追）、[2] 丢掉旧声音的次数、
    /// [3] 这一层自己的延时（毫秒，插值器的半宽）。
    pub fn stats(&self) -> [f32; 4] {
        [self.target() / FS * 1000.0, ((self.trim - 1.0) * 1e6) as f32, self.drops as f32, LATENCY as f32 / FS * 1000.0]
    }
}

/// 这一层的延时（样本）：插值器要等够半个核宽的"未来"样本。
pub const LATENCY: usize = 15;

#[cfg(test)]
mod tests {
    use super::*;

    /// 模拟的输出流：缓冲里有 `fill` 帧，输出设备每 `gulp` 帧取一次（声卡时钟比系统时钟快 `drift`）。
    /// `need_full`：普通混音线程，不够一整块就算断流（把剩下的放完）；否则是快速通道，取空了才算。
    struct Sim {
        p: Pacer,
        fill: f64,
        underruns: i32,
        starving: bool,
        gulp: usize,
        need_full: bool,
        drift: f64,
        burst: usize,
        /// 系统时钟（秒）；下一阵输入、下一次取走的时刻。
        t: f64,
        next_in: f64,
        next_out: f64,
        started: bool,
        seed: u32,
        /// 每次写之前的积压（毫秒）和当时的时刻。
        log: Vec<(f64, f32)>,
        written: u64,
        zero_pushes: u32,
        in_phase: f64,
    }

    impl Sim {
        fn new(gulp: usize, need_full: bool, drift: f64) -> Self {
            Sim { p: Pacer::new(), fill: 0.0, underruns: 0, starving: false, gulp, need_full, drift, burst: 1024, t: 0.0, next_in: 0.0, next_out: 0.0, started: false, seed: 7, log: Vec::new(), written: 0, zero_pushes: 0, in_phase: 0.0 }
        }

        fn rnd(&mut self) -> f64 {
            self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.seed >> 8) as f64 / (1u32 << 24) as f64
        }

        fn take(&mut self) {
            let g = self.gulp as f64;
            let short = if self.need_full { self.fill < g } else { self.fill <= 0.0 };
            if short {
                if !self.starving {
                    self.underruns += 1;
                    self.starving = true;
                }
                self.fill = 0.0;
            } else {
                self.starving = false;
                self.fill = (self.fill - g).max(0.0);
            }
        }

        fn feed(&mut self, frames: usize) {
            let mut data = vec![0f32; frames * 2];
            for (i, f) in data.chunks_exact_mut(2).enumerate() {
                let v = (self.in_phase + i as f64 * 0.05).sin() as f32 * 0.3;
                f[0] = v;
                f[1] = v;
            }
            self.in_phase += frames as f64 * 0.05;
            let mut out = vec![0f32; (frames + OUT_PAD + PAD_MAX) * 2];
            if !self.started {
                self.fill += self.p.begin(frames) as f64;
                self.started = true;
            }
            self.log.push((self.t, self.fill as f32 / 48.0));
            let m = self.p.push(&data, self.fill as i32, self.underruns, &mut out);
            if m == 0 {
                self.zero_pushes += 1;
            }
            self.written += m as u64;
            self.fill += m as f64;
        }

        /// 跑到 `until` 秒。输入每一阵晚 0–2 ms 不等。
        fn run(&mut self, until: f64) {
            while self.t < until {
                if self.next_in <= self.next_out {
                    self.t = self.next_in;
                    let late = self.rnd() * 0.002;
                    self.t += late;
                    self.feed(self.burst);
                    self.next_in += self.burst as f64 / 48_000.0;
                } else {
                    self.t = self.next_out;
                    if self.started {
                        self.take();
                    }
                    self.next_out += self.gulp as f64 / (48_000.0 * (1.0 + self.drift));
                }
            }
        }

        /// [a, b) 秒之间写之前积压的最低点和平均（毫秒）。
        fn fill_between(&self, a: f64, b: f64) -> (f32, f32) {
            let v: Vec<f32> = self.log.iter().filter(|(t, _)| *t >= a && *t < b).map(|x| x.1).collect();
            (v.iter().fold(f32::MAX, |m, x| m.min(*x)), v.iter().sum::<f32>() / v.len().max(1) as f32)
        }
    }

    /// 快速通道（每 4 ms 取一次）、两边时钟一样：不断流，写之前的积压守在目标附近（8 ms 上下），不越攒越多。
    #[test]
    fn steady_stream_sits_on_the_target() {
        let mut s = Sim::new(192, false, 0.0);
        s.run(60.0);
        assert_eq!(s.underruns, 0, "不该断流");
        let (low, avg) = s.fill_between(20.0, 60.0);
        assert!(low > 2.0 && avg < 16.0, "最低 {low} ms，平均 {avg} ms");
        assert_eq!(s.zero_pushes, 0);
    }

    /// 声卡时钟比系统时钟快 / 慢 0.03%（比真实的偏差大）：跑 5 分钟，稳定后不断流，积压既不见底也不越来越多。
    #[test]
    fn clock_drift_is_followed_without_dropouts() {
        for drift in [3e-4, -3e-4] {
            let mut s = Sim::new(192, false, drift);
            s.run(30.0);
            let before = s.underruns;
            s.run(300.0);
            assert_eq!(s.underruns, before, "漂移 {drift}：稳定后又断了");
            let (low, avg) = s.fill_between(200.0, 300.0);
            assert!(low > 1.0 && avg < 20.0, "漂移 {drift}：最低 {low} ms，平均 {avg} ms");
            // 写出去的量跟上了声卡的速度（差不到 0.01%）
            let want = 300.0 * 48_000.0 * (1.0 + drift);
            assert!((s.written as f64 / want - 1.0).abs() < 1e-4, "漂移 {drift}：写了 {}，应约 {want}", s.written);
            assert_eq!(s.zero_pushes, 0, "漂移 {drift}：不该丢声音");
        }
    }

    /// 普通混音线程（每 20 ms 取一整块，不够一整块就断）：开头断几次、多留一些之后不再断；多留的不会超过需要的太多。
    #[test]
    fn a_mixer_that_takes_whole_blocks_gets_enough_headroom() {
        let mut s = Sim::new(960, true, 0.0);
        s.run(20.0);
        let early = s.underruns;
        assert!(early <= 8, "开头断了 {early} 次");
        s.run(50.0);
        assert_eq!(s.underruns, early, "多留之后不该再断");
        let (_, avg) = s.fill_between(30.0, 50.0);
        assert!(avg < 50.0, "平均积压 {avg} ms");
    }

    /// 输出设备卡了 60 ms（刚启动、切换设备）：这段时间来的声音都压在输出流里。不丢声音，靠快 0.5% 追回来，
    /// 20 秒后积压回到原来的水平（以前会一直多着）。
    #[test]
    fn backlog_after_a_stall_is_worked_off() {
        let mut s = Sim::new(192, false, 0.0);
        s.run(20.0);
        let (_, before) = s.fill_between(10.0, 20.0);
        s.next_out += 0.06;
        s.run(21.0);
        let (_, during) = s.fill_between(20.2, 21.0);
        assert!(during > before + 40.0, "卡住之后应多出约 60 ms：{before} → {during}");
        s.run(45.0);
        let (_, after) = s.fill_between(40.0, 45.0);
        assert!(after < before + 4.0, "20 秒后还多着：{before} → {after}");
        assert_eq!(s.zero_pushes, 0, "不该丢声音");
        assert_eq!(s.underruns, 0);
    }

    /// 服务进程被冻住 0.3 秒：输出断流，恢复后先到一大块（录音缓冲里攒的 64 ms），接着卡住期间的声音每 5 ms 一块
    /// 地涌进来。不留着慢慢追：2 秒后积压就回到原来的水平（实测旧做法会一直多 160 ms 以上）。
    #[test]
    fn a_frozen_process_catches_up_at_once() {
        let mut s = Sim::new(192, false, 0.0);
        s.run(20.0);
        let (_, before) = s.fill_between(10.0, 20.0);
        let target = s.p.target();
        // 冻住：0.3 秒没有输入，设备照常取
        s.next_in += 0.3;
        s.run(20.3);
        assert_eq!(s.underruns, 1);
        // 恢复：一大块，然后 10 块挤着来
        s.t = s.next_in;
        s.feed(3072);
        for k in 1..=10 {
            while s.next_out < s.next_in + k as f64 * 0.005 {
                s.t = s.next_out;
                s.take();
                s.next_out += s.gulp as f64 / 48_000.0;
            }
            s.t = s.next_in + k as f64 * 0.005;
            s.feed(1024);
        }
        s.next_in = s.t + 1024.0 / 48_000.0;
        s.run(23.0);
        let (low, after) = s.fill_between(22.0, 23.0);
        assert!(after < before + 8.0 && low > 1.0, "冻住之后 2 秒：{before} → {after} ms（最低 {low}）");
        assert!(s.p.target() <= target + STEP + 1.0, "只多留一截");
        s.run(40.0);
        assert_eq!(s.underruns, 1, "之后不该再断");
        let (_, later) = s.fill_between(35.0, 40.0);
        // 断过一次，多留了 4 ms（一分钟后开始减回去）
        assert!(later < before + 8.0, "{before} → {later} ms");
    }

    /// 输出设备启动得慢（蓝牙耳机）：开始放之后 0.4 秒它才开始取，这期间来的声音都压在输出流里。不留着慢慢追，
    /// 2 秒后积压回到目标附近；目标不变。
    #[test]
    fn a_slow_device_start_does_not_leave_a_backlog() {
        let mut s = Sim::new(192, false, 0.0);
        let t0 = s.p.target();
        s.next_out = 0.4;
        s.run(3.0);
        let (low, avg) = s.fill_between(2.0, 3.0);
        assert!(low > 1.0 && avg < 35.0, "开始 2 秒后：最低 {low} ms，平均 {avg} ms");
        assert_eq!(s.p.target(), t0);
        assert!(s.zero_pushes > 5 && s.p.stats()[2] == 1.0, "多出来的应当丢掉");
        s.run(20.0);
        let (_, later) = s.fill_between(15.0, 20.0);
        assert!(later < 16.0, "{later} ms");
        assert_eq!(s.underruns, 0);
    }

    /// 积压一下子多出 300 ms（卡了很久）：丢掉旧的直到回到目标附近，记一次，之后照常。
    #[test]
    fn a_flood_is_dropped() {
        let mut s = Sim::new(192, false, 0.0);
        s.run(10.0);
        s.fill += 0.3 * 48_000.0;
        s.run(12.0);
        assert!(s.zero_pushes > 5, "应当丢掉一些块");
        assert_eq!(s.p.stats()[2], 1.0);
        let (low, avg) = s.fill_between(11.0, 12.0);
        assert!(low > 1.0 && avg < 40.0, "丢完之后：最低 {low} ms，平均 {avg} ms");
        let z = s.zero_pushes;
        s.run(20.0);
        assert_eq!(s.zero_pushes, z, "回到目标后不该再丢");
    }

    /// 偶然卡了一下断流一次：多留 4 ms；之后一直不断，一分钟后开始减，回到原来的量（以前只加不减）。
    #[test]
    fn headroom_added_for_a_one_off_hiccup_goes_away() {
        let mut s = Sim::new(192, false, 0.0);
        s.run(120.0);
        let t0 = s.p.target();
        s.fill = 0.0;
        s.next_out = s.t; // 马上取一次：取空，断流
        s.run(125.0);
        assert_eq!(s.underruns, 1);
        assert!((s.p.target() - t0 - STEP).abs() < 1.0, "断流后应多留 4 ms");
        s.run(125.0 + 60.0 + 100.0);
        assert!((s.p.target() - t0).abs() < 1.0, "多留的应当减回去：{} → {}", t0, s.p.target());
        assert_eq!(s.underruns, 1, "减回去的过程中不该再断");
    }

    /// 输出流刚开始放时下游的缓冲是空的，设备一口气取走 40 ms：这时断的不算"留得不够"，目标不变；
    /// 垫上静音之后积压回到目标，之后不再断。停了再出声（重新开始）也一样，不会每次多留一截。
    #[test]
    fn the_device_filling_up_at_start_does_not_raise_the_target() {
        let mut s = Sim::new(480, false, 0.0);
        let t0 = s.p.target();
        for round in 0..5 {
            s.started = false; // 输出流清空后重新开始
            s.fill = 0.0;
            s.starving = false;
            let from = s.t;
            s.run(from + 0.001);
            for _ in 0..4 {
                s.take();
            }
            s.run(from + 2.0);
            let early = s.underruns;
            s.run(from + 20.0);
            assert_eq!(s.underruns, early, "第 {round} 次开始：头两秒之后不该再断");
            assert_eq!(s.p.target(), t0, "第 {round} 次开始：目标不该变");
            let (low, avg) = s.fill_between(from + 5.0, from + 20.0);
            assert!(low > 2.0 && avg < 20.0, "第 {round} 次开始：最低 {low} ms，平均 {avg} ms");
        }
    }

    /// 留得不够（减到某个量就断）：记住这个量，减到它上面 2 ms 就停住，不来回试。
    #[test]
    fn it_stops_shrinking_above_the_level_that_failed() {
        let mut s = Sim::new(960, true, 0.0);
        s.run(30.0);
        let settled = s.underruns;
        s.run(30.0 + 20.0 * 60.0);
        assert!(s.underruns <= settled + 1, "20 分钟里又断了 {} 次", s.underruns - settled);
    }

    /// 速度不变时样本原样通过（只晚 15 个样本）；调到最快时还是同一个正弦（幅度不变）。
    #[test]
    fn audio_passes_through_untouched_at_unity_speed() {
        let mut p = Pacer::new();
        let x: Vec<f32> = (0..4800).flat_map(|i| {
            let v = (i as f32 * 0.13).sin() * 0.5;
            [v, -v]
        }).collect();
        let mut out = vec![0f32; (2400 + OUT_PAD) * 2];
        let mut y = Vec::new();
        p.begin(2400);
        for c in x.chunks(2 * 2400) {
            let m = p.push(c, 400, 0, &mut out);
            y.extend_from_slice(&out[..m * 2]);
        }
        let err = (0..4000).map(|i| (y[2 * i] - x[2 * i]).abs().max((y[2 * i + 1] - x[2 * i + 1]).abs())).fold(0f32, f32::max);
        assert!(err < 1e-4, "原样通过时差了 {err}");

        let mut p = Pacer::new();
        p.begin(1024);
        let w = 2.0 * std::f32::consts::PI * 1000.0 / 48_000.0;
        let mut y = Vec::new();
        let mut k = 0usize;
        // 积压一直比目标多 30 ms：速度调到最快（+0.5%）
        for _ in 0..200 {
            let c: Vec<f32> = (0..1024).flat_map(|i| {
                let v = ((k + i) as f32 * w).sin() * 0.5;
                [v, v]
            }).collect();
            k += 1024;
            let m = p.push(&c, p.target() as i32 + 1440, 0, &mut out);
            y.extend_from_slice(&out[..m * 2]);
        }
        assert!(p.stats()[1] > 4900.0, "应调到最快：{} ppm", p.stats()[1]);
        let tail: Vec<f32> = y[y.len() / 2..].iter().step_by(2).copied().collect();
        let pk = tail.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((pk - 0.5).abs() < 0.005, "幅度 {pk}");
        let zc = tail.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32;
        let hz = zc / (tail.len() as f32 / 48_000.0);
        assert!((hz - 1005.0).abs() < 3.0, "快 0.5% 时 1 kHz 应变成约 1005 Hz：{hz}");
    }
}
