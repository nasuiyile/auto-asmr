# AGENTS.md

给在本仓库工作的 AI 助手和协作者的约定。

## 文档

- `README.md`（仓库根目录和 `auto-asmr/README.md`）只保留必要的说明：项目是什么、怎么运行 / 构建、文档在哪、许可概要。不写详细用法、参数表、实测数据、论文依据。
- 详细文档统一放在仓库根目录的 `doc/`，每个主题一个文件，并在 `doc/README.md` 的目录里登记：
  - `app-usage.md`：Rust 版界面
  - `app-cli.md`：Rust 版命令行
  - `development.md`：构建、发布、测试、代码结构
  - `python-cli.md`：Python 版用法
  - `effects.md`：效果的做法、依据和文献
  - `python-vs-rust.md`：两版的关系
  - `data-license.md`：数据来源与许可
- 改了功能、参数、界面或测试数量，同一次改动里更新对应的 `doc/` 文件；README 只在"必要说明"变了时才改。
- 新增主题时在 `doc/` 新建文件，并加进 `doc/README.md`；文档之间用相对链接。
- 文档用中文。
- 发布工作流（`.github/workflows/release.yml`）会把 `auto-asmr/README.md` 和 `doc/` 一起打进压缩包；移动或改名文档时同步检查它。

## 版本对齐

- 新增或调整音频预设（风格、背景音预设）、新增 / 改名 / 删除效果参数、改参数的范围 / 默认值 / 标签时，所有版本都要对齐：桌面版界面和命令行、安卓版、浏览器插件、global-asmr、Python 版（做不到的写进文档）。
- 要改哪些文件、怎么检查、跑哪些测试，见 `.claude/skills/align-versions/SKILL.md`；对齐检查脚本：`python3 .claude/skills/align-versions/scripts/check_alignment.py`。

## 收尾

- 每次结束任务前看一下 `git status`（暂存区和未跟踪的文件）：构建产物（`build/`、`target/`、`.cxx/`、`.gradle/`、APK、wasm 产物）、本机配置（`local.properties` 等）、下载的数据和试听文件不该出现在里面。新目录在提交前用 `git add -n <目录>` 预览一遍会加进哪些文件。
- 发现该忽略却没忽略的，补进对应目录的 `.gitignore`（或根目录的），不要提交它们；已经被跟踪的要提醒用户。
