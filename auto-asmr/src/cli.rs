//! 命令行：不带参数启动图形界面；`convert` 转换文件；`align` 看几段音频怎么对齐；`bg` 只生成背景音（雨声）；
//! `devices` 列出音频设备；`bench` 测处理速度。

use crate::dsp::engine::{Engine, EngineConfig, PostMode};
use crate::dsp::mix::{self, Voice, MAX_VOICES, PRESETS};
use crate::dsp::params::{MotionMode, Params, Playback, Via};
use crate::dsp::tables::tables;
use crate::dsp::{FS, FS_USIZE, R_MAX, R_MIN};
use crate::offline::{convert, convert_mix, default_mix_output, default_output, Channel, ConvertOptions, Duo, MixInput, MixOptions};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub const HELP: &str = "auto-asmr —— KU100 双耳 ASMR 空间化（纯 CPU）

用法：
  auto-asmr                          启动图形界面（实时 + 文件转换）
  auto-asmr convert <输入> [选项]    转换一个音频文件（wav / m4a / mp3 / flac / ogg …），输出 WAV
  auto-asmr convert <输入1> <输入2> … [选项]   批量转换：多个文件或文件夹，多线程并行
  auto-asmr convert <输入1> <输入2> --mix [选项]   多音频合成：2–4 个音频各放一个位置（默认一左一右），合成一个文件
  auto-asmr align <输入1> <输入2> … [--channel …]   只看自动对齐的结果：每段从哪里开始用、裁掉多少（不输出音频）
  auto-asmr bg -o <文件> [--duration 秒] [--lufs L] [背景音选项]   只生成背景音（雨声），按 --lufs（默认 −20）定响度
  auto-asmr devices                  列出本机的音频输入 / 输出设备
  auto-asmr bench                    测试处理速度

convert 的选项（括号里是默认值）：
  -o <文件>               输出文件（输入名_asmr.wav）；只用于单个文件
 批量（多个输入，或输入是文件夹）
  --out-dir <文件夹>      输出都放进这个文件夹（默认放在各自源文件旁边，重名自动加 _2、_3）
  -j, --jobs <n>          同时转换几个文件（默认 = CPU 逻辑核数）
 多音频合成（几个音频同时播放，各在一个位置；左右移动换成各自固定，忽远忽近 / 上下 / 晃动照常）
  --mix                   把输入的 2–4 个音频合成一个文件（输出：第一个输入名_mix_asmr.wav，或用 -o 指定）
  --pos <位置,…>          各音源的位置，按输入顺序：left / right / front / back / front-left / front-right /
                          back-left / back-right（也可写 左耳 / 右耳 / 正前 / 正后 / 左前 / 右前 / 左后 / 右后），
                          或“方位角[:到头中心的距离 米]”，如 270、45:0.3（0 前 90 右 180 后 270 左）。默认 left,right,front,back
  --mix-gain <dB,…>       各音源的音量增减，在自动对齐响度之后再加（默认都是 0），如 0,-3
  --mix-align             自动对齐：同一首歌的几个版本（前奏长短不同、编曲不同都行），自动找到一起开始唱的地方，
                          从那里叠加，对不上的开头和结尾裁掉；--start / --duration 按对齐之后的时间算
  --mix-balance [0–100]   自动平衡（关）：让几个声音每一段时间都差不多一样响（比如一左一右时左右差不多大），
                          按各自此刻听起来多响（空间化之后）补，最多 ±15 dB；强度越大，忽远忽近的响度起伏越小；
                          数值 = 强度 %，100 = 一样响，50 = 差距减半；不写数值 = 100
  --mix-follow            一起移动：几个音源按 --mode 的方式（穿梭 / 随机走位 / 环绕 / 固定）一起走，彼此相隔 360°/个数
                          （两个时正好相对：一个贴左耳时另一个贴右耳，换边时一个从脸前、一个从脑后绕）；不加就是各自固定
 左右双人（一个音频当成两个人：同一个声音一个在左耳、一个在右耳，第 2 个晚一点；批量时每个文件都这样）
  --duo [毫秒]            第 2 个声音晚多少毫秒（1–300；不写数值 = 80）。30 ms 以下两边容易融成一个声音，
                          50–120 ms 像两个人一前一后地说，150 ms 以上像回声。可以和 --pos（两个位置）、--mix-gain、
                          --mix-follow、--mix-balance 一起用；输出比原音频长出这段延迟
  --style ear|ku100|breath|earclean   ear=贴耳加强；ku100=KU100 近耳轻语；breath=呼吸音（近耳呼吸与气流）；
                          earclean=掏耳（给棉签 / 挖耳勺 / 刮擦这类触发音用：伸进耳道进进出出，
                          带耳道共鸣和耳内低频，每只耳朵约 15 秒换一次边，目标响度默认 −20 LUFS）
  --start <秒> --duration <秒>      只处理一段
  --channel auto|left|right|mix     立体声取哪个声道（auto=更响的那个）
 背景音（实时生成的雨声，不用录音素材；人声不受影响。界面里叫「背景音」）
  --bg rain-night|off     雨夜预设：四面八方的小到中雨，重心绕着头慢慢转（8D 环绕），身边地上的落地声比参考的 8D 雨声轻一点，
                          不隔窗、没有屋檐滴水，比人声低约 18 dB；后面写的背景音选项再覆盖它
  --bg-level <0–100>      音量（0 = 关）：1–100 对应比人声的目标响度低 40 … 0 dB（雨夜 55 = 低 18 dB）
  --rain <0–100>          雨量：毛毛雨 0.3 mm/h … 暴雨 60 mm/h（对数刻度，35）
  --bg-pan <−100–100>     雨从哪边来：−100 左、0 四周、100 右（0）
  --bg-width <0–100>      包围感：0 只从一个方向来，100 四面八方（100）
  --bg-flow <0–100>       流动：雨声的重心绕着头慢慢转（平均 30 秒一圈），0 不动，50 和参考的 8D 雨声一样（50）
  --bg-indoor <0–100>     隔窗：0 站在雨里，100 关着窗在屋里（0）
  --rain-land <0–100>     落地声：身边地上雨点「噗」的一声，0 = 没有，60 ≈ 参考的 8D 雨声，每格 0.2 dB；
                          屋檐滴水的响度也跟着它（30；旧名 --rain-near 照样能用）
  --rain-drip <0–100>     屋檐滴水「嗒」：越大越快、地方越多（最多 3 处），0 = 没有（0）
  --thunder <0–100>       雷声（可选）：0 = 没有；越大越近、越频繁、越响——1 ≈ 4 分钟一次、8 km 外的闷雷，
                          50 ≈ 77 秒、3.5 km，100 ≈ 25 秒、1.5 km；打开后 3–10 秒内来第一声（0）
 贴耳感
  --dist <米>             贴耳时嘴到头中心的距离（0.105，即离耳约 1.7 cm）
  --isolation <dB>        远耳额外压低（32）      --presence <dB>     近耳临场感（3）
  --warmth <dB>           凑近的低频温暖感（6）  --ear-warmth <dB>   近耳低频（5）
  --far-lf-cut <dB>       远耳低频再压低（12）    --dark <dB>         近耳 5 kHz 压低（0）
  --ku100-eq              近耳用 KU100 实测校正曲线
  --breath <dB>           气声增强：贴耳时轻的送气声 / 气息最多提高多少，响的齿音不提（6，0=关闭）
  --breath-deboom <0–12>  呼吸去闷：100–300 Hz 堆积时的最大衰减 dB（0=关，呼吸音预设为 6）
  --breath-tone <0–1>     薄声补偿：按输入的气流低频和高频亮度调节，0=关
  --breath-pause-switch on|off   穿梭达到停留时间后在换气间隙换边（呼吸音预设开启）
  --breath-reach <0–1>    近处气声范围：身前等近处也保留部分气声增强，到头中心 20 cm 内最强，65 cm 处淡出（0=仅贴耳）
  --puff <x>              气流扑耳：送气时气流扑到耳朵上的低频脉冲，由嗓音自己的送气声导出（1，0=关闭）
  --pinna ku100|A…G       耳廓选择：ku100=人工头自己的耳廓；A…G=真人耳廓（挑最像自己耳朵的）；--pinna list 列出（ku100）
  --mouth <cm>            嘴的大小：嘴是约 2 cm 宽的开口而不是一个点，贴耳时耳廓的窄谷变浅（2，0=点声源）
  --far-shadow <dB>       贴耳时远耳 6 kHz 以上再压低（5，0=关闭）
  --soft-near <x>         贴耳轻声：凑到耳边时嗓音均衡成放轻声音，退开时恢复（0.5，0=关闭）
  --concha <dB>           耳甲共鸣：贴耳时近耳 4.3 kHz 的耳甲腔共振（4，0=关闭）
 入耳（掏耳；--inear 为 0 时这一组不起作用）
  --inear <cm>            入耳深度：贴到耳朵上之后最深伸进耳道多少（0=不进耳道；earclean 2.1；最大 2.3）
  --inear-period <秒>     一次伸进去—退出来大约几秒（4）    --inear-random <0–1>  进出的随机程度（0.7）
  --canal <dB>            耳道共鸣：伸得越深共振越高，约 3 → 11 kHz（10，0=关闭）
  --rumble <x>            耳内低频：摩擦 / 触碰在耳道里的低频压力，由输入自己的包络导出（1，0=关闭）
  --muffle <dB>           堵耳闷感：伸进耳道时近耳 2.5 kHz 以上压低（6，0=关闭）
 左右移动
  --mode sweep|random|orbit|static  穿梭 / 随机走位 / 环绕 / 固定（sweep）
  --hold-time <秒> --move-time <秒> 停留、移动时长（2.5、2.5）
  --randomness <0–1>      时长和落点的随机程度（0.1）
  --approach <0–1>        换边时先退远再凑近的概率（0.25）
  --front <0–1>           随机走位：停在正前方的比例（0.08；ku100 0.36）
  --via front|back|alternate|random  从哪里绕（front）
  --ear-angle <度>（100）  --pass-dist <米>（0.2）  --az <度>（270）  --radius <米>（0.35）  --period <秒>（12）
  --level-follow <0–1>    响度随位置变化的程度（按模式：0.2 / 0.5 / 0.3 / 1）
 忽远忽近
  --sway <x>（1）  --sway-period <秒>（4）  --sway-random <0–1>（1）  --sway-follow <x>（1）
  --sway-level <0–1>      退开时耳边音量保留物理衰减的比例（0.45）
  --pause-motion <0–1>    停顿时动作的速度，1=一直在动（0.13）
  --micro <x>             自然晃动幅度（1）
 上下
  --height <度>           嘴相对耳朵向上（正）/ 向下（负）偏多少度，离耳朵的距离不变（0）
  --updown <度>           自动上下的幅度，0=关闭（25）    --updown-period <秒>（9）  --updown-random <0–1>（0.6）
  --over-top <0–1>        换耳时从头顶绕过去的概率（0）
 房间与回放
  --room none|real        KU100 实测真实房间（none）    --wet <dB>（-3）
  --static-ambience on|off  固定原声底噪。所有预设默认关闭
  --headphone diffuse|native|<型号>   回放补偿（diffuse）；--headphone list 列出型号
 人声处理
  --denoise [0–100]       输入降噪 RNNoise（关）。数值 = 底噪最多压低多少 dB，如 12；不写数值或 100 = 降到最干净
  --deess <dB>            去齿音最多压低（0）       --soft-voice <x>   轻声化（0）
  --sharp-soften <0–8>    强辅音柔化：强辅音突出时，5 kHz 附近最多收轻多少 dB（0=关）
 输出
  --lufs <L>（-16）  --peak <dBTP>（-1）  --max-gr <dB>（8）  --bits 16|24（24）  --seed <n>（1）
  --save-trajectory <csv> 导出实际走位（时间, 方位角, 距离, 不含忽远忽近的距离, 说话活动, 仰角, 入耳深度）
