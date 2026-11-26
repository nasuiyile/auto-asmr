package io.github.nasuiyile.autoasmr.global

import android.content.ComponentName
import android.content.Context
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Base64
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.nasuiyile.autoasmr.BuildConfig
import io.github.nasuiyile.autoasmr.Settings
import io.github.nasuiyile.autoasmr.global.server.EngineService
import org.json.JSONObject
import rikka.shizuku.Shizuku
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * 全局模式（处理任意 App 的声音）在 App 这一侧的总控：起服务进程、连上它、推参数、定时读状态。
 * 网页模式（内嵌浏览器）不经过这里；两种模式共用 [Settings] 里的参数。
 *
 * - **Shizuku**（推荐）：服务进程是 Shizuku 的 UserService（server/EngineService.kt），用 daemon 模式绑定：
 *   App 进程被系统杀掉后它**继续处理声音**；App 再启动时（[init] 里收到 Shizuku 的 binder 后）用 peek 连回去，不会新开一个。
 * - **root**：`su -c` 起 server/ServerMain.kt，经标准输入 / 输出控制；它的生命跟着这条管道，App 进程没了它也退出。
 *
 * 所有到服务进程的调用都在一个后台线程里按顺序做；状态（[phase]、[poll]……）是界面上的镜像，在主线程更新。
 */
object Controller {
    private const val TAG = "AsmrController"
    private const val MAX_LOG = 200
    private const val MAIN_CLASS = "io.github.nasuiyile.autoasmr.global.server.ServerMain"
    private const val REQ_SHIZUKU = 1

    enum class How(val label: String) { SHIZUKU("Shizuku"), ROOT("root") }
    enum class Phase { IDLE, STARTING, CONNECTED }

    /** 服务进程的状态（`poll` 的回复）。[tele] 和安卓版 NativeHook.telemetry() 同一排列。 */
    data class Poll(
        val running: Boolean = false,
        val dry: Boolean = false,
        val rain: Boolean = false,
        val tele: FloatArray = FloatArray(0),
        val inPeak: Float = 0f,
        val outPeak: Float = 0f,
        /** 延迟各段（毫秒，量不到时 -1）：截取、输出流里攒着的缓冲、设备（声卡 / 蓝牙耳机）本身。 */
        val lagIn: Float = -1f,
        val lagBuf: Float = -1f,
        val lagDev: Float = -1f,
        val engine: Float = 0f,
        val underruns: Int = 0,
        /** 积压太多、丢掉旧声音追上实时的次数。 */
        val drops: Int = 0,
        /** 指定了输出设备、绕开拦截规则而被单独截下来的 App（服务进程给的说明，没有时为空）。 */
        val bypass: String = "",
        val fatal: String? = null,
    )

    var phase by mutableStateOf(Phase.IDLE)
        private set
    var how by mutableStateOf<How?>(null)
        private set
    /** 服务进程报上来的版本、身份、机型。 */
    var hello by mutableStateOf<String?>(null)
        private set
    var poll by mutableStateOf(Poll())
        private set
    /** Shizuku 此刻的状态（界面上显示），binder / 授权变化时更新。 */
    var shizuku by mutableStateOf("没运行")
        private set
    /** Shizuku 在运行、版本够用（不管授没授权）：首页默认进全局模式。 */
    var shizukuUp by mutableStateOf(false)
        private set
    val logs = mutableStateListOf<String>()
    /** 要告诉用户的一句话（界面上用 Toast 显示）。 */
    var message by mutableStateOf<String?>(null)

    val connected: Boolean get() = phase == Phase.CONNECTED
    val intercepting: Boolean get() = connected && poll.running

    private lateinit var app: Context
    private val main = Handler(Looper.getMainLooper())
    private val exec = Executors.newSingleThreadScheduledExecutor { Thread(it, "asmr-link") }
    private val listeners = CopyOnWriteArrayList<() -> Unit>()
    @Volatile private var link: Link? = null
    @Volatile private var pendingShizukuStart = false
    private var lastLogSeq = 0L
    private var polling = false

