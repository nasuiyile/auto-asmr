//! auto-asmr 的库：桌面版程序（`src/main.rs`）和安卓版（`auto-asmr-android/rust`）共用——信号处理 `dsp`，
//! 以及两边通用的设置文件格式 `config`；实时引擎的处理核心 `live`（试听、主播模式）；文件转换 `offline`、`batch`
//! （`files` 功能，桌面版和安卓版都打开）。

pub mod config;
pub mod dsp;
pub mod live;
pub mod spsc;
#[cfg(feature = "files")]
pub mod batch;
#[cfg(feature = "files")]
pub mod offline;
