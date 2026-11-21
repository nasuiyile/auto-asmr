package io.github.nasuiyile.autoasmr.shizuku.server

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.AudioTrack
import android.os.HandlerThread
import android.os.Process
import kotlin.math.abs
import kotlin.math.max

/**
 * 拦截 → 处理 → 重新输出。
 *
 * 1. 先建自己的输出流（AudioTrack，用途 = 媒体，正常走耳机 / 扬声器）。
 * 2. 注册动态音频策略：用途是媒体 / 游戏 / 未知的播放，排除自己的 UID 和输出流的会话号，只回环（ROUTE_FLAG_LOOP_BACK）。
 * 3. 从策略建录音端并开始录：这一刻起，匹配到的 App 的声音改送进 remote submix，不再出现在耳机里。
 * 4. 音频线程：读一块（10 ms）→ 引擎处理（监听原声时不处理，同安卓版）→ 写进自己的输出流。
 *
 * 停止时反过来：先停录音（各 App 的声音自动回到原来的设备），再注销策略。
 * 进程被杀时系统发现策略的回调 binder 断了，也会自动注销，声音恢复原样。
 */
internal class AudioRouter(private val emit: (String) -> Unit) {
    companion object {
        const val SR = 48_000
        const val CH = 2
        /** 每块 10 ms。 */
        const val BLOCK = 480
        val USAGES = intArrayOf(AudioAttributes.USAGE_MEDIA, AudioAttributes.USAGE_GAME, AudioAttributes.USAGE_UNKNOWN)
        /** 连续这么多帧全零就暂停输出流（没有 App 在出声时不占着输出）。 */
        const val GATE_FRAMES = SR * 2L
        /** 输出缓冲的起始大小（毫秒）。 */
        const val START_OUT_MS = 30
    }

    private val policyThread = HandlerThread("asmr-policy").apply { start() }
    private var policy: Any? = null
    private var record: AudioRecord? = null
    private var track: AudioTrack? = null
    private var proc = 0L
    private var worker: Thread? = null
    @Volatile private var running = false
    /** 监听原声：不经过引擎，原样放出（和安卓版一样是直接切换）。 */
    @Volatile var dry = false
    /** 测延迟时打开（--onset）：每个声音的开头放出来时发一行 onset。 */
    @Volatile var reportOnsets = false

    // ---- 统计：音频线程写，统计线程读
    @Volatile var peakIn = 0f
    @Volatile var peakOut = 0f
    @Volatile var framesIn = 0L
    @Volatile var procNs = 0L
    @Volatile var gated = true
    @Volatile var lagInMs = Float.NaN
    @Volatile var lagOutMs = Float.NaN
    @Volatile var outBufMs = 0f
    @Volatile var readErrors = 0
    var engineLatencyMs = 0f
        private set
    val underruns: Int get() = runCatching { track?.underrunCount ?: 0 }.getOrDefault(0)
    val isRunning: Boolean get() = running

    /** 处理器此刻的状态（见 [Engine.procTele]）；没在拦截时为空。和 [stop] 互斥，处理器不会在读的时候被释放。 */
    @Synchronized
    fun tele(): FloatArray? = if (proc != 0L) Engine.procTele(proc) else null

    /** 开始拦截；返回出错原因，成功时 null。[onlyUids] 不空时只拦这些 App。 */
    @Synchronized
    fun start(excludeUids: IntArray, onlyUids: IntArray): String? {
        if (running) return null
        try {
            val outFmt = AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
                .setSampleRate(SR)
                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                .build()
            val minOut = AudioTrack.getMinBufferSize(SR, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_FLOAT)
            val t = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build()
                )
                .setAudioFormat(outFmt)
                .setBufferSizeInBytes(max(minOut, BLOCK * CH * 4 * 4))
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
            track = t
            if (t.state != AudioTrack.STATE_INITIALIZED) return fail("输出流初始化失败")

            val rule = HiddenAudio.buildRule(USAGES, excludeUids, onlyUids, intArrayOf(t.audioSessionId), emit)
            val mixFmt = AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SR)
                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                .build()
            val mix = HiddenAudio.loopbackMix(rule, mixFmt)
            val p = HiddenAudio.buildPolicy(ProcessEnv.context(), mix, policyThread.looper)
            val r = HiddenAudio.register(p)
            if (r != 0) return fail("registerAudioPolicy 返回 $r（这个身份没有 MODIFY_AUDIO_ROUTING？）")
            policy = p

