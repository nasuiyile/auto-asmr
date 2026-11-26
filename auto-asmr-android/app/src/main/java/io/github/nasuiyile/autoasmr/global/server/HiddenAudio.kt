package io.github.nasuiyile.autoasmr.global.server

import android.content.AttributionSource
import android.content.Context
import android.content.ContextWrapper
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.os.Looper
import android.os.Process
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Method

/**
 * 系统的动态音频策略接口（android.media.audiopolicy.*，@SystemApi，公开 SDK 里没有），全部用反射调用。
 *
 * 用法：一个 AudioMix（规则：匹配哪些用途 / 哪些 App，排除哪些 UID）+ 路由方式 ROUTE_FLAG_LOOP_BACK
 * （只送进虚拟输出 remote submix，不再送到扬声器 / 耳机），注册成 AudioPolicy，再从它建一个录音端读出来。
 * 注册需要 MODIFY_AUDIO_ROUTING：shell（adb / Shizuku）和 root 身份都有，普通 App 没有。
 */
internal object HiddenAudio {
    private val clsRule = Class.forName("android.media.audiopolicy.AudioMixingRule")
    private val clsRuleBuilder = Class.forName("android.media.audiopolicy.AudioMixingRule\$Builder")
    private val clsMix = Class.forName("android.media.audiopolicy.AudioMix")
    private val clsMixBuilder = Class.forName("android.media.audiopolicy.AudioMix\$Builder")
    private val clsPolicy = Class.forName("android.media.audiopolicy.AudioPolicy")
    private val clsPolicyBuilder = Class.forName("android.media.audiopolicy.AudioPolicy\$Builder")

    private fun ruleConst(name: String): Int? = runCatching { clsRule.getField(name).getInt(null) }.getOrNull()

    /**
     * 规则：用途是 [usages] 之一的播放；[onlyUids] 不空时只要这些 App，否则排除 [excludeUids]；
     * 另外按会话号排除 [excludeSessions]（系统支持时，Android 13 起）。
     */
    fun buildRule(usages: IntArray, excludeUids: IntArray, onlyUids: IntArray, excludeSessions: IntArray, log: (String) -> Unit): Any {
        val b = clsRuleBuilder.getConstructor().newInstance()
        val add = clsRuleBuilder.getMethod("addMixRule", Int::class.javaPrimitiveType, Any::class.java)
        val exclude = clsRuleBuilder.getMethod("excludeMixRule", Int::class.javaPrimitiveType, Any::class.java)
        val usage = ruleConst("RULE_MATCH_ATTRIBUTE_USAGE") ?: error("没有 RULE_MATCH_ATTRIBUTE_USAGE")
        val uid = ruleConst("RULE_MATCH_UID") ?: error("没有 RULE_MATCH_UID")
        for (u in usages) call { add.invoke(b, usage, AudioAttributes.Builder().setUsage(u).build()) }
        if (onlyUids.isNotEmpty()) {
            for (x in onlyUids) call { add.invoke(b, uid, x) }
        } else {
            for (x in excludeUids) call { exclude.invoke(b, uid, x) }
        }
        val session = ruleConst("RULE_MATCH_AUDIO_SESSION_ID")
        if (session != null) {
            for (s in excludeSessions) {
                try {
                    call { exclude.invoke(b, session, s) }
                } catch (e: Throwable) {
                    log("warn 按会话号排除不支持：${describe(e)}")
                }
            }
        }
        return call { clsRuleBuilder.getMethod("build").invoke(b) }!!
    }

    /**
     * 二级混音（录屏内录那种）的规则：只要 [uid] 这个 App、用途是 [usages] 之一的播放（不同类的条件要同时满足）。
     * 用途里有语音通话时要打开 voiceCommunicationCaptureAllowed（需要 CAPTURE_VOICE_COMMUNICATION_OUTPUT，shell / root 有）。
     */
    fun buildTeeRule(uid: Int, usages: IntArray): Any {
        val b = clsRuleBuilder.getConstructor().newInstance()
        val add = clsRuleBuilder.getMethod("addMixRule", Int::class.javaPrimitiveType, Any::class.java)
        val usage = ruleConst("RULE_MATCH_ATTRIBUTE_USAGE") ?: error("没有 RULE_MATCH_ATTRIBUTE_USAGE")
        val uidRule = ruleConst("RULE_MATCH_UID") ?: error("没有 RULE_MATCH_UID")
        for (u in usages) call { add.invoke(b, usage, AudioAttributes.Builder().setUsage(u).build()) }
        call { add.invoke(b, uidRule, uid) }
        if (AudioAttributes.USAGE_VOICE_COMMUNICATION in usages) {
            call { clsRuleBuilder.getMethod("voiceCommunicationCaptureAllowed", Boolean::class.javaPrimitiveType).invoke(b, true) }
        }
        return call { clsRuleBuilder.getMethod("build").invoke(b) }!!
    }

