//! 安卓版的 Rust 层：把桌面版的引擎（`auto-asmr` 库里的 `dsp`）包成 C 接口，给 hook 层
//! （`app/src/main/cpp/aaudio_hook.c`）调用。声音全程不经过 Java。
//!
//! - **设置是全局的**（[`settings`]）：整份桌面版格式的配置，界面线程按字段改参数，在那里算好 `EngineConfig`、
//!   版本号加一；音频线程在每次回调开头看版本号，拿得到锁就换上，拿不到下次再说，绝不等待（同桌面版实时模式）。
//! - **每个 AAudio 流一个 [`Processor`]**：Chromium 给的是立体声 float，采样率跟着片源（48 kHz、44.1 kHz……）。
//!   左右下混成单声道 →（不是 48 kHz 时先重采样到 48 kHz）→ `Engine::process` →（再转回原采样率）→ 写回左右声道。
//!   缓冲在创建时按 [`CHUNK`] 分配好，回调里不再分配。
//! - **左右双人**（配置里的 `duo_ms` 大于 0）：同一路声音交给库里的 [`LiveMix`]——一个在左耳、一个在右耳，第 2 个
//!   从输入的历史里晚一点取（和桌面版主播模式同一套做法），自动平衡的输入响度边听边算。开 / 关、换排布时在音频线程里
//!   重新建一份（只在用户改这两项的那一下），延迟、位置、强度这些改动直接换配置。
//! - **出错不崩溃**：导出函数都 `catch_unwind`，Rust 里出错只会让这个流改成直通。

use std::ffi::{c_char, c_int, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, TryLockError};

use auto_asmr::dsp::engine::{Engine, PostMode};
use auto_asmr::dsp::mix::{LiveMix, MixSetup};
use auto_asmr::dsp::params::Params;
use auto_asmr::dsp::resample::Resampler;
use auto_asmr::dsp::FS_USIZE;

/// 一次最多处理多少输入帧；回调更长时分几段处理。
const CHUNK: usize = 1024;
/// 不是 48 kHz 时，输出端先垫多少帧静音（原采样率），保证两次重采样的延时攒够之前不会断流。
const PRIME: usize = 128;

pub mod aout;
pub mod files;
pub mod rain;
pub mod settings;
use settings::{current_cfg, update_params, with_global, CFG_VER, GLOBAL};

fn set_error(msg: String) {
    log(6, &msg);
    with_global(|g| g.error = msg);
}

/// C 字符串 → Rust 字符串（空指针当空串）。
unsafe fn from_c(s: *const c_char) -> String {
    if s.is_null() {
        String::new()
    } else {
        CStr::from_ptr(s).to_string_lossy().into_owned()
    }
}

/// 改参数的结果换成 C 的返回值：0 = 成功；失败时记下原因（`asmr_last_error` 取）。
fn report(r: std::thread::Result<Result<(), String>>) -> c_int {
    match r {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            set_error(e);
            -1
        }
        Err(_) => {
            set_error("Rust 里出错了".into());
            -1
        }
    }
}

