# Python 命令行版（asmr_binaural.py）

把任意人声处理成"有人在你耳边 / 身边说话"的效果，用耳机听。立体声输入只取一个声道（默认取更响的那个）再做空间化。

```bash
uv run asmr_binaural.py 输入.wav                          # 默认：贴耳加强，左右耳慢慢穿梭，忽远忽近，贴耳气声 → 输入_asmr.wav
uv run asmr_binaural.py 输入.wav --style ku100            # 像真实 KU100 实录：远耳听得到，离耳约 6 cm，有房间
uv run asmr_binaural.py 输入.m4a -o 输出.wav --mode wander  # 在你身边走动着说话
```

第一次运行时，uv 会安装 numpy / scipy / h5py；如果 `hrtf/` 里没有数据，会从 Zenodo 下载约 16 MB 的 HRIR，然后生成预处理缓存（约半分钟）。之后处理 1 分钟音频大约 2 秒，62 分钟音频约 2.5 分钟。

各项效果的做法和依据见[效果与依据](effects.md)。Python 版的 `--style` 只有 `ear` 和 `ku100`：掏耳、呼吸音预设只有 Rust 版有，Rust 版的 `ku100` 后来也重新标定过，与这里的不再相同（见[两版的关系](python-vs-rust.md)）。

## 运动模式 `--mode`

| 模式 | 效果 | 相关参数 |
|---|---|---|
| `sweep`（默认） | 贴左耳停一会儿 → 经脸前慢慢移到右耳 → 停 → 移回，持续不断（按说话时间走，停顿里不动）；偶尔先退远再凑近 | `--move-time`（2.5 s）`--hold-time`（2.5 s）`--ear-angle`（100°）`--approach`（0.25）`--via front/back/alternate` `--over-top`（从头顶绕的概率，0）`--pass-dist`（0.2 m）`--dist` |
| `phrase` | 只在说话停顿处换位置（移动藏在停顿里，听起来像跳过去）：左耳 ↔ 右耳为主，偶尔到侧前方、侧后方、正前方 | `--min-gap` `--min-hold` `--max-hold` `--seed` |
| `pingpong` | 参考视频的节奏：3 秒一个来回，每只耳朵停约 1 秒，约 0.5 秒穿过中间 | `--period`（默认 3）`--via` `--dist` |
| `orbit` | 绕头转圈 | `--radius`（默认 0.35 m）`--period`（默认 12 s）`--ccw` `--az`（起点） |
| `wander` | 在你身边走动，0.25–1.2 m | `--seed` |
| `static` | 固定位置 | `--az` `--dist`（默认左耳 270°、0.15 m） |
| `path` | 按关键帧走 | `--path 轨迹.json` |

方位角 `az`：0 = 正前，90 = 右，180 = 正后，270 = 左（顺时针，单位为度）。距离 `dist`：到头中心的米数，范围 0.095–3。耳朵本身离头中心约 0.09 m，所以 0.105 m 就是嘴贴着耳朵（默认的贴耳距离）。

`path` 模式的 JSON 示例：相邻两个关键帧之间做平滑移动；如果想停住，就连写两个相同位置。

```json
[
  {"t": 0,  "az": 270, "dist": 0.14},
  {"t": 3,  "az": 270, "dist": 0.14},
  {"t": 5,  "az": 90,  "dist": 0.14, "via": "front", "pass_dist": 0.4},
  {"t": 9,  "az": 180, "dist": 0.6},
  {"t": 14, "az": 0,   "dist": 1.2}
]
```

`via` 可以是 `short` / `front` / `back`，决定从前面还是后面绕过去；`pass_dist` 是绕行途中离头的最小距离。

## 其他常用参数

- `--channel auto|left|right|mix`：立体声取哪个声道。如果输入本身已经做过左右声像（比如原视频），只取一个声道会丢掉另一侧的内容，程序会提示你改用 `mix`。
- `--room none|real|treated|bedroom|living`，`--wet ±dB`：房间声学。默认风格为 `none`，`--style ku100` 为 `real`。想要空间感可以用 `real` 或 `bedroom`。
- `--warmth 6`：凑近时两耳共同的低频温暖感（dB），设 0 关闭。
- `--micro 1`：自然晃动幅度，设 0 关闭。
- `--level-follow`：响度随位置变化的程度。1 = 物理真实（嘴贴耳朵时比在脸前响约 17 dB，移动起来像"这边消失、那边冒出来"），0 = 处处一样响。默认 sweep 用 0.2，pingpong 用 0.1，orbit 用 0.3，phrase 用 0.5，其他模式用 1。
- `--lufs -16`，`--peak -1`，`--max-gr 8`：响度目标、真峰值上限、限制器最多压多少 dB。
- `--start` / `--duration`：只处理一段，方便试听。
- `--save-trajectory 轨迹.csv`：导出实际走位，格式为 时间, 方位角, 距离, 仰角。

## 建议

- 输入越干净越好。程序不会添加底噪，但输入自带的底噪会跟着人声一起移动，可以先用 DeepFilterNet 降噪（`~/ai/qwenasr/DeepFilterNet3/deep-filter`）。
- 必须用耳机听。外放听不出效果。
- HRTF 不是按个人测量的，少数人可能出现前后混淆；运动和房间声能明显减轻这个问题。
- 上下方向比左右和远近弱，因人而异；不想要可以用 `--updown 0` 关掉。近场实测数据只有水平面，带高度的位置在 0.5 m 以内是模型外推（见[效果与依据 · 上下](effects.md#上下默认开启自动上下)）。
