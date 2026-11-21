package io.github.nasuiyile.autoasmr.shizuku

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.nasuiyile.autoasmr.shizuku.server.Engine
import org.json.JSONObject
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * 效果参数，和安卓版（auto-asmr-android）的 Settings 是同一套：真正的值在 App 进程自己加载的那份 Rust 引擎里
 * （和电脑版同一份 `Params`、同一个配置文件格式），这里是界面上的镜像。
 *
 * - 改参数在后台单线程里调用 Rust，同一个参数连续改只送最新的值；改完把全部参数读回来（有些改动会连带改别的参数）。
 * - 每次改动后把参数（RON）和原声开关推给服务进程（[Controller.pushParams]），服务进程里的引擎用它处理声音。
 * - 每次改动后 1 秒自动保存到 `files/auto-asmr.ron`，格式和电脑版的配置文件相同。
 * - 手动摆位的开关逻辑照搬电脑版：打开时记下原来的移动方式和「响度随位置变化」，播放中打开时声源停在此刻的位置；关掉时恢复。
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

    /** 启动时的加载做完了没有：导入、推参数要等它做完。 */
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
        Engine.provideData(ku, pi)
        Engine.initEngine()
        val f = File(context.filesDir, FILE)
        saveFile = f
        if (f.isFile) {
            val r = Engine.loadConfig(f.readText())
            Log.i(TAG, if (r == 0) "读取配置 ${f.path}" else "配置读不懂，用默认值：${Engine.lastError()}")
        }
        val s = Engine.uiSpec()?.let { JSONObject(it) }
        val p = Engine.getParamsJson()?.let { JSONObject(it) } ?: JSONObject()
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
            if (Engine.action(name) != 0) notify(Engine.lastError() ?: "操作失败")
            refresh()
        }
    }

    /** 输出效果 / 监听原声（电脑版的「输出效果 / 监听原声」）。 */
    fun setDry(dry: Boolean) {
        worker.execute {
            Engine.setDry(dry)
            refresh()
        }
    }

    /** 手动摆位开关（电脑版位置图下面的勾选框）。[pose] = 引擎此刻的 [方位角, 仰角, 距离]，没在播放时为空。 */
    fun setManual(on: Boolean, pose: FloatArray?) {
        worker.execute {
            if (on) {
                Engine.setField("mode", "\"Manual\"")
                if (pose != null) {
                    val m = Engine.manualFromPose(pose[0], pose[1], pose[2])
                    Engine.setField("manual_az", m[0].toString())
                    Engine.setField("manual_dist", m[1].toString())
                    Engine.setField("manual_tilt", m[2].toString())
                }
            } else {
                Engine.setField("mode", "\"$autoMode\"")
                Engine.setField("level_follow", autoLevelFollow.toString())
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
            if (Engine.setField(k, v) != 0) notify(Engine.lastError() ?: "$k 没改成")
        }
        refresh()
    }

    private fun refresh() {
        val p = Engine.getParamsJson()?.let { JSONObject(it) } ?: return
        main.post { setParams(p) }
        push(p)
        scheduleSave()
    }

    /** 把参数和原声开关推给服务进程（连上服务进程时也调一次，见 [pushAll]）。 */
    private fun push(p: JSONObject) {
        val ron = Engine.getParamsRon() ?: return
        Controller.pushParams(ron, p.optBoolean("dry"))
    }

    /** 刚连上服务进程时用：当前参数（RON）和原声开关（等启动时的加载做完；在调用方的线程里读）。 */
    fun snapshot(): Pair<String, Boolean>? {
        loaded.await(15, TimeUnit.SECONDS)
        val ron = Engine.getParamsRon() ?: return null
        val dry = Engine.getParamsJson()?.let { runCatching { JSONObject(it).optBoolean("dry") }.getOrNull() } ?: false
        return ron to dry
    }

    private val saveTask = Runnable { worker.execute { save() } }

    private fun scheduleSave() {
        main.removeCallbacks(saveTask)
        main.postDelayed(saveTask, 1000)
    }

    private fun save() {
        val f = saveFile ?: return
        val text = Engine.exportConfig() ?: return
        val tmp = File(f.path + ".tmp")
        tmp.writeText(text)
        tmp.renameTo(f)
    }

    /** 导出配置文件的内容（和电脑版「导出配置」同一格式）。 */
    fun exportText(): String? = Engine.exportConfig()

    /** 导入配置文件：整份换掉（电脑版独有的设置也留着，下次导出时原样写回）。 */
    fun importText(text: String, done: (Boolean) -> Unit) {
        worker.execute {
            loaded.await(15, TimeUnit.SECONDS)
            val ok = Engine.loadConfig(text) == 0
            val err = if (ok) null else Engine.lastError()
            val p = Engine.getParamsJson()?.let { JSONObject(it) }
            if (ok && p != null) push(p)
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
            if (Engine.setParamsRon(ron) != 0) notify("参数没换成：${Engine.lastError()}")
            refresh()
        }
    }

    private fun notify(msg: String) {
        Log.w(TAG, msg)
        main.post { message = msg }
    }
}
