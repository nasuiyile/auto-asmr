//! 人声预处理：输入降噪（RNNoise）、去齿音、轻声化、贴耳气声。都不改变嗓音本身。

use super::filters::{ema_coef, interp_log, minphase_from_mag, Biquad, OnePoleLp, BUTTER_Q};
use super::{db_to_lin, CTRL_DT, FS};
use nnnoiseless::DenoiseState;

const DT: f32 = 1.0 / FS;

/// RNNoise 一帧 480 个样本，内部还有一帧重叠，所以打开降噪会增加 960 个样本（20 ms）的延时。
pub const DENOISE_DELAY: usize = 2 * DenoiseState::FRAME_SIZE;
const FRAME: usize = DenoiseState::FRAME_SIZE;

/// 输入降噪：RNNoise（nnnoiseless，纯 CPU 的小型循环网络）。`mix` 控制强度，干声路径对齐后混合。
pub struct Denoiser {
    st: Box<DenoiseState<'static>>,
    inb: [f32; FRAME],
    outb: [f32; FRAME],
    fill: usize,
    dry: Vec<f32>,
    dpos: usize,
    first: bool,
}

impl Denoiser {
    pub fn new() -> Self {
        Denoiser { st: DenoiseState::new(), inb: [0.0; FRAME], outb: [0.0; FRAME], fill: 0, dry: vec![0.0; DENOISE_DELAY], dpos: 0, first: true }
    }

    /// 输入一个样本，返回 20 ms 之前那个样本降噪后的结果。
    #[inline]
    pub fn process(&mut self, x: f32, mix: f32) -> f32 {
        let wet = self.outb[self.fill];
        self.inb[self.fill] = x * 32767.0; // RNNoise 按 16 位整数的量级工作
        self.fill += 1;
        if self.fill == FRAME {
            self.st.process_frame(&mut self.outb, &self.inb);
            if self.first {
                self.outb.fill(0.0); // 第一帧是淡入的残留，丢掉
                self.first = false;
            }
            for v in self.outb.iter_mut() {
                *v /= 32767.0;
            }
            self.fill = 0;
        }
        let dry = self.dry[self.dpos];
        self.dry[self.dpos] = x;
        self.dpos = (self.dpos + 1) % DENOISE_DELAY;
        dry + (wet - dry) * mix.clamp(0.0, 1.0)
    }
}

/// 去齿音：4.5 kHz 以上的齿音带能量接近整体能量时（"s / sh / x"），按 3:1 动态压低，最多 `max_db`。
/// 用 4 阶 Linkwitz–Riley 分频（低 + 高 = 全通），零延时，其他时刻只多一个全通相移。
pub struct DeEsser {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
    det_lp: Biquad,
    eb: f32,
    ef: f32,
    pk: f32,
    gr: f32,
}

impl DeEsser {
    pub fn new() -> Self {
        DeEsser {
            lp: [Biquad::lowpass(4500.0, BUTTER_Q); 2],
            hp: [Biquad::highpass(4500.0, BUTTER_Q); 2],
            det_lp: Biquad::lowpass(11000.0, BUTTER_Q),
            eb: 0.0,
            ef: 0.0,
            pk: 1e-6,
            gr: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32, max_db: f32) -> f32 {
        let lo = self.lp[0].process(x);
        let lo = self.lp[1].process(lo);
        let hi = self.hp[0].process(x);
        let hi = self.hp[1].process(hi);
        let band = self.det_lp.process(hi);
        let c = ema_coef(0.002, DT);
        self.eb += c * (band * band - self.eb);
        self.ef += c * (x * x - self.ef);
        self.pk = (self.pk * (1.0 - ema_coef(3.0, DT))).max(self.ef);
        let mut target = 0.0;
        if self.ef > self.pk * 1e-6 && self.ef > 1e-12 {
            let over = 10.0 * ((self.eb + 1e-14) / (self.ef + 1e-14)).log10() + 8.0;
            target = (over * (2.0 / 3.0)).clamp(0.0, max_db.max(0.0));
        }
        let tau = if target > self.gr { 0.001 } else { 0.02 };
        self.gr += ema_coef(tau, DT) * (target - self.gr);
        lo + hi * db_to_lin(-self.gr)
    }
}

/// 对强辅音的 5 kHz 邻域做动态衰减。干声减去同相带通，中心最多衰减滑块指定的量。
pub struct SharpSoftener {
    band: Biquad,
    band_power: f32,
    input_power: f32,
    recent_peak: f32,
    mix: f32,
}

impl SharpSoftener {
    pub fn new() -> Self {
        Self { band: Biquad::bandpass(5000.0, 1.5), band_power: 0.0, input_power: 0.0, recent_peak: 0.0, mix: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f32, max_mix: f32) -> f32 {
        if max_mix == 0.0 && self.mix < 1e-5 {
            if self.input_power > 0.0 || self.recent_peak > 0.0 {
                *self = Self::new();
            }
            return x;
        }
        let band = self.band.process(x);
        let detect = ema_coef(0.004, DT);
        self.band_power += detect * (band * band - self.band_power);
        self.input_power += detect * (x * x - self.input_power);
        self.recent_peak = (self.recent_peak * (1.0 - ema_coef(3.0, DT))).max(self.band_power);
        // 两个软门限均按功率：频带占总能量的 8%–36%，且达到近期带内峰值的 4%–40%。
        let share = ((self.band_power / self.input_power.max(1e-9) - 0.08) / 0.28).clamp(0.0, 1.0);
        let level = ((self.band_power / self.recent_peak.max(1e-8) - 0.04) / 0.36).clamp(0.0, 1.0);
        let target = max_mix * share * level;
        let rate = if target > self.mix { ema_coef(0.002, DT) } else { ema_coef(0.025, DT) };
        self.mix += rate * (target - self.mix);
        x - self.mix * band
    }
}

/// 近讲呼吸在约 100–300 Hz 堆积时收低这一带。用同一口气的中频作参照，
/// 使轻声和大声有相同的动作门限。直流和高频的滤波增益始终为 1。
pub struct BreathDeBoom {
    body: Biquad,
    mid: Biquad,
    body_power: f32,
    mid_power: f32,
    mix: f32,
}

impl BreathDeBoom {
    pub fn new() -> Self {
        Self { body: Biquad::bandpass(180.0, 0.95), mid: Biquad::bandpass(560.0, 0.9),
            body_power: 0.0, mid_power: 0.0, mix: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f32, max_mix: f32) -> f32 {
        if max_mix == 0.0 && self.mix < 1e-5 {
            if self.body_power > 0.0 || self.mid_power > 0.0 {
                *self = Self::new();
            }
            return x;
        }
        let body = self.body.process(x);
        let mid = self.mid.process(x);
        let follow = ema_coef(0.045, DT);
        self.body_power += follow * (body * body - self.body_power);
        self.mid_power += follow * (mid * mid - self.mid_power);
        // 两个检测带等功率时不动作，低中频高 6 dB 时达到用户设定的最大衰减。
        let amount = ((self.body_power / self.mid_power.max(1e-16) - 1.0) / 3.0).clamp(0.0, 1.0);
        self.mix += ema_coef(0.018, DT) * (amount * max_mix - self.mix);
        x - self.mix * body
    }
}

/// 根据当前的频谱缺口调节气流和亮度。用中频的周期性和背景电平保护持续音与底噪。
pub struct BreathTone {
    low: Biquad,
    mid: Biquad,
    high: [Biquad; 2],
    soften: Biquad,
    power: [f32; 3],
    floor: f32,
    // 4 kHz 的中频历史。每 16 ms 用最近 20 ms 在 80–400 Hz 寻找重复波形。
    history: [f32; 192],
    pos: usize,
    samples: usize,
    decim_sum: f32,
    target: [f32; 2],
    mix: [f32; 2],
    active: bool,
}

impl BreathTone {
    pub fn new() -> Self {
        Self {
            low: Biquad::lowpass(170.0, BUTTER_Q), mid: Biquad::bandpass(700.0, 0.85),
            high: [Biquad::highpass(3000.0, BUTTER_Q); 2], soften: Biquad::peaking(4250.0, 0.65, -20.0),
            power: [0.0; 3], floor: 0.0, history: [0.0; 192], pos: 0, samples: 0, decim_sum: 0.0,
            target: [0.0; 2], mix: [0.0; 2], active: false,
        }
    }