    /**
     * 播放端混音。[format] 是送进 remote submix（也就是录音端读出来）的格式。
     * 平时只回环（ROUTE_FLAG_LOOP_BACK）：声音改送进 remote submix，不再送到设备。
     * [render] = 回环 + 照常放（ROUTE_FLAG_LOOP_BACK_RENDER，二级混音）：声音照常送到原来的设备，另外复制一份
     * （复制的是 App 写进来的原始数据，在音量之前）。App 给播放流指定了输出设备时只有这种截得到。
     */
    fun loopbackMix(rule: Any, format: AudioFormat, render: Boolean = false): Any {
        val b = clsMixBuilder.getConstructor(clsRule).newInstance(rule)
        call { clsMixBuilder.getMethod("setFormat", AudioFormat::class.java).invoke(b, format) }
        val flag = clsMix.getField(if (render) "ROUTE_FLAG_LOOP_BACK_RENDER" else "ROUTE_FLAG_LOOP_BACK").getInt(null)
        call { clsMixBuilder.getMethod("setRouteFlags", Int::class.javaPrimitiveType).invoke(b, flag) }
        return call { clsMixBuilder.getMethod("build").invoke(b) }!!
    }

    /** 一个正在放声音的播放器（系统的 AudioPlaybackConfiguration）。[deviceType] 是它此刻的去向（AudioDeviceInfo.TYPE_*）。 */
    class Player(val piid: Int, val uid: Int, val usage: Int, val started: Boolean, val deviceType: Int?, val proxy: Any?)

    /** AudioPlaybackConfiguration 的系统接口（用到时才找：老系统上缺哪个，只影响 [players]，不影响拦截本身）。 */
    private class Apc {
        val cls = Class.forName("android.media.AudioPlaybackConfiguration")
        val uid: Method = cls.getMethod("getClientUid")
        val state: Method = cls.getMethod("getPlayerState")
        val piid: Method = cls.getMethod("getPlayerInterfaceId")
        /** Android 11 起才有；没有时去向一律不知道。 */
        val device: Method? = runCatching { cls.getMethod("getAudioDeviceInfo") }.getOrNull()
        val proxy: Method = cls.getMethod("getPlayerProxy")
        val started = cls.getField("PLAYER_STATE_STARTED").getInt(null)
        val am = newAudioManager()
    }
    private val apc by lazy { Apc() }

    /**
     * 各 App 此刻的播放器。有 MODIFY_AUDIO_ROUTING 的身份（shell / root）拿到的是完整信息：UID、去向、播放器代理；
     * 别的身份只有匿名的，这里就读不到 UID。
     */
    fun players(): List<Player> {
        val a = apc
        return a.am.activePlaybackConfigurations.map { c ->
            Player(
                piid = a.piid.invoke(c) as Int,
                uid = a.uid.invoke(c) as Int,
                usage = c.audioAttributes.usage,
                started = a.state.invoke(c) as Int == a.started,
                deviceType = runCatching { (a.device?.invoke(c) as android.media.AudioDeviceInfo?)?.type }.getOrNull(),
                proxy = runCatching { a.proxy.invoke(c) }.getOrNull(),
            )
        }
    }

    /**
     * 设播放器的音量系数（PlayerProxy.setVolume；系统通话时静音别的 App 用的也是它）。乘在 App 自己设的音量上，
     * 作用在混音时，二级混音复制的那份不受影响。播放器已经没了时抛出异常。
     */
    fun setPlayerVolume(proxy: Any, vol: Float) {
        call { proxy.javaClass.getMethod("setVolume", Float::class.javaPrimitiveType).invoke(proxy, vol) }
    }

