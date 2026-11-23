//! 运动：左右移动（穿梭 / 随机走位 / 环绕 / 固定）、人体自然晃动、忽远忽近、上下（高度 / 自动上下 / 从头顶绕）。
//! 全部按"说话时钟"逐块在线生成：说话时按真实时间走，停顿时几乎不动，所以实时和离线用同一套代码。

use super::filters::{Biquad, BUTTER_Q};
use super::params::{MotionMode, Params, Via};
use super::rng::Rng;
use super::{smoothstep, CANAL_L0, CANAL_L_MIN, CTRL_DT, FS, HEAD_A, INEAR_MAX_CM, R_MAX, R_MIN};
use std::collections::VecDeque;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

/// 一个控制块边界上的位置。
#[derive(Clone, Copy, Debug, Default)]
pub struct Pose {
    /// 方位角（度，顺时针：0 前 90 右 180 后 270 左）。
    pub az: f32,
    /// 仰角（度，向上为正；0 = 与耳朵齐平）。
    pub el: f32,
    /// 不含忽远忽近的距离（到头中心，米）——响度补偿按它算。
    pub r_base: f32,
    /// 实际距离（含忽远忽近）。
    pub r: f32,
    /// 不含忽远忽近的位置的方位角、仰角（带高度时与 `az` / `el` 略有不同）。
    pub az_base: f32,
    pub el_base: f32,
    /// 入耳深度（米）：声源贴在耳朵上时伸进耳道多深；0 = 没有进耳道。
    pub depth: f32,
}

/// 贴到耳朵上多近才算"可以伸进耳道"：贴耳程度从 0.80 到 0.95 渐入。
#[inline]
fn inear_gate(az: f32, el: f32, r: f32) -> f32 {
    smoothstep((super::engine::closeness(az, el, r.clamp(R_MIN, R_MAX)) - 0.80) / 0.15)
}

/// 进出手势（0–1）换成入耳深度（米）。手势在"共振频率的对数"上是线性的——实录里共振峰在对数频率轴上
/// 走的是直线（V 字形），所以剩余的耳道长度按几何插值：L = L0^(1−u) · Lmin^u，深度 = L0 − L。
#[inline]
pub fn inear_depth(u: f32, inear_cm: f32) -> f32 {
    let l_min = (CANAL_L0 - 0.01 * inear_cm.clamp(0.0, INEAR_MAX_CM)).max(CANAL_L_MIN);
    (CANAL_L0 - CANAL_L0.powf(1.0 - u) * l_min.powf(u)).max(0.0)
}

/// 把水平面上的位置（方位角 `az`、距离 `r`）抬离水平面，返回（方位角, 仰角, 距离）。
///  1. `lift`：整个位置绕两耳连线向上转这么多度（从头顶绕时用；正前方转 90° 就到了头顶）。
///  2. `tilt`：以"耳朵"为支点向上（正）/ 向下（负）倾这么多度，嘴到耳朵的距离不变——贴耳时就是嘴绕着耳朵
///     上下挪。支点随声源的侧向程度在两耳之间滑动：声源在正侧方时支点就是那只耳朵，在正前 / 正后方时是头中心
///     （这时就是普通的仰角）。从头顶绕到最高处时 `tilt` 不起作用。
///
/// 手动模式：位置（方位角, 仰角, 距离）朝目标走一个控制块。时间常数约 0.1 秒，拖动时跟手；
/// 同时限制最快速度（方位角每秒 300°、仰角每秒 240°、距离每秒 1.2 米），所以一下子点到另一侧时
/// 声音是用大约 0.6 秒很快地绕过去，而不是跳过去——跳变会产生咔嗒声。按真实时间走，与说不说话无关。
fn approach(cur: (f32, f32, f32), tgt: (f32, f32, f32)) -> (f32, f32, f32) {
    const TAU_S: f32 = 0.10;
    let k = 1.0 - (-CTRL_DT / TAU_S).exp();
    let step = |err: f32, cap_per_s: f32| (k * err).clamp(-cap_per_s * CTRL_DT, cap_per_s * CTRL_DT);
    let daz = (tgt.0 - cur.0 + 180.0).rem_euclid(360.0) - 180.0;
    ((cur.0 + step(daz, 300.0)).rem_euclid(360.0), cur.1 + step(tgt.1 - cur.1, 240.0), cur.2 + step(tgt.2 - cur.2, 1.2))
}

/// 两个位置（方位角, 仰角, 距离）之间按比例 `s` 过渡，方位角走近的那一边。
fn mix_pos(a: (f32, f32, f32), b: (f32, f32, f32), s: f32) -> (f32, f32, f32) {
    let d = (b.0 - a.0 + 180.0).rem_euclid(360.0) - 180.0;
    ((a.0 + d * s).rem_euclid(360.0), a.1 + (b.1 - a.1) * s, a.2 + (b.2 - a.2) * s)
}

/// 从手动切回自动后，用这么久（秒）平滑过渡到自动的走位。
const MANUAL_LEAVE_S: f32 = 0.6;

/// 两者都为 0 时原样返回，仰角为 0。
pub fn lift_tilt(az: f32, r: f32, lift: f32, tilt: f32) -> (f32, f32, f32) {
    if lift == 0.0 && tilt == 0.0 {
        return (az, 0.0, r);
    }
    let a = az.to_radians();
    let (x, y) = (r * a.sin(), r * a.cos()); // x 朝右，y 朝前
    let rho = lift.to_radians();
    let (y2, z2) = (y * rho.cos(), y * rho.sin());
    let cx = HEAD_A * x / r.max(1e-9);
    let vx = x - cx;
    let vh = vx.hypot(y2);
    let d = vh.hypot(z2);
    let phi = (z2.atan2(vh) + tilt.to_radians() * rho.cos()).clamp(-FRAC_PI_2, FRAC_PI_2);
    let (ux, uy) = (vx / vh.max(1e-9), y2 / vh.max(1e-9));
    let (px, py, pz) = (cx + ux * d * phi.cos(), uy * d * phi.cos(), d * phi.sin());
    let rr = (px * px + py * py + pz * pz).sqrt();
    let el = (pz / rr.max(1e-9)).clamp(-1.0, 1.0).asin().to_degrees();
    (px.atan2(py).to_degrees().rem_euclid(360.0), el, rr.clamp(R_MIN, R_MAX))
}

/// 高度角（度）：以"耳朵"为支点看声源在上方还是下方多少度——与运动里倾斜用的支点相同
/// （声源在正侧方时支点是那只耳朵，在正前 / 正后方时是头中心）。同时返回水平面上的"未抬高"位置
/// （方位角, 到头中心的距离），用来把一个三维位置换回手动模式的三个量。[`lift_tilt`] 的逆运算。
/// 桌面版位置图和安卓版的「手动摆位」都用它。
pub fn up_angle(az: f32, el: f32, r: f32) -> (f32, f32, f32) {
    let (a, e) = (az.to_radians(), el.to_radians());
    let (x, y, z) = (r * e.cos() * a.sin(), r * e.cos() * a.cos(), r * e.sin());
    // 支点的位置取决于"未抬高"时的方位，先用当前方位估计，迭代几次就收敛
    let mut cx = HEAD_A * x / r.max(1e-6);
    let (mut up, mut px, mut py) = (0.0, x, y);
    for _ in 0..6 {
        let vh = (x - cx).hypot(y);
        let d = vh.hypot(z);
        up = z.atan2(vh).to_degrees();
        px = cx + (x - cx) / vh.max(1e-6) * d;
        py = y / vh.max(1e-6) * d;
        cx = HEAD_A * px / px.hypot(py).max(1e-6);
    }
    (up, px.atan2(py).to_degrees().rem_euclid(360.0), px.hypot(py))
}

#[derive(Clone, Copy)]
struct Seg {
    t0: f64,
    t1: f64,
    a0: f32,
    d: f32,
    r0: f32,
    r1: f32,
    bump: f32,
    /// 从头顶绕：方位角照"从前面绕"走，同时整条路径绕两耳连线向上转 90°。
    top: bool,
}

