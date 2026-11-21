package io.github.nasuiyile.autoasmr.shizuku.server

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTimestamp
import android.media.AudioTrack
import kotlin.math.PI
import kotlin.math.sin

/**
 * 测延迟用的"别的 App"：每秒放一个 50 ms 的 1 kHz 短音，打印每个短音写进去的时刻和它真正从设备放出来的时刻
 * （都是 CLOCK_MONOTONIC，和服务进程的 onset 行可以直接相减）。
 *
 * 不拦截时 present − write = 正常播放的延迟；拦截时用服务进程报的 onset present − 这里的 write = 拦截后的延迟。
 * 要以和服务进程不同的身份运行（服务进程是 shell 时用 root 跑它，反之亦然），否则会被当成服务自己的声音排除掉。
 *
 * ```
 * CLASSPATH=<apk> app_process /system/bin io.github.nasuiyile.autoasmr.shizuku.server.LatencyProbe [短音个数]
 * ```
 */
object LatencyProbe {
    @JvmStatic
    fun main(args: Array<String>) {
        val sr = 48_000
        val clicks = args.firstOrNull()?.toIntOrNull() ?: 10
        val block = 480
        val t = AudioTrack.Builder()
            .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).build())
            .setAudioFormat(
                AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_FLOAT).setSampleRate(sr)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build()
            )
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
        t.play()
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
        System.out.flush()
        // AudioTrack 会留下非守护线程，main 返回后 app_process 不会自己退出
        kotlin.system.exitProcess(0)
    }
}
