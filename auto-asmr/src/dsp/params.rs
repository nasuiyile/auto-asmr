//! 所有可调参数。实时模式和文件模式共用；界面上每个滑块对应这里的一个字段。

use super::{db_to_lin, EAR_R, HEAD_A, R_MAX, R_MIN};
use serde::{Deserialize, Serialize};

/// 降噪强度的最大值：取这个值（或更大）表示不设上限，降到最干净。
pub const DENOISE_MAX_DB: f32 = 100.0;

/// 掏耳预设文件转换的目标响度（LUFS）：能量集中在低频，K 加权的响度读数偏低、峰值却很高，所以比人声的 −16 低。
/// 两段 KU100 掏耳实录是 −18.3 和 −20.8 LUFS（峰值已经削顶）。
pub const EARCLEAN_LUFS: f32 = -20.0;
/// 掏耳预设实时 / 试听的目标响度：实时的自动响度按"有声音时的平均"来拉，一下一下的摩擦声实际会比目标高 3–5 LU，
/// 所以定得再低一些，实际落在 −19 LUFS 上下，限制器不用压太多。
pub const EARCLEAN_LIVE_LUFS: f32 = -23.0;
/// 三份枕边呼吸参考整轨响度中位约 −26 LUFS。
pub const BREATH_LUFS: f32 = -26.0;

/// 左右移动的方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MotionMode {
    /// 慢速穿梭：贴左耳停一会儿 → 慢慢移到右耳 → 停 → 移回，节奏规律、带一点随机。
    Sweep,
    /// 随机走位：停留多久、去哪一侧、落在耳边 / 侧前 / 侧后都随机。
    Random,
    /// 绕头转圈。
    Orbit,
    /// 固定在一个位置。
    Static,
    /// 手动：位置由用户在位置图上拖动决定，没有任何自动移动（穿梭、忽远忽近、自动上下、晃动都停掉）。
    Manual,
}

/// 换边时从哪里绕。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Via {
    Front,
    Back,
    Alternate,
    Random,
}

