//! 引擎：把人声预处理、语音分析、运动、直达声、房间、回放补偿、响度和限制器串起来，逐块流式处理。
//! 实时模式和文件模式用的是同一个引擎；文件模式多了约 1 秒的"预读"，并把响度归一化留到第二遍做。

use super::ambience::{AmbienceSeparator, AMBIENCE_DELAY};
use super::analysis::{Analyzer, Features};
use super::direct::{DirectRenderer, BASE_DELAY};
use super::dynamics::{Leveler, Limiter};
use super::filters::{Biquad, Fir, OnePoleLp, BUTTER_Q};
use super::motion::{clock_step, clock_step_live, Motion, Pose};
use super::params::{Params, Playback};
use super::pinna::pinnas;
use super::rain::{Rain, RainSetup};
use super::room::{conv_energy, Room};
use super::tables::{tables, Hrir, Tables};
use super::voice::{ear_eq_fir, far_shadow_fir, muffle_fir, soft_voice_fir, BreathDeBoom, BreathFx, BreathTone, CanalComb, ContactFx, DeEsser, Denoiser, SharpSoftener, DENOISE_DELAY, EAR_EQ_TAPS};
use super::{db_to_lin, lin_to_db, smoothstep, C, CANAL_L0, CTRL, CTRL_DT, EAR_R, FS, NTAP, R_MAX, R_MIN};
use std::sync::Arc;

/// 直达声比 Python 版多出来的固定时延（样本）：分数延时插值器要"往后看"的那一半。
const ROOM_DELAY: usize = BASE_DELAY as usize - 4;

/// 真实房间：贴耳时远耳听到的房间声比近耳直达声低多少 dB（KU100 实测 −24…−35 dB）。
const REAL_ROOM_DB: f32 = -28.0;

/// 入耳：伸进耳道这么深（米）就算完全"在耳道里"（近耳不再经过耳廓、共鸣和耳内低频全开）。
const INEAR_FULL: f32 = 0.003;
/// 入耳打开时整体压低多少 dB（给耳内低频留余量，使实时模式下未经自动响度的输出与人声预设差不多响）。
const INEAR_TRIM_DB: f32 = 14.0;
/// 入耳：完全伸进耳道时另一只耳朵再压低多少 dB。两段 KU100 掏耳实录里另一只耳朵只剩底噪
/// （比近耳低 35–46 dB，受底噪限制，实际更低）。
const INEAR_FAR_DB: f32 = 30.0;

/// 由参数算出来的滤波器等"重"数据，在界面线程里算好再交给音频线程。
pub struct Derived {
    pub ear_eq: Option<Arc<Vec<f32>>>,
    pub playback: Option<Arc<Vec<f32>>>,
    pub soft: Option<Arc<Vec<f32>>>,
    /// 贴耳时远耳的高频遮挡。
    pub far_eq: Option<Arc<Vec<f32>>>,
    /// 贴耳轻声用的均衡（与轻声化同一种滤波器，强度按 `soft_near`）。
    pub soft_near: Option<Arc<Vec<f32>>>,
    /// 入耳时近耳的堵耳闷感。
    pub muffle: Option<Arc<Vec<f32>>>,
    /// 入耳时近耳的增益：与"贴在耳道口正外面（正侧方、离耳 0.75 cm）"的直达声一样响
    /// （含"响度随位置变化"在那个位置的补偿），所以伸进、退出耳道时响度是连着的；不随耳朵外面那一点位置的晃动变化。
    pub inear_gain: f32,
    /// 耳廓选择：真人耳廓在 [`pinnas`] 里的序号；None = KU100 自己的耳廓。
    pub pinna: Option<usize>,
    pub room_gain: f32,
    pub pref: f32,
    /// 背景音（雨声）的设置；音量为 0 时 None。
    pub rain: Option<Arc<RainSetup>>,
}

/// 参数 + 派生数据的一份不可变快照。
pub struct EngineConfig {
    pub params: Params,
    pub derived: Derived,
}

impl EngineConfig {
    /// 根据参数生成配置；`prev` 里没变的部分直接复用。
    pub fn new(params: Params, prev: Option<&EngineConfig>) -> Self {
        let tab = tables();
        let pp = prev.map(|c| &c.params);
        let pd = prev.map(|c| &c.derived);
        let same = |f: &dyn Fn(&Params) -> bool| pp.map_or(false, f);

        let ear_eq = if same(&|q| q.presence_db == params.presence_db && q.dark_db == params.dark_db && q.ku100_eq == params.ku100_eq && q.concha_db == params.concha_db) {
            pd.unwrap().ear_eq.clone()
        } else {
            ear_eq_fir(params.presence_db, params.dark_db, params.ku100_eq, params.concha_db).map(Arc::new)
        };
        let soft = if same(&|q| q.soft_voice == params.soft_voice) {
            pd.unwrap().soft.clone()
        } else if params.soft_voice > 0.0 {
            Some(Arc::new(soft_voice_fir(params.soft_voice)))
        } else {
            None
        };
        let same_pb = same(&|q| q.playback == params.playback);
        let playback = if same_pb {
            pd.unwrap().playback.clone()
        } else {
            match &params.playback {
                Playback::Diffuse => Some(Arc::new(tab.cdfc.clone())),
                Playback::Native => None,
                Playback::Headphone(name) => tab.headphones.iter().find(|(n, _)| n == name).map(|(_, h)| Arc::new(h.clone())).or_else(|| Some(Arc::new(tab.cdfc.clone()))),
            }
        };
        let room_gain = if same_pb && same(&|q| q.wet_db == params.wet_db) {
            pd.unwrap().room_gain
        } else {
            // 贴耳（右耳、方位 100°）时近耳直达声的能量，与扇区 3 远耳（左耳）房间声的能量之比定标
            let mut h = Hrir::default();
            tab.lookup(260.0, EAR_R, &mut h);
            let pb = playback.as_ref().map(|v| v.as_slice());
            let e_near = conv_energy(&h.h[1], pb, usize::MAX);
            let e_room = conv_energy(tab.room_ir(3, 0), pb, tab.room_len);
            ((e_near / e_room.max(1e-30)) as f32 * 10f32.powf((REAL_ROOM_DB + params.wet_db) / 10.0)).sqrt()
        };
        let pref = tab.lookup_power(270.0, 0.14);
        let far_eq = if same(&|q| q.far_shadow_db == params.far_shadow_db) { pd.unwrap().far_eq.clone() } else { far_shadow_fir(params.far_shadow_db).map(Arc::new) };
        let soft_near = if same(&|q| q.soft_near == params.soft_near) {
            pd.unwrap().soft_near.clone()
        } else if params.soft_near > 0.0 {
            Some(Arc::new(soft_voice_fir(params.soft_near)))
        } else {
            None
        };
        let muffle = if same(&|q| q.muffle_db == params.muffle_db) { pd.unwrap().muffle.clone() } else { muffle_fir(params.muffle_db).map(Arc::new) };
        let inear_gain = if same(&|q| q.level_follow == params.level_follow) {
            pd.unwrap().inear_gain
        } else {
            // 正侧方、最近处两只近耳 HRIR 的能量取平均，再乘上那个位置的响度补偿
            let mut h = Hrir::default();
            tab.lookup(270.0, R_MIN, &mut h);
            let e_r: f32 = h.h[1].iter().map(|v| v * v).sum();
            tab.lookup(90.0, R_MIN, &mut h);
            let e_l: f32 = h.h[0].iter().map(|v| v * v).sum();
            let p_ear = 0.5 * (tab.lookup_power(270.0, R_MIN) + tab.lookup_power(90.0, R_MIN));
            (0.5 * (e_r + e_l)).sqrt() * (p_ear / pref).powf(-(1.0 - params.level_follow) / 2.0)
        };
        let pinna = pinnas().index(&params.pinna);
        let rain = if same_pb && same(&|q| q.bg_same(&params)) { pd.unwrap().rain.clone() } else { RainSetup::new(&params, playback.as_deref().map(|v| v.as_slice())).map(Arc::new) };
        EngineConfig { params, derived: Derived { ear_eq, playback, soft, far_eq, soft_near, muffle, inear_gain, pinna, room_gain, pref, rain } }
    }
}

