//! 多音频合成：几段音频各用一个引擎、各在头边的一个位置，输出相加。两种排布：
//!
//! - **各自固定**（默认一个在左耳、一个在右耳）：每个音源的引擎参数 = 总参数，只是"左右移动"换成固定在它自己的
//!   位置（方位角、离头距离），随机种子各不相同：忽远忽近、上下、自然晃动照常，每个声音在自己的位置上各自轻轻地动，
//!   但不会跑到别的音源那边去。
//! - **一起移动**：几个音源都按总参数里的移动方式（穿梭、随机走位、环绕……）走，用同一个随机种子和同一个说话时钟
//!   （任何一个音源在说话时按真实时间走），所以走位一模一样；位置再整体绕头错开 360°/N。两个音源时正好相对：
//!   一个贴左耳时另一个贴右耳，换边时一个从脸前、一个从脑后绕过去，不会撞在一起。
//!
//! **左右双人**：同一段音频当成两个声音（默认一个在左耳、一个在右耳），第 2 个整体晚几十毫秒（[`MixSetup::delays`]），
//! 听起来是两个人一前一后、一左一右地说。延迟在读输入时加（输入第 k 段的第 t 个样本 = 原音频的第 t − 延迟 个），
//! 引擎、排布、自动平衡与多音频合成完全相同。实时（主播）模式也能用：麦克风的输入留一小段历史，第 2 个声音从里面晚一点取
//! （见 `realtime.rs`），那里没有自动平衡（要事先知道整段的响度）。
//!
//! 各音源先按输入响度（BS.1770 积分响度）对齐到同样响，再加上各自的音量增减；相加之后整体收尾——
//! 文件转换是两遍精确归一化（与转换单个文件相同），试听是自动响度 + 限制器（与单个文件的实时引擎相同）。

use super::balance::{BalanceData, Balancer, LiveLevels};
use super::dynamics::{Leveler, Limiter, LoudnessMeter};
use super::engine::{sync_rain, Engine, EngineConfig, PostMode, Telemetry};
use super::rain::Rain;
use super::filters::ema_coef;
use super::params::{MotionMode, Params};
use super::{db_to_lin, lin_to_db, CTRL, FS, R_MAX, R_MIN};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 最多同时合成几个音源。试听时每个音源是一个实时运行的引擎（每个约占单核 1%）。
pub const MAX_VOICES: usize = 4;

/// 左右双人：第 2 个声音最多晚多少毫秒、命令行只写 `--duo` 时用多少。
pub const DUO_MAX_MS: f32 = 300.0;
pub const DUO_DEFAULT_MS: f32 = 80.0;

/// 毫秒 → 样本数（48 kHz，四舍五入；负数当 0，最多 [`DUO_MAX_MS`]）。
pub fn delay_samples(ms: f32) -> usize {
    (ms.clamp(0.0, DUO_MAX_MS) * 0.001 * FS).round() as usize
}

/// 加了声部延迟之后的总长：各段（长 `lens[k]`、晚 `delays[k]` 个样本）都放完为止。
pub fn timeline_len(lens: &[usize], delays: &[usize]) -> usize {
    lens.iter().enumerate().map(|(k, &l)| l + delays.get(k).copied().unwrap_or(0)).max().unwrap_or(0)
}

/// 一段音频晚 `delay` 个样本之后，时间线上 `[s0, s1)` 这一段（前面不够的补 0；结尾不补，到音频结束为止）。
/// `delay` 为 0 时就是 `x[s0..s1]`（超出结尾的部分不要）。
pub fn delayed(x: &[f32], delay: usize, s0: usize, s1: usize) -> Vec<f32> {
    let end = s1.min(x.len() + delay);
    if end <= s0 {
        return Vec::new();
    }
    // 音频在时间线上从 `delay` 开始：之前的部分是 0
    let a = s0.max(delay).min(end);
    let mut out = Vec::with_capacity(end - s0);
    out.resize(a - s0, 0.0);
    if a < end {
        out.extend_from_slice(&x[a - delay..end - delay]);
    }
    out
}

/// 边播边改延迟时，从旧的位置淡到新的位置用多少样本（20 ms），免得一下跳过去咔嗒一声。
pub const DELAY_FADE: usize = 960;

/// 各声音这一段（时间线上 `pos..pos+n`）的输入：第 k 个按"t − 它的延迟"取（左右双人的第 2 个声音晚一点）。
/// `fading`：改了延迟后的淡变已经走了多少样本，期间是新旧两个位置的混合。`get(k, s)`：第 k 段的第 s 个样本。
/// 试听文件、主播模式的麦克风和 [`LiveMix`]（安卓、浏览器插件的网页声音）都用它。
pub fn read_delayed(inbs: &mut [Vec<f32>], pos: usize, n: usize, dl: &[usize; MAX_VOICES], dl_old: &[usize; MAX_VOICES], fading: Option<usize>, get: impl Fn(usize, usize) -> f32) {
    for (k, b) in inbs.iter_mut().enumerate() {
        let at = |t: usize, d: usize| if t >= d { get(k, t - d) } else { 0.0 };
        let (new, old) = (dl[k], dl_old[k]);
        for (i, v) in b[..n].iter_mut().enumerate() {
            let t = pos + i;
            *v = match fading {
                Some(done) if new != old => {
                    let w = ((done + i + 1) as f32 / DELAY_FADE as f32).min(1.0);
                    at(t, old) * (1.0 - w) + at(t, new) * w
                }
                _ => at(t, new),
            };
        }
    }
}

/// 一个音源的位置和音量。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Voice {
    /// 方位角（度，顺时针：0 前 90 右 180 后 270 左）。
    pub az: f32,
    /// 到头中心的距离（米）；None = 跟随"贴耳距离"。
    pub dist: Option<f32>,
    /// 音量增减（dB），在自动对齐响度之后再加。
    pub gain_db: f32,
}

/// 位置预设：（中文名, 英文名, 方位角, 到头中心的距离；None = 贴耳距离）。左右耳的方位角与"贴耳角度"的默认值（100°，略靠后）一致。
pub const PRESETS: [(&str, &str, f32, Option<f32>); 8] = [
    ("左耳", "Left ear", 260.0, None),
    ("右耳", "Right ear", 100.0, None),
    ("正前", "Front", 0.0, Some(0.35)),
    ("正后", "Back", 180.0, Some(0.35)),
    ("左前", "Front left", 315.0, Some(0.3)),
    ("右前", "Front right", 45.0, Some(0.3)),
    ("左后", "Back left", 225.0, Some(0.3)),
    ("右后", "Back right", 135.0, Some(0.3)),
];