/// 回放端补偿。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Playback {
    /// 通用扩散场补偿（KU100 CDFC）。
    Diffuse,
    /// 不补偿：KU100 原机音色，和真实 KU100 录音一样。
    Native,
    /// 按耳机型号补偿（TH Köln 在同一个 KU100 上实测），值为型号名。
    Headphone(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    // ---- 贴耳感
    /// 贴耳时嘴到头中心的距离（米）。0.105 = 离耳约 1.7 cm。
    pub ear_dist: f32,
    /// 贴耳时远耳额外压低多少 dB（单耳独占感，0 = 纯物理）。
    pub isolation_db: f32,
    /// 贴耳时近耳的临场感 / 空气感，dB。
    pub presence_db: f32,
    /// 近耳音色用 KU100 实测标定的校正曲线（高频更收）。
    pub ku100_eq: bool,
    /// 贴耳时近耳 5 kHz 附近压低多少 dB（更"暗"的音色）。
    pub dark_db: f32,
    /// 凑近时两耳共同的低频温暖感，dB。
    pub warmth_db: f32,
    /// 贴耳时只给近耳加的低频，dB。
    pub ear_warmth_db: f32,
    /// 贴耳时远耳 320 Hz 以下再压低多少 dB。
    pub far_lf_cut_db: f32,
    /// 气声增强：贴耳时嗓音里轻的送气声 / 气息（2.5 kHz 以上）最多提高多少 dB；响的齿音不提。0 = 关闭。
    pub breath_db: f32,
    /// 近距离正面、侧前方也保留的气声增强比例（0–1）。从头中心 20 cm 到 65 cm 逐渐退去；0 = 只在贴耳时增强。
    pub breath_reach: f32,
    /// 气流扑耳：送气时气流扑到贴着的那只耳朵上的低频脉冲（50–130 Hz，由嗓音自己的送气声导出）。0 = 关闭，1 = 标准。
    pub puff: f32,
    /// 呼吸去闷：低中频相对中频突出时，最多压低 180 Hz 附近多少 dB。
    pub breath_deboom_db: f32,
    /// 薄声补偿，0–1。按非周期性活动声的频谱补充气流、柔化过亮的气声。
    pub breath_tone: f32,
    /// 耳甲共鸣：贴耳时近耳 4.3 kHz 的耳甲腔共振，dB。0 = 关闭。
    pub concha_db: f32,
    /// 嘴的大小（厘米）：嘴是约 2 cm 宽的开口而不是一个点，贴耳时耳廓的窄谷被平均变浅。0 = 点声源。
    pub mouth_cm: f32,
    /// 贴耳时远耳 6 kHz 以上再压低多少 dB（近处声源在远耳的高频头影比球头模型深）。0 = 关闭。
    pub far_shadow_db: f32,
    /// 贴耳轻声：凑到耳边时嗓音均衡成"放轻声音"（同 `soft_voice`，但只在贴耳时），退开时恢复。0 = 关闭。
    pub soft_near: f32,
    /// 耳廓选择："KU100" = 人工头自己的耳廓；"A"、"B"… = 真人耳廓（HUTUBS 实测，挑最像自己耳朵的那只）。
    pub pinna: String,

    // ---- 入耳（掏耳）：声源贴到耳朵上之后再伸进耳道。`inear_cm` 为 0 时这一组全部不起作用
    /// 入耳深度（厘米）：贴到耳朵上之后最深伸进耳道多少；0 = 不进耳道（关闭）。
    pub inear_cm: f32,
    /// 进出节奏：有声音时，一次"伸进去—退出来"大约几秒。
    pub inear_period: f32,
    /// 进出的随机程度（0 = 节奏和深浅固定，1 = 都随机）。
    pub inear_random: f32,
    /// 耳道共鸣：伸进耳道后，物体与话筒（鼓膜）之间那段耳道的共振峰，dB；伸得越深共振越高（约 3 → 11 kHz）。0 = 关闭。
    pub canal_db: f32,
    /// 耳内低频：伸进耳道时摩擦 / 触碰在耳道里形成的低频压力（25–170 Hz 的隆隆声），由输入自己的包络导出。0 = 关闭，1 = 标准。
    pub rumble: f32,
    /// 堵耳闷感：伸进耳道时近耳 2.5 kHz 以上压低多少 dB。0 = 关闭。
    pub muffle_db: f32,

    // ---- 左右移动
    pub mode: MotionMode,
    /// 每只耳朵边停留多少秒。
    pub hold_s: f32,
    /// 从一只耳朵移到另一只耳朵用多少秒。
    pub move_s: f32,
    /// 时长和落点的随机程度（0 = 完全规律，1 = 很不规律）。
    pub randomness: f32,
    /// 换边时改成"先退远再凑近"的概率。
    pub approach: f32,
    /// 随机走位：停在正前方（两耳听到的差不多）的比例。0.08 = 原来的比例。
    pub front: f32,
    pub via: Via,
    /// 贴耳的方位角（90 = 正侧方，100 = 略靠后）。
    pub ear_angle: f32,
    /// 穿过中间时离头中心的距离（米）。
    pub pass_dist: f32,
    /// 环绕模式：一圈的秒数、半径（米）。
    pub orbit_period: f32,
    pub orbit_radius: f32,
    /// 固定模式的方位角（0 前 90 右 180 后 270 左）。
    pub static_az: f32,
    /// 响度随位置变化的程度：1 = 物理真实，0 = 处处一样响。
    pub level_follow: f32,
    /// 手动模式的位置：方位角（度）、到头中心的距离（米）、高度（度，以耳朵为支点，含义同 `height`）。
    pub manual_az: f32,
    pub manual_dist: f32,
    pub manual_tilt: f32,

    // ---- 忽远忽近
    /// 随机凑近 / 退开的幅度（0 = 关闭）。
    pub sway: f32,
    /// 说话时一次凑近—退开的大致秒数。
    pub sway_period: f32,
    /// 凑近 / 退开的随机程度（0 = 固定节奏和深浅，1 = 时长和深浅都随机）。
    pub sway_random: f32,
    /// 跟着说话音量：声音大时往后让、轻声时更贴近（0 = 关闭）。
    pub sway_follow: f32,
    /// 退开时耳边音量保留物理衰减的比例。
    pub sway_level: f32,
    /// 停顿时动作的速度（相对说话时）；1 = 一直在动。
    pub pause_motion: f32,
    /// 穿梭时等待一组呼吸后的低电平间隙再换边。
    pub breath_pause_switch: bool,
    /// 自然晃动幅度倍数（1 约 1.2 cm）。
    pub micro: f32,

    // ---- 上下
    /// 高度：嘴相对耳朵向上（正）/ 向下（负）偏多少度，离耳朵的距离不变；0 = 与耳朵齐平。
    pub height: f32,
    /// 自动上下的幅度（度）：嘴在耳朵上方 / 下方之间慢慢换位置；0 = 关闭。
    pub updown: f32,
    /// 自动上下：说话时一个来回的大致秒数。
    pub updown_period: f32,
    /// 自动上下的随机程度（0 = 节奏和高低固定，1 = 时长和高低都随机）。
    pub updown_random: f32,
    /// 换耳时改成"从头顶绕过去"的概率（0 = 只从前 / 后绕，1 = 每次都从头顶绕）。
    pub over_top: f32,

    // ---- 房间与回放
    /// KU100 实测真实房间。
    pub room: bool,
    /// 房间声电平偏移，dB。
    pub wet_db: f32,
    /// 让原录音持续的环境底声留在固定双耳声场，人声仍按设定走位。
    pub static_ambience: bool,
    pub playback: Playback,

    // ---- 人声处理
    /// 输入降噪（RNNoise）。
    pub denoise: bool,
    /// 降噪强度，0–100：底噪最多压低多少 dB；[`DENOISE_MAX_DB`]（100）= 不设上限。
    /// 界面上是一个 0–100 的滑条，拖到 0 就是关闭（`denoise` 置为 false）。
    pub denoise_db: f32,
    /// 去齿音最多压低多少 dB（0 = 关闭）。
    pub deess_db: f32,
    /// 强辅音柔化：只在约 5 kHz 的频带突出且较响时局部衰减，dB；0 = 关闭。
    pub sharp_soften_db: f32,
    /// 轻声化（ANSI S3.5 说话力度均衡），0 = 关闭。
    pub soft_voice: f32,

    // ---- 输出
    /// 自动响度（实时模式：慢速把输出拉到目标响度；文件模式总是精确归一化）。
    pub auto_level: bool,
    /// 目标响度 LUFS。
    pub target_lufs: f32,
    /// 额外输出增益 dB。
    pub out_gain_db: f32,
    /// 真峰值上限 dBTP。
    pub ceiling_db: f32,
    /// 随机种子（换一个就换一套走位）。
    pub seed: u64,

    // ---- 背景音（实时生成的雨声，见 [`crate::dsp::rain`]；音量为 0 时关闭，人声不受影响）
    /// 背景音音量，0–100：0 = 关闭；1–100 对应雨声比人声的目标响度低 40 … 0 dB（见 [`Params::bg_db`]）。
    pub bg_level: f32,
    /// 雨量，0–100：毛毛雨（0.3 mm/h）… 暴雨（60 mm/h），对数刻度。雨越大，大雨滴越多、越密、越低沉。
    pub rain: f32,
    /// 雨从哪边来：−100 = 都在左边，0 = 四周均匀，100 = 都在右边。
    pub bg_pan: f32,
    /// 包围感，0–100：0 = 只从一个方向来，100 = 从四面八方来。
    pub bg_width: f32,
    /// 流动，0–100：雨声的重心绕着头慢慢转（8D 环绕）；0 = 不动，50 = 和参考的 8D 雨声一样，100 = 更明显。
    pub bg_flow: f32,
    /// 隔窗，0–100：0 = 站在雨里，100 = 关着窗在屋里听（高频被墙和玻璃挡掉）。
    pub bg_indoor: f32,
    /// 落地声，0–100：身边地上雨点"噗"的一声有多明显；0 = 没有，60 ≈ 参考的 8D 雨声，每格 0.2 dB。
    /// 屋檐滴水的响度也跟着它。（字段名沿用第一版的"近处雨滴"，老的配置文件照样能读。）
    pub rain_near: f32,
    /// 屋檐滴水，0–100：0 = 没有；越大滴得越快、滴水的地方越多（最多 3 处）。
    pub rain_drip: f32,
    /// 雷声，0–100：0 = 没有（默认）；越大雷越近、越频繁、越响（1 ≈ 4 分钟一次、8 km 外，100 ≈ 25 秒、1.5 km）。
    pub bg_thunder: f32,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            ear_dist: EAR_R,
            isolation_db: 32.0,
            presence_db: 3.0,
            ku100_eq: false,
            dark_db: 0.0,
            warmth_db: 6.0,
            ear_warmth_db: 5.0,
            far_lf_cut_db: 12.0,
            breath_db: 6.0,
            breath_reach: 0.0,
            puff: 1.0,
            breath_deboom_db: 0.0,
            breath_tone: 0.0,
            concha_db: 4.0,
            mouth_cm: 2.0,
            far_shadow_db: 5.0,
            soft_near: 0.5,
            pinna: "KU100".to_string(),
            inear_cm: 0.0,
            inear_period: 4.0,
            inear_random: 0.7,
            canal_db: 10.0,
            rumble: 1.0,
            muffle_db: 6.0,
            mode: MotionMode::Sweep,
            hold_s: 2.5,
            move_s: 2.5,
            randomness: 0.1,
            approach: 0.25,
            front: 0.08,
            via: Via::Front,
            ear_angle: 100.0,
            pass_dist: 0.2,
            orbit_period: 12.0,
            orbit_radius: 0.35,
            static_az: 270.0,
            level_follow: 0.2,
            manual_az: 270.0,
            manual_dist: EAR_R,
            manual_tilt: 0.0,
            sway: 1.0,
            sway_period: 4.0,
            sway_random: 1.0,
            sway_follow: 1.0,
            sway_level: 0.45,
            pause_motion: 0.13,
            breath_pause_switch: false,
            micro: 1.0,
            height: 0.0,
            updown: 25.0,
            updown_period: 9.0,
            updown_random: 0.6,
            over_top: 0.0,
            room: false,
            wet_db: -3.0,
            static_ambience: false,
            playback: Playback::Diffuse,
            denoise: false,
            denoise_db: DENOISE_MAX_DB,
            deess_db: 0.0,
            sharp_soften_db: 0.0,
            soft_voice: 0.0,
            auto_level: true,
            target_lufs: -16.0,
            out_gain_db: 0.0,
            ceiling_db: -1.0,
            seed: 1,
            bg_level: 0.0,
            rain: 35.0,
            bg_pan: 0.0,
            bg_width: 100.0,
            bg_flow: 50.0,
            bg_indoor: 0.0,
            rain_near: 30.0,
            rain_drip: 0.0,
            bg_thunder: 0.0,
        }
    }
}