/// 把 Rust 字符串写进 C 的缓冲（NUL 结尾，截断），返回完整长度（字节，不含 NUL）。
unsafe fn write_c(s: &str, buf: *mut c_char, cap: usize) -> c_int {
    if !buf.is_null() && cap > 0 {
        let n = s.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    s.len() as c_int
}

// ------------------------------------------------------------------ 安卓日志

#[cfg(target_os = "android")]
extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// 写安卓日志（标签 AsmrCore；prio：4 = INFO，6 = ERROR）。在电脑上跑测试时打到 stderr。
fn log(prio: c_int, msg: &str) {
    #[cfg(target_os = "android")]
    if let Ok(c) = std::ffi::CString::new(msg.replace('\0', " ")) {
        unsafe { __android_log_write(prio, c"AsmrCore".as_ptr(), c.as_ptr()) };
    }
    #[cfg(not(target_os = "android"))]
    eprintln!("[AsmrCore {prio}] {msg}");
}

/// 提供数据文件（`ku100.bin`、`pinna.bin` 的内容，App 从 APK 资源里读）。要在 `asmr_init` 之前调用一次。
/// 返回 0 = 成功，1 = 已经提供过。
///
/// # Safety
/// 两个指针各指向 `*_len` 字节（内容会被复制）。
#[no_mangle]
pub unsafe extern "C" fn asmr_provide_data(ku100: *const u8, ku100_len: usize, pinna: *const u8, pinna_len: usize) -> c_int {
    if ku100.is_null() || pinna.is_null() {
        return -1;
    }
    let ku = std::slice::from_raw_parts(ku100, ku100_len).to_vec();
    let pi = std::slice::from_raw_parts(pinna, pinna_len).to_vec();
    if auto_asmr::dsp::data::provide(ku, pi) {
        0
    } else {
        1
    }
}

/// 数据文件提供了没有（没提供时不建处理器，网页声音直通）。
#[no_mangle]
pub extern "C" fn asmr_data_ready() -> c_int {
    auto_asmr::dsp::data::provided() as c_int
}

/// 加载 KU100 数据表、生成默认配置；在后台线程里调用一次。返回 0 = 成功。
#[no_mangle]
pub extern "C" fn asmr_init() -> c_int {
    std::panic::set_hook(Box::new(|info| log(6, &format!("Rust panic：{info}"))));
    match catch_unwind(|| {
        let t = std::time::Instant::now();
        let _ = current_cfg();
        log(4, &format!("引擎初始化 {:.0} ms", t.elapsed().as_secs_f64() * 1000.0));
    }) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// 换一份参数（RON 格式的 `Params`，与桌面版配置文件里 `params` 那一段相同；缺的项用默认值）。
/// 返回 0 = 成功；失败时原因可用 `asmr_last_error` 取。
///
/// # Safety
/// `ron` 是 NUL 结尾的 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn asmr_set_params_ron(ron: *const c_char) -> c_int {
    if ron.is_null() {
        return -1;
    }
    let s = from_c(ron);
    report(catch_unwind(|| {
        let p = ron::from_str::<Params>(&s).map_err(|e| format!("参数读不懂：{e}"))?;
        update_params(|q| {
            *q = p;
            Ok(())
        })
    }))
}

/// 当前参数（RON），返回完整长度；`cap` 不够时截断。
///
/// # Safety
/// `buf` 可写 `cap` 字节（可以为空，只取长度）。
#[no_mangle]
pub unsafe extern "C" fn asmr_get_params_ron(buf: *mut c_char, cap: usize) -> c_int {
    let s = catch_unwind(|| with_global(|g| ron::ser::to_string_pretty(&g.saved.params, ron::ser::PrettyConfig::default()).unwrap_or_default()))
        .unwrap_or_default();
    write_c(&s, buf, cap)
}

/// 风格按钮：0 = 贴耳加强，1 = KU100 实录，2 = 掏耳，3 = 呼吸音。和桌面版一样，只改风格相关的那些参数。
#[no_mangle]
pub extern "C" fn asmr_set_style(style: c_int) -> c_int {
    let name = match style {
        0 => "style_ear",
        1 => "style_ku100",
        2 => "style_earclean",
        3 => "style_breath",
        _ => return -1,
    };
    report(catch_unwind(|| settings::run_action(name)))
}

/// 按钮：`style_ear` / `style_ku100` / `style_earclean` / `style_breath` / `reset` / `bg_rain_night` / `bg_off`。返回 0 = 成功。
///
/// # Safety
/// `name` 是 NUL 结尾的 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn asmr_action(name: *const c_char) -> c_int {
    let name = from_c(name);
    report(catch_unwind(|| settings::run_action(&name)))
}

/// 改一个参数：`key` 是 `Params` 的字段名或虚拟字段（`ear_cm`、`manual_cm`、`moves_per_min`、`denoise_slider`），
/// 或左右双人的设置（`duo_ms`、`mix_follow`、`mix_balance`，和桌面版配置文件同名）；`value` 是 JSON。
/// 返回 0 = 成功；失败时参数不变。
///
/// # Safety
/// `key`、`value` 是 NUL 结尾的 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn asmr_set_field(key: *const c_char, value: *const c_char) -> c_int {
    let (key, value) = (from_c(key), from_c(value));
    report(catch_unwind(|| {
        let v: serde_json::Value = serde_json::from_str(&value).map_err(|e| format!("{key} 的值读不懂：{e}"))?;
        if settings::DOC_KEYS.contains(&key.as_str()) {
            settings::update_doc(&key, v)
        } else {
            update_params(|p| settings::set_field(p, &key, v))
        }
    }))
}

/// 全部参数（JSON 对象，含虚拟字段和 `dry`），返回完整长度。
///
/// # Safety
/// `buf` 可写 `cap` 字节（可以为空，只取长度）。
#[no_mangle]
pub unsafe extern "C" fn asmr_get_params_json(buf: *mut c_char, cap: usize) -> c_int {
    let s = catch_unwind(|| settings::current_params_json().to_string()).unwrap_or_default();
    write_c(&s, buf, cap)
}

/// 输出效果 / 监听原声（存进配置文件的 `dry`，和桌面版共用）。
#[no_mangle]
pub extern "C" fn asmr_set_dry(dry: c_int) {
    let _ = catch_unwind(|| settings::set_dry(dry != 0));
}

