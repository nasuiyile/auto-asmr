package io.github.nasuiyile.autoasmr.global.server

import java.io.File
import java.util.zip.ZipFile

/**
 * 全局模式服务进程里的 Rust 引擎（auto-asmr-android/rust，和网页模式同一个静态库），经 app/src/main/cpp/global_jni.c
 * （libasmrglobal.so）调用：处理截下来的声音、放背景雨声；参数由 App 推过来（`params` 命令）。
 * App 进程不用它：App 进程里的参数、界面走 NativeHook（libasmrhook.so），和网页模式共用。
 */
object Engine {
    @JvmStatic external fun provideData(ku100: ByteArray, pinna: ByteArray): Int
    @JvmStatic external fun initEngine(): Int
    @JvmStatic external fun procNew(sampleRate: Int, channels: Int): Long
    @JvmStatic external fun procFree(p: Long)
    @JvmStatic external fun procProcess(p: Long, data: FloatArray, frames: Int): Int
    @JvmStatic external fun procLatencyMs(p: Long): Float
    /** 处理器此刻的状态：方位角、仰角、距离、说话活动、断流次数、是否出过错、是不是左右双人、2 号的方位角 / 仰角 / 距离 / 说话活动。 */
    @JvmStatic external fun procTele(p: Long): FloatArray
    /** 「输出效果 / 监听原声」（存进配置）。原声时处理器不进引擎，只乘输出增益、过峰值限制器。 */
    @JvmStatic external fun setDry(dry: Boolean)
    /** 0 = 贴耳加强，1 = KU100 实录，2 = 掏耳，3 = 呼吸音，4 = 听歌。 */
    @JvmStatic external fun setStyle(style: Int): Int
    /** 按钮：style_ear / style_ku100 / style_earclean / style_breath / style_music / reset / bg_rain_night / bg_off。 */
    @JvmStatic external fun action(name: String): Int
    /** 全部参数（JSON 对象，含虚拟字段和 dry）。 */
    @JvmStatic external fun getParamsJson(): String?
    /** 换参数（RON 格式的 Params，与配置文件里 params 那一段相同）。 */
    @JvmStatic external fun setParamsRon(ron: String): Int
    /** 导入配置文件（和电脑版同一格式的 .ron），整份换掉。 */
    @JvmStatic external fun loadConfig(text: String): Int
    @JvmStatic external fun lastError(): String?
    /** 输出节拍（rust/src/pace.rs，每路一个）：按输出流里的积压量微调速度，卡顿后追回实时。 */
    @JvmStatic external fun paceNew(): Long
    @JvmStatic external fun paceFree(p: Long)
    /** 输出流（重新）开始放：第一块是 [first] 帧时前面要先垫多少帧静音。 */
    @JvmStatic external fun paceBegin(p: Long, first: Int): Int
    @JvmStatic external fun paceStartFill(p: Long): Int
    /**
     * 送进 [data] 里的 [frames] 帧（处理好的交错立体声）：[fill] 是写之前输出流里还没放出去的帧数，[underruns] 是输出流累计的
     * 断流次数。要写进输出流的帧写到 [out]（至少比输入多 64 帧；再多 4800 帧，断流后才垫得下静音），返回帧数；0 = 这一块丢掉。
     * [p] 为 0 时原样拷过去。
     */
    @JvmStatic external fun pacePush(p: Long, data: FloatArray, frames: Int, fill: Int, underruns: Int, out: FloatArray): Int
    /** 目标（写之前至少剩多少，毫秒）、速度偏差（ppm）、丢掉旧声音的次数、这一层的延时（毫秒）。 */
    @JvmStatic external fun paceStats(p: Long): FloatArray
    @JvmStatic external fun rainNew(): Long
    @JvmStatic external fun rainFree(p: Long)
    @JvmStatic external fun rainRender(p: Long, data: FloatArray, frames: Int): Int
    @JvmStatic external fun rainSetOn(p: Long, on: Boolean)

    /**
     * 服务进程：加载 .so、从 APK 读数据文件、初始化引擎。返回出错原因，成功时 null。
     * [lib] 是 App 解压出来的 libasmrglobal.so；shell 身份映射不了时拷到 /data/local/tmp 再加载。
     */
    fun load(apk: String, lib: String?, log: (String) -> Unit): String? {
        val path = lib ?: (File(apk).parent!! + "/lib/arm64/libasmrglobal.so")
        try {
            System.load(path)
        } catch (e: UnsatisfiedLinkError) {
            log("warn 直接加载 $path 失败（${e.message}），拷到 /data/local/tmp 再试")
            val dir = File("/data/local/tmp/auto-asmr-global").apply { mkdirs() }
            val copy = File(dir, "libasmrglobal.so")
            try {
                File(path).copyTo(copy, overwrite = true)
                System.load(copy.path)
            } catch (e2: Throwable) {
                return "加载引擎失败：${e2.message}"
            }
        }
        val t0 = System.nanoTime()
        try {
            ZipFile(apk).use { z ->
                fun read(name: String): ByteArray =
                    z.getInputStream(z.getEntry(name) ?: error("APK 里没有 $name")).use { it.readBytes() }
                val r = provideData(read("assets/ku100.bin"), read("assets/pinna.bin"))
                if (r != 0) return "数据文件读不懂（$r）：${lastError()}"
            }
        } catch (e: Throwable) {
            return "读 APK 里的数据文件失败：${e.message}"
        }
        if (initEngine() != 0) return "引擎初始化失败：${lastError()}"
        log("log 引擎加载完 ${(System.nanoTime() - t0) / 1_000_000} ms")
        return null
    }
}