impl Params {
    /// 把"风格"管的那些参数恢复成默认值（= 贴耳加强）：贴耳感、入耳、左右移动的方式和节奏、忽远忽近、上下、
    /// 房间与回放、去齿音 / 轻声化、目标响度。其余的——降噪、耳廓、自动响度开关、输出增益、峰值上限、
    /// 随机种子、环绕 / 固定 / 手动模式的位置——保持不变。各风格都先做这一步，所以按哪个按钮结果都是确定的，
    /// 从"掏耳"切回人声的风格时不会留下"伸进耳道、每只耳朵停 40 秒"之类的设置。
    fn style_base(mut self) -> Self {
        let d = Params::default();
        (self.ear_dist, self.isolation_db, self.presence_db, self.ku100_eq, self.dark_db) = (d.ear_dist, d.isolation_db, d.presence_db, d.ku100_eq, d.dark_db);
        (self.warmth_db, self.ear_warmth_db, self.far_lf_cut_db, self.breath_db, self.puff) = (d.warmth_db, d.ear_warmth_db, d.far_lf_cut_db, d.breath_db, d.puff);
        self.breath_reach = d.breath_reach;
        self.breath_pause_switch = d.breath_pause_switch;
        self.breath_deboom_db = d.breath_deboom_db;
        self.breath_tone = d.breath_tone;
        (self.concha_db, self.mouth_cm, self.far_shadow_db, self.soft_near) = (d.concha_db, d.mouth_cm, d.far_shadow_db, d.soft_near);
        (self.inear_cm, self.inear_period, self.inear_random, self.canal_db, self.rumble, self.muffle_db) = (d.inear_cm, d.inear_period, d.inear_random, d.canal_db, d.rumble, d.muffle_db);
        (self.mode, self.hold_s, self.move_s, self.randomness, self.approach, self.front) = (d.mode, d.hold_s, d.move_s, d.randomness, d.approach, d.front);
        (self.via, self.ear_angle, self.pass_dist, self.level_follow) = (d.via, d.ear_angle, d.pass_dist, d.level_follow);
        (self.sway, self.sway_period, self.sway_random, self.sway_follow, self.sway_level, self.pause_motion, self.micro) = (d.sway, d.sway_period, d.sway_random, d.sway_follow, d.sway_level, d.pause_motion, d.micro);
        (self.height, self.updown, self.updown_period, self.updown_random, self.over_top) = (d.height, d.updown, d.updown_period, d.updown_random, d.over_top);
        (self.room, self.wet_db, self.playback) = (d.room, d.wet_db, d.playback);
        self.static_ambience = d.static_ambience;
        (self.deess_db, self.soft_voice, self.target_lufs) = (d.deess_db, d.soft_voice, d.target_lufs);
        self.sharp_soften_db = d.sharp_soften_db;
        self
    }