/// 新音源依次放到哪里（`PRESETS` 的下标）：先左耳、右耳——两个音源时一左一右，离得最远——再正前、正后。
const DEFAULT_ORDER: [usize; MAX_VOICES] = [0, 1, 2, 3];

/// 两个方位角相差多少度（0–180）。
fn ang_diff(a: f32, b: f32) -> f32 {
    ((a - b + 180.0).rem_euclid(360.0) - 180.0).abs()
}

impl Voice {
    pub fn preset(i: usize) -> Self {
        let (_, _, az, dist) = PRESETS[i];
        Voice { az, dist, gain_db: 0.0 }
    }

    /// 和哪个预设位置相同（不看音量）。
    pub fn preset_index(&self) -> Option<usize> {
        PRESETS.iter().position(|&(_, _, az, d)| {
            ang_diff(self.az, az) < 0.5
                && match (self.dist, d) {
                    (None, None) => true,
                    (Some(a), Some(b)) => (a - b).abs() < 1e-3,
                    _ => false,
                }
        })
    }

    /// 实际到头中心的距离（米）。`ear_dist`：总参数里的贴耳距离。
    pub fn distance(&self, ear_dist: f32) -> f32 {
        self.dist.unwrap_or(ear_dist).clamp(R_MIN, R_MAX)
    }
}

/// 给一个新音源挑位置：按默认顺序取第一个附近（30° 以内）没有别的音源的预设；都有了就取离已有音源最远的预设。
pub fn default_voice(used: &[Voice]) -> Voice {
    let free = |az: f32| used.iter().all(|u| ang_diff(u.az, az) >= 30.0);
    if let Some(&i) = DEFAULT_ORDER.iter().find(|&&i| free(PRESETS[i].2)) {
        return Voice::preset(i);
    }
    let gap = |i: usize| used.iter().map(|u| ang_diff(u.az, PRESETS[i].2)).fold(180.0f32, f32::min);
    Voice::preset((0..PRESETS.len()).max_by(|&a, &b| gap(a).total_cmp(&gap(b))).unwrap_or(0))
}

/// 一个音源的引擎参数：总参数里"左右移动"换成固定在它自己的位置，随机种子按序号错开，
/// 几个声音的忽远忽近、上下、晃动各走各的节奏。第 0 个音源的种子与总参数相同。
pub fn voice_params(base: &Params, v: &Voice, k: usize) -> Params {
    let mut p = base.clone();
    p.mode = MotionMode::Static;
    p.static_az = v.az.rem_euclid(360.0);
    p.ear_dist = v.distance(base.ear_dist);
    p.seed = base.seed.wrapping_add(7919 * k as u64);
    p
}

/// 交给引擎（音频线程）的一份合成配置。
pub struct MixSetup {
    /// 每个音源的引擎配置。
    pub cfgs: Vec<Arc<EngineConfig>>,
    /// 每个音源的线性增益（响度对齐 + 音量增减）。
    pub gains: Vec<f32>,
    /// 每个音源的方位角偏移（度）：一起移动时是 0、360/N、2·360/N……；各自固定时全是 0。
    pub offsets: Vec<f32>,
    /// 一起移动：几个音源共用一个说话时钟、同一套走位，位置按 `offsets` 错开。
    pub follow: bool,
    /// 自动平衡：各音源事先由整段算好的输入响度（None = 不平衡）和强度（0–1）。主播模式（[`Mixer::set_live`]）
    /// 的输入响度由混音器边听边算，不看 `balance`，只看强度。
    pub balance: Option<Arc<BalanceData>>,
    pub balance_strength: f32,
    /// 各音源比时间线晚多少个样本（左右双人：[0, 延迟]；多音频合成全是 0）。延迟由读输入的一方加
    /// （文件转换截取各段时、试听读音频时），混音器只用它让自动平衡取对位置。
    pub delays: Vec<usize>,
}

impl MixSetup {
    /// 由总配置、各音源的位置和音量、各音源的响度（对齐用）算出合成配置。`follow`：一起移动，否则各自固定。
    /// `prev` 里参数没变的音源配置原样复用。
    pub fn new(base: &Arc<EngineConfig>, voices: &[Voice], lufs: &[Option<f32>], follow: bool, prev: Option<&MixSetup>) -> Self {
        let n = voices.len();
        // 一起移动：每个音源都直接用总配置（同一个移动方式、同一个随机种子）
        let cfgs = if follow { vec![base.clone(); n] } else { voice_configs(base, voices, prev) };
        MixSetup { cfgs, gains: balance_gains(lufs, voices), offsets: follow_offsets(n, follow), follow, balance: None, balance_strength: 0.0, delays: vec![0; n] }
    }

    /// 设各音源的延迟（样本；少给的按 0）。
    pub fn with_delays(mut self, delays: &[usize]) -> Self {
        for (d, &v) in self.delays.iter_mut().zip(delays) {
            *d = v;
        }
        self
    }

    /// 加上自动平衡（强度 0–1；为 0 或没有数据时不平衡）。
    pub fn with_balance(mut self, data: Option<Arc<BalanceData>>, strength: f32) -> Self {
        self.balance = data.filter(|_| strength > 0.0);
        self.balance_strength = strength;
        self
    }
}

/// 各音源的方位角偏移：一起移动时均匀分布在一圈上（两个时相隔 180°，正好相对）；各自固定时全是 0。
pub fn follow_offsets(n: usize, follow: bool) -> Vec<f32> {
    (0..n).map(|k| if follow { 360.0 * k as f32 / n as f32 } else { 0.0 }).collect()
}

/// 每个音源的引擎配置。滤波器等派生数据直接用 `base` 里算好的（位置不影响它们），所以很轻；
/// `prev` 里参数没变的音源原样复用。
pub fn voice_configs(base: &EngineConfig, voices: &[Voice], prev: Option<&MixSetup>) -> Vec<Arc<EngineConfig>> {
    voices
        .iter()
        .enumerate()
        .map(|(k, v)| {
            let p = voice_params(&base.params, v, k);
            match prev.and_then(|s| s.cfgs.get(k)) {
                Some(c) if c.params == p => c.clone(),
                _ => Arc::new(EngineConfig::new(p, Some(base))),
            }
        })
        .collect()
}

