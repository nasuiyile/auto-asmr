//! 全局设置：整份共用的 [`Saved`]（桌面版的配置文件格式，见 `auto_asmr::config`），其中的参数生成引擎配置。
//!
//! - 导入桌面版的配置时整份换掉，桌面版独有的设置（音频设备、缓冲、文件转换……）原样留着，导出时一起写回去，
//!   带回电脑照样能用。安卓用得上的是 `params` 和 `dry`（输出效果 / 监听原声）。
//! - 界面（Kotlin）按字段读写参数（JSON）。几个界面上的换算量（贴耳距离、离头距离、移动频率、降噪滑条）是
//!   「虚拟字段」，读写都调用库里 `Params` 的同名方法，和桌面版界面的操作完全一样；换左右移动的方式时
//!   「响度随位置变化」跟着换成默认值，也和桌面版一样。
//! - 风格、恢复默认、雨夜这些按钮调用库里同一个函数。
//! - 左右双人（`duo_ms`、`mix_follow`、`mix_balance`）是配置文件里和桌面版共用的设置（不在 `params` 里；延迟是桌面版
//!   「文件转换」页的那个，实时页的 `rt_duo_ms` 不用——网页里是录好的声音，和文件试听一样），
//!   界面按同样的字段名读写；打开时在这里算好两个声音的合成配置（[`Global::duo`]），处理器按版本号换上。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use auto_asmr::config::{self, ConfigError, Saved};
use auto_asmr::dsp::engine::EngineConfig;
use auto_asmr::dsp::mix::{self, MixSetup, DUO_MAX_MS, DUO_VOICES};
use auto_asmr::dsp::motion::up_angle;
use auto_asmr::dsp::params::{MotionMode, Params};
use auto_asmr::dsp::pinna::pinnas;
use auto_asmr::dsp::tables::tables;
use auto_asmr::dsp::R_MIN;
use serde_json::{json, Value};

pub struct Global {
    pub saved: Saved,
    /// 完整的配置：给单独输出的雨声用（[`crate::rain`]）。
    pub cfg: Arc<EngineConfig>,
    /// 去掉背景音的配置：给网页声音的处理器用。雨声单独一条输出流，网页暂停时也不断，所以引擎里不再混一份。
    pub cfg_voice: Arc<EngineConfig>,
    /// 左右双人打开时两个声音的合成配置（由 `cfg_voice` 生成，位置固定在左耳、右耳）；关着时为 None。
    pub duo: Option<Arc<MixSetup>>,
    pub error: String,
}

/// 左右双人的合成配置（关着时 None）。`prev` 里没变的引擎配置原样复用。
fn duo_for(s: &Saved, base: &Arc<EngineConfig>, prev: Option<&MixSetup>) -> Option<Arc<MixSetup>> {
    (s.duo_ms > 0.0).then(|| Arc::new(mix::duo_setup(base, &DUO_VOICES, s.duo_ms, s.mix_follow, s.mix_balance / 100.0, prev)))
}

/// 网页声音用的参数：背景音关掉，其余一样。
fn voice_params(p: &Params) -> Params {
    Params { bg_level: 0.0, ..p.clone() }
}

pub static GLOBAL: Mutex<Option<Global>> = Mutex::new(None);
/// 配置的版本号：每换一次加一，音频线程据此决定要不要换配置。
pub static CFG_VER: AtomicU64 = AtomicU64::new(1);

pub fn with_global<R>(f: impl FnOnce(&mut Global) -> R) -> R {
    let mut g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let g = g.get_or_insert_with(|| {
        let saved = Saved::default();
        let cfg = Arc::new(EngineConfig::new(saved.params.clone(), None));
        let cfg_voice = Arc::new(EngineConfig::new(voice_params(&saved.params), Some(&cfg)));
        let duo = duo_for(&saved, &cfg_voice, None);
        Global { cfg, cfg_voice, duo, saved, error: String::new() }
    });
    f(g)
}

/// 网页声音的处理器用的配置（没有背景音）。
pub fn current_cfg() -> Arc<EngineConfig> {
    with_global(|g| g.cfg_voice.clone())
}

/// 完整的配置（雨声用）。
pub fn full_cfg() -> Arc<EngineConfig> {
    with_global(|g| g.cfg.clone())
}