    fn update(&mut self) {
        let [low, mid, high] = self.power;
        self.power = [0.0; 3];
        let total = mid + high;
        // 缓升快降的声底。功率持续接近声底时不把麦克风底噪加工成扑耳声。
        self.floor += (if total > self.floor { 0.05 } else { 0.55 }) * (total - self.floor);
        let rise = ((total / self.floor.max(1e-12) - 1.5) / 2.5).clamp(0.0, 1.0);
        if rise == 0.0 || mid < 1e-12 {
            self.target = [0.0; 2];
            return;
        }
        let mut periodicity = 0.0f32;
        for lag in 10..=50 {
            let (mut dot, mut aa, mut bb) = (0.0, 0.0, 0.0);
            for j in 0..80 {
                let i = self.pos + self.history.len() - 80 + j;
                let a = self.history[i % self.history.len()];
                let b = self.history[(i - lag) % self.history.len()];
                dot += a * b;
                aa += a * a;
                bb += b * b;
            }
            // 平方相关系数避免逐个延迟求平方根。
            periodicity = periodicity.max(dot.max(0.0).powi(2) / (aa * bb).max(1e-24));
        }
        let air = rise * ((0.60 - periodicity) / 0.35).clamp(0.0, 1.0);
        self.target[0] = air * ((0.42 - low / mid) / 0.35).clamp(0.0, 1.0);
        self.target[1] = air * ((high / mid - 0.004) / 0.034).clamp(0.0, 1.0);
    }

    /// 返回处理后的原声和额外气流的权重。补偿关掉时逐样本旁通。
    #[inline]
    pub fn process(&mut self, x: f32, strength: f32) -> (f32, f32) {
        if strength == 0.0 && self.mix[0].max(self.mix[1]) < 1e-5 {
            if self.active { *self = Self::new(); }
            return (x, 0.0);
        }
        self.active = true;
        let low = self.low.process(x);
        let mid = self.mid.process(x);
        let high = self.high[0].process(x);
        let high = self.high[1].process(high);
        self.power[0] += low * low;
        self.power[1] += mid * mid;
        self.power[2] += high * high;
        self.decim_sum += mid;
        self.samples += 1;
        if self.samples % 12 == 0 {
            self.history[self.pos] = self.decim_sum / 12.0;
            self.decim_sum = 0.0;
            self.pos = (self.pos + 1) % self.history.len();
            if self.samples == 768 {
                self.update();
                self.samples = 0;
            }
        }
        for (mix, target) in self.mix.iter_mut().zip(self.target) {
            *mix += ema_coef(0.012, DT) * (strength * target - *mix);
        }
        let dark = self.soften.process(x);
        (x + self.mix[1] * (dark - x), self.mix[0])
    }
}

// ANSI S3.5-1997 表 3：不同说话力度的标准语音频谱（1/3 倍频程，dB）
const ANSI_F: [f32; 18] = [160., 200., 250., 315., 400., 500., 630., 800., 1000., 1250., 1600., 2000., 2500., 3150., 4000., 5000., 6300., 8000.];
const ANSI_NORMAL: [f32; 18] = [32.41, 34.48, 34.75, 33.98, 34.59, 34.27, 32.06, 28.3, 25.01, 23.0, 20.15, 17.32, 13.18, 11.55, 9.33, 5.31, 2.59, 1.13];
const ANSI_RAISED: [f32; 18] = [33.81, 33.92, 38.98, 38.57, 39.11, 40.15, 38.78, 36.37, 33.86, 31.89, 28.58, 25.32, 22.35, 20.15, 16.78, 11.47, 7.67, 5.07];

pub const SOFT_TAPS: usize = 512;

/// 轻声化的均衡器（最小相位 FIR）：按 ANSI S3.5"正常"与"提高"力度的频谱形状差反向施加 `amount` 倍——
/// 中频略收、低频更饱满、最高频气声略多（Brungart & Scott 2001：说话力度是语音距离感的主要线索）。
pub fn soft_voice_fir(amount: f32) -> Vec<f32> {
    let mut diff = [0f32; 18];
    let mut mean = 0.0;
    for i in 0..18 {
        diff[i] = ANSI_RAISED[i] - ANSI_NORMAL[i];
        mean += diff[i] / 18.0;
    }
    let mut shape = [0f32; 18];
    for i in 0..18 {
        shape[i] = (-(diff[i] - mean)).clamp(-3.0, 3.0);
    }
    let mut sm = [0f32; 18];
    for i in 0..18usize {
        let a = shape[i.saturating_sub(1)];
        let b = shape[(i + 1).min(17)];
        sm[i] = (a + shape[i] + b) / 3.0;
    }
    let nfft = 4096;
    let mag: Vec<f32> = (0..=nfft / 2)
        .map(|k| {
            let f = (k as f32 * FS / nfft as f32).max(20.0);
            db_to_lin(amount * interp_log(f, &ANSI_F, &sm))
        })
        .collect();
    minphase_from_mag(&mag, nfft, SOFT_TAPS)
}

// 近耳音色校正（对齐 KU100 近耳 / 居中差），--ku100-eq 时使用
const EAR_EQ_F: [f32; 9] = [500., 2000., 4000., 6000., 8000., 10000., 12500., 16000., 22000.];
const EAR_EQ_DB: [f32; 9] = [0.0, 0.0, -1.5, -4.0, -5.0, -5.5, -4.0, -3.0, -3.0];

pub const EAR_EQ_TAPS: usize = 64;

/// 耳甲腔深度共振的频率（Shaw 1979：堵住耳道口时 4.3 kHz，各个方向来的声音都能激发）。
pub const CONCHA_F: f32 = 4300.0;

/// 贴耳时近耳的均衡（最小相位 FIR，64 点）：临场感（3.5 kHz）+ 空气感（9 kHz 以上）−"暗"（5 kHz）
/// + 耳甲共鸣（4.3 kHz，约 0.8 倍频程宽）。全为 0 时返回 None。
pub fn ear_eq_fir(presence_db: f32, dark_db: f32, ku100_eq: bool, concha_db: f32) -> Option<Vec<f32>> {
    if presence_db == 0.0 && dark_db == 0.0 && !ku100_eq && concha_db == 0.0 {
        return None;
    }
    let nfft = 512;
    let mag: Vec<f32> = (0..=nfft / 2)
        .map(|k| {
            let f = (k as f32 * FS / nfft as f32).max(1.0);
            let mut d = if ku100_eq { interp_log(f, &EAR_EQ_F, &EAR_EQ_DB) } else { 0.0 };
            d += presence_db * (-0.5 * ((f / 3500.0).log2() / 0.7).powi(2)).exp() + 0.7 * presence_db / (1.0 + (9000.0 / f).powi(2));
            d -= dark_db * (-0.5 * ((f / 5000.0).log2() / 0.6).powi(2)).exp();
            d += concha_db * (-0.5 * ((f / CONCHA_F).log2() / 0.33).powi(2)).exp();
            db_to_lin(d)
        })
        .collect();
    Some(minphase_from_mag(&mag, nfft, EAR_EQ_TAPS))
}

/// 远耳高频遮挡的转折频率。
pub const FAR_SHADOW_F: f32 = 6000.0;

/// 贴耳时远耳的高频遮挡（最小相位 FIR，64 点）：球头模型低估了近处声源在远耳的高频头影
/// （与 BEM 近距仿真比，离耳 3.8 cm 时远耳 6 kHz 以上偏亮 2–9 dB）。`db` 是最高频压低的量；0 时返回 None。
pub fn far_shadow_fir(db: f32) -> Option<Vec<f32>> {
    if db <= 0.0 {
        return None;
    }
    let nfft = 512;
    let mag: Vec<f32> = (0..=nfft / 2)
        .map(|k| {
            let f = (k as f32 * FS / nfft as f32).max(1.0);
            db_to_lin(-db / (1.0 + (FAR_SHADOW_F / f).powi(2)))
        })
        .collect();
    Some(minphase_from_mag(&mag, nfft, EAR_EQ_TAPS))
}

/// 堵耳闷感的转折频率。
pub const MUFFLE_F: f32 = 2500.0;

/// 入耳时近耳的高频闷感（最小相位 FIR，64 点）：物体伸进耳道后把耳道口堵住一部分，外面传进来的高频变少。
/// `db` 是最高频压低的量（2.5 kHz 处压一半）；0 时返回 None。
pub fn muffle_fir(db: f32) -> Option<Vec<f32>> {
    if db <= 0.0 {
        return None;
    }
    let nfft = 512;
    let mag: Vec<f32> = (0..=nfft / 2)
        .map(|k| {
            let f = (k as f32 * FS / nfft as f32).max(1.0);
            db_to_lin(-db / (1.0 + (MUFFLE_F / f).powi(2)))
        })
        .collect();
    Some(minphase_from_mag(&mag, nfft, EAR_EQ_TAPS))
}

/// 耳道共鸣（入耳）：物体伸进耳道后，它和话筒（鼓膜）之间剩下的那段耳道是一根一头堵住的短管，
/// 在四分之一波长的奇数倍频率上共振（f = c / 4L，3f，5f…）。伸得越深、管越短，共振越高——
/// 两段 KU100 掏耳实录里这条共振峰随棉签进出在约 3–11 kHz 之间来回滑动，峰高中位约 8 dB。
///
/// 做法：带负反馈的梳状滤波器 y[n] = (1 + g)·x[n] − g·y[n − D]，D = 2L / c（来回一趟的时间，样本）。
/// 直流和偶数倍频率上增益为 1，奇数倍频率上是 (1 + g) / (1 − g)。D 逐样本变化（线性插值），滑动时没有拉链声；
/// 插值在反馈环里带来的高频损耗正好让 3f、5f 比基频弱。
#[derive(Clone)]
pub struct CanalComb {
    buf: [f32; 16],
    pos: usize,
}

impl CanalComb {
    pub fn new() -> Self {
        CanalComb { buf: [0.0; 16], pos: 0 }
    }

