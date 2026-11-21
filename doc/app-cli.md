# 命令行（Rust 版）

不带参数启动是图形界面；带子命令时在终端运行。

```bash
auto-asmr convert 输入.m4a -o 输出.wav --style ku100 --duration 60
```

KU100 实录风格下，`--breath` 控制原声里轻微送气的最大提升量（dB）。`--breath-reach` 取 0–1，控制声源在身前等近处方位时保留的增强比例，离头中心 20 cm 内权重最大，至 65 cm 淡出。设为 0 时只在贴耳时提升。低频的扑耳触感由 `--puff` 单独控制。参数标定和前后对照见[效果与依据](effects.md#ku100-实录风格---style-ku100)。

`--sharp-soften` 控制强辅音在 5 kHz 附近的最大衰减，范围为 0–8 dB，0 为关闭。较大的值适合个别字音偏尖的录音。需要整体控制宽带齿音时可另调 `--deess`。

`--static-ambience on|off` 控制录音里的持续底噪是否留在固定位置。选择 `--style ku100` 时自动开启；例如 `auto-asmr convert 输入.wav --style ku100 --static-ambience off -o 对照.wav` 可听关闭效果。实时模拟 `rt-sim` 使用同一选项。房间反射用 `--room` 调节，输入降噪用 `--denoise` 调节，合成雨声仍用 `--bg` 调节。

`--style` 有四个内置预设：`ear`（贴耳加强，默认）、`ku100`（近耳轻语）、`breath`（呼吸音）和 `earclean`（掏耳）。风格先生效，显式参数可覆盖预设。处理单声道呼吸录音、保留原来的吸气和呼气节奏：

```bash
auto-asmr convert 呼吸干声.wav --style breath -o 枕边呼吸.wav
```

`--breath-tone 0..1` 调整薄声补偿强度，低频偏弱时补气流，3 kHz 以上过亮时柔化。设为 0 可关闭。`--puff` 单独调气流触感。呼吸去闷 `--breath-deboom 0..12` 限制 180 Hz 附近的最大动态衰减，呼吸音预设取 6 dB。

呼吸音预设开启 `--breath-pause-switch on`，在穿梭模式下达到 `--hold-time` 后等待换气间隙再换边，`--move-time` 调移动时间。设为 `off` 恢复定时穿梭。文件目标响度默认 −26 LUFS，可以用 `--lufs` 覆盖。参考片段和频带测量见[呼吸音预设](effects.md#呼吸音预设)。

掏耳的目标响度默认是 −20 LUFS，入耳的几项可以单独调（`--inear` 入耳深度、`--canal` 耳道共鸣、`--rumble` 耳内低频、`--muffle` 堵耳闷感，见 `--help`）：

```bash
auto-asmr convert 棉签.wav -o 掏耳.wav --style earclean --hold-time 60 --rumble 0.8
```

批量：给多个文件或文件夹，`--out-dir` 指定输出文件夹（不写就放在源文件旁边），`-j` 指定并行数（默认 CPU 逻辑核数）：

```bash
auto-asmr convert a.m4a b.mp3 某文件夹/ --out-dir 输出/ -j 8
```

多音频合成：2–4 个输入加 `--mix`。`--pos` 按输入顺序给位置（`left` / `right` / `front` / `back` / `front-left` …，或"方位角[:到头中心的距离 米]"，如 `45:0.3`），不写就是一左一右；`--mix-gain` 是各自的音量增减：

```bash
auto-asmr convert a.m4a b.m4a --mix -o 合成.wav --pos left,right --mix-gain 0,-3
```

一起移动加 `--mix-follow`，走位方式用 `--mode`（默认穿梭）：

```bash
auto-asmr convert a.m4a b.m4a --mix --mix-follow --mode orbit -o 合成_环绕.wav
```

同一首歌的几个版本（前奏长短不同、编曲不同）加 `--mix-align` 自动对齐：从一起开始唱的地方叠加，对不上的开头和结尾裁掉，`--start` / `--duration` 按对齐之后的时间算；完成后打印每段从哪里开始用、裁掉多少（做法见[界面使用说明 · 自动对齐](app-usage.md#自动对齐同一首歌的几个版本)）：

```bash
auto-asmr convert 摇滚版.m4a DJ版.m4a --mix --mix-align --duration 60 -o 合成.wav
```

自动平衡（让几个声音每一段时间都差不多一样响，比如一左一右时左右差不多大）加 `--mix-balance`，后面的数值是强度 %（不写就是 100）；完成后打印平衡前后的响度差（做法见[界面使用说明 · 自动平衡](app-usage.md#自动平衡让左右两边差不多大)）：

```bash
auto-asmr convert a.m4a b.m4a --mix --mix-balance 80 -o 合成.wav
```

一个音频当成左右两个人（同一个声音一个在左耳、一个在右耳，第 2 个晚一点）加 `--duo`，后面是第 2 个声音晚多少毫秒（1–300，不写就是 80）。`--pos`（两个位置）、`--mix-gain`、`--mix-follow`、`--mix-balance` 也能一起用；批量时每个文件都这样做；输出比原音频长出这段延迟（做法见[界面使用说明 · 左右双人](app-usage.md#左右双人一个音频当成两个人)）：

```bash
auto-asmr convert 人声.m4a --duo 80 --duration 60
auto-asmr convert 人声.m4a --duo 100 --pos left,front-right --mix-balance 50 -o 双人.wav
```

背景音（实时生成的雨声，见[界面使用说明 · 背景音](app-usage.md#背景音雨声)）：`--bg rain-night` 打开雨夜预设，后面再写 `--bg-level`（音量）、`--rain`（雨量）、`--bg-pan`（左右）、`--bg-width`（包围感）、`--bg-flow`（流动）、`--bg-indoor`（隔窗）、`--rain-land`（落地声；旧名 `--rain-near` 也能用）、`--rain-drip`（屋檐滴水）、`--thunder`（雷声，可选，默认 0 = 没有）覆盖它，都是 0–100（`--bg-pan` 是 −100–100）。单个文件、批量、多音频合成、左右双人都能用：

```bash
auto-asmr convert 人声.m4a --bg rain-night --duration 60
auto-asmr convert 人声.m4a --bg rain-night --rain 70 --rain-land 15 --bg-level 45 -o 雨中.wav
auto-asmr convert 人声.m4a --bg rain-night --bg-indoor 90 --bg-flow 0 -o 隔窗听雨.wav
auto-asmr convert 人声.m4a --bg rain-night --thunder 50 -o 雷雨.wav
```

只生成雨声本身（不要人声），响度按 `--lufs`（默认 −20 LUFS），默认雨夜，背景音选项一样能用：

```bash
auto-asmr bg -o 雨夜.wav --duration 60
auto-asmr bg -o 大雨.wav --duration 60 --rain 85
auto-asmr bg -o 屋檐.wav --duration 60 --rain-land 40 --rain-drip 35
auto-asmr bg -o 雷雨.wav --duration 60 --rain 60 --thunder 70
```

只想先看怎么对齐、不输出音频：

```bash
auto-asmr align 摇滚版.m4a DJ版.m4a
```

它还会打印偏移、是否逐样本、得分与次好得分之比、重叠部分的相关和几处偏移相差多少，用来判断结果可不可靠。

```bash
auto-asmr --help
```

命令行里降噪是 `--denoise 12`（底噪最多压低 12 dB，范围 0–100）；只写 `--denoise` 是降到最干净。想单独听降噪的效果（不做空间化）：`auto-asmr decode 输入.m4a --denoise 12 -o 降噪后.wav`。

`--save-trajectory 轨迹.csv` 导出实际走位，列是：时间、方位角、距离、不含忽远忽近的距离、说话活动、仰角、入耳深度（米）。

其他命令：`auto-asmr devices` 列出音频设备，`auto-asmr bench` 测处理速度，`auto-asmr rt-sim 音频文件` 不经过声卡、按实时模式把一个文件跑一遍并打印走位（可加 `--style ku100` / `--style earclean`、`--gain -30` 模拟小音量麦克风、`--noise -50` 加底噪、`--sparse 1.5/4` 模拟说 1.5 秒停 4 秒、`--manual "5:90,0.105,0;10:auto"` 模拟第 5 秒手动摆到右耳、第 10 秒切回自动；掏耳风格下还会打印伸进耳道的次数和在耳道里的时间占比）。
