package io.github.nasuiyile.autoasmr

/** C 写的 hook 层（app/src/main/cpp/aaudio_hook.c）和它背后的 Rust 引擎（rust/src/lib.rs）。 */
object NativeHook {
    const val MODE_BYPASS = 0
    const val MODE_ASMR = 1
    const val MODE_SWAP = 2
    const val MODE_MUTE = 3

    /**
     * 登记 hook，返回失败的个数。要在第一个 WebView 创建之前调用。
     * [how]：0 = PLT hook（ByteHook，默认），1 = 只用 inline hook（ShadowHook，测试备用路径），2 = 两种都装。
     */
    @JvmStatic external fun install(how: Int): Int

    /** 自检发现截不到声音时：再装上备用的 inline hook（之后新开的流就能截到）。返回失败的个数。 */
    @JvmStatic external fun installInline(): Int

    /** [0] PLT hook 截到的调用方个数 [1] inline hook 装上的函数个数 [2] 最近 1 秒里还在回调的被截流个数。 */
    @JvmStatic external fun hookState(): IntArray

    /** 提供数据文件（APK 资源里的 ku100.bin、pinna.bin）。要在 [initEngine] 之前调用。返回 0 = 成功。 */
    @JvmStatic external fun provideData(ku100: ByteArray, pinna: ByteArray): Int

    /** 背景雨声：打开（自己的一条输出流）/ 关掉（淡出后停）。 */
    @JvmStatic external fun setRain(on: Boolean)

    /** 雨声此刻有没有在响。 */
    @JvmStatic external fun rainPlaying(): Boolean

    /** 加载 KU100 数据、生成默认配置（较慢，放后台线程）。返回 0 = 成功。 */
    @JvmStatic external fun initEngine(): Int

    /** 调试用：0 = 直通，1 = ASMR，2 = 左右互换，3 = 静音。界面上的开关用 [setDry]。 */
    @JvmStatic external fun setMode(mode: Int)

    /** 输出效果（false）/ 监听原声（true）：存进配置（和电脑版共用）。原声时不加效果，只乘输出增益、过峰值限制器。 */
    @JvmStatic external fun setDry(dry: Boolean)

    /** 风格按钮：0 = 贴耳加强，1 = KU100 实录，2 = 掏耳，3 = 呼吸音。 */
    @JvmStatic external fun setStyle(style: Int): Int

    /** 按钮：style_ear / style_ku100 / style_earclean / style_breath / reset / bg_rain_night / bg_off。返回 0 = 成功。 */
    @JvmStatic external fun action(name: String): Int

    /** 改一个参数：字段名或虚拟字段（ear_cm、manual_cm、moves_per_min、denoise_slider），值是 JSON。 */
    @JvmStatic external fun setField(key: String, valueJson: String): Int

    /** 全部参数（JSON 对象，含虚拟字段和 dry）。 */
    @JvmStatic external fun getParamsJson(): String?

    /** 界面用的参数表（JSON）。 */
    @JvmStatic external fun uiSpec(): String?

    /** 换参数（RON 格式的 Params，与电脑版配置文件里 params 那一段相同）。返回 0 = 成功。 */
    @JvmStatic external fun setParamsRon(ron: String): Int

    @JvmStatic external fun getParamsRon(): String?

    /** 导入配置文件（电脑版导出 / 自动保存的文件，或本程序导出的文件），整份换掉。返回 0 = 成功。 */
    @JvmStatic external fun loadConfig(text: String): Int

    /**
     * 只检查不导入：别的 App 用「其他应用打开」传来的文件是不是 auto-asmr 的配置。
     * 返回 "ok\n概要"（是）/ "not\n原因"（不是 auto-asmr 的配置）/ "bad\n原因"（读不懂）。不需要引擎加载完。
     */
    @JvmStatic external fun checkConfig(text: String): String

    /** 导出配置文件（和电脑版「导出配置」同一格式）；出错时 null。 */
    @JvmStatic external fun exportConfig(): String?

    @JvmStatic external fun lastError(): String?

    /** 引擎此刻的位置 → 手动摆位的 [方位角, 离头中心距离（米）, 高度（度）]。 */
    @JvmStatic external fun manualFromPose(az: Float, el: Float, r: Float): FloatArray

    /**
     * 最近在出声、建了处理器的那个流：[0] 有没有 [1] 方位角 [2] 仰角 [3] 距离（米） [4] 说话活动
     * [5] 处理占用（单核 %） [6] 延迟（ms） [7] 断流次数 [8] 采样率 [9] 流编号 [10] 引擎出过错
     * [11] 是不是左右双人 [12] 2 号的方位角 [13] 仰角 [14] 距离 [15] 说话活动（左右双人时 [1]–[4] 是 1 号）。
     */
    @JvmStatic external fun telemetry(): FloatArray

    /** 调试录音：下一个出声的流录 [seconds] 秒，处理前后各存一个 WAV 到 [dir]。返回 0 = 已开始。 */
    @JvmStatic external fun record(dir: String, seconds: Int): Int

    /**
     * 文件转换页（rust/src/files.rs）：一条命令（JSON，UTF-8 字节）进，整页状态（JSON，UTF-8 字节）出。
     * 命令和状态的字段见 files.rs；出错时返回 null。
     */
    @JvmStatic external fun files(cmd: ByteArray): ByteArray?

    /** 调试：已 hook 的调用方和每个流此刻的统计。 */
    @JvmStatic external fun status(): String
}
