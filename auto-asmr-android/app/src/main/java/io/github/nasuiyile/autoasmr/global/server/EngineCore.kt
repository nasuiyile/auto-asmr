package io.github.nasuiyile.autoasmr.global.server

import android.os.Build
import android.os.Process
import android.util.Base64
import android.util.Log
import io.github.nasuiyile.autoasmr.BuildConfig
import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale

/**
 * 服务进程的核心：引擎、拦截（[AudioRouter]）、雨声（[RainPlayer]）、命令处理、统计和日志。
 * 两种入口用的是同一份：
 * - [EngineService]：Shizuku 的 UserService（常驻进程，App 被杀也继续跑），命令经 binder 传；
 * - [ServerMain]：app_process 直接起（root 的 su、adb 调试），命令经标准输入 / 输出传。
 *
 * 命令一行一条，每条有一个回复（字符串）：
 * - `hello` → 版本、身份、系统版本、机型
 * - `start [only=UID,UID]` / `stop` → `ok` 或 `error …`
 * - `params <base64 的 RON>`：换参数（App 改了参数就推过来，和配置文件里 params 那一段同一格式）
 * - `dry 0|1`：监听原声（不经过引擎，只乘输出增益、过峰值限制器）/ 输出效果
 * - `config <base64 的 .ron>`：导入整份配置（调试用；App 用 params + dry）
 * - `style 0..3`、`action <名字>`：调试用，直接改服务进程里的参数
 * - `poll` → JSON：是否在拦截、原声、雨声、位置等状态（和安卓版 telemetry() 同一排列）、统计、日志序号
 * - `logs <序号>` → 这个序号之后的日志，一行一条
 * - `status` → `running` / `stopped`
 */
internal object EngineCore {
    private const val TAG = "AsmrShizuku"
    private const val MAX_LOG = 200

    private lateinit var router: AudioRouter
    private lateinit var rain: RainPlayer
    private var excludeUids = intArrayOf()
    @Volatile private var fatal: String? = null
    @Volatile private var hello = ""
    /** 标准输出（ServerMain 模式）；为空时只写 logcat 和日志缓冲。 */
    @Volatile private var stdout: ((String) -> Unit)? = null

    private val logs = ArrayDeque<String>()
    @Volatile private var logSeq = 0L

    // 最近 1 秒的统计（统计线程每秒算一次）
    @Volatile private var statLine = ""
    @Volatile private var cpuPct = 0f
    @Volatile private var peakIn = 0f
    @Volatile private var peakOut = 0f
    @Volatile private var bgLevel = 0.0
    @Volatile private var dry = false

    /** 发一条消息：logcat；日志缓冲（`logs` 命令取）；ServerMain 模式下还写标准输出。 */
    fun emit(line: String) {
        Log.i(TAG, line)
        if (!line.startsWith("stat ") && !line.startsWith("onset ")) {
            synchronized(logs) {
                logs.addLast(line)
                while (logs.size > MAX_LOG) logs.removeFirst()
                logSeq++
            }
        }
        stdout?.invoke(line)
    }

    /**
     * 初始化：补进程环境、加载引擎。出错不抛出，记在 [fatal] 里（`hello` / `poll` 里能看到），服务进程照样能回答命令。
     * [clientUid] 是 App 的 UID：App 自己的声音不拦。
     */
    @Synchronized
    fun init(apk: String, lib: String?, clientUid: Int?, out: ((String) -> Unit)?, reportOnsets: Boolean = false) {
        stdout = out
        val uid = Process.myUid()
        hello = "ver=${BuildConfig.VERSION_NAME} uid=$uid pid=${Process.myPid()} sdk=${Build.VERSION.SDK_INT} device=${Build.MANUFACTURER} ${Build.MODEL}"
        emit("hello $hello")
        // 先补上 ActivityThread / Application / 包名：厂商改过的音频框架会用到（见 ProcessEnv）
        emit("log " + ProcessEnv.apply())
        Engine.load(apk, lib, ::emit)?.let {
            fatal = it
            emit("fatal $it")
        }
        // 自己的输出流不能再被拦回来：排除自己的 UID；root 身份下系统可能把播放记在 system（1000）名下，一起排除
        excludeUids = buildList {
            add(uid)
            if (uid == 0) add(Process.SYSTEM_UID)
            if (clientUid != null && clientUid != uid) add(clientUid)
        }.toIntArray()
        router = AudioRouter(::emit).apply { this.reportOnsets = reportOnsets }
        rain = RainPlayer(::emit)
        Thread(::statsLoop, "asmr-stats").apply { isDaemon = true; start() }
        emit("ready")
    }