/// 参数变了：重新生成引擎配置（没变的部分复用），版本号加一。
fn commit(g: &mut Global) {
    g.cfg = Arc::new(EngineConfig::new(g.saved.params.clone(), Some(&g.cfg)));
    g.cfg_voice = Arc::new(EngineConfig::new(voice_params(&g.saved.params), Some(&g.cfg_voice)));
    g.duo = duo_for(&g.saved, &g.cfg_voice, g.duo.as_deref());
    CFG_VER.fetch_add(1, Ordering::Release);
}

/// 改参数；真的有变化才重新生成配置。
pub fn update_params(f: impl FnOnce(&mut Params) -> Result<(), String>) -> Result<(), String> {
    with_global(|g| {
        let mut p = g.saved.params.clone();
        f(&mut p)?;
        if p != g.saved.params {
            g.saved.params = p;
            commit(g);
        }
        Ok(())
    })
}

/// 按钮（风格、恢复默认、背景音预设，见 [`action`]）。风格按钮和恢复默认还会关掉左右双人：各风格的默认状态都是一个声音
/// （同桌面版）。
pub fn run_action(name: &str) -> Result<(), String> {
    let resets_duo = name == "reset" || Params::STYLES.contains(&name);
    with_global(|g| {
        let mut s = g.saved.clone();
        action(&mut s.params, name)?;
        if resets_duo {
            s.duo_ms = 0.0;
        }
        if s.params != g.saved.params || s.duo_ms != g.saved.duo_ms {
            g.saved = s;
            commit(g);
        }
        Ok(())
    })
}

/// 改配置文件里 `params` 之外的一项（左右双人、文件转换页的设置，见 [`DOC_KEYS`]）。左右双人那几项变了才重新生成
/// 引擎配置（网页 / 全局模式的处理器要换）；文件转换页的设置只记下来（文件转换页每次轮询时自己读）。
pub fn update_doc(key: &str, v: Value) -> Result<(), String> {
    with_global(|g| {
        let mut s = g.saved.clone();
        set_doc_field(&mut s, key, v)?;
        let duo_changed = (s.duo_ms, s.mix_follow, s.mix_balance) != (g.saved.duo_ms, g.saved.mix_follow, g.saved.mix_balance);
        g.saved = s;
        if duo_changed {
            commit(g);
        }
        Ok(())
    })
}

pub fn set_dry(dry: bool) {
    with_global(|g| g.saved.dry = dry);
}

// ------------------------------------------------------------------ 配置文件

/// 导入配置文件（桌面版导出 / 自动保存的文件，或安卓版自己存的同一格式）：整份换掉。
pub fn load_config(text: &str) -> Result<(), String> {
    let s = config::parse(text).map_err(|e| match e {
        ConfigError::NotAutoAsmr => "文件里没有 auto-asmr 的配置".to_string(),
        ConfigError::Parse(e) => format!("配置文件读不懂：{e}"),
    })?;
    with_global(|g| {
        g.saved = s;
        commit(g);
    });
    Ok(())
}

/// 只检查、不导入：别的 App（QQ、微信……）用「其他应用打开」传来的文件是不是 auto-asmr 的配置文件。
/// 是的话返回给确认弹窗看的概要（风格、移动方式、贴耳距离、房间、降噪、雨声、原声），不是的话返回原因。
/// 只解析，不碰数据表，所以引擎还没加载完时也能用。
pub fn describe_config(text: &str) -> Result<String, ConfigError> {
    let s = config::parse(text)?;
    let p = &s.params;
    // 风格按钮只改风格相关的参数：按一下没有变化，就是这个风格（和界面上标出选中的风格是同一个判断）
    let style = match p.active_style() {
        Some("style_earclean") => "掏耳",
        Some("style_breath") => "呼吸音",
        Some("style_ku100") => "KU100 实录",
        Some("style_ear") => "贴耳加强",
        _ => "自定义",
    };
    let mode = match p.mode {
        MotionMode::Sweep => "穿梭",
        MotionMode::Random => "随机走位",
        MotionMode::Orbit => "环绕",
        MotionMode::Static => "固定",
        MotionMode::Manual => "手动摆位",
    };
    let mut parts = vec![format!("风格：{style}"), format!("左右移动：{mode}"), format!("贴耳距离 {:.1} cm", p.ear_cm())];
    if p.inear_cm > 0.0 {
        parts.push(format!("入耳 {:.1} cm", p.inear_cm));
    }
    parts.push(if p.room { "真实房间" } else { "不加房间" }.to_string());
    if p.denoise {
        parts.push(format!("降噪 {:.0}", p.denoise_slider()));
    }
    if let Some(db) = p.bg_db() {
        parts.push(format!("背景雨声（比人声低 {:.0} dB）", -db));
    }
    if s.duo_ms > 0.0 {
        parts.push(format!("左右双人（第 2 个声音晚 {:.0} ms）", s.duo_ms));
    }
    if s.dry {
        parts.push("原声（不加效果）".to_string());
    }
    Ok(parts.join(" · "))
}