/// 导入配置文件（桌面版或安卓版导出 / 保存的文件）：整份换掉。返回 0 = 成功。
///
/// # Safety
/// `text` 是 NUL 结尾的 UTF-8 字符串。
#[no_mangle]
pub unsafe extern "C" fn asmr_load_config(text: *const c_char) -> c_int {
    let text = from_c(text);
    report(catch_unwind(|| settings::load_config(&text)))
}

/// 只检查、不导入：`text` 是不是 auto-asmr 的配置文件。返回 0 = 是（`buf` 里是给确认弹窗看的概要），
/// 1 = 不是 auto-asmr 的配置（比如只有桌面版窗口状态的文件），2 = 读不懂（`buf` 里是原因）。不碰数据表，随时可以调。
///
/// # Safety
/// `text` 是 NUL 结尾的 UTF-8 字符串；`buf` 可写 `cap` 字节。
#[no_mangle]
pub unsafe extern "C" fn asmr_check_config(text: *const c_char, buf: *mut c_char, cap: usize) -> c_int {
    let text = from_c(text);
    match catch_unwind(|| settings::describe_config(&text)) {
        Ok(Ok(d)) => {
            write_c(&d, buf, cap);
            0
        }
        Ok(Err(auto_asmr::config::ConfigError::NotAutoAsmr)) => {
            write_c("文件里没有 auto-asmr 的配置", buf, cap);
            1
        }
        Ok(Err(auto_asmr::config::ConfigError::Parse(e))) => {
            write_c(&e, buf, cap);
            2
        }
        Err(_) => 2,
    }
}

/// 导出配置文件（和桌面版「导出配置」同一格式），返回完整长度；出错时返回 -1。
///
/// # Safety
/// 同 `asmr_get_params_json`。
#[no_mangle]
pub unsafe extern "C" fn asmr_export_config(buf: *mut c_char, cap: usize) -> c_int {
    match catch_unwind(settings::export_config) {
        Ok(Ok(s)) => write_c(&s, buf, cap),
        Ok(Err(e)) => {
            set_error(e);
            -1
        }
        Err(_) => -1,
    }
}

/// 界面用的参数表（JSON），返回完整长度。
///
/// # Safety
/// 同 `asmr_get_params_json`。
#[no_mangle]
pub unsafe extern "C" fn asmr_ui_spec(buf: *mut c_char, cap: usize) -> c_int {
    let s = catch_unwind(|| settings::ui_spec().to_string()).unwrap_or_default();
    write_c(&s, buf, cap)
}

/// 引擎此刻的位置（方位角、仰角、距离）→ 手动模式的方位角、离头中心距离（米）、高度（度），写进 out[0..3]。
///
/// # Safety
/// `out` 可写 3 个 float。
#[no_mangle]
pub unsafe extern "C" fn asmr_manual_from_pose(az: f32, el: f32, r: f32, out: *mut f32) {
    if out.is_null() {
        return;
    }
    let (a, d, t) = settings::manual_from_pose(az, el, r);
    let o = std::slice::from_raw_parts_mut(out, 3);
    (o[0], o[1], o[2]) = (a, d, t);
}

/// 最近一次出错的原因。
///
/// # Safety
/// 同 `asmr_get_params_ron`。
#[no_mangle]
pub unsafe extern "C" fn asmr_last_error(buf: *mut c_char, cap: usize) -> c_int {
    let s = with_global(|g| g.error.clone());
    write_c(&s, buf, cap)
}

// ------------------------------------------------------------------ 每个流的处理器

/// 给上报线程读的状态：音频线程每次回调后写，别的线程只读（都是原子量，单独分配，不和 `Processor` 共享引用）。
/// 左右双人时 `az`… 是 1 号声音，`duo`、`az2`… 是 2 号。
#[derive(Default)]
pub struct Tele {
    az: AtomicU32,
    el: AtomicU32,
    r: AtomicU32,
    act: AtomicU32,
    underruns: AtomicU32,
    failed: AtomicU32,
    duo: AtomicU32,
    az2: AtomicU32,
    el2: AtomicU32,
    r2: AtomicU32,
    act2: AtomicU32,
}