/// 实时模式的背景音跟着配置走：打开时建一个（第一次会在音频线程里分配缓冲，和换回放补偿一样），
/// 改了就换设置，关掉时淡出（淡完由调用方丢掉）。引擎和多音频合成共用。
pub fn sync_rain(rain: &mut Option<Rain>, cfg: &EngineConfig) {
    match (&cfg.derived.rain, rain.as_mut()) {
        (Some(s), Some(r)) => {
            r.set_setup(s.clone());
            r.set_playback(cfg.derived.playback.as_ref());
        }
        (Some(s), None) => *rain = Some(Rain::new(s.clone(), cfg.params.seed, cfg.derived.playback.as_ref())),
        (None, Some(r)) => r.set_off(),
        (None, None) => {}
    }
}

/// 给界面显示用的实时状态。
#[derive(Clone, Copy, Debug, Default)]
pub struct Telemetry {
    pub az: f32,
    /// 仰角（度，向上为正）。
    pub el: f32,
    pub r: f32,
    pub closeness: f32,
    pub act: f32,
    pub in_db: f32,
    pub out_db: f32,
    pub gr_db: f32,
    pub level_gain_db: f32,
    pub n_moves: u32,
    pub n_sways: u32,
    pub n_tops: u32,
    pub n_updowns: u32,
    /// 入耳深度（厘米）和伸进耳道的次数。
    pub depth_cm: f32,
    pub n_strokes: u32,
}

/// 输出端怎么收尾。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PostMode {
    /// 实时：自动响度 + 限制器都在引擎里做。
    Realtime,
    /// 文件：输出不做响度和限幅，留给第二遍精确归一化。
    Offline,
}

/// 贴耳时近耳低频提升、远耳低频压低：4 阶 Linkwitz–Riley 分频，两耳相同的全通相位，ITD 不受影响。
struct EarLf {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
}

/// 原录音的稳定底响。两耳各用两个全通段，保持每耳的幅频响应平直；
/// 低频基本同相，中高频有固定的相位差。没有随人声变化的方向、增益或混响发送量。
struct FixedAmbience {
    split: AmbienceSeparator,
    delay: Vec<f32>,
    pos: usize,
    phase: [[Biquad; 2]; 2],
    gain: f32,
    transition: usize,
    tail: usize,
}

// 开启时先连续移到分离器的时间点（读速只差 1/64），再用 20 ms 分配前景与底声。
// 关闭时按相反顺序走，同一进度也能在快速拨动开关时原路折返。
const AMBIENCE_ALIGN: usize = AMBIENCE_DELAY * 64;
const AMBIENCE_FADE: usize = (0.020 * FS) as usize;
const AMBIENCE_ON: usize = AMBIENCE_ALIGN + AMBIENCE_FADE;

impl FixedAmbience {
    fn new(lookahead: usize, enabled: bool, gain: f32) -> Self {
        let allpass = |fc: f32| {
            let (s, c) = (std::f32::consts::TAU * fc / FS).sin_cos();
            let alpha = s / (2.0 * BUTTER_Q);
            Biquad::from_coeffs([1.0 - alpha, -2.0 * c, 1.0 + alpha], [1.0 + alpha, -2.0 * c, 1.0 - alpha])
        };
        Self {
            split: AmbienceSeparator::new(),
            delay: vec![0.0; lookahead + ROOM_DELAY],
            pos: 0,
            phase: [[allpass(1850.0), allpass(6800.0)], [allpass(2500.0), allpass(9050.0)]],
            gain,
            transition: if enabled { AMBIENCE_ON } else { 0 },
            tail: 0,
        }
    }

    fn separate(&mut self, x: f32, enabled: bool) -> (f32, f32) {
        if !enabled && self.transition == 0 {
            return (x, 0.0);
        }
        let (voice, bed) = self.split.process(x);
        if enabled {
            self.transition = (self.transition + 1).min(AMBIENCE_ON);
        } else {
            self.transition -= 1;
            if self.transition == 0 {
                self.split.reset();
                return (x, 0.0);
            }
        }
        if self.transition == AMBIENCE_ON {
            return (voice, bed);
        }
        if self.transition <= AMBIENCE_ALIGN {
            let delay = self.transition as f32 * AMBIENCE_DELAY as f32 / AMBIENCE_ALIGN as f32;
            return (self.split.dry_at_delay(delay), 0.0);
        }
        let wet = (self.transition - AMBIENCE_ALIGN) as f32 / AMBIENCE_FADE as f32;
        (voice + (1.0 - wet) * bed, wet * bed)
    }

    fn render(&mut self, bed: f32) -> [f32; 2] {
        // 与人声的离线预读和固定插值延迟对齐；开关后让已排队的底响自然播完。
        if bed != 0.0 {
            self.tail = self.delay.len() + 256;
        } else if self.tail != 0 {
            self.tail -= 1;
        }
        let delayed = self.delay[self.pos] * self.gain;
        self.delay[self.pos] = bed;
        self.pos = (self.pos + 1) % self.delay.len();
        self.phase.each_mut().map(|filters| {
            let a = filters[0].process(delayed);
            filters[1].process(a)
        })
    }
}

impl EarLf {
    fn new() -> Self {
        EarLf { lp: [Biquad::lowpass(320.0, BUTTER_Q); 2], hp: [Biquad::highpass(320.0, BUTTER_Q); 2] }
    }
    #[inline]
    fn process(&mut self, x: f32, g: f32) -> f32 {
        let lo = self.lp[0].process(x);
        let lo = self.lp[1].process(lo);
        let hi = self.hp[0].process(x);
        let hi = self.hp[1].process(hi);
        hi + g * lo
    }
}

/// 嘴的大小：沿嘴宽方向的取样点（相对半宽）和权重；中心点的权重是 0.30。
const MOUTH_SPREAD: [(f32, f32); 4] = [(-1.0, 0.10), (-0.5, 0.25), (0.5, 0.25), (1.0, 0.10)];
/// 离头中心超过这个距离时嘴可以当成一个点。
const MOUTH_R: f32 = 0.4;