/// 导出配置文件（和桌面版「导出配置」同一格式）。
pub fn export_config() -> Result<String, String> {
    with_global(|g| config::to_text(&g.saved))
}

// ------------------------------------------------------------------ 按字段读写

/// 界面上的换算量：读写都走库里 `Params` 的同名方法（和桌面版界面一样）。
pub const VIRTUAL: [&str; 4] = ["ear_cm", "manual_cm", "moves_per_min", "denoise_slider"];

/// 配置文件里 `params` 之外、界面也按字段读写的设置（和桌面版共用）：左右双人（排布、自动平衡也是多音频合成的），
/// 以及文件转换页的设置（多音频合成、自动对齐、目标响度、取哪个声道、位深、同时转换几个）。
pub const DOC_KEYS: [&str; 9] = ["duo_ms", "mix_follow", "mix_balance", "mix", "mix_align", "file_lufs", "file_channel", "bits", "jobs"];

/// 改 [`DOC_KEYS`] 里的一项。范围和桌面版界面一样：延迟 0–300 ms（0 = 关），自动平衡 0–100 %，声道 0 自动 / 1 左 /
/// 2 右 / 3 混合，位深 16 / 24，同时转换 0 = 自动（逻辑核数）。
pub fn set_doc_field(s: &mut Saved, key: &str, v: Value) -> Result<(), String> {
    let as_bool = |v: &Value| v.as_bool().ok_or_else(|| format!("{key} 要 true / false"));
    match key {
        "duo_ms" => s.duo_ms = as_f32(&v)?.clamp(0.0, DUO_MAX_MS),
        "mix_follow" => s.mix_follow = as_bool(&v)?,
        "mix_balance" => s.mix_balance = as_f32(&v)?.clamp(0.0, 100.0),
        "mix" => s.mix = as_bool(&v)?,
        "mix_align" => s.mix_align = as_bool(&v)?,
        "file_lufs" => s.file_lufs = as_f32(&v)?.clamp(-40.0, 0.0),
        "file_channel" => s.file_channel = (as_f32(&v)? as i64).clamp(0, 3) as u8,
        "bits" => s.bits = if as_f32(&v)? < 20.0 { 16 } else { 24 },
        "jobs" => s.jobs = (as_f32(&v)? as i64).clamp(0, 256) as usize,
        _ => return Err(format!("没有这个设置：{key}")),
    }
    Ok(())
}

/// 界面要的全部字段：[`params_json`] 加上左右双人那几项。
pub fn saved_json(s: &Saved) -> Value {
    let mut v = params_json(&s.params, s.dry);
    if let Some(m) = v.as_object_mut() {
        m.insert("duo_ms".into(), json!(s.duo_ms));
        m.insert("mix_follow".into(), json!(s.mix_follow));
        m.insert("mix_balance".into(), json!(s.mix_balance));
        m.insert("mix".into(), json!(s.mix));
        m.insert("mix_align".into(), json!(s.mix_align));
        m.insert("file_lufs".into(), json!(s.file_lufs));
        m.insert("file_channel".into(), json!(s.file_channel));
        m.insert("bits".into(), json!(s.bits));
        m.insert("jobs".into(), json!(s.jobs));
    }
    v
}

/// 全部参数（JSON 对象，键就是 `Params` 的字段名），加上虚拟字段和 `dry`。
pub fn params_json(p: &Params, dry: bool) -> Value {
    let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
    if let Some(m) = v.as_object_mut() {
        m.insert("ear_cm".into(), json!(p.ear_cm()));
        m.insert("manual_cm".into(), json!(p.manual_cm()));
        m.insert("moves_per_min".into(), json!(p.moves_per_min()));
        m.insert("denoise_slider".into(), json!(p.denoise_slider()));
        // 选中的风格（action 名，参数和哪个风格都对不上时为空串）：界面上把那个风格按钮标出来
        m.insert("active_style".into(), json!(p.active_style().unwrap_or("")));
        m.insert("dry".into(), json!(dry));
    }
    v
}

pub fn current_params_json() -> Value {
    with_global(|g| saved_json(&g.saved))
}