/// 从 a0 到 a1 的方位角增量（顺时针为正）。只有接近对穿（如左耳到右耳）时才需要选从前面还是后面绕。
fn arc_delta(a0: f32, a1: f32, front: Option<bool>) -> f32 {
    let d = (a1 - a0 + 180.0).rem_euclid(360.0) - 180.0;
    match front {
        None => d,
        Some(_) if d.abs() < 120.0 => d,
        Some(front) => {
            let dcw = (a1 - a0).rem_euclid(360.0);
            let cw_front = a0.rem_euclid(360.0) + dcw >= 360.0; // 顺时针这条弧是否经过正前方
            if cw_front == front {
                dcw
            } else {
                dcw - 360.0
            }
        }
    }
}

pub struct Motion {
    rng: Rng,
    segs: VecDeque<Seg>,
    plan_az: f32,
    plan_r: f32,
    az: f32,
    r: f32,
    next_t: f64,
    right: bool,
    k: u32,
    at_ear: bool,
    mode: Option<MotionMode>,
    pause_switch: bool,
    last_breath: Option<f64>,
    last_ear_dist: f32,
    orbit_t0: f64,
    // 自然晃动：X / Y 各 4 条慢速正弦
    micro: [[(f32, f32, f32); 4]; 2],
    micro_norm: [f32; 2],
    // 忽远忽近
    sw_rng: Rng,
    sw_k0: (f64, f32),
    sw_k1: (f64, f32),
    sw_stage: u8,
    sw_depth: f32,
    sw_in: f32,
    sw_init: bool,
    w_lp: Biquad,
    w_primed: bool,
    // 上下
    top_rng: Rng,
    lift: f32,
    ud_rng: Rng,
    ud_k0: (f64, f32),
    ud_k1: (f64, f32),
    ud_move: bool,
    ud_sign: f32,
    ud_init: bool,
    micro_z: [(f32, f32, f32); 4],
    micro_z_norm: f32,
    // 入耳进出
    dp_rng: Rng,
    dp_k0: (f64, f32),
    dp_k1: (f64, f32),
    dp_stage: u8,
    dp_deep: f32,
    dp_init: bool,
    dp_lp: Biquad,
    dp_on: bool,
    // 手动摆位
    /// 上一步输出的位置（方位角, 仰角, 距离）：切到手动时从这里出发。
    last: (f32, f32, f32),
    /// 手动模式下平滑后的当前位置。
    man: Option<(f32, f32, f32)>,
    /// 刚从手动切回自动：（切换时的位置, 已经过去的秒数）。
    leave: Option<((f32, f32, f32), f32)>,
    /// 统计：换耳次数、往后让的次数、从头顶绕的次数、上下换位的次数。
    pub n_moves: u32,
    pub n_sways: u32,
    pub n_tops: u32,
    pub n_updowns: u32,
    /// 统计：伸进耳道的次数。
    pub n_strokes: u32,
}

impl Motion {
    pub fn new(seed: u64) -> Self {
        let mut mr = Rng::new(seed.wrapping_mul(2).wrapping_add(11));
        let mut micro = [[(0.0, 0.0, 0.0); 4]; 2];
        let mut micro_norm = [1.0; 2];
        for (ax, m) in micro.iter_mut().enumerate() {
            let mut p = 0.0;
            for c in m.iter_mut() {
                *c = (mr.uniform(0.06, 0.45), mr.uniform(0.0, TAU), mr.uniform(0.5, 1.0));
                p += c.2 * c.2 / 2.0;
            }
            micro_norm[ax] = p.sqrt();
        }
        let mut zr = Rng::new(seed.wrapping_mul(2).wrapping_add(13));
        let mut micro_z = [(0.0, 0.0, 0.0); 4];
        let mut pz = 0.0;
        for c in micro_z.iter_mut() {
            *c = (zr.uniform(0.06, 0.45), zr.uniform(0.0, TAU), zr.uniform(0.5, 1.0));
            pz += c.2 * c.2 / 2.0;
        }
        Motion {
            rng: Rng::new(seed),
            segs: VecDeque::new(),
            plan_az: 270.0,
            plan_r: 0.105,
            az: 270.0,
            r: 0.105,
            next_t: 0.0,
            right: false,
            k: 0,
            at_ear: true,
            mode: None,
            pause_switch: false,
            last_breath: None,
            last_ear_dist: 0.105,
            orbit_t0: 0.0,
            micro,
            micro_norm,
            sw_rng: Rng::new(seed.wrapping_add(202)),
            sw_k0: (0.0, 0.0),
            sw_k1: (0.0, 0.0),
            sw_stage: 0,
            sw_depth: 1.5,
            sw_in: 0.0,
            sw_init: false,
            w_lp: Biquad::lowpass_at(1.0, BUTTER_Q, FS / super::CTRL as f32),
            w_primed: false,
            top_rng: Rng::new(seed.wrapping_add(404)), // 单独的随机数，不打乱原来的走位
            lift: 0.0,
            ud_rng: Rng::new(seed.wrapping_add(303)),
            ud_k0: (0.0, 0.0),
            ud_k1: (0.0, 0.0),
            ud_move: false,
            ud_sign: 1.0,
            ud_init: false,
            micro_z,
            micro_z_norm: pz.sqrt(),
            dp_rng: Rng::new(seed.wrapping_add(505)), // 单独的随机数，不打乱原来的走位
            dp_k0: (0.0, 0.0),
            dp_k1: (0.0, 0.0),
            dp_stage: 0,
            dp_deep: 0.9,
            dp_init: false,
            dp_lp: Biquad::lowpass_at(2.5, BUTTER_Q, FS / super::CTRL as f32),
            dp_on: false,
            last: (270.0, 0.0, 0.105),
            man: None,
            leave: None,
            n_moves: 0,
            n_sways: 0,
            n_tops: 0,
            n_updowns: 0,
            n_strokes: 0,
        }
    }

    fn jitter(&mut self, p: &Params, base: f32) -> f32 {
        (self.rng.uniform(-1.0, 1.0) * base * (p.randomness / 0.1).clamp(0.0, 12.0)).exp()
    }

    fn push_move(&mut self, t0: f64, dur: f32, az1: f32, r1: f32, front: Option<bool>, pass_r: Option<f32>) {
        let d = arc_delta(self.plan_az, az1, front);
        let bump = pass_r.map_or(0.0, |p| (p - 0.5 * (self.plan_r + r1)).max(0.0));
        self.segs.push_back(Seg { t0, t1: t0 + dur.max(0.05) as f64, a0: self.plan_az, d, r0: self.plan_r, r1, bump, top: false });
        self.plan_az = (self.plan_az + d).rem_euclid(360.0);
        self.plan_r = r1;
    }

    /// 把刚排进去的那次移动改成"从头顶绕"。
    fn mark_top(&mut self) {
        if let Some(s) = self.segs.back_mut() {
            s.top = true;
            self.n_tops += 1;
        }
    }

    /// 这次换耳要不要从头顶绕（用单独的随机数；概率为 0 时不抽，原来的走位一点不变）。
    fn pick_top(&mut self, p: &Params) -> bool {
        p.over_top > 0.0 && self.top_rng.unit() < p.over_top
    }

    fn pick_via(&mut self, p: &Params) -> bool {
        match p.via {
            Via::Front => true,
            Via::Back => false,
            Via::Alternate => self.k % 2 == 0,
            Via::Random => self.rng.unit() < 0.7,
        }
    }

    fn reset_plan(&mut self, p: &Params, tm: f64) {
        self.segs.clear();
        self.pause_switch = p.mode == MotionMode::Sweep && p.breath_pause_switch;
        self.last_breath = None;
        self.plan_az = self.az;
        self.plan_r = self.r;
        self.k = 0;
        self.orbit_t0 = tm;
        match p.mode {
            MotionMode::Sweep | MotionMode::Random => {
                if self.mode.is_none() {
                    // 开场：贴在左耳
                    self.right = false;
                    self.plan_az = 360.0 - p.ear_angle;
                    self.plan_r = p.ear_dist;
                    self.az = self.plan_az;
                    self.r = self.plan_r;
                    self.next_t = tm + p.hold_s as f64;
                } else {
                    self.right = self.az.rem_euclid(360.0) < 180.0;
                    self.next_t = tm + 0.2;
                }
                self.at_ear = true;
            }
            MotionMode::Orbit => {
                if self.mode.is_some() {
                    let cur = self.az;
                    self.push_move(tm, 1.0, cur, p.orbit_radius, None, None);
                } else {
                    self.az = p.static_az;
                    self.r = p.orbit_radius;
                }
            }
            MotionMode::Static => {
                if self.mode.is_some() {
                    self.push_move(tm, 1.2, p.static_az, p.ear_dist, Some(true), None);
                } else {
                    self.az = p.static_az;
                    self.r = p.ear_dist;
                }
            }
            MotionMode::Manual => {} // 手动模式不排计划，见 step_manual
        }
        if self.mode.is_none() {
            // 开场直接就位，不从默认位置移过去
            self.plan_az = self.az;
            self.plan_r = self.r;
        }
        self.mode = Some(p.mode);
        self.last_ear_dist = p.ear_dist;
    }

