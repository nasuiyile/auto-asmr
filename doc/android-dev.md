# 安卓版（开发中）

方案和各步的验证结果见[安卓版技术方案](android-plan.md)。安卓版在同一个 App 里有三个页面，共用一份配置（参数、左右双人、原声开关、文件转换的设置）：

- **网页模式**（下面大部分内容）：一个内嵌浏览器，网页里正在出声的那个标签页的声音，实时交给和桌面版同一份的 Rust 引擎处理，再播放出来。不需要任何权限，没有 Shizuku 时用它。
- **全局模式**：有 Shizuku 或 root 时，把任意 App 的声音截下来处理后重新输出，见[全局模式](shizuku.md)。
- **文件转换**：和电脑版的文件转换页一样，把音频文件转成双耳效果的 WAV，可以先试听、边听边调，见[文件转换](#文件转换)。

## 界面和操作

- **排布**：卡片的内边距、间距都压得比较小，第一屏放得下更多控件；全局页、文件转换页顶上只有一行标题，页面说明都收进标题右边的「?」按钮，点开是一个可以滚动的说明框。
- **三个页面**：网页模式、全局模式、文件转换，用屏幕最下面的导航栏（网页 / 全局 / 文件转换，52 dp 高，比 Material 默认的 80 dp 矮，给页面多留地方）随时切换；网页里全屏看视频、打字时导航栏自动收起。界面跟随系统深色 / 浅色，Android 12 起按壁纸取色。第一次打开时 Shizuku 在运行就进全局页，否则进网页；之后记住上次用的页面。全局页的内容见[全局模式 · 使用](shizuku.md#使用)，文件转换页见[文件转换](#文件转换)。
- **顶栏**（浏览器的控件都在这里）：后退、前进（能前进时才显示）、地址栏（网址直接打开，其它文字用必应搜索；右端是刷新 / 停止）、标签页个数（点开标签页列表）、⋮ 菜单。
- **底栏**（导航栏上面一条）：
  - 开关：**ASMR 效果 / 原声**，就是电脑版的「输出效果 / 监听原声」，存在同一个配置项里。本身就是双耳录音的视频（很多 ASMR 视频是 KU100 录的）切到原声听。
  - 状态：正在处理哪个网页、采样率、延迟、处理占用。点它也能打开效果面板。
  - **效果**：效果面板（从下面弹出）。
- **标签页列表**：🔊 = 这个网页正在出声；点 📍 把某个标签页固定下来，切到别的标签页浏览时它继续出声。
- **⋮ 菜单**：新标签页、首页；桌面版网站、只放一个标签页的声音（开着时右边打勾）；导入配置、导出配置；测试页、录 20 秒处理前后的声音（调试用）。
- **效果面板**（从上到下，和电脑版右侧的参数一致）：
  - 最上面是**效果 / 原声**卡片：开着效果时用主色，下面一行是处理状态；点卡片或开关切换。
  - 风格：**贴耳加强 / KU100 实录 / 掏耳 / 呼吸音 / 听歌** 排成一行（放不下时折到下一行），**恢复默认**在「风格」标题的右边；和电脑版的按钮是同一个函数，改的参数一样。按风格按钮或恢复默认时左右双人一起关掉（各风格的默认状态都是一个声音，同电脑版、插件），文件转换的目标响度跟着换（掏耳 −20、呼吸音 −26、听歌 −14 LUFS，其余 −16）；「听歌」还把排布切回各自固定。**听歌**是给音乐用的：网页、其他 App 的声音左右声道各当一个声源，摆在左前 45°、右前 45°，音色和响度都和原声一致，见[听歌预设](app-usage.md#听歌预设)。选中的风格（当前参数和它一致）高亮、前面带 ✓，判断和电脑版、插件相同；手动调过风格管的参数、打开手动摆位后都不标。
  - 位置图：俯视，上 = 正前方，刻度和电脑版一样。红点是声源此刻的位置（播放中才有），右边竖条是高度。左右双人时是两个点：1 号橙色、2 号蓝色（颜色同电脑版），下面一行写两个声音各在哪。听歌（「立体声宽度」大于 0）时也是两个点：左声道橙色、右声道蓝色，下面一行写「立体声 · 左声道… 右声道…」。
  - **手动摆位**：和电脑版一样，打开后自动移动全停，声源停在你摆的位置（播放中打开时停在此刻的位置）；在位置图里点 / 拖改方位和距离，在竖条上拖改高度，双击竖条回到与耳朵齐平。关掉后回到原来的移动方式。
  - 降噪和**输出增益**放在同一张卡片里（同电脑版左下角）。降噪 0 = 关闭（网页里的声音一般很干净，不用开）。输出增益只调实时 / 试听听到的音量（相当于耳机音量），下面一行小字写明转换文件不用它；它不在「输出」组里重复出现。
  - **左右双人**（各组参数的第一组，关着时折叠）：和电脑版「文件转换」页的[左右双人](app-usage.md#左右双人一个音频当成两个人)是同一个设置（电脑版实时页的延迟另外记）——网页里的声音变成一左一右两个人，第 2 个晚滑条上的毫秒数（0–300，0 = 关；50–120 ms 最像两个人）。打开后下面是**排布**（各自固定：1 号固定在左耳、2 号在右耳；一起移动：按「左右移动」一起走，始终在头的两侧相对；从 0 打开时自动切到一起移动，同电脑版）和**自动平衡**（0–100 %，让两个声音每一段时间差不多一样响；网页没有"整段"，响度边听边算）。两个声音的位置固定在左耳、右耳（电脑版的默认位置），不能单独拖。动作严格跟着说话走（同电脑版试听）；只有第 2 个声音晚，延迟不变。开 / 关、换排布的那一下会重新开始；改延迟时 20 ms 里淡过去。处理量约是平时的两倍。全局模式里同样有效（处理其他 App 的声音），用的是同一个设置。
  - 各组参数：贴耳感、入耳（掏耳）、左右移动、背景音（雨声，带「雨夜」「关」按钮）、忽远忽近、上下、KU100 临场感（耳廓、回放补偿）、听歌（音乐：立体声宽度、音色校正、低切、房间声低切；没用上时折叠）、人声处理、输出（自动响度、目标响度、和原声一样响、峰值上限、随机种子）。标签、范围、单位、说明（点标签旁的 ⓘ 图标展开）都和电脑版相同；只在某种移动方式下才有用的参数只在那时显示，手动摆位时忽远忽近、上下两组变灰——都和电脑版一样。
  - 后面依次是「声源位置」（位置图和手动摆位开关）、降噪和输出增益、「全部参数」下的各组参数（卡片，点标题展开 / 收起），开关类的参数是右边的开关。
  - 「配置文件」：导入配置 / 导出配置；「关于」：本项目 GitHub（「给个star」）和研发人 B 站主页（「随意Official」）的链接（同电脑版右下角）。GitHub 用系统浏览器打开；B 站先用 `bilibili://space/<uid>` 唤起 B 站 App（手机版、HD 版、国际版都认）直接进研发人主页，没装 B 站 App 时用系统浏览器打开网页版主页。App 自带的首页底部也有这两个链接：B 站的同样优先唤起 App，没装时在当前标签页里打开网页版主页（网站自己发出的 `bilibili://` 跳转照旧拦掉）。
- **通知栏**：App 开着时，下拉通知栏随时找得到 auto-asmr，可以**启用 / 关闭效果**（同底栏的 ASMR 开关）。第一次打开时问一次通知权限（Android 13 起要用户同意）；拒绝了就不再问，通知栏里不显示（网页在出声时的播放通知除外，带媒体会话的播放通知不需要这个权限），可以在系统设置里打开，回到 App 时通知就会出来。
  - 没有声音时是一条常驻通知「auto-asmr 正在运行 · ASMR 效果已启用 / 已关闭（原声）」，按钮是「关闭效果 / 启用效果」和「退出」。「退出」停掉网页声音和雨声、关掉界面（移出最近任务）和这条通知；全局模式、正在转换的文件不受影响。把 App 从最近任务里划掉时，没声音的话通知也跟着消失。
  - 全局模式连着服务进程时由它的通知代替（见[全局模式的后台运行](shizuku.md#后台运行)），这条不重复显示。
- **后台播放**：网页在出声时通知换成带媒体会话的播放通知，切到后台、锁屏后网页接着放、接着处理；通知栏、锁屏和耳机按键可以**暂停 / 继续**，通知栏还能**启用 / 关闭效果**。Android 13 起通知栏顶部的媒体控件只认媒体会话里的按钮，所以这个开关也作为会话的自定义按钮放进去（媒体控件左下角，效果开着是耳机，关着是带斜线的耳机）。没声音 30 秒后换回常驻通知；从通知栏暂停的留 10 分钟。只有正在出声的那个标签页在后台继续，别的标签页照常暂停。
- **原声时的音量**：关掉效果（原声）时声音不进引擎，但仍乘效果面板里的「输出增益」、过峰值限制器（和电脑版一样），所以音量滑条照样起作用；限制器带来约 2 ms 的延时。
- **背景音（雨声）**：单独一条输出流，网页暂停、换网页时也不断；切到原声或关掉时淡出。
- **截不到声音时**：底栏上方会出现红色提示。先自动换成备用的截取方式（声音会停几秒再恢复）；还不行就提示这个 WebView 不支持——声音照常播放，只是没有效果。
- **只放一个标签页的声音**（默认开）：其它标签页静音，引擎只处理当前标签页（或固定的那个）。关掉后所有标签页都出声、都处理；但看不见的标签页里的视频会被浏览器引擎自动暂停，只有纯音频会继续。
- **网站兼容**：B 站的手机版网页不让在网页里播放，所以打开 B 站时自动用桌面版网页；其它网站可以在菜单里手动切「桌面版网站」。唤起 App 的链接（`bilibili://` 等）直接拦掉，留在网页里。网页视频可以全屏。

## 文件转换

导航栏上的「文件转换」页和电脑版的[文件转换](app-usage.md#文件转换)页功能一一对应：Rust 层的 `files.rs` 照搬桌面版 `gui.rs` 文件页的逻辑，解码、两遍处理、批量、合成、对齐、自动平衡调用的都是共用库里同一份代码，所以同样的文件、同样的配置转出来的结果和电脑版一样。

- **页面分区**（从上到下）：音频文件、多人声音（多音频合成、左右双人）、试听与转换（试听 / 停止、转换按钮、播放条、进度）、输出设置（输出位置、声道、位深、起止时间、目标响度、同时转换），下面是效果面板。
- **添加**：「添加文件」打开系统的文件选择器，可以多选；「添加文件夹」加入文件夹里的全部音频（不含子文件夹，跳过已转换的 `_asmr` 文件）。支持 wav / m4a / mp3 / flac / ogg 等常见格式。能直接读的文件原地读，不复制；读不了的（部分云盘等不能随机读的来源）复制一份到 App 缓存，下次启动时清掉。列表里点右边的 × 去掉一个，点名字选中它（一个一个试听时听的是选中的那个），转换完点它看这个文件的结果说明。
- **试听**：「试听」按钮开始后变成「停止」。从手机的默认输出（媒体音量）播放处理后的声音，播放条点 / 拖到哪里就从哪里听；下面的效果面板边听边调，立刻生效（同电脑版）。多音频合成、左右双人时几个声音同时播放，位置图上是各声音此刻的位置。换风格、改参数时接着放；切到别的页面时试听不停。耳机拔掉等输出设备出错时自动停止并提示。
- **转换并保存**：一个文件时是「转换并保存」，几个时「全部转换并保存（N）」，合成时「合成并保存」。每行显示等待中 / 进度 / 完成（用时）/ 失败，上面是总进度和剩余时间，转换中转换按钮变成「取消转换」（没写完的输出删掉）；完成后列表下面是结果说明（时长、响度、真峰值、存到了哪里，合成时还有各段的对齐和平衡前后的响度差），同电脑版。**同时转换** = 同时处理几个文件，默认 = 逻辑核数，长的先处理；整段读进内存，长音频每小时约 0.7 GB，内存小的手机调小。
- **输出位置**：默认放在源文件所在的文件夹（用「添加文件夹」加的，对应电脑版的"源文件旁边"），否则放进「音乐/auto-asmr」；名字加 `_asmr`，重名时加 `_2`、`_3`。一个文件或合成时可以用「输出到…」自己选文件名和位置；几个文件时可以用「输出文件夹…」选一个文件夹（会记住，「放回默认」取消）。
- **多音频合成**：打开开关后列表里的前 4 个音频各在一个位置同时播放，合成一个文件。每个音频可以选位置（左耳、右耳、正前、正后、左前……）和音量（±dB），也可以在位置图上拖；**排布**（各自固定 / 一起移动）、**自动平衡**、**自动对齐**（同一首歌的几个版本从一起开始唱的地方叠加，下面写出每段从哪里开始用、裁掉多少）都和电脑版一样，见[多音频合成](app-usage.md#多音频合成)。
- **左右双人**：把一个音频当成两个人（第 2 个晚 0–300 ms，0 = 关），两个声音的位置、音量、排布、自动平衡都可以改；批量转换时每个文件都这样做，输出比原音频长出这段延迟。见[左右双人](app-usage.md#左右双人一个音频当成两个人)。和网页模式、全局模式的左右双人是同一个设置。
- **位置图**：合成或左右双人时，效果面板里的位置图换成合成的位置图（同电脑版）：彩色实心点是各声音此刻的位置（试听中带一小段轨迹），旁边是编号，同色的圈是摆好的位置；右侧竖条是各声音的高度。各自固定时拖圈改那个声音的位置；一起移动 + 手动摆位时拖任意一个挪整组（其余的保持错开的角度），在竖条上拖高度，双击竖条回到与耳朵齐平。
- **听歌**：点了「听歌」风格后，立体声的音频左右声道都解码、各当一个声源（试听时位置图上是左右两个点）；多音频合成、左右双人里每个音频左右混合成一个声源，两首歌一左一右各在一边。响度"和原声一样"：目标响度取原声的响度和 −14 LUFS 里较低的那个，完成后的说明里写着。做法同电脑版，见[听歌预设](app-usage.md#听歌预设)。
- **其它选项**：取哪个声道（自动 / 左 / 右 / 混合；「自动」平时是更响的那个声道，听歌时单个音频是立体声、合成里是左右混合，按钮上会写出来）、位深（24 / 16 bit）、从第几秒 / 处理多少秒（0 = 到结尾）、目标响度（−24 … −10 LUFS）。
- 这些设置（目标响度、声道、位深、同时转换、合成和自动对齐的开关、左右双人的延迟、排布、自动平衡）存在配置文件里，和电脑版通用；文件列表、合成的位置和音量和电脑版一样不存。
- 和电脑版的差别：没有"把文件拖进窗口"（用选择器）；试听固定从默认输出放，不能选输出设备；转换、试听没有前台服务保护，切到后台久了有的手机会把 App 停掉，长的批量转换最好留在前台。

## 配置文件

和电脑版**通用，同一个格式**（读写代码在共用库 `auto-asmr/src/config.rs` 里，两边是同一份）：

- 电脑版「导出配置」的 `.ron`、电脑版运行目录里自动保存的 `auto-asmr.ron`，都可以在安卓版「导入配置」；安卓版「导出配置」的文件也能在电脑版导入。
- 「贴耳感」里的「呼吸去闷」设置 100–300 Hz 能量堆积时的最大衰减量（0–12 dB），随配置导入和导出。
- 同栏的「薄声补偿」写入配置字段 `breath_tone`（0–1）。穿梭模式下的「换气时换边」写入 `breath_pause_switch`，开启时在达到停留时间后等待换气间隙再移动。两项都随配置导入、导出。
- 在「KU100 临场感」中可以切换「固定原声底噪」，让网页里持续的录音底声保持在固定位置。默认关闭（各风格都不开启），开关状态随配置一同保存。
- 安卓版用得上的是全部效果参数、「输出效果 / 监听原声」和左右双人（延迟、排布、自动平衡）。电脑版独有的设置（音频设备、缓冲、文件转换、多音频合成、语言、3D 视图）安卓版不显示、不改，但会原样保留，导出后带回电脑照样能用。
- 安卓版自己的设置也存成这个格式（App 私有目录的 `files/auto-asmr.ron`），改动后 1 秒自动保存，下次打开时读回。
- 文件里没有 auto-asmr 设置（比如只有窗口状态的文件）时提示「文件里没有 auto-asmr 的配置」，当前设置不变，和电脑版一样。

### 从别的 App 直接打开

QQ、微信、文件管理器里收到的 `.ron` 配置文件，点「用其他应用打开」，列表里会有 **auto-asmr**。选它之后：

- App 先读出文件、只检查不导入（和电脑版同一份解析代码）。是 auto-asmr 的配置，就弹窗显示文件名和概要（风格、左右移动方式、贴耳距离、入耳、房间、降噪、背景雨声、原声），点**导入**才导入，效果和菜单里的「导入配置」完全一样；点**取消**什么都不改。
- 不是配置（比如普通文本、只有电脑版窗口状态的文件、太大的文件），就提示「不是 auto-asmr 的配置文件」，当前设置不变。
- App 没在运行时也行：它会先启动、等引擎加载完再导入，不会被启动时读取的旧配置盖掉。

只有文件名以 `.ron` 结尾的文件才会出现 auto-asmr（不分大小写，重名时加的「(1)」也行），别的文件的打开列表里不会有它。这要求对方传来的文件地址里带文件名——QQ、微信、系统「文件」App 都带；个别 App 的地址里没有文件名（只有一串编号），这时列表里不会出现 auto-asmr，可以先把文件存到手机里，再用菜单里的「导入配置」。

测试：在模拟器里把配置文件推到下载目录，用系统「文件」App 点它，选「用 auto-asmr 打开」。也可以只看匹配规则对不对（地址里的文件名、MIME 类型随便换）：

```bash
adb shell cmd package query-activities --brief -a android.intent.action.VIEW -d '"content://com.tencent.mobileqq.fileprovider/x/auto-asmr-export.ron"' -t '"application/octet-stream"'
```

## 环境

在 macOS（Apple Silicon）上用命令行搭的，不需要 Android Studio（以后想用它打开工程也可以）。都装在用户目录：

| 组件 | 位置 / 版本 |
|---|---|
| JDK | Temurin 21，`~/Library/Java/JavaVirtualMachines/temurin-21.jdk` |
| Android SDK | `~/Library/Android/sdk`：platform-tools、platforms android-36 / 37.0 / 37.2（新版 Compose 要求用 37.1 以上编译，工程用 37.2）、build-tools 36.0.0、CMake 3.31.6、NDK 29.0.14206865、模拟器、系统镜像 android-36 google_apis arm64-v8a |
| 模拟器 | AVD `asmr36`（Pixel 7，Android 16） |
| Gradle | 9.9.2，用工程里的 `./gradlew`；Android Gradle Plugin 9.5.1；Kotlin 2.4（由 Compose 编译插件 2.4.21 带进来，AGP 自带的 2.2 读不了新版 Compose 库）；Compose BOM 2026.11.01 |
| Rust | 和桌面版同一套工具链，另加两个安卓目标：`rustup target add aarch64-linux-android armv7-linux-androideabi` |

每个终端先设好环境变量：

```bash
export JAVA_HOME="$HOME/Library/Java/JavaVirtualMachines/temurin-21.jdk/Contents/Home"
```

```bash
export PATH="$HOME/Library/Android/sdk/platform-tools:$HOME/Library/Android/sdk/emulator:$PATH"
```

工程目录下的 `local.properties` 写着 SDK 位置（`sdk.dir=…`），不进仓库，换机器时自己建一个。

## 构建

```bash
cd auto-asmr-android && ./gradlew assembleDebug
```

生成 `app/build/outputs/apk/debug/app-debug.apk`。发布版用 `./gradlew assembleRelease`（R8 精简，约 23 MB），生成 `app/build/outputs/apk/release/app-release.apk`；签名见[发布](#发布)。

- CPU 架构：`arm64-v8a` 和 `armeabi-v7a`（少数 32 位低端机）。Apple Silicon 上的模拟器是 arm64；32 位只编译过，没有设备实测。
- KU100 / 耳廓数据（`auto-asmr/assets/ku100.bin`、`pinna.bin`，共 15.7 MB）作为 APK 资源，两种架构共用一份，App 启动时读进引擎。共用库为此打开了 `external-data`（数据不编进 .so），文件转换另外打开了 `files`（解码 symphonia、写 WAV hound）；`libasmrhook.so` 约 5.6 MB（arm64）/ 4.2 MB（32 位）。
- Rust 部分不用单独编：`app/src/main/cpp/CMakeLists.txt` 每次构建都会按架构调用 `cargo build --release --target …`（在 `rust/` 下，没改动时很快），把静态库链进 `libasmrhook.so`，再链一份进全局模式服务进程用的 `libasmrglobal.so`（大小相近）。引擎一律按 release 编译，debug 版的 App 也一样。
- 全局模式的服务进程按路径加载 .so，所以 .so 在安装时解压出来（`useLegacyPackaging = true`），见[全局模式 · 构建](shizuku.md#构建)。

Rust 层自己的测试在电脑上跑：

```bash
cd auto-asmr-android/rust && cargo test --release
```

共 15 个：文件转换走和界面相同的 JSON 命令：加入两个文件后播放条的长度对，全部转换后两个输出都在这次任务的暂存目录里、都等着交给界面，交好后列表里显示最终位置；合成时一左一右、改过的音量写进说明、输出一个文件（在下一次任务的目录里）；听歌（文件转换）：按了「听歌」后立体声文件左右声道都解码、转换的说明里写着立体声和响度、目标响度换成 −14，左右双人里改成左右混合，切回贴耳加强后重新解码成一个声源；听歌（网页 / 全局）：4 号风格下流的左右声道各当一个声源（状态里记 2：左声道在左前 45°、右声道在右前 45°），只在左声道的内容左耳响几分贝，48 / 44.1 kHz 都不断流，响度和原声差不多，立体声宽度拖到 0 就合成一个声源，这时打开左右双人是各自固定；44.1 / 48 / 32 kHz 的流在各种回调长度下不断流、确实被空间化、频率不跑偏；左右双人：打开后 1 号在左、2 号在右、左右两边差不多一样响（48 / 44.1 kHz 都不断流），打开时排布切到一起移动、播放中改延迟只换配置（不切排布）、换排布时重新建、拖回 0 回到单个引擎，值的类型不对时报错；网页声音里没有雨声（打开雨声后再改别的参数，网页没声音时输出仍是静音，左右双人时也一样）；原声时（48 / 44.1 kHz）两耳一样、0 dB 时音量不变、输出增益 −12 dB 时响度正好降 12 dB、调大后被峰值上限压住，切回效果后又有空间感；44.1 / 48 / 32 kHz 的流在各种回调长度下不断流、确实被空间化、频率不跑偏；别的 App 传来的文件：电脑版 / 安卓版的配置能认出来并给出概要（风格能认出是哪个预设）、只有窗口状态的文件和普通文本认不出来、检查不改当前设置；单独输出的雨声响度和桌面版引擎里的一样、关掉后淡出停下、网页声音的配置里没有雨声；参数表逐项对得上 `Params`（键存在、范围两端设得上去、每个选项都能设）；按字段改参数和按钮走的是和电脑版相同的逻辑，未知参数、类型不对时报错且参数不变；五个风格按钮和恢复默认都关掉左右双人、背景音按钮不动它，「听歌」把排布切回各自固定、文件转换的目标响度换成 −14（切回人声的风格恢复 −16）；电脑版配置导入后再导出，电脑版独有的设置都还在；播放中打开手动摆位时换算出的位置和运动模型互逆；参数 RON 读写和风格切换。

### 性能基准

`rust/examples/bench.rs`：60 秒合成人声，按 Chromium 的回调长度（4360 帧）送进处理器，分别按 48 kHz 和 44.1 kHz 计时。电脑上：

```bash
cd auto-asmr-android/rust && cargo run --release --example bench
```

在手机 / 模拟器上：先用 NDK 的 clang 当链接器编译，再推上去运行。

```bash
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$HOME/Library/Android/sdk/ndk/29.0.14206865/toolchains/llvm/prebuilt/darwin-x86_64/bin/aarch64-linux-android29-clang cargo build --release --example bench --target aarch64-linux-android
```

```bash
adb push target/aarch64-linux-android/release/examples/bench /data/local/tmp/asmr_bench && adb shell /data/local/tmp/asmr_bench
```

## 发布

推送 tag 时，`.github/workflows/release.yml` 的 Android 任务会装好 SDK 组件、Rust 最新稳定版和两个安卓目标，跑 Rust 测试，用 `assembleRelease` 打 APK（版本名 = tag 名，版本号 = Actions 的运行序号），检查包里两种架构都有 `libasmrhook.so` 和全局模式的 `libasmrglobal.so`、按名字加载的类没被 R8 改名，和其它版本一起放进 GitHub Release（`auto-asmr-android.apk`）。

**签名密钥**：安卓只允许用同一把密钥签名的新版本覆盖安装旧版本，所以要准备一把固定的密钥，放进仓库的 Secrets。只需要做一次：

1. 生成密钥库（`keytool` 在 JDK 里；密码自己定，密钥库文件和密码要自己保存好，丢了就没法再发能覆盖安装的更新）：

   ```bash
   keytool -genkeypair -keystore auto-asmr-release.jks -alias auto-asmr -keyalg RSA -keysize 4096 -validity 36500 -dname "CN=auto-asmr"
   ```

2. 在 GitHub 仓库的 Settings → Secrets and variables → Actions 里加 4 个 secret：`ANDROID_KEYSTORE_BASE64`（`base64 -i auto-asmr-release.jks` 的输出）、`ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS`（上面是 `auto-asmr`）、`ANDROID_KEY_PASSWORD`。

没配这些 secret 时也能发布：APK 用 Actions 临时生成的调试密钥签名，装得上，但每次构建的密钥都不同，新版不能覆盖安装旧版（Actions 里会给出警告）。

本机打带签名的发布版：设好 `ASMR_KEYSTORE`（密钥库路径）、`ASMR_KEYSTORE_PASSWORD`、`ASMR_KEY_ALIAS`、`ASMR_KEY_PASSWORD` 再 `./gradlew assembleRelease`；没设时用本机的调试密钥签名。`ASMR_VERSION_NAME`、`ASMR_VERSION_CODE` 可以指定版本。

## 运行

- **模拟器**：`emulator -avd asmr36` 打开带窗口的模拟器；只跑自动验证时可以加 `-no-window -no-audio`（没有声音设备时 AAudio 回调照常进行）。
- **真机**：手机上打开「开发者选项 → USB 调试」，用 USB 连上，`adb devices` 能看到设备即可。
- 安装：`adb install -r app/build/outputs/apk/debug/app-debug.apk`，桌面上的图标叫「auto-asmr」。

## 调试

测试页（菜单 →「测试页」，`app/src/main/assets/test.html`）有六种出声方式：Web Audio 测试音、`<audio>` 播放页面生成的 WAV（48 kHz 和 44.1 kHz 各一个）、跨域 mp4 直链、原生 HLS、hls.js（MSE）。测试音左边 0.3 × 440 Hz、右边 0.1 × 660 Hz，所以处理前应当是左 −13.5 dB、右 −23.0 dB。

「录 20 秒处理前后的声音」录下一个出声的流 20 秒，处理前（网页直出）和处理后各存一个 24 位 WAV 到 `/sdcard/Android/data/io.github.nasuiyile.autoasmr/files/`（`rec_<时间>_dry.wav` / `_wet.wav`），用 `adb pull` 拉回来对比着听。

也可以用 adb 驱动（都作用于当前标签页）：

| 参数 | 作用 |
|---|---|
| `--es url <网址>` | 打开网址；测试页加 `?auto=webaudio`（或 `element`、`element44`、`mp4`、`hls`、`hlsjs`）自动开始播放 |
| `--ei mode 0…3` | 调试模式：直通 / ASMR / 左右互换 / 静音（界面上的开关只有 ASMR / 原声） |
| `--ei style 0…4` | 贴耳加强 / KU100 实录 / 掏耳 / 呼吸音 / 听歌 |
| `--es params '<RON>'` | 整段换参数（RON 格式的 `Params`，缺的项用默认值） |
| `--ei rec <秒>` | 录音，同菜单里的录音 |
| `--es ua desktop` / `mobile` | 桌面版 / 手机 UA |
| `--es mute 1` / `0` | 静音 / 取消静音这个 WebView |
| `--es import <文件名>` / `--es export <文件名>` | 从 / 往 `/sdcard/Android/data/io.github.nasuiyile.autoasmr/files/` 导入 / 导出配置 |
| `--es action <按钮>` | 效果面板上的按钮：`style_ear`、`style_ku100`、`style_earclean`、`style_breath`、`reset`、`bg_rain_night`、`bg_off` |
| `--es page web` / `global` / `files` | 切到哪个页面 |
| `--es files_add <文件名,…>` | 文件转换页加入 `/sdcard/Android/data/io.github.nasuiyile.autoasmr/files/` 里的这些文件（逗号分隔） |
| `--es files_op '<JSON>'` | 文件转换页执行一条命令，和界面发给 Rust 的命令相同，例如 `{"op":"preview"}`、`{"op":"seek","s":20}`、`{"op":"stop"}`、`{"op":"convert"}`、`{"op":"cancel"}`；命令都在 `rust/src/files.rs` 的 `handle` 里 |

```bash
adb shell am start -n io.github.nasuiyile.autoasmr/.MainActivity --es url "'https://appassets.androidplatform.net/assets/test.html?auto=hls'" --ei style 1
```

JSON 里有逗号，整条命令要用双引号包起来交给设备上的 shell（不然逗号会被展开）：

```bash
adb shell "am start -n io.github.nasuiyile.autoasmr/.MainActivity --es page files --es files_op '{\"op\":\"seek\",\"s\":20}'"
```

日志标签：`AsmrHook`（hook 层、雨声流和 App）、`AsmrCore`（Rust 引擎）、`AsmrSettings`（设置）、`AsmrBrowser`（标签页、发声检测）、`AsmrPlayback`（前台服务、自检）、`AsmrPage`（网页的 console）、`AsmrFiles`（文件转换的读写）。

```bash
adb logcat -s AsmrHook:V AsmrCore:V AsmrSettings:V AsmrBrowser:V AsmrPlayback:V AsmrPage:V AsmrFiles:V
```

**测截取方式**：App 私有目录里的 `files/hook_mode` 决定启动时装哪种 hook——没有这个文件 = PLT hook（ByteHook，默认）；`inline` = 只用备用的 inline hook（ShadowHook）；`both` = 两种都装；`none` = 都不装（用来测自检：网页出声 4 秒后自检会自动换成 inline hook）。只有调试版能写（`run-as`），改完重启 App：

```bash
adb shell run-as io.github.nasuiyile.autoasmr sh -c "'echo inline > files/hook_mode'"
```

```bash
adb shell run-as io.github.nasuiyile.autoasmr rm files/hook_mode
```

**测后台播放**：放着声音时按 Home（`adb shell input keyevent 3`）、关屏（`keyevent 223`），看日志里每秒的回调还在不在；媒体按键：`adb shell cmd media_session dispatch pause` / `play`。

### 自动验证

`tools/hook_test.sh` 依次跑测试页的出声方式：每种重启 App、自动播放，先用左右互换核对 hook，再切直通、静音，最后切到 ASMR 看引擎状态；日志存到 `hook-test-out/`（不进仓库），并打印每种的流格式、左右互换和 ASMR 时的电平与引擎状态。WebView 升级、换手机之后跑一遍，就知道 hook 和引擎还正不正常。

```bash
cd auto-asmr-android && tools/hook_test.sh
```

- 只跑其中几种：`CASES="webaudio hls" tools/hook_test.sh`
- 同时录音并拉回 `hook-test-out/`：`REC=20 CASES=hls tools/hook_test.sh`
- 打开任意网页：`URL=https://www.bilibili.com/video/BV… WAIT=25 tools/hook_test.sh`

## 代码结构

| 路径 | 内容 |
|---|---|
| `app/src/main/java/…/MainActivity.kt` | 主界面（Compose）：底部导航栏切换网页 / 全局 / 文件转换三个页面，网页页面的顶栏（地址栏、标签页个数）、WebView、底栏、标签页列表、菜单、视频全屏，两个页面共用的文件选择器导入导出和提示；adb 参数 |
| `app/src/main/java/…/global/` | 全局模式（Shizuku / root）：全局页、起服务进程和连接、前台服务，以及 `server/`（服务进程），见[全局模式 · 代码结构](shizuku.md#代码结构) |
| `app/src/main/java/…/files/` | 文件转换页：`FilesScreen.kt`（界面：文件列表、合成 / 左右双人、输出位置、选项、按钮、播放条，下面是效果面板）、`MixMap.kt`（合成 / 左右双人的位置图）、`FileSession.kt`（把操作交给 Rust、轮询状态；用系统选择器选的文件交给 Rust 读——能原地读的建符号链接指向打开的文件描述符，不能的复制到缓存；转换结果从暂存目录存到最终位置：选的文件、选的文件夹、源文件所在的文件夹或「音乐/auto-asmr」） |
| `app/src/main/java/…/Browser.kt` | 标签页和 WebView：UA、拦截唤起 App 的跳转、B 站用桌面版、发声检测和暂停 / 继续的脚本（文档开始时注入，经 WebMessageListener 收发）、只放一个标签页的声音（`setAudioMuted`）、固定标签页、正在出声的标签页在后台仍对 Chromium 报告可见、重开音频流 |
| `app/src/main/java/…/EffectPanel.kt` | 效果面板：按参数表生成分组、滑条、勾选、选项、种子（文件转换页也用它，不显示原声开关、位置图换成合成的） |
| `app/src/main/java/…/PositionMap.kt` | 位置图和高度竖条（刻度和电脑版相同），手动摆位时点 / 拖 |
| `app/src/main/java/…/Settings.kt` | 设置（两种模式共用）：后台线程里调用 Rust 改参数（同一个参数连续改只送最新值）、读回全部参数、1 秒后自动保存、导入导出、手动摆位开关的逻辑；全局模式连着时把整份配置推给服务进程；雨声由谁放 |
| `app/src/main/java/…/SharedImport.kt` | 别的 App「用其他应用打开」传来的 `.ron` 文件：读出、只检查不导入、弹窗确认后导入 |
| `app/src/main/java/…/AppLinks.kt` | GitHub、B 站主页的链接：B 站优先唤起 B 站 App，没装时退回网页 |
| `app/src/main/java/…/Ui.kt` | 界面共用：主题（深浅色、Android 12 起按壁纸取色）、图标（Material 图标的路径，不另引图标库）、分区卡片、页头（标题和「?」说明框） |
| `app/src/main/java/…/SpecUi.kt` | 参数表的小工具：数值显示格式、显示 / 可用条件、对数滑条 |
| `app/src/main/java/…/Playback.kt` | 播放的总控（每秒一次，不依赖界面）：什么时候开 / 关前台服务（界面开着或有声音时）、常驻通知还是播放通知、暂停 / 继续、通知栏退出、截取自检和换备用 hook |
| `app/src/main/java/…/PlaybackService.kt` | 媒体播放类型的前台服务：没声音时的常驻通知（启用 / 关闭效果、退出），有声音时的播放通知（暂停 / 继续、启用 / 关闭效果）、MediaSession（锁屏、耳机按键、媒体控件里的效果开关） |
| `app/src/main/java/…/AsmrApp.kt` | App 启动时初始化 ByteHook、登记 hook（必须在第一个 WebView 之前），后台读数据文件、加载引擎和上次的配置 |
| `app/src/main/java/…/NativeHook.kt` | JNI 接口 |
| `app/src/main/assets/home.html`、`test.html` | 首页、测试页 |
| `app/src/main/cpp/aaudio_hook.c` | hook 层：换掉 `AAudioStreamBuilder_setDataCallback` / `AAudioStreamBuilder_openStream` / `AAudioStream_close`——默认用 ByteHook 的 PLT hook（只截 WebView 等调用方），备用 ShadowHook 的 inline hook（改 libaaudio 本身），两种共用一套逻辑、防重入、不截自己的流；流打开时给立体声 float 流建 Rust 处理器，在 Chromium 的数据回调之后原地处理；背景雨声的输出流（开、淡出后停、断开重开）；给界面的实时状态；调试录音；JNI 接口。回调里不分配内存、不加锁、不打日志 |
| `app/src/main/cpp/asmr_core.h` | Rust 层导出的 C 接口 |
| `app/src/main/cpp/CMakeLists.txt` | 按架构调用 cargo 编 Rust 静态库，和 hook 层一起编成 `libasmrhook.so`，链接 ByteHook、ShadowHook（prefab）、libaaudio；同一个静态库再和 `global_jni.c` 编成全局模式服务进程用的 `libasmrglobal.so`；16 KB 页对齐 |
| `app/src/main/cpp/global_jni.c` | 全局模式服务进程的 JNI 层（`global/server/Engine.kt`） |
| `app/proguard-rules.pro` | 发布版 R8 的保留规则（JNI 类名、方法名不能改；全局模式服务进程按名字加载的 `global.server` 包） |
| `app/src/main/res/` | 图标：启动图标（自适应图标，前景和底色由仓库根目录的 `icon/make_icons.py` 生成，见[图标](development.md#图标)）、通知栏小图标 |
| `rust/src/lib.rs` | Rust 层的 C 接口和每个流的处理器（必要时左右声道各自重采样到 48 kHz → 共用库的 `Solo`：平时左右合成单声道过一个引擎，「立体声宽度」大于 0（听歌）时左右声道各一个声源；左右双人时合成单声道交给 `LiveMix` → 转回原采样率）；文件转换的命令入口 `asmr_files`；导出函数都 `catch_unwind`，出错的流改成直通 |
| `rust/src/settings.rs` | 全局设置：整份桌面版格式的配置（`Saved`）、两份引擎配置（完整的给雨声，去掉背景音的给网页声音）、左右双人的合成配置、按字段读写参数（含界面换算的虚拟字段，左右双人、合成和文件转换那几项）、按钮、导入导出、参数表 |
| `rust/src/files.rs` | 文件转换：照搬桌面版 `gui.rs` 文件页的逻辑（文件列表、试听、批量转换、合成、自动对齐、左右双人、结果说明），界面通过 JSON 命令（`asmr_files`）操作它、读回整页状态；设置读写和配置文件同一份（`settings.rs`） |
| `rust/src/aout.rs` | 文件转换试听用的输出流（AAudio），回调里调用共用库的 `live::Pump`（和桌面版试听的声卡回调做的是同一件事） |
| `rust/src/rain.rs` | 背景雨声单独输出：和桌面版引擎同样的 `sync_rain` + `Rain::render` + 限制器，关掉时淡出 |
| `rust/src/ui_spec.json` | 参数表：分组、标签、说明、范围、单位，取自桌面版界面；耳廓和耳机型号的选项运行时填 |
| `rust/examples/bench.rs` | 性能基准 |
| `tools/hook_test.sh` | 自动验证（网页模式） |
| `tools/global_server.sh` | 用 adb 直接跑全局模式的服务进程（调试） |

引擎和设置文件格式在 `auto-asmr/src/`（桌面版的库：`dsp/`、`config.rs`，文件转换用的 `offline.rs`、`batch.rs`、试听用的 `live.rs`，见[构建、发布与开发](development.md#代码结构)）。文件转换页照搬桌面版 `gui.rs` 文件页的逻辑：改了桌面版文件页的功能时，同步改 `rust/src/files.rs` 和 `files/` 下的界面。参数表里的标签和说明是从桌面版 `gui.rs` 抄过来的：改了桌面版的参数界面时，同步改 `ui_spec.json`（`cargo test` 会查出参数表里不存在的参数）。