fn as_f32(v: &Value) -> Result<f32, String> {
    v.as_f64().map(|x| x as f32).filter(|x| x.is_finite()).ok_or_else(|| format!("不是数字：{v}"))
}

/// 改一个字段。`mode` 走 [`Params::set_mode`]（响度随位置变化跟着换默认值），虚拟字段走各自的方法，
/// 其余字段直接按 serde 的字段名替换。
pub fn set_field(p: &mut Params, key: &str, v: Value) -> Result<(), String> {
    match key {
        "ear_cm" => p.set_ear_cm(as_f32(&v)?),
        "manual_cm" => p.set_manual_cm(as_f32(&v)?),
        "moves_per_min" => p.set_moves_per_min(as_f32(&v)?),
        "denoise_slider" => p.set_denoise_slider(as_f32(&v)?),
        "mode" => p.set_mode(serde_json::from_value::<MotionMode>(v).map_err(|e| e.to_string())?),
        _ => {
            let mut obj = serde_json::to_value(&*p).map_err(|e| e.to_string())?;
            let m = obj.as_object_mut().ok_or("参数不是对象")?;
            if !m.contains_key(key) {
                return Err(format!("没有这个参数：{key}"));
            }
            m.insert(key.to_string(), v);
            *p = serde_json::from_value(obj).map_err(|e| format!("{key}：{e}"))?;
        }
    }
    Ok(())
}

/// 按钮：风格（同桌面版）、恢复默认、雨夜、关雨声。
pub fn action(p: &mut Params, name: &str) -> Result<(), String> {
    *p = match name {
        "style_ear" => p.clone().style_ear(),
        "style_ku100" => p.clone().style_ku100(),
        "style_earclean" => p.clone().style_earclean(),
        "style_breath" => p.clone().style_breath(),
        "reset" => Params::default(),
        "bg_rain_night" => p.clone().bg_rain_night(),
        "bg_off" => Params { bg_level: 0.0, ..p.clone() },
        _ => return Err(format!("没有这个操作：{name}")),
    };
    Ok(())
}

/// 位置图边缘对应的距离（米），同桌面版。
pub const RADAR_MAX_R: f32 = 0.8;

/// 播放中打开手动摆位：把引擎此刻的位置（方位角、仰角、距离）换成手动模式的三个量
/// （方位角、到头中心的距离、高度），做法同桌面版。
pub fn manual_from_pose(az: f32, el: f32, r: f32) -> (f32, f32, f32) {
    let (up, az, dist) = up_angle(az, el, r);
    (az, dist.clamp(R_MIN, RADAR_MAX_R), up.clamp(-80.0, 85.0))
}

// ------------------------------------------------------------------ 参数表

/// 界面用的参数表（`ui_spec.json`，标签、说明、范围取自桌面版界面），耳廓和耳机型号的选项在这里填上。
pub fn ui_spec() -> Value {
    let mut spec: Value = serde_json::from_str(include_str!("ui_spec.json")).unwrap_or(Value::Null);
    fill_options(&mut spec);
    spec
}

fn fill_options(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if let Some(src) = m.get("options_from").and_then(|s| s.as_str()).map(str::to_string) {
                let opts: Vec<Value> = match src.as_str() {
                    "pinnas" => std::iter::once(json!({"value": "KU100", "label": "KU100（人工头）", "label_en": "KU100 (dummy head)"}))
                        .chain(pinnas().items.iter().map(|q| json!({"value": q.name, "label": format!("真人 {}", q.name), "label_en": format!("Human {}", q.name)})))
                        .collect(),
                    "playback" => [json!({"value": "Diffuse", "label": "通用扩散场", "label_en": "Diffuse field"}), json!({"value": "Native", "label": "KU100 原机音色", "label_en": "KU100 native"})]
                        .into_iter()
                        .chain(tables().headphones.iter().map(|(n, _)| json!({"value": {"Headphone": n}, "label": n, "label_en": n})))
                        .collect(),
                    _ => Vec::new(),
                };
                m.insert("options".into(), Value::Array(opts));
            }
            for x in m.values_mut() {
                fill_options(x);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(fill_options),
        _ => {}
    }
}