/// 把 64 点的均衡滤波器作用到一只耳朵的 HRIR 上，并按贴耳程度 `cl` 与原来的混合（尾部 16 点淡出）。
fn eq_blend(h: &mut [f32; NTAP], eq: &[f32], cl: f32) {
    let mut he = [0f32; NTAP];
    for (n, o) in he.iter_mut().enumerate() {
        let kmax = n.min(EAR_EQ_TAPS - 1);
        let mut s = 0.0;
        for k in 0..=kmax {
            s += eq[k] * h[n - k];
        }
        *o = s;
    }
    for i in 0..16 {
        let t = i as f32 / 15.0;
        he[NTAP - 16 + i] *= 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
    }
    for (v, e) in h.iter_mut().zip(he.iter()) {
        *v += cl * (e - *v);
    }
}

/// 嘴贴近某一侧耳朵的程度（0–1）：距离越近、越偏向正侧方越大；转到脸前 / 脑后 / 头顶时为 0。
#[inline]
pub fn closeness(az: f32, el: f32, r: f32) -> f32 {
    let lat = az.to_radians().sin().abs() * el.to_radians().cos();
    ((0.2 - r) / (0.2 - EAR_R)).clamp(0.0, 1.0) * lat * lat
}

pub struct Engine {
    tab: &'static Tables,
    cfg: Arc<EngineConfig>,
    post: PostMode,
    in_hp: Biquad,
    fixed_ambience: FixedAmbience,
    denoiser: Denoiser,
    deess: DeEsser,
    sharp_softener: SharpSoftener,
    breath_deboom: BreathDeBoom,
    breath_tone: BreathTone,
    soft: Option<Fir>,
    soft_src: Option<Arc<Vec<f32>>>,
    softn: Option<Fir>,
    softn_src: Option<Arc<Vec<f32>>>,
    softw: (f32, f32),
    analyzer: Analyzer,
    la_buf: Vec<f32>,
    la_pos: usize,
    warm_lp: OnePoleLp,
    breath: BreathFx,
    contact: ContactFx,
    comb: [CanalComb; 2],
    comb_g: [(f32, f32); 2],
    comb_d: (f32, f32),
    direct: DirectRenderer,
    room: Room,
    lf: [EarLf; 2],
    pb: Option<[Fir; 2]>,
    pb_src: Option<Arc<Vec<f32>>>,
    out_hp: [Biquad; 2],
    leveler: Leveler,
    limiter: Limiter,
    motion: Motion,
    tm: f64,
    phase: usize,
    warm: (f32, f32),
    lfg: [(f32, f32); 2],
    feats: Features,
    pose: Pose,
    hr: Hrir,
    hr_tmp: Hrir,
    hr_sp: Hrir,
    xa: [f32; CTRL],
    xr: [f32; CTRL],
    room_dl: [f32; ROOM_DELAY],
    room_dl_pos: usize,
    xd: [f32; CTRL],
    dl: [f32; CTRL],
    dr: [f32; CTRL],
    in_pk: f32,
    out_pk: f32,
    primed: bool,
    pub tele: Telemetry,
    /// 统计：已处理的控制块数、其中在说话的块数。
    pub n_blocks: u64,
    pub n_speech: u64,
    /// 打开后记录每个控制块的 [时间 s, 方位角, 距离, 不含忽远忽近的距离, 说话活动, 仰角, 入耳深度 m]（导出轨迹用）。
    pub trace: Option<Vec<[f32; 7]>>,
    /// 多音频合成"一起移动"：整个位置绕头转这么多度（几个音源按同一套走位、彼此错开）。0 = 不转。
    pub az_offset: f32,
    /// 多音频合成"一起移动"：说话时钟按这个说话活动走，几个音源共用一个时钟、步调完全一致；None = 按自己的说话活动。
    pub clock_act: Option<f32>,
    amp_ms: f32,
    live: bool,
    pause_s: f32,
    /// 背景音（实时模式才有；文件模式在第二遍里加）。
    rain: Option<Rain>,
    rain_l: [f32; CTRL],
    rain_r: [f32; CTRL],
}

impl Engine {
    /// `lookahead_s`：分析比音频提前多少秒（实时为 0；文件模式约 1 s）。
    pub fn new(cfg: Arc<EngineConfig>, post: PostMode, lookahead_s: f32) -> Self {
        let analyzer = Analyzer::new(lookahead_s);
        let la = analyzer.lookahead_samples();
        let seed = cfg.params.seed;
        // 用 14 cm 参考位置的 KU100 双耳能量定一次标，之后底响增益与嘴的位置无关。
        let mut h = Hrir::default();
        tables().lookup(270.0, 0.14, &mut h);
        let bed_gain = (0.5 * h.h.iter().flatten().map(|v| v * v).sum::<f32>()).sqrt();
        let mut e = Engine {
            tab: tables(),
            cfg: cfg.clone(),
            post,
            in_hp: Biquad::highpass(40.0, BUTTER_Q),
            fixed_ambience: FixedAmbience::new(la, cfg.params.static_ambience, bed_gain),
            denoiser: Denoiser::new(),
            deess: DeEsser::new(),
            sharp_softener: SharpSoftener::new(),
            breath_deboom: BreathDeBoom::new(),
            breath_tone: BreathTone::new(),
            soft: None,
            soft_src: None,
            softn: None,
            softn_src: None,
            softw: (0.0, 0.0),
            analyzer,
            la_buf: vec![0.0; la],
            la_pos: 0,
            warm_lp: OnePoleLp::new(220.0),
            breath: BreathFx::new(),
            contact: ContactFx::new(),
            comb: [CanalComb::new(), CanalComb::new()],
            comb_g: [(0.0, 0.0); 2],
            comb_d: (CanalComb::delay(CANAL_L0), CanalComb::delay(CANAL_L0)),
            direct: DirectRenderer::new(),
            room: Room::new(tables()),
            lf: [EarLf::new(), EarLf::new()],
            pb: None,
            pb_src: None,
            out_hp: [Biquad::highpass(25.0, BUTTER_Q); 2],
            leveler: Leveler::new(),
            limiter: Limiter::new(2.0, 80.0),
            motion: Motion::new(seed),
            tm: 0.0,
            phase: 0,
            warm: (0.0, 0.0),
            lfg: [(1.0, 1.0); 2],
            feats: Features::default(),
            pose: Pose::default(),
            hr: Hrir::default(),
            hr_tmp: Hrir::default(),
            hr_sp: Hrir::default(),
            xa: [0.0; CTRL],
            xr: [0.0; CTRL],
            room_dl: [0.0; ROOM_DELAY],
            room_dl_pos: 0,
            xd: [0.0; CTRL],
            dl: [0.0; CTRL],
            dr: [0.0; CTRL],
            in_pk: 0.0,
            out_pk: 0.0,
            primed: false,
            tele: Telemetry::default(),
            n_blocks: 0,
            n_speech: 0,
            trace: None,
            az_offset: 0.0,
            clock_act: None,
            amp_ms: 1.0,
            live: false,
            pause_s: 0.0,
            rain: None,
            rain_l: [0.0; CTRL],
            rain_r: [0.0; CTRL],
        };
        e.set_config(cfg);
        e
    }