/// 单声道音频的积分响度（BS.1770，LUFS）；几乎无声时返回 None。
pub fn loudness(x: &[f32]) -> Option<f32> {
    let mut m = LoudnessMeter::new();
    for &v in x {
        m.push(v, 0.0);
    }
    let l = m.integrated();
    (l > -69.9).then_some(l)
}

/// 各音源的线性增益：先把响度对齐到各音源的平均响度（最多拉 ±30 dB；无声的音源不动），再加各自的音量增减。
pub fn balance_gains(lufs: &[Option<f32>], voices: &[Voice]) -> Vec<f32> {
    let align = align_db(lufs);
    voices.iter().enumerate().map(|(k, v)| db_to_lin(align.get(k).copied().unwrap_or(0.0) + v.gain_db)).collect()
}

/// 只是响度对齐的那部分（dB，不含各自的音量增减）：自动平衡在它之上再按短时响度补。
pub fn align_db(lufs: &[Option<f32>]) -> Vec<f32> {
    let known: Vec<f32> = lufs.iter().flatten().copied().collect();
    let mean = if known.is_empty() { 0.0 } else { known.iter().sum::<f32>() / known.len() as f32 };
    lufs.iter().map(|l| l.map_or(0.0, |l| (mean - l).clamp(-30.0, 30.0))).collect()
}

fn delay_array(d: &[usize]) -> [usize; MAX_VOICES] {
    std::array::from_fn(|k| d.get(k).copied().unwrap_or(0))
}

/// 每次最多处理多少样本（更长的输入分段处理，音频线程里不分配内存）。
const SCRATCH: usize = 4096;

/// 几个音源的引擎 + 相加 + 收尾（实时：自动响度和限制器；文件：不收尾，留给第二遍精确归一化）。
pub struct Mixer {
    engines: Vec<Engine>,
    /// 目标增益、当前增益（试听中改音量时约 20 ms 平滑过去，不会咔嗒）。
    target: Vec<f32>,
    gains: Vec<f32>,
    post: PostMode,
    /// 一起移动：几个引擎按控制块同步推进，共用说话时钟。`phase`：当前控制块里已经处理了多少样本（各引擎相同）。
    follow: bool,
    phase: usize,
    /// 收尾用的参数（自动响度、目标响度、输出增益、峰值上限）取第 0 个音源的配置——各音源这几项都相同。
    cfg: Arc<EngineConfig>,
    leveler: Leveler,
    limiter: Limiter,
    buf: Vec<[Vec<f32>; 2]>,
    /// 自动平衡：输入响度数据、强度、运行状态，以及下一个输入样本在各音频里的位置（试听每块由 [`Mixer::seek`] 设）。
    bal_data: Option<Arc<BalanceData>>,
    bal_strength: f32,
    balancer: Balancer,
    /// 主播模式：边听边算的输入响度（1 号声音的输入就是时间线上的麦克风）。
    live_lv: Option<LiveLevels>,
    pos: usize,
    /// 各音源的延迟（样本，见 [`MixSetup::delays`]）。
    delays: [usize; MAX_VOICES],
    out_pk: f32,
    /// 背景音（实时模式才有，相加之后加一份；文件模式在第二遍里加）。
    rain: Option<Rain>,
    rain_buf: [Vec<f32>; 2],
    /// 给界面显示的合成状态：输入取各音源中最大的，输出、限幅、自动响度是相加之后的；位置是第 0 个音源的。
    pub tele: Telemetry,
}

impl Mixer {
    /// `lookahead_s`：分析预读（实时 0，文件约 1 s）。各音源的引擎都不做响度和限幅，相加之后统一做。
    /// 排布方式（各自固定 / 一起移动）和音源数以这里为准，之后不能改（要改就重新开始）。
    pub fn new(setup: &MixSetup, post: PostMode, lookahead_s: f32) -> Self {
        assert!(!setup.cfgs.is_empty(), "合成至少要有一个音源");
        let n = setup.cfgs.len();
        let gains: Vec<f32> = (0..n).map(|k| setup.gains.get(k).copied().unwrap_or(1.0)).collect();
        let mut engines: Vec<Engine> = setup.cfgs.iter().map(|c| Engine::new(c.clone(), PostMode::Offline, lookahead_s)).collect();
        for (e, &o) in engines.iter_mut().zip(&setup.offsets) {
            e.az_offset = o;
        }
        Mixer {
            engines,
            target: gains.clone(),
            gains,
            post,
            follow: setup.follow && n > 1,
            phase: 0,
            cfg: setup.cfgs[0].clone(),
            leveler: Leveler::new(),
            limiter: Limiter::new(2.0, 80.0),
            buf: (0..n).map(|_| [vec![0.0; SCRATCH], vec![0.0; SCRATCH]]).collect(),
            bal_data: setup.balance.clone(),
            bal_strength: setup.balance_strength,
            balancer: Balancer::new(n),
            live_lv: None,
            pos: 0,
            delays: delay_array(&setup.delays),
            out_pk: 0.0,
            rain: None,
            rain_buf: [Vec::new(), Vec::new()],
            tele: Telemetry::default(),
        }
        .with_rain()
    }

    fn with_rain(mut self) -> Self {
        if self.post == PostMode::Realtime {
            sync_rain(&mut self.rain, &self.cfg);
        }
        self
    }

    pub fn voices(&self) -> usize {
        self.engines.len()
    }

    pub fn engine(&self, k: usize) -> &Engine {
        &self.engines[k]
    }

    pub fn engine_mut(&mut self, k: usize) -> &mut Engine {
        &mut self.engines[k]
    }

    /// 主播模式（麦克风）：每个声音的引擎都按 [`Engine::set_live`]——停顿超过 1 秒后动作恢复正常速度。
    /// 一起移动时几个引擎共用说话时钟（各自数停顿，但数的是同一个说话活动），所以仍然同步。
    /// 自动平衡的输入响度改成边听边算（[`LiveLevels`]，各声音的输入都是同一个麦克风、按自己的延迟取）。
    /// 会分配内存，要在开始处理之前调用。
    pub fn set_live(&mut self, on: bool) {
        for e in self.engines.iter_mut() {
            e.set_live(on);
        }
        self.set_live_balance(on);
    }

    /// 只把自动平衡的输入响度改成边听边算（[`LiveLevels`]），动作照旧：安卓和浏览器插件的网页声音是连续的录音，
    /// 动作严格跟着说话走（同试听），但没有整段事先算好的响度。会分配内存，要在开始处理之前调用。
    pub fn set_live_balance(&mut self, on: bool) {
        self.live_lv = on.then(LiveLevels::new);
    }

