#!/usr/bin/env python3
"""检查 auto-asmr 各版本的预设和参数是否对齐。

用法（在仓库根目录）：
    python3 .claude/skills/align-versions/scripts/check_alignment.py

以 Rust 共用库 `auto-asmr/src/dsp/params.rs` 为准（`Params` 的字段、`style_*` / `bg_*` 预设方法），逐个核对：
  桌面版界面（gui.rs）和命令行（cli.rs）、安卓版 Rust 层（settings.rs、lib.rs、ui_spec.json）、
  浏览器插件（wasm/src/lib.rs、schema.js、popup.html、options.html）、安卓版和 global-asmr 的 adb 风格列表、
  Python 版的 STYLES。
另外比对安卓参数表（ui_spec.json）和插件参数表（schema.js）的标签、滑条范围，以及标签在桌面版界面里能不能找到。

只读文件，不改任何东西。有没解释的差异时退出码为 1。
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[4]

# 有意的差异（改动时如果新增了，写进这里并说明原因，别让检查一直报）
KNOWN_LABEL_DIFF = {
    # 插件没有「试听」，标签里去掉了"实时 / 试听"
    "auto_level",
}
KNOWN_LABEL_NOT_IN_GUI = {
    # 桌面版的移动方式是一排单选按钮，前面没有「方式」两个字
    "mode",
}
# 只在界面上出现的换算项（安卓 settings.rs 的 VIRTUAL / 插件 schema.js 的 derived）各自覆盖哪些 Params 字段
VIRTUAL_COVERS = {
    "ear_cm": ["ear_dist"],
    "manual_cm": ["manual_dist"],
    "denoise_slider": ["denoise", "denoise_db"],  # 安卓
    "denoise": ["denoise", "denoise_db"],  # 插件（schema.js 里 key 叫 denoise，derived）
    "moves_per_min": ["hold_s", "move_s"],
}

problems: list[str] = []
notes: list[str] = []


def read(rel: str) -> str:
    p = ROOT / rel
    if not p.is_file():
        problems.append(f"找不到文件 {rel}（目录结构变了？更新本脚本）")
        return ""
    text = p.read_text(encoding="utf-8")
    # Rust 文件只看正式代码：测试模块（文件末尾的 #[cfg(test)]）里提到某个预设不算界面上有
    if rel.endswith(".rs") and "#[cfg(test)]" in text:
        text = text[: text.index("#[cfg(test)]")]
    return text


# ---------------------------------------------------------------- 共用库：真相来源
params_rs = read("auto-asmr/src/dsp/params.rs")
body = params_rs[params_rs.index("pub struct Params {"):]
body = body[: body.index("\n}\n")]
FIELDS = re.findall(r"^\s+pub (\w+):", body, re.M)
PRESETS = re.findall(r"pub fn ((?:style|bg)_\w+)\((?:mut )?self\)", params_rs)
STYLES = [p for p in PRESETS if p.startswith("style_")]

gui = read("auto-asmr/src/gui.rs")
cli = read("auto-asmr/src/cli.rs")
settings_rs = read("auto-asmr-android/rust/src/settings.rs")
android_lib = read("auto-asmr-android/rust/src/lib.rs")
spec_text = read("auto-asmr-android/rust/src/ui_spec.json")
wasm = read("auto-asmr-chromium/wasm/src/lib.rs")
schema_js = read("auto-asmr-chromium/extension/schema.js")
popup = read("auto-asmr-chromium/extension/popup.html")
options = read("auto-asmr-chromium/extension/options.html")
android_main = read("auto-asmr-android/app/src/main/java/io/github/nasuiyile/autoasmr/MainActivity.kt")
global_main = read("auto-asmr-shizuku/app/src/main/java/io/github/nasuiyile/autoasmr/shizuku/MainActivity.kt")
python = read("asmr_binaural.py")

spec = json.loads(spec_text) if spec_text else {}


def const_list(src: str, name: str) -> list[str]:
    m = re.search(rf"pub const {name}: \[&str; \d+\] = \[([^\]]*)\]", src)
    return re.findall(r'"(\w+)"', m.group(1)) if m else []


ANDROID_VIRTUAL = const_list(settings_rs, "VIRTUAL")
ANDROID_DOC = const_list(settings_rs, "DOC_KEYS")

# ---------------------------------------------------------------- 预设
print(f"共用库：{len(FIELDS)} 个参数，预设 {', '.join(PRESETS)}")

spec_actions = set(re.findall(r'"action":\s*"(\w+)"', spec_text))
style_buttons = {name: len(re.findall(r'data-style="\d+"', html)) for name, html in (("popup.html", popup), ("options.html", options))}
py_styles = set(re.findall(r'^\s+"(\w+)": dict\(', python[python.find("STYLES = {"):python.find("}", python.find("STYLES = {"))], re.M))

for p in PRESETS:
    call = f".{p}()"
    where = {
        "桌面版界面 gui.rs": call in gui,
        "命令行 cli.rs": call in cli,
        "安卓 settings.rs（按钮）": f'"{p}"' in settings_rs,
        "安卓 ui_spec.json（按钮）": p in spec_actions,
        "插件 wasm/src/lib.rs": call in wasm,
    }
    if p in STYLES:
        where["安卓 lib.rs（asmr_set_style 编号）"] = f'"{p}"' in android_lib
        where["安卓 MainActivity（adb 风格列表）"] = f'"{p}"' in android_main
        where["global-asmr MainActivity（adb 风格列表）"] = f'"{p}"' in global_main
    for k, ok in where.items():
        if not ok:
            problems.append(f"预设 {p}：{k} 里没有")
    if p in STYLES:
        short = p.removeprefix("style_")
        if short not in py_styles:
            notes.append(f"预设 {p}：Python 版的 STYLES 里没有（确认 doc/python-cli.md、doc/python-vs-rust.md 写了它是 Rust 版独有）")

for name, n in style_buttons.items():
    if n != len(STYLES):
        problems.append(f"插件 {name} 有 {n} 个风格按钮，共用库有 {len(STYLES)} 个风格（data-style 编号要和 wasm/src/lib.rs、安卓 asmr_set_style 一致）")

# ---------------------------------------------------------------- 参数：每个字段在各版本界面上有没有
spec_items: dict[str, dict] = {}


def walk(o):
    if isinstance(o, dict):
        if "key" in o and "label" in o:
            spec_items[o["key"]] = o
        for v in o.values():
            walk(v)
    elif isinstance(o, list):
        for v in o:
            walk(v)


walk(spec)


def js_object_at(pos: int) -> str:
    i = schema_js.rfind("{", 0, pos)
    depth = 0
    for j in range(i, len(schema_js)):
        if schema_js[j] == "{":
            depth += 1
        elif schema_js[j] == "}":
            depth -= 1
            if depth == 0:
                return schema_js[i : j + 1]
    return schema_js[i:]


schema_rows: dict[str, dict] = {}
for m in re.finditer(r"key: '(\w+)'", schema_js):
    o = js_object_at(m.start())

    def g(pat):
        x = re.search(pat, o)
        return x.group(1) if x else None

    schema_rows[m.group(1)] = {
        "label": g(r"label: '([^']*)'"),
        "min": float(g(r"\bmin: (-?[\d.]+)")) if g(r"\bmin: (-?[\d.]+)") else None,
        "max": float(g(r"\bmax: (-?[\d.]+)")) if g(r"\bmax: (-?[\d.]+)") else None,
        "derived": "derived: true" in o,
        "doc": "doc: true" in o,
    }


def covered(field: str, keys) -> bool:
    return field in keys or any(field in VIRTUAL_COVERS.get(k, []) for k in keys)


for f in FIELDS:
    if not re.search(rf"\.{f}\b", gui):
        problems.append(f"参数 {f}：桌面版界面 gui.rs 里没用到（没有控件？）")
    if not covered(f, spec_items):
        problems.append(f"参数 {f}：安卓参数表 ui_spec.json 里没有（global-asmr 用同一份，也跟着缺）")
    if not covered(f, schema_rows):
        problems.append(f"参数 {f}：插件参数表 schema.js 里没有")
    if not re.search(rf"\.{f}\b", cli):
        notes.append(f"参数 {f}：命令行没有对应的选项（不是每个参数都要有，确认是有意的）")

for k in spec_items:
    if k not in FIELDS and k not in ANDROID_VIRTUAL and k not in ANDROID_DOC:
        problems.append(f"ui_spec.json 的 {k} 既不是 Params 字段，也不在 settings.rs 的 VIRTUAL / DOC_KEYS 里")
for k, r in schema_rows.items():
    if k not in FIELDS and not (r["derived"] or r["doc"]):
        problems.append(f"schema.js 的 {k} 不是 Params 字段，又没标 derived / doc")
    if r["derived"] and f'"{k}"' not in wasm:
        problems.append(f"schema.js 的换算项 {k} 在 wasm/src/lib.rs 里没有读写")
for k in ANDROID_VIRTUAL:
    if k not in VIRTUAL_COVERS:
        problems.append(f"settings.rs 新增了换算项 {k}：在本脚本的 VIRTUAL_COVERS 里写上它覆盖哪些字段")

# ---------------------------------------------------------------- 标签、范围
for k, it in spec_items.items():
    r = schema_rows.get(k)
    if r is not None:
        if r["label"] != it.get("label") and k not in KNOWN_LABEL_DIFF:
            problems.append(f"{k} 的标签不一样：安卓「{it.get('label')}」/ 插件「{r['label']}」")
        if it.get("type") == "slider" and (r["min"], r["max"]) != (it.get("min"), it.get("max")):
            problems.append(f"{k} 的滑条范围不一样：安卓 {it.get('min')}–{it.get('max')} / 插件 {r['min']}–{r['max']}")
    if f'"{it["label"]}' not in gui and k not in KNOWN_LABEL_NOT_IN_GUI:
        problems.append(f"{k} 的标签「{it['label']}」在桌面版界面 gui.rs 里找不到（参数表是从桌面版抄的，改了一边要改另一边）")

# ---------------------------------------------------------------- 结果
print(f"安卓参数表 {len(spec_items)} 项，插件参数表 {len(schema_rows)} 项；风格按钮：{style_buttons}")
if notes:
    print("\n提示（多半是有意的，确认一下）：")
    for n in notes:
        print("  - " + n)
if problems:
    print(f"\n没对齐的地方（{len(problems)} 处）：")
    for p in problems:
        print("  - " + p)
    sys.exit(1)
print("\n预设和参数在各版本间对齐。")
