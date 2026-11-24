//! auto-asmr 浏览器插件的 Rust 层：把桌面版的引擎（`auto-asmr` 库里的 `dsp`）和配置文件格式（`config`）编成 wasm。
//!
//! 同一个 wasm 有两种用法：
//! - **插件页面**（弹窗、设置页）用「配置文档」那一组函数：读写和桌面版同一格式的配置文件，按名字改参数、切风格。
//!   参数的换算（贴耳距离、移动频率、降噪滑条……）都用 `Params` 自己的方法，和桌面版界面完全一致。
//! - **AudioWorklet**（音频线程）用「处理器」那一组函数：每个被处理的标签页一个 [`Processor`]，立体声进、双耳声出。
//!
//! 不用 wasm-bindgen：AudioWorklet 里没有 `TextDecoder` 这类接口，而且只能同步实例化。导出的都是 C 函数，
//! 字符串用"指针 + 长度"传：JS 用 `asmr_alloc` 要一块内存把 UTF-8 写进去；返回字符串的函数把结果放进一个公共缓冲，
//! 返回字节数（出错返回 −1），JS 从 `asmr_out_ptr()` 读（下一次调用前有效）。出错的原因用 `asmr_error` 取。
//!
//! 左右双人（配置文件里的 `duo_ms` 大于 0，和桌面版同一个设置）：处理器把同一路声音交给库里的 [`LiveMix`]——一个在左耳、
//! 一个在右耳，第 2 个晚一点（和桌面版主播模式同一套做法），自动平衡的输入响度边听边算。
//!
//! wasm 是单线程的，全局状态放在 `thread_local` 里（电脑上跑测试时每个测试线程各有一份，互不干扰）。

use std::cell::RefCell;
use std::sync::Arc;

use auto_asmr::config::{self, ConfigError, Saved};
use auto_asmr::dsp::dynamics::DryOut;
use auto_asmr::dsp::engine::{Engine, EngineConfig, PostMode};
use auto_asmr::dsp::mix::{self, LiveMix, MixSetup, DUO_VOICES};
use auto_asmr::dsp::params::{MotionMode, Params};
use auto_asmr::dsp::pinna::pinnas;
use auto_asmr::dsp::rain::RainSetup;
use auto_asmr::dsp::tables::tables;
use auto_asmr::dsp::FS_USIZE;

/// 处理器一次最多处理多少帧（AudioWorklet 每次 128 帧，留足余量）。
pub const CAP: usize = 1024;

/// 处理器状态（`asmr_proc_tele` 指向的数组）里各项的位置。
pub mod tele {
    /// 方位角（度，0 前 90 右 180 后 270 左）
    pub const AZ: usize = 0;
    /// 仰角（度）
    pub const EL: usize = 1;
    /// 到头中心的距离（米）
    pub const R: usize = 2;
    /// 说话活动（0–1）
    pub const ACT: usize = 3;
    /// 输入 / 输出电平（dB）
    pub const IN_DB: usize = 4;
    pub const OUT_DB: usize = 5;
    /// 入耳深度（厘米）
    pub const DEPTH_CM: usize = 6;
    /// 引擎算法延时（毫秒）
    pub const LATENCY_MS: usize = 7;
    /// 是不是左右双人（0 / 1）；是的话上面的位置是 1 号声音，下面是 2 号的方位角、仰角、距离、说话活动
    pub const DUO: usize = 8;
    pub const AZ2: usize = 9;
    pub const EL2: usize = 10;
    pub const R2: usize = 11;
    pub const ACT2: usize = 12;
    pub const N: usize = 13;
}

// ------------------------------------------------------------------ 全局状态

struct State {
    /// 当前的配置文档（桌面版的全部设置，插件只用 `params` 和 `dry`，其余原样保留）。
    doc: Saved,
    out: Vec<u8>,
    /// 引擎配置：`asmr_engine_apply` 时由 `doc` 生成；处理器按版本号换上。
    cfg: Option<Arc<EngineConfig>>,
    /// 左右双人打开时两个声音的合成配置（由 `cfg` 生成，位置固定在左耳、右耳），和 `cfg` 一起生成。
    duo: Option<Arc<MixSetup>>,
    ver: u64,
    dry: bool,
}

impl State {
    /// 用文档生成引擎配置和左右双人的合成配置（没变的部分复用）。
    fn apply(&mut self) {
        let cfg = Arc::new(EngineConfig::new(self.doc.params.clone(), self.cfg.as_deref()));
        let d = &self.doc;
        self.duo = (d.duo_ms > 0.0).then(|| Arc::new(mix::duo_setup(&cfg, &DUO_VOICES, d.duo_ms, d.mix_follow, d.mix_balance / 100.0, self.duo.as_deref())));
        self.cfg = Some(cfg);
        self.dry = d.dry;
    }
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State { doc: Saved::default(), out: Vec::new(), cfg: None, duo: None, ver: 0, dry: false });
    /// 出错原因单独放：panic hook 里也要写，那时 `ST` 可能正被借用。
    static ERR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

