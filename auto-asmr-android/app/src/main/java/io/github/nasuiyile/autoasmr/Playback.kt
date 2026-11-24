package io.github.nasuiyile.autoasmr

import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.webkit.WebViewCompat
import io.github.nasuiyile.autoasmr.global.Controller

/**
 * 播放的总控：每秒看一次（不依赖界面，切到后台也照常跑）。
 *
 * - **前台服务**（[PlaybackService]）：App 的界面开着（[uiAlive]）、全局模式又没在用时一直开着，通知栏里随时找得到
 *   auto-asmr，可以启用 / 关闭效果、退出（全局模式在用时它自己有一条通知，这条就不重复了）。有声音（网页在出声或雨声在响）
 *   时不管界面开没开都开着，换成播放通知（[media]），可以暂停 / 继续；没声音 30 秒后换回普通通知（界面也关了就关掉）。
 *   用户从通知栏暂停的，10 分钟内都留着播放通知，方便接着放。前台服务让 App 在后台、锁屏时不被冻结。
 * - **暂停 / 继续**：让正在出声的标签页里所有框架的媒体和 Web Audio 暂停 / 继续（发消息给注入的脚本），雨声也一起停。
 * - **截取自检**：网页在出声、却 4 秒都没有被截到的 AAudio 流，就装上备用的 inline hook，并让这个标签页静音约 6 秒
 *   再恢复（Chromium 关掉旧流、开一条新流，新流就截得到）；装了还截不到，就提示这个 WebView 不支持。
 */
object Playback {
    private const val TAG = "AsmrPlayback"
    private const val KEEP_MS = 30_000L
    private const val KEEP_PAUSED_MS = 10 * 60_000L

    var browser: Browser? = null
    private lateinit var app: Context
    private val main = Handler(Looper.getMainLooper())

    /** 用户从通知栏 / 耳机按键暂停了。 */
    var pausedByUser by mutableStateOf(false)
        private set
    /** 截取有问题时给用户看的提示；没问题时为 null。 */
    var hookProblem by mutableStateOf<String?>(null)
        private set
    var serviceRunning by mutableStateOf(false)
        private set
    /** 主界面开着（MainActivity 建好、还没被关掉 / 移出最近任务）：这期间通知栏里一直有 auto-asmr。 */
    var uiAlive = false
    /** 通知栏「退出」时关掉主界面（MainActivity 设置）。 */
    var closeUi: (() -> Unit)? = null
    /** 用户在通知栏点了「退出」（再打开界面时清掉）：网页声音和雨声都停下。 */
    var exited = false
        private set
    /** 通知该是播放通知（有声音、刚停不到 30 秒、用户暂停不到 10 分钟），否则是普通的常驻通知。 */
    var media = false
        private set

    private var lastSound = 0L
    private var startBlocked = false
    private var pausedAt = 0L
    private var noHookSec = 0
    private var triedInline = false

    fun init(context: Context) {
        app = context.applicationContext
        main.post(tick)
    }

    private val tick = object : Runnable {
        override fun run() {
            runCatching { check() }.onFailure { Log.w(TAG, "check", it) }
            main.postDelayed(this, 1000)
        }
    }

    /** 此刻有没有声音（网页在出声或雨声在响）。 */
    fun sounding(): Boolean = browser?.audibleTab != null || NativeHook.rainPlaying()

    fun title(): String {
        val t = browser?.audibleTab ?: (browser?.pinned ?: browser?.current)
        return t?.title?.ifEmpty { t.url }?.takeIf { it.isNotEmpty() } ?: "auto-asmr"
    }

    private fun check() {
        val now = SystemClock.elapsedRealtime()
        val sounding = sounding()
        if (sounding) lastSound = now
        media = sounding || now - lastSound < KEEP_MS || (pausedByUser && now - pausedAt < KEEP_PAUSED_MS)
        val keep = media || (uiAlive && Controller.phase == Controller.Phase.IDLE)
        if (keep && !serviceRunning && !startBlocked) startService() else if (!keep && serviceRunning) stopService()
        PlaybackService.instance?.update()
        selfCheck()
    }

    private fun startService() {
        try {
            app.startForegroundService(Intent(app, PlaybackService::class.java))
            serviceRunning = true
        } catch (e: Exception) {
            // Android 12 起不能从后台启动前台服务：一般是在界面上开始的，那时已经开了，这里只是兜底。
            // 失败了就等回到界面再试（[onForeground]），不每秒重试
            Log.w(TAG, "前台服务启动失败：$e")
            startBlocked = true
        }
    }

    /** 回到界面（MainActivity.onResume）：之前在后台没开起来的前台服务可以再试。 */
    fun onForeground() {
        startBlocked = false
    }

    private fun stopService() {
        app.stopService(Intent(app, PlaybackService::class.java))
        serviceRunning = false
    }

    fun pause() {
        val b = browser ?: return
        pausedByUser = true
        pausedAt = SystemClock.elapsedRealtime()
        b.sendToAudible("pause")
        Settings.resyncRain()
        PlaybackService.instance?.update()
    }

    fun resume() {
        val b = browser ?: return
        pausedByUser = false
        lastSound = SystemClock.elapsedRealtime()
        b.sendToAudible("play")
        Settings.resyncRain()
        PlaybackService.instance?.update()
    }

    fun togglePause() = if (pausedByUser) resume() else pause()

    /** 通知栏的「退出」：停掉网页的声音，关掉主界面和通知。全局模式、正在转换的文件不受影响（各自有自己的控制）。 */
    fun exit() {
        exited = true
        browser?.sendToAudible("pause")
        pausedByUser = false
        lastSound = 0L
        media = false
        uiAlive = false
        Settings.resyncRain()
        closeUi?.invoke()
        if (serviceRunning) stopService()
    }

    /** 主界面建好了（MainActivity.onCreate）：通知栏里开始常驻；之前退出过的话，雨声按设置恢复。 */
    fun uiCreated() {
        uiAlive = true
        if (exited) {
            exited = false
            Settings.resyncRain()
        }
    }

    private fun selfCheck() {
        val b = browser ?: return
        val st = NativeHook.hookState()
        if (st[2] > 0) {
            noHookSec = 0
            hookProblem = null
            return
        }
        if (b.audibleTab == null) {
            noHookSec = 0
            return
        }
        noHookSec++
        if (noHookSec != 4) return
        val wv = WebViewCompat.getCurrentWebViewPackage(app)?.let { "${it.packageName} ${it.versionName}" } ?: "未知"
        if (!triedInline && st[1] == 0) {
            triedInline = true
            val fails = NativeHook.installInline()
            Log.w(TAG, "网页在出声但没截到声音（PLT hook 截到的调用方 ${st[0]} 个），改用 inline hook：失败 $fails 个；WebView $wv")
            hookProblem = "没有截获到网页的声音，正在改用备用方式：声音会停几秒再恢复。"
            b.audibleTab?.let { b.reopenAudio(it) }
            noHookSec = -10 // 静音 6 秒、重新开流的这段时间不算
        } else {
            Log.w(TAG, "备用方式也没截到声音；WebView $wv")
            hookProblem = "截不到这个网页的声音（WebView $wv）。声音照常播放，但没有 ASMR 效果。"
        }
    }
}