fn st(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

fn ld(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// 不是 48 kHz 的流：输入端升到 48 kHz，引擎处理后再降回原采样率，输出经一个小 FIFO 对齐到回调长度。
struct Rs {
    up: Resampler,
    down: [Resampler; 2],
    x48: Vec<f32>,
    q: [Vec<f32>; 2],
    qpos: usize,
    delay_frames: usize,
}

pub struct Processor {
    /// 单个声音的引擎（左右双人关着时用；一直跟着换配置，关掉左右双人时接着用）。
    eng: Engine,
    /// 左右双人打开时的两个声音。
    duo: Option<LiveMix>,
    ver: u64,
    rate: usize,
    rs: Option<Rs>,
    mono: Vec<f32>,
    l: Vec<f32>,
    r: Vec<f32>,
    failed: bool,
    tele: Arc<Tele>,
}

impl Processor {
    fn new(rate: usize) -> Self {
        let cfg = current_cfg();
        let mut eng = Engine::new(cfg, PostMode::Realtime, 0.0);
        // 网页内容是连续的录音：动作严格跟着说话走（同桌面版试听），不用主播模式的"停顿 1 秒恢复"
        eng.set_live(false);
        let duo = with_global(|g| g.duo.clone()).map(|s| LiveMix::new(&s, false));
        let n48 = if rate == FS_USIZE { CHUNK } else { CHUNK * FS_USIZE / rate + 64 };
        let rs = (rate != FS_USIZE).then(|| {
            let fs = FS_USIZE as f64;
            let qcap = 2 * (CHUNK + PRIME) + 512;
            let mut q = [Vec::with_capacity(qcap), Vec::with_capacity(qcap)];
            for v in q.iter_mut() {
                v.resize(PRIME, 0.0);
            }
            // 两次重采样各约半个插值核的延时（32 / 36 个样本的一半），换算到原采样率，加上垫的静音
            let delay_frames = PRIME + 16 * rate / FS_USIZE + 18;
            Rs { up: Resampler::new(rate as f64, fs), down: [Resampler::new(fs, rate as f64), Resampler::new(fs, rate as f64)], x48: Vec::with_capacity(n48), q, qpos: 0, delay_frames }
        });
        Processor { eng, duo, ver: CFG_VER.load(Ordering::Acquire), rate, rs, mono: vec![0.0; CHUNK], l: vec![0.0; n48], r: vec![0.0; n48], failed: false, tele: Arc::new(Tele::default()) }
    }

    /// 左右双人的配置换上：开 / 关、换排布时重新建一份（会分配内存，只在用户改这两项的那一下），其余直接换配置。
    fn sync_duo(&mut self, setup: Option<&Arc<MixSetup>>) {
        match (setup, &mut self.duo) {
            (Some(s), Some(d)) if d.mixer().is_follow() == (s.follow && s.cfgs.len() > 1) => d.set_setup(s),
            (Some(s), _) => self.duo = Some(LiveMix::new(s, false)),
            (None, _) => self.duo = None,
        }
    }

    /// 界面换了参数：拿得到锁就换上，拿不到下次再说。
    fn sync_config(&mut self) {
        let v = CFG_VER.load(Ordering::Acquire);
        if v == self.ver {
            return;
        }
        let guard = match GLOBAL.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        // 网页声音用不带背景音的配置（雨声是单独的一条输出流）
        let next = guard.as_ref().map(|g| (g.cfg_voice.clone(), g.duo.clone()));
        drop(guard);
        if let Some((cfg, duo)) = next {
            self.eng.set_config(cfg);
            self.sync_duo(duo.as_ref());
            self.ver = v;
        }
    }

    /// 处理一段交错的立体声（原地改写）。
    fn process(&mut self, data: &mut [f32]) {
        self.sync_config();
        let Processor { eng, duo, rs, mono, l, r, tele, .. } = self;
        // 左右双人打开时交给两个声音，否则单个引擎
        let mut run = |x: &[f32], l: &mut [f32], r: &mut [f32]| match duo {
            Some(d) => d.process(x, l, r),
            None => eng.process(x, l, r),
        };
        let n = data.len() / 2;
        let mut done = 0;
        while done < n {
            let m = (n - done).min(CHUNK);
            let block = &mut data[done * 2..(done + m) * 2];
            for (i, v) in mono[..m].iter_mut().enumerate() {
                *v = 0.5 * (block[2 * i] + block[2 * i + 1]);
            }
            match rs {
                None => {
                    run(&mono[..m], &mut l[..m], &mut r[..m]);
                    for i in 0..m {
                        block[2 * i] = l[i];
                        block[2 * i + 1] = r[i];
                    }
                }
                Some(rs) => {
                    rs.x48.clear();
                    rs.up.process(&mono[..m], &mut rs.x48);
                    let k = rs.x48.len().min(l.len());
                    run(&rs.x48[..k], &mut l[..k], &mut r[..k]);
                    if rs.qpos > 0 {
                        for q in rs.q.iter_mut() {
                            q.drain(..rs.qpos);
                        }
                        rs.qpos = 0;
                    }
                    rs.down[0].process(&l[..k], &mut rs.q[0]);
                    rs.down[1].process(&r[..k], &mut rs.q[1]);
                    let avail = rs.q[0].len().min(rs.q[1].len());
                    let take = m.min(avail);
                    for i in 0..take {
                        block[2 * i] = rs.q[0][i];
                        block[2 * i + 1] = rs.q[1][i];
                    }
                    if take < m {
                        block[2 * take..].fill(0.0);
                        tele.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    rs.qpos = take;
                }
            }
            done += m;
        }
        let (t, t2) = match duo {
            Some(d) => (d.mixer().engine(0).tele, Some(d.mixer().engine(1).tele)),
            None => (eng.tele, None),
        };
        st(&tele.az, t.az);
        st(&tele.el, t.el);
        st(&tele.r, t.r);
        st(&tele.act, t.act);
        st(&tele.duo, if t2.is_some() { 1.0 } else { 0.0 });
        if let Some(t2) = t2 {
            st(&tele.az2, t2.az);
            st(&tele.el2, t2.el);
            st(&tele.r2, t2.r);
            st(&tele.act2, t2.act);
        }
    }

    fn latency_ms(&self) -> f32 {
        let algo = self.duo.as_ref().map_or_else(|| self.eng.latency_samples(), |d| d.latency_samples()) as f32 / FS_USIZE as f32;
        let rs = self.rs.as_ref().map_or(0.0, |rs| rs.delay_frames as f32 / self.rate as f32);
        (algo + rs) * 1000.0
    }
}

/// 给一个新打开的流建处理器（在打开流的线程里调用，不在音频回调里）。只处理立体声；
/// 采样率 8–192 kHz。不支持时返回空指针，调用方直通。
#[no_mangle]
pub extern "C" fn asmr_proc_new(sample_rate: c_int, channels: c_int) -> *mut Processor {
    if channels != 2 || !(8_000..=192_000).contains(&sample_rate) || !auto_asmr::dsp::data::provided() {
        return std::ptr::null_mut();
    }
    catch_unwind(|| Box::into_raw(Box::new(Processor::new(sample_rate as usize)))).unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// `p` 来自 `asmr_proc_new`，之后不再使用（包括它的 `Tele`）。
#[no_mangle]
pub unsafe extern "C" fn asmr_proc_free(p: *mut Processor) {
    if !p.is_null() {
        drop(Box::from_raw(p));
    }
}

/// 在音频回调里调用：原地处理 `frames` 帧交错立体声 float。返回 0 = 已处理；-1 = 出过错，
/// 之后这个流一律直通（数据不动）。
///
/// # Safety
/// `p` 来自 `asmr_proc_new`；`data` 指向 `frames * 2` 个 float；同一个 `p` 不能被两个线程同时处理。
#[no_mangle]
pub unsafe extern "C" fn asmr_proc_process(p: *mut Processor, data: *mut f32, frames: c_int) -> c_int {
    if p.is_null() || data.is_null() || frames <= 0 {
        return -1;
    }
    let p = &mut *p;
    if p.failed {
        return -1;
    }
    let buf = std::slice::from_raw_parts_mut(data, frames as usize * 2);
    match catch_unwind(AssertUnwindSafe(|| p.process(buf))) {
        Ok(()) => 0,
        Err(_) => {
            p.failed = true;
            p.tele.failed.store(1, Ordering::Relaxed);
            -1
        }
    }
}

/// 这个流比原声晚多少毫秒（引擎算法延时 + 重采样）。在建好处理器后、开始回调前调用。
///
/// # Safety
/// `p` 来自 `asmr_proc_new`，此时没有别的线程在用它。
#[no_mangle]
pub unsafe extern "C" fn asmr_proc_latency_ms(p: *const Processor) -> f32 {
    if p.is_null() {
        return 0.0;
    }
    (*p).latency_ms()
}

/// 取出处理器的状态句柄（在建好处理器后、开始回调前调用），给上报线程用 `asmr_tele_read` 读。
/// 句柄随 `asmr_proc_free` 一起失效。
///
/// # Safety
/// 同 `asmr_proc_latency_ms`。
#[no_mangle]
pub unsafe extern "C" fn asmr_proc_tele(p: *const Processor) -> *const Tele {
    if p.is_null() {
        return std::ptr::null();
    }
    Arc::as_ptr(&(*p).tele)
}

/// 读状态：out[0..6] = 方位角（度）、仰角（度）、距离（米）、说话活动（0–1）、断流次数、是否出过错；
/// out[6..11] = 是不是左右双人（0 / 1），以及 2 号声音的方位角、仰角、距离、说话活动（左右双人时前面那 4 项是 1 号）。
///
/// # Safety
/// `t` 来自 `asmr_proc_tele` 且处理器还没释放；`out` 可写 11 个 float。
#[no_mangle]
pub unsafe extern "C" fn asmr_tele_read(t: *const Tele, out: *mut f32) {
    if t.is_null() || out.is_null() {
        return;
    }
    let t = &*t;
    let o = std::slice::from_raw_parts_mut(out, 11);
    o[0] = ld(&t.az);
    o[1] = ld(&t.el);
    o[2] = ld(&t.r);
    o[3] = ld(&t.act);
    o[4] = t.underruns.load(Ordering::Relaxed) as f32;
    o[5] = t.failed.load(Ordering::Relaxed) as f32;
    o[6] = ld(&t.duo);
    o[7] = ld(&t.az2);
    o[8] = ld(&t.el2);
    o[9] = ld(&t.r2);
    o[10] = ld(&t.act2);
}

// ------------------------------------------------------------------ 背景雨声（单独的输出流）

/// 建雨声渲染器（在开输出流的线程里调用）。数据还没提供时返回空指针。
#[no_mangle]
pub extern "C" fn asmr_rain_new() -> *mut rain::RainOut {
    if !auto_asmr::dsp::data::provided() {
        return std::ptr::null_mut();
    }
    catch_unwind(|| Box::into_raw(Box::new(rain::RainOut::new()))).unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// `p` 来自 `asmr_rain_new`，之后不再使用。
#[no_mangle]
pub unsafe extern "C" fn asmr_rain_free(p: *mut rain::RainOut) {
    if !p.is_null() {
        drop(Box::from_raw(p));
    }
}

/// 在输出流的回调里调用：生成 `frames` 帧交错立体声（48 kHz）。返回 1 = 还有声音，0 = 没声音了（可以停流），
/// -1 = 出错（写入静音）。
///
/// # Safety
/// `p` 来自 `asmr_rain_new`；`data` 指向 `frames * 2` 个 float；同一个 `p` 不能被两个线程同时用。
#[no_mangle]
pub unsafe extern "C" fn asmr_rain_render(p: *mut rain::RainOut, data: *mut f32, frames: c_int) -> c_int {
    if p.is_null() || data.is_null() || frames <= 0 {
        return -1;
    }
    let buf = std::slice::from_raw_parts_mut(data, frames as usize * 2);
    match catch_unwind(AssertUnwindSafe(|| (*p).render(buf))) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => {
            buf.fill(0.0);
            -1
        }
    }
}

/// 关掉（淡出）/ 又打开。只改一个原子标志，渲染器在下一次回调里处理，所以随时可以调。
#[no_mangle]
pub extern "C" fn asmr_rain_set_on(_p: *mut rain::RainOut, on: c_int) {
    rain::set_wanted(on != 0);
}

// ------------------------------------------------------------------ 文件转换页

/// 文件转换页的命令（JSON，UTF-8，`len` 字节），返回整页状态（JSON，UTF-8）：指针和长度写在 `out_len`，
/// 用完交给 [`asmr_free_bytes`]。出错时返回空指针。命令见 [`files`]。
///
/// # Safety
/// `cmd` 指向 `len` 字节；`out_len` 可写。
#[no_mangle]
pub unsafe extern "C" fn asmr_files(cmd: *const u8, len: usize, out_len: *mut usize) -> *mut u8 {
    let text = if cmd.is_null() { String::new() } else { String::from_utf8_lossy(std::slice::from_raw_parts(cmd, len)).into_owned() };
    match catch_unwind(AssertUnwindSafe(|| files::handle(&text))) {
        Ok(s) => {
            let b = s.into_bytes().into_boxed_slice();
            *out_len = b.len();
            Box::into_raw(b) as *mut u8
        }
        Err(_) => {
            set_error("文件转换页出错了".into());
            *out_len = 0;
            std::ptr::null_mut()
        }
    }
}

/// 释放 [`asmr_files`] 返回的字节。
///
/// # Safety
/// `p`、`len` 来自 [`asmr_files`]，只释放一次。
#[no_mangle]
pub unsafe extern "C" fn asmr_free_bytes(p: *mut u8, len: usize) {
    if !p.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, len)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: usize, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f32 / rate as f32;
            let s = 0.2 * (std::f32::consts::TAU * 300.0 * t).sin() * (0.5 - 0.5 * (std::f32::consts::TAU * 3.0 * t).cos());
            v.push(s);
            v.push(s);
        }
        v
    }

    /// 44.1 kHz 的流：输出长度不变、不断流、有声音、两耳不一样（被空间化了）、频率没跑偏。
    #[test]
    fn resampled_stream_stays_continuous() {
        let _g = settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        settings::provide_repo_data();
        settings::load_config("()").unwrap(); // 固定为默认单声源，避免并行测试或上一项测试的预设影响声道差。
        for rate in [44_100usize, 48_000, 32_000] {
            let mut p = Processor::new(rate);
            let mut x = tone(rate, 6.0);
            // 回调长度各不相同，模拟 Chromium 的回调
            let mut pos = 0;
            for (k, len) in [4360usize, 192, 1, 2048, 960, 3000].iter().cycle().enumerate() {
                if pos >= x.len() / 2 || k > 10_000 {
                    break;
                }
                let n = (*len).min(x.len() / 2 - pos);
                p.process(&mut x[pos * 2..(pos + n) * 2]);
                pos += n;
            }
            assert_eq!(p.tele.underruns.load(Ordering::Relaxed), 0, "{rate} Hz 断流");
            let tail = &x[x.len() / 2..];
            let (mut el, mut er, mut zc) = (0.0f64, 0.0f64, 0usize);
            for i in 0..tail.len() / 2 {
                el += (tail[2 * i] as f64).powi(2);
                er += (tail[2 * i + 1] as f64).powi(2);
                if i > 0 && tail[2 * i - 2] <= 0.0 && tail[2 * i] > 0.0 {
                    zc += 1;
                }
            }
            assert!(el + er > 1e-3, "{rate} Hz 没有声音");
            assert!((10.0 * (el / er).log10()).abs() > 1.0, "{rate} Hz 两耳一样响：{el} {er}");
            // 后 3 秒 300 Hz 约 900 个过零（空间化的滤波会多出一些，但不会差一倍）
            assert!((600..1300).contains(&zc), "{rate} Hz 过零 {zc}");
        }
    }

    #[test]
    fn params_ron_round_trip_and_style() {
        let _g = settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        settings::provide_repo_data();
        let mut buf = vec![0 as c_char; 1 << 16];
        let n = unsafe { asmr_get_params_ron(buf.as_mut_ptr(), buf.len()) } as usize;
        assert!(n > 100 && n < buf.len());
        assert_eq!(unsafe { asmr_set_params_ron(buf.as_ptr()) }, 0);
        assert_eq!(asmr_set_style(1), 0);
        assert_eq!(unsafe { asmr_set_params_ron(c"(seed: 7)".as_ptr()) }, 0);
        assert_eq!(current_cfg().params.seed, 7);
        assert_eq!(unsafe { asmr_set_params_ron(c"(seed: \"x\")".as_ptr()) }, -1);
    }

    fn set_field(key: &str, value: &str) -> c_int {
        let (k, v) = (std::ffi::CString::new(key).unwrap(), std::ffi::CString::new(value).unwrap());
        unsafe { asmr_set_field(k.as_ptr(), v.as_ptr()) }
    }

    fn tele_of(p: &Processor) -> [f32; 11] {
        let mut t = [0f32; 11];
        unsafe { asmr_tele_read(Arc::as_ptr(&p.tele), t.as_mut_ptr()) };
        t
    }

    /// 左右双人：打开后同一路声音变成一左一右两个声音（状态里有 2 号的位置），左右两边差不多一样响；
    /// 播放中改延迟只换配置，换排布时重新建，拖回 0 回到单个引擎；44.1 kHz 的流照样不断流。
    #[test]
    fn duo_follows_the_settings_while_playing() {
        let _g = settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        settings::provide_repo_data();
        update_params(|p| {
            *p = Params { auto_level: false, ..Params::default() };
            Ok(())
        })
        .unwrap();
        assert_eq!(set_field("duo_ms", "80"), 0);
        assert_eq!(set_field("mix_balance", "\"x\""), -1, "类型不对时报错");
        let json = settings::current_params_json();
        assert_eq!((json["duo_ms"].as_f64(), json["mix_follow"].as_bool()), (Some(80.0), Some(false)));
        for rate in [48_000usize, 44_100] {
            let mut p = Processor::new(rate);
            assert!(p.duo.is_some());
            let voice = auto_asmr::dsp::testsig::test_voice(8.0 * rate as f32 / 48_000.0);
            let mut x: Vec<f32> = voice.iter().flat_map(|&v| [v, v]).collect();
            for c in x.chunks_mut(2 * 960) {
                p.process(c);
            }
            let t = tele_of(&p);
            assert_eq!(t[6], 1.0, "{rate} Hz：状态里应是左右双人");
            assert!(t[0] > 180.0 && t[7] < 180.0, "{rate} Hz：1 号在左、2 号在右：{t:?}");
            assert_eq!(p.tele.underruns.load(Ordering::Relaxed), 0, "{rate} Hz 断流");
            let tail = &x[x.len() / 4..];
            let (el, er) = tail.chunks(2).fold((0f64, 0f64), |(a, b), s| (a + (s[0] as f64).powi(2), b + (s[1] as f64).powi(2)));
            assert!((10.0 * (el / er).log10()).abs() < 3.0, "{rate} Hz：左右两边应差不多一样响：{el} {er}");
        }
        let mut p = Processor::new(48_000);
        let mut buf = vec![0f32; 2 * 4800];
        p.process(&mut buf);
        // 改延迟：还是同一份（只换配置，延迟淡过去）
        let before = p.duo.as_ref().map(|d| d as *const LiveMix);
        assert_eq!(set_field("duo_ms", "120"), 0);
        p.process(&mut buf);
        assert_eq!(p.duo.as_ref().map(|d| d as *const LiveMix), before);
        assert_eq!(p.duo.as_ref().unwrap().mixer().delays()[1], 5760);
        // 换成一起移动：重新建
        assert_eq!(set_field("mix_follow", "true"), 0);
        p.process(&mut buf);
        assert!(p.duo.as_ref().unwrap().mixer().is_follow());
        // 拖回 0：回到单个引擎
        assert_eq!(set_field("duo_ms", "0"), 0);
        p.process(&mut buf);
        assert!(p.duo.is_none());
        assert_eq!(tele_of(&p)[6], 0.0);
        settings::update_doc("mix_follow", serde_json::json!(false)).unwrap();
        update_params(|p| {
            *p = Params::default();
            Ok(())
        })
        .unwrap();
    }

    /// 网页声音里没有雨声：打开雨声、再改别的参数之后，处理器换上的仍是去掉背景音的配置（雨声只在单独的输出流里），
    /// 左右双人时也一样。
    #[test]
    fn web_audio_never_gets_the_rain() {
        let _g = settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        settings::provide_repo_data();
        for duo in ["0", "80"] {
            assert_eq!(set_field("duo_ms", duo), 0);
            let mut p = Processor::new(48_000);
            update_params(|q| {
                *q = Params { bg_level: 80.0, ..Params::default() };
                Ok(())
            })
            .unwrap();
            update_params(|q| settings::set_field(q, "presence_db", serde_json::json!(5.0))).unwrap();
            let mut buf = vec![0f32; 2 * 48_000 * 2];
            for c in buf.chunks_mut(2 * 960) {
                c.fill(0.0);
                p.process(c);
            }
            let peak = buf[buf.len() / 2..].iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!(peak < 1e-4, "左右双人 {duo} ms：网页没有声音时输出了 {peak}（混进了雨声）");
        }
        assert_eq!(set_field("duo_ms", "0"), 0);
        update_params(|p| {
            *p = Params::default();
            Ok(())
        })
        .unwrap();
    }

    /// 单独输出的雨声：响度和桌面版引擎里的一样（同一个式子），关掉后淡出、停下；网页声音的配置里没有雨声。
    #[test]
    fn standalone_rain_matches_the_engine_and_fades_out() {
        let _g = settings::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        settings::provide_repo_data();
        update_params(|p| {
            *p = Params { bg_level: 60.0, rain_near: 0.0, rain_drip: 0.0, ..Params::default() };
            Ok(())
        })
        .unwrap();
        assert!(current_cfg().derived.rain.is_none(), "网页声音的配置里不该有雨声");
        let mut r = rain::RainOut::new();
        let mut buf = vec![0f32; 48_000 * 2 * 6];
        for c in buf.chunks_mut(4360 * 2) {
            assert!(r.render(c));
        }
        let (l, rr): (Vec<f32>, Vec<f32>) = buf[48_000 * 2..].chunks(2).map(|x| (x[0], x[1])).unzip();
        let got = auto_asmr::dsp::testsig::lufs(&l, &rr);
        let p = settings::full_cfg().params.clone();
        // 和桌面版引擎测试（realtime_engine_adds_rain_and_removes_it_when_turned_off）同一个期望值
        let want = p.target_lufs + p.bg_db().unwrap() + 10.0 * p.rain / 100.0 - 5.0;
        assert!((got - want).abs() < 2.0, "雨声 {got:.1} LUFS，应约 {want:.1}");
        rain::set_wanted(false);
        let mut alive = true;
        for _ in 0..200 {
            let mut c = vec![0f32; 960 * 2];
            alive = r.render(&mut c);
            if !alive {
                break;
            }
        }
        assert!(!alive, "关掉后应当淡完停下");
        update_params(|p| {
            *p = Params::default();
            Ok(())
        })
        .unwrap();
    }
}
