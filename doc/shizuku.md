# 安卓版的全局模式（Shizuku / root，原型）

[安卓版](android-dev.md)（auto-asmr）有两种模式：没有 Shizuku 时用**网页模式**，在内嵌浏览器里听网页；有 [Shizuku](https://shizuku.rikka.app/)（或者 root）时可以用**全局模式**，把**任意 App** 播放的声音截下来，交给同一份 Rust 引擎处理，再从耳机放出来。原声不会再进耳机，听到的只有处理后的声音。两种模式在同一个 App 里，共用一份配置（参数、左右双人、原声开关）；同一个 App 里还有和电脑版一样的[文件转换](android-dev.md#文件转换)页。

全局模式原先是一个单独的 App（global-asmr，目录 `auto-asmr-shizuku/`），已经合并进安卓版：代码在 `auto-asmr-android/app/src/main/java/io/github/nasuiyile/autoasmr/global/`，随安卓版一起构建、发布。

目前是原型：在 Android 16 模拟器上验证通过（见[实测结果](#实测结果模拟器android-16)）。真机上第一次试（荣耀 AAP-AN00，Android 17）在开始录音时报错，已经针对性修改（见[厂商系统的兼容处理](#厂商系统的兼容处理)），**还要在这台手机上复测**；其它真机没测过，「用 root 启动」（Magisk / KernelSU 的 su）也还没跑过（见[还没验证的](#还没验证的)）。

## 原理

安卓的"动态音频策略"（`android.media.audiopolicy.AudioPolicy`，系统接口）可以按规则改写声音的去向：

1. 规则：用途是**媒体 / 游戏 / 未知 / 语音通话**的播放，排除服务进程自己的 UID 和它输出流的会话号。也可以只选几个 App（按 UID）。按用途分成两个混音：媒体 / 游戏 / 未知一个，语音通话一个。
2. 路由：`ROUTE_FLAG_LOOP_BACK`，只送进虚拟输出设备 remote submix，**不再送到扬声器 / 耳机**。
3. 从策略给每个混音建录音端（`createAudioRecordSink`）开始录：从这一刻起，匹配到的 App 的声音改走 remote submix，服务进程读出来、处理、再放出去。两个混音各有一个录音端、一个处理器、一条输出流：媒体那路用媒体用途的输出流，语音通话那路用语音通话用途的输出流，各按原来的音量和去向放（见[使用](#使用)里的"语音通话"）。

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

- **App 进程**（界面、通知栏，也是网页模式的浏览器）：参数由 `Settings` 管（和网页模式同一份，引擎在 `libasmrhook.so` 里，经 `NativeHook` 调用；自动保存到 `files/auto-asmr.ron`）。配置一变，就把**整份配置文件**推给服务进程：左右双人的延迟、排布、自动平衡存在配置文件里 `params` 之外，原声开关也在里面，所以推整份，两种模式才用得上同一份配置。
- **服务进程**（shell 或 root 身份）：注册音频策略、截声音、用引擎处理、放出来，背景雨声也在这里放。它加载 `libasmrglobal.so`（同一个 Rust 静态库，不带 hook），不存配置，配置都来自 App。

服务进程有三种起法，核心代码相同（`global/server/EngineCore.kt`），只是入口和命令的通道不同：

| 方式 | 身份 | 入口和通道 | App 进程被杀时 |
|---|---|---|---|
| Shizuku（推荐） | Shizuku 的身份：adb 模式是 shell（2000），root 模式是 root | Shizuku 的 UserService：Shizuku 用 app_process 加载本 APK 里的 `global/server/EngineService.kt`（一个 Binder），交给 App；命令经 binder 传。用 daemon 模式绑定，进程名 `io.github.nasuiyile.autoasmr:engine` | **继续跑**，声音照常处理；App 再启动时连回同一个进程 |
| root | root（0） | `su -c` 起 `global/server/ServerMain.kt`，命令经标准输入 / 输出 | 管道断开，服务进程停止拦截并退出 |
| adb（调试） | shell（2000） | 电脑上跑 `auto-asmr-android/tools/global_server.sh`，同 root 那种通道 | 不涉及 |

第一次点「用 Shizuku 启动」会弹 Shizuku 的授权框。UserService 的版本号用 APK 的安装时间：更新或重装 App 后版本号变了，Shizuku 会结束还在跑旧代码的服务进程，用新 APK 重新起一个。

服务进程退出或被杀时，系统发现策略的回调 binder 断开，自动注销策略，各 App 的声音回到原来的设备（已实测）。

## 后台运行

1. **引擎在常驻进程里**（Shizuku 方式，不用设置）：服务进程是 Shizuku 起的 shell / root 身份的进程，不归系统的 App 后台管理。App 进程被清掉后声音照常处理，参数保持最后一次的。这本身就是防杀，不需要别的手段。
2. **前台服务**：连着服务进程时 App 开着一个前台服务（`global/InterceptService.kt`，和网页模式的播放通知分开），通知栏里可以暂停 / 继续拦截、切效果 / 原声、退出。它被系统杀掉后会被重新拉起，新的 App 进程收到 Shizuku 的 binder 后用 `peekUserService` 连回还在跑的服务进程（不会新开一个），通知随之恢复；10 秒内没连上就自己关掉。

App 不主动申请通知权限（Android 13 起要用户同意）。没有这个权限时前台服务照常运行，只是通知栏里不显示它的通知；想要通知栏控制，在系统设置里给 auto-asmr 打开通知，回到 App 时通知就会出来。

root 方式没有第 1 条，第 2 条也只剩前台服务本身：服务进程跟着 App 进程走（管道断开就停止拦截并退出，声音恢复原样），App 进程被杀后没有可以连回去的服务进程；前台服务让 App 进程不容易被系统回收。想要常驻，可以先用 root 启动 Shizuku，再走 Shizuku 方式。

以前还有悬浮球、「忽略电池优化」一键申请、厂商后台管理的说明，都是为了让 App 进程别被清掉。Shizuku 方式下声音不靠 App 进程，所以都去掉了；悬浮球"点一下切换效果 / 原声"的用法改在 App 里或通知栏切换。去掉以后实测：`kill -9` App 进程，服务进程照常处理，约 1 秒后前台服务被系统拉起并连回去（见[去掉悬浮球等之后](#去掉悬浮球等之后)）。

## 使用

1. 装好 Shizuku 并启动它（无线调试或 adb），或者手机已 root。
2. 打开 auto-asmr。屏幕最下面的导航栏可以在「网页 / 全局 / 文件转换」之间切换。第一次打开时，Shizuku 在运行就直接进**全局模式**，否则进**网页模式**；之后记住上次用的页面。
3. 在全局页点「用 Shizuku 启动」（第一次会弹 Shizuku 授权）或「用 root 启动」。服务进程加载完引擎后自动开始拦截（上次暂停过的话保持暂停）。
4. 去别的 App 放声音，戴耳机听。

全局页一页从上到下：

- **标题行**：「全局模式」和右边的「?」。全局模式是什么、需要什么、Shizuku 和 root 启动的区别、拦截开关和那行数字的含义、版本号都收在「?」点开的说明框里，页面上只留控件和状态。
- **服务进程**：一行里是 Shizuku 状态和服务进程状态（前面的圆点：绿 = 正常、黄 = 启动中、灰 = 没运行），下面是服务进程的版本和身份；没运行时是两个启动按钮；连上后是「拦截其他 App 的声音」开关（拦截中用主色底）（暂停 / 继续），开关下面一行是输入 → 输出峰值、断流次数、输出缓冲；「退出服务」在卡片标题右边。
- **ASMR 效果**：和网页模式同一个效果面板（`EffectPanel.kt`）：效果 / 原声开关、风格按钮、位置图和手动摆位、降噪、各组参数（含**左右双人**）、导入 / 导出配置。状态一行显示是否在处理、链路延迟、处理占用。
- **日志**：App 和服务进程的日志（点开才显示）。

拦**媒体、游戏、未知、语音通话**这四种用途的播放。闹钟、通知、铃声、导航语音、系统提示音照常播出，不处理。

**语音通话**：QQ / 微信的语音、视频通话，语音房（微光等）里别人说话的声音，App 用的是"语音通话"用途（`USAGE_VOICE_COMMUNICATION`），和放视频、音乐的"媒体"不同，所以要单独列进规则（以前没列，这类 App 里对方的声音没有被处理）。截下来后照样处理。

**音量和去向和没拦截时一样**：服务进程把截到的声音分成两路，媒体 / 游戏 / 未知一路，语音通话一路，各自处理，再各用一条输出流放：

- 媒体那路用媒体用途的输出流，跟**媒体音量**；
- 通话那路用语音通话用途的输出流，跟**通话音量**，去向也和通话一样（戴耳机在耳机里，没戴在听筒或免提里）。

所以通话中按音量键调的是对方的声音，同时放的音乐还是跟媒体音量；通话时系统把媒体压低、按通话音量算上限这些规则也照旧（系统对两条输出流的处理和对原来的 App 一样，见[通话音量](#通话音量)）。

- 为什么分两路：截到的是调音量之前的声音，放出来时按输出流的用途决定用哪个音量。只有一条输出流时只能挑一种：用媒体用途，通话中按音量键调不了对方的声音（系统还会把它压在"通话音量 + 3 dB"以下）；通话时整条换成语音通话用途，同时放的音乐又会跟着通话音量变。
- 没有通话时通话那路的输出流是暂停的（连续 2 秒静音就暂停，和媒体那路一样），不占输出。两路都在出声时（通话中又在放音乐），处理占用是两份。两路各有一个处理器，参数共用；界面上的位置、说话活动显示正在出声的那路（两路都在出声时显示通话那路）。
- 立体声：通话类声音有一条专用的直通输出（`VOIP_RX`），很多手机上是 16 位单声道。通话那路的输出流是浮点立体声，和它的格式对不上，AOSP 的音频策略会退回普通的混音输出，立体声保得住。通话那路开始放时，服务进程会查一次这条流所在的输出有几个声道（读 `dumpsys media.audio_flinger`，至少隔 30 秒查一次），日志里有一行 `通话一路的输出：2 声道`；如果这台手机把它放到了单声道输出上，就换成媒体用途（日志里有一行 warn），立体声保住，代价是对方的声音跟媒体音量走。
- 延迟：对方的声音晚到约 90–100 ms（和其它声音一样，见[实测结果](#实测结果模拟器android-16)），对话时能察觉到一点。
- 回声：通话 App 的回声消除用的是它自己放出的声音做参考，处理后的声音左右移动、晚一点，开免提时对方可能听到回声。戴耳机没有这个问题（双耳效果本来也要戴耳机听）。
- 不想处理某个通话：暂停拦截（全局页的开关，或者打开了通知时的通知栏），或者只选要处理的 App（`start only=UID`）。
- 普通电话（运营商通话）不走 App 的播放，截不到，也不处理。

## 和网页模式的关系

- **同一份配置**：参数、风格、左右双人、原声开关在两个页面上改的是同一份（`files/auto-asmr.ron`），改了立刻推给服务进程。导入的配置文件、别的 App 用「用其他应用打开」传来的 `.ron` 也一样。
- **左右双人**：全局模式里也有：其他 App 的声音变成一左一右两个人（第 2 个晚滑条上的毫秒数），位置图上是两个点。做法和网页模式相同（共用库的 `LiveMix`），因为服务进程拿到的是整份配置。
- **不会处理两遍**：App 自己的 UID 不在拦截范围里。全局模式开着时在内嵌浏览器里放网页，网页的声音照常由网页模式的 hook 处理，不会再被服务进程截一次。
- **雨声只响一份**：拦截中由服务进程放雨声，App 自己的雨声关掉；暂停拦截或退出服务后，按网页模式的规则改回 App 自己放（交接时淡入淡出）。
- **位置图**：网页没在出声、全局模式在处理其他 App 的声音时，网页页面的状态和位置图显示全局模式的。
- **文件转换页的试听**：全局模式不截本 App 自己的声音，所以文件试听不会被处理两遍。
- **通知**：网页在出声时是网页模式的播放通知，全局模式连着服务进程时是全局模式的通知，两种模式同时用时两条都有。

## 控制协议

命令一行一条，每条有一个回复（`global/server/EngineCore.kt`）。Shizuku 方式经 binder 传（调用号 `FIRST_CALL_TRANSACTION`，参数和回复各一个字符串）；root / adb 方式经标准输入 / 输出：

```
CLASSPATH=<apk> app_process /system/bin io.github.nasuiyile.autoasmr.global.server.ServerMain \
    --apk <apk> [--lib <libasmrglobal.so>] [--client-uid <App 的 UID>] [--start] [--seconds N] [--onset]
```

- `--start`：引擎加载完马上开始拦截。`--seconds N`：不读标准输入，跑 N 秒后退出（调试用）。`--onset`：测延迟时用，每个声音的开头放出来时发一行 `onset`。
- 标准输入里 App 发的命令带编号：`#12 poll`，回复是一行 `=12 <回复>`（回复里的换行写成 `\n`）。手敲的命令不带编号，回复直接写出来。标准输入关掉时停止拦截并退出。
- 命令：
  - `hello`：版本（`ver=`）、身份、系统版本、机型
  - `start`、`start only=UID,UID`（只拦这些 App）、`stop`：回复 `ok` 或 `error …`
  - `config <base64 的 .ron>`：换整份配置（App 用这个，见[进程和启动方式](#进程和启动方式)）；原声开关取配置里的 `dry`（原声时不经过引擎，同网页模式是直接切换）
  - `poll`：JSON，是否在拦截、原声、雨声，`tele`（和网页模式 `NativeHook.telemetry()` 同一排列的 16 个数，含左右双人的两个位置），峰值、延迟各段、断流、日志序号
  - `logs <序号>`：这个序号之后的日志
  - 调试用：`params <base64 的 RON>`（只换 params 那一段）、`dry 0|1`、`style 0..3`、`action <名字>`、`status`、`exit`
- 标准输出另外还有：`hello …`、`log …`、`warn …`、`error …`、`state …`、拦截中每秒一行 `stat …`、`onset …`、`bye`。

`stat` 各项（两路合起来：峰值取大的，断流、读错误加起来，积压和输出缓冲看正在出声的那路）：`in` / `out` 输入 / 输出峰值；`gated` 为真时表示连续 2 秒全是静音，输出流暂停；`lag_in` 是录音端读到的数据离被录下过了多久（毫秒）；`lag_out` 是输出流里还没放出去的量（毫秒）；`engine` 是引擎本身的延迟；`cpu` 是处理占单核的百分比；`underruns` 是输出断流次数；`out_buf` 是输出缓冲的上限（从 30 ms 开始，一断流就加 5 ms）。

背景雨声（雨夜等）在服务进程里单独开一条输出流放：拦截中、不是原声、背景音音量 > 0 时开，关掉时淡出。它的 UID 被拦截规则排除，不会被自己截回来。雨声用媒体用途，跟媒体音量（通话时和别的媒体声音一样被系统压低）。

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

第一次复测的截图里仍是旧包：日志没有 `log 进程环境：…` 这一行，调用栈的行号（`ServerMain.main:89`、`handle:112`）也是改之前的。为了一眼分清跑的是哪一版，从 0.1.1-prototype 起，界面上和服务进程的 `hello` 行都会显示版本号（`hello ver=0.1.1-prototype uid=…`）。合并进安卓版之后，版本号由环境变量 `ASMR_VERSION_NAME` / `ASMR_VERSION_CODE` 给（发布时是 tag 名和运行序号，见[安卓版 · 发布](android-dev.md#发布)）；本机打包发给别人测时也设上它们，`hello` 行和全局页上就是这个版本号。

## 实测结果（模拟器，Android 16）

模拟器：AVD `asmr36`（Pixel 7，Android 16，Apple Silicon），Shizuku 13.6.0（adb 模式）。测试音：Chrome 打开电脑上用 HTTP 提供的 WAV（440 Hz，幅度 0.2），或 `global/server/LatencyProbe.kt` 以 root 身份放的短音（幅度 0.3）。路由情况看 `dumpsys media.audio_flinger`：每个输出线程上挂着哪个 UID 的播放轨。

### 合并进安卓版之后

| 检查 | 结果 |
|---|---|
| 打开 App（Shizuku 在运行、没选过页面） | 直接进全局页 |
| 「用 Shizuku 启动」 | 弹出「Allow auto-asmr to access Shizuku?」，允许后服务进程 `io.github.nasuiyile.autoasmr:engine`（UID 2000）加载 `libasmrglobal.so`，引擎加载 86 ms，自动开始拦截，排除 UID 2000 和 App 自己的 UID |
| 左右双人 | 导入一份 `duo_ms: 80` 的配置（经 App，推整份配置给服务进程）后，探针的声音变成两个：位置图上 1 号在左（方位约 260°）、2 号在右（约 98°） |
| 雨声交接 | 拦截中开「雨夜」：只有服务进程在放；暂停拦截：App 自己的雨声流打开、服务进程的淡出；继续拦截：服务进程的雨声开始、App 的关掉；「关」：服务进程的停下 |
| 网页模式（`tools/hook_test.sh`，Web Audio 用例，全局模式同时开着） | hook 照常截到网页的流；左右互换模式下左右电平对调；ASMR 模式下左右来回（−48 / −10 dB），方位 99° |
| `kill -9` App 进程 | 服务进程（PID 不变）继续处理；约 1 秒后前台服务被拉起，新的 App 进程连回同一个服务进程 |
| 「退出服务」 | 服务进程退出，全局模式的前台服务停下 |
| release 包（R8 精简） | 19.9 MB（两种架构，各两份引擎 .so）；全局模式在 release 包上照常启动、拦截、退出；`NativeHook` 和 `global/server` 的类没被改名 |

### 语音通话

QQ、微光这类 App 里对方的声音没有被处理，原因是它们用"语音通话"用途播放，规则里没有这个用途。模拟方法：`LatencyProbe` 加参数 `voice`，以 root 身份先把音频模式设成 `MODE_IN_COMMUNICATION`，再用 `USAGE_VOICE_COMMUNICATION` 放短音（幅度 0.3）；服务进程用 `tools/global_server.sh --start` 以 shell 身份跑。

| | 改之前（规则：媒体 / 游戏 / 未知） | 改之后（加上语音通话） |
|---|---|---|
| 探针的轨（UID 1000，用途 2） | 在扬声器输出上（原声直接放出来） | 在 `REMOTE_SUBMIX` 上 |
| 服务进程的输入峰值 | 一直是 0 | 0.300（等于探针的幅度），输出 0.88 |
| 扬声器输出上 | 探针的原声 | 服务进程的轨，处理后的声音（当时只有一路、媒体用途；现在通话声音单独一路，见[通话音量](#通话音量)） |
| 普通媒体声音（探针不加 `voice`） | 照常截到 | 照常截到（0.300） |

探针退出后音频模式自动回到 `MODE_NORMAL`。真的 QQ / 微光通话还要在真机上试（见[还没验证的](#还没验证的)）。

### 通话音量

只有一条媒体用途的输出流时，通话中按音量键调不了处理后的对方声音。后来试过通话时把整条输出流换成语音通话用途，按音量键有用了，但同时放的音乐也跟着通话音量变。现在分两路（见[使用](#使用)里的"语音通话"），检查的标准是**拦截前后每种声音的音量完全一样**。

模拟方法：两个探针都以 root 身份跑，一个加 `voice`（先设 `MODE_IN_COMMUNICATION`，再用语音通话用途放短音），一个放媒体；服务进程以 shell 身份跑（`tools/global_server.sh --start`）。中途用 `adb shell cmd audio set-volume <流> <档位>` 改音量（流 0 = 通话，3 = 媒体），每步看 `dumpsys media.audio_flinger` 里各条播放流的音量（`PortVol dB` 一列）。模拟器的扬声器，通话音量 1–15 档，媒体音量 0–15 档，开始时都是 8 档。

| 操作（之后的档位：通话 / 媒体） | 不拦截：探针的媒体流 / 通话流 | 拦截：服务进程的媒体流 / 通话流 |
|---|---|---|
| 开始（8 / 8） | −21 / −12 dB | −21 / −12 dB |
| 通话调到 15（15 / 8） | −21 / 0 dB | −21 / 0 dB |
| 通话调到 1（1 / 8） | −21 / −24 dB | −21 / −24 dB |
| 媒体调到 15（1 / 15） | −21 / −24 dB（媒体被压在通话的 −24 dB + 3 dB） | −21 / −24 dB |
| 媒体调到 3（1 / 3） | −40 / −24 dB | −40 / −24 dB |
| 通话调到 15（15 / 3） | −40 / 0 dB | −40 / 0 dB |

拦截时两个探针的流分别在两个 `REMOTE_SUBMIX` 输出上（0 dB，固定音量），扬声器上是服务进程的两条流：媒体用途（流类型 3）和语音通话用途（流类型 0，在 2 声道的混音输出上，日志 `通话一路的输出：2 声道`）。

| 其它检查 | 结果 |
|---|---|
| 不在通话、只放媒体 | 服务进程的媒体流在放；通话流建好了但没在放（`active=no`），不占输出 |
| 服务进程用 root 身份、两个探针用 shell 身份 | 两路都截到，扬声器上是服务进程（UID 0）的两条流；输入峰值一直是 0.300，没有把自己的声音截回来 |

通话那路万一要换成媒体用途（单声道输出时），要先放掉旧的输出流再用同一个会话号建新的：AudioFlinger 不允许同一个会话号里有不同用途的播放流（日志 `createTrack_l() mismatched strategy`），而会话号不能变（拦截规则靠它排除自己的声音）。正在放的旧流要等混音线程下一轮才撤下来，所以建新流失败时隔 10 ms 再试，最多试 0.3 秒。这条退路在模拟器上没走到过（模拟器上是 2 声道）。

### 去掉悬浮球等之后

不再主动申请通知权限，去掉悬浮球、忽略电池优化、厂商后台管理的说明（见[后台运行](#后台运行)）。模拟器上全新安装 debug 包，没给通知权限：

| 检查 | 结果 |
|---|---|
| 网页页打开测试页放声音 | 不弹通知权限申请；网页的播放通知照样显示在通知栏的媒体控件里（带媒体会话的播放通知不需要这个权限） |
| 全局页「用 Shizuku 启动」 | 只弹 Shizuku 自己的授权，不弹通知权限申请；页面上没有「后台运行」卡片；前台服务在运行（`isForeground=true`），通知栏里没有它的通知 |
| 拦截中（探针在放）`kill -9` App 进程 | 服务进程（PID 不变）继续处理，扬声器上照常是它的输出流；约 1 秒后系统重新拉起前台服务，新的 App 进程连回同一个服务进程（日志「服务进程还在运行，连回去」），前台服务恢复。没有悬浮窗、电池优化白名单这些豁免也一样 |
| 在系统里打开通知（`pm grant … POST_NOTIFICATIONS`）再回到 App | 通知栏出现「auto-asmr 全局模式 · 拦截中 · ASMR 效果（通过 Shizuku）」 |
| 清单 | 不再声明 `SYSTEM_ALERT_WINDOW`、`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`；`POST_NOTIFICATIONS` 保留声明（不主动申请，用户才能在系统设置里打开通知） |

### 0.2.0（单独的 App 时：常驻服务进程、悬浮球、参数面板）

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
| root / adb 通道（当时的 `tools/server.sh`，现在是 `auto-asmr-android/tools/global_server.sh`） | 照常；延迟对照多出 98.6 ms，和之前一致 |

悬浮球和「后台运行」卡片后来去掉了（见[后台运行](#后台运行)）。

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

复现：装好 App 后用 `auto-asmr-android/tools/global_server.sh --start --onset`。探针的第二个参数写 `voice` 时模拟语音通话 App（见[语音通话](#语音通话)）。延迟对照要让探针以另一个身份跑，例如服务进程用 shell，探针用 `adb shell "su 0 sh -c 'CLASSPATH=<apk> app_process /system/bin io.github.nasuiyile.autoasmr.global.server.LatencyProbe 12'"`。

模拟器上测 Shizuku：装官方 APK（GitHub Release），用 `adb shell <Shizuku 的安装目录>/lib/arm64/libshizuku.so` 启动它（安装目录用 `adb shell pm path moe.shizuku.privileged.api` 查），再在 App 的全局页点「用 Shizuku 启动」，或者 `adb shell am start -n io.github.nasuiyile.autoasmr/.MainActivity --es page global --es launch shizuku`（其它测试参数见 `MainActivity.kt` 的类注释）。通知权限可以用 `adb shell pm grant io.github.nasuiyile.autoasmr android.permission.POST_NOTIFICATIONS` 直接给（`pm revoke` 收回，收回时 App 进程会被系统结束）。

## 还没验证的

- **真机**：荣耀 AAP-AN00（Android 17）第一次试在开始录音时出错，已经修改（见[厂商系统的兼容处理](#厂商系统的兼容处理)），还要复测；其它机型都没测。厂商可能改过音频框架或音频策略（华为、小米、三星等），要逐个看。
- **真机上的保活**：常驻服务进程、前台服务被拉起后连回去，在 MagicOS 这类后台管理很严的系统上表现如何。去掉悬浮球和电池优化以后 App 进程更容易被清掉：Shizuku 方式下不影响声音（只是通知要等 App 再起来才恢复），root 方式下拦截会停。Shizuku 本身被清掉时服务进程也会退出。
- **Shizuku 的 root 模式**：只测了 adb 模式（shell 身份）。root 模式下服务进程是 root 身份，这个身份用 adb 测过，但没经过 Shizuku 跑。
- **root 启动**：模拟器自带的 `su` 不让 App 用（只认 shell 和 root），点「用 root 启动」会提示 `Permission denied`。服务进程在 root 身份下已经验证，没验证的是 Magisk / KernelSU 的 `su -c` 和管道转发（0.2.0 的编号请求 / 回复）。
- **硬件直通播放**：用 offload（DSP 解码长音频）或 AAudio MMAP 独占模式播放的 App 会不会按规则被截走。
- **长时间运行**：remote submix 由系统时钟驱动，输出由声卡时钟驱动，真机上两者有细微偏差，时间长了会偶尔断流或丢一小段。现在只靠断流时加大输出缓冲，没有做微调采样率的补偿。
- **蓝牙耳机**：输出走蓝牙时的延迟和行为。通话时蓝牙耳机多半切到通话协议（HFP / SCO，单声道、窄带），处理后的声音也跟着走它，会变成单声道；有线耳机没有这个问题。这时通话音量由耳机那头调，语音通话用途的流在手机上按满音量放，和通话 App 自己的声音一样。
- **真实的语音通话 App**：QQ / 微信通话、微光语音房只用模拟的探针验证过（见[语音通话](#语音通话)、[通话音量](#通话音量)）；真机上要看对方的声音是否被处理、戴耳机时有没有回声、厂商的通话音频路径（有的手机把通话声音走专门的硬件通路）会不会绕开规则、通话那路的立体声输出流会不会被放到单声道输出上（服务进程会查，查声道数用的 `dumpsys` 格式只在 Android 16 模拟器上核对过；查不到时日志写"声道数查不到"，照常用语音通话用途）。

## 构建

随安卓版一起构建（环境见[安卓版](android-dev.md#环境)）：

```bash
cd auto-asmr-android && ./gradlew assembleRelease
```

- `app/src/main/cpp/CMakeLists.txt` 把同一个 Rust 静态库链成两个 .so：`libasmrhook.so`（App 进程：网页模式的 hook、参数和界面）和 `libasmrglobal.so`（全局模式的服务进程，`global_jni.c`，不带 hook）。两种架构（arm64、32 位）都编。
- 服务进程是 shell / root 身份的 app_process，按路径 `System.load` 引擎，所以 `useLegacyPackaging = true`：安装时把 .so 解压到 `/data/app/…/lib/<架构>/`。
- 服务进程按名字加载的类（`ServerMain`、`EngineService`、`Engine`、`LatencyProbe`）和 JNI 在 `app/proguard-rules.pro` 里保留（整个 `global.server` 包）。
- 推送 tag 时，发布工作流的安卓任务把它和网页模式一起打进 `auto-asmr-android.apk`，没有单独的包。

## 代码结构

```
auto-asmr-android/
  app/src/main/cpp/
    global_jni.c          JNI：把 asmr_core.h 的函数转给 global/server/Engine.kt，编成 libasmrglobal.so
  app/src/main/java/io/github/nasuiyile/autoasmr/
    MainActivity.kt       三个页面（网页 / 全局 / 文件转换）的切换、共用的导入导出和提示；adb 测试参数
    Settings.kt           参数（两种模式共用），连着服务进程时把整份配置推过去；雨声由谁放
    global/
      GlobalScreen.kt     全局页：服务进程、效果面板、日志
      Controller.kt       起服务进程（Shizuku UserService / su）、连上、推配置、定时读状态
      Links.kt            到服务进程的连接：binder（Shizuku）、管道（root）
      InterceptService.kt 全局模式的前台服务：通知栏控制、被杀后拉起再连回去
      server/             服务进程（shell / root 身份）
        EngineCore.kt     核心：引擎、拦截、雨声、命令处理、统计、日志
        EngineService.kt  Shizuku UserService 入口（binder）
        ServerMain.kt     app_process 入口（标准输入 / 输出，root、adb 用）
        AudioRouter.kt    注册策略；分媒体、通话两路，每路录音端 → 引擎 → 输出流（媒体 / 语音通话用途）
        RainPlayer.kt     背景雨声的输出流
        ProcessEnv.kt     给服务进程补上 ActivityThread / Application / 包名（厂商框架、身份要用）
        HiddenAudio.kt    音频策略系统接口的反射封装、开始 / 停止录音（必要时绕开厂商代码）、输出声道数、服务进程的 Context
        Engine.kt         Rust 引擎的 JNI 声明，加载 .so 和 APK 里的数据文件
        LatencyProbe.kt   测延迟用的短音播放器
  tools/global_server.sh  用 adb 直接跑服务进程（调试）
```

## 参考

- AOSP Shell 包的权限：`frameworks/base/packages/Shell/AndroidManifest.xml`
- AOSP `AudioRecord.java`、`AudioPolicy.java`、`ActivityThread.java`（`android17-release` 分支）：对照荣耀的报错
- scrcpy 的 `server/src/main/java/com/genymobile/scrcpy/Workarounds.java`：app_process 进程补 ActivityThread / Application 的做法
- Shizuku 的 UserService（`Shizuku.bindUserService`、`peekUserService`、`UserServiceArgs.daemon`）：[Shizuku-API](https://github.com/RikkaApps/Shizuku-API)
- 混音规则的匹配：`frameworks/av/services/audiopolicy/common/managerdefinitions/src/AudioPolicyMix.cpp`（`mixMatch`）
- scrcpy 的音频转发（shell 身份录整机输出，设备本身会变静音）：[doc/audio.md](https://github.com/Genymobile/scrcpy/blob/master/doc/audio.md)
- 同类思路的 App：[RootlessJamesDSP](https://github.com/timschneeb/RootlessJamesDSP)（用 AudioPlaybackCapture，截不到 Chrome、Spotify 等禁止被录的 App）