    /// 穿梭：换到另一只耳朵；一定概率改成"先退到斜前方约 0.5 m，再慢慢凑近"。
    fn plan_sweep(&mut self, p: &Params) {
        let mut t0 = self.next_t;
        self.right = !self.right;
        let spread = 6.0 * (p.randomness / 0.1).clamp(0.0, 3.0);
        let a = p.ear_angle + self.rng.uniform(-spread, spread);
        let az1 = if self.right { a } else { 360.0 - a };
        let r1 = p.ear_dist * self.rng.uniform(0.95, 1.05);
        if self.k > 0 && self.rng.unit() < p.approach {
            let am = self.rng.uniform(30.0, 55.0);
            let azm = if self.right { am } else { 360.0 - am };
            let mv = p.move_s * self.jitter(p, 0.08);
            let far = self.rng.uniform(0.45, 0.6).max(p.ear_dist + 0.2);
            self.push_move(t0, mv, azm, far, None, None);
            t0 += (mv + self.rng.uniform(0.3, 0.6)) as f64;
            let loom = self.rng.uniform(2.2, 3.0);
            self.push_move(t0, loom, az1, r1, None, None);
            t0 += (loom + p.hold_s * self.rng.uniform(1.0, 1.3)) as f64;
        } else {
            let top = self.pick_top(p);
            let front = self.pick_via(p) || top;
            let mv = p.move_s * self.jitter(p, 0.08);
            self.push_move(t0, mv, az1, r1, Some(front), Some(p.pass_dist.max(p.ear_dist)));
            if top {
                self.mark_top();
            }
            t0 += (mv + p.hold_s * self.jitter(p, 0.1)) as f64;
        }
        self.k += 1;
        self.next_t = t0;
        self.at_ear = true;
    }

    /// 随机走位：大多换到另一侧的耳边，有时停在侧前方 / 侧后方 / 正前方；停留时长随机。
    fn plan_random(&mut self, p: &Params) {
        let t0 = self.next_t;
        if self.rng.unit() < 0.85 {
            self.right = !self.right;
        }
        let q = self.rng.unit();
        let scale = p.ear_dist / 0.105;
        // (方位角范围, 距离范围)，右侧定义，左侧镜像
        // 停在正前方的比例可调（默认 0.08 = 原来的比例，其余三种位置按原来的比例分）
        let k = if (p.front - 0.08).abs() < 1e-6 { 1.0 } else { (1.0 - p.front.clamp(0.0, 1.0)) / 0.92 };
        let (kind, (a0, a1), (r0, r1)) = if q < 0.58 * k {
            (0, (80.0, 98.0), (0.105 * scale, 0.12 * scale))
        } else if q < 0.80 * k {
            (1, (28.0, 60.0), (0.22f32.max(p.ear_dist), 0.38f32.max(p.ear_dist + 0.05)))
        } else if q < 0.92 * k {
            (2, (112.0, 150.0), (0.20f32.max(p.ear_dist), 0.38f32.max(p.ear_dist + 0.05)))
        } else {
            (3, (-15.0, 15.0), (0.30f32.max(p.ear_dist + 0.05), 0.50f32.max(p.ear_dist + 0.1)))
        };
        let mut az1 = self.rng.uniform(a0, a1);
        let r1v = (self.rng.uniform(r0.ln(), r1.ln())).exp();
        if !self.right && kind != 3 {
            az1 = -az1;
        }
        let az1 = az1.rem_euclid(360.0);
        let mut front = self.rng.unit() < 0.75;
        // 换到另一侧（接近对穿）的移动，一定概率从头顶绕
        let top = arc_delta(self.plan_az, az1, None).abs() >= 120.0 && self.pick_top(p);
        front |= top;
        let sweep = arc_delta(self.plan_az, az1, Some(front)).abs();
        let mv = p.move_s * (0.45 + 0.55 * sweep / 180.0) * self.jitter(p, 0.15);
        self.push_move(t0, mv, az1, r1v, Some(front), Some(0.26 + 0.12 * sweep / 180.0));
        if top {
            self.mark_top();
        }
        let sigma = 0.25 + 0.6 * p.randomness.clamp(0.0, 1.0);
        let hold = p.hold_s * self.rng.lognormal(1.0, sigma).clamp(0.3, 4.0) * if kind == 0 { 1.0 } else { 0.7 };
        self.k += 1;
        self.next_t = t0 + (mv + hold) as f64;
        self.at_ear = kind == 0;
    }

    /// 忽远忽近的下一个关键点：贴耳停留 → 慢慢往后让 → 稍停 → 慢慢凑回来（比退开慢），循环。
    /// 随机程度为 0 时每个量都取区间中点，节奏和深浅固定；为 1 时与 Python 版的随机分布相同。
    fn next_sway_key(&mut self, p: &Params) {
        fn ur(rng: &mut Rng, rho: f32, a: f32, b: f32) -> f32 {
            let mid = 0.5 * (a + b);
            mid + (rng.uniform(a, b) - mid) * rho
        }
        let rho = p.sway_random.clamp(0.0, 1.0);
        let sc = p.sway_period.max(1.0) / 8.0;
        let t = self.sw_k1.0;
        let rng = &mut self.sw_rng;
        let nk = match self.sw_stage {
            0 => {
                let q = rng.unit();
                self.sw_depth = if q < 0.12 * rho {
                    ur(rng, rho, 0.4, 0.9) // 偶尔只让一点
                } else if q < 1.0 - 0.2 * rho {
                    ur(rng, rho, 1.0, 2.0) // 多数：退到 2–4 倍距离
                } else {
                    ur(rng, rho, 2.0, 3.2) // 偶尔退得更远
                };
                self.n_sways += 1;
                let slow = 0.8 + 0.2 * self.sw_depth;
                (t + (ur(rng, rho, 1.2, 2.6) * sc * slow) as f64, self.sw_depth)
            }
            1 => (t + (ur(rng, rho, 0.3, 1.8) * sc) as f64, self.sw_depth * ur(rng, rho, 0.85, 1.05)),
            2 => {
                let slow = 0.8 + 0.2 * self.sw_depth;
                self.sw_in = ur(rng, rho, -0.4, 0.1);
                (t + (ur(rng, rho, 1.6, 3.4) * sc * slow) as f64, self.sw_in)
            }
            _ => {
                // 贴近时至少停 2 s（Koumura 等 2021：战栗感比声音滞后约 2 s）
                let dwell = (2.6f32.ln() + 0.4 * rho * rng.normal()).exp().min(7.0);
                let v = (self.sw_in + rng.uniform(-0.15, 0.15) * rho).clamp(-0.45, 0.2);
                (t + (dwell * sc).max(2.0) as f64, v)
            }
        };
        self.sw_stage = (self.sw_stage + 1) % 4;
        self.sw_k0 = self.sw_k1;
        self.sw_k1 = nk;
    }

    fn sway_gesture(&mut self, p: &Params, tm: f64) -> f32 {
        if !self.sw_init {
            let rho = p.sway_random.clamp(0.0, 1.0);
            let u0 = -0.1 + (self.sw_rng.uniform(-0.3, 0.1) + 0.1) * rho;
            let sc = p.sway_period.max(1.0) / 8.0;
            self.sw_k0 = (tm, u0);
            self.sw_k1 = (tm + (1.4 + (self.sw_rng.uniform(0.3, 2.5) - 1.4) * rho) as f64 * sc as f64, u0);
            self.sw_stage = 0;
            self.sw_init = true;
        }
        while tm >= self.sw_k1.0 {
            self.next_sway_key(p);
        }
        let (t0, v0) = self.sw_k0;
        let (t1, v1) = self.sw_k1;
        v0 + (v1 - v0) * smoothstep(((tm - t0) / (t1 - t0).max(1e-6)) as f32)
    }