    /// 贴耳加强（默认）：嘴离耳约 1.7 cm，远耳额外压低 32 dB，不加房间，左右耳慢慢穿梭。
    pub fn style_ear(self) -> Self {
        self.style_base()
    }

    /// KU100 实录：用原机音色和实测房间，保留远耳。近处各方向都带出轻微气息，
    /// 低频气流集中在耳侧。距离、走位与人声参数的整轨对照见 doc/effects.md。
    pub fn style_ku100(self) -> Self {
        let mut p = self.style_base();
        p.ear_dist = 0.115;
        p.isolation_db = 0.0;
        p.far_lf_cut_db = 0.0;
        p.breath_db = 12.0;
        p.breath_reach = 0.9;
        p.presence_db = 4.0;
        p.warmth_db = 5.0;
        p.ear_warmth_db = 4.0;
        p.dark_db = 2.0;
        p.deess_db = 4.0;
        p.sharp_soften_db = 4.0;
        p.room = true;
        p.playback = Playback::Native;
        p.mode = MotionMode::Random;
        p.level_follow = 0.2;
        p.front = 0.36;
        p.hold_s = 3.5;
        p.move_s = 2.5;
        p.randomness = 0.5;
        p.sway = 0.85;
        p.sway_follow = 0.85;
        p
    }