    /// 共振峰高度（dB）换成反馈系数。
    pub fn feedback(peak_db: f32) -> f32 {
        let g = db_to_lin(peak_db.clamp(0.0, 24.0));
        (g - 1.0) / (g + 1.0)
    }

    /// 剩余耳道长度（米）换成梳状滤波器的时延（样本）。
    pub fn delay(len_m: f32) -> f32 {
        (2.0 * len_m / super::C * FS).clamp(1.0, 14.0)
    }

    /// `g`：反馈系数（0 = 直通）；`d`：时延（样本，1–14）。
    #[inline]
    pub fn process(&mut self, x: f32, g: f32, d: f32) -> f32 {
        let di = d as usize;
        let fr = d - di as f32;
        let a = self.buf[(self.pos + di - 1) & 15];
        let b = self.buf[(self.pos + di) & 15];
        let y = (1.0 + g) * x - g * (a + fr * (b - a));
        self.pos = (self.pos + 15) & 15;
        self.buf[self.pos] = y;
        y
    }
}

/// 耳内低频的标定增益（`rumble` = 1 时）：掏耳预设下输出的 20–160 Hz 比 300–2000 Hz 高约 15 dB
/// （两段 KU100 掏耳实录：+10…+20 dB）。
pub const RUMBLE_K: f32 = 21.0;
/// 摩擦声的频带下限：这以上的内容当作"摩擦 / 触碰声"，由它的包络导出耳内低频。
pub const RUMBLE_SRC_HP: f32 = 250.0;

/// 耳内低频（入耳）：物体在耳道里摩擦 / 触碰时，话筒前那个很小的腔体里会有很强的低频压力——
/// 两段 KU100 掏耳实录的近耳，20–160 Hz 比 300–2000 Hz 的摩擦声高 10–20 dB，两者的包络同步（相关 0.6–0.76，时差 0–20 ms），
/// 低频段内是粉红谱（25–160 Hz 各 1/3 倍频程一样响）。普通话筒隔着空气录的摩擦声里没有这部分。
///
/// 做法与"气流扑耳"同一个思路，只用输入自己的成分、不生成噪声：取 250 Hz 以上摩擦声的整流包络，
/// 留下它在 25–170 Hz 的起伏（每一下微小的黏滑 / 磕碰就是一个压力脉冲），整形成粉红谱；
/// 再按"慢包络 ÷ 快包络"的平方根做 2:1 的压缩，让它像实录那样连续地隆隆响，而不是一下一下地蹦。
/// 幅度与输入成正比（输入大 6 dB，它也大 6 dB），没有声音时为 0。
pub struct ContactFx {
    hp: Biquad,
    res: Biquad,
    lp: Biquad,
    sh: [f32; 3],
    sh_x1: f32,
    sh_y1: f32,
    fast: OnePoleLp,
    slow: f32,
    gain: (f32, f32),
    primed: bool,
}

impl ContactFx {
    pub fn new() -> Self {
        // 一阶低架：极点 40 Hz、零点 80 Hz（把平坦的包络起伏往粉红谱上靠：越低越强），整体再提 √2
        let (kp, kz) = ((std::f32::consts::PI * 40.0 / FS).tan(), (std::f32::consts::PI * 80.0 / FS).tan());
        let n = (kp / kz) * std::f32::consts::SQRT_2 / (kp + 1.0);
        ContactFx {
            hp: Biquad::highpass(RUMBLE_SRC_HP, BUTTER_Q),
            res: Biquad::highpass(32.0, 1.0),
            lp: Biquad::lowpass(170.0, BUTTER_Q),
            sh: [n * (kz + 1.0), n * (kz - 1.0), (kp - 1.0) / (kp + 1.0)],
            sh_x1: 0.0,
            sh_y1: 0.0,
            fast: OnePoleLp::new(16.0),
            slow: 0.0,
            gain: (0.0, 0.0),
            primed: false,
        }
    }