    /// 是不是"一起移动"（构造时定的，之后不变）。
    pub fn is_follow(&self) -> bool {
        self.follow
    }

    /// 换配置（在各引擎的控制块边界生效）：参数、位置、音量。不分配内存，可以在音频线程里调用；
    /// 音源数、排布方式和方位偏移以构造时为准（`setup` 里的这几项不看）。
    pub fn set_setup(&mut self, setup: &MixSetup) {
        for (e, c) in self.engines.iter_mut().zip(&setup.cfgs) {
            e.set_config(c.clone());
        }
        for (t, &g) in self.target.iter_mut().zip(&setup.gains) {
            *t = g;
        }
        if let Some(c) = setup.cfgs.first() {
            self.cfg = c.clone();
        }
        if self.post == PostMode::Realtime {
            sync_rain(&mut self.rain, &self.cfg);
        }
        self.bal_data = setup.balance.clone();
        self.bal_strength = setup.balance_strength;
        self.delays = delay_array(&setup.delays);
    }

    /// 各音源现在的延迟（样本）：试听读输入时按它取（变了就淡过去）。
    pub fn delays(&self) -> [usize; MAX_VOICES] {
        self.delays
    }

    /// 自动平衡的统计：几个声音都在出声时最响和最轻相差多少，平衡前、后的（中位数, 90% 分位）dB。
    pub fn balance_stats(&self) -> Option<((f32, f32), (f32, f32))> {
        self.balancer.stats()
    }

    /// 下一块输入从各音频的第几个样本开始（自动平衡按它取输入响度）。文件转换从截取的起点开始；试听每块都设一次。
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// 算法延时（样本）：各引擎相同，实时模式再加限制器的前瞻。
    pub fn latency_samples(&self) -> usize {
        self.engines[0].latency_samples() + if self.post == PostMode::Realtime { self.limiter.delay() } else { 0 }
    }

    /// 每个音源一段等长的单声道输入（`inputs[k]` 对应第 k 个音源），输出相加后的左右声道。
    pub fn process(&mut self, inputs: &[&[f32]], out_l: &mut [f32], out_r: &mut [f32]) {
        self.run(inputs, out_l, out_r, false);
    }

    /// 同 [`Mixer::process`]，但各音源的引擎各在一个线程里算（文件转换用）。每个引擎仍按顺序处理自己的音频，
    /// 相加按固定顺序，所以结果与 `process` 逐位相同。一起移动时几个引擎要逐个控制块同步，仍按顺序算。
    pub fn process_parallel(&mut self, inputs: &[&[f32]], out_l: &mut [f32], out_r: &mut [f32]) {
        self.run(inputs, out_l, out_r, true);
    }

    fn run(&mut self, inputs: &[&[f32]], out_l: &mut [f32], out_r: &mut [f32], par: bool) {
        let n = out_l.len().min(out_r.len());
        let mut o = 0;
        while o < n {
            let m = (n - o).min(SCRATCH);
            self.block(inputs, o, m, &mut out_l[o..o + m], &mut out_r[o..o + m], par);
            o += m;
        }
        self.pos += n;
    }

    fn block(&mut self, inputs: &[&[f32]], o: usize, m: usize, out_l: &mut [f32], out_r: &mut [f32], par: bool) {
        static SILENCE: [f32; SCRATCH] = [0.0; SCRATCH];
        let input = |k: usize| inputs.get(k).and_then(|x| x.get(o..o + m)).unwrap_or(&SILENCE[..m]);
        if self.follow {
            // 一起移动：几个引擎按控制块同步推进。每块开始前取各音源这一块的说话活动的最大值交给它们，
            // 说话时钟完全一致（任何一个音源在说话就按真实时间走），同一个随机种子下走位也就一模一样
            let mut i = 0;
            while i < m {
                if self.phase == 0 {
                    let act = self.engines.iter().map(|e| e.upcoming_act()).fold(0.0f32, f32::max);
                    for e in self.engines.iter_mut() {
                        e.clock_act = Some(act);
                    }
                }
                let step = (m - i).min(CTRL - self.phase);
                for (k, (eng, [bl, br])) in self.engines.iter_mut().zip(self.buf.iter_mut()).enumerate() {
                    eng.process(&input(k)[i..i + step], &mut bl[i..i + step], &mut br[i..i + step]);
                }
                self.phase = (self.phase + step) % CTRL;
                i += step;
            }
        } else if par && self.engines.len() > 1 {
            std::thread::scope(|s| {
                for (k, (eng, [bl, br])) in self.engines.iter_mut().zip(self.buf.iter_mut()).enumerate() {
                    let x = input(k);
                    s.spawn(move || {
                        crate::dsp::flush_denormals();
                        eng.process(x, &mut bl[..m], &mut br[..m]);
                    });
                }
            });
        } else {
            for (k, (eng, [bl, br])) in self.engines.iter_mut().zip(self.buf.iter_mut()).enumerate() {
                eng.process(input(k), &mut bl[..m], &mut br[..m]);
            }
        }
        // 自动平衡：各音源空间化之后、相加之前，按此刻听起来多响补增益（引擎有时延：这一块输出对应更早的输入）
        let in_pos = (self.pos + o) as i64 - self.engines[0].latency_samples() as i64;
        if let Some(lv) = &mut self.live_lv {
            // 主播模式：输入响度边听边算（一直记着，随时打开都有过去几秒的数据）；强度调到 0 后增益慢慢回到 0 dB 再停
            lv.feed(input(0));
            if self.bal_strength > 0.0 || !self.balancer.settled() {
                self.balancer.process(lv, self.bal_strength, in_pos, &self.delays, &mut self.buf, m);
            }
        } else if let Some(d) = &self.bal_data {
            self.balancer.process(d.as_ref(), self.bal_strength, in_pos, &self.delays, &mut self.buf, m);
        }
        out_l.fill(0.0);
        out_r.fill(0.0);
        let kg = ema_coef(0.02, 1.0 / FS);
        for (k, [bl, br]) in self.buf.iter().enumerate() {
            let tgt = self.target[k];
            let mut g = self.gains[k];
            if g == tgt {
                for i in 0..m {
                    out_l[i] += g * bl[i];
                    out_r[i] += g * br[i];
                }
            } else {
                for i in 0..m {
                    g += kg * (tgt - g);
                    out_l[i] += g * bl[i];
                    out_r[i] += g * br[i];
                }
                if (g - tgt).abs() <= 1e-4 * tgt.abs() {
                    g = tgt;
                }
                self.gains[k] = g;
            }
        }
        if self.post == PostMode::Realtime {
            // 与单个音源的实时引擎相同的收尾：有任何一个音源在说话时测响度
            let speaking = self.engines.iter().any(|e| e.tele.act > 0.5);
            let p = &self.cfg.params;
            let gain = db_to_lin(p.out_gain_db);
            let ceiling = db_to_lin(p.ceiling_db - 0.1);
            // 背景音：相加之后加一份，和单个音源的引擎一样在自动响度之后、限制器之前
            let rain_on = match &mut self.rain {
                Some(rain) => {
                    for b in self.rain_buf.iter_mut() {
                        if b.len() < m {
                            b.resize(SCRATCH, 0.0);
                        }
                    }
                    let [bl, br] = &mut self.rain_buf;
                    rain.render(p.target_lufs, &mut bl[..m], &mut br[..m]);
                    true
                }
                None => false,
            };
            for i in 0..m {
                let (l, r) = (out_l[i], out_r[i]);
                let lg = if p.auto_level { self.leveler.process(l, r, speaking, p.target_lufs) } else { 1.0 };
                let (a, b) = if rain_on {
                    self.limiter.process((l * lg + self.rain_buf[0][i]) * gain, (r * lg + self.rain_buf[1][i]) * gain, ceiling)
                } else {
                    self.limiter.process(l * lg * gain, r * lg * gain, ceiling)
                };
                out_l[i] = a;
                out_r[i] = b;
            }
            if self.rain.as_ref().map_or(false, |r| r.silent()) {
                self.rain = None;
            }
        }
        let pk = out_l.iter().chain(out_r.iter()).fold(0f32, |a, &v| a.max(v.abs()));
        self.out_pk = (self.out_pk * 0.97f32.powf(m as f32 / CTRL as f32)).max(pk);
        self.update_tele();
    }