fn set_error(msg: impl Into<String>) {
    let msg = msg.into();
    ERR.with(|e| {
        if let Ok(mut e) = e.try_borrow_mut() {
            *e = msg;
        }
    });
}

fn fail(msg: impl Into<String>) -> i32 {
    set_error(msg);
    -1
}

/// 把字符串放进公共输出缓冲，返回字节数。
fn put_out(s: String) -> i32 {
    with(|st| {
        st.out = s.into_bytes();
        st.out.len() as i32
    })
}

/// 读 JS 传进来的 UTF-8 字符串。
///
/// # Safety
/// `p` 指向 `n` 个可读字节（`n` 为 0 时可以为空）。
unsafe fn str_arg<'a>(p: *const u8, n: usize) -> Result<&'a str, String> {
    if n == 0 {
        return Ok("");
    }
    if p.is_null() {
        return Err("空指针".into());
    }
    std::str::from_utf8(std::slice::from_raw_parts(p, n)).map_err(|_| "不是 UTF-8 文本".to_string())
}

// ------------------------------------------------------------------ 内存、错误

/// 初始化：装上 panic hook（出错时记下原因，JS 接住异常后用 `asmr_error` 取）。每个 wasm 实例调用一次。
#[no_mangle]
pub extern "C" fn asmr_init() {
    std::panic::set_hook(Box::new(|info| set_error(format!("Rust panic：{info}"))));
}

/// 要一块 `n` 字节的内存（给 JS 写字符串用），用完用 `asmr_free` 还回来。
#[no_mangle]
pub extern "C" fn asmr_alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `p` 来自 `asmr_alloc(n)`，之后不再使用。
#[no_mangle]
pub unsafe extern "C" fn asmr_free(p: *mut u8, n: usize) {
    if !p.is_null() {
        drop(Vec::from_raw_parts(p, 0, n.max(1)));
    }
}

/// 公共输出缓冲的地址（返回字符串的函数把结果放在这里）。
#[no_mangle]
pub extern "C" fn asmr_out_ptr() -> *const u8 {
    with(|st| st.out.as_ptr())
}

/// 最近一次出错的原因（放进输出缓冲），返回字节数。
#[no_mangle]
pub extern "C" fn asmr_error() -> i32 {
    let e = ERR.with(|e| e.try_borrow().map(|e| e.clone()).unwrap_or_default());
    put_out(e)
}

// ------------------------------------------------------------------ 配置文档（插件页面用）

/// 读一份配置文件（桌面版自动保存 / 导出的键值表格式，或直接写的 `Saved`；缺的项用默认值）。
/// 返回 0 = 成功；失败时文档不变。
///
/// # Safety
/// `p` 指向 `n` 个可读字节。
#[no_mangle]
pub unsafe extern "C" fn asmr_doc_load(p: *const u8, n: usize) -> i32 {
    let text = match str_arg(p, n) {
        Ok(s) => s,
        Err(e) => return fail(format!("配置文件{e}")),
    };
    match config::parse(text) {
        Ok(s) => {
            with(|st| st.doc = s);
            0
        }
        Err(ConfigError::NotAutoAsmr) => fail("这个文件里没有 auto-asmr 的设置"),
        Err(ConfigError::Parse(e)) => fail(format!("配置文件读不懂：{e}")),
    }
}

/// 全部效果参数恢复默认、切回「输出效果」、关掉左右双人；桌面版专用的设置（音频设备、文件转换……）不动。
#[no_mangle]
pub extern "C" fn asmr_doc_reset_params() {
    with(|st| {
        st.doc.params = Params::default();
        st.doc.dry = false;
        st.doc.duo_ms = 0.0;
    });
}

/// 当前文档写成配置文件（与桌面版「导出配置」相同的格式），放进输出缓冲，返回字节数。
#[no_mangle]
pub extern "C" fn asmr_doc_text() -> i32 {
    match with(|st| config::to_text(&st.doc)) {
        Ok(s) => put_out(s),
        Err(e) => fail(format!("写配置文件出错：{e}")),
    }
}

/// 界面要显示的内容（JSON）：`params`（全部参数，字段名同配置文件）、`dry`、
/// `derived`（界面上的换算值：`ear_cm` 贴耳距离、`manual_cm` 手动摆位的离头距离、`moves_per_min` 移动频率、
/// `denoise` 降噪滑条、`bg_db` 雨声比人声低多少 dB，关闭时为 null；`style` 选中的风格，编号同 `style`，
/// 参数和哪个风格都对不上时为 null）、
/// `doc`（配置文件里 `params` 之外、和桌面版共用的左右双人设置：`duo_ms`、`mix_follow`、`mix_balance`）。
/// 插件风格按钮的编号（`style` 0..3）对应的风格名（[`Params::STYLES`]）。
const STYLE_ORDER: [&str; 4] = ["style_ear", "style_ku100", "style_earclean", "style_breath"];

