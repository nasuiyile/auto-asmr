#!/usr/bin/env python3
"""从 icon/auto-asmr-icon.svg 生成各端的图标（图标只改 SVG，再跑一遍这个脚本）。

用法：python3 icon/make_icons.py

需要：resvg 命令行（`cargo install resvg`）把 SVG 渲染成 PNG；macOS 的 iconutil 生成 .icns（不在 macOS 上就跳过 .icns）。

生成：
- 浏览器插件：auto-asmr-chromium/extension/icons/icon{16,32,48,128}.png
- 安卓：res/drawable/ic_launcher_foreground.xml（SVG 里除底色以外的部分转成矢量图）、
  res/values/ic_launcher_background.xml 的底色（取 SVG 里 id="bg" 的颜色）
- 桌面版：auto-asmr/assets/icon.png（256，窗口图标）、icon.ico（Windows exe）、icon.icns（macOS .app）
"""

import os
import shutil
import struct
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SVG = os.path.join(ROOT, "icon", "auto-asmr-icon.svg")
EXT_ICONS = os.path.join(ROOT, "auto-asmr-chromium", "extension", "icons")
ANDROID_RES = os.path.join(ROOT, "auto-asmr-android", "app", "src", "main", "res")
DESKTOP = os.path.join(ROOT, "auto-asmr", "assets")
NS = "{http://www.w3.org/2000/svg}"


def render(size, out):
    subprocess.run(["resvg", "-w", str(size), "-h", str(size), SVG, out], check=True)


def write_ico(pngs, out):
    """ICO 里直接放 PNG（Windows Vista 起都支持）。"""
    head = struct.pack("<HHH", 0, 1, len(pngs))
    entries, blobs = b"", b""
    offset = 6 + 16 * len(pngs)
    for size, data in pngs:
        s = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", s, s, 0, 0, 1, 32, len(data), offset + len(blobs))
        blobs += data
    with open(out, "wb") as f:
        f.write(head + entries + blobs)


# ---------- SVG → 安卓矢量图（只认这个图标用到的元素和属性） ----------

STYLE_KEYS = ("fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin", "stroke-opacity", "fill-opacity", "opacity")


def ellipse_path(cx, cy, rx, ry):
    return f"M{cx - rx:g},{cy:g} A{rx:g},{ry:g} 0 1 0 {cx + rx:g},{cy:g} A{rx:g},{ry:g} 0 1 0 {cx - rx:g},{cy:g} Z"


def rect_path(x, y, w, h, r):
    r = min(r, w / 2, h / 2)
    if r <= 0:
        return f"M{x:g},{y:g} H{x + w:g} V{y + h:g} H{x:g} Z"
    return (
        f"M{x + r:g},{y:g} H{x + w - r:g} A{r:g},{r:g} 0 0 1 {x + w:g},{y + r:g} V{y + h - r:g} "
        f"A{r:g},{r:g} 0 0 1 {x + w - r:g},{y + h:g} H{x + r:g} A{r:g},{r:g} 0 0 1 {x:g},{y + h - r:g} "
        f"V{y + r:g} A{r:g},{r:g} 0 0 1 {x + r:g},{y:g} Z"
    )


def color(c):
    c = c.lstrip("#")
    return "#FF" + c.upper() if len(c) == 6 else "#" + c.upper()


