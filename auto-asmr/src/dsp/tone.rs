//! 音色校正，和"和原声一样响"用的静态估计。
//!
//! 声源待在一个固定位置时，从输入到两只耳朵的整体频响是算得出来的：直达声（那个方向、距离的 HRIR × 回放补偿）
//! 加房间声。这里把它按 1/3 倍频程算出来（输出响度的功率 ÷ 输入响度的功率，见 [`render_bands`]），得到两样东西：
//!
//! - **音色校正**（[`design`]）：把这条曲线的起伏均衡掉的一串峰值滤波器，加在引擎的输入端（直达声和房间声都经过它）。
//!   两只耳朵用的是同一个均衡，所以两耳之间的差别——声级差、时间差、各自的谱形——一点不变，变的只是整体音色。
//! - **响度估计**（[`gain_db`]）：这个位置的渲染会让响度变化多少 dB。"和原声一样响"的自动响度从它的相反数起步，
//!   一开头就差不多是对的，不用从 0 dB 慢慢爬过去。
//!
//! 立体声对（左右声道各一个声源）：歌里既有只在一个声道里的内容，也有两个声道里一样的内容（人声、底鼓、贝斯大多在中间）。
//! 中间的内容由两个声源同时发出，在耳朵里相干叠加——低频多出 3 dB，1–2 kHz 有梳状的谷；只在一侧的内容没有这些。
//! 校正曲线按"一半内容在中间、一半在两侧"取平均，两种内容都不会偏得太多。

use super::dynamics::KWeight;
use super::engine::closeness;
use super::filters::{interp_log, Biquad};
use super::motion::lift_tilt;
use super::params::{MotionMode, Params};
use super::room::{sector_weights, NSEC};
use super::tables::{Hrir, Tables};
use super::{db_to_lin, smoothstep, FS, R_MAX, R_MIN};
use std::sync::OnceLock;

/// 1/3 倍频程带的个数和中心频率（25 Hz … 16 kHz）。
pub const NB: usize = 29;
pub const BANDS: [f32; NB] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0, 10000.0,
    12500.0, 16000.0,
];
/// 每个频带里取几个频率点（对数均匀）。
const PTS: usize = 4;

