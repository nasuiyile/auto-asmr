package io.github.nasuiyile.autoasmr.global.server

import android.annotation.SuppressLint
import android.app.Application
import android.content.Context
import android.content.ContextWrapper
import android.content.pm.ApplicationInfo
import android.os.Looper
import android.os.Process

/**
 * 给 app_process 起的服务进程补上普通 App 进程才有的东西：当前 ActivityThread、Application、包名、系统 Context。
 *
 * AOSP 的音频代码在这些为空时能正常工作，但厂商改过的框架不一定。例如荣耀（MagicOS，Android 17）的
 * AudioRecord.startRecording() 里有 Objects.requireNonNull(...)，空着就抛 NullPointerException。
 * 做法和 scrcpy 的 Workarounds 相同（要在主线程上调用）：
 * - Looper.prepareMainLooper()（ActivityThread 的构造函数要建 Handler）
 * - new ActivityThread()，设成 sCurrentActivityThread（ActivityThread.currentActivityThread() 不再为空）
 * - mConfigurationController（Android 12 起，取系统 Context 要用）
 * - mBoundApplication.appInfo.packageName / processName（currentPackageName()、currentProcessName()）
 * - mInitialApplication = 一个以 [ShellContext] 为底的 Application（currentApplication()、currentOpPackageName()）
 * ActivityThread、ConfigurationController 已经有的（例如 Shizuku 的 UserService 启动器建好了）就用现成的；
 * 包名和 Application 一律换成 shell / root 身份的：Shizuku 启动器放进去的是本 App 包名的 Context，
 * 音频系统按它的身份（UID 2000 + 本 App 包名，对不上）建流会被拒绝（Cannot create AudioTrack）。
 * 每一步失败都不致命，只记下来。
 */
// 只在服务进程（shell / root 身份，app_process 起的）里跑：那里没有对 App 的隐藏接口限制
@SuppressLint("BlockedPrivateApi", "DiscouragedPrivateApi", "PrivateApi", "SoonBlockedPrivateApi")
internal object ProcessEnv {
    /** ActivityThread.getSystemContext()；拿不到时为空，[ShellContext] 就退回到没有底的做法。 */
    @Volatile var systemContext: Context? = null
        private set

    val packageName: String get() = if (Process.myUid() == 0) "root" else "com.android.shell"

    private val shell: ShellContext by lazy { ShellContext(systemContext) }

    /** 进程里共用的 Context（给 AudioPolicy 等用）。 */
    fun context(): Context = shell

    @Volatile private var applied: String? = null

    /** 返回一行说明，哪些补上了、哪些没有。同一个进程里只做一次。 */
    @Synchronized
    fun apply(): String = applied ?: doApply().also { applied = it }

    private fun doApply(): String {
        val done = mutableListOf<String>()
        val failed = mutableListOf<String>()
        fun step(name: String, block: () -> Unit) {
            try {
                block()
                done += name
            } catch (e: Throwable) {
                failed += "$name（${HiddenAudio.describe(e)}）"
            }
        }

        if (Looper.getMainLooper() == null) step("主线程 Looper") { Looper.prepareMainLooper() }
        val atClass = Class.forName("android.app.ActivityThread")
        var at: Any? = null
        step("ActivityThread") {
            val cur = atClass.getDeclaredMethod("currentActivityThread").invoke(null)
            at = cur ?: atClass.getDeclaredConstructor().apply { isAccessible = true }.newInstance().also {
                atClass.getDeclaredField("sCurrentActivityThread").apply { isAccessible = true }.set(null, it)
            }
        }
        val thread = at ?: return "进程环境：补不上 ActivityThread（${failed.joinToString()}）"

        fun field(name: String) = atClass.getDeclaredField(name).apply { isAccessible = true }
        fun empty(name: String) = runCatching { field(name).get(thread) == null }.getOrDefault(true)

        // Android 12 起取系统 Context 要先有它（scrcpy #4467：有些三星机型上不补会崩）
        if (android.os.Build.VERSION.SDK_INT >= 31 && empty("mConfigurationController")) {
            step("ConfigurationController") {
                val cc = Class.forName("android.app.ConfigurationController")
                val internal = Class.forName("android.app.ActivityThreadInternal")
                val obj = cc.getDeclaredConstructor(internal).apply { isAccessible = true }.newInstance(thread)
                atClass.getDeclaredField("mConfigurationController").apply { isAccessible = true }.set(thread, obj)
            }
        }
        step("系统 Context") {
            systemContext = atClass.getDeclaredMethod("getSystemContext").invoke(thread) as Context
        }
        val hadBound = !empty("mBoundApplication")
        val hadApp = !empty("mInitialApplication")
        step(if (hadBound) "包名（替换）" else "包名") {
            val bindClass = Class.forName("android.app.ActivityThread\$AppBindData")
            val bind = bindClass.getDeclaredConstructor().apply { isAccessible = true }.newInstance()
            val info = ApplicationInfo().apply {
                packageName = this@ProcessEnv.packageName
                uid = Process.myUid()
            }
            bindClass.getDeclaredField("appInfo").apply { isAccessible = true }.set(bind, info)
            runCatching { bindClass.getDeclaredField("processName").apply { isAccessible = true }.set(bind, packageName) }
            atClass.getDeclaredField("mBoundApplication").apply { isAccessible = true }.set(thread, bind)
        }
        step(if (hadApp) "Application（替换）" else "Application") {
            val app = Application()
            ContextWrapper::class.java.getDeclaredField("mBase").apply { isAccessible = true }.set(app, shell)
            atClass.getDeclaredField("mInitialApplication").apply { isAccessible = true }.set(thread, app)
        }
        return "进程环境：补上了 " + done.joinToString("、") +
            (if (failed.isNotEmpty()) "；没补上 " + failed.joinToString("、") else "")
    }
}
