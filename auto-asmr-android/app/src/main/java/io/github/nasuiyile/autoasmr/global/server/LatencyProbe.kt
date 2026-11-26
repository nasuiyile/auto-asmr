package io.github.nasuiyile.autoasmr.global.server

import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTimestamp
import android.media.AudioTrack
import android.os.Binder
import android.os.IBinder
import android.os.Process
import kotlin.math.PI
import kotlin.math.sin

/**
 * 测延迟用的"别的 App"：每秒放一个 50 ms 的 1 kHz 短音，打印每个短音写进去的时刻和它真正从设备放出来的时刻
 * （都是 CLOCK_MONOTONIC，和服务进程的 onset 行可以直接相减）。
 *
 * 不拦截时 present − write = 正常播放的延迟；拦截时用服务进程报的 onset present − 这里的 write = 拦截后的延迟。
 * 要以和服务进程不同的身份运行（服务进程是 shell 时用 root 跑它，反之亦然），否则会被当成服务自己的声音排除掉。
 *
 * 短音个数之后的选项（可以组合）：
 * - `voice`：模拟语音通话类 App（QQ 通话、语音房）：先把音频模式设成 MODE_IN_COMMUNICATION，
 *   再用"语音通话"用途（USAGE_VOICE_COMMUNICATION）播放，用来验证全局模式截得到这类声音；
 * - `pref`：像有的语音 SDK 那样给播放流指定输出设备（扬声器），这种流不按拦截规则走（见 AudioRouter.watchBypass）；
 * - `lowlat`：低延迟模式。
 * 开始放 1.5 秒后打印播放流实际的去向（AudioDeviceInfo 的类型：25 = remote submix，也就是被截到了）。
 * 测单独截的那一路时，探针不要以 root 身份跑：root 进程的播放在系统的播放器列表里记成 UID 0，在音频策略里却是 1000，
 * 按 UID 写的规则匹配不上；可以用 `run-as <可调试的 App>` 以普通 App 的 UID 跑。
 *
 * ```
 * CLASSPATH=<apk> app_process /system/bin io.github.nasuiyile.autoasmr.global.server.LatencyProbe [短音个数] [voice] [pref] [lowlat]
 * ```
 */
object LatencyProbe {
    @JvmStatic
    fun main(args: Array<String>) {
        val sr = 48_000
        val clicks = args.firstOrNull()?.toIntOrNull() ?: 10
        val opts = args.drop(1).toSet()
        val voice = "voice" in opts
        val block = 480
        // 通话模式由持有这个 token 的进程占着，进程退出时系统自动恢复
        val modeToken = if (voice) setCommunicationMode() else null
        val attrs = if (voice) {
            AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION).setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build()
        } else {
            AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).build()
        }
        val t = AudioTrack.Builder()
            .setAudioAttributes(attrs)
            .setAudioFormat(
                AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_FLOAT).setSampleRate(sr)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build()
            )
            .setTransferMode(AudioTrack.MODE_STREAM)
            .apply { if ("lowlat" in opts) setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY) }
            .build()
        // pref：像有的语音 SDK 那样给播放流指定输出设备（这里指定扬声器）
        if ("pref" in opts) {
            val outs = AudioManager::class.java.getMethod("getDevicesStatic", Int::class.javaPrimitiveType)
                .invoke(null, AudioManager.GET_DEVICES_OUTPUTS) as Array<AudioDeviceInfo>
            val spk = outs.firstOrNull { it.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER }
            println("setPreferredDevice(${spk?.type}) = ${spk?.let { t.setPreferredDevice(it) }}")
        }
        t.play()
        Thread {
            Thread.sleep(1500)
            println("routed device type=${t.routedDevice?.type}")
        }.apply { isDaemon = true; start() }
        val buf = FloatArray(block * 2)
        val ts = AudioTimestamp()
        val clickFrames = LongArray(clicks) { (it + 1L) * sr }
        val writeNs = LongArray(clicks)
        val presentNs = LongArray(clicks)
        val clickLen = sr / 20
        val total = (clicks + 1L) * sr + sr / 2
        var frame = 0L
        while (frame < total) {
            var clickHere = -1
            for (i in 0 until block) {
                val f = frame + i
                val k = (f / sr - 1).toInt()
                val inClick = k in 0 until clicks && f - clickFrames[k] in 0 until clickLen
                if (inClick && f == clickFrames[k]) clickHere = k
                val v = if (inClick) 0.3f * sin(2 * PI * 1000 * (f - clickFrames[k]) / sr).toFloat() else 0f
                buf[2 * i] = v
                buf[2 * i + 1] = v
            }
            t.write(buf, 0, buf.size, AudioTrack.WRITE_BLOCKING)
            // 写进去（被输出流收下）的时刻；短音都从一块的第一帧开始
            if (clickHere >= 0) writeNs[clickHere] = System.nanoTime()
            frame += block
            if (t.getTimestamp(ts)) {
                for (k in 0 until clicks) {
                    if (presentNs[k] == 0L && writeNs[k] != 0L && ts.framePosition >= clickFrames[k]) {
                        presentNs[k] = ts.nanoTime - (ts.framePosition - clickFrames[k]) * 1_000_000_000L / sr
                    }
                }
            }
        }
        Thread.sleep(500)
        t.stop()
        t.release()
        for (k in 0 until clicks) println("click $k write_ns=${writeNs[k]} present_ns=${presentNs[k]}")
        if (modeToken != null) println("usage=VOICE_COMMUNICATION")
        System.out.flush()
        // AudioTrack 会留下非守护线程，main 返回后 app_process 不会自己退出
        kotlin.system.exitProcess(0)
    }

    /** 直接调 AudioService.setMode(MODE_IN_COMMUNICATION)（app_process 里没有 App 的 Context），返回占着模式的 token。 */
    private fun setCommunicationMode(): IBinder? = runCatching {
        val b = Class.forName("android.os.ServiceManager").getMethod("getService", String::class.java).invoke(null, "audio") as IBinder
        val svc = Class.forName("android.media.IAudioService\$Stub").getMethod("asInterface", IBinder::class.java).invoke(null, b)!!
        val token = Binder()
        val pkg = if (Process.myUid() == 0) "root" else "com.android.shell"
        val m = svc.javaClass.methods.first { it.name == "setMode" }
        when (m.parameterTypes.size) {
            3 -> m.invoke(svc, AudioManager.MODE_IN_COMMUNICATION, token, pkg)
            else -> m.invoke(svc, AudioManager.MODE_IN_COMMUNICATION, token)
        }
        println("mode=${svc.javaClass.getMethod("getMode").invoke(svc)}（3 = MODE_IN_COMMUNICATION）")
        token
    }.onFailure { println("setMode 失败：$it") }.getOrNull()
}
