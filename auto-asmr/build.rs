//! 构建脚本：Windows 桌面版把 assets/icon.ico（戴耳机的白色小猫，由 icon/make_icons.py 生成）嵌进 exe 当文件图标。
//! 其它平台、只编库（安卓版、浏览器插件）时什么也不做。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(all(windows, feature = "desktop"))]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("嵌入 Windows 图标失败");
    }
}