";

/// `--bg` 的预设：rain-night（雨夜）或 off。
fn parse_bg_preset(p: Params, v: Option<&str>) -> Result<Params, String> {
    match v {
        Some("rain-night") | Some("雨夜") | Some("rain") => Ok(p.bg_rain_night()),
        Some("off") | Some("关") => Ok(Params { bg_level: 0.0, ..p }),
        _ => Err("--bg 只能是 rain-night 或 off".into()),
    }
}

fn parse_on_off(value: Option<&String>, name: &str) -> Result<bool, String> {
    match value.map(String::as_str) {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        _ => Err(format!("{name} 需要 on 或 off")),
    }
}

fn realtime_breath_options(p: &mut Params, args: &[String]) -> Result<(), String> {
    for (i, name) in args.iter().enumerate() {
        let value = args.get(i + 1);
        match name.as_str() {
            "--breath-tone" => p.breath_tone = parse_f(value, name)?.clamp(0.0, 1.0),
            "--breath-deboom" => p.breath_deboom_db = parse_f(value, name)?.clamp(0.0, 12.0),
            "--breath-pause-switch" => p.breath_pause_switch = parse_on_off(value, name)?,
            "--static-ambience" => p.static_ambience = parse_on_off(value, name)?,
            _ => {}
        }
    }
    Ok(())
}