    /// 自动上下的下一个关键点：停一会儿 → 慢慢移到耳朵的另一边（上 / 下）→ 停一会儿 → 再移回来。
    /// 随机程度为 0 时节奏和高低都固定（上、下各到幅度的 75%）；为 1 时停留和移动的时长、每次到多高都随机，
    /// 偶尔连续两次停在同一侧。返回 −1…1。
    fn updown_gesture(&mut self, p: &Params, tm: f64) -> f32 {
        let rho = p.updown_random.clamp(0.0, 1.0);
        let sc = p.updown_period.max(1.0) / 6.4;
        let mix = |rng: &mut Rng, mid: f32, lo: f32, hi: f32| mid + (rng.uniform(lo, hi) - mid) * rho;
        if !self.ud_init {
            let v = self.ud_rng.uniform(-0.5, 0.5) * rho;
            self.ud_sign = if self.ud_rng.unit() < 0.5 { 1.0 } else { -1.0 };
            self.ud_k0 = (tm, v);
            self.ud_k1 = (tm, v);
            self.ud_move = false;
            self.ud_init = true;
        }
        while tm >= self.ud_k1.0 {
            let (t, v) = self.ud_k1;
            let nk = if self.ud_move {
                if self.ud_rng.unit() >= 0.2 * rho {
                    self.ud_sign = -self.ud_sign;
                }
                let nv = self.ud_sign * mix(&mut self.ud_rng, 0.75, 0.35, 1.0);
                self.n_updowns += 1;
                (t + (mix(&mut self.ud_rng, 1.9, 1.3, 2.5) * sc) as f64, nv)
            } else {
                (t + (mix(&mut self.ud_rng, 1.3, 0.5, 2.1) * sc) as f64, v)
            };
            self.ud_move = !self.ud_move;
            self.ud_k0 = self.ud_k1;
            self.ud_k1 = nk;
        }
        let (t0, v0) = self.ud_k0;
        let (t1, v1) = self.ud_k1;
        v0 + (v1 - v0) * smoothstep(((tm - t0) / (t1 - t0).max(1e-6)) as f32)
    }

    /// 入耳的进出手势（0 = 在耳道口，1 = 伸到最深）：在外面停一下 → 伸进去 → 在里面停一会儿 → 退出来，循环。
    /// 两段 KU100 掏耳实录里，共振峰一个来回 2–6 s，在"浅"（约 3–4 kHz）和"深"（约 8–12 kHz）两头停得多、中间走得快
    /// （约每秒 1 个倍频程）。随机程度为 0 时节奏和深浅固定；为 1 时都随机，偶尔只伸到一半、偶尔整个退出来。
    fn depth_gesture(&mut self, p: &Params, tm: f64) -> f32 {
        let rho = p.inear_random.clamp(0.0, 1.0);
        let sc = p.inear_period.max(0.5) / 4.05;
        let mix = |rng: &mut Rng, lo: f32, hi: f32| {
            let mid = 0.5 * (lo + hi);
            mid + (rng.uniform(lo, hi) - mid) * rho
        };
        if !self.dp_init {
            let v = mix(&mut self.dp_rng, 0.05, 0.3);
            self.dp_k0 = (tm, v);
            self.dp_k1 = (tm + (mix(&mut self.dp_rng, 0.2, 1.2) * sc) as f64, v);
            self.dp_stage = 0;
            self.dp_init = true;
        }
        while tm >= self.dp_k1.0 {
            let (t, v) = self.dp_k1;
            let rng = &mut self.dp_rng;
            let nk = match self.dp_stage {
                0 => {
                    // 伸进去：多数伸到底，偶尔只到一半
                    self.dp_deep = if rng.unit() < 0.18 * rho { mix(rng, 0.45, 0.7) } else { mix(rng, 0.8, 1.0) };
                    self.n_strokes += 1;
                    (t + (mix(rng, 0.7, 1.6) * sc) as f64, self.dp_deep)
                }
                1 => (t + (mix(rng, 0.3, 1.8) * sc) as f64, (self.dp_deep * mix(rng, 0.9, 1.04)).min(1.0)),
                2 => {
                    // 退出来：多数退到耳道口附近，偶尔整个拿出来
                    let out = if rng.unit() < 0.12 * rho { 0.0 } else { mix(rng, 0.05, 0.3) };
                    (t + (mix(rng, 0.7, 1.6) * sc) as f64, out)
                }
                _ => (t + (mix(rng, 0.2, 1.2) * sc) as f64, v),
            };
            self.dp_stage = (self.dp_stage + 1) % 4;
            self.dp_k0 = self.dp_k1;
            self.dp_k1 = nk;
        }
        let (t0, v0) = self.dp_k0;
        let (t1, v1) = self.dp_k1;
        v0 + (v1 - v0) * smoothstep(((tm - t0) / (t1 - t0).max(1e-6)) as f32)
    }

    /// 这一步的入耳深度（米）。只有贴在耳朵上时才伸得进去；快要换边时先退出来；`auto` = false（手动摆位）时
    /// 没有自动进出，贴上耳朵就伸到设定的深度。入耳深度为 0 时恒为 0。
    fn depth_step(&mut self, p: &Params, tm: f64, pos: (f32, f32, f32), auto: bool) -> f32 {
        if p.inear_cm <= 0.0 {
            self.dp_on = false;
            return 0.0;
        }
        let gate = inear_gate(pos.0, pos.1, pos.2);
        if gate < 0.02 {
            // 不在耳边：手势停下；下次贴上耳朵时从"在耳道口停一下、再慢慢伸进去"重新开始，而不是一下子捅到底
            self.dp_init = false;
        }
        let mut u = if !auto {
            gate
        } else if gate >= 0.02 {
            gate * self.depth_gesture(p, tm)
        } else {
            0.0
        };
        if auto && matches!(p.mode, MotionMode::Sweep | MotionMode::Random) {
            // 快要换边时先退出来（提前 0.8 s）；换边途中一直在外面，到了另一只耳朵再伸进去
            u *= if self.segs.is_empty() { smoothstep((self.next_t - tm) as f32 / 0.8) } else { 0.0 };
        }
        if !self.dp_on {
            self.dp_lp.prime(0.0);
            self.dp_on = true;
        }
        inear_depth(self.dp_lp.process(u).clamp(0.0, 1.0), p.inear_cm)
    }

    /// 手动模式的一步：位置跟着用户拖到的目标走，没有任何自动移动。
    fn step_manual(&mut self, p: &Params) -> Pose {
        let tgt = lift_tilt(p.manual_az.rem_euclid(360.0), p.manual_dist.clamp(R_MIN, R_MAX), 0.0, p.manual_tilt.clamp(-80.0, 85.0));
        let cur = match (self.man, self.mode) {
            (Some(c), _) => c,
            (None, None) => tgt,          // 开场：直接就位
            (None, Some(_)) => self.last, // 从自动切过来：从当前位置出发，不跳
        };
        let now = approach(cur, tgt);
        self.man = Some(now);
        self.leave = None;
        self.segs.clear();
        self.lift = 0.0;
        self.mode = Some(MotionMode::Manual);
        self.last = now;
        let depth = self.depth_step(p, 0.0, now, false);
        Pose { az: now.0, el: now.1, r: now.2, r_base: now.2, az_base: now.0, el_base: now.1, depth }
    }

    /// 推进到说话时钟 `tm`（秒），返回这个边界上的位置。
    /// `amp`：晃动幅度倍数（随音量）；`follow_db`：说话音量包络（dB，相对中位）。
    pub fn step(&mut self, p: &Params, tm: f64, amp: f32, follow_db: f32) -> Pose {
        self.step_with_activity(p, tm, amp, follow_db, 1.0)
    }

