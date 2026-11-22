//! 文件转换页试听用的输出流（AAudio）：回调里调用共用库的 [`Pump::fill`]，和桌面版试听的声卡回调做的是同一件事。
//!
//! 打开流之后才知道设备的采样率，所以先开流、再按这个采样率建 [`Pump`]（重采样到设备采样率），最后才开始播放；
//! 回调在开始播放之前不会被调用。关掉时先停流、关流（AAudio 保证关流返回后回调不会再跑），再释放 `Pump`。
//! 输出设备断开（拔耳机等）时记进 [`Shared`]，界面读到后停掉试听，和桌面版设备出错时一样。

// 回调、上下文只在安卓上用得到（电脑上 `Output::start` 直接报错）
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::sync::Arc;

use auto_asmr::live::{Pump, Shared};

/// 一次回调最多处理多少帧；更长时分几段（缓冲事先分配好，回调里不分配）。
const MAX_FRAMES: usize = 4096;

struct Ctx {
    pump: Option<Pump>,
    shared: Arc<Shared>,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl Ctx {
    /// 写 `frames` 帧交错立体声到 `out`。
    fn render(&mut self, out: &mut [f32], frames: usize) {
        self.shared.out_frames.store(frames as u32, std::sync::atomic::Ordering::Relaxed);
        let Some(pump) = self.pump.as_mut() else {
            out.fill(0.0);
            return;
        };
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_FRAMES);
            pump.fill(&mut self.l[..n], &mut self.r[..n]);
            for i in 0..n {
                out[(done + i) * 2] = self.l[i];
                out[(done + i) * 2 + 1] = self.r[i];
            }
            done += n;
        }
    }
}

#[cfg(target_os = "android")]
mod sys {
    use std::ffi::{c_char, c_void};

    pub type Builder = c_void;
    pub type Stream = c_void;
    pub type DataCb = unsafe extern "C" fn(*mut Stream, *mut c_void, *mut c_void, i32) -> i32;
    pub type ErrorCb = unsafe extern "C" fn(*mut Stream, *mut c_void, i32);

    pub const FORMAT_PCM_FLOAT: i32 = 2;
    pub const USAGE_MEDIA: i32 = 1;
    pub const CONTENT_TYPE_MUSIC: i32 = 2;
    pub const PERFORMANCE_MODE_NONE: i32 = 10;
    pub const CALLBACK_CONTINUE: i32 = 0;

    #[link(name = "aaudio")]
    extern "C" {
        pub fn AAudio_createStreamBuilder(builder: *mut *mut Builder) -> i32;
        pub fn AAudioStreamBuilder_setFormat(b: *mut Builder, format: i32);
        pub fn AAudioStreamBuilder_setChannelCount(b: *mut Builder, n: i32);
        pub fn AAudioStreamBuilder_setSampleRate(b: *mut Builder, rate: i32);
        pub fn AAudioStreamBuilder_setUsage(b: *mut Builder, usage: i32);
        pub fn AAudioStreamBuilder_setContentType(b: *mut Builder, t: i32);
        pub fn AAudioStreamBuilder_setPerformanceMode(b: *mut Builder, m: i32);
        pub fn AAudioStreamBuilder_setDataCallback(b: *mut Builder, cb: DataCb, user: *mut c_void);
        pub fn AAudioStreamBuilder_setErrorCallback(b: *mut Builder, cb: ErrorCb, user: *mut c_void);
        pub fn AAudioStreamBuilder_openStream(b: *mut Builder, s: *mut *mut Stream) -> i32;
        pub fn AAudioStreamBuilder_delete(b: *mut Builder) -> i32;
        pub fn AAudioStream_requestStart(s: *mut Stream) -> i32;
        pub fn AAudioStream_requestStop(s: *mut Stream) -> i32;
        pub fn AAudioStream_close(s: *mut Stream) -> i32;
        pub fn AAudioStream_getSampleRate(s: *mut Stream) -> i32;
        pub fn AAudio_convertResultToText(r: i32) -> *const c_char;
    }