            val rec = HiddenAudio.recordSink(p, mix) ?: return fail("createAudioRecordSink 返回空")
            record = rec
            if (rec.state != AudioRecord.STATE_INITIALIZED) return fail("录音端初始化失败")

            proc = Engine.procNew(SR, CH)
            if (proc == 0L) emit("warn 引擎处理器建不起来，只做直通")
            engineLatencyMs = if (proc != 0L) Engine.procLatencyMs(proc) else 0f

            resetStats()
            running = true
            HiddenAudio.startRecording(rec)?.let { emit("warn startRecording 出错（$it），已绕开厂商代码直接开始录音") }
            if (rec.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
                running = false
                return fail("录音端开不起来")
            }
            worker = Thread(::loop, "asmr-route").apply { start() }
            emit(
                "log 拦截开始：" +
                    (if (onlyUids.isNotEmpty()) "只拦 UID ${onlyUids.joinToString(",")}" else "排除 UID ${excludeUids.joinToString(",")}") +
                    "；输出会话 ${t.audioSessionId}，输出缓冲 ${t.bufferSizeInFrames} 帧，录音缓冲 ${rec.bufferSizeInFrames} 帧"
            )
            return null
        } catch (e: Throwable) {
            android.util.Log.e("AsmrShizuku", "start", e)
            return fail("${HiddenAudio.describe(e)}（${stackOf(e)}）")
        }
    }

    @Synchronized
    fun stop() {
        running = false
        record?.let { r -> runCatching { HiddenAudio.stopRecording(r) } } // 让阻塞在 read 里的音频线程返回
        worker?.join(2000)
        worker = null
        release()
    }

    private fun stackOf(e: Throwable): String =
        e.stackTrace.take(6).joinToString(" ← ") { "${it.className.substringAfterLast('.')}.${it.methodName}:${it.lineNumber}" }

    private fun fail(why: String): String {
        release()
        return why
    }

    private fun release() {
        running = false
        record?.let { r -> runCatching { HiddenAudio.stopRecording(r) }; runCatching { r.release() } }
        record = null
        policy?.let { p -> runCatching { HiddenAudio.unregister(p) }.onFailure { emit("warn 注销策略失败：${HiddenAudio.describe(it)}") } }
        policy = null
        track?.let { t -> runCatching { t.pause(); t.flush() }; runCatching { t.release() } }
        track = null
        if (proc != 0L) Engine.procFree(proc)
        proc = 0L
    }

    private fun resetStats() {
        peakIn = 0f; peakOut = 0f; framesIn = 0L; procNs = 0L; gated = true
        lagInMs = Float.NaN; lagOutMs = Float.NaN; outBufMs = 0f; readErrors = 0
    }

    /** 音频线程。出错时报出来（不然线程里没接住的异常会让整个服务进程崩掉，App 那边只看到进程退出）。 */
    private fun loop() {
        try {
            loopInner()
        } catch (e: Throwable) {
            android.util.Log.e("AsmrShizuku", "loop", e)
            if (running) emit("error 音频线程出错：${HiddenAudio.describe(e)}（${stackOf(e)}）")
        }
    }

    private fun loopInner() {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
        val rec = record ?: return
        val t = track ?: return
        val s16 = ShortArray(BLOCK * CH)
        val buf = FloatArray(BLOCK * CH)
        val tsIn = AudioTimestamp()
        val tsOut = AudioTimestamp()
        var silentFrames = GATE_FRAMES
        var playing = false
        var written = 0L
        var nextMeasure = 0L
        var lastUnderruns = t.underrunCount
        var quietRun = 0L
        var onsetFrame = -1L
        // 输出缓冲先压到 START_OUT_MS：写满就阻塞，所以这就是输出这一段的延迟上限；断流时再往上加
        t.bufferSizeInFrames = minOf(t.bufferCapacityInFrames, SR * START_OUT_MS / 1000)
        while (running) {
            val n = rec.read(s16, 0, s16.size, AudioRecord.READ_BLOCKING)
            if (n < 0) {
                readErrors++
                if (n == AudioRecord.ERROR_DEAD_OBJECT) {
                    emit("error 录音端失效（$n）")
                    break
                }
                continue
            }
            if (n == 0) continue
            val frames = n / CH
            var pk = 0f
            for (i in 0 until n) {
                val v = s16[i] * (1f / 32768f)
                buf[i] = v
                val a = abs(v)
                if (a > pk) pk = a
            }
            framesIn += frames
            if (pk > peakIn) peakIn = pk

            // 每 0.5 s 量一次两端的积压
            val now = System.nanoTime()
            if (now >= nextMeasure) {
                nextMeasure = now + 500_000_000L
                if (rec.getTimestamp(tsIn, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS) {
                    // 刚读到的最后一帧是什么时候录下的
                    val capturedAt = tsIn.nanoTime + (framesIn - tsIn.framePosition) * 1_000_000_000L / SR
                    lagInMs = (now - capturedAt) / 1e6f
                }
                if (playing && t.getTimestamp(tsOut)) {
                    val presented = tsOut.framePosition + (now - tsOut.nanoTime) * SR / 1_000_000_000L
                    lagOutMs = (written - presented) * 1000f / SR
                }
                // 断流了就把输出缓冲加大一块（每次 +5 ms，最多到建流时的容量）
                val u = t.underrunCount
                if (u > lastUnderruns) {
                    lastUnderruns = u
                    val want = minOf(t.bufferCapacityInFrames, t.bufferSizeInFrames + BLOCK / 2)
                    if (want > t.bufferSizeInFrames) t.bufferSizeInFrames = want
                }
                outBufMs = t.bufferSizeInFrames * 1000f / SR
            }

            silentFrames = if (pk == 0f) silentFrames + frames else 0
            if (silentFrames >= GATE_FRAMES) {
                if (playing) {
                    t.pause()
                    playing = false
                    gated = true
                }
                continue
            }
            if (!playing) {
                t.play()
                playing = true
                gated = false
            }
            // 测延迟（配合 LatencyProbe）：静了 ≥ 0.2 s 后第一个超过 0.01 的样本算一个声音的开头，
            // 记下它会落在输出流的第几帧，等输出流放到那一帧时报出放出来的时刻
            if (reportOnsets && onsetFrame < 0) {
                if (pk < 0.01f) {
                    quietRun += frames
                } else {
                    if (quietRun >= SR / 5) {
                        var i = 0
                        while (i < n && abs(buf[i]) < 0.01f) i++
                        onsetFrame = written + i / CH
                    }
                    quietRun = 0
                }
            }
            if (proc != 0L && !dry) {
                val t0 = System.nanoTime()
                Engine.procProcess(proc, buf, frames)
                procNs += System.nanoTime() - t0
            }
            var po = 0f
            for (i in 0 until n) {
                val a = abs(buf[i])
                if (a > po) po = a
            }
            if (po > peakOut) peakOut = po
            var off = 0
            while (off < n && running) {
                val w = t.write(buf, off, n - off, AudioTrack.WRITE_BLOCKING)
                if (w < 0) {
                    emit("warn 写输出流出错（$w）")
                    break
                }
                off += w
            }
            written += frames
            if (onsetFrame >= 0 && t.getTimestamp(tsOut) && tsOut.framePosition >= onsetFrame) {
                val presentNs = tsOut.nanoTime - (tsOut.framePosition - onsetFrame) * 1_000_000_000L / SR +
                    (engineLatencyMs * 1e6f).toLong()
                emit("onset present_ns=$presentNs")
                onsetFrame = -1
            }
        }
    }
}