def to_android(el, style, out, indent="        "):
    tag = el.tag.replace(NS, "")
    if tag in ("defs", "clipPath") or el.get("id") == "bg":
        return
    st = dict(style)
    st.update({k: el.get(k) for k in STYLE_KEYS if el.get(k) is not None})
    if tag in ("g", "svg"):
        for c in el:
            to_android(c, st, out, indent)
        return
    f = lambda k, d=0.0: float(el.get(k, d))
    if tag == "path":
        d = el.get("d")
    elif tag == "ellipse":
        d = ellipse_path(f("cx"), f("cy"), f("rx"), f("ry"))
    elif tag == "circle":
        d = ellipse_path(f("cx"), f("cy"), f("r"), f("r"))
    elif tag == "rect":
        d = rect_path(f("x"), f("y"), f("width"), f("height"), f("rx"))
    else:
        raise SystemExit(f"不认识的元素：{tag}")
    attrs = [f'android:pathData="{d}"']
    op = float(st.get("opacity", 1))
    if st.get("fill", "#000000") != "none":
        attrs.append(f'android:fillColor="{color(st.get("fill", "#000000"))}"')
        a = op * float(st.get("fill-opacity", 1))
        if a < 1:
            attrs.append(f'android:fillAlpha="{a:g}"')
    if st.get("stroke", "none") != "none":
        attrs.append(f'android:strokeColor="{color(st["stroke"])}"')
        attrs.append(f'android:strokeWidth="{float(st.get("stroke-width", 1)):g}"')
        if "stroke-linecap" in st:
            attrs.append(f'android:strokeLineCap="{st["stroke-linecap"]}"')
        if "stroke-linejoin" in st:
            attrs.append(f'android:strokeLineJoin="{st["stroke-linejoin"]}"')
        a = op * float(st.get("stroke-opacity", 1))
        if a < 1:
            attrs.append(f'android:strokeAlpha="{a:g}"')
    out.append(indent + "<path\n" + "\n".join(indent + "    " + a for a in attrs) + " />")


def android(root):
    bg = next(e for e in root.iter() if e.get("id") == "bg").get("fill")
    paths = []
    to_android(root, {}, paths)
    # 108dp 的画布里，中间 72dp 是遮罩后看得见的部分：SVG 里直径 480 的圆 → 72dp，所以 1 个 SVG 单位 = 0.15dp，
    # 画布是 720 个单位，SVG 的 512×512 放在正中间（各边让出 104）。
    xml = (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<!-- 启动图标前景：戴耳机的白色小猫。由 icon/make_icons.py 从 icon/auto-asmr-icon.svg 生成，不要手改 -->\n"
        '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
        '    android:width="108dp"\n    android:height="108dp"\n'
        '    android:viewportWidth="720"\n    android:viewportHeight="720">\n'
        '    <group\n        android:translateX="104"\n        android:translateY="104">\n'
        + "\n".join(paths)
        + "\n    </group>\n</vector>\n"
    )
    with open(os.path.join(ANDROID_RES, "drawable", "ic_launcher_foreground.xml"), "w") as fh:
        fh.write(xml)
    with open(os.path.join(ANDROID_RES, "values", "ic_launcher_background.xml"), "w") as fh:
        fh.write(
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<!-- 启动图标底色：由 icon/make_icons.py 从 icon/auto-asmr-icon.svg 生成，不要手改 -->\n"
            f'<resources>\n    <color name="ic_launcher_background">{color(bg)}</color>\n</resources>\n'
        )


def main():
    if not shutil.which("resvg"):
        sys.exit("找不到 resvg：先 cargo install resvg")
    os.makedirs(EXT_ICONS, exist_ok=True)
    os.makedirs(DESKTOP, exist_ok=True)
    for s in (16, 32, 48, 128):
        render(s, os.path.join(EXT_ICONS, f"icon{s}.png"))
    render(256, os.path.join(DESKTOP, "icon.png"))
    with tempfile.TemporaryDirectory() as tmp:
        pngs = []
        for s in (16, 24, 32, 48, 64, 128, 256):
            p = os.path.join(tmp, f"{s}.png")
            render(s, p)
            with open(p, "rb") as fh:
                pngs.append((s, fh.read()))
        write_ico(pngs, os.path.join(DESKTOP, "icon.ico"))
        if shutil.which("iconutil"):
            iconset = os.path.join(tmp, "icon.iconset")
            os.makedirs(iconset)
            for s in (16, 32, 128, 256, 512):
                render(s, os.path.join(iconset, f"icon_{s}x{s}.png"))
                render(s * 2, os.path.join(iconset, f"icon_{s}x{s}@2x.png"))
            subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(DESKTOP, "icon.icns")], check=True)
        else:
            print("没有 iconutil（不是 macOS），跳过 icon.icns")
    android(ET.parse(SVG).getroot())
    print("已生成：插件 icons/、安卓启动图标、桌面版 assets/icon.{png,ico,icns}")


if __name__ == "__main__":
    main()
