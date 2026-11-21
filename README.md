# auto-asmr：KU100 双耳 ASMR 空间化

把任意人声处理成"有人在你耳边 / 身边说话"的双耳效果，戴耳机听。基于 Neumann KU100 人工头的实测近场 HRIR。Rust 版另有"掏耳"预设，把棉签、刮擦这类触发音做成在耳朵里掏的效果。

本仓库有以下几个版本，算法相同：

- **Rust 版（[`auto-asmr/`](auto-asmr/)，推荐）**：带图形界面，同时支持实时（主播用，算法延迟 2.4 ms）和文件转换，Windows / macOS / Linux 通用。
- **Python 命令行版（`asmr_binaural.py`）**：只做文件转换，额外有几种运动模式和合成房间。
- **安卓版（[`auto-asmr-android/`](auto-asmr-android/)，开发中）**：内嵌浏览器，把网页里的声音实时做成双耳效果；有 Shizuku 或 root 时还能处理任意 App 的声音（[全局模式](doc/shizuku.md)，原型）。参数和配置文件与 Rust 版通用。下载 GitHub Release 里的 `auto-asmr-android.apk`。
- **浏览器插件（[`auto-asmr-chromium/`](auto-asmr-chromium/)）**：Chrome / Edge 插件，把标签页里的声音实时做成双耳效果。下载 GitHub Release 里的 `auto-asmr-chromium.zip`，解压后在扩展页「加载已解压的扩展程序」，详见[浏览器插件](doc/chromium.md)。

## 快速开始

Rust 版：从 GitHub Release 下载程序，或者自己构建：

```bash
cd auto-asmr && cargo build --release
```

Python 版（需要 [uv](https://docs.astral.sh/uv/)，第一次运行会自动下载约 16 MB 的 HRIR 数据）：

```bash
uv run asmr_binaural.py 输入.wav
```

必须用耳机听，外放听不出效果。

## 文档

详细说明都在 [`doc/`](doc/README.md)：

- [界面使用说明](doc/app-usage.md) · [命令行](doc/app-cli.md) · [构建、发布与开发](doc/development.md)
- [Python 命令行版](doc/python-cli.md) · [效果与依据](doc/effects.md) · [两版的关系](doc/python-vs-rust.md)
- [安卓版](doc/android-dev.md) · [安卓版技术方案](doc/android-plan.md) · [安卓版全局模式](doc/shizuku.md)
- [数据与许可](doc/data-license.md)

## 许可

HRIR、房间、耳机补偿数据来自 TH Köln，耳廓数据来自 TU Berlin HUTUBS，均为 CC BY 4.0，出处见[数据与许可](doc/data-license.md)。
