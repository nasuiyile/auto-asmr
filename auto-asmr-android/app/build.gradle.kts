plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "io.github.nasuiyile.autoasmr"
    // ByteHook 1.1.2 要求至少用 37、新版 Compose 要求至少用 37.1 编译（不影响 minSdk）
    compileSdk {
        version = release(37) { minorApiLevel = 2 }
    }
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "io.github.nasuiyile.autoasmr"
        // Chromium 只在 API 29 以上用 AAudio
        minSdk = 29
        targetSdk = 36
        // 发布时由 GitHub Actions 传进来（tag 名、运行序号）
        versionCode = System.getenv("ASMR_VERSION_CODE")?.toIntOrNull() ?: 1
        versionName = System.getenv("ASMR_VERSION_NAME") ?: "0.1.0-dev"
        ndk {
            // 64 位手机 + 少数 32 位低端机；Apple Silicon 上的模拟器是 arm64
            abiFilters += listOf("arm64-v8a", "armeabi-v7a")
        }
    }

    // KU100 / 耳廓数据（auto-asmr/assets/*.bin）作为 APK 资源，各 CPU 架构共用一份，启动时读进引擎
    sourceSets {
        getByName("main") {
            assets.srcDir("../../auto-asmr/assets")
        }
    }

    // 发布签名：环境变量里有密钥库时用它（GitHub Actions 从 secrets 传进来）；没有时用调试密钥（只能自己装着试）
    val keystore = System.getenv("ASMR_KEYSTORE")?.takeIf { file(it).isFile }
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("ASMR_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ASMR_KEY_ALIAS")
                keyPassword = System.getenv("ASMR_KEY_PASSWORD")
            }
        }
    }
    buildTypes {
        getByName("release") {
            // R8 精简（Compose 的代码大部分用不到）；JNI 的类名、方法名要保留，见 proguard-rules.pro
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
        }
    }

    buildFeatures {
        prefab = true
        compose = true
        // 全局模式的服务进程在 hello 里报版本号
        buildConfig = true
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.31.6"
        }
    }

    packaging {
        jniLibs {
            pickFirsts += "**/libbytehook.so"
            // 把 .so 解压到 /data/app/…/lib/<架构>/：全局模式的服务进程（shell / root 身份）按路径 System.load libasmrglobal.so
            useLegacyPackaging = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation("com.bytedance:bytehook:1.1.2")
    implementation("androidx.webkit:webkit:1.14.0")
    implementation(platform("androidx.compose:compose-bom:2026.11.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.13.0")
    // 全局模式：Shizuku 的 API 和把 binder 交给本 App 的 provider
    implementation("dev.rikka.shizuku:api:13.1.5")
    implementation("dev.rikka.shizuku:provider:13.1.5")
}