    /// 控制块开始：`g` = 块末尾的增益（入耳程度 × `rumble`）。
    pub fn begin_block(&mut self, g: f32) {
        let g = RUMBLE_K * g.max(0.0);
        if self.primed {
            self.gain = (self.gain.1, g);
        } else {
            self.gain = (g, g);
            self.primed = true;
        }
    }

    /// 处理一个样本：`a` = 输入，`wp` = 在控制块里的进度（0–1）。返回要加到直达声上的低频。
    #[inline]
    pub fn process(&mut self, a: f32, wp: f32) -> f32 {
        let q = self.hp.process(a).abs();
        let w = self.lp.process(self.res.process(q));
        let y = self.sh[0] * w + self.sh[1] * self.sh_x1 - self.sh[2] * self.sh_y1;
        self.sh_x1 = w;
        self.sh_y1 = y;
        let sf = self.fast.process(q).max(0.0);
        // 慢包络：30 ms 跟上去，150 ms 落下来
        const CA: f32 = 1.0 / (0.030 * FS);
        const CR: f32 = 1.0 / (0.150 * FS);
        self.slow += (if sf > self.slow { CA } else { CR }) * (sf - self.slow);
        let g = self.gain.0 + (self.gain.1 - self.gain.0) * wp;
        if g <= 0.0 {
            return 0.0;
        }
        g * y * ((self.slow + 1e-9) / (sf + 0.05 * self.slow + 1e-9)).sqrt()
    }
}

/// 气流脉冲的标定增益（`puff` = 1 时）。
pub const PUFF_K: f32 = 1.5;
/// 气声带下限：F3 以上送气噪声取代谐波（Klatt & Klatt 1990）。
pub const BREATH_FC: f32 = 2500.0;

/// 贴耳气声（与 Python 版 `breath_fx` 相同）：嘴贴近耳朵时把嗓音自己的气声提上来，并加上气流扑到耳朵上的低频脉冲。
/// 两样都只用嗓音里已有的高频成分。气声增强可延伸到近距离正面，气流脉冲只在贴耳时出现。
///
/// * 气声增强：气声带 = 2.5 kHz 以上。只把"轻"的部分提上来（频带内向上压缩）：电平比响的齿音低 16 dB 以上的
///   送气声 / 气息 / 轻辅音提得最多，接近齿音电平的不提，低到底噪附近（低 36–48 dB）的也不提。
/// * 气流扑耳：气声带瞬时功率在 50–130 Hz 的起伏（除以功率的平方根，幅度与气声成正比），幅度跟着人声的电平走，
///   送气越强脉冲越强，爆破音开头最明显。加在直达声的输入上，由近场 HRIR 送到贴着的那只耳朵。
///
/// 电平每个控制块（1.33 ms）检测一次，下一个控制块生效，没有额外延时。
pub struct BreathFx {
    hp: Biquad,
    s_lp: OnePoleLp,
    w_hp: Biquad,
    w_lp: [Biquad; 2],
    acc_b: f64,
    acc_x: f64,
    n: usize,
    env: f64,
    lref: f64,
    envx: f64,
    vref: f64,
    /// 当前可用的提升比例（0–1）。
    g: f32,
    /// "响的齿音"的功率、"响的人声"的幅度。
    pref: f32,
    vamp: f32,
    lift: (f32, f32),
    puff: (f32, f32),
    primed: bool,
}

/// "响的"电平跟踪的初值（dBFS）：从很低开始，听到第一个响的音就跟上去，与输入音量无关。
const BREATH_REF0: f64 = -90.0;

impl BreathFx {
    pub fn new() -> Self {
        BreathFx {
            hp: Biquad::highpass(BREATH_FC, BUTTER_Q),
            s_lp: OnePoleLp::new(30.0),
            w_hp: Biquad::highpass(50.0, BUTTER_Q),
            w_lp: [Biquad::lowpass(130.0, BUTTER_Q); 2],
            acc_b: 0.0,
            acc_x: 0.0,
            n: 0,
            env: 0.0,
            lref: BREATH_REF0,
            envx: 0.0,
            vref: BREATH_REF0,
            g: 0.0,
            pref: 10f64.powf(BREATH_REF0 / 10.0) as f32,
            vamp: 10f64.powf(BREATH_REF0 / 20.0) as f32,
            lift: (0.0, 0.0),
            puff: (0.0, 0.0),
            primed: false,
        }
    }

    /// 控制块开始：`cl` = 块末尾的贴耳程度（0–1），算出这一块里气声增益和气流增益走到哪里。
    pub fn begin_block(&mut self, cl: f32, lift_db: f32, puff: f32) {
        self.begin_block_at(cl, 0.0, 0.0, lift_db, puff);
    }

    /// `distance` 为嘴到头中心的距离（米），`reach` 只扩展气声增强的作用范围。
    pub(crate) fn begin_block_at(&mut self, cl: f32, distance: f32, reach: f32, lift_db: f32, puff: f32) {
        let nearby = ((0.65 - distance) / (0.65 - 0.20)).clamp(0.0, 1.0);
        let lift_weight = cl.max(reach.clamp(0.0, 1.0) * nearby);
        let t = if lift_db > 0.0 { lift_weight * (db_to_lin(lift_db * self.g) - 1.0) } else { 0.0 };
        let u = PUFF_K * puff.max(0.0) * cl;
        if self.primed {
            self.lift = (self.lift.1, t);
            self.puff = (self.puff.1, u);
        } else {
            self.lift = (t, t);
            self.puff = (u, u);
            self.primed = true;
        }
    }

    /// 处理一个样本：`a` = 人声，`wp` = 在控制块里的进度（0–1）。返回（加强后的人声，气流脉冲）。
    #[inline]
    pub fn process(&mut self, a: f32, wp: f32) -> (f32, f32) {
        let b = self.hp.process(a);
        let q = b * b;
        self.acc_b += q as f64;
        self.acc_x += (a * a) as f64;
        self.n += 1;
        let y = a + (self.lift.0 + (self.lift.1 - self.lift.0) * wp) * b;
        let s = self.s_lp.process(q).max(0.0);
        let w = self.w_hp.process(q);
        let w = self.w_lp[0].process(w);
        let w = self.w_lp[1].process(w);
        let gp = self.puff.0 + (self.puff.1 - self.puff.0) * wp;
        if gp <= 0.0 {
            return (y, 0.0);
        }
        // 气声功率的低频起伏 / 功率的平方根 = 与气声幅度成正比；再除以响的齿音幅度，得到与音色明暗无关的量
        let raw = w / ((s + 1e-3 * self.pref) * self.pref + 1e-30).sqrt();
        (y, gp * self.vamp * raw.tanh())
    }