    /// 主播模式（麦克风直通）：停顿超过 1 秒后动作恢复正常速度，话少时声音也一直在动。
    /// 文件转换和试听不开：动作严格跟着说话走，与 Python 版一致。
    pub fn set_live(&mut self, on: bool) {
        self.live = on;
    }

    /// 换一份配置（在控制块边界生效）。
    pub fn set_config(&mut self, cfg: Arc<EngineConfig>) {
        let d = &cfg.derived;
        let changed = |a: &Option<Arc<Vec<f32>>>, b: &Option<Arc<Vec<f32>>>| match (a, b) {
            (Some(x), Some(y)) => !Arc::ptr_eq(x, y),
            (None, None) => false,
            _ => true,
        };
        if changed(&self.pb_src, &d.playback) {
            self.pb = d.playback.as_ref().map(|h| [Fir::new(h), Fir::new(h)]);
            self.pb_src = d.playback.clone();
        }
        if changed(&self.soft_src, &d.soft) {
            match (&mut self.soft, &d.soft) {
                (Some(f), Some(h)) if f.len() == h.len() => f.set_taps(h),
                (_, Some(h)) => self.soft = Some(Fir::new(h)),
                (_, None) => self.soft = None,
            }
            self.soft_src = d.soft.clone();
        }
        if changed(&self.softn_src, &d.soft_near) {
            match (&mut self.softn, &d.soft_near) {
                (Some(f), Some(h)) if f.len() == h.len() => f.set_taps(h),
                (_, Some(h)) => self.softn = Some(Fir::new(h)),
                (_, None) => self.softn = None,
            }
            self.softn_src = d.soft_near.clone();
        }
        if self.post == PostMode::Realtime {
            sync_rain(&mut self.rain, &cfg);
        }
        self.cfg = cfg;
    }

    /// 文件模式：用整段音频统计的说话阈值和中位音量。
    pub fn set_fixed_analysis(&mut self, thr_db: f32, median_db: f32) {
        self.analyzer.set_fixed(thr_db, median_db);
    }

    /// 下一个控制块的说话活动（0–1）。只在控制块边界上（刚处理完整数个控制块时）有意义：
    /// 下一次 `process` 开始那一块用的就是这个值。多音频合成"一起移动"用它算几个音源共同的说话时钟。
    pub fn upcoming_act(&self) -> f32 {
        self.analyzer.features().act
    }

    /// 算法本身的延时（样本，48 kHz）：可选降噪、可选底响分离、预读、分数延时插值、限制器。
    /// 声音从嘴传到耳朵的传播时间（贴耳时约 0.3 ms）是效果的一部分，不算在内。
    pub fn latency_samples(&self) -> usize {
        let p = &self.cfg.params;
        (if p.denoise { DENOISE_DELAY } else { 0 }) + (if p.static_ambience { AMBIENCE_DELAY } else { 0 })
            + self.la_buf.len() + ROOM_DELAY + if self.post == PostMode::Realtime { self.limiter.delay() } else { 0 }
    }