    /// 呼吸音：侧耳长停，保留原录音的换气和静默。气流脉冲取自呼吸本身的包络。
    pub fn style_breath(self) -> Self {
        let mut p = self.style_base();
        p.playback = Playback::Native;
        p.room = true;
        p.ear_dist = 0.15;
        p.ear_angle = 80.0;
        p.isolation_db = 0.0;
        p.far_lf_cut_db = 0.0;
        p.breath_db = 0.0;
        p.puff = 0.25;
        p.warmth_db = 0.0;
        p.ear_warmth_db = 0.0;
        p.breath_deboom_db = 6.0;
        p.breath_tone = 0.85;
        p.target_lufs = BREATH_LUFS;
        p.ku100_eq = true;
        p.presence_db = 0.0;
        p.concha_db = 0.0;
        p.dark_db = 5.0;
        p.breath_pause_switch = true;
        p.hold_s = 5.0;
        p.move_s = 0.8;
        p.pause_motion = 1.0;
        p.approach = 0.0;
        p.sway = 0.0;
        p.sway_follow = 0.0;
        p.micro = 0.15;
        p.updown = 0.0;
        p
    }

    /// 掏耳：按两段 KU100 掏耳实录标定。给棉签 / 挖耳勺 / 刮擦这类"触发音"用，不是给人声用的——
    /// 声源贴在耳朵上并伸进耳道进进出出（耳道共鸣跟着在约 3–11 kHz 之间滑动），带耳内低频和堵耳的闷感；
    /// 另一只耳朵基本听不到；每只耳朵停留约 40 秒才换边，换边时先退出来。人声用的几项（气声、气流、轻声、
    /// 忽远忽近、耳甲共鸣、临场感、嘴的大小）都关掉。
    pub fn style_earclean(self) -> Self {
        let mut p = self.style_base();
        p.ear_dist = R_MIN;
        p.isolation_db = 40.0;
        p.playback = Playback::Native;
        p.presence_db = 0.0;
        p.breath_db = 0.0;
        p.puff = 0.0;
        p.concha_db = 0.0;
        p.mouth_cm = 0.0;
        p.soft_near = 0.0;
        p.inear_cm = 2.1;
        p.hold_s = 40.0;
        p.move_s = 2.0;
        p.randomness = 0.2;
        p.approach = 0.0;
        p.ear_angle = 90.0;
        p.level_follow = 0.5;
        p.sway = 0.0;
        p.sway_follow = 0.0;
        p.micro = 0.3;
        p.updown = 10.0;
        p.target_lufs = EARCLEAN_LIVE_LUFS;
        p
    }