    fn update_tele(&mut self) {
        let mut t = self.engines[0].tele;
        for e in &self.engines[1..] {
            let u = &e.tele;
            t.act = t.act.max(u.act);
            t.in_db = t.in_db.max(u.in_db);
            t.n_moves += u.n_moves;
            t.n_sways += u.n_sways;
            t.n_tops += u.n_tops;
            t.n_updowns += u.n_updowns;
            t.n_strokes += u.n_strokes;
        }
        t.out_db = lin_to_db(self.out_pk);
        let rt = self.post == PostMode::Realtime;
        t.gr_db = if rt { self.limiter.gr_db } else { 0.0 };
        t.level_gain_db = if rt { self.leveler.gain_db } else { 0.0 };
        self.tele = t;
    }
}

/// 左右双人的两个声音默认放在哪里：左耳、右耳（桌面版界面的默认位置，安卓和浏览器插件固定用它）。
pub const DUO_VOICES: [Voice; 2] = [Voice { az: PRESETS[0].2, dist: None, gain_db: 0.0 }, Voice { az: PRESETS[1].2, dist: None, gain_db: 0.0 }];

/// 边播边处理的左右双人用的合成配置：两个声音（`voices`）都是同一路输入，所以不做响度对齐；第 2 个晚 `delay_ms`；
/// `follow`：一起移动；`balance`：自动平衡强度（0–1，输入响度由合成器边听边算）。`prev` 里没变的配置原样复用。
pub fn duo_setup(base: &Arc<EngineConfig>, voices: &[Voice; 2], delay_ms: f32, follow: bool, balance: f32, prev: Option<&MixSetup>) -> MixSetup {
    MixSetup::new(base, voices, &[None, None], follow, prev).with_balance(None, balance.clamp(0.0, 1.0)).with_delays(&[0, delay_samples(delay_ms)])
}

/// 边播边处理时输入留多少历史（样本，2 的幂）：要装得下最长的延迟（300 ms）再加一段 [`SCRATCH`]。
const LIVE_HIST: usize = 1 << 15;
const _: () = assert!(LIVE_HIST >= (DUO_MAX_MS as usize) * (FS as usize) / 1000 + SCRATCH);

/// 边播边处理的左右双人：一路单声道输入进来，各声音按自己的延迟从输入的历史里取（改延迟时 20 ms 淡过去），
/// 交给 [`Mixer`]（实时收尾：自动响度、限制器、背景音），输出双耳。安卓版和浏览器插件处理网页声音用它；
/// 桌面版主播模式的麦克风是同一套做法（`realtime.rs`）。自动平衡的输入响度边听边算。构造时分配好，之后不分配内存。
pub struct LiveMix {
    mixer: Mixer,
    hist: Vec<f32>,
    /// 已经写进历史多少个样本（= 时间线上的位置）。
    pos: usize,
    dl: [usize; MAX_VOICES],
    dl_old: [usize; MAX_VOICES],
    dl_fade: usize,
    inbs: Vec<Vec<f32>>,
}

impl LiveMix {
    /// `live_motion`：动作按主播模式（停顿超过 1 秒后恢复正常速度）；网页声音是连续的录音，用 false（同试听）。
    pub fn new(setup: &MixSetup, live_motion: bool) -> Self {
        let mut mixer = Mixer::new(setup, PostMode::Realtime, 0.0);
        mixer.set_live(live_motion);
        mixer.set_live_balance(true);
        let dl = mixer.delays();
        let n = mixer.voices();
        LiveMix { mixer, hist: vec![0.0; LIVE_HIST], pos: 0, dl, dl_old: dl, dl_fade: 0, inbs: (0..n).map(|_| vec![0.0; SCRATCH]).collect() }
    }

    /// 换配置：参数、位置、音量、延迟（淡过去）、自动平衡强度。音源数和排布方式以构造时为准。
    pub fn set_setup(&mut self, setup: &MixSetup) {
        self.mixer.set_setup(setup);
    }

    pub fn mixer(&self) -> &Mixer {
        &self.mixer
    }

    /// 算法延时（样本）：和单个引擎的实时模式相同（只有第 2 个声音另外晚一点）。
    pub fn latency_samples(&self) -> usize {
        self.mixer.latency_samples()
    }

