package io.github.nasuiyile.autoasmr.global.server

import android.content.Context
import android.os.Binder
import android.os.Handler
import android.os.Looper
import android.os.Parcel
import kotlin.system.exitProcess

/**
 * Shizuku 的 UserService：Shizuku 用 app_process 起一个进程（身份和 Shizuku 相同：adb 模式是 shell，root 模式是 root），
 * 用本 App 的 APK 建这个类，把它（一个 Binder）交给 App。App 用 daemon 模式绑定（见 Controller），
 * **App 进程被杀后这个进程继续跑**，声音照常处理；App 再打开时连回同一个进程。
 *
 * 不用 AIDL，只有一个调用：[TX_CALL]，参数一个字符串（命令），回复一个字符串（见 [EngineCore]）。
 */
class EngineService(context: Context) : Binder() {
    companion object {
        const val DESCRIPTOR = "io.github.nasuiyile.autoasmr.global.IEngine"
        const val TX_CALL = FIRST_CALL_TRANSACTION
        /** Shizuku 注销 UserService（unbindUserService 带 remove = true）时发的调用号（AIDL 里的 16777114 + 1）。 */
        const val TX_DESTROY = 16777115
    }

    init {
        val ai = context.applicationInfo
        EngineCore.init(ai.sourceDir, ai.nativeLibraryDir + "/libasmrglobal.so", ai.uid, null)
        EngineCore.emit("log 以 Shizuku 常驻服务的方式运行（App 被杀后继续处理）")
    }

    override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
        when (code) {
            INTERFACE_TRANSACTION -> {
                reply?.writeString(DESCRIPTOR)
                return true
            }
            TX_CALL -> {
                data.enforceInterface(DESCRIPTOR)
                val cmd = data.readString().orEmpty().trim()
                val r = if (cmd == "exit") {
                    exitSoon()
                    "bye"
                } else {
                    runCatching { EngineCore.call(cmd) }.getOrElse { "error ${HiddenAudio.describe(it)}" }
                }
                reply?.writeNoException()
                reply?.writeString(r)
                return true
            }
            TX_DESTROY -> {
                exitSoon()
                reply?.writeNoException()
                return true
            }
        }
        return super.onTransact(code, data, reply, flags)
    }

    /** 停掉拦截和雨声，回复发出去之后退出进程（系统随即注销音频策略，各 App 的声音恢复原样）。 */
    private fun exitSoon() {
        Thread({
            EngineCore.emit("log 服务进程退出")
            EngineCore.shutdown()
            Handler(Looper.getMainLooper()).postDelayed({ exitProcess(0) }, 200)
            Thread.sleep(1000)
            exitProcess(0)
        }, "asmr-exit").start()
    }
}
