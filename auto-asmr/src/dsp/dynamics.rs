//! 响度与峰值：BS.1770 响度计、实时自动响度、带真峰值检测的前瞻限制器。

use super::filters::{ema_coef, Biquad, SincTable};
use super::{db_to_lin, FS, FS_USIZE};
use std::collections::VecDeque;

/// BS.1770 的 K 加权（48 kHz 系数）。
#[derive(Clone, Copy)]
pub struct KWeight {
    s1: Biquad,
    s2: Biquad,
}

impl KWeight {
    pub fn new() -> Self {
        KWeight {
            s1: Biquad::from_coeffs([1.535_124_9, -2.691_696_2, 1.198_392_8], [1.0, -1.690_659_3, 0.732_480_77]),
            s2: Biquad::from_coeffs([1.0, -2.0, 1.0], [1.0, -1.990_047_5, 0.990_072_25]),
        }
    }
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.s2.process(self.s1.process(x))
    }

    /// 频率 `f` 处的功率增益（背景音按期望功率定响度时用）。
    pub fn mag2(&self, f: f32) -> f32 {
        self.s1.mag2(f) * self.s2.mag2(f)
    }
}

/// ITU-R BS.1770-4 积分响度（绝对门限 −70 LUFS，相对门限 −10 LU）。
pub struct LoudnessMeter {
    kw: [KWeight; 2],
    acc: f64,
    n: usize,
    subs: Vec<f32>, // 每 100 ms 的两声道 K 加权均方之和
}

impl LoudnessMeter {
    pub fn new() -> Self {
        LoudnessMeter { kw: [KWeight::new(); 2], acc: 0.0, n: 0, subs: Vec::new() }
    }

    #[inline]
    pub fn push(&mut self, l: f32, r: f32) {
        let (a, b) = (self.kw[0].process(l), self.kw[1].process(r));
        self.acc += (a * a + b * b) as f64;
        self.n += 1;
        if self.n == FS_USIZE / 10 {
            self.subs.push((self.acc / self.n as f64) as f32);
            self.acc = 0.0;
            self.n = 0;
        }
    }

    pub fn integrated(&self) -> f32 {
        self.integrated_scaled(1.0)
    }

    /// 把整段乘上多少 dB 的增益，积分响度正好等于 `target`。
    /// 绝对门限（−70 LUFS）让响度读数与电平不完全成正比，所以迭代几次。
    pub fn gain_db_for(&self, target: f32) -> f32 {
        let mut g = 0.0f32;
        for _ in 0..6 {
            let l = self.integrated_scaled(10f64.powf(g as f64 / 10.0));
            if l <= -69.9 {
                break;
            }
            g += target - l;
        }
        g
    }

    /// 功率乘 `pscale` 之后的积分响度。
    fn integrated_scaled(&self, pscale: f64) -> f32 {
        if self.subs.len() < 4 {
            return -70.0;
        }
        let blk: Vec<f64> = self.subs.windows(4).map(|w| pscale * w.iter().map(|&v| v as f64).sum::<f64>() / 4.0).collect();
        let lk = |p: f64| -0.691 + 10.0 * (p + 1e-20).log10();
        let g1: Vec<f64> = blk.iter().cloned().filter(|&p| lk(p) > -70.0).collect();
        if g1.is_empty() {
            return -70.0;
        }
        let gate = lk(g1.iter().sum::<f64>() / g1.len() as f64) - 10.0;
        let g2: Vec<f64> = g1.into_iter().filter(|&p| lk(p) > gate).collect();
        if g2.is_empty() {
            return -70.0;
        }
        lk(g2.iter().sum::<f64>() / g2.len() as f64) as f32
    }
}

/// 实时自动响度：只在说话时测量短时响度（约 3 s），慢慢把输出拉到目标（每秒最多改 2 dB，范围 −12…+18 dB）。
pub struct Leveler {
    kw: [KWeight; 2],
    st: f32,
    heard: f32,
    pub gain_db: f32,
}

impl Leveler {
    pub fn new() -> Self {
        Leveler { kw: [KWeight::new(); 2], st: 0.0, heard: 0.0, gain_db: 0.0 }
    }

