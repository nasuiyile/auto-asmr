# 数据与许可

## 代码

本仓库的代码（桌面版、安卓版、浏览器插件、Python 版）采用 GNU GPL 3.0 或更新版本（GPL-3.0-or-later），全文见仓库根目录的 [`LICENSE`](../LICENSE)。修改后再发布（包括打包进别的程序发布）时，整个衍生作品也要按 GPL 开源。下面的数据不受 GPL 约束，各自按 CC BY 4.0 署名即可。

依赖库的许可：rustfft、realfft、serde、ron、cpal、eframe、rfd、png、hound 为 MIT / Apache-2.0，nnnoiseless 为 BSD-3-Clause，symphonia 为 MPL-2.0，都可以和 GPL-3.0 一起发布。

## Python 版

`hrtf/` 中的数据来自 TH Köln 的 Spherical Near-Field HRIR Compilation of the Neumann KU100，作者 J. M. Arend、A. Neidhardt、C. Pörschmann，[Zenodo 4297951](https://zenodo.org/records/4297951)，CC BY 4.0。程序第一次运行时会自动下载这些数据。`ku100_tables_v4.npz` 是预处理缓存，删掉后会自动重建。

`data/pinna_hutubs.npz` 和 `auto-asmr/assets/pinna.bin`（耳廓选择的滤波表）由 HUTUBS 真人 HRTF 库生成：F. Brinkmann, M. Dinakaran, R. Pelzer, P. Grosche, D. Voss, S. Weinzierl, "A cross-validated database of measured and simulated HRTFs including 3D head meshes and anthropometric features", J. Audio Eng. Soc., 2019（按数据集说明文档给出的引用方式）；数据集本身："The HUTUBS HRTF database"，DOI 10.14279/depositonce-8487，[depositonce.tu-berlin.de/handle/11303/9429](https://depositonce.tu-berlin.de/handle/11303/9429)，CC BY 4.0。原始数据集（1.36 GB）不在仓库里，只有重新生成表时才需要。

## Rust 版（`auto-asmr/assets/`）

`assets/ku100.bin` 里的数据来自 TH Köln，均为 CC BY 4.0：

- 近场 HRIR 和耳机补偿滤波器：J. M. Arend、A. Neidhardt、C. Pörschmann，*Spherical Near-Field HRIR Compilation of the Neumann KU100*，[Zenodo 4297951](https://zenodo.org/records/4297951)。0.25 m 以内用刚性球头模型外推。高度修正取自同一数据集 1.5 m 的全球面测量（2702 个方向），做法和检验见[效果与依据 · 上下](effects.md#上下默认开启自动上下)。
- 真实房间：T. Lübeck、J. M. Arend、C. Pörschmann (2021)，*A High-Resolution Spatial Room Impulse Response Database*，[Zenodo 5031335](https://zenodo.org/records/5031335)，去掉了直达声并缩短了混响时间。

输入降噪使用 [nnnoiseless](https://github.com/jneem/nnnoiseless)（RNNoise 的 Rust 移植）。
