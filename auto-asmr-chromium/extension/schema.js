// 设置页的参数表：分组、名字、范围、单位与桌面版界面（auto-asmr/src/gui.rs）一致。
// 滑块用常用范围，数字框里可以手输超出范围的值（同桌面版）。
// `derived: true` 的项是界面上的换算值（贴耳距离、移动频率……），读写都经过 Params 自己的方法（见 wasm/src/lib.rs）。
// `doc: true` 的项是配置文件里 `params` 之外、和桌面版共用的设置（左右双人：延迟是文件转换页的 `duo_ms`）。
// `show` / `on` 收到 (params, 整个界面状态)，要看 doc 里的值时用第二个参数。

const mode = (...ms) => (p) => ms.includes(p.mode);

const offOr = (x, f) => (x < 0.5 ? '0 关' : f(x));

const duoOn = (p, s) => s.doc.duo_ms > 0;

export const GROUPS = [
  {
    title: '左右双人',
    rows: [
      {
        key: 'duo_ms',
        doc: true,
        label: '左右双人',
        min: 0,
        max: 300,
        dec: 0,
        fmt: (x) => offOr(x, () => 'ms'),
        tip: '把网页里的声音当成两个人：同一个声音一个在左耳、一个在右耳（各自忽远忽近），第 2 个整体晚这么多毫秒，听得出是两个人一前一后地说。30 ms 以下两边容易融成一个声音，50–120 ms 像两个人，150 ms 以上像回声。只有第 2 个声音晚，第 1 个和平时一样及时。和桌面版「文件转换」页的左右双人是同一个设置。0 = 关闭',
      },
      {
        type: 'choice',
        key: 'mix_follow',
        doc: true,
        label: '排布',
        show: duoOn,
        tip: '各自固定：1 号固定在左耳、2 号固定在右耳，只做忽远忽近、上下、自然晃动，「左右移动」不起作用。一起移动：两个声音按「左右移动」的方式一起走，始终在头的两侧相对',
        options: [
          [false, '各自固定'],
          [true, '一起移动'],
        ],
      },
      {
        key: 'mix_balance',
        doc: true,
        label: '自动平衡',
        min: 0,
        max: 100,
        dec: 0,
        show: duoOn,
        fmt: (x) => offOr(x, () => '%'),
        tip: '让两个声音每一段时间听起来都差不多一样响（各自忽远忽近时，常常一个退开了另一个正贴着耳朵）。最多 ±15 dB，0.25 秒平滑；停顿里不往上拉；网页没有「整段」，响度边听边算。100% = 一样响，50% = 差距减半，0 = 关闭',
      },
    ],
  },
  {
    title: '贴耳感',
    rows: [
      { key: 'ear_cm', derived: true, label: '贴耳距离', min: 0.8, max: 25, log: true, unit: 'cm', dec: 1, tip: '嘴离耳朵多远。1.7 cm = 贴着耳朵' },
      { key: 'isolation_db', label: '远耳压低', min: 0, max: 40, unit: 'dB', dec: 0, tip: '贴耳时另一只耳朵额外压低多少（0 = 纯物理，像真实 KU100）' },
      { key: 'presence_db', label: '临场感', min: 0, max: 8, unit: 'dB', dec: 1, tip: '贴耳时近耳 3.5 kHz 和空气感' },
      { key: 'dark_db', label: '暗度', min: 0, max: 8, unit: 'dB', dec: 1, tip: '贴耳时近耳 5 kHz 附近压低，音色更暗、更柔' },
      { key: 'warmth_db', label: '温暖感', min: 0, max: 12, unit: 'dB', dec: 1, tip: '凑近时两耳共同的低频' },
      { key: 'ear_warmth_db', label: '近耳低频', min: 0, max: 10, unit: 'dB', dec: 1, tip: '贴耳时只给近耳加的低频' },
      { key: 'far_lf_cut_db', label: '远耳低频削减', min: 0, max: 18, unit: 'dB', dec: 0, tip: '贴耳时远耳 320 Hz 以下再压低' },
      { key: 'breath_db', label: '气声增强', min: 0, max: 12, unit: 'dB', dec: 1, tip: '原声中轻微送气和呼吸声的最大增益，响亮的齿音不会得到额外增强。0 = 关闭' },
      { key: 'breath_deboom_db', label: '呼吸去闷', min: 0, max: 12, unit: 'dB', dec: 1, tip: '呼吸在 100–300 Hz 的能量过强时，最多压低多少分贝。0 = 关闭' },
      { key: 'breath_tone', label: '薄声补偿', min: 0, max: 1, dec: 2, tip: '呼吸低频偏少时补气流，3 kHz 以上过亮时柔化。0 = 关闭。' },
      { key: 'breath_reach', label: '近处气声范围', min: 0, max: 1, dec: 2, tip: '在身前和耳侧近处保留多少比例的气声增强。0 = 仅贴耳，1 = 近处可用全部增益。声源离头中心 20 cm 内最强，渐弱至 65 cm 消失。低频气流仍只在贴耳时生效' },
      { key: 'puff', label: '气流扑耳', min: 0, max: 8, unit: '倍', dec: 2, tip: '送气时气流扑到耳朵上的低频脉冲（耳朵痒的感觉）。0 = 关闭，1 倍 = 标准' },
      { key: 'concha_db', label: '耳甲共鸣', min: 0, max: 8, unit: 'dB', dec: 1, tip: '贴耳时近耳 4.3 kHz 的耳甲腔共振。0 = 关闭' },
      { key: 'soft_near', label: '贴耳轻声', min: 0, max: 1.5, dec: 2, tip: '凑到耳边时把嗓音均衡成「放轻声音」，退开时恢复（只做均衡）。0 = 关闭' },
      { key: 'mouth_cm', label: '嘴的大小', min: 0, max: 5, unit: 'cm', dec: 1, tip: '嘴是一个开口而不是一个点：贴耳时耳廓的窄谷被平均变浅。0 = 点声源' },
      { key: 'far_shadow_db', label: '远耳高频遮挡', min: 0, max: 12, unit: 'dB', dec: 1, tip: '贴耳时另一只耳朵 6 kHz 以上再压低。0 = 关闭' },
      { type: 'check', key: 'ku100_eq', label: '近耳用 KU100 实测校正曲线（高频更收）' },
    ],
  },
  {
    title: '入耳（掏耳）',
    rows: [
      { key: 'inear_cm', label: '入耳深度', min: 0, max: 2.3, unit: 'cm', dec: 1, tip: '声源贴到耳朵上之后，最深伸进耳道多少。0 = 不进耳道，这一组都不起作用' },
      { key: 'inear_period', label: '进出节奏', min: 1.5, max: 12, unit: 's', dec: 1, on: (p) => p.inear_cm > 0, tip: '有声音时，一次「伸进去—退出来」大约几秒' },
      { key: 'inear_random', label: '随机程度', min: 0, max: 1, dec: 2, on: (p) => p.inear_cm > 0, tip: '0 = 进出的节奏和深浅固定，1 = 都随机' },
      { key: 'canal_db', label: '耳道共鸣', min: 0, max: 16, unit: 'dB', dec: 1, on: (p) => p.inear_cm > 0, tip: '伸得越深共振越高（约 3 → 11 kHz）。0 = 关闭' },
      { key: 'rumble', label: '耳内低频', min: 0, max: 3, unit: '倍', dec: 2, on: (p) => p.inear_cm > 0, tip: '摩擦 / 触碰在耳道里形成的低频压力。0 = 关闭，1 倍 = 标准' },
      { key: 'muffle_db', label: '堵耳闷感', min: 0, max: 12, unit: 'dB', dec: 1, on: (p) => p.inear_cm > 0, tip: '伸进耳道时近耳 2.5 kHz 以上压低多少。0 = 关闭' },
    ],
  },
  {
    title: '左右移动',
    rows: [
      {
        type: 'choice',
        key: 'mode',
        label: '方式',
        options: [
          ['Sweep', '穿梭'],
          ['Random', '随机走位'],
          ['Orbit', '环绕'],
          ['Static', '固定'],
          ['Manual', '手动'],
        ],
      },
      { key: 'moves_per_min', derived: true, label: '移动频率', min: 2, max: 40, unit: '次/分', dec: 1, show: mode('Sweep', 'Random'), tip: '说话时每分钟换几次耳朵（同时缩放停留和移动时间）' },
      { key: 'hold_s', label: '停留时间', min: 0.3, max: 60, log: true, unit: 's', dec: 1, show: mode('Sweep', 'Random'), tip: '每只耳朵边停留多久（掏耳时一般几十秒）' },
      { type: 'check', key: 'breath_pause_switch', label: '换气时换边', show: mode('Sweep'), tip: '在一侧停够设定时间后，等换气间隙再移动。' },
      { key: 'move_s', label: '移动时间', min: 0.3, max: 12, unit: 's', dec: 1, show: mode('Sweep', 'Random'), tip: '从一只耳朵移到另一只耳朵用多久' },
      { key: 'randomness', label: '随机程度', min: 0, max: 1, dec: 2, show: mode('Sweep', 'Random'), tip: '时长和落点的随机程度：0 = 完全规律' },
      { key: 'front', label: '正面比例', min: 0, max: 0.7, dec: 2, show: mode('Random'), tip: '每次停下来，有多大比例停在正前方（两只耳朵听到的差不多）' },
      { key: 'approach', label: '从远处凑近', min: 0, max: 1, dec: 2, show: mode('Sweep'), tip: '换边时先退到约 0.5 m 再慢慢凑近的概率' },
      { key: 'ear_angle', label: '贴耳角度', min: 60, max: 140, unit: '°', dec: 0, show: mode('Sweep'), tip: '90 = 正侧方，100 = 略靠后' },
      { key: 'pass_dist', label: '穿过时的距离', min: 0.12, max: 0.6, unit: 'm', dec: 2, show: mode('Sweep'), tip: '经过脸前 / 脑后时离头中心多远' },
      {
        type: 'choice',
        key: 'via',
        label: '换边时从哪里绕',
        show: mode('Sweep'),
        options: [
          ['Front', '脸前'],
          ['Back', '脑后'],
          ['Alternate', '交替'],
          ['Random', '随机'],
        ],
      },
      { key: 'orbit_period', label: '一圈用时', min: 3, max: 60, unit: 's', dec: 0, show: mode('Orbit') },
      { key: 'orbit_radius', label: '半径', min: 0.1, max: 1.5, log: true, unit: 'm', dec: 2, show: mode('Orbit') },
      { key: 'static_az', label: '方位角', min: 0, max: 359, unit: '°', dec: 0, show: mode('Static'), tip: '0 前，90 右，180 后，270 左' },
      { key: 'manual_az', label: '方位角', min: 0, max: 359.9, unit: '°', dec: 0, show: mode('Manual'), tip: '0 前，90 右，180 后，270 左' },
      { key: 'manual_cm', derived: true, label: '离头距离', min: 0.8, max: 71, log: true, unit: 'cm', dec: 1, show: mode('Manual'), tip: '声源离头表面多远；在耳朵正侧方时就是离耳朵多远' },
      { key: 'manual_tilt', label: '高度', min: -80, max: 85, unit: '°', dec: 0, show: mode('Manual'), tip: '以耳朵为支点向上（正）/ 向下（负）偏多少度' },
      { key: 'level_follow', label: '响度随位置变化', min: 0, max: 1, dec: 2, tip: '1 = 物理真实（贴耳很响、到脸前会弱很多），0 = 处处一样响' },
    ],
  },
  {
    title: '背景音（雨声）',
    rows: [
      {
        type: 'buttons',
        items: [
          ['雨夜', 'bg_preset', 'rain_night', '四面八方的小到中雨，重心绕着头慢慢转（8D 环绕），身边地上的落地声比参考的 8D 雨声轻一点，雨声比人声低约 18 dB。只改背景音这几项'],
          ['关', 'bg_preset', 'off', '关掉背景音'],
        ],
        note: '实时生成，不是录音；人声不受影响',
      },
      { key: 'bg_level', label: '音量', min: 0, max: 100, dec: 0, fmt: (x) => offOr(x, (v) => `比人声低 ${(40 - 0.4 * v).toFixed(0)} dB`), tip: '雨声比人声的目标响度低多少：1 = 低 40 dB，100 = 一样响；0 = 关闭' },
      { key: 'rain', label: '雨量', min: 0, max: 100, dec: 0, fmt: (x) => `${(0.3 * Math.pow(200, x / 100)).toFixed(1)} mm/h`, on: (p) => p.bg_level > 0, tip: '毛毛雨（0.3 mm/h）… 暴雨（60 mm/h）' },
      { key: 'bg_pan', label: '左右', min: -100, max: 100, dec: 0, fmt: (x) => (Math.abs(x) < 0.5 ? '中' : x < 0 ? `左 ${(-x).toFixed(0)}` : `右 ${x.toFixed(0)}`), on: (p) => p.bg_level > 0, tip: '雨从哪边来：−100 = 都在左边，0 = 四周均匀，100 = 都在右边' },
      { key: 'bg_width', label: '包围感', min: 0, max: 100, dec: 0, on: (p) => p.bg_level > 0, tip: '0 = 只从一个方向来，100 = 从四面八方来' },
      { key: 'bg_flow', label: '流动', min: 0, max: 100, dec: 0, fmt: (x) => (x < 0.5 ? '0 不动' : x.toFixed(0)), on: (p) => p.bg_level > 0, tip: '雨声的重心绕着头慢慢转（8D 环绕，平均 26 秒一圈）。0 = 不动，50 = 和参考的 8D 雨声一样，100 = 更明显' },
      { key: 'bg_indoor', label: '隔窗', min: 0, max: 100, dec: 0, on: (p) => p.bg_level > 0, tip: '0 = 站在雨里，100 = 关着窗在屋里听' },
      { key: 'rain_near', label: '落地声', min: 0, max: 100, dec: 0, fmt: (x) => (x < 0.5 ? '0 无' : `${x.toFixed(0)} · ${0.2 * (x - 60) >= 0 ? '+' : '−'}${Math.abs(0.2 * (x - 60)).toFixed(0)} dB`), on: (p) => p.bg_level > 0, tip: '身边地上一个个雨点「噗」的一声有多明显：0 = 没有，60 ≈ 参考的 8D 雨声，每格 0.2 dB。屋檐滴水的响度也跟着它' },
      { key: 'rain_drip', label: '屋檐滴水', min: 0, max: 100, dec: 0, on: (p) => p.bg_level > 0, tip: '屋檐往石阶 / 积水的地上滴的「嗒」，1–3 处。0 = 没有。多响跟着「落地声」' },
      { key: 'bg_thunder', label: '雷声', min: 0, max: 100, dec: 0, fmt: (x) => { if (x < 0.5) return '0 无'; const i = 240 * Math.pow(25 / 240, x / 100), d = 8 * Math.pow(1.5 / 8, x / 100); return i >= 90 ? `${x.toFixed(0)} · ${(i / 60).toFixed(0)} 分钟 · ${d.toFixed(1)} km` : `${x.toFixed(0)} · ${i.toFixed(0)} s · ${d.toFixed(1)} km`; }, on: (p) => p.bg_level > 0, tip: '远处的雷（可选，默认没有）。0 = 没有；越大越近、越频繁、越响：30 ≈ 两分钟一次、5 km 外，100 ≈ 25 秒一次、1.5 km' },
    ],
  },
  {
    title: '忽远忽近',
    rows: [
      { key: 'sway', label: '幅度', min: 0, max: 2, dec: 2, tip: '随机地凑近、往后让的幅度（0 = 关闭）' },
      { key: 'sway_period', label: '节奏', min: 2, max: 14, unit: 's', dec: 1, tip: '说话时一次凑近—退开大约几秒' },
      { key: 'sway_random', label: '随机程度', min: 0, max: 1, dec: 2, tip: '0 = 固定节奏和深浅，1 = 时长和深浅都随机' },
      { key: 'sway_follow', label: '跟随音量', min: 0, max: 2, dec: 2, tip: '声音大时往后让、轻声时更贴近' },
      { key: 'sway_level', label: '退开时的音量', min: 0, max: 1, dec: 2, tip: '退开时耳边音量保留物理衰减的比例' },
      { key: 'pause_motion', label: '停顿时的动作', min: 0, max: 1, dec: 2, tip: '停顿时动作的速度：0.13 = 几乎不动，1 = 一直在动' },
      { key: 'micro', label: '自然晃动', min: 0, max: 2, dec: 2, tip: '人体自然的微小晃动（1 约 1.2 cm）' },
    ],
  },
  {
    title: '上下',
    rows: [
      { key: 'height', label: '高度', min: -60, max: 75, unit: '°', dec: 0, tip: '嘴相对耳朵向上（正）/ 向下（负）偏多少度；0 = 与耳朵齐平' },
      { key: 'updown', label: '自动上下', min: 0, max: 60, unit: '°', dec: 0, tip: '嘴在耳朵上方 / 下方之间慢慢换位置的幅度（0 = 关闭）' },
      { key: 'updown_period', label: '节奏', min: 3, max: 30, unit: 's', dec: 1, tip: '说话时上下一个来回大约几秒' },
      { key: 'updown_random', label: '随机程度', min: 0, max: 1, dec: 2, tip: '0 = 节奏和高低固定，1 = 时长和高低都随机' },
      { key: 'over_top', label: '从头顶绕', min: 0, max: 1, dec: 2, tip: '换耳时改成从头顶绕过去的概率' },
    ],
  },
  {
    title: 'KU100 临场感',
    rows: [
      { type: 'check', key: 'static_ambience', label: '固定原声底噪', tip: '让原录音里的稳定底噪留在固定空间，人声和呼吸继续随声源移动。合成雨声单独调节。' },
      { type: 'check', key: 'room', label: '真实房间（KU100 实测，远耳听到的是房间声而不是寂静）' },
      { key: 'wet_db', label: '房间声大小', min: -12, max: 9, unit: 'dB', dec: 0, on: (p) => p.room },
      { type: 'pinna', key: 'pinna', label: '耳廓', tip: 'KU100：人工头自己的耳廓（给别人听选这个最稳）；真人 A–G：自己听时挑前后、上下最分得清的那只' },
      { type: 'playback', key: 'playback', label: '回放补偿', tip: '扩散场：通用；KU100 原机：和真实 KU100 录音一样；或按你的耳机型号补偿' },
    ],
  },
  {
    title: '人声处理',
    rows: [
      { key: 'denoise', derived: true, label: '降噪', min: 0, max: 100, dec: 0, fmt: (x) => (x < 0.5 ? '0 关' : x >= 99.5 ? '100 最大' : `${x.toFixed(0)} dB`), tip: '输入降噪（RNNoise）：数值 = 底噪最多压低多少分贝，0 = 关闭，100 = 不设上限。开启后多 20 ms 延时' },
      { key: 'deess_db', label: '去齿音', min: 0, max: 10, unit: 'dB', dec: 1, tip: '只在 s / sh / x 出现时压低 4.5 kHz 以上，最多压这么多' },
      { key: 'sharp_soften_db', label: '强辅音柔化', min: 0, max: 8, unit: 'dB', dec: 1, tip: '强辅音突出时，最多压低 5 kHz 附近窄频带这么多，保留轻微呼吸的细节。0 = 关闭' },
      { key: 'soft_voice', label: '轻声化', min: 0, max: 1.5, dec: 2, tip: '把嗓音均衡成「压低声音说话」（只做均衡）' },
    ],
  },
  {
    title: '输出',
    rows: [
      { type: 'check', key: 'auto_level', label: '自动响度（慢慢把音量拉到目标）' },
      { key: 'target_lufs', label: '目标响度', min: -30, max: -10, unit: 'LUFS', dec: 0, on: (p) => p.auto_level },
      { key: 'out_gain_db', label: '输出增益', min: -60, max: 12, unit: 'dB', dec: 1, tip: '在自动响度之后、峰值限制器之前整体加减音量，相当于耳机音量旋钮。只在实时播放时起作用（电脑版、安卓版转换文件时不用它，文件的响度由目标响度决定）。往上推到峰值上限后只会被限制器压扁，不会更响' },
      { key: 'ceiling_db', label: '峰值上限', min: -6, max: -0.1, unit: 'dBTP', dec: 1, tip: '限制器的真峰值上限' },
      { type: 'int', key: 'seed', label: '随机种子', tip: '换一个数字就换一套走位（下次开始处理时生效）' },
    ],
  },
];