#[no_mangle]
pub extern "C" fn asmr_doc_ui() -> i32 {
    let v = with(|st| {
        let p = &st.doc.params;
        serde_json::to_value(p).map(|params| {
            serde_json::json!({
                "params": params,
                "dry": st.doc.dry,
                "derived": {
                    "ear_cm": p.ear_cm(),
                    "manual_cm": p.manual_cm(),
                    "moves_per_min": p.moves_per_min(),
                    "denoise": p.denoise_slider(),
                    "bg_db": p.bg_db(),
                    "style": p.active_style().and_then(|n| STYLE_ORDER.iter().position(|x| *x == n)),
                },
                "doc": {
                    "duo_ms": st.doc.duo_ms,
                    "mix_follow": st.doc.mix_follow,
                    "mix_balance": st.doc.mix_balance,
                }
            })
        })
    });
    match v {
        Ok(v) => put_out(v.to_string()),
        Err(e) => fail(format!("参数转 JSON 出错：{e}")),
    }
}

/// 改一项设置。`key` 是参数名（同配置文件里 `params` 的字段名），`value` 是 JSON。另有几个特殊的名字：
/// - `dry`：true = 监听原声，false = 输出效果
/// - `style`：0 = 贴耳加强，1 = KU100 实录，2 = 掏耳，3 = 呼吸音（同桌面版的风格按钮，只改风格相关的参数）
/// - `bg_preset`：`"rain_night"` = 雨夜，`"off"` = 关掉背景音
/// - `ear_cm`、`manual_cm`、`moves_per_min`、`denoise`：界面上的换算值（见 `asmr_doc_ui`）
/// - `mode`：换左右移动的方式，「响度随位置变化」跟着换成这个方式的默认值（同桌面版）
/// - `duo_ms`（0–300，0 = 关）、`mix_follow`（true = 一起移动）、`mix_balance`（0–100）：左右双人（同桌面版配置文件）
///
/// 返回 0 = 成功；名字不对、值的类型不对时返回 −1，文档不变。
///
/// # Safety
/// `kp` / `vp` 分别指向 `kn` / `vn` 个可读字节。
#[no_mangle]
pub unsafe extern "C" fn asmr_doc_set(kp: *const u8, kn: usize, vp: *const u8, vn: usize) -> i32 {
    let (key, val) = match (str_arg(kp, kn), str_arg(vp, vn)) {
        (Ok(k), Ok(v)) => (k, v),
        _ => return fail("参数名或值不是 UTF-8 文本"),
    };
    let v: serde_json::Value = match serde_json::from_str(val) {
        Ok(v) => v,
        Err(e) => return fail(format!("{key} 的值不是 JSON：{e}")),
    };
    match with(|st| set_field(&mut st.doc, key, v)) {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

fn set_field(doc: &mut Saved, key: &str, v: serde_json::Value) -> Result<(), String> {
    let num = || v.as_f64().map(|x| x as f32).filter(|x| x.is_finite()).ok_or_else(|| format!("{key} 要一个数"));
    let p = &mut doc.params;
    match key {
        "dry" => doc.dry = v.as_bool().ok_or("dry 要 true / false")?,
        "style" => {
            *p = match v.as_u64() {
                Some(0) => p.clone().style_ear(),
                Some(1) => p.clone().style_ku100(),
                Some(2) => p.clone().style_earclean(),
                Some(3) => p.clone().style_breath(),
                _ => return Err("style 只能是 0、1、2、3".into()),
            };
            // 各风格的默认状态都是一个声音：按风格按钮时关掉左右双人（同桌面版）
            doc.duo_ms = 0.0;
        }
        "bg_preset" => match v.as_str() {
            Some("rain_night") => *p = p.clone().bg_rain_night(),
            Some("off") => p.bg_level = 0.0,
            _ => return Err("bg_preset 只能是 \"rain_night\" 或 \"off\"".into()),
        },
        "ear_cm" => p.set_ear_cm(num()?),
        "manual_cm" => p.set_manual_cm(num()?),
        "moves_per_min" => p.set_moves_per_min(num()?),
        "denoise" => p.set_denoise_slider(num()?),
        "mode" => {
            let m: MotionMode = serde_json::from_value(v).map_err(|e| format!("mode 不对：{e}"))?;
            p.set_mode(m);
        }
        "duo_ms" => doc.set_duo_ms(num()?), // 从 0 打开时排布切到一起移动（同桌面版）
        "mix_follow" => doc.mix_follow = v.as_bool().ok_or("mix_follow 要 true / false")?,
        "mix_balance" => doc.mix_balance = num()?.clamp(0.0, 100.0),
        _ => {
            // 其余参数按名字改：转成 JSON 对象改一项再转回来，类型不对时报错、不改
            let mut obj = serde_json::to_value(&*p).map_err(|e| e.to_string())?;
            let map = obj.as_object_mut().ok_or("参数不是对象")?;
            if !map.contains_key(key) {
                return Err(format!("没有这个参数：{key}"));
            }
            if v.as_f64().is_some_and(|x| !x.is_finite()) {
                return Err(format!("{key} 要一个有限的数"));
            }
            map.insert(key.to_string(), v);
            *p = serde_json::from_value(obj).map_err(|e| format!("{key} 的值不对：{e}"))?;
        }
    }
    Ok(())
}

/// 下拉框的选项（JSON）：`pinnas` = 真人耳廓的名字（"KU100" 之外的），`headphones` = 可补偿的耳机型号。
/// 第一次调用会解开 KU100 数据表（约 20 ms）。
#[no_mangle]
pub extern "C" fn asmr_lists() -> i32 {
    let v = serde_json::json!({
        "pinnas": pinnas().items.iter().map(|q| q.name.clone()).collect::<Vec<_>>(),
        "headphones": tables().headphones.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
    });
    put_out(v.to_string())
}

// ------------------------------------------------------------------ 引擎（AudioWorklet 用）

/// 预先解开 KU100 数据表、建好雨声用的数据（两样都只建一次）。第一次打开雨声要建约 80 ms，
/// 放在开始处理时做（那时还没出声），免得播放中打开雨声时在音频线程上卡一下。
#[no_mangle]
pub extern "C" fn asmr_warm_up() {
    let _ = tables();
    let _ = RainSetup::new(&Params { bg_level: 1.0, ..Params::default() }, None);
}

/// 用当前文档的参数生成引擎配置，所有处理器在下一次处理时换上（没变的部分直接复用）。
/// 第一次调用会解开 KU100 数据表。
#[no_mangle]
pub extern "C" fn asmr_engine_apply() {
    with(|st| {
        st.apply();
        st.ver += 1;
    });
}

/// 一个标签页的处理器：立体声下混成单声道 → `Engine::process`（左右双人时 [`LiveMix`]）→ 双耳；「监听原声」时输出
/// 原来的立体声，只乘输出增益、过峰值限制器（[`DryOut`]，音量滑条照样起作用；引擎照常在跑，切回效果时动作和响度是接着的）。
/// 只处理 48 kHz（插件的 AudioContext 固定 48 kHz）。
pub struct Processor {
    /// 单个声音的引擎（左右双人关着时用；一直跟着换配置，关掉左右双人时接着用）。
    eng: Engine,
    /// 左右双人打开时的两个声音。
    duo: Option<LiveMix>,
    ver: u64,
    dry: bool,
    /// 输入输出缓冲：[左入 | 右入 | 左出 | 右出]，各 `CAP` 帧。
    io: Box<[f32]>,
    mono: Box<[f32]>,
    tele: [f32; tele::N],
    /// 监听原声时的输出级，以及上一块是不是原声（刚切到原声时清掉它的旧状态）。
    dry_out: DryOut,
    was_dry: bool,
}

impl Processor {
    pub fn new() -> Self {
        let (cfg, duo, ver, dry) = with(|st| {
            if st.cfg.is_none() {
                st.apply();
            }
            (st.cfg.clone().unwrap(), st.duo.clone(), st.ver, st.dry)
        });
        let mut eng = Engine::new(cfg, PostMode::Realtime, 0.0);
        // 网页内容是连续的录音：动作严格跟着说话走（同桌面版试听、安卓版），不用主播模式的"停顿 1 秒恢复"
        eng.set_live(false);
        let duo = duo.map(|s| LiveMix::new(&s, false));
        Processor {
            eng,
            duo,
            ver,
            dry,
            io: vec![0.0; 4 * CAP].into_boxed_slice(),
            mono: vec![0.0; CAP].into_boxed_slice(),
            tele: [0.0; tele::N],
            dry_out: DryOut::new(FS_USIZE as f32),
            was_dry: false,
        }
    }

    /// 配置换过了就换上（只换 `Arc`）。左右双人开 / 关、换排布时重新建一份（会分配内存，只在用户改这两项的那一下）。
    fn sync(&mut self) {
        let next = with(|st| (st.ver != self.ver).then(|| (st.cfg.clone(), st.duo.clone(), st.ver, st.dry)));
        if let Some((cfg, duo, ver, dry)) = next {
            if let Some(cfg) = cfg {
                self.eng.set_config(cfg);
            }
            match (duo, &mut self.duo) {
                (Some(s), Some(d)) if d.mixer().is_follow() == (s.follow && s.cfgs.len() > 1) => d.set_setup(&s),
                (Some(s), _) => self.duo = Some(LiveMix::new(&s, false)),
                (None, _) => self.duo = None,
            }
            self.ver = ver;
            self.dry = dry;
        }
    }

    /// 处理 `io` 里的前 `n` 帧（最多 `CAP`）。
    pub fn process(&mut self, n: usize) {
        self.sync();
        let n = n.min(CAP);
        let (inl, rest) = self.io.split_at_mut(CAP);
        let (inr, rest) = rest.split_at_mut(CAP);
        let (outl, outr) = rest.split_at_mut(CAP);
        for i in 0..n {
            self.mono[i] = 0.5 * (inl[i] + inr[i]);
        }
        match &mut self.duo {
            Some(d) => d.process(&self.mono[..n], &mut outl[..n], &mut outr[..n]),
            None => self.eng.process(&self.mono[..n], &mut outl[..n], &mut outr[..n]),
        }
        if self.dry {
            outl[..n].copy_from_slice(&inl[..n]);
            outr[..n].copy_from_slice(&inr[..n]);
            if !self.was_dry {
                self.dry_out.reset();
            }
            let p = &self.eng.config().params;
            self.dry_out.process(&mut outl[..n], &mut outr[..n], p.out_gain_db, p.ceiling_db);
        }
        self.was_dry = self.dry;
        let (t, t2) = match &self.duo {
            Some(d) => (&d.mixer().engine(0).tele, Some(&d.mixer().engine(1).tele)),
            None => (&self.eng.tele, None),
        };
        self.tele[tele::AZ] = t.az;
        self.tele[tele::EL] = t.el;
        self.tele[tele::R] = t.r;
        self.tele[tele::ACT] = t.act;
        self.tele[tele::IN_DB] = t.in_db;
        self.tele[tele::OUT_DB] = t.out_db;
        self.tele[tele::DEPTH_CM] = t.depth_cm;
        self.tele[tele::DUO] = if t2.is_some() { 1.0 } else { 0.0 };
        if let Some(t2) = t2 {
            self.tele[tele::AZ2] = t2.az;
            self.tele[tele::EL2] = t2.el;
            self.tele[tele::R2] = t2.r;
            self.tele[tele::ACT2] = t2.act;
        }
        self.tele[tele::LATENCY_MS] = self.latency_ms();
    }

    pub fn latency_ms(&self) -> f32 {
        let s = self.duo.as_ref().map_or_else(|| self.eng.latency_samples(), |d| d.latency_samples());
        s as f32 * 1000.0 / FS_USIZE as f32
    }

    pub fn io(&mut self) -> &mut [f32] {
        &mut self.io
    }

    pub fn tele(&self) -> &[f32; tele::N] {
        &self.tele
    }
}

impl Default for Processor {
    fn default() -> Self {
        Self::new()
    }
}

/// 建一个处理器（第一次会生成引擎配置、解开数据表，约几十毫秒）。
#[no_mangle]
pub extern "C" fn asmr_proc_new() -> *mut Processor {
    Box::into_raw(Box::new(Processor::new()))
}

/// # Safety
/// `p` 来自 `asmr_proc_new`，之后不再使用。
#[no_mangle]
pub unsafe extern "C" fn asmr_proc_free(p: *mut Processor) {
    if !p.is_null() {
        drop(Box::from_raw(p));
    }
}

/// 一次最多处理多少帧。
#[no_mangle]
pub extern "C" fn asmr_proc_cap() -> usize {
    CAP
}

/// 输入输出缓冲的地址：4 × `CAP` 个 float，依次是左入、右入、左出、右出。
///
/// # Safety
/// `p` 来自 `asmr_proc_new` 且还没释放。
#[no_mangle]
pub unsafe extern "C" fn asmr_proc_io(p: *mut Processor) -> *mut f32 {
    (*p).io.as_mut_ptr()
}

/// 处理缓冲里的前 `frames` 帧（最多 `CAP`）。返回 0 = 已处理。
///
/// # Safety
/// `p` 来自 `asmr_proc_new` 且还没释放。
#[no_mangle]
pub unsafe extern "C" fn asmr_proc_process(p: *mut Processor, frames: usize) -> i32 {
    if p.is_null() {
        return -1;
    }
    (*p).process(frames);
    0
}

/// 状态数组有几个 float（[`tele::N`]）。
#[no_mangle]
pub extern "C" fn asmr_tele_len() -> usize {
    tele::N
}

/// 状态数组的地址（`tele::N` 个 float，各项见 [`tele`]），每次处理后更新。
///
/// # Safety
/// `p` 来自 `asmr_proc_new` 且还没释放。
#[no_mangle]
pub unsafe extern "C" fn asmr_proc_tele(p: *const Processor) -> *const f32 {
    (*p).tele.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out() -> String {
        with(|st| String::from_utf8(st.out.clone()).unwrap())
    }

    fn error() -> String {
        asmr_error();
        out()
    }

    fn load(text: &str) -> i32 {
        unsafe { asmr_doc_load(text.as_ptr(), text.len()) }
    }

    fn set(key: &str, val: &str) -> i32 {
        unsafe { asmr_doc_set(key.as_ptr(), key.len(), val.as_ptr(), val.len()) }
    }

    fn ui() -> serde_json::Value {
        assert!(asmr_doc_ui() > 0);
        serde_json::from_str(&out()).unwrap()
    }

    /// 桌面版导出的配置：读进来、改几项、再写出去，桌面版专用的设置原样保留。
    #[test]
    fn desktop_config_round_trip() {
        let desktop = Saved { buffer: 512, input: Some("Mic".into()), jobs: 3, params: Params { puff: 2.5, ..Params::default() }, ..Saved::default() };
        assert_eq!(load(&config::to_text(&desktop).unwrap()), 0);
        assert_eq!(ui()["params"]["puff"], 2.5);
        assert_eq!(set("dry", "true"), 0);
        assert_eq!(set("static_ambience", "true"), 0);
        assert_eq!(set("breath_deboom_db", "4.5"), 0);
        assert_eq!(set("breath_tone", "0.65"), 0);
        assert_eq!(set("breath_pause_switch", "true"), 0);
        assert_eq!(set("seed", "7"), 0);
        assert_eq!(set("pinna", "\"B\""), 0);
        assert!(asmr_doc_text() > 0);
        let back = config::parse(&out()).unwrap();
        assert!(back.params.static_ambience);
        assert_eq!(back.params.breath_deboom_db, 4.5);
        assert_eq!(back.params.breath_tone, 0.65);
        assert!(back.params.breath_pause_switch);
        assert!(back.dry && back.params.seed == 7 && back.params.pinna == "B" && back.params.puff == 2.5);
        assert!(back.buffer == 512 && back.input.as_deref() == Some("Mic") && back.jobs == 3);

        // 只写了参数的文件也能读；不是 auto-asmr 的、读不懂的报错且文档不变
        assert_eq!(load("(params: (seed: 3))"), 0);
        assert_eq!(ui()["params"]["static_ambience"], Params::default().static_ambience);
        assert_eq!(ui()["params"]["seed"], 3);
        assert_eq!(load("{\"other\": \"()\"}"), -1);
        assert!(error().contains("没有 auto-asmr"));
        assert_eq!(load("not ron"), -1);
        assert_eq!(ui()["params"]["seed"], 3);
    }

    /// 按名字改参数：类型不对、名字不对时报错且不改；换算值、风格、背景音预设与桌面版的方法一致。
    #[test]
    fn set_by_name_matches_desktop_helpers() {
        asmr_doc_reset_params();
        assert_eq!(set("hold_s", "4"), 0);
        assert_eq!(ui()["params"]["hold_s"], 4.0);
        assert_eq!(set("seed", "1.5"), -1);
        assert_eq!(set("nope", "1"), -1);
        assert!(error().contains("nope"));
        assert_eq!(set("room", "1"), -1);
        assert_eq!(set("room", "true"), 0);
        assert_eq!(set("playback", "{\"Headphone\": \"HD 650\"}"), 0);
        assert_eq!(ui()["params"]["playback"]["Headphone"], "HD 650");

        assert_eq!(set("ear_cm", "5"), 0);
        let d = ui();
        assert!((d["derived"]["ear_cm"].as_f64().unwrap() - 5.0).abs() < 1e-3);
        let mut want = Params::default();
        want.hold_s = 4.0;
        want.set_moves_per_min(20.0);
        assert_eq!(set("moves_per_min", "20"), 0);
        assert!((ui()["params"]["hold_s"].as_f64().unwrap() - want.hold_s as f64).abs() < 1e-4);

        assert_eq!(set("mode", "\"Orbit\""), 0);
        assert_eq!(ui()["params"]["level_follow"].as_f64().unwrap() as f32, Params::default_level_follow(MotionMode::Orbit));
        assert_eq!(set("mode", "\"Spin\""), -1);

        assert_eq!(set("denoise", "40"), 0);
        let d = ui();
        assert_eq!(d["params"]["denoise"], true);
        assert_eq!(d["derived"]["denoise"], 40.0);
        assert_eq!(set("denoise", "0"), 0);
        assert_eq!(ui()["params"]["denoise"], false);

        assert_eq!(set("style", "2"), 0);
        assert_eq!(ui()["params"]["inear_cm"].as_f64().unwrap() as f32, Params::default().style_earclean().inear_cm);
        assert_eq!(ui()["derived"]["style"], 2);
        let expected = with(|st| st.doc.params.clone().style_breath());
        assert_eq!(set("style", "3"), 0);
        assert_eq!(with(|st| st.doc.params.clone()), expected);
        assert_eq!(set("style", "5"), -1);
        // 风格按钮和恢复默认都关掉左右双人
        for style in ["0", "1", "2", "3"] {
            assert_eq!(set("duo_ms", "80"), 0);
            assert_eq!(set("style", style), 0);
            assert_eq!(with(|st| st.doc.duo_ms), 0.0, "style {style}");
        }
        assert_eq!(set("duo_ms", "80"), 0);
        asmr_doc_reset_params();
        assert_eq!(with(|st| st.doc.duo_ms), 0.0);
        assert_eq!(set("bg_preset", "\"rain_night\""), 0);
        assert!(ui()["derived"]["bg_db"].as_f64().is_some());
        assert_eq!(set("bg_preset", "\"off\""), 0);
        assert!(ui()["derived"]["bg_db"].is_null());

        assert!(asmr_lists() > 0);
        let l: serde_json::Value = serde_json::from_str(&out()).unwrap();
        assert!(!l["pinnas"].as_array().unwrap().is_empty());
        assert!(!l["headphones"].as_array().unwrap().is_empty());
    }

    /// 合成的"说话"：基频 140 Hz 左右的谐波，按音节开合、偶尔停顿；左右声道略有不同。
    fn speech(i: usize) -> (f32, f32) {
        use std::f32::consts::TAU;
        let t = i as f32 / FS_USIZE as f32;
        let syl = (TAU * 3.3 * t).sin().max(0.0) * if (TAU * 0.25 * t).sin() > -0.6 { 1.0 } else { 0.0 };
        let f0 = 140.0 + 20.0 * (TAU * 0.7 * t).sin();
        let ph = TAU * f0 * t;
        let s = 0.2 * syl * (ph.sin() + 0.5 * (2.0 * ph).sin() + 0.3 * (3.0 * ph).sin());
        (s, 0.9 * s)
    }

    /// 按 AudioWorklet 的方式（每次 128 帧）处理：有声音、两耳不一样；「监听原声」时原样输出；中途换配置不出错。
    /// 左右双人：设置写进文档、应用后处理器换成两个声音（一左一右，左右两边差不多一样响），界面读得到这几项；
    /// 改延迟只换配置，换排布时重新建，拖回 0 回到单个引擎；值不对时报错。
    #[test]
    fn processor_follows_the_two_voices_setting() {
        asmr_doc_reset_params();
        assert_eq!(set("auto_level", "false"), 0);
        assert_eq!(set("duo_ms", "80"), 0);
        assert_eq!(set("mix_balance", "50"), 0);
        assert_ne!(set("mix_follow", "1"), 0);
        let d = ui()["doc"].clone();
        assert_eq!((d["duo_ms"].as_f64(), d["mix_follow"].as_bool(), d["mix_balance"].as_f64()), (Some(80.0), Some(true), Some(50.0)), "打开左右双人时排布切到一起移动");
        // 改回各自固定（下面先看一左一右固定时的样子）；之后改延迟不会再切回一起移动
        assert_eq!(set("mix_follow", "false"), 0);
        asmr_engine_apply();
        let mut p = Processor::new();
        assert!(p.duo.is_some());
        let x = auto_asmr::dsp::testsig::test_voice(8.0);
        let (mut el, mut er) = (0f64, 0f64);
        for (j, c) in x.chunks_exact(128).enumerate() {
            let io = p.io();
            io[..128].copy_from_slice(c);
            io[CAP..CAP + 128].copy_from_slice(c);
            p.process(128);
            let io = p.io();
            if j * 128 > FS_USIZE * 2 {
                el += io[2 * CAP..2 * CAP + 128].iter().map(|v| (v * v) as f64).sum::<f64>();
                er += io[3 * CAP..3 * CAP + 128].iter().map(|v| (v * v) as f64).sum::<f64>();
            }
        }
        let t = *p.tele();
        assert_eq!(t[tele::DUO], 1.0);
        assert!(t[tele::AZ] > 180.0 && t[tele::AZ2] < 180.0, "1 号在左、2 号在右：{t:?}");
        assert!((10.0 * (el / er).log10()).abs() < 3.0, "左右两边应差不多一样响：{el} {er}");
        // 改延迟：同一份，延迟换成新的
        let before = p.duo.as_ref().map(|d| d as *const LiveMix);
        assert_eq!(set("duo_ms", "120"), 0);
        asmr_engine_apply();
        p.process(128);
        assert_eq!(p.duo.as_ref().map(|d| d as *const LiveMix), before);
        assert_eq!(p.duo.as_ref().unwrap().mixer().delays()[1], 5760);
        assert_eq!(ui()["doc"]["mix_follow"].as_bool(), Some(false), "只是改延迟，不切排布");
        // 一起移动：重新建；拖回 0：单个引擎
        assert_eq!(set("mix_follow", "true"), 0);
        asmr_engine_apply();
        p.process(128);
        assert!(p.duo.as_ref().unwrap().mixer().is_follow());
        assert_eq!(set("duo_ms", "0"), 0);
        asmr_engine_apply();
        p.process(128);
        assert!(p.duo.is_none() && p.tele()[tele::DUO] == 0.0);
        // 配置文件里存的就是桌面版的那几项
        asmr_doc_text();
        let back = config::parse(&out()).unwrap();
        assert!(back.mix_follow && back.mix_balance == 50.0 && back.duo_ms == 0.0);
        asmr_doc_reset_params();
        assert_eq!(set("mix_follow", "false"), 0);
        assert_eq!(set("mix_balance", "0"), 0);
    }

    #[test]
    fn processor_spatializes_and_monitors_dry() {
        asmr_doc_reset_params();
        asmr_engine_apply();
        let mut p = Processor::new();
        let secs = 8;
        let (mut el, mut er) = (0.0f64, 0.0f64);
        let mut lr_db_min = f64::MAX;
        let mut lr_db_max = f64::MIN;
        let mut i = 0;
        let mut win = (0.0f64, 0.0f64);
        while i < FS_USIZE * secs {
            let n = 128;
            {
                let io = p.io();
                for k in 0..n {
                    let (l, r) = speech(i + k);
                    io[k] = l;
                    io[CAP + k] = r;
                }
            }
            p.process(n);
            let io = p.io();
            for k in 0..n {
                let (l, r) = (io[2 * CAP + k] as f64, io[3 * CAP + k] as f64);
                assert!(l.is_finite() && r.is_finite());
                if i > FS_USIZE * 2 {
                    el += l * l;
                    er += r * r;
                    win.0 += l * l;
                    win.1 += r * r;
                }
            }
            i += n;
            // 每 0.5 秒看一次左右电平差：穿梭时应该时左时右
            if i % (FS_USIZE / 2) == 0 && i > FS_USIZE * 2 {
                if win.0 + win.1 > 1e-4 {
                    let d = 10.0 * ((win.0 + 1e-12) / (win.1 + 1e-12)).log10();
                    lr_db_min = lr_db_min.min(d);
                    lr_db_max = lr_db_max.max(d);
                }
                win = (0.0, 0.0);
            }
        }
        assert!(el + er > 1.0, "没有声音：{el} {er}");
        assert!(lr_db_max > 10.0 && lr_db_min < -10.0, "左右没有来回：{lr_db_min:.1} … {lr_db_max:.1} dB");
        assert!(p.tele()[tele::LATENCY_MS] > 0.0 && p.tele()[tele::LATENCY_MS] < 50.0);

        // 监听原声：输出就是输入（立体声原样，只晚限制器的约 2 ms）；输出增益照样起作用
        assert_eq!(set("dry", "true"), 0);
        asmr_engine_apply();
        let mut dry_block = |p: &mut Processor, i: usize| -> (Vec<f32>, Vec<f32>) {
            let io = p.io();
            for k in 0..128 {
                let (l, r) = speech(i + k);
                io[k] = l;
                io[CAP + k] = r;
            }
            p.process(128);
            let io = p.io();
            (io[2 * CAP..2 * CAP + 128].to_vec(), io[3 * CAP..3 * CAP + 128].to_vec())
        };
        let (mut gl, mut gr) = (Vec::new(), Vec::new());
        let i0 = i;
        for _ in 0..40 {
            let (l, r) = dry_block(&mut p, i);
            gl.extend(l);
            gr.extend(r);
            i += 128;
        }
        let k = p.dry_out.delay();
        for j in k..gl.len() {
            let (l, r) = speech(i0 + j - k);
            assert!((gl[j] - l).abs() < 1e-6 && (gr[j] - r).abs() < 1e-6, "原声第 {j} 个样本不对");
        }
        assert_eq!(set("out_gain_db", "-12"), 0);
        asmr_engine_apply();
        let (i1, mut ql) = (i, Vec::new());
        for _ in 0..40 {
            ql.extend(dry_block(&mut p, i).0);
            i += 128;
        }
        // 第一块里从 0 dB 过渡过去，之后每个样本都是原声的 −12 dB
        let g = 10f32.powf(-12.0 / 20.0);
        assert!(ql[k + 128..].iter().any(|v| v.abs() > 0.01), "这一段应该有声音");
        for j in k + 128..ql.len() {
            assert!((ql[j] - speech(i1 + j - k).0 * g).abs() < 1e-5, "原声时输出增益没起作用：第 {j} 个样本 {} ≠ {}", ql[j], speech(i1 + j - k).0 * g);
        }
        assert_eq!(set("out_gain_db", "0"), 0);

        // 换成掏耳 + 雨声，再处理一段：不出错、有声音
        assert_eq!(set("dry", "false"), 0);
        assert_eq!(set("style", "2"), 0);
        assert_eq!(set("bg_preset", "\"rain_night\""), 0);
        asmr_engine_apply();
        let mut e = 0.0f64;
        for b in 0..(FS_USIZE * 2 / 128) {
            {
                let io = p.io();
                for k in 0..128 {
                    let (l, r) = speech(i + b * 128 + k);
                    io[k] = l;
                    io[CAP + k] = r;
                }
            }
            p.process(128);
            let io = p.io();
            e += io[2 * CAP..2 * CAP + 128].iter().chain(&io[3 * CAP..3 * CAP + 128]).map(|x| (*x as f64).powi(2)).sum::<f64>();
        }
        assert!(e > 0.1, "换配置后没有声音：{e}");
    }
}