    /// 风格按钮的名字（和安卓版、插件的 action 同名），按桌面版按钮的顺序。
    pub const STYLES: [&'static str; 4] = ["style_ear", "style_ku100", "style_breath", "style_earclean"];

    /// 按名字套用风格（名字见 [`Params::STYLES`]）；不认识的名字返回 None。
    pub fn with_style(self, name: &str) -> Option<Self> {
        Some(match name {
            "style_ear" => self.style_ear(),
            "style_ku100" => self.style_ku100(),
            "style_breath" => self.style_breath(),
            "style_earclean" => self.style_earclean(),
            _ => return None,
        })
    }

    /// 现在选中的风格：再按一次这个风格按钮参数一点不变，就算选中了它（界面上把它标出来）。
    /// 风格管的参数手动调过、和哪个风格都对不上时为 None；风格不管的设置（降噪、耳廓、输出、位置等）不影响。
    /// 恢复默认后是贴耳加强（默认参数就是贴耳加强）。
    pub fn active_style(&self) -> Option<&'static str> {
        Self::STYLES.into_iter().find(|n| self.clone().with_style(n).as_ref() == Some(self))
    }

    /// "雨夜"：四面八方的小到中雨（约 2 mm/h），重心绕着头慢慢转（8D 环绕）；身边地上的落地声比参考的 8D 雨声
    /// 轻约 6 dB；不隔窗、没有屋檐滴水、没有雷（雷声是可选的，要的话再拉「雷声」）；雨声比人声低约 18 dB。只改背景音这几项。
    pub fn bg_rain_night(mut self) -> Self {
        let d = Params::default();
        (self.rain, self.bg_pan, self.bg_width, self.bg_flow) = (d.rain, d.bg_pan, d.bg_width, d.bg_flow);
        (self.bg_indoor, self.rain_near, self.rain_drip, self.bg_thunder) = (d.bg_indoor, d.rain_near, d.rain_drip, d.bg_thunder);
        self.bg_level = 55.0;
        self
    }

    /// 背景音比人声的目标响度低多少 dB（音量 1 → −40 dB，100 → 0 dB）；音量为 0（关闭）时 None。
    pub fn bg_db(&self) -> Option<f32> {
        (self.bg_level > 0.0).then(|| -40.0 + 0.4 * self.bg_level.min(100.0))
    }

    /// 背景音的几项是否和 `o` 相同。
    pub fn bg_same(&self, o: &Params) -> bool {
        (self.bg_level, self.rain, self.bg_pan, self.bg_width, self.bg_flow, self.bg_indoor, self.rain_near, self.rain_drip, self.bg_thunder)
            == (o.bg_level, o.rain, o.bg_pan, o.bg_width, o.bg_flow, o.bg_indoor, o.rain_near, o.rain_drip, o.bg_thunder)
    }

    /// 降噪强度换成"降噪后的声音占多少"：输出 = 原声 × (1 − m) + 降噪后 × m。
    /// 降噪器把某个成分压到再低，输出里它也至少还剩原来的 1 − m = 10^(−dB/20)，
    /// 所以底噪最多被压低 `denoise_db` 分贝；人声（降噪器原样放过的部分）不受影响。
    pub fn denoise_mix(&self) -> f32 {
        if self.denoise_db >= DENOISE_MAX_DB {
            1.0
        } else {
            1.0 - db_to_lin(-self.denoise_db.max(0.0))
        }
    }