    /// `activity` 是与输出对齐的输入活动度。一起移动时传入音源共同的活动度。
    pub fn step_with_activity(&mut self, p: &Params, tm: f64, amp: f32, follow_db: f32, activity: f32) -> Pose {
        if p.mode == MotionMode::Manual {
            return self.step_manual(p);
        }
        if self.mode == Some(MotionMode::Manual) {
            // 刚从手动切回自动：自动的走位从手动摆的位置接着走，并在 0.6 秒内平滑过渡过去
            if let Some(m) = self.man.take() {
                self.leave = Some((m, 0.0));
            }
            self.az = p.manual_az.rem_euclid(360.0);
            self.r = p.manual_dist.clamp(R_MIN, R_MAX);
        }
        if self.mode != Some(p.mode) || self.pause_switch != (p.mode == MotionMode::Sweep && p.breath_pause_switch) {
            self.reset_plan(p, tm);
        }
        // 贴耳距离被拖动时，停在耳边的声源跟着挪过去
        if (p.ear_dist - self.last_ear_dist).abs() > 1e-4 {
            if self.at_ear && matches!(p.mode, MotionMode::Sweep | MotionMode::Random | MotionMode::Static) {
                let (az, r1) = (self.plan_az, self.plan_r * p.ear_dist / self.last_ear_dist.max(1e-3));
                // 拖动中的每次更新都重新瞄准；旧的平滑移动不能吞掉后续距离。
                self.segs.clear();
                self.plan_az = self.az;
                self.plan_r = self.r;
                self.push_move(tm, 0.18, az, r1.clamp(R_MIN, R_MAX), None, None);
                self.next_t = self.next_t.max(tm + 0.2);
            }
            self.last_ear_dist = p.ear_dist;
        }
        match p.mode {
            MotionMode::Sweep => {
                if self.segs.is_empty() {
                    if self.pause_switch && activity > 0.35 {
                        if self.last_breath.is_none() {
                            // 从这一侧第一口可听的气息起算，前导静音不占停留时间。
                            self.next_t = self.next_t.max(tm + f64::from(p.hold_s));
                        }
                        self.last_breath = Some(tm);
                    }
                    // 在新停顿刚被确认时开始。若最短停留时间在长静音中间才到，
                    // 继续等下一次气息落下，不在下一口开始前的未知时刻起步。
                    let ready = !self.pause_switch || self.last_breath.is_some_and(|last| {
                        let gap_start = last + 0.10;
                        gap_start >= self.next_t && tm >= gap_start
                    });
                    if ready && tm >= self.next_t {
                        if self.pause_switch {
                            // 可能已等过预定时刻，此刻从原位置起步，以完整的时长走完。
                            self.next_t = tm;
                            self.last_breath = None;
                        }
                        self.plan_sweep(p);
                        self.n_moves += 1;
                    }
                }
            }
            MotionMode::Random => {
                while self.segs.is_empty() && tm >= self.next_t {
                    self.plan_random(p);
                    self.n_moves += 1;
                }
            }
            MotionMode::Static => {
                if arc_delta(self.plan_az, p.static_az, None).abs() > 0.5 {
                    self.segs.clear();
                    self.plan_az = self.az;
                    self.plan_r = self.r;
                    self.push_move(tm, 0.18, p.static_az, p.ear_dist, Some(true), None);
                }
            }
            MotionMode::Orbit | MotionMode::Manual => {}
        }
        // 走完已经结束的移动，求当前位置
        self.lift = 0.0;
        while let Some(s) = self.segs.front().copied() {
            if tm >= s.t1 {
                self.az = (s.a0 + s.d).rem_euclid(360.0);
                self.r = s.r1;
                self.segs.pop_front();
            } else {
                if tm >= s.t0 {
                    let u = smoothstep(((tm - s.t0) / (s.t1 - s.t0)) as f32);
                    self.az = (s.a0 + s.d * u).rem_euclid(360.0);
                    self.r = (1.0 - u) * s.r0 + u * s.r1 + s.bump * (PI * u).sin();
                    if s.top {
                        // 起止各用四分之一的行程转上去 / 转回来，中间一半贴着头顶走
                        self.lift = 90.0 * smoothstep(u / 0.25) * smoothstep((1.0 - u) / 0.25);
                    }
                }
                break;
            }
        }
        if p.mode == MotionMode::Orbit && self.segs.is_empty() {
            let per = p.orbit_period.max(1.0) as f64;
            self.az = (p.static_az as f64 + 360.0 * ((tm - self.orbit_t0) / per)).rem_euclid(360.0) as f32;
            self.r = p.orbit_radius;
            self.plan_az = self.az;
            self.plan_r = self.r;
        }

        // 人体自然晃动（笛卡尔坐标），按说话时钟走，幅度随音量
        let (mut x, mut y) = (self.r * self.az.to_radians().sin(), self.r * self.az.to_radians().cos());
        let sigma = 0.012 * p.micro;
        if sigma > 0.0 {
            let t = tm as f32;
            for ax in 0..2 {
                let mut s = 0.0;
                for &(f, ph, am) in &self.micro[ax] {
                    s += am * (TAU * f * t + ph).sin();
                }
                let v = s / self.micro_norm[ax] * sigma * amp;
                if ax == 0 {
                    x += v;
                } else {
                    y += v;
                }
            }
        }
        let az = x.atan2(y).to_degrees().rem_euclid(360.0);
        let r_base = x.hypot(y).clamp(R_MIN, R_MAX);

        // 忽远忽近：只改嘴到头表面的距离
        let mut w = 0.0;
        if p.sway > 0.0 {
            w += p.sway * self.sway_gesture(p, tm);
        }
        if p.sway_follow > 0.0 {
            w += p.sway_follow * follow_db.clamp(-3.0, 12.0) / 6.0;
        }
        if !self.w_primed {
            self.w_lp.prime(w);
            self.w_primed = true;
        }
        let w = self.w_lp.process(w);
        let r = if p.sway > 0.0 || p.sway_follow > 0.0 {
            let near = ((0.45 - r_base) / (0.45 - 0.2)).clamp(0.0, 1.0);
            let d0 = (r_base - HEAD_A).max(0.005);
            let d = (d0 * 2f32.powf(w * near)).min(d0.max(0.35));
            (HEAD_A + d).clamp(R_MIN, R_MAX)
        } else {
            r_base
        };

        // 上下：固定高度 + 自动上下（带一点上下的晃动）+ 从头顶绕。都没有时仰角恒为 0，位置与原来逐位相同
        let mut tilt = p.height;
        if p.updown > 0.0 {
            let g = self.updown_gesture(p, tm);
            let t = tm as f32;
            let mut wob = 0.0;
            for &(f, ph, am) in &self.micro_z {
                wob += am * (TAU * f * t + ph).sin();
            }
            tilt += p.updown * (g + 0.15 * wob / self.micro_z_norm * amp);
        }
        let tilt = tilt.clamp(-80.0, 85.0);
        let (az3, el, r3) = lift_tilt(az, r, self.lift, tilt);
        let (az_base, el_base, rb) = lift_tilt(az, r_base, self.lift, tilt);
        let mut pose = Pose { az: az3, el, r_base: rb, r: r3, az_base, el_base, depth: 0.0 };
        if let Some((hold, t)) = self.leave {
            // 从手动切回来的头 0.6 秒：从切换时的位置平滑过渡到自动的走位
            let t = t + CTRL_DT;
            let s = smoothstep(t / MANUAL_LEAVE_S);
            if s >= 1.0 {
                self.leave = None;
            } else {
                self.leave = Some((hold, t));
                let now = mix_pos(hold, (pose.az, pose.el, pose.r), s);
                let base = mix_pos(hold, (pose.az_base, pose.el_base, pose.r_base), s);
                pose = Pose { az: now.0, el: now.1, r: now.2, r_base: base.2, az_base: base.0, el_base: base.1, depth: 0.0 };
            }
        }
        pose.depth = self.depth_step(p, tm, (pose.az, pose.el, pose.r), true);
        self.last = (pose.az, pose.el, pose.r);
        pose
    }
}

/// 说话时钟一步的增量：说话时按真实时间走，停顿时只走 `idle` 倍。
#[inline]
pub fn clock_step(act: f32, idle: f32) -> f64 {
    let idle = idle.clamp(0.0, 1.0);
    (CTRL_DT * (idle + (1.0 - idle) * act.clamp(0.0, 1.0))) as f64
}

/// 主播模式：停顿超过这么久（秒），动作恢复正常速度。
pub const LIVE_RESUME_S: f32 = 1.0;
const LIVE_RESUME_RAMP_S: f32 = 0.6;