    pub fn text(r: i32) -> String {
        unsafe {
            let p = AAudio_convertResultToText(r);
            if p.is_null() {
                r.to_string()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }
}

/// 一条正在播放的输出流。drop 时停流、关流、释放 `Pump`。
pub struct Output {
    #[cfg(target_os = "android")]
    stream: *mut sys::Stream,
    ctx: *mut Ctx,
    pub out_rate: u32,
    pub name: String,
}

// 流和上下文只在打开 / 关闭时由持有者访问，回调线程只通过 ctx 指针访问（关流后不再访问）
unsafe impl Send for Output {}

#[cfg(target_os = "android")]
unsafe extern "C" fn data_cb(_s: *mut sys::Stream, user: *mut std::ffi::c_void, audio: *mut std::ffi::c_void, frames: i32) -> i32 {
    let ctx = &mut *(user as *mut Ctx);
    let n = frames.max(0) as usize;
    let out = std::slice::from_raw_parts_mut(audio as *mut f32, n * 2);
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.render(out, n))).is_err() {
        out.fill(0.0);
        ctx.pump = None; // 出过错：之后一直静音，界面读到错误后停掉
        ctx.shared.record_error("试听出错了".into());
    }
    sys::CALLBACK_CONTINUE
}

#[cfg(target_os = "android")]
unsafe extern "C" fn error_cb(_s: *mut sys::Stream, user: *mut std::ffi::c_void, err: i32) {
    let ctx = &*(user as *const Ctx);
    ctx.shared.record_error(format!("输出设备出错（{}），试听已停止", sys::text(err)));
}

impl Output {
    /// 打开默认输出设备（48 kHz 立体声 float，媒体用途），按设备的采样率用 `make` 建 `Pump`，开始播放。
    #[cfg(target_os = "android")]
    pub fn start(shared: Arc<Shared>, make: impl FnOnce(f64) -> Pump) -> Result<Output, String> {
        use sys::*;
        let ctx = Box::into_raw(Box::new(Ctx { pump: None, shared, l: vec![0.0; MAX_FRAMES], r: vec![0.0; MAX_FRAMES] }));
        let free_ctx = move || unsafe { drop(Box::from_raw(ctx)) };
        unsafe {
            let mut b: *mut Builder = std::ptr::null_mut();
            let r = AAudio_createStreamBuilder(&mut b);
            if r != 0 || b.is_null() {
                free_ctx();
                return Err(format!("打开输出失败（{}）", text(r)));
            }
            AAudioStreamBuilder_setFormat(b, FORMAT_PCM_FLOAT);
            AAudioStreamBuilder_setChannelCount(b, 2);
            AAudioStreamBuilder_setSampleRate(b, 48_000);
            AAudioStreamBuilder_setUsage(b, USAGE_MEDIA);
            AAudioStreamBuilder_setContentType(b, CONTENT_TYPE_MUSIC);
            AAudioStreamBuilder_setPerformanceMode(b, PERFORMANCE_MODE_NONE);
            AAudioStreamBuilder_setDataCallback(b, data_cb, ctx as *mut _);
            AAudioStreamBuilder_setErrorCallback(b, error_cb, ctx as *mut _);
            let mut s: *mut Stream = std::ptr::null_mut();
            let r = AAudioStreamBuilder_openStream(b, &mut s);
            AAudioStreamBuilder_delete(b);
            if r != 0 || s.is_null() {
                free_ctx();
                return Err(format!("打开输出失败（{}）", text(r)));
            }
            let rate = AAudioStream_getSampleRate(s).max(8000) as u32;
            (*ctx).pump = Some(make(rate as f64));
            let r = AAudioStream_requestStart(s);
            if r != 0 {
                AAudioStream_close(s);
                free_ctx();
                return Err(format!("开始播放失败（{}）", text(r)));
            }
            Ok(Output { stream: s, ctx, out_rate: rate, name: "默认输出".into() })
        }
    }

    /// 电脑上（测试、基准）没有 AAudio。
    #[cfg(not(target_os = "android"))]
    pub fn start(_shared: Arc<Shared>, _make: impl FnOnce(f64) -> Pump) -> Result<Output, String> {
        Err("这里没有音频输出（只在安卓上可以试听）".into())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        #[cfg(target_os = "android")]
        unsafe {
            sys::AAudioStream_requestStop(self.stream);
            sys::AAudioStream_close(self.stream); // 返回后回调不会再跑
        }
        unsafe { drop(Box::from_raw(self.ctx)) };
    }
}
