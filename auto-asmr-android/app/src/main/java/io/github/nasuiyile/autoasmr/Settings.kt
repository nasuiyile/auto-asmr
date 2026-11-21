package io.github.nasuiyile.autoasmr

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.nasuiyile.autoasmr.global.Controller
import org.json.JSONObject
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * 效果参数：真正的值在 Rust 里（和电脑版同一份 `Params`、同一个配置文件格式），这里只是界面上的镜像。
 *
 * - 改参数在后台单线程里调用 Rust（生成引擎配置要几毫秒），同一个参数连续改只送最新的值；改完把全部参数读回来
 *   （有些改动会连带改别的参数，比如换移动方式会改「响度随位置变化」，和电脑版一样）。
 * - 每次改动后 1 秒自动保存到 `files/auto-asmr.ron`，格式和电脑版的配置文件相同，可以直接拷过去用。
 * - 全局模式（[Controller]）连着服务进程时，每次改动后把整份配置（和保存的文件一样，含左右双人、原声开关）推给它；
 *   服务进程里的引擎用它处理其他 App 的声音。
 * - 手动摆位的开关逻辑照搬电脑版：打开时记下原来的移动方式和「响度随位置变化」，播放中打开时声源停在此刻的位置；
 *   关掉时恢复。
 */
object Settings {
    private const val TAG = "AsmrSettings"
    const val FILE = "auto-asmr.ron"

    /** 参数表（ui_spec.json）。 */
    var spec by mutableStateOf<JSONObject?>(null)
        private set

    /** 全部参数（JSON，键是 Params 的字段名，另有虚拟字段和 dry）。 */
    var params by mutableStateOf(JSONObject())
        private set

    var ready by mutableStateOf(false)
        private set

    /** 最近一条要告诉用户的消息（导入导出的结果等）。 */
    var message by mutableStateOf<String?>(null)

    private val worker = Executors.newSingleThreadExecutor { Thread(it, "asmr-settings") }
    private val main = Handler(Looper.getMainLooper())
    private val pending = LinkedHashMap<String, String>()
    private var saveFile: File? = null

    // 电脑版界面里的 auto_mode / auto_level_follow：取消手动摆位时回到哪种移动方式
    private var autoMode = "Sweep"
    private var autoLevelFollow = 0.2

    /** 背景雨声在不在开（音量 > 0 且不是原声）。 */
    var rainOn by mutableStateOf(false)
        private set
    private var rainSent: Boolean? = null

    /** 启动时的加载做完了没有：导入要等它做完，免得导入的配置被启动时读的旧配置盖掉。 */
    private val loaded = CountDownLatch(1)

    /** App 启动时在后台线程调用：数据文件交给引擎、加载引擎、读上次保存的配置。 */
    fun load(context: Context) {
        try {
            doLoad(context)
        } finally {
            loaded.countDown()
        }
    }

    private fun doLoad(context: Context) {
        val t0 = System.nanoTime()
        val ku = context.assets.open("ku100.bin").use { it.readBytes() }
        val pi = context.assets.open("pinna.bin").use { it.readBytes() }
        NativeHook.provideData(ku, pi)
        NativeHook.initEngine()
        val f = File(context.filesDir, FILE)
        saveFile = f
        if (f.isFile) {
            val r = NativeHook.loadConfig(f.readText())
            Log.i(TAG, if (r == 0) "读取配置 ${f.path}" else "配置读不懂，用默认值：${NativeHook.lastError()}")
        }
        val s = NativeHook.uiSpec()?.let { JSONObject(it) }
        val p = NativeHook.getParamsJson()?.let { JSONObject(it) } ?: JSONObject()
        NativeHook.setDry(p.optBoolean("dry", false))
        syncRain(p)
        main.post {
            spec = s
            setParams(p, initial = true)
            ready = true
        }
        Log.i(TAG, "设置就绪 ${(System.nanoTime() - t0) / 1_000_000} ms")
    }

    private fun setParams(p: JSONObject, initial: Boolean = false) {
        val old = params
        params = p
        val mode = p.optString("mode")
        if (initial) {
            autoMode = if (mode == "Manual") "Sweep" else mode
            autoLevelFollow = if (mode == "Manual") 0.2 else p.optDouble("level_follow", 0.2)
        } else if (mode == "Manual" && old.optString("mode") != "Manual") {
            // 进手动：记下进来之前的方式和「响度随位置变化」（同电脑版 sync_params）
            autoMode = old.optString("mode", "Sweep")
            autoLevelFollow = old.optDouble("level_follow", 0.2)
        }
    }

    /** 改一个参数（值是 JSON 字面量：数字、true / false、"字符串"、对象）。 */
    fun set(key: String, json: String) {
        synchronized(pending) { pending[key] = json }
        worker.execute { drain() }
    }

    fun setNumber(key: String, v: Double) = set(key, if (v.isFinite()) v.toString() else "0")

    fun setBool(key: String, v: Boolean) = set(key, v.toString())

