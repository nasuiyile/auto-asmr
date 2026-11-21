plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "io.github.nasuiyile.autoasmr.shizuku"
    // 新版 Compose 要求至少用 37.1 编译（同安卓版，不影响 minSdk）
    compileSdk {
        version = release(37) { minorApiLevel = 2 }
    }
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "io.github.nasuiyile.autoasmr.shizuku"
        // 动态音频策略按 UID 排除播放端从 Android 10 开始支持；目前只在 Android 16 模拟器上验证过
        minSdk = 29
        targetSdk = 36
        // 每次发给别人测都要加一：服务进程的 hello 行和界面上会显示 versionName，用来确认跑的是哪一版
        versionCode = 3
        versionName = "0.2.0-prototype"
        ndk {
            // 原型只编 64 位
            abiFilters += listOf("arm64-v8a")
        }
    }

    // KU100 / 耳廓数据：服务进程直接从 APK 里读 assets/ku100.bin、assets/pinna.bin
    sourceSets {
        getByName("main") {
            assets.srcDir("../../auto-asmr/assets")
        }
    }

    buildTypes {
        getByName("release") {
            // R8 精简（Compose 的代码大部分用不到，同安卓版）；服务进程按名字加载的类和 JNI 见 proguard-rules.pro。
            // 原型阶段用调试密钥签名，和 debug 包可以互相覆盖安装
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.31.6"
        }
    }

    packaging {
        jniLibs {
            // 把 .so 解压到 /data/app/…/lib/arm64/，服务进程（shell / root 身份）按路径 System.load
            useLegacyPackaging = true
        }
    }

    buildFeatures {
        buildConfig = true
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation("dev.rikka.shizuku:api:13.1.5")
    implementation("dev.rikka.shizuku:provider:13.1.5")
    // 界面和安卓版（auto-asmr-android）同一套
    implementation(platform("androidx.compose:compose-bom:2026.11.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.13.0")
}
