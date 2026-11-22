//! auto-asmr：KU100 双耳 ASMR 空间化。一个程序两个功能——实时（主播用）和音频文件转换。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod gui;
mod realtime;

// 信号处理、设置文件格式、实时处理核心、文件转换在库里（src/lib.rs），这里引进来，其余模块照旧用 `crate::dsp::…` 等
use auto_asmr::{batch, config, dsp, live, offline, spsc};

/// Windows 图形程序默认没有控制台：从命令行带参数启动时，接到父进程的控制台上才能看到输出。
#[cfg(windows)]
fn attach_console() {
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    unsafe {
        AttachConsole(0xFFFF_FFFF);
    }
}

#[cfg(not(windows))]
fn attach_console() {}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // 不带参数：图形界面。--screenshot <png> [--file-tab] [--demo <音频>]：打开界面、截一张图后退出（自检用）
    let shot = args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1)).map(|p| {
        let demo = args.iter().position(|a| a == "--demo").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
        (std::path::PathBuf::from(p), args.iter().any(|a| a == "--file-tab"), demo)
    });
    if args.is_empty() || shot.is_some() {
        if let Err(e) = gui::run(shot) {
            attach_console();
            eprintln!("界面启动失败：{e}");
            std::process::exit(1);
        }
        return;
    }
    attach_console();
    std::process::exit(cli::run(args));
}