    /// 处理接下来的一段输入（任意长度），输出等长的左右声道。
    pub fn process(&mut self, x: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let n = x.len().min(out_l.len()).min(out_r.len());
        let mask = LIVE_HIST - 1;
        let mut o = 0;
        while o < n {
            let m = (n - o).min(SCRATCH);
            for (i, &v) in x[o..o + m].iter().enumerate() {
                self.hist[(self.pos + i) & mask] = v;
            }
            // 延迟变了：先把这次的淡变走完，再从旧位置淡到新位置
            if self.dl_fade == 0 {
                let want = self.mixer.delays();
                if want != self.dl {
                    self.dl_old = self.dl;
                    self.dl = want;
                    self.dl_fade = DELAY_FADE;
                }
            }
            let fading = (self.dl_fade > 0).then(|| DELAY_FADE - self.dl_fade);
            let (h, end) = (&self.hist, self.pos + m);
            read_delayed(&mut self.inbs, self.pos, m, &self.dl, &self.dl_old, fading, |_, s| if end - s <= LIVE_HIST { h[s & mask] } else { 0.0 });
            self.dl_fade = self.dl_fade.saturating_sub(m);
            let ins: [&[f32]; MAX_VOICES] = std::array::from_fn(|k| self.inbs.get(k).map_or(&[][..], |b| &b[..m]));
            self.mixer.process(&ins[..self.inbs.len()], &mut out_l[o..o + m], &mut out_r[o..o + m]);
            self.pos += m;
            o += m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testsig::test_voice;

    fn setup(voices: &[Voice], gains: Vec<f32>) -> MixSetup {
        let base = EngineConfig::new(Params::default(), None);
        MixSetup { cfgs: voice_configs(&base, voices, None), gains, offsets: vec![0.0; voices.len()], follow: false, balance: None, balance_strength: 0.0, delays: vec![0; voices.len()] }
    }

    #[test]
    fn delayed_windows_and_timeline() {
        let x: Vec<f32> = (1..=10).map(|v| v as f32).collect();
        // 不延迟：就是原来的那一段，超出结尾的不要
        assert_eq!(delayed(&x, 0, 2, 5), vec![3.0, 4.0, 5.0]);
        assert_eq!(delayed(&x, 0, 8, 20), vec![9.0, 10.0]);
        assert!(delayed(&x, 0, 12, 20).is_empty());
        // 晚 3 个样本：开头补 0，结尾多出 3 个
        assert_eq!(delayed(&x, 3, 0, 5), vec![0.0, 0.0, 0.0, 1.0, 2.0]);
        assert_eq!(delayed(&x, 3, 2, 6), vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(delayed(&x, 3, 10, 20), vec![8.0, 9.0, 10.0]);
        assert_eq!(delayed(&x, 30, 0, 4), vec![0.0; 4]);
        assert_eq!(timeline_len(&[10, 10], &[0, 3]), 13);
        assert_eq!(timeline_len(&[10, 20], &[5]), 20);
        assert_eq!(delay_samples(80.0), 3840);
        assert_eq!(delay_samples(1e6), delay_samples(DUO_MAX_MS));
    }

    fn energy(v: &[f32]) -> f64 {
        v.iter().map(|x| (x * x) as f64).sum::<f64>() + 1e-30
    }

    /// 前一半只有 A 在说、后一半只有 B 在说：前一半声音在左耳，后一半在右耳。
    fn split_voices(seconds: f32) -> (Vec<f32>, Vec<f32>, usize) {
        let x = test_voice(seconds);
        let half = x.len() / 2;
        let a = x.iter().enumerate().map(|(i, &v)| if i < half { v } else { 0.0 }).collect();
        let b = x.iter().enumerate().map(|(i, &v)| if i >= half { v } else { 0.0 }).collect();
        (a, b, half)
    }

    #[test]
    fn two_voices_stay_on_their_own_side() {
        let (a, b, half) = split_voices(12.0);
        let n = a.len();
        let mut mx = Mixer::new(&setup(&[Voice::preset(0), Voice::preset(1)], vec![1.0, 1.0]), PostMode::Offline, 0.0);
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        mx.process(&[&a, &b], &mut l, &mut r);
        // 跳过两段交界处（引擎的时延和尾音）各 0.2 s
        let g = (0.2 * FS) as usize;
        let first = 10.0 * (energy(&l[g..half]) / energy(&r[g..half])).log10();
        let second = 10.0 * (energy(&r[half + g..]) / energy(&l[half + g..])).log10();
        assert!(first > 20.0, "A 应该在左耳：左比右高 {first:.1} dB");
        assert!(second > 20.0, "B 应该在右耳：右比左高 {second:.1} dB");
        // 整段都在说话的那个音源一直留在自己那边：位置始终在左半边 / 右半边
        assert!(mx.engine(0).tele.az > 180.0 && mx.engine(1).tele.az < 180.0);
    }

    #[test]
    fn parallel_and_sequential_are_bit_identical() {
        let (a, b, _) = split_voices(6.0);
        let voices = [Voice::preset(0), Voice::preset(1), Voice::preset(2)];
        let c: Vec<f32> = a.iter().zip(&b).map(|(x, y)| 0.5 * (x + y)).collect();
        let n = a.len();
        let run = |par: bool| {
            let mut mx = Mixer::new(&setup(&voices, vec![1.0, 0.7, 0.5]), PostMode::Offline, 1.0);
            let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
            for o in (0..n).step_by(4096) {
                let e = (o + 4096).min(n);
                let ins = [&a[o..e], &b[o..e], &c[o..e]];
                if par {
                    mx.process_parallel(&ins, &mut l[o..e], &mut r[o..e]);
                } else {
                    mx.process(&ins, &mut l[o..e], &mut r[o..e]);
                }
            }
            (l, r)
        };
        let (s, p) = (run(false), run(true));
        assert!(s.0.iter().zip(&p.0).chain(s.1.iter().zip(&p.1)).all(|(x, y)| x.to_bits() == y.to_bits()));
    }

    #[test]
    fn loudness_alignment_and_gain_offsets() {
        let x = test_voice(8.0);
        let quiet: Vec<f32> = x.iter().map(|v| v * 0.25).collect(); // 低 12 dB
        let (la, lb) = (loudness(&x).unwrap(), loudness(&quiet).unwrap());
        assert!((la - lb - 12.04).abs() < 0.05, "{la} {lb}");
        let v = [Voice::preset(0), Voice { gain_db: -3.0, ..Voice::preset(1) }];
        let g = balance_gains(&[Some(la), Some(lb)], &v);
        // 对齐之后 B 比 A 低 3 dB（它自己的音量增减），整体在两者平均响度附近
        let (oa, ob) = (la + lin_to_db(g[0]), lb + lin_to_db(g[1]));
        assert!((oa - ob - 3.0).abs() < 0.01, "{oa} {ob}");
        assert!((oa - 0.5 * (la + lb)).abs() < 0.01);
        // 无声的音源不拉高，只加它自己的音量增减
        assert_eq!(loudness(&vec![0.0; 48_000 * 2]), None);
        let g = balance_gains(&[Some(la), None], &[Voice::preset(0), Voice { gain_db: 6.0, ..Voice::preset(1) }]);
        assert!((g[0] - 1.0).abs() < 1e-6 && (lin_to_db(g[1]) - 6.0).abs() < 1e-3);
    }

    #[test]
    fn default_positions_are_far_apart() {
        let mut used = Vec::new();
        for _ in 0..MAX_VOICES {
            let v = default_voice(&used);
            used.push(v);
        }
        // 两个音源：一左一右；四个：左、右、前、后，两两至少相隔 80°
        assert_eq!(used[0].preset_index(), Some(0));
        assert_eq!(used[1].preset_index(), Some(1));
        for i in 0..used.len() {
            for j in 0..i {
                assert!(ang_diff(used[i].az, used[j].az) >= 80.0, "{:?} {:?}", used[i], used[j]);
            }
        }
        // 第一个音源被挪到正前方后，新来的补到空出来的左耳
        let moved = [Voice::preset(2), Voice::preset(1)];
        assert_eq!(default_voice(&moved).preset_index(), Some(0));
        // 四个方向都占了：挑离已有音源最远的预设，不会叠在某个音源上
        let extra = default_voice(&used);
        assert!(used.iter().all(|u| ang_diff(u.az, extra.az) >= 40.0));
    }

    #[test]
    fn voices_keep_their_own_position_and_share_filters() {
        let base = EngineConfig::new(Params::default(), None);
        let voices = [Voice::preset(0), Voice { dist: Some(0.3), ..Voice::preset(5) }];
        let cfgs = voice_configs(&base, &voices, None);
        assert_eq!(cfgs[0].params.mode, MotionMode::Static);
        assert_eq!((cfgs[0].params.static_az, cfgs[0].params.ear_dist), (260.0, base.params.ear_dist));
        assert_eq!((cfgs[1].params.static_az, cfgs[1].params.ear_dist), (45.0, 0.3));
        assert_ne!(cfgs[0].params.seed, cfgs[1].params.seed);
        // 滤波器直接共用总参数里算好的，不重算
        assert!(Arc::ptr_eq(cfgs[1].derived.ear_eq.as_ref().unwrap(), base.derived.ear_eq.as_ref().unwrap()));
        // 参数没变的音源原样复用
        let prev = MixSetup { cfgs: cfgs.clone(), gains: vec![1.0; 2], offsets: vec![0.0; 2], follow: false, balance: None, balance_strength: 0.0, delays: vec![0; voices.len()] };
        let again = voice_configs(&base, &voices, Some(&prev));
        assert!(Arc::ptr_eq(&again[0], &cfgs[0]) && Arc::ptr_eq(&again[1], &cfgs[1]));
    }

    #[test]
    fn follow_mode_moves_together_and_stays_opposite() {
        // 一起移动（默认是穿梭）：A 一直在说，B 只在后半段说。两个音源始终在头的两侧相对，而且确实在两耳之间换边
        let a = test_voice(30.0);
        let n = a.len();
        let half = n / 2;
        let b: Vec<f32> = a.iter().enumerate().map(|(i, &v)| if i >= half { 0.7 * v } else { 0.0 }).collect();
        let base = Arc::new(EngineConfig::new(Params::default(), None));
        let st = MixSetup::new(&base, &[Voice::preset(0), Voice::preset(1)], &[None, None], true, None);
        assert_eq!(st.offsets, vec![0.0, 180.0]);
        assert!(Arc::ptr_eq(&st.cfgs[0], &base) && Arc::ptr_eq(&st.cfgs[1], &base));
        let mut mx = Mixer::new(&st, PostMode::Offline, 0.0);
        // 对照：A 单独过一个同样参数的引擎（前半段只有 A 在说，共同的说话时钟就是 A 自己的）
        let mut solo = Engine::new(base.clone(), PostMode::Offline, 0.0);
        let (mut l, mut r) = (vec![0f32; 1000], vec![0f32; 1000]);
        let (mut sl, mut sr) = (vec![0f32; 1000], vec![0f32; 1000]);
        let (mut min_gap, mut sides, mut max_dev) = (180.0f32, [0usize; 2], 0f32);
        // 每次 1000 个样本（不是 64 的整数倍），顺带检查控制块对齐
        for o in (0..n).step_by(1000) {
            let e = (o + 1000).min(n);
            mx.process(&[&a[o..e], &b[o..e]], &mut l[..e - o], &mut r[..e - o]);
            let (p, q) = (mx.engine(0).tele, mx.engine(1).tele);
            min_gap = min_gap.min(ang_diff(p.az, q.az));
            let s = p.az.to_radians().sin();
            if s < -0.9 {
                sides[0] += 1;
            } else if s > 0.9 {
                sides[1] += 1;
            }
            if e <= half {
                solo.process(&a[o..e], &mut sl[..e - o], &mut sr[..e - o]);
                max_dev = max_dev.max(ang_diff(p.az, solo.tele.az)).max((p.r - solo.tele.r).abs() * 100.0);
            }
        }
        assert!(min_gap > 150.0, "两个音源最近时只相隔 {min_gap:.0}°");
        assert!(sides[0] > 100 && sides[1] > 100, "A 应在两耳之间来回：{sides:?}");
        assert!(mx.engine(0).tele.n_moves >= 4);
        assert_eq!(mx.engine(0).tele.n_moves, mx.engine(1).tele.n_moves, "同一套走位");
        // 只有一个人在说话时，它的走位与单独转换完全相同
        assert!(max_dev < 1e-3, "与单独转换的走位相差 {max_dev}");
    }

    #[test]
    fn follow_mode_spreads_more_voices_evenly() {
        let base = Arc::new(EngineConfig::new(Params { mode: MotionMode::Orbit, ..Params::default() }, None));
        let voices: Vec<Voice> = (0..4).map(Voice::preset).collect();
        let st = MixSetup::new(&base, &voices, &[None; 4], true, None);
        assert_eq!(st.offsets, vec![0.0, 90.0, 180.0, 270.0]);
        let x = test_voice(10.0);
        let mut mx = Mixer::new(&st, PostMode::Offline, 0.0);
        let (mut l, mut r) = (vec![0f32; x.len()], vec![0f32; x.len()]);
        mx.process(&[&x, &x, &x, &x], &mut l, &mut r);
        // 环绕：四个音源相邻两个相隔约 90°
        for k in 0..4 {
            let gap = ang_diff(mx.engine(k).tele.az, mx.engine((k + 1) % 4).tele.az);
            assert!((gap - 90.0).abs() < 15.0, "音源 {k} 与下一个相隔 {gap:.0}°");
        }
        // 各自固定时没有偏移
        assert_eq!(MixSetup::new(&base, &voices, &[None; 4], false, None).offsets, vec![0.0; 4]);
    }

    #[test]
    fn live_mix_delays_the_second_voice_and_does_not_depend_on_chunking() {
        // 边播边处理的左右双人（安卓、浏览器插件）：第 2 个声音的输入逐样本等于原音频晚 80 ms（历史绕过好几圈后也一样）；
        // 两个声音一左一右；一次给多长（128、1000、4096…）输出都一样
        let x = test_voice(4.0);
        let base = Arc::new(EngineConfig::new(Params { auto_level: false, ..Params::default() }, None));
        let st = duo_setup(&base, &DUO_VOICES, 80.0, false, 0.0, None);
        assert_eq!(st.delays, vec![0, 3840]);
        let run = |chunks: &[usize]| {
            let mut lm = LiveMix::new(&st, false);
            let (mut l, mut r) = (vec![0f32; x.len()], vec![0f32; x.len()]);
            let (mut o, mut k) = (0, 0);
            while o < x.len() {
                let m = chunks[k % chunks.len()].min(x.len() - o);
                lm.process(&x[o..o + m], &mut l[o..o + m], &mut r[o..o + m]);
                // 刚处理完的这一段：1 号的输入就是原音频，2 号是原音频晚 3840 个样本
                for i in 0..m {
                    let t = o + i;
                    assert_eq!(lm.inbs[0][i], x[t]);
                    assert_eq!(lm.inbs[1][i], if t >= 3840 { x[t - 3840] } else { 0.0 });
                }
                o += m;
                k += 1;
            }
            (l, r, lm)
        };
        let (l, r, lm) = run(&[128]);
        assert!(lm.pos > LIVE_HIST);
        assert!(lm.mixer().engine(0).tele.az > 180.0 && lm.mixer().engine(1).tele.az < 180.0);
        let e = |v: &[f32]| v.iter().map(|s| (s * s) as f64).sum::<f64>();
        let lr = 10.0 * (e(&l) / e(&r)).log10();
        assert!(lr.abs() < 3.0, "左右两边应差不多一样响：{lr:.1} dB");
        for chunks in [&[1000usize][..], &[4096], &[7, 333, 2048, 64]] {
            let (l2, r2, _) = run(chunks);
            assert!(l.iter().zip(&l2).chain(r.iter().zip(&r2)).all(|(a, b)| a == b), "一次给 {chunks:?} 个样本时输出不一样");
        }
    }

    #[test]
    fn live_mix_changes_delay_smoothly_and_keeps_motion_strict() {
        // 播放中改延迟：20 ms 里淡过去，第 2 个声音的输入没有跳变，之后正好是新的延迟。
        // 动作不按主播模式（网页声音是连续的录音）：话很少时和严格跟着说话的试听一样几乎不动
        let x = test_voice(3.0);
        let base = Arc::new(EngineConfig::new(Params::default(), None));
        let mut lm = LiveMix::new(&duo_setup(&base, &DUO_VOICES, 100.0, false, 0.0, None), false);
        let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
        let mut o = 0;
        while o + 256 <= FS as usize {
            lm.process(&x[o..o + 256], &mut l, &mut r);
            o += 256;
        }
        lm.set_setup(&duo_setup(&base, &DUO_VOICES, 50.0, false, 0.0, None));
        let own_step = x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        let mut prev = lm.inbs[1][255];
        let mut max_step = 0f32;
        for b in 0..10 {
            lm.process(&x[o..o + 256], &mut l, &mut r);
            for i in 0..256 {
                let v = lm.inbs[1][i];
                if b * 256 + i < DELAY_FADE {
                    max_step = max_step.max((v - prev).abs());
                } else {
                    assert_eq!(v, x[o + i - 2400]);
                }
                prev = v;
            }
            o += 256;
        }
        assert!(max_step <= own_step * 1.01 + 1e-6, "淡变中一步跳了 {max_step}（原音频自己最多 {own_step}）");
        // 每说 1 秒停 8 秒，一起移动：不按主播模式时 30 秒里几乎不换边（主播模式下会照常换边）
        let v = test_voice(1.0);
        let sparse: Vec<f32> = (0..FS as usize * 30).map(|i| if i % (FS as usize * 9) < v.len() { v[i % (FS as usize * 9)] } else { 0.0 }).collect();
        let moves = |live: bool| {
            let mut lm = LiveMix::new(&duo_setup(&base, &DUO_VOICES, 80.0, true, 0.0, None), live);
            let (mut l, mut r) = (vec![0f32; sparse.len()], vec![0f32; sparse.len()]);
            lm.process(&sparse, &mut l, &mut r);
            lm.mixer().engine(0).tele.n_moves
        };
        let (strict, live) = (moves(false), moves(true));
        assert!(live >= strict + 2, "严格跟着说话 {strict} 次，主播模式 {live} 次");
    }

    #[test]
    fn live_mix_balance_works_without_precomputed_data() {
        // 自动平衡（输入响度边听边算）：2 号放在右边 50 cm 外，比贴着左耳的 1 号轻得多；100% 时两个声音之间的响度差明显变小
        let x = test_voice(16.0);
        let base = Arc::new(EngineConfig::new(Params { auto_level: false, ..Params::default() }, None));
        let voices = [DUO_VOICES[0], Voice { dist: Some(0.5), ..DUO_VOICES[1] }];
        let mut lm = LiveMix::new(&duo_setup(&base, &voices, 80.0, false, 1.0, None), false);
        let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
        for c in x.chunks_exact(512) {
            lm.process(c, &mut l, &mut r);
        }
        let ((b5, _), (a5, _)) = lm.mixer().balance_stats().expect("两个声音同时出声，应有统计");
        assert!(b5 > 4.0 && a5 < b5 / 2.0, "两个声音的响度差中位 {b5:.1} → {a5:.1} dB");
    }
}