    /// 控制块结束：用这一块的均方更新电平、"响的"电平和提升比例。
    pub fn end_block(&mut self) {
        if self.n == 0 {
            return;
        }
        let dt = CTRL_DT as f64 * self.n as f64 / super::CTRL as f64;
        let (ca, cr) = (1.0 - (-dt / 0.003).exp(), 1.0 - (-dt / 0.03).exp());
        // "响的"电平：50 ms 跟上去；有声音时每秒落 0.5 dB，静下来（低 30 dB 以上）就保持
        let (cu, down) = (1.0 - (-dt / 0.05).exp(), 0.5 * dt);
        let m = self.acc_b / self.n as f64;
        self.env += (if m > self.env { ca } else { cr }) * (m - self.env);
        let lb = 10.0 * (self.env + 1e-12).log10();
        self.lref = if lb > self.lref {
            self.lref + cu * (lb - self.lref)
        } else if lb > self.lref - 30.0 {
            self.lref - down
        } else {
            self.lref
        };
        let m = self.acc_x / self.n as f64;
        self.envx += (if m > self.envx { ca } else { cr }) * (m - self.envx);
        let lx = 10.0 * (self.envx + 1e-12).log10();
        self.vref = if lx > self.vref {
            self.vref + cu * (lx - self.vref)
        } else if lx > self.vref - 30.0 {
            self.vref - down
        } else {
            self.vref
        };
        let d = self.lref - lb;
        let u = ((d - 4.0) / 12.0).clamp(0.0, 1.0);
        let v = ((d - 36.0) / 12.0).clamp(0.0, 1.0);
        self.g = (u * u * (3.0 - 2.0 * u) * (1.0 - v * v * (3.0 - 2.0 * v))) as f32;
        self.pref = 10f64.powf(self.lref / 10.0) as f32;
        self.vamp = 10f64.powf(self.vref / 20.0) as f32;
        self.acc_b = 0.0;
        self.acc_x = 0.0;
        self.n = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::filters::fir_mag;

    #[test]
    fn sharp_softener_is_selective_and_bypasses_cleanly() {
        let mut fx = SharpSoftener::new();
        let max_mix = 1.0 - db_to_lin(-6.0);
        let n = FS as usize / 2;
        let mut run = |frequency: f32, amplitude: f32, mix: f32| {
            let (mut dry, mut wet) = (0.0f64, 0.0f64);
            for i in 0..n {
                let phase = std::f32::consts::TAU * frequency * (i % 480) as f32 / FS;
                let x = amplitude * phase.sin();
                let y = fx.process(x, mix);
                if i >= n / 2 {
                    dry += f64::from(x).powi(2);
                    wet += f64::from(y).powi(2);
                }
            }
            10.0 * (wet / dry).log10()
        };
        assert_eq!(run(5000.0, 0.3, 0.0), 0.0);
        assert!((run(5000.0, 0.3, max_mix) + 6.0).abs() < 0.03);
        assert!(run(5000.0, 0.015, max_mix).abs() < 0.03, "峰值之后的微弱同频成分");
        assert!(run(10000.0, 0.3, max_mix).abs() < 0.5, "高频空气感");
        assert!(run(400.0, 0.3, max_mix).abs() < 0.03, "低频人声基音");
        run(5000.0, 0.3, max_mix);
        run(5000.0, 0.3, 0.0);
        assert_eq!(fx.process(-0.1234567, 0.0).to_bits(), (-0.1234567f32).to_bits());
        assert_eq!(fx.recent_peak, 0.0);
        // 中途短暂关掉，在淡出完成前重开时沿当前的衰减量继续。
        for i in 0..4800 {
            fx.process(0.3 * (std::f32::consts::TAU * 5000.0 * (i % 48) as f32 / FS).sin(), max_mix);
        }
        let on = fx.mix;
        for _ in 0..48 {
            fx.process(0.0, 0.0);
        }
        let releasing = fx.mix;
        assert!(releasing > 0.9 * on && releasing < on);
        fx.process(0.0, max_mix);
        assert!((fx.mix - releasing).abs() < 0.01);
    }

    /// 远耳高频遮挡：6 kHz 处压一半，最高频接近设定值，500 Hz 基本不动。
    #[test]
    fn far_shadow_shape() {
        let h = far_shadow_fir(5.0).unwrap();
        let m = fir_mag(&h, 4096);
        let db = |f: f32| 20.0 * m[(f / FS * 4096.0).round() as usize].log10();
        assert!(db(500.0).abs() < 0.3, "500 Hz {}", db(500.0));
        assert!((db(6000.0) + 2.5).abs() < 0.4, "6 kHz {}", db(6000.0));
        assert!(db(16000.0) < -4.0 && db(16000.0) > -5.2, "16 kHz {}", db(16000.0));
        assert!(far_shadow_fir(0.0).is_none());
    }

    /// 耳甲共鸣：4.3 kHz 处约 +4 dB，1 kHz 和 12 kHz 基本不动。
    #[test]
    fn concha_peak() {
        let h = ear_eq_fir(0.0, 0.0, false, 4.0).unwrap();
        let m = fir_mag(&h, 4096);
        let db = |f: f32| 20.0 * m[(f / FS * 4096.0).round() as usize].log10();
        assert!((db(4300.0) - 4.0).abs() < 0.4, "4.3 kHz {}", db(4300.0));
        assert!(db(1000.0).abs() < 0.4 && db(12000.0).abs() < 0.4, "1k {} 12k {}", db(1000.0), db(12000.0));
    }

    /// 跑一段信号：返回（提升后 2.5 kHz 以上相对原来的增益 dB，气流脉冲有效值相对输入有效值 dB）。
    fn run_breath(x: &[f32], cl: f32, lift_db: f32, puff: f32, skip: usize, placement: Option<(f32, f32)>) -> (f32, f32) {
        let mut fx = BreathFx::new();
        let mut hp_in = Biquad::highpass(BREATH_FC, BUTTER_Q);
        let mut hp_out = Biquad::highpass(BREATH_FC, BUTTER_Q);
        let (mut ei, mut eo, mut ep, mut ex) = (0f64, 0f64, 0f64, 0f64);
        for (k, blk) in x.chunks(super::super::CTRL).enumerate() {
            if let Some((distance, reach)) = placement {
                fx.begin_block_at(cl, distance, reach, lift_db, puff);
            } else {
                fx.begin_block(cl, lift_db, puff);
            }
            for (i, &a) in blk.iter().enumerate() {
                let (y, p) = fx.process(a, i as f32 / super::super::CTRL as f32);
                let (bi, bo) = (hp_in.process(a), hp_out.process(y));
                if k * super::super::CTRL + i >= skip {
                    ei += (bi * bi) as f64;
                    eo += (bo * bo) as f64;
                    ep += (p * p) as f64;
                    ex += (a * a) as f64;
                }
            }
            fx.end_block();
        }
        ((10.0 * (eo / ei.max(1e-30)).log10()) as f32, (10.0 * (ep / ex.max(1e-30)).log10()) as f32)
    }

    /// 气声增强：响的齿音不提，比它低 20 dB 的气声提到接近设定值，离开耳边（贴耳程度 0）时不动，底噪不提。
    #[test]
    fn breath_lift_only_soft() {
        let n = FS as usize * 4;
        let loud = noise(n, -20.0, true);
        // 前 2 s 响（让"响的齿音"电平跟上去），后 2 s 低 20 dB / 低 60 dB；从变轻 0.6 s 后开始量
        let mk = |drop_db: f32| -> Vec<f32> { loud.iter().enumerate().map(|(i, &v)| if i < n / 2 { v } else { v * db_to_lin(-drop_db) }).collect() };
        let half = n / 2 + FS as usize * 6 / 10;
        let soft = mk(20.0);
        let (g_loud, _) = run_breath(&loud, 1.0, 6.0, 0.0, half, None);
        let (g_soft, _) = run_breath(&soft, 1.0, 6.0, 0.0, half, None);
        let (g_floor, _) = run_breath(&mk(60.0), 1.0, 6.0, 0.0, half, None);
        let (g_away, _) = run_breath(&soft, 0.0, 6.0, 0.0, half, None);
        assert!(g_loud.abs() < 0.5, "响的不提：{g_loud}");
        assert!(g_soft > 4.5 && g_soft < 6.2, "轻的提到接近 6 dB：{g_soft}");
        assert!(g_floor.abs() < 0.5, "底噪不提：{g_floor}");
        assert!(g_away.abs() < 1e-3, "离开耳边不动：{g_away}");

        // 正前方没有贴耳气流。扩展到近场后仍只增强原声的高频，退到 65 cm 或关闭时恢复。
        let frontal = |distance, reach| run_breath(&soft, 0.0, 6.0, 1.0, half, Some((distance, reach)));
        let (near, pulse) = frontal(0.20, 0.75);
        let (middle, _) = frontal(0.425, 0.75);
        assert!(near > 3.0 && middle > 1.0 && middle < near);
        assert_eq!(pulse, f32::NEG_INFINITY);
        assert_eq!(frontal(0.65, 0.75).0, g_away);
        assert_eq!(frontal(0.2, 0.0).0, g_away);
        assert_eq!(run_breath(&soft, 1.0, 6.0, 0.0, half, Some((0.11, 0.75))).0, g_soft);
    }

    #[test]
    fn breath_deboom_attenuates_a_dominant_low_mid_and_bypasses_cleanly() {
        let mut fx = BreathDeBoom::new();
        let mix = 1.0 - db_to_lin(-6.0);
        let gain_db = |fx: &mut BreathDeBoom, hz: f32, amp: f32| {
            let mut energy = [0.0f64; 2];
            for i in 0..48000 {
                let x = amp * (std::f32::consts::TAU * hz * i as f32 / FS).sin();
                let y = fx.process(x, mix);
                if i > 24000 {
                    energy[0] += f64::from(x).powi(2);
                    energy[1] += f64::from(y).powi(2);
                }
            }
            10.0 * (energy[1] / energy[0]).log10()
        };
        for amp in [0.01, 0.25] {
            assert!((gain_db(&mut fx, 180.0, amp) + 6.0).abs() < 0.2);
        }
        assert!(gain_db(&mut fx, 55.0, 0.1).abs() < 1.0);
        assert!(gain_db(&mut fx, 560.0, 0.1).abs() < 0.1);
        for _ in 0..24000 { fx.process(0.1, 0.0); }
        assert_eq!(fx.process(-0.25, 0.0), -0.25);
    }

    #[test]
    fn breath_tone_checks_activity_periodicity_and_the_input_spectrum() {
        let fs = FS as usize;
        let mut rng = crate::dsp::rng::Rng::new(91);
        let mut hp = [Biquad::highpass(650.0, BUTTER_Q); 2];
        let mut fx = BreathTone::new();
        // 同一频谱分别持续、起伏、富含低频、带音高；检查实际用于气流的权重。
        let mut peaks = [0.0f32; 4];
        for i in 0..8 * fs {
            let t = i as f32 / FS;
            let stage = i / (2 * fs);
            let pos = i % (2 * fs);
            let phase = std::f32::consts::TAU * 160.0 * t;
            let white = hp[0].process(rng.uniform(-1.0, 1.0));
            let white = hp[1].process(white);
            let env = if stage == 0 { 1.0 } else { (std::f32::consts::TAU * 1.25 * t).sin().max(0.0) };
            let x = 0.02 * env * match stage {
                2 => white + 2.0 * (0.5 * phase).sin(),
                3 => (4.0 * phase).sin() + 0.6 * (5.0 * phase).sin() + 0.3 * (19.0 * phase).sin(),
                _ => white,
            };
            let (_, wind) = fx.process(x, 0.85);
            if pos > fs { peaks[stage] = peaks[stage].max(wind); }
        }
        assert!(peaks[0] < 0.08 && peaks[1] > 0.35 && peaks[2] < 0.1 && peaks[3] < 0.1, "{peaks:?}");
        for _ in 0..fs { fx.process(0.0, 0.0); }
        assert_eq!(fx.process(0.25, 0.0), (0.25, 0.0));
    }

    /// 气流扑耳：能量集中在 50–130 Hz，强度随 puff 线性变化，离开耳边时为 0。
    #[test]
    fn puff_is_low_frequency() {
        let n = FS as usize * 2;
        // 每 0.25 s 一段 60 ms 的"送气"噪声
        let x: Vec<f32> = noise(n, -20.0, true).iter().enumerate().map(|(i, &v)| if i % 12000 < 2880 { v } else { 0.0 }).collect();
        let mut fx = BreathFx::new();
        let (mut lo, mut hi) = (Biquad::lowpass(200.0, BUTTER_Q), Biquad::highpass(400.0, BUTTER_Q));
        let (mut e_all, mut e_lo, mut e_hi) = (0f64, 0f64, 0f64);
        for blk in x.chunks(super::super::CTRL) {
            fx.begin_block(1.0, 0.0, 1.0);
            for (i, &a) in blk.iter().enumerate() {
                let (y, p) = fx.process(a, i as f32 / super::super::CTRL as f32);
                assert_eq!(y, a, "不开气声增强时人声不变");
                let (l, h) = (lo.process(p), hi.process(p));
                e_all += (p * p) as f64;
                e_lo += (l * l) as f64;
                e_hi += (h * h) as f64;
            }
            fx.end_block();
        }
        assert!(e_all > 0.0);
        assert!(e_lo / e_all > 0.8, "200 Hz 以下占 {}", e_lo / e_all);
        assert!(e_hi / e_all < 0.01, "400 Hz 以上占 {}", e_hi / e_all);
        let (_, p1) = run_breath(&x, 1.0, 0.0, 1.0, 0, None);
        let (_, p2) = run_breath(&x, 1.0, 0.0, 2.0, 0, None);
        let (_, p0) = run_breath(&x, 0.0, 0.0, 1.0, 0, None);
        assert!((p2 - p1 - 6.02).abs() < 0.1, "puff 加倍 +6 dB：{}", p2 - p1);
        assert!(p1 > -30.0 && p1 < 0.0, "脉冲电平相对输入 {p1} dB");
        assert!(p0 < -200.0, "离开耳边没有脉冲：{p0}");
    }

    /// 堵耳闷感：2.5 kHz 处压一半，最高频接近设定值，500 Hz 基本不动。
    #[test]
    fn muffle_shape() {
        let h = muffle_fir(6.0).unwrap();
        let m = fir_mag(&h, 4096);
        let db = |f: f32| 20.0 * m[(f / FS * 4096.0).round() as usize].log10();
        assert!(db(500.0).abs() < 0.5, "500 Hz {}", db(500.0));
        assert!((db(2500.0) + 3.0).abs() < 0.5, "2.5 kHz {}", db(2500.0));
        assert!(db(10000.0) < -5.0 && db(10000.0) > -6.2, "10 kHz {}", db(10000.0));
        assert!(muffle_fir(0.0).is_none());
    }

    /// 梳状滤波器在给定时延下的幅频响应（dB）：喂一个单位冲激，取冲激响应的谱。
    fn comb_response(g: f32, d: f32) -> Vec<f32> {
        let mut c = CanalComb::new();
        let h: Vec<f32> = (0..2048).map(|n| c.process(if n == 0 { 1.0 } else { 0.0 }, g, d)).collect();
        fir_mag(&h, 8192).iter().map(|v| 20.0 * v.max(1e-9).log10()).collect()
    }

    /// 耳道共鸣：共振峰在 c / 4L（和它的 3 倍）上，峰高接近设定值，直流增益为 1；伸得越深（L 越短）峰越高。
    #[test]
    fn canal_resonance_follows_the_remaining_length() {
        let g = CanalComb::feedback(10.0);
        let bin = |f: f32| (f / FS * 8192.0).round() as usize;
        let mut last = 0.0;
        for len in [0.029f32, 0.020, 0.012, 0.008] {
            let m = comb_response(g, CanalComb::delay(len));
            let f1 = crate::dsp::C / (4.0 * len);
            // 在 f1 的 ±1/3 倍频程里找峰
            let (lo, hi) = (bin(f1 / 1.26), bin(f1 * 1.26).min(m.len() - 1));
            let (k, pk) = (lo..=hi).map(|k| (k, m[k])).fold((0, f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
            let f_pk = k as f32 * FS / 8192.0;
            assert!((f_pk / f1).log2().abs() < 0.08, "L {len}: 峰在 {f_pk} Hz，应在 {f1} Hz");
            assert!(pk > 7.5 && pk < 10.3, "L {len}: 峰高 {pk} dB");
            assert!(m[0].abs() < 0.05, "直流 {} dB", m[0]);
            // 两个共振峰之间（2·f1）回到 0 dB 附近
            if 2.0 * f1 < 20000.0 {
                assert!(m[bin(2.0 * f1)].abs() < 1.5, "L {len}: 2f1 处 {} dB", m[bin(2.0 * f1)]);
            }
            assert!(f_pk > last, "越短越高");
            last = f_pk;
        }
        // 3 倍频率上还有一个峰，比基频的矮（反馈环里插值的高频损耗）
        let m = comb_response(g, CanalComb::delay(0.029));
        let (p1, p3) = (m[bin(2957.0)], m[bin(3.0 * 2957.0)]);
        assert!(p3 > 3.0 && p3 < p1, "f1 {p1} dB，3f1 {p3} dB");
        // 反馈为 0 时原样通过
        let mut c = CanalComb::new();
        for n in 0..500 {
            let x = (n as f32 * 0.37).sin();
            assert_eq!(c.process(x, 0.0, 3.3 + 0.01 * n as f32), x);
        }
    }

    /// 共振频率飞快地来回滑动时输出仍然有界（时变梳状滤波器稳定）。
    #[test]
    fn canal_comb_stays_bounded_while_sweeping() {
        let x = noise(48000, -12.0, true);
        let g = CanalComb::feedback(16.0);
        let mut c = CanalComb::new();
        let mut pk = 0f32;
        for (n, &v) in x.iter().enumerate() {
            let d = 4.9 + 3.2 * (n as f32 * 0.002).sin() + 0.9 * (n as f32 * 0.11).sin();
            let y = c.process(v, g, d);
            assert!(y.is_finite());
            pk = pk.max(y.abs());
        }
        let in_pk = x.iter().fold(0f32, |a, v| a.max(v.abs()));
        assert!(pk < 12.0 * in_pk, "输出峰值 {pk}，输入峰值 {in_pk}");
    }

    /// 一下一下的摩擦声（每 0.5 s 一段 0.3 s 的噪声，带起音和收尾）。
    fn strokes(seconds: f32, rms_db: f32) -> Vec<f32> {
        let n = (seconds * FS) as usize;
        noise(n, rms_db, true)
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let t = (i % 24000) as f32 / 14400.0;
                if t < 1.0 {
                    v * (std::f32::consts::PI * t).sin().powi(2)
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn run_rumble(x: &[f32], gain: f32) -> Vec<f32> {
        let mut fx = ContactFx::new();
        let mut out = Vec::with_capacity(x.len());
        for blk in x.chunks(super::super::CTRL) {
            fx.begin_block(gain);
            for (i, &a) in blk.iter().enumerate() {
                out.push(fx.process(a, i as f32 / super::super::CTRL as f32));
            }
        }
        out
    }

    /// 耳内低频：能量集中在 200 Hz 以下，幅度与输入成正比，没伸进耳道（增益 0）或没有声音时为 0。
    #[test]
    fn rumble_is_low_frequency_and_follows_the_input() {
        let x = strokes(6.0, -26.0);
        let y = run_rumble(&x, 1.0);
        let (mut lo, mut hi) = (Biquad::lowpass(200.0, BUTTER_Q), Biquad::highpass(400.0, BUTTER_Q));
        let (mut e_all, mut e_lo, mut e_hi) = (0f64, 0f64, 0f64);
        for &v in &y {
            let (l, h) = (lo.process(v), hi.process(v));
            e_all += (v * v) as f64;
            e_lo += (l * l) as f64;
            e_hi += (h * h) as f64;
        }
        assert!(e_all > 0.0);
        assert!(e_lo / e_all > 0.85, "200 Hz 以下占 {}", e_lo / e_all);
        assert!(e_hi / e_all < 0.05, "400 Hz 以上占 {}", e_hi / e_all);
        // 输入大 6 dB，低频也大 6 dB；增益加倍也是 +6 dB
        let x2: Vec<f32> = x.iter().map(|v| 2.0 * v).collect();
        let d_in = rms_db(&run_rumble(&x2, 1.0)) - rms_db(&y);
        let d_gain = rms_db(&run_rumble(&x, 2.0)) - rms_db(&y);
        assert!((d_in - 6.02).abs() < 0.1 && (d_gain - 6.02).abs() < 0.1, "输入 ×2：{d_in} dB，增益 ×2：{d_gain} dB");
        // 增益 0 或没有声音：没有低频
        assert!(run_rumble(&x, 0.0).iter().all(|&v| v == 0.0));
        assert!(run_rumble(&vec![0.0; 48000], 1.0).iter().all(|&v| v.abs() < 1e-9));
        // 只在有摩擦声的时候响：两下之间（每 0.5 s 的后 0.15 s）比摩擦声中间低 25 dB 以上
        let seg = |a: usize, b: usize| -> Vec<f32> { y.chunks(24000).skip(2).flat_map(|c| c[a.min(c.len())..b.min(c.len())].to_vec()).collect() };
        let (on, off) = (rms_db(&seg(4800, 10800)), rms_db(&seg(19200, 23800)));
        assert!(on - off > 25.0, "摩擦时 {on} dB，间隙里 {off} dB");
    }

    /// 耳内低频的谱形：31.5–125 Hz 各 1/3 倍频程差不多一样响（粉红谱，实录如此），200 Hz 以上逐渐往下掉
    /// （实录里 400 Hz 比 100 Hz 低约 10 dB，低频与摩擦声之间是连着的），1 kHz 处已经很弱。
    #[test]
    fn rumble_is_pinkish() {
        let x = strokes(20.0, -26.0);
        let y = run_rumble(&x, 1.0);
        let band = |fc: f32| -> f32 {
            // 1/3 倍频程带通：两级二阶（高通 fc/1.12 + 低通 fc·1.12）
            let (mut a, mut b) = ([Biquad::highpass(fc / 1.122, BUTTER_Q); 2], [Biquad::lowpass(fc * 1.122, BUTTER_Q); 2]);
            let v: Vec<f32> = y
                .iter()
                .map(|&v| {
                    let mut u = v;
                    for f in a.iter_mut().chain(b.iter_mut()) {
                        u = f.process(u);
                    }
                    u
                })
                .collect();
            rms_db(&v[48000..])
        };
        let l: Vec<f32> = [31.5f32, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0].iter().map(|&f| band(f)).collect();
        let (mx, mn) = (l.iter().cloned().fold(f32::MIN, f32::max), l.iter().cloned().fold(f32::MAX, f32::min));
        assert!(mx - mn < 5.0, "31.5–125 Hz 各带 {l:?}");
        assert!(band(400.0) < mn - 5.0 && band(400.0) > mn - 16.0, "400 Hz {} dB，低频段最低 {mn} dB", band(400.0));
        assert!(band(1000.0) < mn - 18.0, "1 kHz {} dB，低频段最低 {mn} dB", band(1000.0));
    }

    #[test]
    fn soft_voice_shape() {
        let h = soft_voice_fir(1.0);
        let m = fir_mag(&h, 4096);
        let db = |f: f32| 20.0 * m[(f / FS * 4096.0).round() as usize].log10();
        // Python 版实测：低频约 +2.7 dB，1–2.5 kHz 约 −2.5…−2.8 dB，最高频约 +1.5 dB
        assert!(db(200.0) > 1.8 && db(200.0) < 3.2, "200 Hz {}", db(200.0));
        assert!(db(1600.0) < -2.0 && db(1600.0) > -3.2, "1.6 kHz {}", db(1600.0));
        assert!(db(8000.0) > 0.8 && db(8000.0) < 2.2, "8 kHz {}", db(8000.0));
    }

    /// 确定性的噪声：white = 白噪声；否则一阶低通过的"风扇 / 空调"式低频噪声。有效值 `rms_db` dBFS。
    fn noise(n: usize, rms_db: f32, white: bool) -> Vec<f32> {
        let mut seed = 12345u32;
        let mut lp = 0.0f32;
        let mut x: Vec<f32> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let w = (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0;
                lp += 0.05 * (w - lp);
                if white {
                    w
                } else {
                    lp
                }
            })
            .collect();
        let rms = (x.iter().map(|v| v * v).sum::<f32>() / n as f32).sqrt();
        let g = crate::dsp::db_to_lin(rms_db) / rms;
        x.iter_mut().for_each(|v| *v *= g);
        x
    }

    fn rms_db(x: &[f32]) -> f32 {
        10.0 * (x.iter().map(|v| (v * v) as f64).sum::<f64>() / x.len() as f64 + 1e-30).log10() as f32
    }

    fn run_denoiser(x: &[f32], mix: f32) -> Vec<f32> {
        let mut d = Denoiser::new();
        x.iter().map(|&v| d.process(v, mix)).collect()
    }

    fn attenuation(x: &[f32], db: f32) -> f32 {
        let p = crate::dsp::params::Params { denoise_db: db, ..Default::default() };
        let y = run_denoiser(x, p.denoise_mix());
        rms_db(&x[3 * 48000..]) - rms_db(&y[3 * 48000..])
    }

    #[test]
    fn denoise_strength_is_a_fixed_number_of_db() {
        use crate::dsp::params::DENOISE_MAX_DB;
        const DENOISE_STEPS: [f32; 5] = [6.0, 12.0, 18.0, 24.0, 30.0];
        // 稳定的低频底噪（风扇 / 空调一类），降噪器自己能把它压得很干净：设多少分贝就降多少分贝
        let fan = noise(6 * 48000, -40.0, false);
        for db in DENOISE_STEPS {
            let att = attenuation(&fan, db);
            assert!((att - db).abs() < 0.6, "set {db} dB, got {att} dB");
        }
        // 拖到头（100）不设上限，比 30 dB 干净得多；60 以上降噪器自己已经到底，差别很小
        assert!(attenuation(&fan, DENOISE_MAX_DB) > 40.0);
        assert!(attenuation(&fan, 60.0) > 40.0);
        // 0 就是原样通过
        assert!(attenuation(&fan, 0.0).abs() < 0.01);
        // 嘶嘶的白噪声，降噪器自己只能压低二十多分贝：实际降的不会超过设定值，小档位仍然基本到位
        let hiss = noise(6 * 48000, -55.0, true);
        for db in DENOISE_STEPS {
            let att = attenuation(&hiss, db);
            assert!(att < db + 0.3 && att > db.min(24.0) - 3.0, "hiss: set {db} dB, got {att} dB");
        }
    }

    #[test]
    fn denoiser_keeps_the_voice_and_stays_aligned() {
        // 合成的"人声"（带谐波的浊音 + 一点噪声）：降噪后音量基本不变；
        // 原声那一路和降噪后那一路严格对齐（差一个样本，按比例混合时就会出梳状滤波）
        let v = crate::dsp::testsig::test_voice(6.0);
        let p = crate::dsp::params::Params { denoise_db: 12.0, ..Default::default() };
        for mix in [p.denoise_mix(), 1.0] {
            let y = run_denoiser(&v, mix);
            let (i0, n) = (2 * 48000usize, 3 * 48000usize);
            let a = &v[i0..i0 + n];
            let corr = |lag: i32| -> f64 { a.iter().enumerate().map(|(i, &u)| u as f64 * y[(i0 + DENOISE_DELAY + i) as i32 as usize + lag as usize] as f64).sum() };
            let best = (-8i32..=8).max_by(|&p, &q| corr(p).total_cmp(&corr(q))).unwrap();
            assert_eq!(best, 0);
            let change = rms_db(&y[i0 + DENOISE_DELAY..i0 + DENOISE_DELAY + n]) - rms_db(a);
            assert!(change.abs() < 1.5, "voice level changed by {change} dB");
        }
    }

    #[test]
    fn deesser_is_transparent_on_vowels() {
        let mut d = DeEsser::new();
        let w = 2.0 * std::f32::consts::PI * 300.0 / FS;
        let (mut ein, mut eout) = (0.0, 0.0);
        for n in 0..48000 {
            let x = 0.3 * (w * n as f32).sin();
            let y = d.process(x, 6.0);
            if n > 4800 {
                ein += x * x;
                eout += y * y;
            }
        }
        assert!((10.0 * (eout / ein).log10()).abs() < 0.05);
    }
}