    /** UID 对应的包名（几个 App 共用一个 UID 时是系统给的名字）；查不到时为空。 */
    fun packageOf(uid: Int): String? = runCatching { ProcessEnv.context().packageManager?.getNameForUid(uid) }.getOrNull()

    /** 一个策略里可以放几个混音（全局模式按用途分两路，见 [AudioRouter]），各有各的 remote submix 和录音端。 */
    fun buildPolicy(context: Context, mixes: List<Any>, looper: Looper): Any {
        val b = clsPolicyBuilder.getConstructor(Context::class.java).newInstance(context)
        val add = clsPolicyBuilder.getMethod("addMix", clsMix)
        for (mix in mixes) call { add.invoke(b, mix) }
        call { clsPolicyBuilder.getMethod("setLooper", Looper::class.java).invoke(b, looper) }
        return call { clsPolicyBuilder.getMethod("build").invoke(b) }!!
    }

    /** 返回 0 = 成功（AudioManager.SUCCESS），其它 = 失败。 */
    fun register(policy: Any): Int {
        val static = findMethod(AudioManager::class.java, "registerAudioPolicyStatic", clsPolicy)
        if (static != null) return call { static.invoke(null, policy) } as Int
        val m = AudioManager::class.java.getMethod("registerAudioPolicy", clsPolicy)
        return call { m.invoke(newAudioManager(), policy) } as Int
    }

    fun unregister(policy: Any) {
        val static = findMethod(AudioManager::class.java, "unregisterAudioPolicyAsyncStatic", clsPolicy)
        if (static != null) {
            call { static.invoke(null, policy) }
            return
        }
        val m = AudioManager::class.java.getMethod("unregisterAudioPolicy", clsPolicy)
        call { m.invoke(newAudioManager(), policy) }
    }

    /** 读混音结果的录音端（格式和 mix 的格式一样）。 */
    fun recordSink(policy: Any, mix: Any): AudioRecord? =
        call { clsPolicy.getMethod("createAudioRecordSink", clsMix).invoke(policy, mix) } as AudioRecord?

    /**
     * 开始录音。先走正常的 startRecording()；厂商在里面加的代码出错时（例如荣耀 Android 17 上的
     * NullPointerException），改为自己做 AOSP 那几步：native_start → 固定音量（handleFullVolumeRec）→ 记录状态。
     * 返回 null = 走的正常路径，否则是绕开的原因。
     */
    fun startRecording(rec: AudioRecord): String? {
        try {
            rec.startRecording()
            return null
        } catch (e: RuntimeException) {
            if (e is IllegalStateException && rec.state != AudioRecord.STATE_INITIALIZED) throw e
            val why = describe(e)
            val c = AudioRecord::class.java
            // 厂商代码可能在 native_start 之后才出错，这时已经在录；再调一次 native_start 什么也不做，返回成功
            val r = c.getDeclaredMethod("native_start", Int::class.javaPrimitiveType, Int::class.javaPrimitiveType)
                .apply { isAccessible = true }.invoke(rec, 0, 0) as Int
            if (r != 0) throw IllegalStateException("startRecording 出错（$why），直接调 native_start 也失败（$r）")
            runCatching { c.getDeclaredMethod("handleFullVolumeRec", Boolean::class.javaPrimitiveType).apply { isAccessible = true }.invoke(rec, true) }
            c.getDeclaredField("mRecordingState").apply { isAccessible = true }.setInt(rec, AudioRecord.RECORDSTATE_RECORDING)
            return why
        }
    }

    /** 停止录音；stop() 里的厂商代码出错时同样绕开。 */
    fun stopRecording(rec: AudioRecord) {
        try {
            rec.stop()
        } catch (e: RuntimeException) {
            val c = AudioRecord::class.java
            runCatching { c.getDeclaredMethod("handleFullVolumeRec", Boolean::class.javaPrimitiveType).apply { isAccessible = true }.invoke(rec, false) }
            runCatching { c.getDeclaredMethod("native_stop").apply { isAccessible = true }.invoke(rec) }
            runCatching { c.getDeclaredField("mRecordingState").apply { isAccessible = true }.setInt(rec, AudioRecord.RECORDSTATE_STOPPED) }
        }
    }