/// 主播模式的说话时钟：短停顿里照样放慢（下一句从原位置接着说），但停顿超过 [`LIVE_RESUME_S`] 后
/// 恢复正常速度。直播时话少、或者说话检测没认出来（麦克风底噪大、声音很轻）时，声音也一直在动，
/// 不会停在一个点上。`pause_s`：已经连续停顿了多久。
#[inline]
pub fn clock_step_live(act: f32, idle: f32, pause_s: f32) -> f64 {
    let idle = idle.clamp(0.0, 1.0);
    let rate = idle + (1.0 - idle) * act.clamp(0.0, 1.0);
    let u = smoothstep((pause_s - LIVE_RESUME_S) / LIVE_RESUME_RAMP_S);
    (CTRL_DT * (rate + (1.0 - rate) * u)) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_goes_the_requested_way() {
        // 左耳 260° -> 右耳 100°：从前面绕是顺时针 +200°，从后面绕是 −160°
        assert!((arc_delta(260.0, 100.0, Some(true)) - 200.0).abs() < 1e-3);
        assert!((arc_delta(260.0, 100.0, Some(false)) + 160.0).abs() < 1e-3);
        assert!((arc_delta(350.0, 10.0, Some(false)) - 20.0).abs() < 1e-3);
    }

    #[test]
    fn breath_sweep_waits_for_a_gap_then_finishes_the_crossing() {
        let p = Params { hold_s: 0.6, move_s: 0.6, randomness: 0.0, micro: 0.0, ..Params::default().style_breath() };
        let mut m = Motion::new(1);
        let mut previous = 360.0 - p.ear_angle;
        for i in 0..(8.0 / CTRL_DT) as usize {
            let t = i as f32 * CTRL_DT;
            // 一口长气中有 50 ms 的凹口。下一侧停很久才有第一口短气。
            let sounding = ((1.0..2.5).contains(&t) && !(1.9..1.95).contains(&t))
                || (5.0..5.3).contains(&t) || (6.2..6.6).contains(&t);
            let pose = m.step_with_activity(&p, f64::from(t), 1.0, 0.0, if sounding { 1.0 } else { 0.0 });
            assert!(arc_delta(previous, pose.az, None).abs() < 1.0, "换边跳变 {t}");
            if t < 2.59 {
                assert_eq!(m.n_moves, 0, "长元音或短暂气息起伏触发换边 {t}");
            } else if (3.3..6.69).contains(&t) {
                assert_eq!(m.n_moves, 1, "右侧没有新的一组气息仍在换边 {t}");
                assert!((pose.az - p.ear_angle).abs() < 1e-3);
            } else if t > 7.4 {
                assert_eq!(m.n_moves, 2);
                assert!((pose.az - (360.0 - p.ear_angle)).abs() < 1e-3);
            }
            previous = pose.az;
        }
    }

    #[test]
    fn live_clock_keeps_moving_when_speech_is_sparse() {
        // 每说 1 秒停 8 秒。严格跟着说话的时钟 60 秒里只走约 14 秒，声音几乎停在一个点上；
        // 主播模式的时钟停顿超过 1 秒就恢复，60 秒里要走掉大半，照常换耳。
        let run = |live: bool| {
            let p = Params::default();
            let mut m = Motion::new(1);
            let (mut tm, mut pause) = (0.0f64, 0.0f32);
            for i in 0..(60.0 / CTRL_DT) as usize {
                let act = if (i as f32 * CTRL_DT) % 9.0 < 1.0 { 1.0 } else { 0.0 };
                pause = if act > 0.5 { 0.0 } else { pause + CTRL_DT };
                m.step(&p, tm, 1.0, 0.0);
                tm += if live { clock_step_live(act, p.pause_motion, pause) } else { clock_step(act, p.pause_motion) };
            }
            (tm, m.n_moves)
        };
        let (t_strict, n_strict) = run(false);
        let (t_live, n_live) = run(true);
        assert!(t_strict < 16.0 && n_strict <= 3, "strict {t_strict} {n_strict}");
        assert!(t_live > 45.0 && n_live >= 7, "live {t_live} {n_live}");
        // 一直在说话时两种时钟完全一样
        assert_eq!(clock_step_live(1.0, 0.13, 0.0), clock_step(1.0, 0.13));
        assert_eq!(clock_step_live(0.0, 0.13, 0.5), clock_step(0.0, 0.13));
    }

    #[test]
    fn lift_tilt_geometry() {
        // 都为 0：原样返回
        assert_eq!(lift_tilt(260.0, 0.105, 0.0, 0.0), (260.0, 0.0, 0.105));
        // 贴右耳（正侧方，离耳 1.75 cm）向上倾 45°：嘴到耳朵的距离不变，从耳朵看正好在上方 45°
        let (az, el, r) = lift_tilt(90.0, 0.105, 0.0, 45.0);
        let (x, z) = (r * el.to_radians().cos() * az.to_radians().sin(), r * el.to_radians().sin());
        let d = ((x - HEAD_A).powi(2) + z * z).sqrt();
        assert!((d - 0.0175).abs() < 1e-4, "ear distance {d}");
        assert!((z.atan2(x - HEAD_A).to_degrees() - 45.0).abs() < 0.1);
        // 正前方 0.5 m 向上倾 30°：支点是头中心，就是普通的仰角
        let (az, el, r) = lift_tilt(0.0, 0.5, 0.0, 30.0);
        assert!(az.min(360.0 - az) < 0.01 && (el - 30.0).abs() < 0.01 && (r - 0.5).abs() < 1e-5);
        // 从头顶绕到正中：正前方 0.2 m 转 90° 就在头顶正上方
        let (_, el, r) = lift_tilt(0.0, 0.2, 90.0, 20.0);
        assert!(el > 89.9 && (r - 0.2).abs() < 1e-5);
        // 倾得再多也不会钻进头里
        for t in [-80.0f32, -40.0, 40.0, 85.0] {
            let (_, _, r) = lift_tilt(100.0, 0.105, 0.0, t);
            assert!(r >= R_MIN);
        }
    }

    #[test]
    fn height_off_means_flat_and_unchanged() {
        // 上下全关时仰角恒为 0，位置与"没有上下功能"时逐位相同（基准位置等于实际位置）
        let mut p = Params::default();
        p.updown = 0.0;
        let mut m = Motion::new(1);
        let mut tm = 0.0f64;
        for _ in 0..(40.0 / CTRL_DT) as usize {
            let pose = m.step(&p, tm, 1.0, 0.0);
            assert_eq!(pose.el, 0.0);
            assert_eq!(pose.el_base, 0.0);
            assert_eq!(pose.az, pose.az_base);
            tm += clock_step(1.0, 0.13);
        }
        assert_eq!(m.n_tops, 0);
    }

    #[test]
    fn over_the_top_passes_above_the_head() {
        let mut p = Params::default();
        p.updown = 0.0;
        p.over_top = 1.0;
        p.approach = 0.0;
        let mut m = Motion::new(1);
        let (mut tm, mut left, mut right, mut el_max, mut el_min) = (0.0f64, 0, 0, 0f32, 0f32);
        let mut prev: Option<[f32; 3]> = None;
        let mut jump = 0f32;
        for _ in 0..(60.0 / CTRL_DT) as usize {
            let pose = m.step(&p, tm, 1.0, 0.0);
            assert!(pose.r >= R_MIN && pose.r <= R_MAX);
            let (a, e) = (pose.az.to_radians(), pose.el.to_radians());
            let v = [pose.r * e.cos() * a.sin(), pose.r * e.cos() * a.cos(), pose.r * e.sin()];
            if let Some(q) = prev {
                jump = jump.max(((v[0] - q[0]).powi(2) + (v[1] - q[1]).powi(2) + (v[2] - q[2]).powi(2)).sqrt());
            }
            prev = Some(v);
            if e.cos() * a.sin() > 0.9 {
                right += 1;
            } else if e.cos() * a.sin() < -0.9 {
                left += 1;
            }
            el_max = el_max.max(pose.el);
            el_min = el_min.min(pose.el);
            tm += clock_step(1.0, 0.13);
        }
        assert!(m.n_tops >= 9 && m.n_tops == m.n_moves, "tops {} moves {}", m.n_tops, m.n_moves);
        assert!(el_max > 80.0, "el_max {el_max}"); // 确实经过头顶
        assert!(el_min > -15.0, "el_min {el_min}"); // 不会绕到下面去
        assert!(left > 5000 && right > 5000);
        assert!(jump < 0.004, "jump {jump} m per block"); // 经过头顶时位置连续，没有跳变
    }

    #[test]
    fn auto_updown_swings_both_ways() {
        let p = Params::default();
        assert!(p.updown > 0.0);
        let mut m = Motion::new(3);
        let (mut tm, mut up, mut down) = (0.0f64, 0, 0);
        for _ in 0..(90.0 / CTRL_DT) as usize {
            let pose = m.step(&p, tm, 1.0, 0.0);
            assert!(pose.r >= R_MIN && pose.el.abs() <= 90.0);
            // 从近耳看，嘴在上方还是下方（只统计贴在耳边的时候）
            let (a, e) = (pose.az.to_radians(), pose.el.to_radians());
            let lat = pose.r * e.cos() * a.sin().abs() - HEAD_A;
            if e.cos() * a.sin().abs() > 0.9 && pose.r < 0.14 {
                let ang = (pose.r * e.sin()).atan2(lat.max(1e-4)).to_degrees();
                if ang > 8.0 {
                    up += 1;
                } else if ang < -8.0 {
                    down += 1;
                }
            }
            tm += clock_step(1.0, 0.13);
        }
        assert!(m.n_updowns >= 12, "updowns {}", m.n_updowns);
        assert!(up > 3000 && down > 3000, "up {up} down {down}");
    }

    /// 位置换成直角坐标（米）。
    fn xyz(p: &Pose) -> [f32; 3] {
        let (a, e) = (p.az.to_radians(), p.el.to_radians());
        [p.r * e.cos() * a.sin(), p.r * e.cos() * a.cos(), p.r * e.sin()]
    }

    fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    #[test]
    fn continuous_slider_changes_reach_the_latest_distance_and_angle() {
        let mut p = Params { mode: MotionMode::Static, static_az: 270.0, sway: 0.0, sway_follow: 0.0, micro: 0.0, height: 0.0, updown: 0.0, ..Params::default() };
        let mut motion = Motion::new(1);
        let mut pose = motion.step(&p, 0.0, 1.0, 0.0);
        let mut previous = xyz(&pose);
        for i in 1..=900 {
            if i <= 600 && i % 12 == 0 {
                p.ear_dist = 0.105 + 0.195 * i as f32 / 600.0;
                p.static_az = 270.0 - 60.0 * i as f32 / 600.0;
            }
            pose = motion.step(&p, i as f64 * CTRL_DT as f64, 1.0, 0.0);
            assert!(dist(previous, xyz(&pose)) < 0.01, "slider caused a position jump");
            previous = xyz(&pose);
        }
        assert!((pose.r - p.ear_dist).abs() < 1e-4, "distance {} != {}", pose.r, p.ear_dist);
        assert!(arc_delta(pose.az, p.static_az, None).abs() < 0.1);
    }

    #[test]
    fn manual_follows_the_target_without_jumps() {
        let mut p = Params::default();
        p.mode = MotionMode::Manual;
        (p.manual_az, p.manual_dist, p.manual_tilt) = (270.0, 0.105, 0.0);
        let mut m = Motion::new(1);
        // 开场直接就位；目标不动时位置纹丝不动（没有晃动、忽远忽近、自动上下）
        let first = m.step(&p, 0.0, 1.0, 0.0);
        assert!((first.az - 270.0).abs() < 1e-3 && (first.r - 0.105).abs() < 1e-6 && first.el == 0.0);
        for _ in 0..2000 {
            let q = m.step(&p, 0.0, 1.6, 9.0);
            assert!(dist(xyz(&q), xyz(&first)) < 1e-6);
            assert_eq!(q.r, q.r_base);
        }
        // 一下子点到右耳：很快地绕过去，但每个控制块只挪一点点（不是跳过去），全程不进头里。
        // 说话时钟不走（tm 不变）也照样动：手动摆位按真实时间走
        (p.manual_az, p.manual_dist, p.manual_tilt) = (90.0, 0.105, 30.0);
        let (mut prev, mut step_max, mut passed_mid) = (xyz(&first), 0f32, false);
        let mut settle = None;
        for i in 0..(1.5 / CTRL_DT) as usize {
            let q = m.step(&p, 0.0, 1.0, 0.0);
            assert!(q.r >= R_MIN - 1e-6);
            let v = xyz(&q);
            step_max = step_max.max(dist(v, prev));
            prev = v;
            passed_mid |= q.az < 20.0 || q.az > 340.0 || (q.az - 180.0).abs() < 20.0;
            if settle.is_none() && (q.az - 90.0).abs() < 1.0 {
                settle = Some(i as f32 * CTRL_DT);
            }
        }
        assert!(step_max < 0.002, "largest step {step_max} m");
        assert!(passed_mid, "should travel around the head");
        assert!(settle.map_or(false, |t| t > 0.3 && t < 1.0), "settle {settle:?}");
        // 到位后高度也到了：从右耳看，嘴在上方约 30°
        let (x, z) = (prev[0], prev[2]);
        assert!((z.atan2(x - HEAD_A).to_degrees() - 30.0).abs() < 1.0);
    }

    #[test]
    fn switching_between_auto_and_manual_is_continuous() {
        let mut p = Params::default();
        let mut m = Motion::new(1);
        let mut tm = 0.0f64;
        let (mut prev, mut step_max) = (None::<[f32; 3]>, 0f32);
        let run = |m: &mut Motion, p: &Params, secs: f32, tm: &mut f64, prev: &mut Option<[f32; 3]>, step_max: &mut f32| {
            for _ in 0..(secs / CTRL_DT) as usize {
                let q = m.step(p, *tm, 1.0, 0.0);
                assert!(q.r >= R_MIN - 1e-6 && q.r <= R_MAX);
                let v = xyz(&q);
                if let Some(o) = *prev {
                    *step_max = step_max.max(dist(v, o));
                }
                *prev = Some(v);
                *tm += clock_step(1.0, 0.13);
            }
        };
        // 自动穿梭 4 秒 → 手动摆到右前方 40 cm、向上 40° → 3 秒后回到自动
        run(&mut m, &p, 4.0, &mut tm, &mut prev, &mut step_max);
        let moves_before = m.n_moves;
        p.mode = MotionMode::Manual;
        (p.manual_az, p.manual_dist, p.manual_tilt) = (40.0, 0.4, 40.0);
        run(&mut m, &p, 3.0, &mut tm, &mut prev, &mut step_max);
        let held = prev.unwrap();
        let want = {
            let (az, el, r) = lift_tilt(40.0, 0.4, 0.0, 40.0);
            xyz(&Pose { az, el, r, ..Default::default() })
        };
        assert!(dist(held, want) < 0.002, "manual position not reached: {held:?} vs {want:?}");
        assert_eq!(m.n_moves, moves_before); // 手动时不再自动换耳
        p.mode = MotionMode::Sweep;
        run(&mut m, &p, 12.0, &mut tm, &mut prev, &mut step_max);
        assert!(m.n_moves > moves_before, "sweeps should resume");
        // 两次切换都没有跳变：最快也就是手动跟随的限速（40 cm 处每块约 4 mm）
        assert!(step_max < 0.006, "largest step {step_max} m");
    }

    #[test]
    fn inear_depth_mapping() {
        // 手势 0 = 耳道口，1 = 设定的最深处；中间按"剩余长度的几何插值"单调变深
        assert_eq!(inear_depth(0.0, 2.1), 0.0);
        assert!((inear_depth(1.0, 2.1) - 0.021).abs() < 1e-6);
        let mut last = 0.0;
        for k in 1..=20 {
            let d = inear_depth(k as f32 / 20.0, 2.1);
            assert!(d > last);
            last = d;
        }
        // 共振频率（c / 4L）在手势上是对数线性的：一半的手势 = 两端频率的几何平均
        let f = |d: f32| 343.0 / (4.0 * (CANAL_L0 - d));
        let (f0, f1, fm) = (f(0.0), f(inear_depth(1.0, 2.1)), f(inear_depth(0.5, 2.1)));
        assert!((fm / (f0 * f1).sqrt() - 1.0).abs() < 1e-3, "{f0} {fm} {f1}");
        assert!(f0 > 2900.0 && f0 < 3000.0 && f1 > 10000.0 && f1 < 11500.0);
        // 设得再深也不会超过耳道的长度
        assert!((inear_depth(1.0, 9.0) - (CANAL_L0 - CANAL_L_MIN)).abs() < 1e-6);
        assert_eq!(inear_depth(1.0, 0.0), 0.0);
    }

    #[test]
    fn inear_is_off_by_default() {
        // 入耳深度为 0（默认、贴耳加强、KU100 实录）：深度恒为 0，也不计伸进耳道的次数
        for p in [Params::default(), Params::default().style_ku100()] {
            let mut m = Motion::new(1);
            let mut tm = 0.0f64;
            for _ in 0..(30.0 / CTRL_DT) as usize {
                assert_eq!(m.step(&p, tm, 1.0, 0.0).depth, 0.0);
                tm += clock_step(1.0, 0.13);
            }
            assert_eq!(m.n_strokes, 0);
        }
    }

    #[test]
    fn earclean_strokes_in_and_out_and_pulls_out_before_switching() {
        let p = Params::default().style_earclean();
        let max = 0.01 * p.inear_cm;
        let mut m = Motion::new(1);
        let (mut tm, mut deep, mut shallow, mut prev, mut glide_max) = (0.0f64, 0usize, 0usize, 0f32, 0f32);
        // 深度对应的耳道共振频率（log2 Hz）
        let lf = |d: f32| (343.0 / (4.0 * (CANAL_L0 - d))).log2();
        let (mut left, mut right, mut crossing) = (0usize, 0usize, 0usize);
        let n = (200.0 / CTRL_DT) as usize;
        for _ in 0..n {
            let q = m.step(&p, tm, 1.0, 0.0);
            assert!(q.depth >= 0.0 && q.depth <= max + 1e-5, "depth {}", q.depth);
            glide_max = glide_max.max((lf(q.depth) - lf(prev)).abs() / CTRL_DT);
            prev = q.depth;
            let s = q.az.to_radians().sin() * q.el.to_radians().cos();
            if s.abs() > 0.9 {
                if s > 0.0 {
                    right += 1;
                } else {
                    left += 1;
                }
                deep += (q.depth > 0.8 * max) as usize;
                shallow += (q.depth < 0.25 * max) as usize;
            } else if s.abs() < 0.6 {
                // 换边途中（离开耳朵）：一定已经退出来了
                crossing += 1;
                assert!(q.depth < 5e-4, "换边途中还在耳道里：{} m，方位 {}", q.depth, q.az);
            }
            tm += clock_step(1.0, 0.13);
        }
        // 200 秒里每只耳朵都掏过，每只耳朵停约 15 秒、换边 2 秒：换 11 次左右的边；一个来回约 4 秒
        assert!(m.n_moves >= 9 && m.n_moves <= 14, "moves {}", m.n_moves);
        assert!(left > n / 5 && right > n / 5 && crossing > 100, "left {left} right {right} crossing {crossing}");
        assert!(m.n_strokes >= 35 && m.n_strokes <= 65, "strokes {}", m.n_strokes);
        // 在"深"和"浅"两头都停得不少
        assert!(deep > n / 5 && shallow > n / 12, "deep {deep} shallow {shallow} of {n}");
        // 进出是平滑的：共振峰最快每秒滑 5 个倍频程以内（实录：中位约 1，最快的 10% 在 2.5–4.8）
        assert!(glide_max < 5.0, "fastest resonance glide {glide_max} oct/s");
    }

    /// 掏耳不能一直停在一只耳朵：开场 20 秒内就换到另一边，之后每一侧最多停 25 秒左右（说话时钟按有声音的时间走）；
    /// 断断续续的声音（一半时间在停顿）按网页 / 试听的时钟走，真实时间里一分钟内也换过边。
    #[test]
    fn earclean_switches_ears_regularly() {
        let p = Params::default().style_earclean();
        for (seed, sparse) in [(1u64, false), (7, false), (3, true)] {
            let mut m = Motion::new(seed);
            let (mut tm, mut side, mut since, mut first, mut longest) = (0.0f64, 0i8, 0.0f32, None, 0f32);
            let n = (240.0 / CTRL_DT) as usize;
            for i in 0..n {
                let t = i as f32 * CTRL_DT;
                let q = m.step(&p, tm, 1.0, 0.0);
                let s = q.az.to_radians().sin();
                let now = if s > 0.9 { 1 } else if s < -0.9 { -1 } else { 0 };
                if now != 0 && now != side {
                    if side != 0 {
                        first.get_or_insert(t);
                        longest = longest.max(t - since);
                    }
                    (side, since) = (now, t);
                }
                // 断断续续：说 1 秒、停 1 秒
                let act = if sparse && (t as u32) % 2 == 1 { 0.0 } else { 1.0 };
                tm += clock_step(act, p.pause_motion);
            }
            let first = first.expect("240 秒里一次都没换边");
            let (first_max, hold_max) = if sparse { (40.0, 45.0) } else { (20.0, 25.0) };
            assert!(first < first_max, "第一次换边在 {first} 秒（sparse {sparse}）");
            assert!(longest < hold_max, "一侧最长停了 {longest} 秒（sparse {sparse}）");
        }
    }

    #[test]
    fn manual_position_enters_the_ear_only_at_the_ear() {
        let mut p = Params::default().style_earclean();
        p.mode = MotionMode::Manual;
        (p.manual_az, p.manual_dist, p.manual_tilt) = (270.0, R_MIN, 0.0);
        let mut m = Motion::new(1);
        let mut q = m.step(&p, 0.0, 1.0, 0.0);
        for _ in 0..(1.5 / CTRL_DT) as usize {
            q = m.step(&p, 0.0, 1.0, 0.0);
        }
        // 手动摆在耳朵上：伸到设定的深度，没有自动进出
        assert!((q.depth - 0.021).abs() < 2e-4, "depth {}", q.depth);
        let held = q.depth;
        for _ in 0..500 {
            assert!((m.step(&p, 0.0, 1.0, 0.0).depth - held).abs() < 1e-5);
        }
        // 拖到脸前 30 cm：退出来
        (p.manual_az, p.manual_dist) = (0.0, 0.3);
        for _ in 0..(1.5 / CTRL_DT) as usize {
            q = m.step(&p, 0.0, 1.0, 0.0);
        }
        assert_eq!(q.depth, 0.0);
    }

    #[test]
    fn random_mode_front_share() {
        // 随机走位：正面比例调大后，在正前方（±25° 以内）的时间明显变多；默认比例下与原来一样少
        let share = |front: f32| {
            let mut p = Params::default();
            p.mode = MotionMode::Random;
            p.front = front;
            let mut m = Motion::new(5);
            let (mut tm, mut n_front, n) = (0.0f64, 0usize, (600.0 / CTRL_DT) as usize);
            for _ in 0..n {
                let q = m.step(&p, tm, 1.0, 0.0);
                n_front += (q.az < 25.0 || q.az > 335.0) as usize;
                tm += clock_step(1.0, 0.13);
            }
            n_front as f32 / n as f32
        };
        let (lo, hi) = (share(0.08), share(0.4));
        assert!(lo < 0.14, "默认 {lo}");
        assert!(hi > 0.22 && hi > 2.0 * lo, "正面比例 0.4：{hi}（默认 {lo}）");
    }

    #[test]
    fn sweep_alternates_ears_and_stays_in_range() {
        let p = Params::default();
        let mut m = Motion::new(1);
        let (mut tm, mut left, mut right) = (0.0f64, 0, 0);
        for _ in 0..(60.0 / CTRL_DT) as usize {
            let pose = m.step(&p, tm, 1.0, 0.0);
            assert!(pose.r >= R_MIN && pose.r <= R_MAX && pose.r_base >= R_MIN);
            let s = pose.az.to_radians().sin();
            if s > 0.9 {
                right += 1;
            } else if s < -0.9 {
                left += 1;
            }
            tm += clock_step(1.0, 0.13);
        }
        assert!(m.n_moves >= 9 && m.n_moves <= 13, "moves {}", m.n_moves);
        assert!(left > 5000 && right > 5000);
        assert!(m.n_sways >= 6);
    }
}