/// 改全局设置的测试一个一个跑（测试默认并行）。
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 测试和基准用：从仓库里读数据文件提供给库（App 里是从 APK 资源读）。
pub fn provide_repo_data() {
    if !auto_asmr::dsp::data::provided() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../auto-asmr/assets");
        let ku = std::fs::read(format!("{dir}/ku100.bin")).expect("读不到 ku100.bin");
        let pi = std::fs::read(format!("{dir}/pinna.bin")).expect("读不到 pinna.bin");
        auto_asmr::dsp::data::provide(ku, pi);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(spec: &Value) -> Vec<Value> {
        let mut out = vec![spec["denoise"].clone()];
        for g in spec["groups"].as_array().unwrap() {
            out.extend(g["items"].as_array().unwrap().iter().cloned());
        }
        out
    }

    /// 参数表里每一项都对得上 `Params`：键存在、范围合理、每个选项都能设上去，设完读回来就是它。
    #[test]
    fn ui_spec_matches_params() {
        provide_repo_data();
        let spec = ui_spec();
        let fields = saved_json(&Saved::default());
        // 改一项、读回来：`params` 里的走 set_field，左右双人那几项走 set_doc_field
        let set = |key: &str, v: Value| {
            let mut s = Saved::default();
            if DOC_KEYS.contains(&key) {
                set_doc_field(&mut s, key, v).unwrap();
            } else {
                set_field(&mut s.params, key, v).unwrap();
            }
            saved_json(&s)
        };
        let mut n = 0;
        for it in items(&spec) {
            let t = it["type"].as_str().unwrap();
            if t == "note" {
                continue;
            }
            let key = it["key"].as_str().unwrap();
            assert!(fields.get(key).is_some(), "参数表里的 {key} 不是参数");
            match t {
                "slider" => {
                    let (lo, hi) = (it["min"].as_f64().unwrap(), it["max"].as_f64().unwrap());
                    assert!(lo < hi, "{key} 范围");
                    for x in [lo, hi] {
                        let back = set(key, json!(x))[key].as_f64().unwrap();
                        assert!((back - x).abs() < 1e-3 * (1.0 + x.abs()), "{key} 设 {x} 读回 {back}");
                    }
                }
                "check" => {
                    assert_eq!(set(key, json!(true))[key], json!(true));
                }
                "choice" => {
                    let opts = it["options"].as_array().unwrap();
                    assert!(opts.len() >= 2, "{key} 选项");
                    for o in opts {
                        assert_eq!(set(key, o["value"].clone())[key], o["value"], "{key}");
                    }
                }
                "int" => {
                    let mut p = Params::default();
                    set_field(&mut p, key, json!(7)).unwrap();
                    assert_eq!(p.seed, 7);
                }
                _ => panic!("未知类型 {t}"),
            }
            n += 1;
        }
        assert!(n > 60, "只有 {n} 项");
        // 7 只真人耳廓、二十多款耳机都在选项里
        let menus: Vec<usize> = items(&spec).iter().filter(|i| i["style"] == "menu").map(|i| i["options"].as_array().unwrap().len()).collect();
        assert!(menus.contains(&8) && menus.iter().any(|&k| k > 20), "{menus:?}");
    }

    #[test]
    fn fields_and_actions_follow_the_desktop_logic() {
        provide_repo_data();
        let mut p = Params::default();
        // 换方式：响度随位置变化跟着换
        set_field(&mut p, "mode", json!("Static")).unwrap();
        assert_eq!((p.mode, p.level_follow), (MotionMode::Static, 1.0));
        // 虚拟字段
        set_field(&mut p, "ear_cm", json!(3.0)).unwrap();
        assert!((p.ear_cm() - 3.0).abs() < 1e-4);
        set_field(&mut p, "denoise_slider", json!(0)).unwrap();
        assert!(!p.denoise);
        // 未知参数、类型不对：报错，参数不变
        let before = p.clone();
        assert!(set_field(&mut p, "no_such", json!(1)).is_err());
        assert!(set_field(&mut p, "room", json!("yes")).is_err());
        assert_eq!(p, before);
        // 风格按钮和桌面版是同一个函数
        action(&mut p, "style_ku100").unwrap();
        assert_eq!(p, before.clone().style_ku100());
        let expected = p.clone().style_breath();
        action(&mut p, "style_breath").unwrap();
        assert_eq!(p, expected);
        action(&mut p, "bg_rain_night").unwrap();
        assert!(p.bg_level > 0.0);
        action(&mut p, "bg_off").unwrap();
        assert_eq!(p.bg_level, 0.0);
        action(&mut p, "reset").unwrap();
        assert_eq!(p, Params::default());
    }

    /// 各风格的默认状态都是一个声音：风格按钮和恢复默认都关掉左右双人，背景音按钮不动它。
    #[test]
    fn style_buttons_turn_two_voices_off() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        provide_repo_data();
        assert_eq!(Saved::default().duo_ms, 0.0);
        for name in Params::STYLES.into_iter().chain(["reset"]) {
            update_doc("duo_ms", json!(80.0)).unwrap();
            run_action(name).unwrap();
            assert_eq!(with_global(|g| g.saved.duo_ms), 0.0, "{name}");
            assert!(with_global(|g| g.duo.is_none()), "{name}");
        }
        update_doc("duo_ms", json!(80.0)).unwrap();
        run_action("bg_rain_night").unwrap();
        assert_eq!(with_global(|g| g.saved.duo_ms), 80.0);
        run_action("reset").unwrap();
        assert!(run_action("no_such").is_err());
    }

    /// 导入桌面版的配置后再导出：参数和桌面版独有的设置都还在。
    #[test]
    fn desktop_config_round_trips() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        provide_repo_data();
        let desk = Saved { buffer: 512, input: Some("Mic".into()), params: Params { puff: 2.5, ..Params::default() }, ..Saved::default() };
        load_config(&config::to_text(&desk).unwrap()).unwrap();
        update_params(|p| set_field(p, "presence_db", json!(5.0))).unwrap();
        update_params(|p| set_field(p, "static_ambience", json!(true))).unwrap();
        update_params(|p| set_field(p, "breath_deboom_db", json!(4.5))).unwrap();
        update_params(|p| set_field(p, "breath_tone", json!(0.65))).unwrap();
        update_params(|p| set_field(p, "breath_pause_switch", json!(true))).unwrap();
        set_dry(true);
        let back = config::parse(&export_config().unwrap()).unwrap();
        assert!(back.buffer == 512 && back.input.as_deref() == Some("Mic") && back.dry);
        assert!(back.params.static_ambience);
        assert_eq!(back.params.breath_deboom_db, 4.5);
        assert_eq!(back.params.breath_tone, 0.65);
        assert!(back.params.breath_pause_switch);
        assert_eq!((back.params.puff, back.params.presence_db), (2.5, 5.0));
        assert!(load_config("not ron").is_err());
    }

    /// 别的 App 传来的文件：桌面版 / 安卓版导出的配置能认出来并给出概要；别的文件认不出来；检查不会改当前设置。
    #[test]
    fn shared_files_are_recognised_without_importing() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        provide_repo_data();
        let before = export_config().unwrap();
        let desk = Saved { dry: true, params: Params { bg_level: 55.0, ..Params::default().style_ku100() }, ..Saved::default() };
        let d = describe_config(&config::to_text(&desk).unwrap()).unwrap();
        assert!(d.contains("风格：KU100 实录") && d.contains("随机走位") && d.contains("真实房间") && d.contains("比人声低 18 dB") && d.contains("原声"), "{d}");
        let d = describe_config(&config::to_text(&Saved { params: Params::default().style_earclean(), ..Saved::default() }).unwrap()).unwrap();
        assert!(d.contains("风格：掏耳") && d.contains("入耳"), "{d}");
        let d = describe_config(&config::to_text(&Saved { params: Params::default().style_breath(), ..Saved::default() }).unwrap()).unwrap();
        assert!(d.contains("风格：呼吸音"), "{d}");
        let d = describe_config(&config::to_text(&Saved { params: Params { presence_db: 7.0, ..Params::default() }, ..Saved::default() }).unwrap()).unwrap();
        assert!(d.contains("风格：自定义"), "{d}");
        // 桌面版的窗口状态文件（没有设置）、别的程序的键值文件、普通文本：都认不出来
        assert!(matches!(describe_config("{\"egui\": \"()\", \"window\": \"()\"}"), Err(ConfigError::NotAutoAsmr)));
        assert!(matches!(describe_config("今天天气不错"), Err(ConfigError::Parse(_))));
        assert_eq!(export_config().unwrap(), before, "检查不该改当前设置");
    }

    #[test]
    fn manual_from_pose_matches_the_motion_model() {
        provide_repo_data();
        // 手动模式的三个量 → 引擎的位置（lift_tilt）→ 再换回来，应当是原来的值
        let (az, dist, tilt) = (250.0, 0.15, 20.0);
        let (a, e, r) = auto_asmr::dsp::motion::lift_tilt(az, dist, 0.0, tilt);
        let (az2, dist2, tilt2) = manual_from_pose(a, e, r);
        assert!((az2 - az).abs() < 0.5 && (dist2 - dist).abs() < 2e-3 && (tilt2 - tilt).abs() < 0.5, "{az2} {dist2} {tilt2}");
    }
}