    /// 控制块开始：推进说话时钟和位置，算出块末尾的滤波器、时延和各种增益。
    fn begin_block(&mut self) {
        let cfg = self.cfg.clone();
        let p = &cfg.params;
        self.feats = self.analyzer.features();
        let f = self.feats;
        self.n_blocks += 1;
        if f.act > 0.5 {
            self.n_speech += 1;
        }
        let (step, amp) = if p.pause_motion < 1.0 {
            // 晃动幅度随音量（0.5–1.6 倍），并按说话时段的均方把整体幅度归一，平均幅度不变
            let a = db_to_lin(f.loud_db).clamp(0.5, 1.6);
            if f.act > 0.5 {
                self.amp_ms += super::filters::ema_coef(8.0, CTRL_DT) * (a * a - self.amp_ms);
            }
            // 说话时钟：平时按自己的说话活动走；"一起移动"时按几个音源共同的说话活动走
            let ca = self.clock_act.unwrap_or(f.act);
            let step = if self.live {
                if ca > 0.5 {
                    self.pause_s = 0.0;
                } else {
                    self.pause_s += CTRL_DT;
                }
                clock_step_live(ca, p.pause_motion, self.pause_s)
            } else {
                clock_step(ca, p.pause_motion)
            };
            (step, a / self.amp_ms.max(0.25).sqrt())
        } else {
            (CTRL_DT as f64, 1.0)
        };
        // 轨迹时间与音频对齐：预读和底响分离积累输入的这段时间，文件模式不会提前移动。
        let pre_roll = self.la_buf.len() + if p.static_ambience { AMBIENCE_DELAY } else { 0 };
        let pre_blocks = (pre_roll / CTRL) as u64;
        if self.n_blocks > pre_blocks {
            self.tm += step;
        }
        let mut pose = self.motion.step_with_activity(p, self.tm, amp, f.follow_db, self.clock_act.unwrap_or(f.act));
        if self.az_offset != 0.0 {
            // 整个位置绕竖直轴转过去（刚体旋转，贴耳、上下的几何关系都不变）
            pose.az = (pose.az + self.az_offset).rem_euclid(360.0);
            pose.az_base = (pose.az_base + self.az_offset).rem_euclid(360.0);
        }
        self.pose = pose;
        if let Some(tr) = &mut self.trace {
            let t = self.n_blocks.saturating_sub(pre_blocks) as f32 * CTRL_DT;
            tr.push([t, pose.az, pose.r, pose.r_base, f.act, pose.el, pose.depth]);
        }
        let az_sofa = (360.0 - pose.az).rem_euclid(360.0); // 顺时针 -> SOFA 逆时针
        self.tab.lookup3(az_sofa, pose.el, pose.r, &mut self.hr, &mut self.hr_tmp);
        // 耳廓选择：换成真人的耳廓
        if let Some(pi) = cfg.derived.pinna {
            pinnas().apply(pi, &mut self.hr.h, az_sofa, pose.el, pose.r);
        }
        // 嘴的大小：嘴是一个开口而不是一个点。沿嘴宽方向取 5 个点，把各点的 HRIR 加权相加；
        // 贴耳时开口从耳朵看过去张角很大，耳廓的窄谷被平均变浅，离远了与点声源相同
        let r_m = pose.r.clamp(R_MIN, R_MAX);
        if p.mouth_cm > 0.0 && r_m < MOUTH_R {
            let half = 0.005 * p.mouth_cm;
            let w0 = 1.0 - MOUTH_SPREAD.iter().map(|&(_, w)| w).sum::<f32>();
            for e in 0..2 {
                for v in self.hr.h[e].iter_mut() {
                    *v *= w0;
                }
            }
            for (t, w) in MOUTH_SPREAD {
                let d = (t * half).atan2(r_m).to_degrees();
                self.tab.lookup3((az_sofa + d).rem_euclid(360.0), pose.el, pose.r, &mut self.hr_sp, &mut self.hr_tmp);
                if let Some(pi) = cfg.derived.pinna {
                    pinnas().apply(pi, &mut self.hr_sp.h, (az_sofa + d).rem_euclid(360.0), pose.el, pose.r);
                }
                for e in 0..2 {
                    for (v, s) in self.hr.h[e].iter_mut().zip(self.hr_sp.h[e].iter()) {
                        *v += w * s;
                    }
                }
            }
        }

        // 响度：沿不含忽远忽近的轨迹补偿；退开时说话人自己会说大声一点（只保留一部分物理衰减）
        let az_base_sofa = (360.0 - pose.az_base).rem_euclid(360.0);
        let p_base = self.tab.lookup_power3(az_base_sofa, pose.el_base, pose.r_base).max(1e-20);
        let mut g = if p.level_follow != 1.0 { (p_base / cfg.derived.pref).powf(-(1.0 - p.level_follow) / 2.0) } else { 1.0 };
        let mut talker = 1.0;
        if (pose.r - pose.r_base).abs() > 1e-6 && p.sway_level < 1.0 {
            let p_now = self.tab.lookup_power3(az_sofa, pose.el, pose.r).max(1e-20);
            talker = (p_now / p_base).powf(-(1.0 - p.sway_level) / 2.0);
            g *= talker;
        }
        // 入耳（掏耳）打开时整体留出余量：耳内低频比摩擦声高十几分贝，不留的话实时模式的自动响度拉不回来
        let trim = if p.inear_cm > 0.0 { db_to_lin(-INEAR_TRIM_DB) } else { 1.0 };
        g *= trim;
        // 人头麦贴耳感：远耳额外压低，近耳加临场 / 空气感
        let rc = pose.r.clamp(R_MIN, R_MAX);
        let cl = closeness(pose.az, pose.el, rc);
        let ipsi = if pose.az.rem_euclid(360.0) < 180.0 { 1 } else { 0 };
        for e in 0..2 {
            let mut ge = g;
            if e != ipsi && p.isolation_db > 0.0 {
                ge *= db_to_lin(-p.isolation_db * cl);
            }
            for v in self.hr.h[e].iter_mut() {
                *v *= ge;
            }
        }
        if cl > 0.0 {
            if let Some(eq) = &cfg.derived.ear_eq {
                let h = &mut self.hr.h[ipsi];
                let mut he = [0f32; NTAP];
                for (n, o) in he.iter_mut().enumerate() {
                    let kmax = n.min(EAR_EQ_TAPS - 1);
                    let mut s = 0.0;
                    for k in 0..=kmax {
                        s += eq[k] * h[n - k];
                    }
                    *o = s;
                }
                for i in 0..16 {
                    let t = i as f32 / 15.0;
                    he[NTAP - 16 + i] *= 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
                }
                for (v, e) in h.iter_mut().zip(he.iter()) {
                    *v += cl * (e - *v);
                }
            }
        }
        // 贴耳时远耳的高频遮挡
        if cl > 0.0 {
            if let Some(eq) = &cfg.derived.far_eq {
                eq_blend(&mut self.hr.h[1 - ipsi], eq, cl);
            }
        }
        // 入耳（掏耳）：声源伸进耳道后，近耳听到的不再经过耳廓，也不再随耳朵外面那一点位置的晃动变化——
        // 换成平直的响应（与贴在耳道口正外面一样响），再加堵耳的闷感；另一只耳朵基本听不到。
        // 耳道共鸣和耳内低频在后面逐样本处理
        let win = if p.inear_cm > 0.0 { smoothstep(pose.depth / INEAR_FULL) } else { 0.0 };
        if win > 0.0 {
            let h = &mut self.hr.h[ipsi];
            let g0 = cfg.derived.inear_gain * trim;
            for (i, v) in h.iter_mut().enumerate() {
                *v += win * (if i == 0 { g0 } else { 0.0 } - *v);
            }
            if let Some(eq) = &cfg.derived.muffle {
                eq_blend(h, eq, win);
            }
            let gf = db_to_lin(-INEAR_FAR_DB * win);
            for v in self.hr.h[1 - ipsi].iter_mut() {
                *v *= gf;
            }
        }
        let mut comb_g = [0f32; 2];
        if win > 0.0 && p.canal_db > 0.0 {
            comb_g[ipsi] = CanalComb::feedback(p.canal_db) * win;
        }
        let comb_d = CanalComb::delay(CANAL_L0 - pose.depth);
        self.contact.begin_block(if p.rumble > 0.0 { win * p.rumble } else { 0.0 });
        let d = [rc / C * FS + self.hr.tau[0] + BASE_DELAY, rc / C * FS + self.hr.tau[1] + BASE_DELAY];
        self.direct.set_target(&self.hr.h, d);

        // 自身气息也可在近距离正面听见；气流扑耳仍只随贴耳程度渐入。
        self.breath.begin_block_at(cl, rc, p.breath_reach, p.breath_db, p.puff);

        // 近讲温暖感（两耳共同）、贴耳低频（近耳加、远耳减）
        let prox = ((0.6 - pose.r) / (0.6 - 0.13)).clamp(0.0, 1.0);
        let warm = if p.warmth_db > 0.0 { db_to_lin(p.warmth_db * prox) - 1.0 } else { 0.0 };
        let mut lfg = [1.0f32; 2];
        for (e, v) in lfg.iter_mut().enumerate() {
            *v = if e == ipsi { db_to_lin(p.ear_warmth_db * cl) } else { db_to_lin(-p.far_lf_cut_db * cl) };
        }
        if self.primed {
            self.warm = (self.warm.1, warm);
            self.softw = (self.softw.1, cl);
            self.comb_d = (self.comb_d.1, comb_d);
            for e in 0..2 {
                self.lfg[e] = (self.lfg[e].1, lfg[e]);
                self.comb_g[e] = (self.comb_g[e].1, comb_g[e]);
            }
        } else {
            self.warm = (warm, warm);
            self.softw = (cl, cl);
            self.comb_d = (comb_d, comb_d);
            for e in 0..2 {
                self.lfg[e] = (lfg[e], lfg[e]);
                self.comb_g[e] = (comb_g[e], comb_g[e]);
            }
            self.primed = true;
        }
        self.room.set_target(pose.az, pose.el, talker);

        self.tele = Telemetry {
            az: pose.az,
            el: pose.el,
            r: pose.r,
            closeness: cl,
            act: f.act,
            in_db: lin_to_db(self.in_pk),
            out_db: lin_to_db(self.out_pk),
            gr_db: self.limiter.gr_db,
            level_gain_db: self.leveler.gain_db,
            n_moves: self.motion.n_moves,
            n_sways: self.motion.n_sways,
            n_tops: self.motion.n_tops,
            n_updowns: self.motion.n_updowns,
            depth_cm: pose.depth * 100.0,
            n_strokes: self.motion.n_strokes,
        };
        self.in_pk *= 0.97;
        self.out_pk *= 0.97;
    }