    /// 输入处理前的样本，返回应乘的线性增益。`speaking`：此刻是否在说话。
    #[inline]
    pub fn process(&mut self, l: f32, r: f32, speaking: bool, target_lufs: f32) -> f32 {
        let dt = 1.0 / FS;
        let (a, b) = (self.kw[0].process(l), self.kw[1].process(r));
        if speaking {
            let p = a * a + b * b;
            // 刚开始时积累得快一些
            let tau = if self.heard < 1.0 { 0.4 } else { 3.0 };
            self.st += ema_coef(tau, dt) * (p - self.st);
            self.heard += dt;
            if self.heard > 0.4 {
                let lufs = -0.691 + 10.0 * (self.st + 1e-12).log10();
                let want = (target_lufs - lufs).clamp(-12.0, 18.0);
                let step = 2.0 * dt;
                self.gain_db += (want - self.gain_db).clamp(-step, step);
            }
        }
        db_to_lin(self.gain_db)
    }
}

const TP_TAPS: usize = 12;
/// 真峰值检测要往后看的样本数。
const TP_LOOK: usize = TP_TAPS / 2;

/// 前瞻峰值限制器（两声道联动，真峰值检测）：
/// 所需增益 → 带慢恢复的保持 → 滑动最小值 → 滑动平均，增益在峰值到来时一定不高于所需值，没有过冲。
pub struct Limiter {
    look: usize,
    dl: [Vec<f32>; 2],
    dpos: usize,
    hist: [Vec<f32>; 2],
    hpos: usize,
    tp: SincTable,
    held: f32,
    rel: f32,
    minq: VecDeque<(f32, u64)>,
    idx: u64,
    box_buf: Vec<f32>,
    box_pos: usize,
    box_sum: f64,
    /// 当前增益衰减量（dB，≥ 0），给电平表用。
    pub gr_db: f32,
    pub max_gr_db: f32,
}

impl Limiter {
    /// `look_ms`：前瞻时间；`release_ms`：恢复时间常数（48 kHz）。
    pub fn new(look_ms: f32, release_ms: f32) -> Self {
        Self::with_rate(look_ms, release_ms, FS)
    }

    /// 同 [`Limiter::new`]，用在采样率是 `rate` 的信号上。
    pub fn with_rate(look_ms: f32, release_ms: f32, rate: f32) -> Self {
        let look = ((look_ms / 1000.0 * rate) as usize).max(8);
        let delay = look - 1 + TP_LOOK;
        Limiter {
            look,
            dl: [vec![0.0; delay], vec![0.0; delay]],
            dpos: 0,
            hist: [vec![0.0; 2 * TP_TAPS], vec![0.0; 2 * TP_TAPS]],
            hpos: 0,
            tp: SincTable::new(TP_TAPS, 4, 6.0, 1.0),
            held: 1.0,
            rel: (-1.0 / (release_ms / 1000.0 * rate)).exp(),
            minq: VecDeque::with_capacity(look + 2),
            idx: 0,
            box_buf: vec![1.0; look],
            box_pos: 0,
            box_sum: look as f64,
            gr_db: 0.0,
            max_gr_db: 0.0,
        }
    }

    /// 限制器带来的延时（样本）。
    pub fn delay(&self) -> usize {
        self.look - 1 + TP_LOOK
    }

    /// 回到刚建好时的状态（清空延时线，不压限），不分配内存。
    pub fn reset(&mut self) {
        for v in self.dl.iter_mut().chain(self.hist.iter_mut()) {
            v.fill(0.0);
        }
        self.dpos = 0;
        self.hpos = 0;
        self.held = 1.0;
        self.minq.clear();
        self.idx = 0;
        self.box_buf.fill(1.0);
        self.box_pos = 0;
        self.box_sum = self.look as f64;
        self.gr_db = 0.0;
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32, ceiling: f32) -> (f32, f32) {
        // 真峰值：样本本身和它与前一个样本之间 3 个 4 倍插值点
        self.hpos = if self.hpos == 0 { TP_TAPS - 1 } else { self.hpos - 1 };
        let mut pk = 0f32;
        for (c, &x) in [l, r].iter().enumerate() {
            let h = &mut self.hist[c];
            h[self.hpos] = x;
            h[self.hpos + TP_TAPS] = x;
            let s = &h[self.hpos..self.hpos + TP_TAPS];
            pk = pk.max(s[TP_LOOK].abs()); // 中心样本
            for f in [0.25f32, 0.5, 0.75] {
                pk = pk.max(self.tp.interp(s, f).abs());
            }
        }
        let req = if pk > ceiling { ceiling / pk } else { 1.0 };
        // 保持并慢慢恢复
        self.held = req.min(1.0 - (1.0 - self.held) * self.rel);
        // 滑动最小值（单调队列）
        while let Some(&(v, _)) = self.minq.back() {
            if v >= self.held {
                self.minq.pop_back();
            } else {
                break;
            }
        }
        self.minq.push_back((self.held, self.idx));
        while let Some(&(_, i)) = self.minq.front() {
            if i + self.look as u64 <= self.idx {
                self.minq.pop_front();
            } else {
                break;
            }
        }
        self.idx += 1;
        let m = self.minq.front().map_or(1.0, |v| v.0);
        // 滑动平均
        self.box_sum += (m - self.box_buf[self.box_pos]) as f64;
        self.box_buf[self.box_pos] = m;
        self.box_pos = (self.box_pos + 1) % self.look;
        let g = ((self.box_sum / self.look as f64) as f32).min(1.0);
        self.gr_db = -20.0 * g.max(1e-6).log10();
        if self.gr_db > self.max_gr_db {
            self.max_gr_db = self.gr_db;
        }
        // 延时后的信号乘增益
        let n = self.dl[0].len();
        let (ol, or) = (self.dl[0][self.dpos], self.dl[1][self.dpos]);
        self.dl[0][self.dpos] = l;
        self.dl[1][self.dpos] = r;
        self.dpos = (self.dpos + 1) % n;
        (ol * g, or * g)
    }
}

