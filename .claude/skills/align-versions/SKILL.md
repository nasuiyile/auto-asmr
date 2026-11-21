---
name: align-versions
description: auto-asmr 仓库里新增或调整音频预设（风格按钮如贴耳加强 / KU100 实录 / 掏耳 / 呼吸音，背景音预设如雨夜）、新增 / 改名 / 删除效果参数、改参数的范围 / 默认值 / 标签 / 说明时，用它保证所有版本对齐：桌面版界面和命令行、安卓版（网页模式和全局模式）、浏览器插件、Python 版，以及对应的测试和文档。只要改动碰到 auto-asmr/src/dsp/params.rs、gui.rs 的参数控件、cli.rs 的 --style、ui_spec.json、schema.js、popup.html / options.html 的风格按钮、asmr_binaural.py 的 STYLES，就要用这个技能，即使用户只说"加个预设""调一下掏耳""加个参数"，没提"对齐"。
---

# 各版本对齐：预设和参数

auto-asmr 有一份 Rust 引擎和四个版本（安卓版有网页、全局两种模式）。预设和参数的数值都在共用库里，但每个版本都有自己的界面层：按钮列表、参数表、编号映射、帮助文字。配置文件（`.ron`）在桌面版、安卓版、插件之间通用。所以只改一处，常见的后果是：某个版本看不到新按钮或新参数；导入别人的配置后行为不一样；按钮编号错位，点「掏耳」出来的是「呼吸音」。

做法：以共用库为准，把下面清单里每个版本都过一遍，跑对齐检查和各版本的测试，再同步文档。

## 版本地图

| 版本 | 预设和参数从哪来 | 要跟着改的地方 |
|---|---|---|
| **共用库**（基准） | `auto-asmr/src/dsp/params.rs`：`Params` 的字段（`#[serde(default)]`），`style_*`、`bg_*` 预设方法；配置文件格式在 `auto-asmr/src/config.rs` | — |
| 桌面版界面 | 调共用库的方法 | `auto-asmr/src/gui.rs`：风格按钮那一排（「恢复默认」旁边）、背景音预设按钮、右侧参数控件（标签和说明都写成 `t("中文", "English")`） |
| 桌面版命令行 | 调共用库的方法 | `auto-asmr/src/cli.rs`：`--style` 的取值和帮助文字、各参数的选项；文件转换用的目标响度常量（`EARCLEAN_LUFS`、`BREATH_LUFS` 这类，在 params.rs） |
| 安卓版（网页模式 + 全局模式） | `auto-asmr-android/rust`（依赖共用库）；全局模式的服务进程用同一个 Rust 层（`libasmrglobal.so`），参数表和网页模式共用，整份配置由 App 推过去 | `rust/src/settings.rs`：`action()` 的按钮名、`describe_config()` 的风格识别、换算项 `VIRTUAL` 和配置里 `params` 之外的 `DOC_KEYS`、`set_field` 的换算；`rust/src/lib.rs`：`asmr_set_style` 的编号；`rust/src/ui_spec.json`：`styles`（按钮）和 `groups`（参数表，标签、说明、范围、单位从 gui.rs 抄，另有 `label_en` / `tip_en`）；`app/src/main/cpp/asmr_core.h`、`NativeHook.kt` 注释里的按钮列表；`MainActivity.kt` 的 adb 风格列表；全局模式只有 `global/server/Engine.kt` 注释里的按钮列表要跟着改 |
| 浏览器插件 | `auto-asmr-chromium/wasm`（依赖共用库） | `wasm/src/lib.rs`：`change('style', 编号)`、`bg_preset`、`derived` 换算项；`extension/schema.js`：参数表；`extension/popup.html`、`options.html`：`data-style` 按钮 |
| Python 版 | `asmr_binaural.py` **独立实现**：`STYLES` 只有 `ear`、`ku100`，参数名也不同 | 新东西要么移植过去，要么在 `doc/python-cli.md` 和 `doc/python-vs-rust.md` 里写明"只有 Rust 版有" |

风格按钮的编号是 0 贴耳加强、1 KU100 实录、2 掏耳、3 呼吸音。安卓的 `asmr_set_style`、插件的 `data-style`、安卓 App 的 adb 列表都按编号对应，所以新风格加在末尾，已有的编号不要动。桌面版界面上的按钮顺序可以不同，它按名字调方法，不用编号。

## 新增一个预设

1. **共用库**：在 params.rs 加方法。风格预设从 `self.style_base()` 开始，只改和风格有关的字段：「按一下没有变化，就说明当前是这个风格」，安卓的 `describe_config` 就靠这一点认出配置文件是哪个风格。背景音预设只改背景音那几项（参考 `bg_rain_night`）。在 params.rs 的测试里加上风格互相切换、重复按的断言（已有的测试可以照着写）。
2. **桌面版**：gui.rs 加按钮和中英文提示；cli.rs 的 `--style` 加取值、写进帮助文字。文件转换要用不同的目标响度时，加一个常量。
3. **安卓版**：settings.rs 的 `action()` 和 `describe_config()`；风格预设还要在 lib.rs 的 `asmr_set_style` 末尾加编号；ui_spec.json 的 `styles` 加一项（`action`、`label`、`label_en`、`tip`、`tip_en`），背景音预设放进对应分组的 `actions`；更新 asmr_core.h、NativeHook.kt 注释里的按钮列表，以及 MainActivity.kt 的 adb 列表。
4. **插件**：wasm/src/lib.rs 的编号映射，popup.html 和 options.html 各加一个按钮，wasm 的测试里加上它。
5. **安卓全局模式**：`global/server/Engine.kt` 注释里的按钮列表。界面按 ui_spec.json 自动出现，服务进程的引擎跟着 Rust 层编进去，不用改。
6. **Python 版**：移植到 `STYLES`；做不到就写进文档，说明只有 Rust 版有。

