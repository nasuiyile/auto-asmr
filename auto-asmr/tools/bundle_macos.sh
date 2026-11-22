#!/bin/bash
# 把程序打包成 macOS 的 auto-asmr.app：可以双击启动，并带上麦克风权限说明
# （没有这条说明，macOS 不会弹出麦克风授权，实时模式就拿不到声音）。
# 用法：tools/bundle_macos.sh    生成 target/release/auto-asmr.app
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release
APP=target/release/auto-asmr.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/auto-asmr "$APP/Contents/MacOS/auto-asmr"
# 应用图标：戴耳机的白色小猫（icon/make_icons.py 生成）
cp assets/icon.icns "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>auto-asmr</string>
    <key>CFBundleDisplayName</key><string>auto-asmr</string>
    <key>CFBundleIdentifier</key><string>local.auto-asmr</string>
    <key>CFBundleExecutable</key><string>auto-asmr</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSMicrophoneUsageDescription</key><string>实时模式需要使用麦克风，把你的声音变成耳边说话的效果。</string>
</dict>
</plist>
EOF
echo "已生成 $APP"