/// 校正滤波器：每个倍频程一个峰值滤波器。
pub const N_TONE: usize = 10;
const TONE_FC: [f32; N_TONE] = [31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
const TONE_Q: f32 = 1.25;
/// 最多提这么多、压这么多（dB）：窄而深的耳廓谷不去填，免得换一个人的耳朵就变刺。
const TONE_MAX_BOOST: f32 = 6.0;
const TONE_MAX_CUT: f32 = 9.0;

/// 一组校正滤波器（只有系数，状态是 0；引擎里另有一份带状态的）。
#[derive(Clone, Copy, Debug)]
pub struct ToneEq {
    pub sos: [Biquad; N_TONE],
    /// 各倍频程的增益（dB），给界面和测试看。
    pub gains_db: [f32; N_TONE],
}

impl ToneEq {
    /// 频率 `f` 处的功率增益。
    pub fn mag2(&self, f: f32) -> f32 {
        self.sos.iter().map(|b| b.mag2(f)).product()
    }
}

/// 声源"待着不动"的位置（方位角 顺时针度, 仰角, 到头中心的距离），以及它是不是固定的：固定、手动摆位时就是
/// 那个位置（带高度），音色校正只在这两种方式下做；其余方式取开场的位置（穿梭 / 随机走位：贴左耳；环绕：起点），
/// 只用来估计响度。
pub fn rest_pose(p: &Params) -> ((f32, f32, f32), bool) {
    let tilt = p.height.clamp(-80.0, 85.0);
    match p.mode {
        MotionMode::Static => (lift_tilt(p.static_az, p.ear_dist.clamp(R_MIN, R_MAX), 0.0, tilt), true),
        MotionMode::Manual => (lift_tilt(p.manual_az, p.manual_dist.clamp(R_MIN, R_MAX), 0.0, p.manual_tilt.clamp(-80.0, 85.0)), true),
        MotionMode::Orbit => (lift_tilt(p.static_az, p.orbit_radius.clamp(R_MIN, R_MAX), 0.0, tilt), false),
        MotionMode::Sweep | MotionMode::Random => (lift_tilt(360.0 - p.ear_angle, p.ear_dist.clamp(R_MIN, R_MAX), 0.0, tilt), false),
    }
}

/// 频带里的第 j 个频率点。
#[inline]
fn band_pt(b: usize, j: usize) -> f32 {
    BANDS[b] * 2f32.powf(((j as f32 + 0.5) / PTS as f32 - 0.5) / 3.0)
}

/// FIR 在频率 `f` 处的复数频响 Σ h[n]·e^{−jωn}。
fn dft(h: &[f32], f: f32) -> (f64, f64) {
    let w = std::f64::consts::TAU * f as f64 / FS as f64;
    let (s, c) = w.sin_cos();
    let (mut re, mut im) = (0f64, 0f64);
    let (mut cr, mut ci) = (1f64, 0f64);
    for &v in h {
        re += v as f64 * cr;
        im += v as f64 * ci;
        let t = cr * c + ci * s;
        ci = ci * c - cr * s;
        cr = t;
    }
    (re, im)
}

/// 实测房间各扇区、各耳在每个频带里的平均功率增益 `[扇区][耳][频带]`（只算一次）。
fn room_bands(tab: &'static Tables) -> &'static [f32] {
    static R: OnceLock<Vec<f32>> = OnceLock::new();
    R.get_or_init(|| {
        use rustfft::{num_complex::Complex32, FftPlanner};
        let n = tab.room_len.next_power_of_two();
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
        let mut out = vec![0f32; NSEC * 2 * NB];
        let mut buf = vec![Complex32::new(0.0, 0.0); n];
        for sec in 0..NSEC {
            for ear in 0..2 {
                buf.fill(Complex32::new(0.0, 0.0));
                for (b, &v) in buf.iter_mut().zip(tab.room_ir(sec, ear)) {
                    b.re = v;
                }
                fft.process(&mut buf);
                for (bi, &fc) in BANDS.iter().enumerate() {
                    let (lo, hi) = (fc / 2f32.powf(1.0 / 6.0), fc * 2f32.powf(1.0 / 6.0));
                    let k0 = ((lo * n as f32 / FS).floor() as usize).max(1);
                    let k1 = ((hi * n as f32 / FS).ceil() as usize).clamp(k0, n / 2);
                    let s: f64 = buf[k0..=k1].iter().map(|c| c.norm_sqr() as f64).sum();
                    out[(sec * 2 + ear) * NB + bi] = (s / (k1 - k0 + 1) as f64) as f32;
                }
            }
        }
        out
    })
}

/// 一个声源在两只耳朵里的直达声（各频率点的复数频响，已含响度补偿、远耳压低和两耳的相对时延）和房间声（各频带的功率）。
struct Source {
    /// `[耳][频带 × PTS]`（实部, 虚部）
    direct: [Vec<(f64, f64)>; 2],
    /// `[耳][频带]`
    room: [[f64; NB]; 2],
}

fn source(tab: &'static Tables, p: &Params, room_gain: f32, pref: f32, az: f32, el: f32, r: f32) -> Source {
    let r = r.clamp(R_MIN, R_MAX);
    let az_sofa = (360.0 - az).rem_euclid(360.0);
    let (mut h, mut tmp) = (Hrir::default(), Hrir::default());
    tab.lookup3(az_sofa, el, r, &mut h, &mut tmp);
    // 响度随位置变化（与引擎 begin_block 相同）
    let p_base = tab.lookup_power3(az_sofa, el, r).max(1e-20);
    let g = if p.level_follow != 1.0 { (p_base / pref).powf(-(1.0 - p.level_follow) / 2.0) } else { 1.0 };
    let cl = closeness(az, el, r);
    let ipsi = if az.rem_euclid(360.0) < 180.0 { 1 } else { 0 };
    let mut direct = [Vec::with_capacity(NB * PTS), Vec::with_capacity(NB * PTS)];
    for e in 0..2 {
        let ge = (g * if e != ipsi && p.isolation_db > 0.0 { db_to_lin(-p.isolation_db * cl) } else { 1.0 }) as f64;
        for b in 0..NB {
            for j in 0..PTS {
                let f = band_pt(b, j);
                let (re, im) = dft(&h.h[e], f);
                // 两耳的相对时延（样本）：只影响两个声源相干叠加时的相位
                let ph = -std::f64::consts::TAU * f as f64 / FS as f64 * h.tau[e] as f64;
                let (s, c) = ph.sin_cos();
                direct[e].push((ge * (re * c - im * s), ge * (re * s + im * c)));
            }
        }
    }
    let mut room = [[0f64; NB]; 2];
    if p.room {
        let rb = room_bands(tab);
        // 房间声低切：送进房间之前的高通
        let hp = super::engine::room_lowcut(p);
        let mut w = sector_weights(az);
        let up = smoothstep((el.abs() - 45.0) / 35.0);
        if up > 0.0 {
            for v in w.iter_mut() {
                *v = *v * (1.0 - up) + up / NSEC as f32;
            }
        }
        for (sec, &ws) in w.iter().enumerate() {
            if ws == 0.0 {
                continue;
            }
            for e in 0..2 {
                for b in 0..NB {
                    room[e][b] += (room_gain * room_gain * ws * ws * rb[(sec * 2 + e) * NB + b] * hp[0].mag2(BANDS[b]) * hp[1].mag2(BANDS[b])) as f64;
                }
            }
        }
    }
    Source { direct, room }
}

/// 一个固定位置的渲染在各 1/3 倍频程带上的"输出响度的功率 ÷ 输入响度的功率"（BS.1770 的算法：各声道的功率相加）。
///
/// - `width` 为 None：一个声源在 `pose`。输入按"单声道放进两个声道"算（功率 2P），输出是两只耳朵的功率之和。
/// - `width` 为 Some(度)：立体声对，左右声道各一个声源，在 `pose` 两侧各偏一半。输入是立体声（功率 = 左 + 右），
///   按"一半内容在中间、一半在两侧"取平均。
///
/// `playback`：回放补偿滤波器；`room_gain`、`pref`：引擎配置里算好的房间声增益和参考功率。
pub fn render_bands(tab: &'static Tables, p: &Params, playback: Option<&[f32]>, room_gain: f32, pref: f32, pose: (f32, f32, f32), width: Option<f32>) -> [f32; NB] {
    let (az, el, r) = pose;
    let srcs: Vec<Source> = match width {
        Some(w) => vec![source(tab, p, room_gain, pref, az - 0.5 * w, el, r), source(tab, p, room_gain, pref, az + 0.5 * w, el, r)],
        None => vec![source(tab, p, room_gain, pref, az, el, r)],
    };
    let mut out = [0f32; NB];
    for b in 0..NB {
        // 回放补偿在这个频带里的平均功率增益
        let pb: f64 = match playback {
            Some(h) => {
                (0..PTS)
                    .map(|j| {
                        let (re, im) = dft(h, band_pt(b, j));
                        re * re + im * im
                    })
                    .sum::<f64>()
                    / PTS as f64
            }
            None => 1.0,
        };
        let pw = |s: &Source, e: usize| -> f64 { s.direct[e][b * PTS..(b + 1) * PTS].iter().map(|&(re, im)| re * re + im * im).sum::<f64>() / PTS as f64 };
        let v = match srcs.as_slice() {
            [s] => 0.5 * (0..2).map(|e| pw(s, e) + s.room[e][b]).sum::<f64>(),
            [l, r] => {
                // 只在一侧的内容：两个声道各自的结果取平均
                let side = 0.5 * (0..2).map(|e| pw(l, e) + l.room[e][b] + pw(r, e) + r.room[e][b]).sum::<f64>();
                // 中间的内容（两个声道一样）：两个声源的直达声相干叠加，房间声按功率相加；输入功率是 2P
                let center = 0.5
                    * (0..2)
                        .map(|e| {
                            let co: f64 = (0..PTS)
                                .map(|j| {
                                    let (a, c) = (l.direct[e][b * PTS + j], r.direct[e][b * PTS + j]);
                                    (a.0 + c.0).powi(2) + (a.1 + c.1).powi(2)
                                })
                                .sum::<f64>()
                                / PTS as f64;
                            co + l.room[e][b] + r.room[e][b]
                        })
                        .sum::<f64>();
                0.5 * (side + center)
            }
            _ => unreachable!(),
        };
        out[b] = (v * pb) as f32;
    }
    out
}

/// 响度的频率加权：粉红噪声（每个 1/3 倍频程一样多）× BS.1770 的 K 加权，只算 40 Hz–12.5 kHz。
fn loud_weights() -> [f32; NB] {
    let kw = KWeight::new();
    std::array::from_fn(|b| if (40.0..=12500.0).contains(&BANDS[b]) { kw.mag2(BANDS[b]) } else { 0.0 })
}

/// 这个位置的渲染让响度变化多少 dB（典型音乐的频谱；`eq`：音色校正，有的话算在内）。
pub fn gain_db(ratio: &[f32; NB], eq: Option<&ToneEq>) -> f32 {
    let w = loud_weights();
    let (mut num, mut den) = (0f64, 0f64);
    for b in 0..NB {
        let c = eq.map_or(1.0, |q| q.mag2(BANDS[b]));
        num += (w[b] * ratio[b] * c) as f64;
        den += w[b] as f64;
    }
    10.0 * ((num / den.max(1e-30)).max(1e-30)).log10() as f32
}

/// 由 [`render_bands`] 的结果设计音色校正：把曲线相对它自己的响度平均值的起伏反过来（先在频率上平滑到约 2/3 倍频程，
/// 限制在 +6 / −9 dB 之内），乘上强度 `strength`（0–1），用每个倍频程一个的峰值滤波器去逼近。
/// 强度为 0、或曲线本来就是平的时返回 None。
pub fn design(ratio: &[f32; NB], strength: f32) -> Option<ToneEq> {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.0 {
        return None;
    }
    let mean = gain_db(ratio, None);
    let mut t: [f32; NB] = std::array::from_fn(|b| -(10.0 * ratio[b].max(1e-20).log10() - mean));
    // 40 Hz 以下、12.5 kHz 以上保持边上的值（那里测量和滤波器本身都不可靠）
    let (lo, hi) = (BANDS.iter().position(|&f| f >= 40.0).unwrap(), BANDS.iter().rposition(|&f| f <= 12500.0).unwrap());
    for b in 0..NB {
        t[b] = t[b.clamp(lo, hi)];
    }
    for _ in 0..2 {
        let s = t;
        for b in 0..NB {
            t[b] = 0.25 * s[b.saturating_sub(1)] + 0.5 * s[b] + 0.25 * s[(b + 1).min(NB - 1)];
        }
    }
    for v in t.iter_mut() {
        *v = strength * v.clamp(-TONE_MAX_CUT, TONE_MAX_BOOST);
    }
    // 每个倍频程一个峰值滤波器：相邻的互相有影响，迭代几次让各中心频率上正好等于目标
    let want: [f32; N_TONE] = std::array::from_fn(|i| interp_log(TONE_FC[i], &BANDS, &t));
    let mut g = want;
    let build = |g: &[f32; N_TONE]| -> [Biquad; N_TONE] { std::array::from_fn(|i| Biquad::peaking(TONE_FC[i], TONE_Q, g[i])) };
    for _ in 0..12 {
        let sos = build(&g);
        for i in 0..N_TONE {
            let got: f32 = sos.iter().map(|b| 10.0 * b.mag2(TONE_FC[i]).log10()).sum();
            g[i] = (g[i] + 0.7 * (want[i] - got)).clamp(-15.0, 12.0);
        }
    }
    if g.iter().all(|v| v.abs() < 0.05) {
        return None;
    }
    Some(ToneEq { sos: build(&g), gains_db: g })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::tables::tables;

    fn music_like() -> Params {
        Params { mode: MotionMode::Static, static_az: 0.0, ear_dist: 0.75, level_follow: 1.0, isolation_db: 0.0, room: false, ..Params::default() }
    }

    fn bands(p: &Params, width: Option<f32>) -> [f32; NB] {
        let tab = tables();
        let pref = tab.lookup_power(270.0, 0.14);
        render_bands(tab, p, Some(&tab.cdfc), 0.0, pref, rest_pose(p).0, width)
    }

    /// 正前方的声源相对扩散场均衡偏亮（3–5 kHz 多几分贝）；校正之后整体频谱是平的，而且没改响度。
    #[test]
    fn correction_flattens_a_frontal_source() {
        let p = music_like();
        let r = bands(&p, None);
        let db = |f: f32| 10.0 * r[BANDS.iter().position(|&b| b == f).unwrap()].log10();
        assert!(db(4000.0) - db(250.0) > 2.0, "正前方 4 kHz 应比 250 Hz 高：{:.1} dB", db(4000.0) - db(250.0));
        let eq = design(&r, 1.0).expect("应当需要校正");
        let after: Vec<f32> = (0..NB).map(|b| 10.0 * (r[b] * eq.mag2(BANDS[b])).log10()).collect();
        let mean = gain_db(&r, Some(&eq));
        // 100 Hz–5 kHz 之间校正后离平均不超过 1.5 dB（8 kHz 的耳廓谷又窄又深，只补一部分）
        for b in 0..NB {
            if (100.0..=5000.0).contains(&BANDS[b]) {
                assert!((after[b] - mean).abs() < 1.5, "{} Hz 校正后 {:.1} dB", BANDS[b], after[b] - mean);
            }
        }
        assert!((mean - gain_db(&r, None)).abs() < 1.0, "校正不该改响度：{:.2} → {:.2}", gain_db(&r, None), mean);
        // 强度减半：各滤波器的增益约减半；强度 0：没有校正
        let half = design(&r, 0.5).unwrap();
        for i in 0..N_TONE {
            assert!((half.gains_db[i] - 0.5 * eq.gains_db[i]).abs() < 0.6, "{:?} / {:?}", half.gains_db, eq.gains_db);
        }
        assert!(design(&r, 0.0).is_none());
    }

    /// 立体声对：中间的内容低频比两侧的内容多约 3 dB（两个声源相干叠加），两侧各偏一半；宽度为 0 时两个声源重合。
    #[test]
    fn stereo_pair_counts_centre_and_side_content() {
        let p = music_like();
        let (one, pair0, pair60) = (bands(&p, None), bands(&p, Some(0.0)), bands(&p, Some(60.0)));
        let i = BANDS.iter().position(|&b| b == 100.0).unwrap();
        // 两个重合的声源：只在一侧的内容 = 单个声源的 2 倍（输入功率 P，输出两耳之和），中间的内容 = 4 倍（相干叠加）
        assert!((pair0[i] / one[i] - 3.0).abs() < 0.05, "{}", pair0[i] / one[i]);
        // 张开 60°：低频几乎不变（波长远大于头）
        assert!((10.0 * (pair60[i] / pair0[i]).log10()).abs() < 1.0);
        // 位置：固定模式用固定的方位，手动用手动的位置，别的方式不算固定
        assert_eq!(rest_pose(&p), ((0.0, 0.0, 0.75), true));
        assert!(!rest_pose(&Params::default()).1 && !rest_pose(&Params { mode: MotionMode::Orbit, ..Params::default() }).1);
        let m = Params { mode: MotionMode::Manual, manual_az: 90.0, manual_dist: 0.3, manual_tilt: 0.0, ..Params::default() };
        assert_eq!(rest_pose(&m), ((90.0, 0.0, 0.3), true));
    }

    /// 房间声算进去：打开房间后总能量变多，而且房间声越大多得越多。
    #[test]
    fn room_energy_is_included() {
        let tab = tables();
        let pref = tab.lookup_power(270.0, 0.14);
        let p = Params { room: true, ..music_like() };
        let e = |g: f32| {
            let r = render_bands(tab, &p, Some(&tab.cdfc), g, pref, rest_pose(&p).0, None);
            gain_db(&r, None)
        };
        assert!(e(0.0) < e(1.0) && e(1.0) < e(3.0), "{} {} {}", e(0.0), e(1.0), e(3.0));
        let dry = Params { room: false, ..p.clone() };
        let r = render_bands(tab, &dry, Some(&tab.cdfc), 3.0, pref, rest_pose(&dry).0, None);
        assert!((gain_db(&r, None) - e(0.0)).abs() < 1e-3);
    }
}
