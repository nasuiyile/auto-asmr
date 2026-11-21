package io.github.nasuiyile.autoasmr.shizuku.server

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import kotlin.math.max

/**
 * 背景雨声：服务进程自己的一条输出流（48 kHz 立体声），和安卓版一样由 Rust 的雨声渲染器生成。
 * 放在服务进程里：它的 UID 被拦截规则排除，雨声不会被自己截回来；App 被杀时也照常响。
 *
 * 关掉时先淡出，淡完渲染器说"没声音了"，线程自己停流退出。
 */
internal class RainPlayer(private val emit: (String) -> Unit) {
    companion object {
        const val SR = 48_000
        /** 每次渲染 20 ms。 */
        const val BLOCK = 960
    }

    private var thread: Thread? = null
    private var handle = 0L
    @Volatile private var want = false
    /** 此刻有没有声音（淡出完之前都算）。 */
    @Volatile var playing = false
        private set

    @Synchronized
    fun set(on: Boolean) {
        if (on == want && (on == (thread != null))) return
        want = on
        if (on) {
            if (thread != null) {
                Engine.rainSetOn(handle, true) // 淡出途中又打开
            } else {
                startLocked()
            }
        } else if (thread != null) {
            Engine.rainSetOn(handle, false) // 淡出，淡完线程自己结束
        }
    }

    private fun startLocked() {
        val h = Engine.rainNew()
        if (h == 0L) {
            emit("warn 雨声：数据还没加载")
            want = false
            return
        }
        handle = h
        Engine.rainSetOn(h, true)
        thread = Thread({ run(h) }, "asmr-rain").apply { start() }
    }

    private fun run(h: Long) {
        var track: AudioTrack? = null
        try {
            val fmt = AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
                .setSampleRate(SR)
                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                .build()
            val min = AudioTrack.getMinBufferSize(SR, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_FLOAT)
            val t = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build()
                )
                .setAudioFormat(fmt)
                .setBufferSizeInBytes(max(min, BLOCK * 2 * 4 * 4))
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_POWER_SAVING)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
            track = t
            val buf = FloatArray(BLOCK * 2)
            t.play()
            emit("log 雨声开始")
            while (true) {
                val r = Engine.rainRender(h, buf, BLOCK)
                playing = r == 1
                if (r == 0 && !want) break
                var off = 0
                while (off < buf.size) {
                    val w = t.write(buf, off, buf.size - off, AudioTrack.WRITE_BLOCKING)
                    if (w < 0) error("写雨声输出流出错（$w）")
                    off += w
                }
            }
            emit("log 雨声停了")
        } catch (e: Throwable) {
            emit("warn 雨声出错：${HiddenAudio.describe(e)}")
        } finally {
            track?.let { runCatching { it.stop() }; runCatching { it.release() } }
            playing = false
            synchronized(this) {
                Engine.rainFree(h)
                handle = 0L
                thread = null
                // 停流的同时又被打开了：重新开一条
                if (want) startLocked()
            }
        }
    }
}