/// 监听原声时的输出级：原声不经过引擎（没有空间效果、自动响度、背景音），但仍乘「输出增益」、过峰值限制器
/// （和效果那一路的收尾一样），所以切到原声时音量滑条照样起作用，调大了也不会削波。
/// 增益变化在一次调用之内线性过渡，拖滑条没有咔哒声。限制器带来约 2 ms 的延时（[`DryOut::delay`]）。
pub struct DryOut {
    lim: Limiter,
    /// 上一次用的线性增益；刚建好 / 刚复位时为 None（第一块直接用目标值，不从 0 淡入）。
    g: Option<f32>,
}

impl DryOut {
    /// `rate`：信号的采样率。
    pub fn new(rate: f32) -> Self {
        DryOut { lim: Limiter::with_rate(2.0, 80.0, rate), g: None }
    }

    /// 带来的延时（样本）。
    pub fn delay(&self) -> usize {
        self.lim.delay()
    }

    /// 从效果切回原声时调用：清掉上次留在限制器里的旧声音。
    pub fn reset(&mut self) {
        self.lim.reset();
        self.g = None;
    }

    fn gains(&mut self, n: usize, gain_db: f32) -> (f32, f32) {
        let target = db_to_lin(gain_db);
        let g0 = self.g.unwrap_or(target);
        self.g = Some(target);
        (g0, if n > 0 { (target - g0) / n as f32 } else { 0.0 })
    }

    /// 原地处理左右两路（等长）。`gain_db`、`ceiling_db` 即参数里的输出增益、峰值上限。
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], gain_db: f32, ceiling_db: f32) {
        let n = l.len().min(r.len());
        let (g0, step) = self.gains(n, gain_db);
        let ceiling = db_to_lin(ceiling_db - 0.1);
        for i in 0..n {
            let g = g0 + step * (i + 1) as f32;
            (l[i], r[i]) = self.lim.process(l[i] * g, r[i] * g, ceiling);
        }
    }

    /// 同 [`DryOut::process`]，原地处理交错的立体声。
    pub fn process_interleaved(&mut self, x: &mut [f32], gain_db: f32, ceiling_db: f32) {
        let n = x.len() / 2;
        let (g0, step) = self.gains(n, gain_db);
        let ceiling = db_to_lin(ceiling_db - 0.1);
        for i in 0..n {
            let g = g0 + step * (i + 1) as f32;
            (x[2 * i], x[2 * i + 1]) = self.lim.process(x[2 * i] * g, x[2 * i + 1] * g, ceiling);
        }
    }
}

/// 4 倍过采样估计真峰值（文件模式最后核对用）。
pub struct TruePeak {
    hist: [Vec<f32>; 2],
    pos: usize,
    tp: SincTable,
    pub peak: f32,
}