## 新增或修改一个参数

1. **共用库**：给 `Params` 加字段，写上中文注释，并在 `Default` 里给默认值。整个结构是 `#[serde(default)]`，所以旧配置文件读得进来。**不要改字段名**：改了名，所有版本里的旧配置都会丢掉这一项。确实要改，就用 `#[serde(alias = "旧名")]` 兼容。然后在 `dsp/` 里接上它的效果。
2. **桌面版**：gui.rs 加控件（标签和说明写中英文、范围、单位）。文件转换时用得上的参数，cli.rs 加选项。
3. **安卓版**：ui_spec.json 加一项，标签、说明、范围、单位和 gui.rs 一字不差，再写 `label_en` / `tip_en`。界面上显示的是换算值时（比如厘米、次/分），在 settings.rs 的 `VIRTUAL` 和读写函数里加上换算，同时在检查脚本的 `VIRTUAL_COVERS` 里写清它覆盖哪个字段。
4. **插件**：schema.js 加一行，标签和范围同上。换算项标 `derived: true`，并在 wasm/src/lib.rs 里读写。
5. **安卓全局模式**：不用改（参数表、配置和网页模式共用），重新打包就行。
6. **Python 版**：Python 里有对应的算法就移植，没有就写进文档。
7. **改范围、默认值、标签**：gui.rs、ui_spec.json、schema.js 三处一起改。改默认值还会影响所有从 `style_base()` 派生的预设，以及文档里写的默认值。

## 只调整预设的数值

Rust 的四个版本调用的是同一个方法，数值通常只改 params.rs 一处。但还要找这些地方：

- **测试**：断言了预设数值的测试（params.rs、engine.rs、motion.rs、安卓 settings.rs、插件 wasm/src/lib.rs）。
- **说明文字**：引用了具体数字的说明，比如"嘴离耳约 1.7 cm""每只耳朵约 40 秒才换边"。这些写在 gui.rs 的提示、ui_spec.json 的 `styles[].tip`、popup.html / options.html 的 `title`、cli.rs 的帮助文字里。
- **文档**：`doc/effects.md` 里这个预设的参数和标定说明。
- **Python 版**：改的是 `ear` 或 `ku100` 时，决定 Python 版跟不跟。Python 的 `ku100` 本来就是另一套标定，文档里写着；不跟的话，确认文档里的说法仍然成立。

## 检查和测试

先跑对齐检查（只读文件，几秒钟）：

```bash
python3 .claude/skills/align-versions/scripts/check_alignment.py
```

它以 params.rs 为准，核对以下几项：

- **预设**：每个预设在 gui.rs、cli.rs、安卓 settings.rs / lib.rs / ui_spec.json / adb 列表、插件 wasm 里都有；插件的风格按钮个数等于风格个数。
- **参数**：每个参数在桌面版界面、安卓参数表、插件参数表里都有，换算项能对上。
- **标签和范围**：安卓参数表和插件参数表的标签、滑条范围一致，标签在 gui.rs 里找得到。

「没对齐的地方」要修，修完退出码为 0。「提示」多半是有意的差异（比如 Python 没有某个风格、某个参数没有命令行选项），确认一下即可。新增一处有意的差异时，把它写进脚本开头的 `KNOWN_*` 列表并注明原因，别让检查一直报。目录结构变了导致脚本找不到文件时，顺手更新脚本。

再跑各版本的测试，测试数量变了要更新文档：

```bash
cd auto-asmr && cargo test --release                  # 共用库和桌面版
cd auto-asmr-android/rust && cargo test --release     # 安卓 Rust 层，含 ui_spec_matches_params（参数表逐项对得上 Params）
cd auto-asmr-chromium/wasm && cargo test --release    # 插件
```

要出包或在界面上看效果时再构建：插件 `auto-asmr-chromium/build.sh`，安卓（含全局模式）`auto-asmr-android/./gradlew assembleDebug`。动了 Python 版时，至少跑一遍 `uv run asmr_binaural.py --help`，再用一小段音频试一下新选项。

## 文档

按 AGENTS.md，同一次改动里更新对应的 `doc/` 文件：

- `effects.md`：效果的做法、依据、预设的参数和标定。
- `app-usage.md`、`app-cli.md`：桌面版界面、命令行。
- `android-dev.md`：安卓版界面，Rust 层的测试个数和说明。
- `chromium.md`：插件的测试个数和说明。
- `shizuku.md`：安卓全局模式的行为变了时才改。
- `development.md`：桌面版的测试个数。
- `python-cli.md`、`python-vs-rust.md`：Python 版有没有这项、两版的差别。

`android-plan.md` 里"第 N 步结果"是当时的记录，不用改里面的数字。

## 收尾

- 告诉用户改了哪些版本、哪些地方是有意不同的（比如 Python 没跟），以及各版本测试和对齐检查的结果。
- 按 AGENTS.md 的收尾要求，看一下 git 的暂存区和未跟踪的文件：构建产物、本机配置这类文件应该被忽略。