    val running: Boolean get() = ::router.isInitialized && router.isRunning

    /** 处理一条命令，返回回复。 */
    fun call(line: String): String {
        val cmd = line.substringBefore(' ').trim()
        val arg = line.substringAfter(' ', "").trim()
        if (cmd == "hello") return hello + (fatal?.let { " fatal=$it" } ?: "")
        if (cmd == "poll") return poll()
        if (cmd == "logs") return logsSince(arg.toLongOrNull() ?: 0L)
        if (fatal != null) return "error 引擎没加载：$fatal"
        return synchronized(this) { handle(cmd, arg) }
    }

    private fun handle(cmd: String, arg: String): String = when (cmd) {
        "start" -> {
            val only = arg.removePrefix("only=").takeIf { arg.startsWith("only=") }
                ?.split(',')?.mapNotNull { it.trim().toIntOrNull() }?.toIntArray() ?: intArrayOf()
            val err = router.start(excludeUids, only)
            syncRain()
            if (err != null) {
                emit("error 开始拦截失败：$err")
                "error $err"
            } else {
                emit("state running")
                "ok"
            }
        }
        "stop" -> {
            router.stop()
            syncRain()
            emit("state stopped")
            "ok"
        }
        "dry" -> {
            setDry(arg == "1" || arg == "true")
            "ok"
        }
        "params" -> {
            val ron = decode(arg) ?: return "error 参数不是 base64"
            if (Engine.setParamsRon(ron) == 0) {
                readParams()
                "ok"
            } else {
                "error 参数读不懂：${Engine.lastError()}"
            }
        }
        "config" -> {
            val text = decode(arg) ?: return "error 配置不是 base64"
            if (Engine.loadConfig(text) == 0) {
                readParams(applyDry = true)
                "ok"
            } else {
                "error 导入配置失败：${Engine.lastError()}"
            }
        }
        "style" -> {
            if (Engine.setStyle(arg.toIntOrNull() ?: 0) == 0) readParams().let { "ok" } else "error ${Engine.lastError()}"
        }
        "action" -> {
            if (Engine.action(arg) == 0) readParams().let { "ok" } else "error ${Engine.lastError()}"
        }
        "status" -> if (router.isRunning) "running" else "stopped"
        else -> "error 不认识的命令：$cmd"
    }

    private fun decode(b64: String): String? =
        runCatching { String(Base64.decode(b64, Base64.DEFAULT), Charsets.UTF_8) }.getOrNull()

    private fun setDry(value: Boolean) {
        dry = value
        Engine.setDry(value)
        syncRain()
    }

    /** 参数换过之后：记下背景音音量（决定雨声开不开）；导入整份配置时连原声开关一起用配置里的。 */
    private fun readParams(applyDry: Boolean = false) {
        val p = Engine.getParamsJson()?.let { runCatching { JSONObject(it) }.getOrNull() } ?: return
        bgLevel = p.optDouble("bg_level", 0.0)
        if (applyDry) setDry(p.optBoolean("dry", false)) else syncRain()
    }

    /** 雨声：在拦截、不是原声、背景音音量 > 0 时开（和安卓版一样，监听原声时没有效果也没有雨声）。 */
    private fun syncRain() {
        rain.set(router.isRunning && !dry && bgLevel > 0)
    }

    /** 停掉一切（退出前调用）。 */
    @Synchronized
    fun shutdown() {
        if (!::router.isInitialized) return
        router.stop()
        rain.set(false)
    }