    /** 风格、恢复默认、雨夜、关雨声。 */
    fun action(name: String) {
        worker.execute {
            if (NativeHook.action(name) != 0) notify(NativeHook.lastError() ?: "操作失败")
            refresh()
        }
    }

    /** 输出效果 / 监听原声（电脑版的「输出效果 / 监听原声」）。 */
    fun setDry(dry: Boolean) {
        worker.execute {
            NativeHook.setDry(dry)
            refresh()
        }
    }

    /** 手动摆位开关（电脑版位置图下面的勾选框）。[pose] = 引擎此刻的 [方位角, 仰角, 距离]，没在播放时为空。 */
    fun setManual(on: Boolean, pose: FloatArray?) {
        worker.execute {
            if (on) {
                NativeHook.setField("mode", "\"Manual\"")
                if (pose != null) {
                    val m = NativeHook.manualFromPose(pose[0], pose[1], pose[2])
                    NativeHook.setField("manual_az", m[0].toString())
                    NativeHook.setField("manual_dist", m[1].toString())
                    NativeHook.setField("manual_tilt", m[2].toString())
                }
            } else {
                NativeHook.setField("mode", "\"$autoMode\"")
                NativeHook.setField("level_follow", autoLevelFollow.toString())
            }
            refresh()
        }
    }

    private fun drain() {
        while (true) {
            val (k, v) = synchronized(pending) {
                val e = pending.entries.firstOrNull() ?: return@synchronized null
                pending.remove(e.key)
                e.key to e.value
            } ?: break
            if (NativeHook.setField(k, v) != 0) notify(NativeHook.lastError() ?: "$k 没改成")
        }
        refresh()
    }

    private fun refresh() {
        val p = NativeHook.getParamsJson()?.let { JSONObject(it) } ?: return
        syncRain(p)
        main.post { setParams(p) }
        push(p)
        scheduleSave()
    }

    /** 把整份配置推给全局模式的服务进程（没连着时什么也不做）。 */
    @Suppress("UNUSED_PARAMETER")
    private fun push(p: JSONObject) {
        if (!Controller.connected) return
        NativeHook.exportConfig()?.let { Controller.pushConfig(it) }
    }

    /** 全局模式刚连上服务进程时用：整份配置（等启动时的加载做完；在调用方的线程里读）。 */
    fun snapshot(): String? {
        loaded.await(15, TimeUnit.SECONDS)
        return NativeHook.exportConfig()
    }

    /**
     * 雨声单独一条输出流：背景音音量 > 0、而且不是原声时打开（和电脑版一样，监听原声时没有效果也没有雨声）。
     * 全局模式在拦截时雨声由服务进程放，这里关掉，免得响两份。
     */
    private fun syncRain(p: JSONObject) {
        val want = p.optDouble("bg_level", 0.0) > 0 && !p.optBoolean("dry") && !Playback.pausedByUser && !Controller.intercepting
        if (want != rainSent) {
            rainSent = want
            NativeHook.setRain(want)
            main.post { rainOn = want }
        }
    }

    /** 暂停 / 继续（通知栏）后重新决定雨声。 */
    fun resyncRain() {
        worker.execute { NativeHook.getParamsJson()?.let { syncRain(JSONObject(it)) } }
    }

    private val saveTask = Runnable { worker.execute { save() } }

    private fun scheduleSave() {
        main.removeCallbacks(saveTask)
        main.postDelayed(saveTask, 1000)
    }

    private fun save() {
        val f = saveFile ?: return
        val text = NativeHook.exportConfig() ?: return
        val tmp = File(f.path + ".tmp")
        tmp.writeText(text)
        tmp.renameTo(f)
    }

    /** 导出配置文件的内容（和电脑版「导出配置」同一格式）。 */
    fun exportText(): String? = NativeHook.exportConfig()

    /** 导入配置文件：整份换掉（电脑版独有的设置也留着，下次导出时原样写回）。 */
    fun importText(text: String, done: (Boolean) -> Unit) {
        worker.execute {
            loaded.await(15, TimeUnit.SECONDS)
            val ok = NativeHook.loadConfig(text) == 0
            val err = if (ok) null else NativeHook.lastError()
            val p = NativeHook.getParamsJson()?.let { JSONObject(it) }
            if (ok && p != null) {
                NativeHook.setDry(p.optBoolean("dry", false))
                syncRain(p)
                push(p)
            }
            main.post {
                if (ok && p != null) setParams(p, initial = true)
                message = if (ok) "配置已导入" else "导入失败：$err"
                done(ok)
            }
            if (ok) save()
        }
    }

    /** 调试 intent 用：整段换参数（RON）。 */
    fun setRon(ron: String) {
        worker.execute {
            if (NativeHook.setParamsRon(ron) != 0) notify("参数没换成：${NativeHook.lastError()}")
            refresh()
        }
    }

    private fun notify(msg: String) {
        Log.w(TAG, msg)
        main.post { message = msg }
    }
}
