# global-asmr（全局拦截版，原型）

安卓 App（目录 `auto-asmr-shizuku/`）：借 Shizuku 或 root，把任意 App 播放的声音截下来（原声不再进耳机），用和安卓版同一份 Rust 引擎做成 KU100 双耳 ASMR 效果，再重新输出。界面和参数与安卓版一致；用 Shizuku 时处理声音的进程常驻，App 被杀后声音不断。

原型阶段：在 Android 16 模拟器上用 Shizuku 跑通了。真机上第一次试（荣耀，Android 17）开始拦截时出错，已经修改，待复测；root（su）启动还没测。

## 构建

环境和安卓版相同，见 [`doc/android-dev.md`](../doc/android-dev.md#环境)：

```bash
./gradlew assembleRelease
```

生成 `app/build/outputs/apk/release/app-release.apk`。不用 App、直接用 adb 跑服务进程：`tools/server.sh --start`。

## 文档

- [全局拦截版](../doc/shizuku.md)：原理、启动方式、防止被杀后台、界面、控制协议、实测结果、还没验证的、代码结构