    /**
     * 本进程会话号为 [session] 的播放流所在的输出线程有几个声道（从 dumpsys media.audio_flinger 里找）；
     * 找不到时为空。用来发现系统把输出流放到了单声道的输出上（有的手机通话类声音走单声道输出）。
     */
    fun outputChannels(session: Int): Int? {
        val text = runCatching {
            val p = ProcessBuilder("/system/bin/dumpsys", "media.audio_flinger").redirectErrorStream(true).start()
            p.inputStream.bufferedReader().use { it.readText() }.also { p.waitFor() }
        }.getOrNull() ?: return null
        val pid = Process.myPid()
        // 播放流那一行：[类型] 编号 yes/no 进程号[/UID] 会话号 …（各版本的列大体一样）
        val row = Regex("""^\s+\S*\s*\d+\s+(?:yes|no)\s+(\d+)(?:/\s*\d+)?\s+(\d+)\s""")
        val count = Regex("""^\s+Channel count:\s*(\d+)""")
        var inOutput = false
        var channels: Int? = null
        for (line in text.lineSequence()) {
            if (line.startsWith("Output thread")) {
                inOutput = true
                channels = null
                continue
            }
            // 录音线程、以"- "开头的（已经关掉的线程）不看；线程中间也有不缩进的说明行，不能拿它当结尾
            if (line.startsWith("Input thread") || line.startsWith("Mmap") || line.startsWith("- ") || line.startsWith("##")) {
                inOutput = false
                continue
            }
            if (!inOutput) continue
            val c = count.find(line)
            if (c != null) {
                if (channels == null) channels = c.groupValues[1].toInt()
                continue
            }
            val m = row.find(line) ?: continue
            if (m.groupValues[1].toInt() == pid && m.groupValues[2].toInt() == session) return channels
        }
        return null
    }

    private fun findMethod(c: Class<*>, name: String, vararg p: Class<*>): Method? =
        runCatching { c.getDeclaredMethod(name, *p).apply { isAccessible = true } }.getOrNull()

    private fun newAudioManager(): AudioManager =
        AudioManager::class.java.getDeclaredConstructor().apply { isAccessible = true }.newInstance()

    /** 反射调用：把 InvocationTargetException 拆开，直接抛真正的异常。 */
    private inline fun <T> call(block: () -> T): T =
        try {
            block()
        } catch (e: InvocationTargetException) {
            throw e.targetException ?: e
        }

    fun describe(e: Throwable): String {
        val t = (e as? InvocationTargetException)?.targetException ?: e
        return "${t.javaClass.simpleName}: ${t.message}"
    }
}

/**
 * 服务进程用的 Context。服务进程是 app_process 起的，没有 App 的 Context。
 * 底是系统 Context（[ProcessEnv] 取到时），身份（包名、AttributionSource）换成 shell 或 root；
 * 权限检查直接答"有"，真正的检查在系统服务那边。AudioPolicy 拿它查 MODIFY_AUDIO_ROUTING 和取身份，
 * 进程的 Application 也以它为底（见 [ProcessEnv]），厂商代码通过 ActivityThread.currentApplication() 拿到的就是它。
 */
internal class ShellContext(base: Context?) : ContextWrapper(base) {
    private val hasBase = base != null
    private val pkg = ProcessEnv.packageName
    override fun getPackageName(): String = pkg
    override fun getOpPackageName(): String = pkg
    /** 隐藏方法（SDK 里没有，不能写 override），运行时照样覆盖 ContextWrapper 的同名方法。 */
    @Suppress("unused") fun getBasePackageName(): String = pkg
    override fun getApplicationContext(): Context = this
    override fun getMainLooper(): Looper = Looper.getMainLooper() ?: Looper.myLooper() ?: error("没有 Looper")
    override fun checkCallingOrSelfPermission(permission: String): Int = PackageManager.PERMISSION_GRANTED
    override fun checkSelfPermission(permission: String): Int = PackageManager.PERMISSION_GRANTED
    override fun checkPermission(permission: String, pid: Int, uid: Int): Int = PackageManager.PERMISSION_GRANTED
    override fun getSystemService(name: String): Any? =
        if (hasBase) runCatching { super.getSystemService(name) }.getOrNull() else null
    // AudioPolicy.Builder.addMix 从这里取设备号（虚拟设备用）
    override fun getAttributionSource(): AttributionSource =
        AttributionSource.Builder(Process.myUid()).setPackageName(pkg).build()
    override fun getDeviceId(): Int = 0
}
