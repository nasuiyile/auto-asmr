package io.github.nasuiyile.autoasmr

import android.app.Application
import android.util.Log
import com.bytedance.android.bytehook.ByteHook
import io.github.nasuiyile.autoasmr.global.Controller

class AsmrApp : Application() {
    override fun onCreate() {
        super.onCreate()
        // hook 必须在 WebView 加载它的 so 之前登记好；之后加载的 so 由 ByteHook 自动补上。
        val init = ByteHook.init()
        System.loadLibrary("asmrhook")
        // 调试：files/hook_mode 写 inline / both 时换成备用的 inline hook，写 none 时一个都不装（测自检和备用路径用，
        // 见 doc/android-dev.md）
        val how = when (runCatching { java.io.File(filesDir, "hook_mode").readText().trim() }.getOrNull()) {
            "inline" -> 1
            "both" -> 2
            "none" -> 3
            else -> 0
        }
        val fails = NativeHook.install(how)
        Log.i(TAG, "ByteHook.init=$init，hook 方式 $how，失败 $fails 个")
        Playback.init(this)
        // 数据文件从 APK 资源读进来交给引擎，再读上次保存的配置（和电脑版同一格式），都在后台线程。
        // 加载完之前打开的流不处理（直通）
        Thread({ Settings.load(this) }, "asmr-init").start()
        // 全局模式：收到 Shizuku 的 binder 后，连回还在跑的服务进程（App 被杀后它继续处理声音）
        Controller.init(this)
    }

    companion object {
        const val TAG = "AsmrHook"
    }
}
