# 安卓版技术方案

代码目录：仓库根目录的 [`auto-asmr-android/`](../auto-asmr-android/)。本文是 2026-11 的调研结论和进度：第 1–4 步（hook 验证、接入引擎、浏览器和效果面板、后台播放和发布）都已在模拟器上通过，结果见[第 1 步](#第-1-步验证结果)、[第 2 步](#第-2-步结果)、[第 3 步](#第-3-步结果)、[第 4 步](#第-4-步结果)。还没在真机上测过。界面怎么用、构建和验证工具见[安卓版开发](android-dev.md)。

## 目标

- 带一个内嵌浏览器。哪个网页在发声，就把它的声音实时送进 auto-asmr 引擎，输出处理后的双耳声音（类似桌面版的实时模式，只是输入从麦克风换成了网页）。
- 保留 Rust 版的核心选项：三个风格预设、全部效果参数、降噪、手动摆位和位置图、背景雨声、配置导入 / 导出（与桌面版 `.ron` 通用）、效果 / 原声对比。
- 去掉麦克风实时（主播）模式，以及音频设备、缓冲这类桌面设置（由系统决定）。

## 结论

**用系统自带的 Android WebView（它本身就是 Chromium）当浏览器，在 App 自己的进程里用 PLT hook 截住 WebView 的 AAudio 数据回调。** Chromium 每填好一块声音，先在原地交给 Rust 引擎处理，再交还系统播放。Rust 引擎（和桌面版是同一份 `dsp` 代码）编成静态库链进 hook 层，由 C 直接调用，声音全程不经过 Java；界面用 Kotlin + Jetpack Compose，参数经 JNI 传给 Rust。

这样不用自己编译 Chromium、不用录屏授权，网页用 `<video>`、MSE、HLS、Web Audio 中的哪一种都能截到。48 kHz 的流额外延迟只有引擎本身的 2.4 ms，44.1 kHz 的流加上重采样约 6 ms。

## 为什么可行：WebView 的声音怎么出来

以下依据 Chromium main 分支源码（2026-11 核对）：

| 事实 | 出处 |
|---|---|
| WebView 的"浏览器进程"就是宿主 App 的进程。网页在单独的沙箱渲染进程里解码、混音，PCM 经共享内存送回浏览器进程，由那里的 audio service 输出 | [WebView 架构](https://chromium.googlesource.com/chromium/src/+/main/android_webview/docs/architecture.md)；`content/public/common/content_features.cc` 中 `kAudioServiceOutOfProcess` 只在 Windows / macOS / Linux 默认开启，安卓上 audio service 在浏览器进程内运行 |
| 输出用 AAudio：`kUseAAudioDriver` 默认开（只有电视 HDMI 棒会关掉，改用 OpenSL ES） | `media/audio/audio_features.cc`；`media/audio/android/audio_manager_android.cc` 的 `MakeLowLatencyOutputStream` / `MakeLinearOutputStream` |
| AAudio 用回调模式、32 位浮点：`setFormat(AAUDIO_FORMAT_PCM_FLOAT)`、`setDataCallback(OnAudioDataRequestedCallback, this)`，打开后 CHECK 格式和声道数；`setFramesPerDataCallback` 固定每次回调的帧数（可变长回调 `kAAudioVariableSizedCallbacks` 默认关） | `media/audio/android/aaudio_stream_wrapper.cc` |
| libaaudio 是直接链接的（`libs += [ "aaudio" ]`），不是 dlopen / dlsym。所以 WebView 的 so（`libmonochrome_64.so` 或 `libwebviewchromium.so`）调用 `AAudioStreamBuilder_setDataCallback` 要经过 PLT / GOT，PLT hook 能截到 | `media/audio/BUILD.gn` |
| `AudioTrackOutputStream` 只用于 AC3 / E-AC3 直通（接 HDMI 功放时），耳机场景不走 | `audio_manager_android.cc` 的 `MakeBitstreamOutputStream` |
| 用系统 MediaPlayer 播 HLS 的旧路径已经删除（`media_player_bridge.cc`、`media_player_renderer.cc` 都不在了），HLS 由内置 demuxer 解码，同样走 AAudio | `media/media_options.gni` 的 `enable_hls_demuxer` |

所以最终调用系统音频接口的就是我们自己的进程，回调里拿到的是 Chromium 已经混好、即将播放的那一块 float PCM。

## 第 1 步验证结果

2026-11-20 在模拟器上（Android 16，arm64，`com.google.android.webview` 133.0.6943.137）用验证 App 跑通，日志由 [`tools/hook_test.sh`](../auto-asmr-android/tools/hook_test.sh) 收集：

| 出声方式 | 截到的流 | 改写 |
|---|---|---|
| Web Audio（左 0.3 × 440 Hz，右 0.1 × 660 Hz） | 48 kHz 2 声道 float | 改写前左 −13.5、右 −23.0 dB（与设定振幅完全吻合）；左右互换后对调，只留左耳时右耳 −120 dB，静音时两耳 −120 dB，直通时前后相同 |
| `<audio>` 播放页面生成的 WAV | 48 kHz 2 声道 float | 同上 |
| `<video>` 跨域 mp4 直链（没有 CORS 头，5.1 声道 AAC） | 44.1 kHz 2 声道 float（Chromium 先下混成立体声） | 左右互换正确。这种媒体方案 B 拿到的是静音，hook 不受影响 |
| `<video src=.m3u8>` 原生 HLS | 44.1 kHz 2 声道 float | 正确 |
| hls.js（MSE） | 44.1 kHz 2 声道 float | 正确 |
| 真实网站：B 站视频页（桌面版 UA，MSE / DASH） | 48 kHz 2 声道 float | 正确 |

- 进程里只有 WebView 的 `libmonochrome_64.so` 调用这几个 AAudio 函数，三个 hook 都一次成功；App 启动时登记、WebView 之后才加载 so 也能补上。
- 流的格式都是 float 立体声，但**采样率跟着片源走**：Web Audio 和 48 kHz 素材是 48 kHz，媒体文件是 44.1 kHz。接引擎时必须在回调里重采样（见下面的"声音格式"）。
- 模拟器上每次回调 4360 帧（48 kHz 时约 91 ms），性能模式 NONE；真机上的回调大小要实测，引擎本身任意长度都能处理。
- `WebViewCompat.setAudioMuted(web, true)` 之后，这个 WebView 的流**立刻不再回调**，约 5 秒后流被关掉；取消静音时重新开一个新流。所以用它让其它标签页不出声是可行的。
- 有的流打开后一次回调都没有就关了（B 站页面加载时），无害。
- 网站方面的两个问题（正式版的浏览器要处理）：B 站移动版看到 UA 里的 `; wv`（WebView 标记）就只引导下载 App，还会跳转到 `bilibili://` 唤起链接。验证 App 默认去掉 `; wv`、拦掉非 http(s) 跳转，并支持桌面版 UA；用桌面版 UA 打开 B 站视频页可以直接播放。

还没验证的：真机（尤其国产 ROM 自带的 WebView）、WebView 新版本（模拟器镜像里是 133，真机上通常更新）、DRM 内容、耳机实听。

## 第 2 步结果

2026-11-20 把桌面版的引擎接进了 hook 回调，同样在模拟器上跑通：

- **结构**：`auto-asmr` 拆成了"库 + 程序"（库只有 `dsp`，桌面依赖放进默认开启的 `desktop` feature，桌面版 87 个测试照常通过）。安卓的 Rust 层 `auto-asmr-android/rust` 编成静态库，`CMakeLists.txt` 在构建时调用 cargo 并把它链进 `libasmrhook.so`，hook 层用 C 直接调用它的 C 接口（`app/src/main/cpp/asmr_core.h`），不需要 `jni` crate 和 cargo-ndk。APK 21 MB，其中 `libasmrhook.so` 17.4 MB（含内嵌的 KU100 / 耳廓数据 15.7 MB）。
- **每个流一个处理器**：流打开时（不在音频回调里）建好，左右下混成单声道送进 `Engine::process`，写回双耳；不是 48 kHz 的流先升到 48 kHz、处理后降回原采样率。回调里不分配内存；参数更新和桌面版一样用"锁 + 版本号"，音频线程拿不到锁就下次再换。Rust 里出错（panic）只会让这个流改成直通，App 不崩。
- **五种出声方式 + B 站真实视频都正常**：没有断流，AAudio 也没报 xrun。48 kHz 的流延迟 2.4 ms，44.1 kHz 的流 6.0 ms。
- **44.1 kHz 重采样正确**：录下 440 / 660 Hz 测试音的处理结果，能量仍在 440 / 660 Hz（如果采样率弄错会跑到 479 / 718 Hz，那里低了 40 dB 以上）；处理后没有比原声更大的突变（没有咔嗒）。
- **真实人声**：B 站视频（BV1F4411T7em）录了 41 秒，处理后左右电平差在左耳 +70 dB 到右耳 −73 dB 之间来回（贴耳加强的左右穿梭），响度被自动响度拉到目标。试听文件：`~/Downloads/ASMR_KU100测试/安卓模拟器_病娇_原声（网页直出）.wav` 和 `…_贴耳加强（实时处理）.wav`。
- **运行中切换**：三个风格按钮、用 RON 改参数立即生效；切到掏耳后声音只在一只耳朵里（另一只 −120 dB）；RON 写错时报错、参数不变。
- **处理占用**：同一个基准（60 秒合成人声，按每次 4360 帧送入）在 Mac 上占单核 1.8–2.2%，在模拟器里 2.3–3.5%，说明安卓版的引擎本身不慢。App 里实际播放时测到 2–10%，是模拟器里 WebView 渲染抢 CPU、计时被打断造成的，要以真机实测为准。

还没做的：真机实测（处理占用、耳机实听）；数据文件挪到 APK 资源目录（加 armeabi-v7a 时一起做）。

## 第 3 步结果

2026-11-20 做了正式的浏览器界面和效果面板（Jetpack Compose），在模拟器上逐项点过：

- **配置文件和电脑版通用**：桌面版的「保存的设置」（`Saved`）和配置文件读写挪进了共用库（`auto-asmr/src/config.rs`），桌面版和安卓版用同一份代码读写同一个格式。安卓版的设置也存成这个格式（`files/auto-asmr.ron`，改动后 1 秒自动保存）。实测：按桌面版自动保存的样子（带 `egui`、`window` 两段）和「导出配置」的样子各生成一份文件，安卓导入后再导出，用共用库解析出来整份设置逐项相同——效果参数、`dry`，以及安卓用不上的音频设备名（含中文）、缓冲、位深、多音频合成、左右双人等都原样带回。只有窗口状态（`egui`、`window`）不带，桌面版自己的「导出配置」也不带。
- **预设和操作逻辑一致**：三个风格按钮、恢复默认、雨夜、关雨声调用库里同一个函数；贴耳距离和离头距离按厘米显示、移动频率同时缩放停留和移动时间、降噪滑条 0 = 关闭、换移动方式时「响度随位置变化」换成默认值，这几处换算挪进了库里的 `Params` 方法，桌面版界面也改成调用它们。手动摆位照搬桌面版：打开时记下原来的方式，播放中打开时声源停在此刻的位置；关掉时恢复原来的方式和「响度随位置变化」。实测：打开手动摆位后模式 = 手动、响度随位置变化 = 0.5、位置取自打开那一刻（方位 79°、离头约 35 cm、高 16°）；关掉后回到随机走位、0.2。
- **参数面板按参数表生成**：分组、标签、说明、范围、单位取自桌面版界面（`rust/src/ui_spec.json`，9 组 72 项），Rust 的测试逐项核对每一项都对得上 `Params`、范围两端设得上去、每个选项（8 种耳廓、二十多款耳机）都能设。
- **不能共用的参数**：桌面版的音频设备、缓冲、文件转换、多音频合成、语言、3D 视图在安卓上没有对应功能，界面不显示，配置里原样保留。左右双人后来也加上了（处理网页声音，见[安卓版](android-dev.md#界面和操作)）。
- **只放一个标签页的声音**：其它标签页用 `setAudioMuted` 静音。实测发现切走的标签页即使不静音，Chromium 也会把它的视频暂停（网页不可见）；所以「固定」出声的标签页时，把它的 WebView 垫在当前标签页下面（看不见，但对 Chromium 来说仍可见），视频会自动恢复播放、引擎改为处理它。
- **发声检测**：每个网页（含 iframe）在文档开始时注入脚本，媒体元素（包括没放进页面的 `new Audio()`）和 Web Audio 的状态变化时报告给 App，标签页列表里显示 🔊。
- **网站兼容**：B 站自动用桌面版 UA；其余网站用去掉 `; wv` 的手机 UA；拦掉唤起 App 的非 http(s) 跳转；支持网页视频全屏。
- 原来的验证功能（左右互换 / 静音模式、录音、adb 参数）还在，`tools/hook_test.sh` 照常通过。

还没做的：真机实测；切到后台、锁屏后继续播放（第 4 步的前台服务）；背景雨声目前只在网页出声时才有（同第 4 步）。

## 第 4 步结果

2026-11-20 做完，在模拟器上测了发布版（R8 精简后的 APK）和调试版：

- **后台播放**：有声音时开媒体播放类型的前台服务，通知栏和锁屏上可以暂停 / 继续、切效果 / 原声，耳机按键也能暂停 / 继续（MediaSession）；没声音 30 秒后关掉，从通知栏暂停的留 10 分钟。正在出声的标签页在切到后台、锁屏时仍对 Chromium 报告「可见」（WebView 子类改写 `onWindowVisibilityChanged`），否则 Chromium 会把视频暂停。实测：放 HLS 后按 Home，15 秒后仍每秒 10 次回调、引擎在动；再关屏 12 秒照常；媒体键暂停后网页停止出声、通知变成「已暂停」，继续后恢复。
- **背景雨声单独输出**：雨声改成自己的一条 AAudio 输出流（48 kHz），网页暂停、换网页时不断；网页声音的引擎用去掉背景音的配置，不会有两份。算法和桌面版引擎里的一样（`sync_rain` + `Rain::render` + 限制器），Rust 测试核对了响度和桌面版引擎测试的期望值相差 2 dB 以内。切到原声或关雨声时淡出、停流；切回效果时重开；流断开（耳机拔插）时自动重开。实测：没有网页出声时打开雨夜，雨声流自己开始播放；原声 / 效果来回切、关雨声都对。
- **截取自检和备用路径**：网页在出声、却 4 秒没有被截到的流，就用 ShadowHook 在 libaaudio 里装 inline hook（截所有调用方，App 自己的雨声流除外），并把这个标签页静音约 6 秒再恢复，让 Chromium 开一条新流；还截不到就提示这个 WebView 不支持。两种 hook 同时开着时有防重入，一次调用只处理一次。实测：只用 inline hook 时 WebView 的流照常被截到、处理；一个 hook 都不装时，自检在第 4 秒装上 inline hook，静音约 6 秒后新流被截到、开始处理，提示自动消失。
- **数据文件和 32 位**：`ku100.bin`、`pinna.bin` 不再编进 .so，作为 APK 资源（各架构共用一份），启动时读进引擎（共用库加了可选的 `external-data`，桌面版和浏览器插件不受影响）。加了 `armeabi-v7a`。`libasmrhook.so` 从 17.4 MB 降到 3.2 MB（arm64）/ 2.2 MB（32 位）；发布版 APK（两种架构）20 MB。32 位只编译过，没有设备实测。
- **发布**：GitHub Actions 新增 Android 任务（装 SDK 组件、Rust 1.95 和两个安卓目标、跑 Rust 测试、打 APK），APK 随 Windows / Linux 版一起放进 Release，写进 `SHA256SUMS.txt`。签名密钥放在仓库 Secrets 里；没配时用临时调试密钥签名并在 Actions 里警告。本机用临时密钥库模拟过签名和版本号（tag 名、运行序号）；工作流本身还没在 GitHub 上跑过。

还没做的：真机实测（尤其国产 ROM、32 位机、处理占用、耳机实听）；按网站记住直通（双耳录音的视频）；书签。

## 方案对比

| 方案 | 做法 | 主要问题 | 结论 |
|---|---|---|---|
| **A. WebView + 进程内 hook AAudio 回调** | 见上 | 依赖 Chromium 内部实现，WebView 每月自动更新，需要检测失效（见[风险](#风险)） | **推荐** |
| B. WebView + 注入 JS，在页面里用 Web Audio 处理 | `addDocumentStartJavaScript` 注入脚本，把 `<video>` / `<audio>` 用 `createMediaElementSource` 接进 AudioWorklet，引擎编译成 WASM | 跨域且没有 CORS 头的媒体会被判为"污染"，输出静音（Blink `media_element_audio_source_handler.cc` 的 `WouldTaintOrigin`）；跨域 iframe、页面自己建的 AudioContext 要逐个挂；降噪等也要编进 WASM；网站一改版就可能失效 | 作为 A 失效时的备用 |
| C. AudioPlaybackCapture（录本 App 的播放） | MediaProjection + `AudioPlaybackCaptureConfiguration.addMatchingUid` | 每次都要用户同意"录屏"，要前台服务；它只是复制一份声音，原声照样播出，会同时听到原声和处理后的两份；还多一道录制缓冲的延迟 | 不可用 |
| D. 自己编译 Chromium | 基于 Chromium 或 Cromite 这类开源分支做浏览器，直接在 `aaudio_stream_wrapper.cc` 里调用引擎 | 最稳，也能分清是哪个标签页的声音；但要 Linux 构建机（源码上百 GB、编译数小时），APK 大，每个 Chromium 版本都要重打补丁、跟进安全更新；Chromium 没有现成的可嵌入库（WebLayer 已停，[chromium-dev 讨论](https://groups.google.com/a/chromium.org/g/chromium-dev/c/7oa3OoEt-7w)） | A、B 都走不通时再考虑 |
| E. GeckoView（Firefox 内核） | Mozilla 官方的可嵌入库 | 不是 Chromium；没有音频输出接口，截声音同样只能 hook 或自己编译 Gecko | 不推荐 |
| F. 系统音效 AudioEffect | 给 WebView 的音频会话挂效果 | 自定义效果要写进系统配置（需要 root），自带的只有均衡、低音、虚拟环绕、动态处理，做不了双耳空间化 | 不可用 |

以上都只处理本 App 的声音。处理**任意 App** 的声音（需要 Shizuku 或 root）是另一个原型，见[全局拦截版](shizuku.md)。

## 推荐方案的细节

### 结构

```
网页（沙箱渲染进程）：解码、混音 ──共享内存──┐
                                             ▼
App 进程：WebView 浏览器侧（libmonochrome_64.so）
          └─ AAudio 回调：Chromium 填好一块 float PCM（立体声）
               └─ hook 包的一层：下混成单声道 → Engine::process → 左右声道写回同一块缓冲
                    └─ 交还 AAudio → 耳机
```

### hook

- 用 [ByteHook](https://github.com/bytedance/bhook)（字节跳动开源，MIT，支持 Android 4.1–17，Maven 依赖 `com.bytedance:bytehook`）。只 hook 调用方是 WebView so（文件名匹配 `libmonochrome*.so`、`libwebviewchromium.so`）、被调方是 `libaaudio.so` 的这几个函数：
  - `AAudioStreamBuilder_setDataCallback`：把 Chromium 的 `(回调, userData)` 换成 `(我们的回调, 上下文)`，上下文里记着原回调、原 userData 和分到的引擎。
  - `AAudioStreamBuilder_openStream`：记下流的采样率和声道数。
  - `AAudioStream_close`：把引擎还回池子。
- 我们的回调：先调原回调让 Chromium 填数据，再原地处理。回调跑在 AAudio 的高优先级线程上，所以引擎事先建好（池子，比如 4 个），回调里不分配内存、不加锁；参数更新沿用桌面版 `realtime.rs` 的做法（`Arc` 配置 + 版本号原子量）。
- App 启动时（第一个 WebView 创建之前）装好 hook；ByteHook 会自动处理之后才加载的 so。
- 备用：PLT hook 在某些 WebView 版本上不生效时，改用 [ShadowHook](https://github.com/bytedance/android-inline-hook)（同一团队，MIT，v2.0.1）直接在 `libaaudio.so` 里 inline hook 同一个函数，不管调用方怎么链接都能截到。
- hook 这一层用 C 写（几百行，CMake 编译），只负责转发，处理全在 Rust 里，通过 `extern "C"` 接口调用。

### 声音格式

- **声道**：引擎是单声道输入、双耳输出。立体声流先下混（或像桌面版一样只取一个声道 / 取更响的声道），处理后写回左右声道；不是 2 声道的流直接放过。
- **采样率**：引擎是 48 kHz。实测 Chromium 的流采样率跟着片源走，媒体文件常是 44.1 kHz（见[验证结果](#第-1-步验证结果)）。已实现：不是 48 kHz 的流，在回调里用现有的 `resample.rs` 先转到 48 kHz、处理后转回原采样率，输出端先垫 128 帧静音作缓冲（44.1 kHz 时总延迟约 6 ms）。
- **延迟**：原地处理，48 kHz 的流只多引擎算法的 2.4 ms（开降噪再加 20 ms）。Chromium 按 AAudio 时间戳做音画同步，不知道这几毫秒，但差这么一点看不出口型对不上。
- **动作**：网页内容是连续的录音，默认用"试听"的行为（`set_live(false)`，动作严格跟着说话走），不用主播模式的"停顿 1 秒恢复"。
- **多个流**：一个流一个引擎，通常一个页面只有一个流。同时有几个流时它们会各走各的，验证时看要不要共用位置。
- **已经是双耳录音的视频**（很多 ASMR 视频本身就是 KU100 录的）：再处理会变成"双重空间化"，要有一键直通，最好能按网站记住。
- **背景雨声**：网页暂停后 Chromium 会停掉输出流，雨声不能只混在网页的流里。雨声单独开一个自己的 AAudio 输出流（Rust 里用 `ndk` 或 `cpal` 的安卓后端）。

### 只处理"在发声的那个网页"

hook 截到的是整个 App 里所有 WebView 的声音，从 AAudio 这一侧分不出是哪个标签页。做法：

- 注入一段文档开始时运行的脚本（`WebViewCompat.addDocumentStartJavaScript`），监听媒体元素的 play / pause / volumechange 和 AudioContext 的状态，通过 `addWebMessageListener` 报告给 App，标签页上显示"正在发声"。
- 用 [`WebViewCompat.setAudioMuted`](https://developer.android.com/reference/androidx/webkit/WebViewCompat)（androidx.webkit，需要 `WebViewFeature.MUTE_AUDIO`）把其它标签页静音，只让选中的那个出声。默认"跟随当前标签页"，也可以锁定某一个。已验证：静音后这个 WebView 的流立刻不再回调，几秒后关掉。
- 浏览器要去掉 UA 里的 `; wv`、拦掉 `bilibili://` 这类唤起 App 的跳转，并提供"桌面版网站"开关（见[验证结果](#第-1-步验证结果)）。

### 复用 Rust 代码

- 不另写一套 Java 版：声音在 AAudio 的原生线程里处理，Java 进不了这条路径（垃圾回收、即时编译会爆音）；算法只维护一份；Rust 版还能编成 WebAssembly 给方案 B 用。
- `auto-asmr/src/dsp/` 只依赖 rustfft、realfft、nnnoiseless、serde 和内嵌的 `assets/ku100.bin`（10.9 MB）、`pinna.bin`（4.8 MB），直接编译到 `aarch64-linux-android`。
- `auto-asmr` 已拆成"库 + 程序"（第 2 步）：`src/lib.rs` 导出 `dsp`；cpal、eframe、rfd、symphonia、hound、png 放进默认开启的 `desktop` feature；测试用的合成信号挪进 `dsp/testsig.rs`。桌面版行为不变。
- 安卓的 Rust 层（`auto-asmr-android/rust/`）以 `default-features = false` 依赖它，编成静态库链进 hook 层，导出 C 接口（建 / 释放处理器、处理、读状态、换参数、切风格）。`Params` 本来就是 serde 结构，参数用 RON 传，和桌面版配置文件互通。
- 参数的名称（中英）、范围、默认值、说明由 Rust 导出成一张表，Kotlin 按表生成滑条，不在两边各写一份（第 3 步）。
- 构建：`app/src/main/cpp/CMakeLists.txt` 按 ABI 调用 `cargo build --release --target …`，Gradle 一条命令编完。Google Play 要求面向 Android 15 以上的新应用和更新支持 16 KB 内存页：NDK r29 默认就按 16 KB 对齐（已核对 `libasmrhook.so` 的 LOAD 段对齐是 0x4000），CMake 里也显式写了 `-z max-page-size=16384`。
- 兼容性：minSdk 29（Android 10）——Chromium 只在 Android 10 以上用 AAudio，Chromium 主干本身的最低版本也是 29。ABI 目前只有 `arm64-v8a`，发布时加 `armeabi-v7a`（少数 32 位低端机）；那时把数据文件从 .so 里挪到 APK 的资源目录，免得每个 ABI 各带一份 15.7 MB。

### 界面和其它

- Kotlin + Jetpack Compose（第 3 步已做，见[第 3 步结果](#第-3-步结果)）：标签页、地址栏、前进后退；底部抽屉是效果面板，分组和桌面版右侧参数一致；位置图（俯视图 + 手动摆位）用 Compose Canvas 画，引擎状态每 0.1 秒经 JNI 读一次。书签还没做。
- 后台播放：`mediaPlayback` 类型的前台服务 + MediaSession（锁屏控制）；切到后台时不调用 WebView 的 `onPause()` / `pauseTimers()`。
- 桌面版功能的取舍：

| 功能 | 安卓版 |
|---|---|
| 风格预设、效果参数、降噪、手动摆位、位置图、背景雨声、配置导入导出（和电脑版同一格式）、效果 / 原声对比、延迟和处理时间显示 | 已做（第 3 步） |
| 麦克风实时（主播）模式、音频设备和缓冲设置 | 去掉 |
| 文件转换、批量、多音频合成、自动对齐、3D 视图 | 以后再说（引擎和 `offline.rs` 本身能在安卓上跑，主要是界面工作量） |
| 左右双人 | 已加上：处理网页声音，延迟、排布、自动平衡和电脑版同一个设置 |

## 风险

| 风险 | 应对 |
|---|---|
| PLT hook 在某些 WebView 构建上不生效（RELRO 共享、链接方式变化等） | 自检发现后自动换 ShadowHook inline hook（已做、已在模拟器上测过）；再不行退到方案 B |
| WebView 每月自动更新，Chromium 改了音频后端（比如服务端开关让某些设备改回 OpenSL ES）后 hook 悄悄失效 | 已做运行时自检（第 4 步）：注入脚本报告"在播放"、但 4 秒内没有被截到的流，就换 inline hook 并让网页重开音频流；还不行就提示。改用 OpenSL ES 这种情况两种 hook 都截不到，届时考虑方案 B（浏览器插件那边已有引擎的 WebAssembly 版）；hook 失败时一律放过、不改声音，保证不崩溃 |
| 国产 ROM 自带的 WebView 版本旧、实现有差异 | 验证时覆盖 Pixel（或原生系统）和一两台主流国产机；App 里显示 WebView 版本 |
| DRM（Widevine）内容的声音是否也经过这条路径 | 验证时确认；不行就提示不支持 |
| 手机 CPU 比桌面弱 | 基准在 Mac 上占单核约 2%，模拟器里约 2.3–3.5%（见[第 2 步结果](#第-2-步结果)）；真机上用 `examples/bench.rs` 和 App 的状态行实测 |
| 应用商店审核 | 只改本进程内的调用，不碰其它 App 和系统，常见于大厂 App；但没有官方保证 |

## 开发步骤

0. **环境**（已完成，见[安卓版开发](android-dev.md)）：JDK、SDK、NDK、模拟器、Rust 的 `aarch64-linux-android` 目标。
1. **验证 hook（已在模拟器上通过）**：最小 App = 一个 WebView + ByteHook，回调里只做左右互换 / 只留左耳 / 静音，日志打出每个流的格式和改写前后的电平。结果见[第 1 步验证结果](#第-1-步验证结果)。还差：在真机上戴耳机实听，并在 WebView 升级后复跑 `tools/hook_test.sh`。
2. **接引擎（已在模拟器上通过）**：拆出 Rust 库、写 C 接口，回调里跑 `Engine::process`，44.1 kHz 重采样，实测处理时间和断流。结果见[第 2 步结果](#第-2-步结果)。还差：真机实测。
3. **浏览器和效果面板（已在模拟器上通过）**：标签页、发声检测、静音其它标签页、参数面板、位置图、配置导入导出（和电脑版通用）。结果见[第 3 步结果](#第-3-步结果)。
4. **完善（已在模拟器上通过）**：雨声单独输出、后台播放、hook 失效自检和备用路径、数据文件挪到 APK 资源、32 位、发布（GitHub Actions 打 APK）。结果见[第 4 步结果](#第-4-步结果)。下一步：真机实测。

## 目录规划

```
auto-asmr-android/
  README.md
  settings.gradle.kts, build.gradle.kts
  app/                       Kotlin + Compose：浏览器、效果面板
  app/src/main/cpp/          hook 层（C，ByteHook），调用 Rust 的 C 接口（asmr_core.h）
  rust/                      静态库：引擎、设置、雨声的 C 接口，依赖 ../../auto-asmr 的 dsp 和 config；
                             src/ui_spec.json 是参数表；examples/bench.rs 是性能基准
  tools/hook_test.sh         自动验证（hook、引擎、录音）
```

## 参考

- Chromium 源码：[`media/audio/android/`](https://source.chromium.org/chromium/chromium/src/+/main:media/audio/android/)、[`media/audio/audio_features.cc`](https://source.chromium.org/chromium/chromium/src/+/main:media/audio/audio_features.cc)、[`media/audio/BUILD.gn`](https://source.chromium.org/chromium/chromium/src/+/main:media/audio/BUILD.gn)、[`media_element_audio_source_handler.cc`](https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/modules/webaudio/media_element_audio_source_handler.cc)
- [WebView 架构](https://chromium.googlesource.com/chromium/src/+/main/android_webview/docs/architecture.md)
- [ByteHook](https://github.com/bytedance/bhook)、[ShadowHook](https://github.com/bytedance/android-inline-hook)
- [WebViewCompat（setAudioMuted、addDocumentStartJavaScript、addWebMessageListener）](https://developer.android.com/reference/androidx/webkit/WebViewCompat)
- [AudioPlaybackCaptureConfiguration](https://developer.android.com/reference/android/media/AudioPlaybackCaptureConfiguration)
- [GeckoView](https://firefox-source-docs.mozilla.org/mobile/android/geckoview/index.html)