/// 背景音的选项（convert 和 bg 共用）；是背景音选项时返回 true。
fn parse_bg_option(p: &mut Params, name: &str, it: &mut std::iter::Peekable<std::slice::Iter<String>>) -> Result<bool, String> {
    let mut f = |lo: f32, hi: f32| parse_f(it.next(), name).map(|v| v.clamp(lo, hi));
    match name {
        "--bg" => {
            it.next();
        }
        "--bg-level" => p.bg_level = f(0.0, 100.0)?,
        "--rain" => p.rain = f(0.0, 100.0)?,
        "--bg-pan" => p.bg_pan = f(-100.0, 100.0)?,
        "--bg-width" => p.bg_width = f(0.0, 100.0)?,
        "--bg-indoor" => p.bg_indoor = f(0.0, 100.0)?,
        "--rain-land" | "--rain-near" => p.rain_near = f(0.0, 100.0)?,
        "--bg-flow" => p.bg_flow = f(0.0, 100.0)?,
        "--rain-drip" => p.rain_drip = f(0.0, 100.0)?,
        "--thunder" => p.bg_thunder = f(0.0, 100.0)?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// `bg`：只生成背景音（雨声），写成双耳 WAV。默认雨夜，响度按 --lufs（默认 −20 LUFS），开头 1 秒淡入、结尾 2 秒淡出。
fn run_bg(args: &[String]) -> Result<(), String> {
    use crate::dsp::dynamics::{Limiter, LoudnessMeter};
    use crate::dsp::rain::Rain;
    let mut p = Params::default().bg_rain_night();
    if let Some(i) = args.iter().position(|a| a == "--bg") {
        p = parse_bg_preset(p, args.get(i + 1).map(|s| s.as_str()))?;
    }
    let (mut out, mut secs, mut lufs, mut bits) = (PathBuf::from("rain_bg.wav"), 60.0f32, -20.0f32, 24u16);
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        let name = a.as_str();
        if parse_bg_option(&mut p, name, &mut it)? {
            continue;
        }
        match name {
            "-o" | "--output" => out = PathBuf::from(it.next().ok_or("-o 后面缺少文件名")?),
            "--duration" => secs = parse_f(it.next(), name)?.max(1.0),
            "--lufs" => lufs = parse_f(it.next(), name)?,
            "--bits" => bits = if parse_f(it.next(), name)? as u16 == 16 { 16 } else { 24 },
            "--seed" => p.seed = parse_f(it.next(), name)? as u64,
            _ => return Err(format!("bg 不认识的选项 {name}")),
        }
    }
    if p.bg_level <= 0.0 {
        return Err("背景音是关着的（--bg off 或 --bg-level 0）".into());
    }
    // 只要雨声本身：音量当作 0 dB，响度按 --lufs
    p.bg_level = 100.0;
    let cfg = EngineConfig::new(p.clone(), None);
    let setup = cfg.derived.rain.clone().ok_or("背景音没打开")?;
    let reference = lufs - setup.trim_db;
    let mut rain = Rain::new(setup, p.seed, cfg.derived.playback.as_ref());
    rain.prime_gain(reference);
    let n = (secs * FS) as usize;
    let spec = hound::WavSpec { channels: 2, sample_rate: FS_USIZE as u32, bits_per_sample: bits, sample_format: hound::SampleFormat::Int };
    let mut wav = hound::WavWriter::create(&out, spec).map_err(|e| format!("无法写入 {}：{e}", out.display()))?;
    let full = if bits == 16 { 32767.0 } else { 8_388_607.0 };
    let mut lim = Limiter::new(6.0, 40.0);
    let ld = lim.delay();
    let ceiling = 10f32.powf(-1.0 / 20.0) * 0.97;
    let mut meter = LoudnessMeter::new();
    let (mut l, mut r) = (vec![0f32; 4096], vec![0f32; 4096]);
    let (fi, fo) = (FS_USIZE.min(n / 4), (2 * FS_USIZE).min(n / 3));
    let t0 = std::time::Instant::now();
    let mut i = 0;
    while i < n + ld {
        let m = 4096.min(n + ld - i);
        rain.render(reference, &mut l[..m], &mut r[..m]);
        for k in 0..m {
            let j = i + k;
            let a = if j < fi { 0.5 - 0.5 * (std::f32::consts::PI * j as f32 / fi as f32).cos() } else { 1.0 };
            let left = n.saturating_sub(j);
            let b = if left < fo { 0.5 - 0.5 * (std::f32::consts::PI * left as f32 / fo as f32).cos() } else { 1.0 };
            let (x, y) = lim.process(l[k] * a * b, r[k] * a * b, ceiling);
            if j >= ld {
                meter.push(x, y);
                wav.write_sample((x.clamp(-1.0, 1.0) * full).round() as i32).map_err(|e| e.to_string())?;
                wav.write_sample((y.clamp(-1.0, 1.0) * full).round() as i32).map_err(|e| e.to_string())?;
            }
        }
        i += m;
    }
    wav.finalize().map_err(|e| e.to_string())?;
    println!("{}", crate::dsp::rain::describe(&Params { bg_level: 55.0, ..p.clone() }, false).replace("比人声低 18 dB，", ""));
    println!(
        "完成：{}  ({secs:.1} s，{:.1} LUFS；落地声 {} 个，屋檐滴水 {} 滴，雷 {} 次；生成用时 {:.2} s)",
        out.display(),
        meter.integrated(),
        rain.n_land,
        rain.n_drips,
        rain.thunder_strikes(),
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

fn parse_f(v: Option<&String>, name: &str) -> Result<f32, String> {
    v.ok_or(format!("{name} 后面缺少数值"))?.parse::<f32>().map_err(|_| format!("{name} 的值不是数字"))
}

/// `--pos` 里的一个位置：预设名（英文或中文），或"方位角[:到头中心的距离 米]"。
fn parse_pos(s: &str) -> Result<Voice, String> {
    const EN: [&str; 8] = ["left", "right", "front", "back", "front-left", "front-right", "back-left", "back-right"];
    let t = s.trim();
    if let Some(i) = EN.iter().position(|n| n.eq_ignore_ascii_case(t)).or_else(|| PRESETS.iter().position(|p| p.0 == t)) {
        return Ok(Voice::preset(i));
    }
    let bad = || format!("--pos 里的 {t:?} 不认识：写 left / right / front / back …，或 方位角[:距离]");
    let (a, d) = match t.split_once(':') {
        Some((a, d)) => (a, Some(d)),
        None => (t, None),
    };
    let az = a.trim().parse::<f32>().map_err(|_| bad())?;
    let dist = match d {
        Some(d) => Some(d.trim().parse::<f32>().map_err(|_| bad())?.clamp(R_MIN, R_MAX)),
        None => None,
    };
    Ok(Voice { az: az.rem_euclid(360.0), dist, gain_db: 0.0 })
}

/// 命令行输出里一个音源的位置说明。
fn voice_text(v: &Voice) -> String {
    let pos = match v.preset_index() {
        Some(i) => PRESETS[i].0.to_string(),
        None => match v.dist {
            Some(d) => format!("方位 {:.0}°、离头中心 {:.0} cm", v.az, d * 100.0),
            None => format!("方位 {:.0}°、贴耳", v.az),
        },
    };
    if v.gain_db != 0.0 {
        format!("{pos}，{:+.1} dB", v.gain_db)
    } else {
        pos
    }
}

pub fn run(args: Vec<String>) -> i32 {
    match args[0].as_str() {
        "convert" => match run_convert(&args[1..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("错误：{e}");
                2
            }
        },
        "bg" => match run_bg(&args[1..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("错误：{e}");
                2
            }
        },
        "align" => match run_align(&args[1..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("错误：{e}");
                2
            }
        },
        "decode" => match run_decode(&args[1..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("错误：{e}");
                2
            }
        },
        "devices" => {
            crate::realtime::print_devices();
            0
        }
        "rt-test" => {
            // 调试用：用默认设备实时跑几秒（可加 秒数、--buffer 帧数、--style ku100、--denoise）
            let secs = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(5.0);
            let mut p = match style_from_args(&args) {
                Ok((p, _)) => p,
                Err(e) => { eprintln!("{e}"); return 1; }
            };
            p.denoise = args.iter().any(|a| a == "--denoise");
            if let Err(e) = realtime_breath_options(&mut p, &args) {
                eprintln!("{e}");
                return 1;
            }
            let mut o = crate::realtime::RtOptions::default();
            if let Some(i) = args.iter().position(|a| a == "--buffer") {
                o.buffer_frames = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(256);
            }
            // --file <音频>：不用麦克风，改用实时引擎播放文件（试听链路）；--gain <dB>：输出增益
            if let Some(i) = args.iter().position(|a| a == "--gain") {
                p.out_gain_db = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
                p.auto_level = false;
            }
            let file = args.iter().position(|a| a == "--file").and_then(|i| args.get(i + 1)).map(PathBuf::from);
            match crate::realtime::selftest(secs, &o, Arc::new(EngineConfig::new(p, None)), file) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("错误：{e}");
                    2
                }
            }
        }
        "rt-sim" => match run_rt_sim(&args[1..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("错误：{e}");
                2
            }
        },
        "bench" => {
            bench();
            0
        }
        "-h" | "--help" | "help" => {
            println!("{HELP}");
            0
        }
        other => {
            eprintln!("不认识的命令：{other}\n\n{HELP}");
            2
        }
    }
}

fn style_from_args(args: &[String]) -> Result<(Params, f32), String> {
    let p = Params::default();
    let default_lufs = p.target_lufs;
    let Some(i) = args.iter().position(|a| a == "--style") else { return Ok((p, default_lufs)); };
    match args.get(i + 1).map(String::as_str) {
        Some("ear") => Ok((p.style_ear(), default_lufs)),
        Some("ku100") => Ok((p.style_ku100(), default_lufs)),
        Some("breath" | "呼吸音") => Ok((p.style_breath(), crate::dsp::params::BREATH_LUFS)),
        Some("earclean" | "ear-clean" | "掏耳") => Ok((p.style_earclean(), crate::dsp::params::EARCLEAN_LUFS)),
        _ => Err("--style 只能是 ear、ku100、breath 或 earclean".into()),
    }
}

fn run_convert(args: &[String]) -> Result<(), String> {
    let (mut p, mut lufs) = style_from_args(args)?;
    let style_mode = p.mode;
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut out_dir: Option<PathBuf> = None;
    let mut jobs = crate::batch::default_threads();
    let mut output: Option<PathBuf> = None;
    let (mut start, mut duration) = (0.0f64, None);
    let mut channel = Channel::Auto;
    let (mut bits, mut peak, mut max_gr) = (24u16, -1.0f32, 8.0f32);
    let mut level_follow: Option<f32> = None;
    let mut save_traj: Option<PathBuf> = None;
    let (mut mix_on, mut mix_follow, mut mix_align) = (false, false, false);
    let mut mix_balance = 0.0f32;
    let mut duo_ms = 0.0f32;
    let (mut pos_list, mut gain_list): (Option<String>, Option<String>) = (None, None);
    // 背景音的预设也先生效，后面的背景音选项再覆盖
    if let Some(i) = args.iter().position(|a| a == "--bg") {
        p = parse_bg_preset(p, args.get(i + 1).map(|s| s.as_str()))?;
    }
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        let name = a.as_str();
        let f = |it: &mut std::iter::Peekable<std::slice::Iter<String>>| parse_f(it.next(), name);
        if parse_bg_option(&mut p, name, &mut it)? {
            continue;
        }
        match name {
            "-o" | "--output" => output = Some(PathBuf::from(it.next().ok_or("-o 后面缺少文件名")?)),
            "--out-dir" => out_dir = Some(PathBuf::from(it.next().ok_or("--out-dir 后面缺少文件夹")?)),
            "-j" | "--jobs" => jobs = (f(&mut it)? as usize).max(1),
            "--mix" => mix_on = true,
            "--mix-follow" => mix_follow = true,
            "--mix-align" => mix_align = true,
            "--mix-balance" => {
                mix_balance = 100.0;
                if let Some(v) = it.peek().and_then(|s| s.parse::<f32>().ok()) {
                    mix_balance = v.clamp(0.0, 100.0);
                    it.next();
                }
            }
            "--duo" => {
                duo_ms = mix::DUO_DEFAULT_MS;
                if let Some(v) = it.peek().and_then(|s| s.parse::<f32>().ok()) {
                    duo_ms = v.clamp(0.0, mix::DUO_MAX_MS);
                    it.next();
                }
            }
            "--pos" => pos_list = Some(it.next().ok_or("--pos 后面缺少位置")?.clone()),
            "--mix-gain" => gain_list = Some(it.next().ok_or("--mix-gain 后面缺少数值")?.clone()),
            "--style" => {
                it.next();
            }
            "--start" => start = f(&mut it)? as f64,
            "--duration" => duration = Some(f(&mut it)? as f64),
            "--channel" => channel = parse_channel(it.next().map(|s| s.as_str()).unwrap_or(""))?,
            "--dist" => p.ear_dist = f(&mut it)?.clamp(0.095, 1.0),
            "--isolation" => p.isolation_db = f(&mut it)?,
            "--presence" => p.presence_db = f(&mut it)?,
            "--warmth" => p.warmth_db = f(&mut it)?,
            "--ear-warmth" => p.ear_warmth_db = f(&mut it)?,
            "--far-lf-cut" => p.far_lf_cut_db = f(&mut it)?,
            "--dark" => p.dark_db = f(&mut it)?,
            "--ku100-eq" => p.ku100_eq = true,
            "--breath" => p.breath_db = f(&mut it)?.max(0.0),
            "--breath-deboom" => p.breath_deboom_db = f(&mut it)?.clamp(0.0, 12.0),
            "--breath-tone" => p.breath_tone = f(&mut it)?.clamp(0.0, 1.0),
            "--breath-pause-switch" => p.breath_pause_switch = parse_on_off(it.next(), name)?,
            "--breath-reach" => p.breath_reach = f(&mut it)?.clamp(0.0, 1.0),
            "--puff" => p.puff = f(&mut it)?.max(0.0),
            "--concha" => p.concha_db = f(&mut it)?,
            "--pinna" => {
                let v = it.next().ok_or("--pinna 需要一个名字")?;
                let ps = crate::dsp::pinna::pinnas();
                if v.eq_ignore_ascii_case("list") {
                    println!("ku100（默认，KU100 人工头自己的耳廓）");
                    for q in &ps.items {
                        println!("{}（真人，HUTUBS pp{}）", q.name, q.subject);
                    }
                    return Ok(());
                }
                if v.eq_ignore_ascii_case("ku100") {
                    p.pinna = "KU100".to_string();
                } else if let Some(i) = ps.index(v) {
                    p.pinna = ps.items[i].name.clone();
                } else {
                    return Err(format!("--pinna {v:?} 不认识，用 --pinna list 查看"));
                }
            }
            "--mouth" => p.mouth_cm = f(&mut it)?.clamp(0.0, 8.0),
            "--far-shadow" => p.far_shadow_db = f(&mut it)?.max(0.0),
            "--soft-near" => p.soft_near = f(&mut it)?.max(0.0),
            "--inear" => p.inear_cm = f(&mut it)?.clamp(0.0, crate::dsp::INEAR_MAX_CM),
            "--inear-period" => p.inear_period = f(&mut it)?.max(0.5),
            "--inear-random" => p.inear_random = f(&mut it)?.clamp(0.0, 1.0),
            "--canal" => p.canal_db = f(&mut it)?.clamp(0.0, 24.0),
            "--rumble" => p.rumble = f(&mut it)?.max(0.0),
            "--muffle" => p.muffle_db = f(&mut it)?.max(0.0),
            "--front" => p.front = f(&mut it)?.clamp(0.0, 1.0),
            "--mode" => {
                p.mode = match it.next().map(|s| s.as_str()) {
                    Some("sweep") => MotionMode::Sweep,
                    Some("random") => MotionMode::Random,
                    Some("orbit") => MotionMode::Orbit,
                    Some("static") => MotionMode::Static,
                    _ => return Err("--mode 只能是 sweep / random / orbit / static".into()),
                }
            }
            "--hold-time" => p.hold_s = f(&mut it)?,
            "--move-time" => p.move_s = f(&mut it)?,
            "--randomness" => p.randomness = f(&mut it)?,
            "--approach" => p.approach = f(&mut it)?,
            "--via" => {
                p.via = match it.next().map(|s| s.as_str()) {
                    Some("front") => Via::Front,
                    Some("back") => Via::Back,
                    Some("alternate") => Via::Alternate,
                    Some("random") => Via::Random,
                    _ => return Err("--via 只能是 front / back / alternate / random".into()),
                }
            }
            "--ear-angle" => p.ear_angle = f(&mut it)?,
            "--pass-dist" => p.pass_dist = f(&mut it)?,
            "--az" => p.static_az = f(&mut it)?,
            "--radius" => p.orbit_radius = f(&mut it)?,
            "--period" => p.orbit_period = f(&mut it)?,
            "--level-follow" => level_follow = Some(f(&mut it)?),
            "--sway" => p.sway = f(&mut it)?,
            "--sway-period" => p.sway_period = f(&mut it)?,
            "--sway-random" => p.sway_random = f(&mut it)?,
            "--sway-follow" => p.sway_follow = f(&mut it)?,
            "--sway-level" => p.sway_level = f(&mut it)?,
            "--pause-motion" => p.pause_motion = f(&mut it)?,
            "--micro" => p.micro = f(&mut it)?,
            "--height" => p.height = f(&mut it)?.clamp(-80.0, 85.0),
            "--updown" => p.updown = f(&mut it)?.clamp(0.0, 80.0),
            "--updown-period" => p.updown_period = f(&mut it)?,
            "--updown-random" => p.updown_random = f(&mut it)?,
            "--over-top" => p.over_top = f(&mut it)?.clamp(0.0, 1.0),
            "--room" => {
                p.room = match it.next().map(|s| s.as_str()) {
                    Some("none") => false,
                    Some("real") => true,
                    _ => return Err("--room 只能是 none 或 real".into()),
                }
            }
            "--wet" => p.wet_db = f(&mut it)?,
            "--static-ambience" => p.static_ambience = parse_on_off(it.next(), name)?,
            "--headphone" => {
                let v = it.next().ok_or("--headphone 后面缺少名称")?;
                let norm = |s: &str| s.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>();
                p.playback = match v.to_lowercase().as_str() {
                    "diffuse" | "df" => Playback::Diffuse,
                    "native" | "none" | "ku100" => Playback::Native,
                    "list" => {
                        println!("diffuse（通用扩散场补偿）\nnative（不补偿，KU100 原机音色）");
                        for (n, _) in &tables().headphones {
                            println!("{n}");
                        }
                        return Ok(());
                    }
                    _ => {
                        let hits: Vec<&String> = tables().headphones.iter().map(|(n, _)| n).filter(|n| norm(n).contains(&norm(v))).collect();
                        if hits.len() != 1 {
                            return Err(format!("耳机型号 {v:?} 没有唯一匹配，用 --headphone list 查看"));
                        }
                        Playback::Headphone(hits[0].clone())
                    }
                };
            }
            "--denoise" => {
                p.denoise = true;
                if let Some(v) = it.peek().and_then(|s| s.parse::<f32>().ok()) {
                    p.denoise = v > 0.0; // 0 = 关闭
                    p.denoise_db = v.clamp(0.0, crate::dsp::params::DENOISE_MAX_DB);
                    it.next();
                }
            }
            "--deess" => p.deess_db = f(&mut it)?,
            "--sharp-soften" => p.sharp_soften_db = f(&mut it)?.clamp(0.0, 8.0),
            "--soft-voice" => p.soft_voice = f(&mut it)?,
            "--lufs" => lufs = f(&mut it)?,
            "--peak" => peak = f(&mut it)?,
            "--max-gr" => max_gr = f(&mut it)?,
            "--bits" => bits = if f(&mut it)? as u16 == 16 { 16 } else { 24 },
            "--seed" => p.seed = f(&mut it)? as u64,
            "--save-trajectory" => save_traj = Some(PathBuf::from(it.next().ok_or("--save-trajectory 后面缺少文件名")?)),
            _ if name.starts_with('-') => return Err(format!("不认识的选项 {name}")),
            _ => inputs.push(PathBuf::from(name)),
        }
    }
    p.level_follow = match level_follow {
        Some(v) => v,
        None if p.mode == style_mode => p.level_follow,
        None => Params::default_level_follow(p.mode),
    };
    let duo_on = duo_ms > 0.0;
    if duo_on && mix_on {
        return Err("--duo（一个音频当成左右两个人）不能和 --mix 一起用".into());
    }
    if mix_align && !mix_on {
        return Err("--mix-align 只用于多音频合成，请同时加 --mix".into());
    }
    if (pos_list.is_some() || gain_list.is_some() || mix_follow || mix_balance > 0.0) && !mix_on && !duo_on {
        return Err("--pos / --mix-gain / --mix-follow / --mix-balance 只用于多音频合成（--mix）或左右双人（--duo）".into());
    }
    if mix_follow && pos_list.is_some() {
        return Err("一起移动时位置由 --mode 的走位决定，不能再用 --pos".into());
    }
    // 左右双人：两个声音的位置和音量（默认左耳、右耳）
    let duo = if duo_on {
        let mut voices = [Voice::preset(0), Voice::preset(1)];
        if let Some(s) = &pos_list {
            let v = s.split(',').filter(|x| !x.trim().is_empty()).map(parse_pos).collect::<Result<Vec<Voice>, String>>()?;
            if v.len() > 2 {
                return Err(format!("左右双人只有两个声音，--pos 给了 {} 个位置", v.len()));
            }
            voices[..v.len()].copy_from_slice(&v);
        }
        if let Some(s) = &gain_list {
            let g = s.split(',').map(|x| x.trim().parse::<f32>().map_err(|_| format!("--mix-gain 里的 {:?} 不是数字", x.trim()))).collect::<Result<Vec<f32>, String>>()?;
            if g.len() > 2 {
                return Err(format!("左右双人只有两个声音，--mix-gain 给了 {} 个数值", g.len()));
            }
            for (v, g) in voices.iter_mut().zip(g) {
                v.gain_db = g;
            }
        }
        if save_traj.is_some() {
            return Err("左右双人不支持 --save-trajectory".into());
        }
        let d = Duo { voices, follow: mix_follow, balance: mix_balance / 100.0, ..Duo::new(duo_ms) };
        let pos = if mix_follow { "一起移动，彼此相对".to_string() } else { format!("{} / {}", voice_text(&voices[0]), voice_text(&voices[1])) };
        eprintln!("左右双人：第 2 个声音晚 {duo_ms:.0} ms（{pos}）{}", if mix_balance > 0.0 { format!("，自动平衡 {mix_balance:.0}%") } else { String::new() });
        Some(d)
    } else {
        None
    };
    if mix_on {
        if out_dir.is_some() {
            return Err("多音频合成只输出一个文件，请用 -o 指定，不能用 --out-dir".into());
        }
        if save_traj.is_some() {
            return Err("多音频合成不支持 --save-trajectory".into());
        }
        let inputs = crate::batch::expand_inputs(&inputs);
        if inputs.len() < 2 || inputs.len() > MAX_VOICES {
            return Err(format!("多音频合成需要 2–{MAX_VOICES} 个音频，现在是 {} 个", inputs.len()));
        }
        let mut voices: Vec<Voice> = Vec::new();
        if let Some(s) = &pos_list {
            for part in s.split(',').filter(|x| !x.trim().is_empty()) {
                voices.push(parse_pos(part)?);
            }
            if voices.len() > inputs.len() {
                return Err(format!("--pos 给了 {} 个位置，但只有 {} 个音频", voices.len(), inputs.len()));
            }
        }
        while voices.len() < inputs.len() {
            let v = mix::default_voice(&voices);
            voices.push(v);
        }
        if let Some(s) = &gain_list {
            let g = s.split(',').map(|x| x.trim().parse::<f32>().map_err(|_| format!("--mix-gain 里的 {:?} 不是数字", x.trim()))).collect::<Result<Vec<f32>, String>>()?;
            if g.len() > inputs.len() {
                return Err(format!("--mix-gain 给了 {} 个数值，但只有 {} 个音频", g.len(), inputs.len()));
            }
            for (v, g) in voices.iter_mut().zip(g) {
                v.gain_db = g;
            }
        }
        let mut opts = ConvertOptions::new(inputs[0].clone());
        opts.output = output.unwrap_or_else(|| default_mix_output(&inputs[0]));
        opts.start = start;
        opts.duration = duration;
        opts.channel = channel;
        opts.bits = bits;
        opts.lufs = lufs;
        opts.peak_db = peak;
        opts.max_gr_db = max_gr;
        let items: Vec<MixInput> = inputs.iter().cloned().zip(voices.iter().copied()).map(|(path, voice)| MixInput { path, voice }).collect();
        if mix_follow {
            eprintln!("多音频合成 {} 个音频，一起移动（彼此相隔 {:.0}°）：", items.len(), 360.0 / items.len() as f32);
        } else {
            eprintln!("多音频合成 {} 个音频，各自固定：", items.len());
        }
        for (k, it) in items.iter().enumerate() {
            let pos = if mix_follow { format!("+{:.0}°{}", 360.0 * k as f32 / items.len() as f32, if it.voice.gain_db != 0.0 { format!("，{:+.1} dB", it.voice.gain_db) } else { String::new() }) } else { voice_text(&it.voice) };
            eprintln!("  {}. {}（{pos}）", k + 1, it.path.display());
        }
        let mut last = -1i32;
        if mix_align {
            eprintln!("  先自动对齐（--start / --duration 按对齐之后的时间算）");
        }
        if mix_balance > 0.0 {
            eprintln!("  自动平衡 {mix_balance:.0}%（按短时响度让几段一样响）");
        }
        let mo = MixOptions { follow: mix_follow, align: mix_align, balance: mix_balance / 100.0, ..Default::default() };
        let rep = convert_mix(&opts, &items, &p, mo, Arc::new(AtomicBool::new(false)), |f, stage| {
            let pc = (f * 100.0) as i32;
            if pc / 10 != last / 10 {
                eprintln!("  {stage} {pc}%");
                last = pc;
            }
        })?;
        if !rep.channel_note.is_empty() {
            println!("{}", rep.channel_note);
        }
        if !rep.align_note.is_empty() {
            println!("{}", rep.align_note);
        }
        if !rep.balance_note.is_empty() {
            println!("{}", rep.balance_note);
        }
        if !rep.bg_note.is_empty() {
            println!("{}", rep.bg_note);
        }
        println!(
            "完成：{}  ({:.1} s，{:.1} LUFS，真峰值 {:.1} dBTP，限制器最多压 {:.1} dB；说话平均占 {:.0}%，{}；用时 {:.1} s)",
            opts.output.display(),
            rep.seconds,
            rep.lufs,
            rep.true_peak_db,
            rep.max_gr_db,
            rep.speech_pct,
            if mix_follow {
                format!("一起换边 {} 次（其中从头顶绕 {} 次），往后让 {} 次，上下换位 {} 次", rep.n_moves, rep.n_tops, rep.n_sways, rep.n_updowns)
            } else {
                format!("往后让共 {} 次，上下换位共 {} 次", rep.n_sways, rep.n_updowns)
            },
            rep.elapsed_s
        );
        return Ok(());
    }
    let is_batch = inputs.len() > 1 || inputs.iter().any(|p| p.is_dir()) || out_dir.is_some();
    let inputs = crate::batch::expand_inputs(&inputs);
    if inputs.is_empty() {
        return Err("缺少输入文件".into());
    }
    if is_batch {
        if output.is_some() {
            return Err("批量转换不能用 -o，请用 --out-dir 指定输出文件夹".into());
        }
        if save_traj.is_some() {
            return Err("批量转换不支持 --save-trajectory".into());
        }
        if let Some(d) = &out_dir {
            std::fs::create_dir_all(d).map_err(|e| format!("无法创建 {}：{e}", d.display()))?;
        }
        let outs = crate::batch::plan_outputs(&inputs, out_dir.as_deref());
        let items: Vec<ConvertOptions> = inputs
            .iter()
            .zip(outs)
            .map(|(inp, out)| {
                let mut o = ConvertOptions::new(inp.clone());
                o.output = out;
                o.start = start;
                o.duration = duration;
                o.channel = channel;
                o.bits = bits;
                o.lufs = lufs;
                o.peak_db = peak;
                o.max_gr_db = max_gr;
                o.duo = duo;
                o
            })
            .collect();
        let n = items.len();
        let workers = jobs.min(n);
        eprintln!("批量转换 {n} 个文件，{workers} 个线程并行");
        let t0 = std::time::Instant::now();
        let n_done = std::sync::atomic::AtomicUsize::new(0);
        let n_fail = std::sync::atomic::AtomicUsize::new(0);
        let cpu = std::sync::Mutex::new(0f64);
        let est = crate::batch::estimates(&items);
        crate::batch::run(&items, &est, &p, jobs, &Arc::new(AtomicBool::new(false)), |_, _, _| {}, |i, r| {
            let k = n_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            match r {
                Ok(rep) => {
                    *cpu.lock().unwrap() += rep.elapsed_s;
                    println!("[{k}/{n}] 完成：{}  ({:.1} s，{:.1} LUFS，真峰值 {:.1} dBTP；用时 {:.1} s)", items[i].output.display(), rep.seconds, rep.lufs, rep.true_peak_db, rep.elapsed_s);
                }
                Err(e) => {
                    n_fail.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    println!("[{k}/{n}] 失败：{}：{e}", items[i].input.display());
                }
            }
        });
        let wall = t0.elapsed().as_secs_f64();
        let cpu = *cpu.lock().unwrap();
        println!("全部完成：成功 {}，失败 {}；总用时 {wall:.1} s，各文件用时合计 {cpu:.1} s（平均同时在转换 {:.1} 个文件）", n - n_fail.load(std::sync::atomic::Ordering::Relaxed), n_fail.load(std::sync::atomic::Ordering::Relaxed), cpu / wall.max(1e-9));
        return if n_fail.load(std::sync::atomic::Ordering::Relaxed) > 0 { Err("有文件转换失败".into()) } else { Ok(()) };
    }
    let input = inputs.into_iter().next().unwrap();
    let mut opts = ConvertOptions::new(input.clone());
    opts.output = output.unwrap_or_else(|| default_output(&input));
    opts.start = start;
    opts.duration = duration;
    opts.channel = channel;
    opts.bits = bits;
    opts.lufs = lufs;
    opts.peak_db = peak;
    opts.max_gr_db = max_gr;
    opts.save_trajectory = save_traj;
    opts.duo = duo;
    let mut last = -1i32;
    let rep = convert(&opts, &p, Arc::new(AtomicBool::new(false)), |f, stage| {
        let pc = (f * 100.0) as i32;
        if pc / 10 != last / 10 {
            eprintln!("  {stage} {pc}%");
            last = pc;
        }
    })?;
    for note in [&rep.channel_note, &rep.duo_note, &rep.balance_note, &rep.bg_note] {
        if !note.is_empty() {
            println!("{note}");
        }
    }
    println!(
        "完成：{}  ({:.1} s，{:.1} LUFS，真峰值 {:.1} dBTP，限制器最多压 {:.1} dB；说话占 {:.0}%，换耳 {} 次（其中从头顶绕 {} 次），往后让 {} 次，上下换位 {} 次{}；用时 {:.1} s)",
        opts.output.display(),
        rep.seconds,
        rep.lufs,
        rep.true_peak_db,
        rep.max_gr_db,
        rep.speech_pct,
        rep.n_moves,
        rep.n_tops,
        rep.n_sways,
        rep.n_updowns,
        if p.inear_cm > 0.0 { format!("，伸进耳道 {} 次", rep.n_strokes) } else { String::new() },
        rep.elapsed_s
    );
    Ok(())
}

