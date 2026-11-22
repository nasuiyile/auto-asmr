# auto-asmr-android（安卓版，开发中）

安卓版 auto-asmr，三个页面：网页模式是内嵌浏览器，把浏览器里正在发声的网页的声音实时做成 KU100 双耳 ASMR 效果再播放；有 Shizuku 或 root 时还可以用全局模式，处理任意 App 的声音；文件转换和电脑版的文件转换页一样，把音频文件转成双耳效果的 WAV（可以试听、批量、多音频合成、左右双人）。效果参数、风格预设和电脑版相同，配置文件和电脑版通用，不含麦克风实时（主播）模式。

目前在模拟器上能用（包括后台播放、通知栏控制、背景雨声），还没有在真机上测过。发布版 APK 随 GitHub Release 发布（`auto-asmr-android.apk`）。

## 构建

需要 JDK 17+、Android SDK / NDK、Rust 和它的两个安卓目标（`rustup target add aarch64-linux-android armv7-linux-androideabi`），环境搭法见 [`doc/android-dev.md`](../doc/android-dev.md)：

```bash
./gradlew assembleDebug
```

生成 `app/build/outputs/apk/debug/app-debug.apk`，用 `adb install -r` 装到手机或模拟器。

## 文档

- [安卓版](../doc/android-dev.md)：界面和操作、文件转换、后台播放、配置文件、环境、构建、发布和签名、调试、代码结构
- [全局模式](../doc/shizuku.md)：Shizuku / root 处理任意 App 的声音：原理、防止被杀后台、和网页模式的关系、实测结果
- [安卓版技术方案](../doc/android-plan.md)：方案对比、各步的验证结果、开发步骤
