# auto-asmr（Rust 版，带界面）

把任意人声变成"有人在你耳边说话"的双耳效果，戴耳机听；另有"掏耳"预设，把棉签、刮擦这类触发音做成在耳朵里掏的效果，和给音乐用的"听歌"预设。

- **实时**：麦克风 → 效果 → 耳机 / 虚拟声卡，给主播用。纯 CPU，算法延迟 2.4 ms。
- **文件转换**：选一段或一批音频，输出处理后的 WAV；可以边试听边调参数，也可以把几个音频合成到头边不同位置，或者把一个音频变成左右两个人；可以加上实时生成的背景雨声（雨夜）。

## 运行

- **Windows**：双击 `auto-asmr.exe`。直播时把输出设备选成虚拟声卡（如 VB-Cable）。
- **macOS**：运行 `tools/bundle_macos.sh` 生成 `target/release/auto-asmr.app`，双击启动（打包后才会弹出麦克风授权）。
- **Linux**：解压后运行 `./auto-asmr`，需要桌面图形环境和 ALSA 运行库。

不带参数启动是图形界面，`auto-asmr --help` 查看命令行用法。设置自动保存在启动时所在目录的 `auto-asmr.ron`，右上角可以导入 / 导出配置。

## 构建

需要 Rust 工具链（<https://rustup.rs>）：

```bash
cargo build --release
```

程序在 `target/release/auto-asmr`，数据都内嵌在里面，单个文件即可运行。

## 文档

详细说明在仓库根目录的 [`doc/`](../doc/README.md)（发布压缩包里是 `doc/` 文件夹）：[界面使用说明](../doc/app-usage.md)、[命令行](../doc/app-cli.md)、[构建、发布与开发](../doc/development.md)、[效果与依据](../doc/effects.md)、[数据与许可](../doc/data-license.md)。