/// 调试用：不经过声卡，把一个文件按"主播模式"（实时引擎：没有预读、说话检测现学现用）跑一遍，
/// 打印走位统计；可以模拟小音量麦克风（--gain）、底噪（--noise）、说得少（--sparse）、开头静音（--lead）。
///   auto-asmr rt-sim <音频> [--duration 秒] [--gain dB] [--noise dBFS] [--sparse 说秒/停秒] [--lead 秒]
///                    [--style ku100] [--mode …] [--pause-motion x] [--denoise [dB]] [--no-level]
///                    [--manual "秒:方位角,距离,高度;…;秒:auto"] [-o 输出.wav]
fn run_rt_sim(args: &[String]) -> Result<(), String> {
    let (mut p, _) = style_from_args(args)?;
    let get = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
    let getf = |name: &str| get(name).and_then(|v| v.parse::<f32>().ok());
    realtime_breath_options(&mut p, args)?;
    let input = PathBuf::from(args.first().ok_or("缺少输入文件")?);
    let mut opts = ConvertOptions::new(input);
    opts.duration = getf("--duration").map(|v| v as f64);
    opts.start = getf("--start").unwrap_or(0.0) as f64;
    if let Some(m) = get("--mode") {
        p.mode = match m.as_str() {
            "random" => MotionMode::Random,
            "orbit" => MotionMode::Orbit,
            "static" => MotionMode::Static,
            _ => MotionMode::Sweep,
        };
        p.level_follow = Params::default_level_follow(p.mode);
    }
    if let Some(v) = getf("--pause-motion") {
        p.pause_motion = v;
    }
    if let Some(v) = getf("--micro") {
        p.micro = v;
    }
    if let Some(i) = args.iter().position(|a| a == "--denoise") {
        p.denoise = true;
        if let Some(v) = args.get(i + 1).and_then(|v| v.parse::<f32>().ok()) {
            p.denoise_db = v.clamp(1.0, crate::dsp::params::DENOISE_MAX_DB);
        }
    }
    if let Some(v) = getf("--height") {
        p.height = v;
    }
    if let Some(v) = getf("--updown") {
        p.updown = v;
    }
    if let Some(v) = getf("--over-top") {
        p.over_top = v;
    }
    let (mut x, _) = crate::offline::load_mono(&opts, &AtomicBool::new(false))?;
    let g = crate::dsp::db_to_lin(getf("--gain").unwrap_or(0.0));
    for v in x.iter_mut() {
        *v *= g;
    }
    if let Some(sp) = get("--sparse") {
        // "1.5/4"：每说 1.5 秒，插入 4 秒静音
        let mut it = sp.split('/').filter_map(|v| v.parse::<f32>().ok());
        let (on, off) = (it.next().unwrap_or(1.5), it.next().unwrap_or(4.0));
        let (n_on, n_off) = ((on * FS) as usize, (off * FS) as usize);
        let mut y = Vec::with_capacity(x.len() * 3);
        for c in x.chunks(n_on.max(1)) {
            y.extend_from_slice(c);
            y.extend(std::iter::repeat(0.0).take(n_off));
        }
        if let Some(d) = opts.duration {
            y.truncate((d * FS as f64) as usize);
        }
        x = y;
    }
    if let Some(l) = getf("--lead") {
        let mut y = vec![0f32; (l * FS) as usize];
        y.extend_from_slice(&x);
        x = y;
    }
    if let Some(n) = getf("--noise") {
        let a = crate::dsp::db_to_lin(n) * 3f32.sqrt(); // 均匀分布白噪声，有效值 n dBFS
        let mut seed = 7u32;
        for v in x.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *v += a * ((seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0);
        }
    }
    if args.iter().any(|a| a == "--no-level") {
        p.auto_level = false;
    }
    // --manual "0:270,0.105,0;3:90,0.105,30;6:auto"：按时间表（秒:方位角,到头中心的距离,高度）切到手动并摆位，
    // "auto" 切回自动——用来测试手动摆位
    let mut script: Vec<(f32, Option<[f32; 3]>)> = get("--manual")
        .map(|s| {
            s.split(';')
                .filter_map(|e| {
                    let (t, v) = e.split_once(':')?;
                    let t = t.trim().parse::<f32>().ok()?;
                    if v.trim() == "auto" {
                        return Some((t, None));
                    }
                    let f: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    (f.len() >= 2).then(|| (t, Some([f[0], f[1], f.get(2).copied().unwrap_or(0.0)])))
                })
                .collect()
        })
        .unwrap_or_default();
    let auto_mode = p.mode;
    let mut cur = p.clone();
    let mut cfg = Arc::new(EngineConfig::new(p, None));
    let mut eng = Engine::new(cfg.clone(), PostMode::Realtime, 0.0);
    eng.set_live(!args.iter().any(|a| a == "--strict")); // --strict：动作严格跟着说话（试听 / 修复前的行为）
    eng.trace = Some(Vec::new());
    let (mut l, mut r) = (vec![0f32; x.len()], vec![0f32; x.len()]);
    for (i, c) in x.chunks(128).enumerate() {
        let o = i * 128;
        while script.first().map_or(false, |e| e.0 <= o as f32 / FS) {
            match script.remove(0).1 {
                Some([az, d, tilt]) => {
                    cur.mode = MotionMode::Manual;
                    (cur.manual_az, cur.manual_dist, cur.manual_tilt) = (az, d, tilt);
                }
                None => cur.mode = auto_mode,
            }
            cfg = Arc::new(EngineConfig::new(cur.clone(), Some(&cfg)));
            eng.set_config(cfg.clone());
        }
        eng.process(c, &mut l[o..o + c.len()], &mut r[o..o + c.len()]);
    }
    let tr = eng.trace.take().unwrap_or_default();
    // 每 0.5 秒一个字符：L / R 贴在左 / 右耳边，l / r 偏左 / 偏右，^ 正前，v 正后，T 头顶；下一行是说话检测（# 说话，. 停顿）
    let per = (0.5 / crate::dsp::CTRL_DT) as usize;
    let (mut line, mut acts) = (String::new(), String::new());
    for c in tr.chunks(per) {
        let m = c[c.len() / 2];
        let ce = m[5].to_radians().cos();
        let s = m[1].to_radians().sin() * ce;
        let f = m[1].to_radians().cos() * ce;
        line.push(if m[5] > 55.0 {
            'T'
        } else if s < -0.85 {
            'L'
        } else if s > 0.85 {
            'R'
        } else if s < -0.3 {
            'l'
        } else if s > 0.3 {
            'r'
        } else if f > 0.0 {
            '^'
        } else {
            'v'
        });
        let a = c.iter().map(|v| v[4]).sum::<f32>() / c.len() as f32;
        acts.push(if a > 0.6 {
            '#'
        } else if a > 0.2 {
            '+'
        } else {
            '.'
        });
    }
    let secs = x.len() as f32 / FS;
    println!("主播模式模拟 {secs:.1} s（每个字符 0.5 s）");
    for (a, b) in line.as_bytes().chunks(120).zip(acts.as_bytes().chunks(120)) {
        println!("  位置 {}", String::from_utf8_lossy(a));
        println!("  说话 {}", String::from_utf8_lossy(b));
    }
    let speech = 100.0 * eng.n_speech as f32 / eng.n_blocks.max(1) as f32;
    let rms = |v: &[f32]| (v.iter().map(|a| (a * a) as f64).sum::<f64>() / v.len().max(1) as f64).sqrt() as f32;
    println!(
        "  说话占 {speech:.0}%，换耳 {} 次，往后让 {} 次；输入有效值 {:.1} dBFS，输出有效值 {:.1} dBFS",
        eng.tele.n_moves,
        eng.tele.n_sways,
        crate::dsp::lin_to_db(rms(&x)),
        crate::dsp::lin_to_db(rms(&l).max(rms(&r)))
    );
    if cur.inear_cm > 0.0 && !tr.is_empty() {
        // 入耳（掏耳）：在耳道里的时间占比、深度的中位数
        let mut d: Vec<f32> = tr.iter().map(|v| v[6] * 100.0).collect();
        d.sort_by(|a, b| a.total_cmp(b));
        let inside = d.iter().filter(|&&v| v > 0.3).count() as f32 / d.len() as f32;
        println!("  伸进耳道 {} 次，{:.0}% 的时间在耳道里，深度中位 {:.1} cm、最深 {:.1} cm", eng.tele.n_strokes, 100.0 * inside, d[d.len() / 2], d[d.len() - 1]);
    }
    if let Some(out) = get("-o") {
        let spec = hound::WavSpec { channels: 2, sample_rate: FS_USIZE as u32, bits_per_sample: 24, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(out, spec).map_err(|e| e.to_string())?;
        for (a, b) in l.iter().zip(r.iter()) {
            w.write_sample((a.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32).map_err(|e| e.to_string())?;
            w.write_sample((b.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32).map_err(|e| e.to_string())?;
        }
        w.finalize().map_err(|e| e.to_string())?;
        println!("  已写出 {out}");
    }
    Ok(())
}

/// 调试用：只解码并取成 48 kHz 单声道，写成 WAV（用来核对解码器）。
/// 加 `--denoise [dB]` 时再过一遍输入降噪（不做空间化），可以单独听降噪的效果。
fn parse_channel(s: &str) -> Result<Channel, String> {
    match s {
        "auto" => Ok(Channel::Auto),
        "left" => Ok(Channel::Left),
        "right" => Ok(Channel::Right),
        "mix" => Ok(Channel::Mix),
        _ => Err("--channel 只能是 auto / left / right / mix".into()),
    }
}

/// `align`：整段解码几段音频（和合成试听一样），自动对齐，打印每段从哪里开始用、裁掉多少，以及各项得分。
fn run_align(args: &[String]) -> Result<(), String> {
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut channel = Channel::Auto;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--channel" => channel = parse_channel(it.next().map(|s| s.as_str()).unwrap_or(""))?,
            s if s.starts_with('-') => return Err(format!("align 不认识的选项：{s}")),
            s => inputs.push(PathBuf::from(s)),
        }
    }
    if inputs.len() < 2 || inputs.len() > MAX_VOICES {
        return Err(format!("align 需要 2–{MAX_VOICES} 个音频"));
    }
    let t0 = std::time::Instant::now();
    let mut monos = Vec::new();
    for p in &inputs {
        let mut o = ConvertOptions::new(p.clone());
        o.channel = channel;
        let (m, _) = crate::offline::load_mono(&o, &AtomicBool::new(false)).map_err(|e| format!("{}：{e}", p.display()))?;
        monos.push(m);
    }
    let t1 = std::time::Instant::now();
    let refs: Vec<&[f32]> = monos.iter().map(|m| m.as_slice()).collect();
    let al = crate::dsp::align::align(&refs)?;
    let names: Vec<String> = inputs.iter().map(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()).collect();
    println!("{}", al.describe(&names, false));
    for (k, q) in al.pairs.iter().enumerate() {
        println!(
            "  第 {} 段相对第 1 段：偏移 {:+.4} s（{}），得分 {:.3}（次好 {:.3}，{:.2} 倍），重叠部分相关 {:.2}，几处偏移相差 {:.1} ms，{}",
            k + 2,
            q.offset as f64 / FS as f64,
            if q.exact { "逐样本" } else { "按起音包络" },
            q.score,
            q.second,
            q.score / q.second.max(1e-6),
            q.local_r,
            q.drift_ms,
            if q.reliable { "可靠" } else { "不可靠" }
        );
    }
    println!("解码 {:.2} s，对齐 {:.2} s", (t1 - t0).as_secs_f64(), t1.elapsed().as_secs_f64());
    Ok(())
}

fn run_decode(args: &[String]) -> Result<(), String> {
    let input = PathBuf::from(args.first().ok_or("缺少输入文件")?);
    let out = args.iter().position(|a| a == "-o").and_then(|i| args.get(i + 1)).map(PathBuf::from).unwrap_or_else(|| input.with_extension("decoded.wav"));
    let mut opts = ConvertOptions::new(input);
    if let Some(i) = args.iter().position(|a| a == "--duration") {
        opts.duration = args.get(i + 1).and_then(|v| v.parse().ok());
    }
    let (mut mono, note) = crate::offline::load_mono(&opts, &AtomicBool::new(false))?;
    if let Some(i) = args.iter().position(|a| a == "--denoise") {
        let mut p = Params::default();
        if let Some(v) = args.get(i + 1).and_then(|v| v.parse::<f32>().ok()) {
            p.denoise_db = v.clamp(1.0, crate::dsp::params::DENOISE_MAX_DB);
        }
        let mix = p.denoise_mix();
        let mut d = crate::dsp::voice::Denoiser::new();
        // 降噪器有 20 ms 的固定时延：多喂 20 ms 的静音，再把开头的 20 ms 去掉，输出与输入对齐
        mono.extend(std::iter::repeat(0.0).take(crate::dsp::voice::DENOISE_DELAY));
        let y: Vec<f32> = mono.iter().map(|&v| d.process(v, mix)).collect();
        mono = y[crate::dsp::voice::DENOISE_DELAY..].to_vec();
    }
    let spec = hound::WavSpec { channels: 1, sample_rate: FS_USIZE as u32, bits_per_sample: 24, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&out, spec).map_err(|e| e.to_string())?;
    for v in &mono {
        w.write_sample((v.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())?;
    println!("{note}\n解码 {:.2} s -> {}", mono.len() as f32 / FS, out.display());
    Ok(())
}

pub use crate::dsp::testsig::{test_scratch, test_voice};

fn bench() {
    let x = test_voice(30.0);
    // 掏耳预设用刮擦声来测（给触发音用的），其余用人声
    let scratch = test_scratch(30.0);
    let cases: [(&str, Params); 7] = [
        ("贴耳加强（默认）", Params::default()),
        ("贴耳加强 + 雨夜背景音", Params::default().bg_rain_night()),
        ("贴耳加强 + 雨（落地声 100、屋檐滴水 100、暴雨）", Params { rain: 100.0, rain_near: 100.0, rain_drip: 100.0, ..Params::default().bg_rain_night() }),
        ("贴耳加强 + 雨夜 + 雷声 100", Params { bg_thunder: 100.0, ..Params::default().bg_rain_night() }),
        ("掏耳（入耳：耳道共鸣 + 耳内低频）", Params::default().style_earclean()),
        ("KU100 风格（真实房间）", Params::default().style_ku100()),
        ("KU100 风格 + 降噪 + 去齿音 + 轻声化", {
            let mut p = Params::default().style_ku100();
            p.denoise = true;
            p.deess_db = 6.0;
            p.soft_voice = 1.0;
            p
        }),
    ];
    println!("每次处理 128 个样本（2.67 ms 一块），模拟实时回调：");
    for (name, p) in cases {
        let x = if p.inear_cm > 0.0 { &scratch } else { &x };
        let cfg = Arc::new(EngineConfig::new(p, None));
        let mut eng = Engine::new(cfg, PostMode::Realtime, 0.0);
        let (mut l, mut r) = ([0f32; 128], [0f32; 128]);
        let (mut worst, mut total) = (0f64, 0f64);
        for c in x.chunks_exact(128) {
            let t0 = std::time::Instant::now();
            eng.process(c, &mut l, &mut r);
            let dt = t0.elapsed().as_secs_f64();
            worst = worst.max(dt);
            total += dt;
        }
        let blocks = (x.len() / 128) as f64;
        println!(
            "  {name}：比实时快 {:.0} 倍，单核占用约 {:.1}%；每块平均 {:.0} µs，最慢 {:.0} µs；算法延时 {:.1} ms",
            (x.len() as f64 / FS_USIZE as f64) / total,
            100.0 * total / (x.len() as f64 / FS_USIZE as f64),
            1e6 * total / blocks,
            1e6 * worst,
            1000.0 * eng.latency_samples() as f64 / FS_USIZE as f64
        );
    }
    // 多音频合成的试听：每个音源一个引擎，相加后统一做自动响度和限幅
    for (n, follow) in [(2usize, false), (MAX_VOICES, false), (2, true)] {
        let base = Arc::new(EngineConfig::new(Params::default(), None));
        let mut voices: Vec<Voice> = Vec::new();
        for _ in 0..n {
            let v = mix::default_voice(&voices);
            voices.push(v);
        }
        let setup = mix::MixSetup::new(&base, &voices, &vec![None; n], follow, None);
        let mut mx = mix::Mixer::new(&setup, PostMode::Realtime, 0.0);
        // 各音源错开 0.1 s，免得几路完全相同
        let ins: Vec<Vec<f32>> = (0..n).map(|k| x.iter().copied().skip(k * 4800).chain(std::iter::repeat(0.0).take(k * 4800)).collect()).collect();
        let (mut l, mut r) = ([0f32; 128], [0f32; 128]);
        let (mut worst, mut total) = (0f64, 0f64);
        for o in (0..x.len() - 128).step_by(128) {
            let refs: Vec<&[f32]> = ins.iter().map(|v| &v[o..o + 128]).collect();
            let t0 = std::time::Instant::now();
            mx.process(&refs, &mut l, &mut r);
            let dt = t0.elapsed().as_secs_f64();
            worst = worst.max(dt);
            total += dt;
        }
        let secs = x.len() as f64 / FS_USIZE as f64;
        println!("  多音频合成 {n} 个音源{}（试听）：单核占用约 {:.1}%；每块平均 {:.0} µs，最慢 {:.0} µs", if follow { "、一起移动" } else { "" }, 100.0 * total / secs, 1e6 * total / (x.len() / 128) as f64, 1e6 * worst);
    }
}
