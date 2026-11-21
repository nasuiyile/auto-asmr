# 全局拦截版 global-asmr（Shizuku / root，原型）

`auto-asmr-shizuku/` 是一个单独的安卓 App，名字叫 **global-asmr**：把**任意 App** 播放的声音截下来，交给和安卓版同一份 Rust 引擎处理，再从耳机放出来。原声不会再进耳机，听到的只有处理后的声音。不需要 root，但需要 [Shizuku](https://shizuku.rikka.app/)（或者 root）。界面、参数、配置文件和[安卓版](android-dev.md)（auto-asmr）一致，所有参数在一页里改。

目前是原型。0.2.0 在 Android 16 模拟器上验证通过（见[实测结果](#实测结果模拟器android-16)）：常驻服务进程（App 被杀后声音照常处理）、悬浮球、参数面板。真机上第一次试（荣耀 AAP-AN00，Android 17）在开始录音时报错，已经针对性修改（见[厂商系统的兼容处理](#厂商系统的兼容处理)），**还要在这台手机上复测**；其它真机没测过，「用 root 启动」（Magisk / KernelSU 的 su）也还没跑过（见[还没验证的](#还没验证的)）。

## 原理

安卓的"动态音频策略"（`android.media.audiopolicy.AudioPolicy`，系统接口）可以按规则改写声音的去向：

1. 规则：用途是**媒体 / 游戏 / 未知**的播放，排除服务进程自己的 UID 和它输出流的会话号。也可以只选几个 App（按 UID）。
2. 路由：`ROUTE_FLAG_LOOP_BACK`，只送进虚拟输出设备 remote submix，**不再送到扬声器 / 耳机**。
3. 从策略建一个录音端（`createAudioRecordSink`）开始录：从这一刻起，匹配到的 App 的声音改走 remote submix，服务进程读出来、处理、再用普通的媒体输出流放出去。

注册这种策略需要 `MODIFY_AUDIO_ROUTING` 权限。普通 App 拿不到，但 shell 身份（adb、Shizuku 的 adb 模式）有：AOSP 的 Shell 包为了 CTS 测试申请了它，同时还有 `CAPTURE_AUDIO_OUTPUT`、`CAPTURE_MEDIA_OUTPUT`、`DUMP`。root 身份什么权限都有。

和 `AudioPlaybackCapture`（录屏录内录）的区别：

| | AudioPlaybackCapture | 这里的回环策略 |
|---|---|---|
| 原声 | 照样播出，只复制一份（会同时听到两份） | 不再送到耳机 |
| 权限 | 普通 App + 用户同意录屏 | 需要 shell / root 身份 |
| App 禁止被录（`allowAudioPlaybackCapture=false` 等） | 生效，截不到（Chrome、Spotify 这类） | **不生效**：AOSP `AudioPolicyMix::mixMatch` 里这几项检查只对"回环 + 同时播出"这种混音做，纯回环不检查 |

App 禁止被录，是因为不想被录音。所以这个 App 只做实时处理，不提供录音导出。

## 进程和启动方式

一共两个进程：

- **App 进程**（界面、通知栏、悬浮球）：也加载一份 Rust 引擎，但只用来管参数，和安卓版的 `Settings` 完全一样（生成界面、改参数、导入导出、自动保存到 `files/auto-asmr.ron`）。参数一变，就把整份参数（RON）和原声开关推给服务进程。
- **服务进程**（shell 或 root 身份）：注册音频策略、截声音、用引擎处理、放出来，背景雨声也在这里放。它不存配置，参数都来自 App。

服务进程有三种起法，核心代码相同（`server/EngineCore.kt`），只是入口和命令的通道不同：

| 方式 | 身份 | 入口和通道 | App 进程被杀时 |
|---|---|---|---|
| Shizuku（推荐） | Shizuku 的身份：adb 模式是 shell（2000），root 模式是 root | Shizuku 的 UserService：Shizuku 用 app_process 加载本 APK 里的 `server/EngineService.kt`（一个 Binder），交给 App；命令经 binder 传。用 daemon 模式绑定 | **继续跑**，声音照常处理；App 再启动时连回同一个进程 |
| root | root（0） | `su -c` 起 `server/ServerMain.kt`，命令经标准输入 / 输出 | 管道断开，服务进程停止拦截并退出 |
| adb（调试） | shell（2000） | 电脑上跑 `tools/server.sh`，同 root 那种通道 | 不涉及 |

第一次点「用 Shizuku 启动」会弹 Shizuku 的授权框。UserService 的版本号用 APK 的安装时间：更新或重装 App 后版本号变了，Shizuku 会结束还在跑旧代码的服务进程，用新 APK 重新起一个。

服务进程退出或被杀时，系统发现策略的回调 binder 断开，自动注销策略，各 App 的声音回到原来的设备（已实测）。

## 防止被杀后台

1. **引擎在常驻进程里**（Shizuku 方式，不用设置）：服务进程是 shell / root 身份的进程，不归系统的 App 后台管理。App 进程被清掉后声音照常处理，参数保持最后一次的。这是最主要的一条。
2. **前台服务自动拉起**：连着服务进程时 App 开着一个前台服务（通知栏可以暂停 / 继续拦截、切效果 / 原声、退出）。它被系统杀掉后会被重新拉起，新的 App 进程收到 Shizuku 的 binder 后用 `peekUserService` 连回还在跑的服务进程（不会新开一个），通知和悬浮球随之恢复；10 秒内没连上就自己关掉。
3. **悬浮球**（要「显示在其他应用上层」的权限）：点一下切换效果 / 原声，长按打开 App，拖动后贴到屏幕左边或右边。颜色表示状态：紫 = 效果，灰 = 原声，浅灰 = 暂停拦截，红 = 没连上；外圈亮起表示此刻在处理声音。App 有一个看得见的窗口时，系统把它当作"用户看得见的"进程，很少回收。
4. **忽略电池优化**：界面上一键申请，系统省电时不再限制 App。
5. **厂商的后台管理**：只能由用户在系统设置里改，界面上按厂商给出说明和「应用设置」入口。荣耀 / 华为：应用启动管理里把 global-asmr 改成手动管理，打开「允许自启动」「允许后台活动」。

root 方式没有第 1、2 条：服务进程跟着 App 进程走。想要常驻，可以先用 root 启动 Shizuku，再走 Shizuku 方式。

## 使用

1. 装好 Shizuku 并启动它（无线调试或 adb），或者手机已 root。
2. 打开 global-asmr，点「用 Shizuku 启动」或「用 root 启动」。服务进程加载完引擎后自动开始拦截（上次暂停过的话保持暂停）。
3. 去别的 App 放声音，戴耳机听。

主界面一页从上到下：

- **服务进程**：Shizuku 状态、服务进程的版本和身份、「拦截其他 App 的声音」开关（暂停 / 继续）、输入 / 输出峰值、断流次数、「退出服务」。
- **后台运行（防止被杀）**：悬浮球开关、忽略电池优化、厂商后台管理的说明和入口。
- **ASMR 效果**：和安卓版同一个面板（`EffectPanel.kt`、`PositionMap.kt`、`SpecUi.kt` 从安卓版拷来）：效果 / 原声开关、风格按钮、位置图和手动摆位、降噪、各组参数、导入 / 导出配置。状态一行显示是否在处理、链路延迟、处理占用。
- **日志**：App 和服务进程的日志（点开才显示）。

别的 App 里对 `.ron` 文件选「用其他应用打开 → global-asmr」也能导入，和安卓版一样先检查再确认。

只拦**媒体、游戏、未知**这三种用途的播放。通话、闹钟、通知、导航语音、系统提示音照常播出，不处理。

## 控制协议

命令一行一条，每条有一个回复（`server/EngineCore.kt`）。Shizuku 方式经 binder 传（调用号 `FIRST_CALL_TRANSACTION`，参数和回复各一个字符串）；root / adb 方式经标准输入 / 输出：

```
CLASSPATH=<apk> app_process /system/bin io.github.nasuiyile.autoasmr.shizuku.server.ServerMain \
    --apk <apk> [--lib <libasmrshizuku.so>] [--client-uid <App 的 UID>] [--start] [--seconds N] [--onset]
```

- `--start`：引擎加载完马上开始拦截。`--seconds N`：不读标准输入，跑 N 秒后退出（调试用）。`--onset`：测延迟时用，每个声音的开头放出来时发一行 `onset`。
- 标准输入里 App 发的命令带编号：`#12 poll`，回复是一行 `=12 <回复>`（回复里的换行写成 `\n`）。手敲的命令不带编号，回复直接写出来。标准输入关掉时停止拦截并退出。
- 命令：
  - `hello`：版本（`ver=`）、身份、系统版本、机型
  - `start`、`start only=UID,UID`（只拦这些 App）、`stop`：回复 `ok` 或 `error …`
  - `params <base64 的 RON>`：换参数（和配置文件里 params 那一段同一格式）；`dry 0|1`：原声（不经过引擎，同安卓版是直接切换）/ 效果
  - `poll`：JSON，是否在拦截、原声、雨声，`tele`（和安卓版 `NativeHook.telemetry()` 同一排列的 16 个数），峰值、延迟各段、断流、日志序号
  - `logs <序号>`：这个序号之后的日志
  - 调试用：`config <base64 的 .ron>`、`style 0..3`、`action <名字>`、`status`、`exit`
- 标准输出另外还有：`hello …`、`log …`、`warn …`、`error …`、`state …`、拦截中每秒一行 `stat …`、`onset …`、`bye`。

`stat` 各项：`in` / `out` 输入 / 输出峰值；`gated` 为真时表示连续 2 秒全是静音，输出流暂停；`lag_in` 是录音端读到的数据离被录下过了多久（毫秒）；`lag_out` 是输出流里还没放出去的量（毫秒）；`engine` 是引擎本身的延迟；`cpu` 是处理占单核的百分比；`underruns` 是输出断流次数；`out_buf` 是输出缓冲的上限（从 30 ms 开始，一断流就加 5 ms）。

背景雨声（雨夜等）在服务进程里单独开一条输出流放：拦截中、不是原声、背景音音量 > 0 时开，关掉时淡出。它的 UID 被拦截规则排除，不会被自己截回来。

## 厂商系统的兼容处理

服务进程是 app_process 起的，不是普通 App 进程：没有当前 ActivityThread、Application 和包名。AOSP 的音频代码在这些为空时照样能用（模拟器上就是这样跑通的），厂商改过的框架不一定。

**荣耀 AAP-AN00（Android 17，MagicOS）**：Shizuku 启动、加载引擎、注册策略、建录音端都成功，但每次开始拦截都失败：

```
error 开始拦截失败：NullPointerException: null（Objects.throwNullPointerException:504 ← Objects.requireNonNull:222 ←
AudioRecord.startRecording:1573 ← AudioRouter.start:110 ← ServerMain.handle:112 ← ServerMain.main:89）
```

对照 AOSP 的 `android17-release` 分支：`AudioRecord.startRecording()` 只有十几行，没有任何 `requireNonNull`，所以这是荣耀加的代码，很可能要用 `ActivityThread.currentApplication()` 或当前包名。另外，策略建录音端时走的 AOSP 构造函数传的 Context 本来就是 `ActivityThread.currentApplication()`，在服务进程里是空的。

现在分两层处理：

1. **补进程环境**（`server/ProcessEnv.kt`，服务进程一启动就做，做法和 scrcpy 的 `Workarounds` 相同）：准备主线程 Looper，新建 ActivityThread 并设成当前的，补上 ConfigurationController 和系统 Context，把包名（shell 身份是 `com.android.shell`，root 是 `root`）填进 `mBoundApplication`，再放一个以 `ShellContext`（底是系统 Context）为底的 Application。这样 `currentApplication()`、`currentPackageName()`、`currentOpPackageName()` 都不再为空。结果在日志里是一行 `log 进程环境：补上了 …`，哪一步没补上也会写出来。
2. **绕开出错的 `startRecording()`**（`HiddenAudio.startRecording`）：如果厂商代码还是抛异常，就自己做 AOSP 里那几步：直接调 `native_start`，再开固定音量（`handleFullVolumeRec`），把状态记成"在录"。厂商代码可能在 `native_start` 之后才出错，这时已经在录，再调一次 `native_start` 不会有副作用。走了这条路时日志里会有一行 `warn startRecording 出错（…），已绕开厂商代码直接开始录音`。停止时（`stop()`）同样处理。

音频线程里没接住的异常现在也会报成 `error 音频线程出错：…`，不会让整个服务进程直接崩掉、App 那边只看到"服务进程退出"。

模拟器上的验证：补进程环境后，shell、root 身份和 App 里「用 Shizuku 启动」都照常拦截，延迟不变（多出 92–99 ms）；第 2 层用临时代码让 `startRecording()` 分别在 `native_start` 之前和之后抛 NullPointerException，两种情况都能正常开始录音，截到的电平是固定音量下的（`in=0.300`，等于探针短音的幅度），处理后的声音照常放出。荣耀手机上的效果还要复测。

**Shizuku UserService 里的身份**：Shizuku 的 UserService 启动器在进程里放的是本 App 包名的 Context。音频系统按"UID 2000 + 本 App 包名"建流，两者对不上，建输出流失败（`UnsupportedOperationException: Cannot create AudioTrack`）。所以 `ProcessEnv` 对包名和 Application 一律换成 shell / root 身份的（日志里显示「Application（替换）」），ActivityThread 等已有的照用。

第一次复测的截图里仍是旧包：日志没有 `log 进程环境：…` 这一行，调用栈的行号（`ServerMain.main:89`、`handle:112`）也是改之前的。为了一眼分清跑的是哪一版，从 0.1.1-prototype 起，界面上和服务进程的 `hello` 行都会显示版本号（`hello ver=0.1.1-prototype uid=…`）。每次发新包给别人测，都要在 `app/build.gradle.kts` 里把 `versionCode` 加一，并改 `versionName`。

## 实测结果（模拟器，Android 16）

模拟器：AVD `asmr36`（Pixel 7，Android 16，Apple Silicon），Shizuku 13.6.0（adb 模式）。测试音：Chrome 打开电脑上用 HTTP 提供的 WAV（440 Hz，幅度 0.2），或 `server/LatencyProbe.kt` 以 root 身份放的短音（幅度 0.3）。路由情况看 `dumpsys media.audio_flinger`：每个输出线程上挂着哪个 UID 的播放轨。

### 0.2.0（常驻服务进程、悬浮球、参数面板）

| 检查 | 结果 |
|---|---|
| 「用 Shizuku 启动」 | Shizuku 用 `moe.shizuku.starter.ServiceStarter` 起服务进程（进程名 `…shizuku:engine`，UID 2000），引擎加载约 75–100 ms，自动开始拦截；探针的轨在 `REMOTE_SUBMIX`，扬声器上是服务进程的轨 |
| `kill -9` App 进程（模拟被系统清掉） | 服务进程不受影响，探针的声音照常被截、处理、放出；约 1 秒后系统重新拉起前台服务，新的 App 进程用 peek 连回**同一个**服务进程（PID 不变） |
| 悬浮球 | 显示在桌面上；点一下变成「原」，服务进程里输出峰值 = 输入峰值（0.30），处理占用 0%：原声确实不经过引擎 |
| 雨夜 / 关雨声 | 服务进程里多出一条 UID 2000 的输出轨（雨声），关掉时淡出后停流。这也说明 App 改的参数推到了服务进程 |
| 「退出服务」 | 服务进程退出，前台服务、通知、悬浮球都消失 |
| 重装 App | 版本号（安装时间）变了，下次启动时换成新 APK 的服务进程 |
| 界面 | 一页里：服务进程、后台运行、和安卓版相同的效果面板（全部参数组）、日志 |
| release 包（R8 精简） | 15.5 MB（debug 包 47 MB，主要是没精简的 Compose）；以上流程在 release 包上重跑一遍，照常 |
| root / adb 通道（`tools/server.sh`） | 照常；延迟对照多出 98.6 ms，和之前一致 |

0.2.0 之前的版本没有真正切到原声：`dry` 只记进了配置，引擎照样在处理。现在原声时服务进程不调用引擎（和安卓版 hook 层的做法一样）。

### 0.1.x（核心流程）

| 检查 | 结果 |
|---|---|
| shell 身份注册策略 | 成功 |
| 拦截前 | Chrome（UID 10154）的轨在扬声器输出上 |
| 拦截中 | Chrome 的轨在 `REMOTE_SUBMIX` 输出上，扬声器输出上只有服务进程（UID 2000）的轨。Chrome 禁止被录屏录音，这里照样截到 |
| 截到的电平 | 0.200，等于测试音本身。媒体音量调到 2/25 也不变：截的是音量调节之前的声音，最后由服务进程的输出流按媒体音量放 |
| root 身份 | 成功。服务进程自己的输出被系统记在 **UID 1000**（system）名下，所以 root 时要同时排除 0 和 1000，否则会被自己截回来 |
| `start only=UID` 只拦指定 App | 成功 |
| `kill -9` 服务进程 | 3 秒内 Chrome 的轨回到扬声器输出，remote submix 输出消失 |
| App 里「用 Shizuku 启动」（0.1.x 用 `Shizuku.newProcess`） | 第一次弹 Shizuku 授权，允许后服务进程以 UID 2000 运行（父进程是 `shizuku_server`），加载完自动开始拦截；以后再点不再弹窗 |
| App 切到后台、Chrome 在放 | 前台服务一直在，拦截照常：Chrome 在 remote submix 上，扬声器上是服务进程的轨；回到 App 能看到统计 |
| 界面上的按钮 | 暂停 / 开始拦截（Chrome 的轨跟着在扬声器和 remote submix 之间切换）、切风格都有回应 |
| 「退出服务」、通知栏「停止」 | 服务进程退出、前台服务和通知消失，Chrome 的轨回到扬声器 |
| 强制停止 App | 服务进程随之退出 |
| 处理占用 | 单核 3–8% |
| 断流 | 刚开始放的那一两秒断 4–7 次（输出缓冲从 30 ms 自动加到 40–45 ms），之后不再断 |

**端到端延迟**：用 `server/LatencyProbe.kt` 测。它是另一个身份的"App"：每秒放一个 50 ms 的短音，记下每个短音写进输出流的时刻；服务进程记下这个短音从自己的输出流真正放出来的时刻。两者用同一个时钟，相减就是拦截后的延迟。再让它不拦截放一遍，作为对照。

| | 写入 → 放出（中位数，11 个短音） |
|---|---|
| 不拦截（正常播放） | 131–132 ms |
| 拦截（服务进程是 shell / root） | 225–233 ms |
| **多出来的** | **约 90–100 ms**（几次分别是 92、93、94、97、99、102） |

多出来的部分大致是：服务进程的输出缓冲 40 ms + 引擎 2.4 ms，剩下约 50 ms 在 remote submix 的管道和录音缓冲里。模拟器本身的播放延迟就很高（正常也要 130 ms），真机上两边都会小一些，要实测。

复现：装好 App 后用 `tools/server.sh --start --onset`。延迟对照要让探针以另一个身份跑，例如服务进程用 shell，探针用 `adb shell "su 0 sh -c 'CLASSPATH=<apk> app_process /system/bin io.github.nasuiyile.autoasmr.shizuku.server.LatencyProbe 12'"`。

模拟器上测 Shizuku：装官方 APK（GitHub Release），用 `adb shell <Shizuku 的安装目录>/lib/arm64/libshizuku.so` 启动它（安装目录用 `adb shell pm path moe.shizuku.privileged.api` 查），再在本 App 里点「用 Shizuku 启动」，或者 `adb shell am start -n io.github.nasuiyile.autoasmr.shizuku/.MainActivity --es launch shizuku`（其它测试参数见 `MainActivity.kt` 的类注释）。悬浮窗权限可以用 `adb shell appops set io.github.nasuiyile.autoasmr.shizuku SYSTEM_ALERT_WINDOW allow` 直接给。

## 还没验证的

- **真机**：荣耀 AAP-AN00（Android 17）第一次试在开始录音时出错，已经修改（见[厂商系统的兼容处理](#厂商系统的兼容处理)），还要复测；其它机型都没测。厂商可能改过音频框架或音频策略（华为、小米、三星等），要逐个看。
- **真机上的保活**：常驻服务进程、前台服务被拉起后连回去、悬浮球，在 MagicOS 这类后台管理很严的系统上表现如何；Shizuku 本身被清掉时服务进程也会退出。
- **Shizuku 的 root 模式**：只测了 adb 模式（shell 身份）。root 模式下服务进程是 root 身份，这个身份用 adb 测过，但没经过 Shizuku 跑。
- **root 启动**：模拟器自带的 `su` 不让 App 用（只认 shell 和 root），点「用 root 启动」会提示 `Permission denied`。服务进程在 root 身份下已经验证，没验证的是 Magisk / KernelSU 的 `su -c` 和管道转发（0.2.0 的编号请求 / 回复）。
- **硬件直通播放**：用 offload（DSP 解码长音频）或 AAudio MMAP 独占模式播放的 App 会不会按规则被截走。
- **长时间运行**：remote submix 由系统时钟驱动，输出由声卡时钟驱动，真机上两者有细微偏差，时间长了会偶尔断流或丢一小段。现在只靠断流时加大输出缓冲，没有做微调采样率的补偿。
- **蓝牙耳机**：输出走蓝牙时的延迟和行为。

## 构建

和安卓版同一套环境（JDK、Android SDK / NDK、Rust 安卓目标），见[安卓版](android-dev.md#环境)：

```bash
cd auto-asmr-shizuku && ./gradlew assembleRelease
```

生成 `app/build/outputs/apk/release/app-release.apk`（R8 精简，约 15 MB；原型阶段用调试密钥签名，和 debug 包可以互相覆盖安装）。`assembleDebug` 是不精简的 debug 包（约 47 MB）。服务进程按名字加载的类和 JNI 在 `app/proguard-rules.pro` 里保留。

Rust 引擎直接用安卓版的 Rust 层（`auto-asmr-android/rust`，C 接口 `asmr_core.h`），共用它的 `target` 目录。界面用 Compose，版本和安卓版相同。原型只编 arm64，不在发布工作流里。每次发新包给别人测，都要在 `app/build.gradle.kts` 里把 `versionCode` 加一、改 `versionName`。

## 代码结构

```
auto-asmr-shizuku/
  app/src/main/cpp/
    asmr_jni.c            JNI：把 asmr_core.h 的函数转给 server/Engine.kt（两个进程都用）
    CMakeLists.txt        调 cargo 编安卓版的 Rust 层，链进 libasmrshizuku.so
  app/src/main/java/io/github/nasuiyile/autoasmr/shizuku/
    AsmrApp.kt            App 进程启动：加载引擎、读配置、初始化 Controller
    MainActivity.kt       主界面（一页）：服务进程、后台运行、效果面板、日志；adb 测试参数
    EffectPanel.kt        效果面板（从安卓版拷来，加了页头 / 页脚）
    PositionMap.kt        位置图（从安卓版拷来）
    SpecUi.kt             参数表小工具（从安卓版拷来）
    Settings.kt           参数（同安卓版），改了就推给服务进程
    SharedImport.kt       别的 App 传来的 .ron（从安卓版拷来）
    Controller.kt         起服务进程（Shizuku UserService / su）、连上、推参数、定时读状态
    Links.kt              到服务进程的连接：binder（Shizuku）、管道（root）
    InterceptService.kt   前台服务：通知栏控制、挂悬浮球、被杀后拉起再连回去
    FloatingBall.kt       悬浮球
    KeepAlive.kt          悬浮窗权限、电池优化、厂商后台管理的说明
    server/               服务进程（shell / root 身份）
      EngineCore.kt       核心：引擎、拦截、雨声、命令处理、统计、日志
      EngineService.kt    Shizuku UserService 入口（binder）
      ServerMain.kt       app_process 入口（标准输入 / 输出，root、adb 用）
      AudioRouter.kt      注册策略、录音端 → 引擎 → 输出流
      RainPlayer.kt       背景雨声的输出流
      ProcessEnv.kt       给服务进程补上 ActivityThread / Application / 包名（厂商框架、身份要用）
      HiddenAudio.kt      音频策略系统接口的反射封装、开始 / 停止录音（必要时绕开厂商代码）、服务进程的 Context
      Engine.kt           Rust 引擎的 JNI 声明，服务进程加载 .so 和 APK 里的数据文件
      LatencyProbe.kt     测延迟用的短音播放器
  tools/server.sh         用 adb 直接跑服务进程（调试）
```

## 参考

- AOSP Shell 包的权限：`frameworks/base/packages/Shell/AndroidManifest.xml`
- AOSP `AudioRecord.java`、`AudioPolicy.java`、`ActivityThread.java`（`android17-release` 分支）：对照荣耀的报错
- scrcpy 的 `server/src/main/java/com/genymobile/scrcpy/Workarounds.java`：app_process 进程补 ActivityThread / Application 的做法
- Shizuku 的 UserService（`Shizuku.bindUserService`、`peekUserService`、`UserServiceArgs.daemon`）：[Shizuku-API](https://github.com/RikkaApps/Shizuku-API)
- 混音规则的匹配：`frameworks/av/services/audiopolicy/common/managerdefinitions/src/AudioPolicyMix.cpp`（`mixMatch`）
- scrcpy 的音频转发（shell 身份录整机输出，设备本身会变静音）：[doc/audio.md](https://github.com/Genymobile/scrcpy/blob/master/doc/audio.md)
- 同类思路的 App：[RootlessJamesDSP](https://github.com/timschneeb/RootlessJamesDSP)（用 AudioPlaybackCapture，截不到 Chrome、Spotify 等禁止被录的 App）
