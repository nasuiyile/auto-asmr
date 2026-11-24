//! 信号处理核心：与 Python 版 `asmr_binaural.py` 同一套算法，改写成可以逐块流式运行的形式，
//! 实时模式和文件模式共用同一个 [`engine::Engine`]。

pub mod align;
pub mod ambience;
pub mod balance;
pub mod analysis;
pub mod direct;
pub mod dynamics;
pub mod engine;
pub mod filters;
pub mod mix;
pub mod motion;
pub mod params;
pub mod pinna;
pub mod rain;
pub mod resample;
pub mod rng;
pub mod solo;
pub mod room;
pub mod tables;
pub mod testsig;
pub mod thunder;
pub mod tone;
pub mod voice;

/// 数据文件（`assets/ku100.bin`：KU100 HRIR 表、房间、耳机补偿；`assets/pinna.bin`：真人耳廓）。
/// 默认编进程序里；打开 `external-data` 时改成运行时由调用方用 [`data::provide`] 提供——安卓版从 APK 资源里读，
/// 免得每个 CPU 架构的 .so 各带一份 15.7 MB。第一次用到数据表之前必须提供，否则 panic。
pub mod data {
    #[cfg(not(feature = "external-data"))]
    pub(crate) fn ku100() -> &'static [u8] {
        include_bytes!("../../assets/ku100.bin")
    }

    #[cfg(not(feature = "external-data"))]
    pub(crate) fn pinna() -> &'static [u8] {
        include_bytes!("../../assets/pinna.bin")
    }

    #[cfg(feature = "external-data")]
    static EXT: std::sync::OnceLock<(&'static [u8], &'static [u8])> = std::sync::OnceLock::new();

    /// 提供两个数据文件的内容（只能提供一次；返回 false = 已经提供过）。
    #[cfg(feature = "external-data")]
    pub fn provide(ku100: Vec<u8>, pinna: Vec<u8>) -> bool {
        EXT.set((Box::leak(ku100.into_boxed_slice()), Box::leak(pinna.into_boxed_slice()))).is_ok()
    }

    /// 数据文件提供了没有。
    #[cfg(feature = "external-data")]
    pub fn provided() -> bool {
        EXT.get().is_some()
    }

    #[cfg(feature = "external-data")]
    pub(crate) fn ku100() -> &'static [u8] {
        EXT.get().expect("数据文件还没提供（先调用 dsp::data::provide）").0
    }

    #[cfg(feature = "external-data")]
    pub(crate) fn pinna() -> &'static [u8] {
        EXT.get().expect("数据文件还没提供（先调用 dsp::data::provide）").1
    }
}

/// 内部处理采样率（HRIR 数据的采样率）。
pub const FS: f32 = 48_000.0;
pub const FS_USIZE: usize = 48_000;
/// 声速 m/s。
pub const C: f32 = 343.0;
/// 球头半径（与 KU100 耳间距一致），米。
pub const HEAD_A: f32 = 0.0875;
/// 声源到头中心的最小 / 最大距离，米。
pub const R_MIN: f32 = 0.095;
pub const R_MAX: f32 = 3.0;
/// 贴耳轻语时嘴到头中心的距离（离耳朵约 1.7 cm）。
pub const EAR_R: f32 = 0.105;
/// 入耳（掏耳）：物体还在耳道口时，它与话筒之间那段"耳甲 + 耳道"的等效长度（米）。
/// 两段 KU100 掏耳实录里，棉签在最外面时共振约 3 kHz（四分之一波长：343 / (4 × 0.029) ≈ 2.96 kHz）。
pub const CANAL_L0: f32 = 0.029;
/// 物体伸到最里面时与话筒之间最短还剩多长（米）：0.6 cm ≈ 14 kHz。
pub const CANAL_L_MIN: f32 = 0.006;
/// 入耳深度的上限（厘米）。
pub const INEAR_MAX_CM: f32 = (CANAL_L0 - CANAL_L_MIN) * 100.0;
/// 最小相位 HRIR 长度。
pub const NTAP: usize = 128;
/// 控制块长度：每 64 个样本（1.33 ms）更新一次位置和滤波器。
pub const CTRL: usize = 64;
/// 一个控制块的时长，秒。
pub const CTRL_DT: f32 = CTRL as f32 / FS;

#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-10).log10()
}

#[inline]
pub fn smoothstep(s: f32) -> f32 {
    let s = s.clamp(0.0, 1.0);
    s * s * (3.0 - 2.0 * s)
}

/// x86 上把"非规格化数"当 0 处理（FTZ / DAZ）：输入静音时，滤波器的状态会衰减成极小的数，
/// 不处理的话这类数的运算会慢几十倍，实时线程可能因此爆音。ARM 上硬件直接处理，不需要。
#[cfg(target_arch = "x86_64")]
pub fn flush_denormals() {
    let mut csr: u32 = 0;
    unsafe {
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr |= 0x8040; // FTZ | DAZ
        std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, readonly));
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn flush_denormals() {}