    /** 界面在前台时读得勤一些（位置图要动），否则 1.5 秒一次（通知栏用）。 */
    @Volatile var uiVisible = false
        set(v) {
            field = v
            if (v) exec.execute { pollOnce() }
        }

    private val args by lazy {
        Shizuku.UserServiceArgs(ComponentName(app.packageName, EngineService::class.java.name))
            .daemon(true)
            .processNameSuffix("engine")
            // 版本号用 APK 的安装时间：每次更新 / 重装后都不同，Shizuku 会结束旧的服务进程（还在跑旧代码），用新 APK 再起一个
            .version(installStamp())
    }

    private val conn = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            if (binder == null || !binder.pingBinder()) {
                addLog("# Shizuku 服务进程连不上")
                setPhase(Phase.IDLE)
                return
            }
            onConnected(BinderLink(binder), How.SHIZUKU)
        }

        override fun onServiceDisconnected(name: ComponentName?) {
            onDisconnected("Shizuku 服务进程断开了")
        }
    }

    private fun installStamp(): Int = runCatching {
        (app.packageManager.getPackageInfo(app.packageName, 0).lastUpdateTime / 1000 % Int.MAX_VALUE).toInt()
    }.getOrDefault(BuildConfig.VERSION_CODE)

    fun addListener(l: () -> Unit) = listeners.add(l)
    fun removeListener(l: () -> Unit) = listeners.remove(l)
    private fun changed() = listeners.forEach { it() }

    /** App 进程启动时调用一次。 */
    fun init(context: Context) {
        app = context.applicationContext
        Shizuku.addBinderReceivedListenerSticky {
            updateShizuku()
            // 服务进程可能还在（App 被杀后它继续跑）：连回去；不在就什么也不做
            if (phase == Phase.IDLE && granted()) peek()
        }
        Shizuku.addBinderDeadListener {
            updateShizuku()
            if (how == How.SHIZUKU) onDisconnected("Shizuku 停止了")
        }
        Shizuku.addRequestPermissionResultListener { code, result ->
            updateShizuku()
            if (code == REQ_SHIZUKU && pendingShizukuStart) {
                pendingShizukuStart = false
                if (result == PackageManager.PERMISSION_GRANTED) startShizuku() else addLog("# 没有得到 Shizuku 授权")
            }
        }
    }

    private fun granted(): Boolean = runCatching {
        Shizuku.pingBinder() && !Shizuku.isPreV11() && Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED
    }.getOrDefault(false)

    private fun updateShizuku() {
        val s = when {
            !Shizuku.pingBinder() -> "没运行（先在 Shizuku 里启动它）"
            Shizuku.isPreV11() -> "版本太旧，请更新 Shizuku"
            Shizuku.checkSelfPermission() != PackageManager.PERMISSION_GRANTED -> "可用，还没授权给本 App"
            else -> "可用（" + (if (Shizuku.getUid() == 0) "root" else "adb") + " 身份）"
        }
        val up = runCatching { Shizuku.pingBinder() && !Shizuku.isPreV11() }.getOrDefault(false)
        main.post {
            shizuku = s
            shizukuUp = up
        }
    }

    /** 用 Shizuku 起服务进程（常驻）。没授权时先弹 Shizuku 的授权框。 */
    fun startShizuku() {
        if (phase != Phase.IDLE) return
        if (!Shizuku.pingBinder()) {
            message = "Shizuku 没在运行：先打开 Shizuku 启动它（无线调试或 root）"
            return
        }
        if (Shizuku.isPreV11()) {
            message = "Shizuku 版本太旧"
            return
        }
        if (Shizuku.checkSelfPermission() != PackageManager.PERMISSION_GRANTED) {
            pendingShizukuStart = true
            Shizuku.requestPermission(REQ_SHIZUKU)
            return
        }
        setPhase(Phase.STARTING, How.SHIZUKU)
        addLog("# 用 Shizuku 启动服务进程（常驻）")
        try {
            Shizuku.bindUserService(args, conn)
        } catch (e: Throwable) {
            addLog("# Shizuku 启动失败：${e.javaClass.simpleName}: ${e.message}")
            setPhase(Phase.IDLE)
        }
    }

    /** 连回还在跑的 Shizuku 服务进程；它不在时什么也不做。 */
    private fun peek() {
        try {
            val v = Shizuku.peekUserService(args, conn)
            if (v >= 0) {
                addLog("# 服务进程还在运行，连回去")
                setPhase(Phase.STARTING, How.SHIZUKU)
            }
        } catch (e: Throwable) {
            Log.w(TAG, "peek", e)
        }
    }

    /** 用 su 起服务进程（跟着 App 进程走）。 */
    fun startRoot() {
        if (phase != Phase.IDLE) return
        setPhase(Phase.STARTING, How.ROOT)
        addLog("# 用 root 启动服务进程")
        exec.execute {
            val p = try {
                ProcessBuilder("su", "-c", serverCommand()).start()
            } catch (e: Throwable) {
                addLog("# root 启动失败：${e.javaClass.simpleName}: ${e.message}")
                setPhase(Phase.IDLE)
                return@execute
            }
            onConnected(PipeLink(p, onLine = ::addLog, onDead = { onDisconnected("root 服务进程退出了") }), How.ROOT)
        }
    }

    /** su 起服务进程的命令（adb 调试时手动跑的也是这条，见 tools/server.sh）。 */
    fun serverCommand(): String {
        val ai = app.applicationInfo
        val apk = ai.sourceDir
        val lib = ai.nativeLibraryDir + "/libasmrglobal.so"
        return "CLASSPATH='$apk' exec app_process /system/bin $MAIN_CLASS --apk '$apk' --lib '$lib' --client-uid ${ai.uid}"
    }

    private fun onConnected(l: Link, h: How) {
        exec.execute {
            link = l
            lastLogSeq = 0L
            val hi = l.call("hello") ?: return@execute onDisconnected("服务进程没有回应")
            addLog("服务进程：$hi")
            main.post {
                hello = hi
                how = h
                phase = Phase.CONNECTED
                changed()
            }
            // 先把参数推过去，再开始拦截（不然头一下是服务进程默认参数的声音）
            Settings.snapshot()?.let { sendConfig(l, it) }
            // 新起的进程还没在拦截；用户上次没暂停的话就开始。连回已在跑的进程时保持它原来的状态
            if (l.call("status") != "running" && !paused()) {
                val r = l.call("start")
                if (r != "ok") addLog("# 开始拦截失败：$r")
            }
            InterceptService.start(app)
            if (!polling) {
                polling = true
                exec.execute(::pollLoop)
            }
        }
    }

    private fun onDisconnected(why: String) {
        exec.execute {
            if (link == null && phase == Phase.IDLE) return@execute
            link?.close()
            link = null
            addLog("# $why")
            main.post {
                phase = Phase.IDLE
                how = null
                hello = null
                poll = Poll()
                changed()
                Settings.resyncRain() // 全局模式停了：雨声改回 App 自己放（网页模式的规则）
            }
        }
    }

    private fun setPhase(p: Phase, h: How? = how) {
        main.post {
            phase = p
            how = if (p == Phase.IDLE) null else h
            changed()
        }
    }

    // ---- 配置

    @Volatile private var pendingConfig: String? = null

    /**
     * 配置变了（[Settings] 调用）：把整份配置文件推给服务进程，连续改动只推最新的一份。
     * 推整份而不是只推 `params`：左右双人的延迟、排布、自动平衡存在配置文件里 `params` 之外，原声开关也在里面，
     * 两种模式这样用的是同一份配置。
     */
    fun pushConfig(text: String) {
        val first = synchronized(this) {
            val f = pendingConfig == null
            pendingConfig = text
            f
        }
        if (first) {
            exec.execute {
                val t = synchronized(this) { pendingConfig.also { pendingConfig = null } } ?: return@execute
                link?.let { sendConfig(it, t) }
            }
        }
    }

    private fun sendConfig(l: Link, text: String) {
        val r = l.call("config " + Base64.encodeToString(text.toByteArray(), Base64.NO_WRAP))
        if (r != null && r != "ok") addLog("# 配置没推过去：$r")
    }

    // ---- 控制

    private fun prefs() = app.getSharedPreferences("ui", 0)
    private fun paused() = prefs().getBoolean("paused", false)

    /** 开始 / 暂停拦截（记住选择：下次起服务进程时照这个来）。 */
    fun setIntercept(on: Boolean) {
        prefs().edit().putBoolean("paused", !on).apply()
        exec.execute {
            val l = link ?: return@execute
            val r = l.call(if (on) "start" else "stop")
            if (r != "ok") {
                addLog("# ${if (on) "开始" else "暂停"}拦截失败：$r")
                main.post { message = "${if (on) "开始" else "暂停"}拦截失败，详情见日志" }
            }
            pollOnce()
        }
    }

    /** 让服务进程停止拦截并退出。 */
    fun exit() {
        exec.execute {
            val l = link ?: return@execute
            val h = how
            l.call("exit")
            if (h == How.SHIZUKU) main.post { runCatching { Shizuku.unbindUserService(args, conn, true) } }
            onDisconnected("服务进程已退出")
        }
    }

    // ---- 定时读状态

    private fun pollLoop() {
        if (link == null) {
            polling = false
            return
        }
        pollOnce()
        exec.schedule(::pollLoop, if (uiVisible) 250 else 1500, TimeUnit.MILLISECONDS)
    }

    private fun pollOnce() {
        val l = link ?: return
        val r = l.call("poll") ?: return onDisconnected("服务进程没了")
        val o = runCatching { JSONObject(r) }.getOrNull() ?: return
        val t = o.optJSONArray("tele")
        val p = Poll(
            running = o.optBoolean("running"),
            dry = o.optBoolean("dry"),
            rain = o.optBoolean("rain"),
            tele = if (t != null) FloatArray(t.length()) { t.optDouble(it).toFloat() } else FloatArray(0),
            inPeak = o.optDouble("in").toFloat(),
            outPeak = o.optDouble("out").toFloat(),
            lagIn = o.optDouble("lag_in", -1.0).toFloat(),
            lagBuf = o.optDouble("lag_buf", -1.0).toFloat(),
            lagDev = o.optDouble("lag_dev", -1.0).toFloat(),
            engine = o.optDouble("engine").toFloat(),
            underruns = o.optInt("underruns"),
            drops = o.optInt("drops"),
            bypass = o.optString("bypass"),
            fatal = o.optString("fatal").ifEmpty { null },
        )
        val seq = o.optLong("log")
        if (seq != lastLogSeq) {
            val since = if (seq < lastLogSeq) 0L else lastLogSeq // 服务进程换了一个：从头取
            l.call("logs $since")?.takeIf { it.isNotEmpty() }?.lines()?.forEach(::addLog)
            lastLogSeq = seq
        }
        main.post {
            val was = poll.running
            poll = p
            changed()
            // 拦截开 / 关时决定雨声由谁放：拦截中由服务进程放，App 自己的雨声关掉，免得响两份
            if (was != p.running) Settings.resyncRain()
        }
    }

    fun addLog(line: String) {
        Log.i(TAG, line)
        main.post {
            logs.add(line)
            while (logs.size > MAX_LOG) logs.removeAt(0)
        }
    }
}
