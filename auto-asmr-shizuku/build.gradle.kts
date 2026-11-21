plugins {
    // AGP 9 自带 Kotlin 编译，不用再加 kotlin-android 插件
    id("com.android.application") version "9.5.1" apply false
    // Compose 编译插件（和安卓版同一版本）；它同时把 Kotlin 升到 2.4（AGP 9 自带的是 2.2，读不了新版 Compose 库）
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.21" apply false
}
