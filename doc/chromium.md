# 浏览器插件（Chrome / Edge）

代码目录：仓库根目录的 [`auto-asmr-chromium/`](../auto-asmr-chromium/)。插件把某个标签页里的声音截下来，用和桌面版同一份 Rust 引擎（编译成 WebAssembly）实时处理成双耳 ASMR 效果，再播放出来，类似桌面版的实时（主播）模式，只是输入从麦克风换成了网页。配置文件和桌面版、安卓版通用。

支持 Chrome 116 及以上和其它 Chromium 内核的浏览器（Edge 等）。Firefox 没有标签页采集接口（`chrome.tabCapture`），不支持。

## 状态

2026-11-20：在 Chrome for Testing 158（macOS，无头模式）里装上插件做了端到端测试（见[测试](#测试)）：真采集一个正在播放的标签页，输出端实测左右来回、切到原声后就是原来的立体声，弹窗、设置页、角标、停止、换页面、关标签页都正常。

还没验证的：用户手势这一关（测试里用 Chrome 的测试开关跳过了"必须先点插件图标"的要求，真点图标、按快捷键要手动试）、戴耳机实听、声音比画面晚多少、Edge、Windows / Linux。

## 构建和安装

需要 Rust 和它的 wasm 目标：

```bash
rustup target add wasm32-unknown-unknown
```

```bash
cd auto-asmr-chromium && ./build.sh
```

把引擎编成 `extension/asmr.wasm`（约 17 MB，其中 15.7 MB 是内嵌的 KU100 / 耳廓数据）。然后在 Chrome 打开 `chrome://extensions`，打开右上角「开发者模式」，点「加载已解压的扩展程序」，选 `auto-asmr-chromium/extension/` 目录。改了 Rust 代码要重新 `./build.sh`，再在扩展页点这个插件的「重新加载」。

`./build.sh --zip` 另外打一个 `dist/auto-asmr-chromium.zip`，上传 Chrome 应用商店用。推送 tag 时 GitHub Actions 会自动打这个包（tag 是 `v1.2.3` 这样的数字版本时版本号跟着 tag），放进 GitHub Release；不想自己构建的话，下载它、解压，再按上面「加载已解压的扩展程序」选解压出来的目录。

## 使用

- **开始 / 停止**：在要处理的标签页上点插件图标，弹窗一打开就开始处理（这个标签页已经在处理时不重复开始）；弹窗最上面的开关关掉就停止、恢复原声，再打开又继续。每次点图标都会开始，关掉之后想改设置又不想处理，就在弹窗里再把开关关上。不打开弹窗也可以按快捷键 `Alt+Shift+A` 开始 / 停止（可以在 `chrome://extensions/shortcuts` 改）。在处理的标签页，插件图标上显示 `ON`。可以同时处理几个标签页。关掉标签页时自动停止。
- **弹窗**：处理开关、效果 / 原声切换、四个风格（贴耳加强、KU100 实录、掏耳、呼吸音；当前参数和哪个风格一致，那个按钮就亮着，设置页也一样；按风格按钮或设置页的「恢复默认」会关掉左右双人，同桌面版）、左右移动的方式、雨夜背景音和它的音量、左右双人（按钮开 / 关，开是 80 ms；滑条调第 2 个声音晚多少毫秒）；位置图实时显示声源在头的哪一侧、多远（左右双人时两个点：1 号橙色、2 号蓝色），下面是说话活动、输出电平、延时。
- **设置页**（弹窗右上角「全部参数 / 导入配置」）：桌面版的全部效果参数，分组、名字、范围与桌面版界面相同；数字框可以手输超出滑块范围的值。改动立即对正在处理的标签页生效。
- **链接**：弹窗底部和设置页标题栏右侧有本项目 GitHub（「给个star」）和研发人 B 站主页（「随意Official」）的链接，在新标签页打开（同桌面版右下角）。
- **呼吸去闷**在设置页的「贴耳感」里。呼吸声偏闷时可调整 100–300 Hz 能量堆积时的最大衰减，范围为 0–12 dB。
- 同一栏的「薄声补偿」可按录音频谱补充偏弱的气流低频、柔化偏亮的高频，范围为 0–1。选择「穿梭」时可勾选「换气时换边」，声源在一侧停够时间后等换气间隙再移动。
- **固定原声底噪**在设置页的「KU100 临场感」里。默认关闭，各风格都不开启，需要时手动打开；打开后，说话人走位时，录音中的持续环境声保持在固定的双耳声场。该项与生成雨声的「背景音」相互独立。
- **左右双人**（设置页第一组，关着时折叠；弹窗里也有开关）：和桌面版「文件转换」页的[左右双人](app-usage.md#左右双人一个音频当成两个人)是同一个设置（桌面版实时页的延迟另外记）——网页里的声音变成一左一右两个人，第 2 个晚设定的毫秒数（0–300，0 = 关）。打开后可以选**排布**（各自固定 / 一起移动 / 镜像；镜像是两个人左右对称、各守一边，见[多音频合成](app-usage.md#多音频合成)）和**自动平衡**（0–100 %，响度边听边算）。两个声音固定在左耳、右耳（桌面版的默认位置）。只有第 2 个声音晚，延时不变；开 / 关、换排布的那一下在音频线程上重新建两个引擎，改延迟时 20 ms 里淡过去。处理量约是平时的两倍。
- **配置文件**：设置页的「导入配置」读桌面版「导出配置」得到的 `.ron` 文件（也能读只写了几项的文件，缺的项用默认值）；「导出配置」写出同一格式的 `auto-asmr-export.ron`，可以导入桌面版和安卓版。插件用 `params`（效果参数）、`dry`（效果 / 原声）和左右双人（`duo_ms`、`mix_follow`、`mix_balance`），音频设备、缓冲、文件转换这些桌面版专用的设置原样保留。桌面版自检截图时留下的配置文件里没有 `app` 设置，导入会提示"没有 auto-asmr 的设置"（桌面版也一样）。

Chrome 自己的页面（`chrome://`、扩展商店）不能处理。用 `--mute-audio` 启动的 Chrome 采集到的是静音（实测）；在标签页上点「静音」大概也一样（没验证），要处理的标签页不要静音。动作严格跟着说话走（和桌面版试听、安卓版一样），不用主播模式里"停顿超过 1 秒恢复正常速度"的做法，因为网页内容是连续的录音。

## 做法

```
标签页 ──tabCapture──▶ offscreen 页面：MediaStreamSource ─▶ AudioWorkletNode（wasm 引擎）─▶ AudioContext 输出
                          ▲ streamId                         ▲ 配置（桌面版格式的文本）
后台 service worker ───────┴───────────────────────────────────┘
   ▲ 开始 / 停止、状态                ▲ chrome.storage.local 的 config 变了
弹窗 / 快捷键                       设置页 / 弹窗
```

- **截取声音**：用 `chrome.tabCapture.getMediaStreamId` 拿到标签页的流（要求用户刚点过插件图标或按过快捷键），在 offscreen 页面里用 `getUserMedia` 打开。标签页被采集后 Chrome 不再直接播放它，处理后的声音从 AudioContext 播出，听到的只有处理后的一份。`<video>`、MSE、HLS、网页自己的 Web Audio 都能截到，也不受跨域媒体的限制。
- **不用的做法**：在网页里注入脚本、用 `createMediaElementSource` 把 `<video>` 接进 Web Audio。跨域且没有 CORS 头的媒体会被判为"污染"、输出静音，跨域 iframe 和网页自己建的 AudioContext 也要逐个处理（安卓版调研时的方案 B，见[安卓版技术方案](android-plan.md#方案对比)）。
- **引擎**：`wasm/` 把 `auto-asmr` 库（`default-features = false`，只有 `dsp` 和 `config`）编成 `wasm32-unknown-unknown`，打开 SIMD。导出的是 C 函数，不用 wasm-bindgen：AudioWorklet 里没有 `TextDecoder` 这类接口，也只能同步实例化。字符串用"指针 + 长度"传。
- **AudioWorklet**：offscreen 页面在主线程编译 wasm 模块，经 `processorOptions` 交给 worklet，worklet 里同步实例化（主线程上不允许同步实例化超过 8 MB 的模块，worklet 里可以）。同一个 AudioContext 里的处理器共用一个 wasm 实例。每次 128 帧：左右下混成单声道 → `Engine::process`（左右双人时是共用库的 `LiveMix`：两个引擎，第 2 个从输入的历史里晚一点取）→ 双耳；「原声」时输出原来的立体声，引擎照常在跑，切回效果时动作和响度是接着的。AudioContext 固定 48 kHz（引擎内部的采样率），标签页的声音由 Chrome 重采样，不用自己做。
- **配置**：存在 `chrome.storage.local` 的 `config` 里，内容就是桌面版的配置文件文本。设置页和弹窗各自加载一个 wasm 实例，用 `Params` 自己的方法改参数（风格按钮、贴耳距离、移动频率、降噪滑条这些换算和桌面版完全一致），写回存储；后台看到变化就发给 offscreen 页面，worklet 生成新的引擎配置，处理器在下一块换上。
- **改参数不卡**：生成引擎配置在音频线程上做。开始处理时先把 KU100 数据表和雨声数据一次建好（约 30 ms，那时还没出声），之后改任何参数（包括第一次打开雨声）在音频线程上最多约 3.4 ms（实测，换耳机补偿那一项最慢），其余基本不到 1 ms。
- **出错不影响网页的声音**：wasm 里出错（panic）后，所有处理器改成直通，原因显示在弹窗里；之后新开始处理的标签页换一个新的 wasm 实例。
- **资源**：所有标签页都停了就关掉 offscreen 页面，释放 AudioContext 和 wasm 的内存。

## 延时

声音比画面晚：标签页采集的延时（Chrome 不报告；采集流自己报的 `latency` 是 9 ms）+ 引擎算法延时（默认 2.4 ms；开降噪再加 20 ms）+ AudioContext 和声卡的输出延时（弹窗里显示，`baseLatency + outputLatency`；这台 Mac 用默认输出设备时 21 ms）。插件没法让画面跟着推迟，实际差多少要对口型听。

## 性能

都在这台 Mac（Apple Silicon）上测：

| 项目 | 结果 |
|---|---|
| 引擎本身（wasm，每次 128 帧，60 秒合成人声） | 约占单核 2.2%（含 JS 生成测试信号）；原生版同样的基准 1.4–1.8% |
| 最慢的一块 128 帧 | 0.20 ms（一块的时长是 2.67 ms） |
| AudioWorklet 离线渲染 12 秒（含 Web Audio 本身） | 406 ms，实时的 30 倍 |
| 开始处理（编译 wasm、建 AudioContext、建处理器） | 约 90 ms |

x86 电脑上没测过。桌面版在 x86 上把非规格化数当 0 处理（防止静音时滤波器变慢），wasm 做不到这一点，静音段的处理占用可能会高一些。

## 测试

```bash
cd auto-asmr-chromium/wasm && cargo test --release
```

4 个测试（在电脑上原生运行）：桌面版导出的配置读进来、改几项、再写出去，桌面版专用的设置原样保留，读不懂的文件报错且不改；按名字改参数，类型不对、名字不对时报错，换算值、风格、背景音预设与桌面版的方法一致，按过的风格在界面状态里认得出来，四个风格按钮和恢复默认都关掉左右双人；按 AudioWorklet 的方式每次 128 帧处理时左右来回超过 ±10 dB、「原声」原样输出、中途换成掏耳 + 雨声后照常有声音；左右双人：设置写进文档、应用后处理器换成两个声音（1 号在左、2 号在右、左右两边差不多一样响），界面读得到这几项，改延迟只换配置、换排布时重新建（镜像时 1 号在左、2 号在右、左右对称，排布名不对时报错）、拖回 0 回到单个引擎，写出的配置文件里就是桌面版的那几项（镜像时 `mix_follow`、`mix_mirror` 都是 true）。

浏览器里的测试用开发服务器打开（先 `./build.sh`）：

```bash
python3 auto-asmr-chromium/tools/serve.py
```

- `http://127.0.0.1:8765/tools/worklet-test.html`（自动运行，14 项）：用 OfflineAudioContext 跑 `worklet.js` + `asmr.wasm`，和插件里的接法相同——默认参数有声音、左右来回穿梭（±60 dB）、按时报状态、处理速度；「原声」逐样本等于输入；处理中途经 port 换配置后立即生效；配置读不懂时报错并照常用默认参数；桌面版格式的掏耳配置声音只在一只耳朵里，加雨夜后另一只耳朵有雨声；左右双人时 1 号在左、2 号在右、两边差不多一样响，处理中途打开也立即生效；两个处理器共用一个 wasm 实例；改参数时音频线程上的耗时。
- `http://127.0.0.1:8765/tools/offscreen-test.html`（点「开始测试」，10 项）：把 `offscreen.js` 原样放进网页，`chrome.runtime` 和标签页采集换成假的（输入是一段合成人声），在实时 AudioContext 里按插件的顺序发 start → status → config → stop：声源在左右移动、换配置立即生效、停止后通知后台、停止后能再开始。默认不出声（AudioContext 用"无输出设备"），勾上「出声」才接到声卡。
- `http://127.0.0.1:8765/extension/options.html`、`/extension/popup.html`：插件界面。服务器会自动插入一个假的 `chrome.*`（`tools/chrome-shim.js`，配置存在 localStorage，弹窗的状态是假的），可以直接在普通标签页里改界面、试导入导出。

### 端到端（装进 Chrome）

```bash
python3 auto-asmr-chromium/tools/e2e_test.py --chrome "<Chrome for Testing 的可执行文件>"
```

Chrome for Testing 从 [Chrome for Testing](https://googlechromelabs.github.io/chrome-for-testing/) 下载（品牌版 Chrome 137 起不再支持 `--load-extension`）；macOS 上可执行文件是 `chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing`。脚本用临时用户目录启动它、装上插件，经 Chrome 调试协议驱动（只用 Python 标准库，自带一个最小的 websocket 客户端），默认无头、不出声（`--headful` 显示窗口，`--audible` 把处理后的声音接到声卡）。23 项：

- 开始处理（后台取 streamId、建 offscreen 页面、采集）后发声页在播放，AudioContext 在运行，引擎看到在说话、输入峰值约 −12 dB，声源 5 秒内转过 330°，角标 `ON`，没有报错；
- **输出端实测**（在 offscreen 页面的输出上接两个分析器）：左右电平差在 −60 … +69 dB 之间来回；切到「原声」后恒为 0.92 dB，正好是发声页左右声道之比；
- 写 `chrome.storage` 改配置后立即生效（固定到正前方）；设置页在插件环境里（CSP、wasm）能打开，点「掏耳」后存成桌面版格式，正在处理的标签页随即贴到耳边、伸进耳道；
- 标签页换页面后继续处理；`stop()` 后关掉 offscreen 页面、清掉角标；停止后能再开始；关掉正在处理的标签页后自动停止；
- 真弹窗（`chrome.action.openPopup()`）：一打开就自动开始处理、显示实时状态（方位、离头距离、入耳深度、延时），关掉开关后停止，再打开开关重新开始；把 `popup.html` 当成普通标签页打开时正常显示、不自动开始；
- 各页面（后台、offscreen、设置页、弹窗）没有报错。

做法上的几点：用户手势没法自动化，启动时加 `--allowlisted-extension-id=<插件 ID>`，这个插件不点图标也能采集，所以**真点图标 / 按快捷键这一关要手动验证**。不出声的办法：发声页等开始采集后才播放（被采集的标签页不在本地出声），offscreen 页面的 AudioContext 换成"无输出设备"；不能用 `--mute-audio`，它会让采集到的也是静音。

## 代码结构

| 路径 | 内容 |
|---|---|
| `build.sh` | 编译 wasm 并放进 `extension/`；`--zip` 打包 |
| `wasm/` | Rust 层：依赖 `auto-asmr` 库，导出配置文档（读写配置文件、按名字改参数）和处理器（每个标签页一个）两组 C 函数；`.cargo/config.toml` 打开 SIMD |
| `extension/manifest.json` | MV3 清单：权限 `tabCapture`、`offscreen`、`storage`、`activeTab`；允许 wasm 的 CSP；快捷键 |
| `extension/background.js` | 后台：建 / 关 offscreen 页面、取 streamId、转发消息、配置变化时通知、角标 |
| `extension/offscreen.html`、`offscreen.js` | 采集标签页、AudioContext、每个标签页一个 AudioWorkletNode、汇总状态 |
| `extension/worklet.js` | AudioWorklet：共用的 wasm 实例、处理器、配置、出错时直通 |
| `extension/asmr.js` | 插件页面用的 wasm 封装（配置文档）和存取 `chrome.storage` |
| `extension/popup.*`、`options.*`、`schema.js`、`style.css` | 弹窗、设置页、设置页的参数表（与桌面版界面对应） |
| `extension/icons/` | 图标（仓库根目录的 `icon/make_icons.py` 从 `icon/auto-asmr-icon.svg` 生成，三端共用一个图标） |
| `tools/serve.py`、`chrome-shim.js` | 开发服务器、假的 `chrome.*` |
| `tools/worklet-test.*`、`offscreen-test.*` | 浏览器里的测试页 |
| `tools/e2e_test.py`、`e2e-page.html` | 端到端测试（装进 Chrome for Testing、CDP 驱动）和它用的发声页 |

## 限制和没做的

- 界面只有中文（配置里的 `english` 不起作用）。
- 没有桌面版的位置图拖动摆位（手动模式可以在设置页用滑块摆）、3D 观察窗、录音。
- 没有让画面跟着声音推迟的办法（见[延时](#延时)）。