impl TruePeak {
    pub fn new() -> Self {
        TruePeak { hist: [vec![0.0; 2 * TP_TAPS], vec![0.0; 2 * TP_TAPS]], pos: 0, tp: SincTable::new(TP_TAPS, 4, 6.0, 1.0), peak: 0.0 }
    }
    #[inline]
    pub fn push(&mut self, l: f32, r: f32) {
        self.pos = if self.pos == 0 { TP_TAPS - 1 } else { self.pos - 1 };
        for (c, &x) in [l, r].iter().enumerate() {
            let h = &mut self.hist[c];
            h[self.pos] = x;
            h[self.pos + TP_TAPS] = x;
            let s = &h[self.pos..self.pos + TP_TAPS];
            self.peak = self.peak.max(x.abs());
            for f in [0.25f32, 0.5, 0.75] {
                self.peak = self.peak.max(self.tp.interp(s, f).abs());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudness_of_sine_matches_bs1770() {
        // 997 Hz、0 dBFS 正弦、两声道同相：BS.1770 规定读数为 −3.01 + 3.01 ≈ 0 LUFS 附近（单声道 −3.01 LUFS）
        let mut m = LoudnessMeter::new();
        let w = 2.0 * std::f32::consts::PI * 997.0 / FS;
        for n in 0..(5 * FS_USIZE) {
            let x = (w * n as f32).sin();
            m.push(x, 0.0);
        }
        assert!((m.integrated() + 3.01).abs() < 0.1, "{}", m.integrated());
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let mut lim = Limiter::new(2.0, 80.0);
        let mut tp = TruePeak::new();
        let mut seed = 7u32;
        for n in 0..96000 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            let burst = if (n / 4000) % 2 == 1 { 3.0 } else { 0.4 };
            let x = burst * (noise + (n as f32 * 0.05).sin());
            let (l, r) = lim.process(x, -x * 0.7, 0.89);
            tp.push(l, r);
        }
        assert!(tp.peak <= 0.89 * 1.02, "true peak {}", tp.peak);
        assert!(lim.max_gr_db > 6.0);
    }

    #[test]
    fn dry_out_applies_gain_and_ceiling() {
        let sine = |amp: f32, rate: f32, n: usize| -> Vec<f32> { (0..n).map(|i| amp * (i as f32 * std::f32::consts::TAU * 440.0 / rate).sin()).collect() };
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        for rate in [48_000.0f32, 44_100.0] {
            let n = rate as usize;
            // 0 dB、没超过峰值上限：原样通过，只晚 delay() 个样本
            let x = sine(0.3, rate, n);
            let mut d = DryOut::new(rate);
            let (mut l, mut r) = (x.clone(), x.clone());
            for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
                d.process(a, b, 0.0, -1.0);
            }
            let k = d.delay();
            assert!(k > 0 && k < (0.003 * rate) as usize, "delay {k}");
            assert!(l[k..].iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-6) && l == r);
            // −12 dB：响度跟着降 12 dB
            let mut y = x.iter().flat_map(|&v| [v, v]).collect::<Vec<f32>>();
            for c in y.chunks_mut(512) {
                d.process_interleaved(c, -12.0, -1.0);
            }
            let left: Vec<f32> = y.iter().step_by(2).copied().collect();
            let db = 20.0 * (rms(&left[n / 2..]) / rms(&x[n / 2..])).log10();
            assert!((db + 12.0).abs() < 0.1, "{db} dB");
            // +12 dB 加在接近满幅的声音上：被限制器压住，真峰值不超过上限
            let loud = sine(0.8, rate, n);
            let (mut l, mut r) = (loud.clone(), loud.clone());
            let mut tp = TruePeak::new();
            for (a, b) in l.chunks_mut(300).zip(r.chunks_mut(300)) {
                d.process(a, b, 12.0, -1.0);
                for (&u, &v) in a.iter().zip(b.iter()) {
                    tp.push(u, v);
                }
            }
            assert!(tp.peak <= db_to_lin(-1.0) * 1.02, "true peak {}", tp.peak);
            assert!(rms(&l[n / 2..]) > rms(&loud[n / 2..]) * 1.05, "调大了应该更响");
            // 复位后不再放出上次留下的声音
            d.reset();
            let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
            d.process(&mut l, &mut r, 0.0, -1.0);
            assert!(l.iter().chain(&r).all(|v| *v == 0.0));
        }
    }

    #[test]
    fn dry_out_gain_change_is_smooth() {
        // 增益从 0 dB 跳到 −20 dB：一块之内线性过渡，相邻样本之间不会突然跳一下
        let mut d = DryOut::new(FS);
        let (mut l, mut r) = (vec![0.5f32; 6144], vec![0.5f32; 6144]);
        d.process(&mut l[..2048], &mut r[..2048], 0.0, -1.0);
        d.process(&mut l[2048..4096], &mut r[2048..4096], -20.0, -1.0);
        d.process(&mut l[4096..], &mut r[4096..], -20.0, -1.0);
        let jump = l.windows(2).skip(d.delay() + 1).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(jump < 0.001, "最大跳变 {jump}");
        assert!((l[6143] - 0.05).abs() < 1e-4, "{}", l[6143]);
    }
}