    private fun poll(): String {
        val o = JSONObject()
        o.put("running", running)
        o.put("dry", dry)
        o.put("log", logSeq)
        fatal?.let { o.put("fatal", it) }
        if (!::router.isInitialized) return o.toString()
        val r = router
        o.put("rain", rain.playing)
        // 和安卓版 NativeHook.telemetry() 同一排列：[0] 有没有在处理声音 [1] 方位角 [2] 仰角 [3] 距离 [4] 说话活动
        // [5] 处理占用 [6] 延迟 [7] 断流 [8] 采样率 [9] 流编号 [10] 引擎出过错 [11] 左右双人 [12..15] 2 号
        val tele = FloatArray(16)
        val t = r.tele()
        if (t != null && r.isRunning && !r.gated) {
            tele[0] = 1f
            for (i in 0 until 4) tele[1 + i] = t[i]
            tele[5] = cpuPct
            tele[6] = latencyMs(r)
            tele[7] = r.underruns.toFloat()
            tele[8] = AudioRouter.SR.toFloat()
            tele[10] = t[5]
            for (i in 0 until 5) tele[11 + i] = t[6 + i]
        }
        o.put("tele", JSONArray().apply { tele.forEach { put(it.toDouble()) } })
        o.put("in", peakIn.toDouble())
        o.put("out", peakOut.toDouble())
        // 延迟各段（毫秒，量不到时 -1）：截取（录音端）、缓冲（输出流里攒着的）、设备（声卡 / 蓝牙耳机本身）
        o.put("lag_in", r.lagInMs.toDouble().takeIf { it.isFinite() } ?: -1.0)
        o.put("lag_buf", r.lagBufMs.toDouble().takeIf { it.isFinite() } ?: -1.0)
        o.put("lag_dev", r.lagDevMs.toDouble().takeIf { it.isFinite() } ?: -1.0)
        o.put("engine", r.engineLatencyMs.toDouble())
        o.put("underruns", r.underruns)
        o.put("drops", r.drops)
        o.put("stat", statLine)
        return o.toString()
    }

    /** 从声音被截下到真正放出来的延迟估计：截取 + 引擎 + 输出节拍 + 输出流里攒着的 + 设备本身。 */
    private fun latencyMs(r: AudioRouter): Float {
        val a = r.lagInMs.takeIf { it.isFinite() } ?: 0f
        val b = r.lagBufMs.takeIf { it.isFinite() } ?: 0f
        val c = r.lagDevMs.takeIf { it.isFinite() } ?: 0f
        return a + b + c + r.paceLatencyMs + (if (dry) 0f else r.engineLatencyMs)
    }

    private fun logsSince(seq: Long): String = synchronized(logs) {
        val n = (logSeq - seq).coerceIn(0, logs.size.toLong()).toInt()
        logs.toList().takeLast(n).joinToString("\n")
    }

    /** 每秒算一次统计；ServerMain 模式下拦截中每秒往标准输出写一行 stat。 */
    private fun statsLoop() {
        var lastFrames = 0L
        var lastProcNs = 0L
        while (true) {
            Thread.sleep(1000)
            val r = router
            if (!r.isRunning) {
                lastFrames = 0; lastProcNs = 0
                cpuPct = 0f; peakIn = 0f; peakOut = 0f
                continue
            }
            val frames = r.framesIn
            val procNs = r.procNs
            cpuPct = ((procNs - lastProcNs) / 1e7).toFloat() // 占单核的百分比（1 s = 1e9 ns）
            peakIn = r.peakIn
            peakOut = r.peakOut
            statLine = String.format(
                Locale.ROOT,
                "in=%.3f out=%.3f frames=%d gated=%b lag_in=%.1f buf=%.1f dev=%.1f engine=%.1f cpu=%.1f underruns=%d target=%.1f trim=%.0f drops=%d read_err=%d",
                peakIn, peakOut, frames - lastFrames, r.gated, r.lagInMs, r.lagBufMs, r.lagDevMs,
                r.engineLatencyMs, cpuPct, r.underruns, r.targetMs, r.trimPpm, r.drops, r.readErrors,
            )
            if (stdout != null) emit("stat $statLine")
            r.peakIn = 0f
            r.peakOut = 0f
            lastFrames = frames
            lastProcNs = procNs
        }
    }
}