    // ---- 界面上的换算（桌面版和安卓版的界面都用这几个，保证两边操作一致）

    /// 「贴耳距离」：嘴离耳朵（头表面）多少厘米。
    pub fn ear_cm(&self) -> f32 {
        (self.ear_dist - HEAD_A) * 100.0
    }

    pub fn set_ear_cm(&mut self, cm: f32) {
        self.ear_dist = (HEAD_A + cm / 100.0).clamp(R_MIN, R_MAX);
    }

    /// 手动摆位的「离头距离」：声源离头表面多少厘米（在正侧方时就是离耳朵多远）。
    pub fn manual_cm(&self) -> f32 {
        (self.manual_dist - HEAD_A) * 100.0
    }

    pub fn set_manual_cm(&mut self, cm: f32) {
        self.manual_dist = (HEAD_A + cm / 100.0).clamp(R_MIN, R_MAX);
    }

    /// 「移动频率」：说话时每分钟换几次耳朵。
    pub fn moves_per_min(&self) -> f32 {
        60.0 / (self.hold_s + self.move_s).max(0.2)
    }

    /// 改移动频率：停留时间和移动时间按同一比例缩放。
    pub fn set_moves_per_min(&mut self, freq: f32) {
        if freq > 0.0 && freq.is_finite() {
            let k = self.moves_per_min() / freq;
            self.hold_s = (self.hold_s * k).max(0.001);
            self.move_s = (self.move_s * k).max(0.001);
        }
    }

    /// 换左右移动的方式：「响度随位置变化」跟着换成这个方式的默认值。
    pub fn set_mode(&mut self, mode: MotionMode) {
        if mode != self.mode {
            self.mode = mode;
            self.level_follow = Self::default_level_follow(mode);
        }
    }

    /// 降噪滑条（0–100）：0 = 关闭，其余 = 底噪最多压低多少 dB，100 = 不设上限。
    pub fn denoise_slider(&self) -> f32 {
        if self.denoise {
            self.denoise_db.clamp(0.0, DENOISE_MAX_DB)
        } else {
            0.0
        }
    }

    pub fn set_denoise_slider(&mut self, v: f32) {
        self.denoise = v >= 0.5;
        if self.denoise {
            self.denoise_db = v.round();
        }
    }