    /// 处理一段单声道输入（任意长度），输出等长的左右声道。
    pub fn process(&mut self, input: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let n = input.len();
        let mut i = 0;
        while i < n {
            if self.phase == 0 {
                self.begin_block();
            }
            let m = (n - i).min(CTRL - self.phase);
            self.run(&input[i..i + m], &mut out_l[i..i + m], &mut out_r[i..i + m]);
            self.phase += m;
            if self.phase == CTRL {
                self.analyzer.end_block();
                self.breath.end_block();
                self.phase = 0;
            }
            i += m;
        }
    }

    fn run(&mut self, input: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let cfg = self.cfg.clone();
        let p = &cfg.params;
        let m = input.len();
        let ph = self.phase;
        let la = self.la_buf.len();
        let dn_mix = p.denoise_mix();
        let sharp_mix = 1.0 - db_to_lin(-p.sharp_soften_db.clamp(0.0, 8.0));
        let deboom_mix = 1.0 - db_to_lin(-p.breath_deboom_db.clamp(0.0, 12.0));
        let breath_on = p.breath_db > 0.0 || p.puff > 0.0;
        let tone = p.breath_tone.clamp(0.0, 1.0);
        // 薄声缺气流时最多补至原气流旋钮约 4 倍档，旋钮关掉后仍然没有气流。
        let puff_boost = (4.0 / p.puff.max(0.25) - 1.0).max(0.0);
        let rumble_on = p.inear_cm > 0.0 && p.rumble > 0.0;
        let mut bed = [0.0; CTRL];
        for i in 0..m {
            let x = input[i];
            self.in_pk = self.in_pk.max(x.abs());
            let mut v = self.in_hp.process(x);
            if p.denoise {
                v = self.denoiser.process(v, dn_mix);
            }
            (v, bed[i]) = self.fixed_ambience.separate(v, p.static_ambience);
            if p.deess_db > 0.0 {
                v = self.deess.process(v, p.deess_db);
            }
            if let Some(f) = &mut self.soft {
                v = f.process(v);
            }
            self.analyzer.push(v);
            let a = if la == 0 {
                v
            } else {
                let o = self.la_buf[self.la_pos];
                self.la_buf[self.la_pos] = v;
                self.la_pos = (self.la_pos + 1) % la;
                o
            };
            let a = self.sharp_softener.process(a, sharp_mix);
            let a = self.breath_deboom.process(a, deboom_mix);
            let (a, thin) = self.breath_tone.process(a, tone);
            self.xa[i] = a;
            // 房间声的输入和直达声一样多延后几个样本（分数延时插值器的固定时延），两者的相对时间与 Python 版一致
            self.xr[i] = self.room_dl[self.room_dl_pos];
            self.room_dl[self.room_dl_pos] = a;
            self.room_dl_pos = (self.room_dl_pos + 1) % ROOM_DELAY;
            let wp = (ph + i) as f32 / CTRL as f32;
            let gb = self.warm.0 + (self.warm.1 - self.warm.0) * wp;
            // 贴耳轻声：只作用于直达声，随贴耳程度渐入
            let a = if let Some(f) = &mut self.softn {
                let s = f.process(a);
                a + (self.softw.0 + (self.softw.1 - self.softw.0) * wp) * (s - a)
            } else {
                a
            };
            self.xd[i] = if breath_on {
                // 只作用于直达声；房间声仍用原来的人声
                let (ab, pf) = self.breath.process(a, wp);
                ab + gb * self.warm_lp.process(ab) + pf * (1.0 + puff_boost * thin)
            } else {
                a + gb * self.warm_lp.process(a)
            };
            if rumble_on {
                // 耳内低频：由输入自己的包络导出，加在直达声的输入上，由近耳的响应送到伸进去的那只耳朵
                self.xd[i] += self.contact.process(a, wp);
            }
        }
        self.direct.process(&self.xd[..m], ph, &mut self.dl[..m], &mut self.dr[..m]);
        if p.inear_cm > 0.0 && p.canal_db > 0.0 {
            // 耳道共鸣：只作用在伸进去的那只耳朵上（另一只的反馈系数为 0，原样通过）
            for i in 0..m {
                let wp = (ph + i) as f32 / CTRL as f32;
                let d = self.comb_d.0 + (self.comb_d.1 - self.comb_d.0) * wp;
                let gl = self.comb_g[0].0 + (self.comb_g[0].1 - self.comb_g[0].0) * wp;
                let gr = self.comb_g[1].0 + (self.comb_g[1].1 - self.comb_g[1].0) * wp;
                self.dl[i] = self.comb[0].process(self.dl[i], gl, d);
                self.dr[i] = self.comb[1].process(self.dr[i], gr, d);
            }
        }
        if p.ear_warmth_db > 0.0 || p.far_lf_cut_db > 0.0 {
            for i in 0..m {
                let wp = (ph + i) as f32 / CTRL as f32;
                let gl = self.lfg[0].0 + (self.lfg[0].1 - self.lfg[0].0) * wp;
                let gr = self.lfg[1].0 + (self.lfg[1].1 - self.lfg[1].0) * wp;
                self.dl[i] = self.lf[0].process(self.dl[i], gl);
                self.dr[i] = self.lf[1].process(self.dr[i], gr);
            }
        }
        if p.room {
            // 入耳打开时直达声整体留了余量，房间声跟着一起压低，两者的比例不变
            let room_gain = if p.inear_cm > 0.0 { cfg.derived.room_gain * db_to_lin(-INEAR_TRIM_DB) } else { cfg.derived.room_gain };
            self.room.process(&self.xr[..m], ph, room_gain, &mut self.dl[..m], &mut self.dr[..m]);
        }
        let speaking = self.feats.act > 0.5;
        let gain = db_to_lin(p.out_gain_db);
        let ceiling = db_to_lin(p.ceiling_db - 0.1);
        // 背景音：比人声的目标响度低"音量"那么多，加在自动响度之后、限制器之前（只有实时模式在这里加）
        let rain_on = match &mut self.rain {
            Some(rain) => {
                rain.render(p.target_lufs, &mut self.rain_l[..m], &mut self.rain_r[..m]);
                true
            }
            None => false,
        };
        if self.rain.as_ref().map_or(false, |r| r.silent()) {
            self.rain = None;
        }
        for i in 0..m {
            let (mut l, mut r) = (self.dl[i], self.dr[i]);
            if bed[i] != 0.0 || self.fixed_ambience.tail != 0 {
                let [bl, br] = self.fixed_ambience.render(bed[i]);
                l += bl;
                r += br;
            }
            if let Some(pb) = &mut self.pb {
                l = pb[0].process(l);
                r = pb[1].process(r);
            }
            l = self.out_hp[0].process(l);
            r = self.out_hp[1].process(r);
            if self.post == PostMode::Realtime {
                let lg = if p.auto_level { self.leveler.process(l, r, speaking, p.target_lufs) } else { 1.0 };
                let (a, b) = if rain_on {
                    self.limiter.process((l * lg + self.rain_l[i]) * gain, (r * lg + self.rain_r[i]) * gain, ceiling)
                } else {
                    self.limiter.process(l * lg * gain, r * lg * gain, ceiling)
                };
                l = a;
                r = b;
            }
            self.out_pk = self.out_pk.max(l.abs()).max(r.abs());
            out_l[i] = l;
            out_r[i] = r;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::params::MotionMode;

    fn render(p: Params, x: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let mut eng = Engine::new(Arc::new(EngineConfig::new(p, None)), PostMode::Offline, 0.0);
        let (mut l, mut r) = (vec![0f32; x.len()], vec![0f32; x.len()]);
        for (i, c) in x.chunks(480).enumerate() {
            let o = i * 480;
            eng.process(c, &mut l[o..o + c.len()], &mut r[o..o + c.len()]);
        }
        (l, r)
    }

    /// 用同一段麦克风底噪打底，先留三秒安静，再加入持续变化的发声。
    /// 两个等价渲染只镜像声源位置，底响应稳定，叠在上面的说话声应仍可定位。
    #[test]
    fn recorded_ambience_stays_put_while_the_voice_moves() {
        let fs = FS as usize;
        let voice = crate::dsp::testsig::test_voice(1.5);
        let mut seed = 12345u32;
        let x: Vec<_> = (0..5 * fs).map(|i| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = 0.003 * ((seed >> 8) as f32 / 8388608.0 - 1.0);
            noise + i.checked_sub(3 * fs).and_then(|n| voice.get(n)).copied().unwrap_or(0.0)
        }).collect();
        let ilds = |fixed, az| {
            let p = Params { mode: MotionMode::Manual, manual_az: az, manual_dist: 0.115, static_ambience: fixed, ..Params::default().style_ku100() };
            let (l, r) = render(p, &x);
            let ild = |start, end| band_db(&l[start..end], 250.0, 8000.0) - band_db(&r[start..end], 250.0, 8000.0);
            (ild(fs * 2, fs * 3), ild(fs * 3 + fs / 10, fs * 4))
        };
        for (az, side) in [(270.0, 1.0), (90.0, -1.0)] {
            let moving = ilds(false, az);
            let fixed = ilds(true, az);
            assert!(side * moving.0 > 10.0 && fixed.0.abs() < 1.0, "方位 {az}° 的底响声级差：原来 {moving:?}，固定后 {fixed:?}");
            assert!(side * fixed.1 > 10.0 && (fixed.1 - moving.1).abs() < 2.0, "方位 {az}° 的话音声级差：原来 {moving:?}，固定后 {fixed:?}");
        }
    }

    #[test]
    fn toggling_recorded_ambience_preserves_tone_level_and_returns_to_bypass() {
        // 延迟支路恰好比直通晚半整数个周期。若直接交叉淡化，两路在过渡中点会抵消。
        let mut fixed = FixedAmbience::new(0, false, 1.0);
        let omega = std::f64::consts::TAU * 5.5 / AMBIENCE_DELAY as f64;
        let mut sample = 0;
        let mut energy = 0.0;
        for (enabled, duration) in [
            (false, 512), (true, AMBIENCE_ON), (false, AMBIENCE_FADE / 2),
            (true, AMBIENCE_FADE), (false, AMBIENCE_ON / 2), (true, 512), (false, AMBIENCE_ON),
        ] {
            for _ in 0..duration {
                let x = (omega * sample as f64).sin() as f32;
                let (voice, bed) = fixed.separate(x, enabled);
                energy += (voice + bed).powi(2);
                sample += 1;
                if sample % 128 == 0 {
                    assert!((48.0..80.0).contains(&energy), "切换时第 {sample} 个采样处的短窗能量 {energy}，期望约 64");
                    energy = 0.0;
                }
            }
        }
        assert_eq!(fixed.separate(0.371, false), (0.371, 0.0));
        // 清空刚才的信号后再开启，旧底声和延迟干声都不能重播。
        for _ in 0..2 * AMBIENCE_DELAY {
            assert_eq!(fixed.separate(0.0, true), (0.0, 0.0));
        }
    }

    /// 冷启动先收到数字静音，再收到渐强的微弱有色底声，语音稍后才出现。
    #[test]
    fn ku100_keeps_a_fading_in_noise_floor_centered_from_the_opening() {
        let fs = FS as usize;
        let voice = crate::dsp::testsig::test_voice(0.9);
        let mut rng = crate::dsp::rng::Rng::new(105);
        let mut colored = 0.0;
        let input: Vec<f32> = (0..3 * fs).map(|i| {
            let t = i as f32 / FS;
            let white = rng.uniform(-1.0, 1.0);
            colored = 0.96 * colored + 0.04 * white;
            let fade = ((t - 0.2) / 0.4).clamp(0.0, 1.0);
            let noise = 0.00085 * fade * (colored + 0.08 * white);
            noise + i.checked_sub(2 * fs).and_then(|j| voice.get(j)).copied().unwrap_or(0.0)
        }).collect();
        let params = Params { mode: MotionMode::Manual, manual_az: 270.0, manual_dist: 0.115, ..Params::default().style_ku100() };
        let (left, right) = render(params, &input);
        let levels = |ms: usize, end_ms: usize| {
            let span = ms * fs / 1000..end_ms * fs / 1000;
            (band_db(&left[span.clone()], 80.0, 12000.0), band_db(&right[span], 80.0, 12000.0))
        };
        for (begin, end) in [(250, 400), (400, 800), (800, 1200), (1200, 1600)] {
            let (l, r) = levels(begin, end);
            assert!(l > -100.0 && r > -100.0 && (l - r).abs() < 3.0, "{begin}–{end} ms: {l:.1}/{r:.1} dB");
        }
        let (l, r) = levels(2100, 2700);
        assert!(l - r > 10.0, "开头底声固定后，左侧人声仍需清楚定位：{l:.1}/{r:.1} dB");
    }

    /// 背景音：实时引擎在人声之外加上雨声（没有人声时也在），比目标响度低"音量"那么多；拖到 0 淡出后完全消失，
    /// 文件模式的引擎不加（在第二遍里加）。
    #[test]
    fn realtime_engine_adds_rain_and_removes_it_when_turned_off() {
        let p = Params { bg_level: 60.0, rain_near: 0.0, rain_drip: 0.0, auto_level: false, ..Params::default() };
        let mut eng = Engine::new(Arc::new(EngineConfig::new(p.clone(), None)), PostMode::Realtime, 0.0);
        let x = vec![0f32; 20 * 48_000];
        let (mut l, mut r) = (vec![0f32; x.len()], vec![0f32; x.len()]);
        for (i, c) in x.chunks(64).enumerate() {
            eng.process(c, &mut l[i * 64..i * 64 + c.len()], &mut r[i * 64..i * 64 + c.len()]);
        }
        let got = crate::dsp::rain::tests::lufs(&l[48_000..], &r[48_000..]);
        let want = -16.0 + p.bg_db().unwrap() + 10.0 * p.rain / 100.0 - 5.0;
        assert!((got - want).abs() < 2.5, "实时雨声 {got:.1} LUFS，应约为 {want:.1}");
        let ceiling = db_to_lin(p.ceiling_db);
        assert!(l.iter().chain(&r).all(|v| v.abs() <= ceiling));
        // 关掉：淡出，之后输出是 0（人声也是 0），引擎里的雨声被丢掉
        eng.set_config(Arc::new(EngineConfig::new(Params { bg_level: 0.0, ..p.clone() }, None)));
        let (mut a, mut b) = (vec![0f32; 64], vec![0f32; 64]);
        for _ in 0..(2 * 48_000 / 64) {
            eng.process(&[0.0; 64], &mut a, &mut b);
        }
        assert!(eng.rain.is_none());
        eng.process(&[0.0; 64], &mut a, &mut b);
        assert!(a.iter().chain(&b).all(|v| v.abs() < 1e-6));
        // 文件模式的引擎不加雨声
        let (lo, ro) = render(p, &x[..48_000]);
        assert!(lo.iter().chain(&ro).all(|&v| v == 0.0));
    }

    /// 一段信号在 lo–hi Hz 里的能量（dB）：4 阶高通 + 4 阶低通。
    fn band_db(x: &[f32], lo: f32, hi: f32) -> f32 {
        let (mut a, mut b) = ([Biquad::highpass(lo, BUTTER_Q); 2], [Biquad::lowpass(hi, BUTTER_Q); 2]);
        let e: f64 = x
            .iter()
            .map(|&v| {
                let mut u = v;
                for f in a.iter_mut().chain(b.iter_mut()) {
                    u = f.process(u);
                }
                (u * u) as f64
            })
            .sum();
        10.0 * (e / x.len() as f64 + 1e-30).log10() as f32
    }

    /// 两口同频谱的呼吸相差 12 dB，间隔足以让房间尾音衰减。
    #[test]
    fn breath_style_preserves_side_pauses_and_dynamics() {
        let fs = FS as usize;
        let first = fs / 4;
        let second = first + 2 * fs;
        let mut rng = crate::dsp::rng::Rng::new(431);
        let mut filter = Biquad::bandpass(1400.0, 0.7);
        let mut x = vec![0.0; 4 * fs];
        for i in 0..fs {
            let envelope = (std::f32::consts::PI * i as f32 / FS).sin().powi(2);
            let breath = 0.035 * envelope * filter.process(rng.uniform(-1.0, 1.0));
            x[first + i] = breath;
            x[second + i] = 4.0 * breath;
        }
        let (l, r) = render(Params::default().style_breath(), &x);
        assert!(l[..first].iter().chain(&r[..first]).all(|&s| s == 0.0));
        let levels = |ear: &[f32], start| band_db(&ear[start + fs / 4..start + 3 * fs / 4], 200.0, 6000.0);
        let (l1, r1) = (levels(&l, first), levels(&r, first));
        let (l2, r2) = (levels(&l, second), levels(&r, second));
        for ild in [l1 - r1, l2 - r2] {
            assert!((5.0..32.0).contains(&ild.abs()), "近耳呼吸的双耳声级差 {ild:.1} dB");
        }
        let dynamics = l2.max(r2) - l1.max(r1);
        assert!((8.0..16.0).contains(&dynamics), "两口呼吸在近耳相差 {dynamics:.1} dB");
        for ear in [&l, &r] {
            let pause = band_db(&ear[7 * fs / 4..2 * fs], 200.0, 6000.0);
            assert!(pause < l1.max(r1) - 60.0, "呼吸间的停顿仍有 {pause:.1} dB");
        }
    }

    /// 掏耳预设：声音只在一只耳朵里（另一只低 40 dB 以上），带很强的耳内低频；输入里并没有低频。
    #[test]
    fn earclean_is_one_sided_and_rumbles() {
        let x = crate::dsp::testsig::test_scratch(20.0);
        assert!(band_db(&x, 25.0, 160.0) < band_db(&x, 300.0, 2000.0) - 25.0, "测试输入不该有低频");
        let (l, r) = render(Params::default().style_earclean(), &x);
        assert!(l.iter().chain(r.iter()).all(|v| v.is_finite()));
        let e = |v: &[f32]| 10.0 * (v.iter().map(|a| (a * a) as f64).sum::<f64>() / v.len() as f64 + 1e-30).log10() as f32;
        // 开场在左耳，20 秒内不换边
        assert!(e(&l) - e(&r) > 40.0, "左 {} dB，右 {} dB", e(&l), e(&r));
        let (lf, mid) = (band_db(&l, 25.0, 160.0), band_db(&l, 300.0, 2000.0));
        assert!(lf - mid > 6.0 && lf - mid < 24.0, "耳内低频比摩擦声高 {} dB（实录 10–20 dB）", lf - mid);
        // 关掉耳内低频：低频回到输入本来的样子（远低于摩擦声）
        let mut p = Params::default().style_earclean();
        p.rumble = 0.0;
        let (l0, _) = render(p, &x);
        assert!(band_db(&l0, 25.0, 160.0) < band_db(&l0, 300.0, 2000.0) - 8.0);
    }

    /// 耳道共鸣跟着深度走：伸到 1.2 cm 时共振在 5 kHz 附近，伸到 2.1 cm 时在 10.7 kHz 附近。
    #[test]
    fn canal_resonance_moves_with_depth() {
        // 白噪声，手动摆在左耳上（没有自动进出，伸到设定的深度）
        let mut seed = 99u32;
        let x: Vec<f32> = (0..96000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                0.1 * ((seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0)
            })
            .collect();
        let at = |cm: f32, canal: f32| {
            let mut p = Params::default().style_earclean();
            p.mode = MotionMode::Manual;
            (p.manual_az, p.manual_dist, p.manual_tilt) = (270.0, R_MIN, 0.0);
            (p.inear_cm, p.canal_db, p.rumble) = (cm, canal, 0.0);
            let (l, _) = render(p, &x);
            let s = &l[48000..];
            band_db(s, 9500.0, 12000.0) - band_db(s, 4500.0, 5600.0)
        };
        let (mid, deep) = (at(1.2, 10.0), at(2.1, 10.0));
        assert!(deep - mid > 8.0, "10 kHz 带相对 5 kHz 带：1.2 cm 时 {mid} dB，2.1 cm 时 {deep} dB");
        // 共鸣关掉后深浅不再改变音色
        let (mid0, deep0) = (at(1.2, 0.0), at(2.1, 0.0));
        assert!((deep0 - mid0).abs() < 0.5, "{mid0} {deep0}");
    }

    /// 入耳深度为 0 时，入耳这一组参数怎么设都不影响输出（与加入这一组之前逐样本相同）。
    #[test]
    fn inear_group_is_inert_at_zero_depth() {
        let x = crate::dsp::testsig::test_voice(8.0);
        for base in [Params::default(), Params::default().style_ku100()] {
            let mut q = base.clone();
            (q.inear_period, q.inear_random, q.canal_db, q.rumble, q.muffle_db) = (2.0, 0.1, 16.0, 3.0, 12.0);
            assert_eq!(q.inear_cm, 0.0);
            let (a, b) = (render(base, &x), render(q, &x));
            assert!(a.0 == b.0 && a.1 == b.1, "入耳深度为 0 时输出变了");
        }
    }
}
