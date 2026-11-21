#!/bin/sh
# 不经过 App，直接用 adb 以 shell 身份跑服务进程（和 Shizuku 的 adb 模式是同一个身份），调试用。
# 先装好 App（./gradlew installDebug），然后：
#   tools/server.sh --start            # 交互：从键盘输命令（stop / start / dry 1 / style 2 / exit …）
#   tools/server.sh --start --seconds 30   # 跑 30 秒自动退出
# 以 root 身份跑（模拟器 / 已 root 的手机）：AS_ROOT=1 tools/server.sh --start
set -e
PKG=io.github.nasuiyile.autoasmr.shizuku
APK=$(adb shell pm path "$PKG" | sed -n 's/^package://p' | tr -d '\r')
if [ -z "$APK" ]; then
    echo "没装 $PKG，先 ./gradlew installDebug" >&2
    exit 1
fi
LIB="$(dirname "$APK")/lib/arm64/libasmrshizuku.so"
CMD="CLASSPATH=$APK exec app_process /system/bin io.github.nasuiyile.autoasmr.shizuku.server.ServerMain --apk $APK --lib $LIB $*"
if [ -n "$AS_ROOT" ]; then
    exec adb shell "su 0 sh -c '$CMD'"
else
    exec adb shell "$CMD"
fi