    /// 各运动模式默认的"响度随位置变化"程度（与 Python 版相同）。
    pub fn default_level_follow(mode: MotionMode) -> f32 {
        match mode {
            MotionMode::Sweep => 0.2,
            MotionMode::Random => 0.5,
            MotionMode::Orbit => 0.3,
            MotionMode::Static => 1.0,
            MotionMode::Manual => 0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_are_complete_and_leave_other_settings_alone() {
        let d = Params::default();
        // 贴耳加强就是默认值；从掏耳、KU100 实录切回来不留下任何痕迹
        assert_eq!(d.clone().style_ear(), d);
        assert_eq!(d.clone().style_earclean().style_ear(), d);
        assert_eq!(d.clone().style_ku100().style_ear(), d);
        let breath = d.clone().style_breath();
        assert_eq!(d.clone().style_earclean().style_breath(), breath);
        assert_eq!(breath.clone().style_ear(), d);
        assert_eq!(breath.style_ku100(), d.clone().style_ku100());
        // 先按哪个按钮都一样
        assert_eq!(d.clone().style_earclean().style_ku100(), d.clone().style_ku100());
        assert_eq!(d.clone().style_ku100().style_earclean(), d.clone().style_earclean());
        // 默认和 KU100 实录不进耳道；掏耳进耳道、每只耳朵停得久、人声用的几项关掉
        assert_eq!(d.inear_cm, 0.0);
        assert_eq!(d.clone().style_ku100().inear_cm, 0.0);
        assert!(!d.clone().style_ku100().static_ambience);
        let e = d.clone().style_earclean();
        assert!(e.inear_cm > 1.0 && e.hold_s >= 20.0 && e.isolation_db >= 40.0);
        assert!(e.breath_db == 0.0 && e.puff == 0.0 && e.sway == 0.0 && e.sway_follow == 0.0 && e.soft_near == 0.0);
        assert!(!e.room && e.target_lufs == EARCLEAN_LIVE_LUFS && EARCLEAN_LUFS < d.target_lufs);
        // 风格不管的设置保持不变：降噪、耳廓、输出、随机种子、各模式的位置
        let mine = Params { denoise: true, denoise_db: 12.0, pinna: "B".to_string(), out_gain_db: -3.0, ceiling_db: -2.0, auto_level: false, seed: 9, static_az: 45.0, orbit_radius: 0.5, manual_az: 10.0, ..d.clone() };
        for q in [mine.clone().style_ear(), mine.clone().style_ku100(), mine.clone().style_breath(), mine.clone().style_earclean()] {
            assert!(q.denoise && q.denoise_db == 12.0 && q.pinna == "B");
            assert!(q.out_gain_db == -3.0 && q.ceiling_db == -2.0 && !q.auto_level && q.seed == 9);
            assert!(q.static_az == 45.0 && q.orbit_radius == 0.5 && q.manual_az == 10.0);
        }
    }

    #[test]
    fn active_style_follows_the_last_style_button() {
        let d = Params::default();
        assert_eq!(d.active_style(), Some("style_ear"));
        // 风格不管的设置改了也还算选中
        let mine = Params { denoise: true, denoise_db: 12.0, pinna: "B".to_string(), out_gain_db: -3.0, seed: 9, static_az: 45.0, bg_level: 55.0, ..d.clone() };
        for name in Params::STYLES {
            let q = mine.clone().with_style(name).unwrap();
            assert_eq!(q.active_style(), Some(name), "{name}");
        }
        // 手动调了风格管的参数：谁都不是
        let q = Params { ear_dist: d.ear_dist + 0.01, ..d.clone().style_ku100() };
        assert_eq!(q.active_style(), None);
        assert!(d.clone().with_style("style_x").is_none());
    }

    #[test]
    fn old_config_files_load_with_the_new_fields_at_their_defaults() {
        // 旧配置文件里没有入耳和正面比例这几项：读进来是默认值（不进耳道）
        let p: Params = ron::from_str("(ear_dist:0.15,isolation_db:0.0,room:true)").unwrap();
        assert_eq!(p.inear_cm, 0.0);
        assert_eq!(p.breath_reach, 0.0);
        assert!(!p.static_ambience);
        assert_eq!(p.sharp_soften_db, 0.0);
        assert_eq!((p.front, p.canal_db, p.rumble, p.muffle_db), (0.08, 10.0, 1.0, 6.0));
        assert!(p.room && p.ear_dist == 0.15);
        let e = Params::default().style_earclean();
        assert_eq!(ron::from_str::<Params>(&ron::to_string(&e).unwrap()).unwrap(), e);
    }
    #[test]
    fn ui_conversions_round_trip() {
        let mut p = Params::default();
        assert!((p.ear_cm() - 1.75).abs() < 1e-4);
        p.set_ear_cm(4.0);
        assert!((p.ear_cm() - 4.0).abs() < 1e-4);
        p.set_ear_cm(-50.0);
        assert_eq!(p.ear_dist, R_MIN);
        p.set_manual_cm(10.0);
        assert!((p.manual_cm() - 10.0).abs() < 1e-4);
        // 移动频率：停留和移动按同一比例缩放
        let ratio = p.hold_s / p.move_s;
        p.set_moves_per_min(24.0);
        assert!((p.moves_per_min() - 24.0).abs() < 1e-3 && (p.hold_s / p.move_s - ratio).abs() < 1e-4);
        // 换方式时响度随位置变化跟着换；同一个方式不动
        p.level_follow = 0.9;
        p.set_mode(MotionMode::Sweep);
        assert_eq!(p.level_follow, 0.9);
        p.set_mode(MotionMode::Static);
        assert_eq!(p.level_follow, 1.0);
        // 降噪滑条：0 关，其余是分贝数
        p.set_denoise_slider(0.2);
        assert!(!p.denoise && p.denoise_slider() == 0.0);
        p.set_denoise_slider(12.4);
        assert!(p.denoise && p.denoise_db == 12.0 && p.denoise_slider() == 12.0);
    }
}
