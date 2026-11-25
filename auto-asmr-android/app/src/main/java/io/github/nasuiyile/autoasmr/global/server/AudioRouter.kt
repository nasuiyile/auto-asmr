package io.github.nasuiyile.autoasmr.global.server

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.AudioTrack
import android.os.Build
import android.os.HandlerThread
import android.os.Process
import android.os.SystemClock
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min

/**
 * 拦截 → 处理 → 重新输出。分两路，各按原来的音量和去向放，和没拦截时一样：
 * - 媒体一路：媒体 / 游戏 / 未知用途的播放 → 媒体用途的输出流（跟媒体音量）；
 * - 通话一路：语音通话用途的播放（QQ / 微信通话、语音房里对方的声音）→ 语音通话用途的输出流
 *   （跟通话音量，去向和通话一样：听筒 / 免提 / 耳机 / 蓝牙）。
 *
 * 为什么分两路：截到的是调音量之前的声音，放出来时按输出流的用途决定用哪个音量、走哪个设备。合成一路只能挑一种，
 * 另一种声音的音量就不对了（例如通话时按音量键调不了对方的声音，或者调通话音量时音乐也跟着变）。
 *
 * 1. 先建两条输出流（拿到会话号）。
 * 2. 注册动态音频策略，里面两个混音（一路一个），按用途分；都排除自己的 UID 和两条输出流的会话号，只回环（ROUTE_FLAG_LOOP_BACK）。
 * 3. 从策略给每个混音建录音端并开始录：这一刻起，匹配到的 App 的声音改送进 remote submix，不再出现在耳机里。
 * 4. 每路一个音频线程：录音端来多少取多少 → 引擎处理（每路一个处理器，参数共用；监听原声时不处理，同安卓版）→
 *    输出节拍（rust/src/pace.rs）→ 写进这一路的输出流。连续 2 秒全是静音就暂停这一路的输出流，所以没有通话时通话一路不占输出。
 *
 * 延迟：截到的声音一阵一阵地来（remote submix 每次交出约 21 ms），输出流按声卡的时钟取走，中间攒着的就是我们多出来的延迟。
 * 输出流里只留"够撑到下一阵"的量：开始放时垫到目标量，之后每次写之前看还剩多少，交给输出节拍微调速度（最多 ±0.5%），
 * 卡顿后多出来的会追回去，断流才多留，之后再减回来。输出缓冲的上限只是保险，平时碰不到。
 *
 * 通话一路的输出是浮点立体声，和通话专用的直通输出（VOIP_RX，常是 16 位单声道）格式对不上，系统会退回普通混音输出，
 * 立体声保得住。开始放时查一次它所在输出的声道数，万一是单声道就换成媒体用途（立体声保住，代价是音量跟媒体走）。
 *
 * 停止时反过来：先停录音（各 App 的声音自动回到原来的设备），再注销策略。
 * 进程被杀时系统发现策略的回调 binder 断了，也会自动注销，声音恢复原样。
 */
internal class AudioRouter(private val emit: (String) -> Unit) {
    companion object {
        const val SR = 48_000
        const val CH = 2
        /** 一次最多处理多少帧（和 rust/src/pace.rs 的 MAX_FRAMES 一样）。 */
        const val MAX_FRAMES = 4096
        /** 输出节拍交出来的最多比输入多多少帧：调速多出的（rust/src/pace.rs 的 OUT_PAD）+ 断流后垫的静音（PAD_MAX）。 */
        const val OUT_PAD = 64 + SR / 10
        /** 录音端没有数据时先等这么多帧（2 ms），等到了把已经到的一起取走。 */
        const val WAIT_FRAMES = 96
        /** 媒体一路截的用途（闹钟、通知、铃声、导航语音、系统提示音不截）。 */
        val MEDIA_USAGES = intArrayOf(AudioAttributes.USAGE_MEDIA, AudioAttributes.USAGE_GAME, AudioAttributes.USAGE_UNKNOWN)
        /** 通话一路截的用途。 */
        val VOICE_USAGES = intArrayOf(AudioAttributes.USAGE_VOICE_COMMUNICATION)
        /** 连续这么多帧全零就暂停输出流（没有 App 在出声时不占着输出）。 */
        const val GATE_FRAMES = SR * 2L
        /** 输出流的容量（毫秒）：平时只用其中一小段，卡顿后多出来的声音先放在里面再追回去。 */
        const val CAPACITY_MS = 400
        /** 放起来之后输出缓冲的上限 = 开始放时攒的量 + 这么多（毫秒）。只是保险：多出 150 ms 输出节拍就会丢掉旧的。 */
        const val HEADROOM_MS = 250
        /** 输出节拍建不起来时，开始放之前攒这么多（毫秒）。 */
        const val FALLBACK_START_MS = 30
        /** 在放、输出缓冲里也有声音，播放位置却这么久没动（毫秒）：输出流卡住了，清空重新开始。 */
        const val STUCK_MS = 500L
        /** 通话一路两次查声道数至少隔这么久（毫秒）。 */
        const val CHECK_GAP_MS = 30_000L
    }

    private val policyThread = HandlerThread("asmr-policy").apply { start() }
    private var policy: Any? = null
    /** [0] 媒体一路，[1] 通话一路；没在拦截时为空。 */
    @Volatile private var lanes: List<Lane> = emptyList()
    @Volatile private var running = false
    /** 测延迟时打开（--onset）：每个声音的开头放出来时发一行 onset。 */
    @Volatile var reportOnsets = false

    // ---- 统计：两路合起来（音频线程写，统计线程读）
    var peakIn: Float
        get() = lanes.maxOfOrNull { it.peakIn } ?: 0f
        set(v) = lanes.forEach { it.peakIn = v }
    var peakOut: Float
        get() = lanes.maxOfOrNull { it.peakOut } ?: 0f
        set(v) = lanes.forEach { it.peakOut = v }
    val framesIn: Long get() = lanes.maxOfOrNull { it.framesIn } ?: 0L
    val procNs: Long get() = lanes.sumOf { it.procNs }
    val gated: Boolean get() = lanes.all { it.gated }
    /** 录音端读到的数据离被录下过了多久（毫秒；系统给得出时间戳时含 remote submix 管道里的那一段）。 */
    val lagInMs: Float get() = shown()?.lagInMs ?: Float.NaN
    /** 输出流里攒着还没被取走的量（毫秒，半秒里的平均）：我们自己留的缓冲。 */
    val lagBufMs: Float get() = shown()?.lagBufMs ?: Float.NaN
    /** 被取走之后到真正放出来还要多久（毫秒）：混音线程、声卡 / 蓝牙耳机本身的延迟，不拦截时也有。 */
    val lagDevMs: Float get() = shown()?.lagDevMs ?: Float.NaN
    /** 输出节拍的状态：目标（写之前至少剩多少，毫秒）、速度偏差（ppm）、丢掉旧声音的次数（两路加起来）。 */
    val targetMs: Float get() = shown()?.targetMs ?: 0f
    val trimPpm: Float get() = shown()?.trimPpm ?: 0f
    val drops: Int get() = lanes.sumOf { it.drops }
    val readErrors: Int get() = lanes.sumOf { it.readErrors }
    val underruns: Int get() = lanes.sumOf { it.underruns }
    /** 引擎算法的延迟 + 输出节拍的插值延时（毫秒）。 */
    var engineLatencyMs = 0f
        private set
    var paceLatencyMs = 0f
        private set
    val isRunning: Boolean get() = running

    /** 界面上显示哪一路：在出声的那路（两路都在出声时显示通话一路），都没出声时显示媒体一路。 */
    private fun shown(): Lane? {
        val ls = lanes
        return ls.lastOrNull { !it.gated } ?: ls.firstOrNull()
    }

    /** 处理器此刻的状态（见 [Engine.procTele]，取 [shown] 那一路）；没在拦截时为空。和 [stop] 互斥，处理器不会在读的时候被释放。 */
    @Synchronized
    fun tele(): FloatArray? {
        val p = shown()?.proc ?: 0L
        return if (p != 0L) Engine.procTele(p) else null
    }

    /** 开始拦截；返回出错原因，成功时 null。[onlyUids] 不空时只拦这些 App。 */
    @Synchronized
    fun start(excludeUids: IntArray, onlyUids: IntArray): String? {
        if (running) return null
        try {
            val ls = listOf(Lane("媒体", false, MEDIA_USAGES), Lane("通话", true, VOICE_USAGES))
            lanes = ls
            for (l in ls) {
                val t = buildTrack(l.voice, 0) ?: return fail("${l.name}一路的输出流初始化失败")
                l.track = t
                l.session = t.audioSessionId
            }
            val sessions = ls.map { it.session }.toIntArray()
            val mixFmt = AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SR)
                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                .build()
            for (l in ls) {
                val rule = HiddenAudio.buildRule(l.usages, excludeUids, onlyUids, sessions, emit)
                l.mix = HiddenAudio.loopbackMix(rule, mixFmt)
            }
            val p = HiddenAudio.buildPolicy(ProcessEnv.context(), ls.map { it.mix!! }, policyThread.looper)
            val r = HiddenAudio.register(p)
            if (r != 0) return fail("registerAudioPolicy 返回 $r（这个身份没有 MODIFY_AUDIO_ROUTING？）")
            policy = p

            for (l in ls) {
                val rec = HiddenAudio.recordSink(p, l.mix!!) ?: return fail("createAudioRecordSink 返回空")
                l.record = rec
                if (rec.state != AudioRecord.STATE_INITIALIZED) return fail("${l.name}一路的录音端初始化失败")
                l.proc = Engine.procNew(SR, CH)
                if (l.proc == 0L) emit("warn ${l.name}一路的引擎处理器建不起来，只做直通")
                l.pace = Engine.paceNew()
                if (l.pace == 0L) emit("warn ${l.name}一路的输出节拍建不起来，按固定的量缓冲")
            }
            engineLatencyMs = ls[0].proc.let { if (it != 0L) Engine.procLatencyMs(it) else 0f }
            paceLatencyMs = ls[0].pace.let { if (it != 0L) Engine.paceStats(it)[3] else 0f }

            running = true
            for (l in ls) {
                val rec = l.record!!
                HiddenAudio.startRecording(rec)?.let { emit("warn startRecording 出错（$it），已绕开厂商代码直接开始录音") }
                if (rec.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
                    running = false
                    return fail("${l.name}一路的录音端开不起来")
                }
            }
            for (l in ls) l.worker = Thread(l::loop, if (l.voice) "asmr-route-voice" else "asmr-route").apply { start() }
            emit(
                "log 拦截开始：" +
                    (if (onlyUids.isNotEmpty()) "只拦 UID ${onlyUids.joinToString(",")}" else "排除 UID ${excludeUids.joinToString(",")}") +
                    "；分两路：" + ls.joinToString("，") { "${it.name}（输出会话 ${it.session}）" } +
                    "；输出流容量 ${ls[0].track?.bufferCapacityInFrames} 帧，录音缓冲 ${ls[0].record?.bufferSizeInFrames} 帧"
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
        val ls = lanes
        for (l in ls) l.record?.let { r -> runCatching { HiddenAudio.stopRecording(r) } } // 让阻塞在 read 里的音频线程返回
        for (l in ls) {
            l.worker?.join(2000)
            l.worker = null
        }
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
        val ls = lanes
        for (l in ls) {
            l.record?.let { r -> runCatching { HiddenAudio.stopRecording(r) }; runCatching { r.release() } }
            l.record = null
        }
        policy?.let { p -> runCatching { HiddenAudio.unregister(p) }.onFailure { emit("warn 注销策略失败：${HiddenAudio.describe(it)}") } }
        policy = null
        for (l in ls) {
            l.track?.let { t -> runCatching { t.pause(); t.flush() }; runCatching { t.release() } }
            l.track = null
            if (l.proc != 0L) Engine.procFree(l.proc)
            l.proc = 0L
            if (l.pace != 0L) Engine.paceFree(l.pace)
            l.pace = 0L
        }
        lanes = emptyList()
    }

    /**
     * 建输出流。[voice] = 语音通话用途（音量跟通话音量，去向和通话一样），否则媒体用途。
     * [session] 不为 0 时沿用这个会话号（策略按它排除自己的声音，换流时不能变）。建不起来时为空。
     */
    private fun buildTrack(voice: Boolean, session: Int): AudioTrack? {
        val fmt = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
            .setSampleRate(SR)
            .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
            .build()
        val attrs = if (voice) {
            AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                .build()
        } else {
            AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build()
        }
        val minOut = AudioTrack.getMinBufferSize(SR, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_FLOAT)
        val b = AudioTrack.Builder()
            .setAudioAttributes(attrs)
            .setAudioFormat(fmt)
            .setBufferSizeInBytes(max(minOut, SR * CAPACITY_MS / 1000 * CH * 4))
            .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
            .setTransferMode(AudioTrack.MODE_STREAM)
        if (session != 0) b.setSessionId(session)
        val t = b.build()
        if (t.state != AudioTrack.STATE_INITIALIZED) {
            t.release()
            return null
        }
        return t
    }

    /** 一路：一个混音 → 录音端 → 处理器 → 输出流。 */
    private inner class Lane(val name: String, val voice: Boolean, val usages: IntArray) {
        var mix: Any? = null
        var record: AudioRecord? = null
        @Volatile var track: AudioTrack? = null
        var session = 0
        @Volatile var proc = 0L
        /** 输出节拍（rust/src/pace.rs）；建不起来时为 0，按固定的量缓冲。 */
        @Volatile var pace = 0L
        var worker: Thread? = null
        /** 输出流现在是不是语音通话用途；[wantVoice] 和它不一样时音频线程换流（通话一路在单声道输出上时换成媒体用途）。 */
        @Volatile var outVoice = voice
        @Volatile var wantVoice = voice
        private var lastCheck = -CHECK_GAP_MS
        private var lastChannels: Int? = null

        @Volatile var peakIn = 0f
        @Volatile var peakOut = 0f
        @Volatile var framesIn = 0L
        @Volatile var procNs = 0L
        @Volatile var gated = true
        @Volatile var lagInMs = Float.NaN
        @Volatile var lagBufMs = Float.NaN
        @Volatile var lagDevMs = Float.NaN
        @Volatile var targetMs = 0f
        @Volatile var trimPpm = 0f
        @Volatile var drops = 0
        @Volatile var readErrors = 0
        val underruns: Int get() = runCatching { track?.underrunCount ?: 0 }.getOrDefault(0)

        /** 音频线程。出错时报出来（不然线程里没接住的异常会让整个服务进程崩掉，App 那边只看到进程退出）。 */
        fun loop() {
            try {
                loopInner()
            } catch (e: Throwable) {
                android.util.Log.e("AsmrShizuku", "loop $name", e)
                if (running) emit("error ${name}一路的音频线程出错：${HiddenAudio.describe(e)}（${stackOf(e)}）")
            }
        }

        /**
         * 通话一路开始放时，在另一个线程里查一次它所在输出的声道数（读 dumpsys 要几十毫秒，不放在音频线程上做；
         * 等 0.5 s 让系统把路由定下来）。单声道就让音频线程换成媒体用途。
         */
        private fun checkChannels() {
            val now = SystemClock.elapsedRealtime()
            if (now - lastCheck < CHECK_GAP_MS) return
            lastCheck = now
            Thread({
                try {
                    Thread.sleep(500)
                } catch (e: InterruptedException) {
                    return@Thread
                }
                if (!running || !outVoice) return@Thread
                val ch = HiddenAudio.outputChannels(session)
                if (ch == 1) {
                    wantVoice = false
                    emit("warn 这台手机把语音通话用途的声音放在单声道输出上，通话一路改用媒体用途（处理后的通话声音跟媒体音量走）")
                } else if (ch != lastChannels) {
                    emit("log 通话一路的输出：${ch?.let { "$it 声道" } ?: "声道数查不到"}")
                }
                lastChannels = ch
            }, "asmr-check").apply { isDaemon = true; start() }
        }

        /**
         * 换输出流的用途（音频线程上调用）。同一个会话号里的播放流必须是同一类用途（AudioFlinger 会拒绝，
         * 日志 `mismatched strategy`），所以先放掉旧的再建新的；正在放的旧流要等混音线程下一轮才撤下来，
         * 建不起来时隔 10 ms 再试，最多试 0.3 s。还是建不起来返回空。
         */
        private fun swapTrack(old: AudioTrack, toVoice: Boolean): AudioTrack? {
            runCatching { old.pause(); old.flush() }
            runCatching { old.release() }
            var err: Throwable? = null
            for (i in 0 until 30) {
                val t = try {
                    buildTrack(toVoice, session)
                } catch (e: Throwable) {
                    err = e
                    null
                }
                if (t != null) {
                    track = t
                    outVoice = toVoice
                    emit("log ${name}一路的输出改用${if (toVoice) "语音通话" else "媒体"}用途")
                    return t
                }
                Thread.sleep(10)
            }
            track = null
            emit("error ${name}一路的输出流建不起来" + (err?.let { "：${HiddenAudio.describe(it)}" } ?: ""))
            return null
        }

        // ---- 输出流里的位置（只在音频线程上用）
        /** 这条输出流（上次清空之后）一共写进去多少帧。 */
        private var written = 0L
        private var headLast = 0L
        private var headWraps = 0L

        /** 输出流已经被混音线程取走多少帧（系统给的是 32 位，自己接上回绕）。 */
        private fun headOf(t: AudioTrack): Long {
            val raw = t.playbackHeadPosition.toLong() and 0xFFFFFFFFL
            if (raw < headLast && headLast - raw > (1L shl 31)) headWraps++
            headLast = raw
            return (headWraps shl 32) + raw
        }

        /** 把 [a] 里的 [frames] 帧写进输出流（写满时等）；出错返回 false。 */
        private fun writeAll(t: AudioTrack, a: FloatArray, frames: Int): Boolean {
            var off = 0
            val n = frames * CH
            while (off < n && running) {
                val w = t.write(a, off, n - off, AudioTrack.WRITE_BLOCKING)
                if (w < 0) {
                    emit("warn ${name}一路写输出流出错（$w）")
                    return false
                }
                if (w == 0) return false // 输出流没在放（被别处停掉了）
                off += w
            }
            return true
        }

        private fun loopInner() {
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
            val rec = record ?: return
            var t = track ?: return
            val s16 = ShortArray(MAX_FRAMES * CH)
            val buf = FloatArray(MAX_FRAMES * CH)
            val out = FloatArray((MAX_FRAMES + OUT_PAD) * CH)
            val zeros = FloatArray(MAX_FRAMES * CH)
            val tsIn = AudioTimestamp()
            val tsOut = AudioTimestamp()
            var silentFrames = GATE_FRAMES
            var playing = false
            /** 放起来之后（播放位置动了）才把输出缓冲的上限放开；开始放时攒了多少帧。 */
            var opened = false
            var startFill = 0
            var headMovedAt = 0L
            var headSeen = 0L
            var nextMeasure = 0L
            var quietRun = 0L
            var onsetFrame = -1L
            var fillSum = 0L
            var fillCount = 0
            while (running) {
                // 来多少取多少：没有就等 2 ms 的量，等到了把已经到的一起取走。不按固定的块攒，声音不在录音缓冲里多等，
                // 音频线程卡了一下之后也一次读完，不留积压
                var n = rec.read(s16, 0, s16.size, AudioRecord.READ_NON_BLOCKING)
                if (n == 0) {
                    n = rec.read(s16, 0, WAIT_FRAMES * CH, AudioRecord.READ_BLOCKING)
                    if (n > 0) {
                        val more = rec.read(s16, n, s16.size - n, AudioRecord.READ_NON_BLOCKING)
                        if (more > 0) n += more
                    }
                }
                if (n < 0) {
                    readErrors++
                    if (n == AudioRecord.ERROR_DEAD_OBJECT) {
                        emit("error ${name}一路的录音端失效（$n）")
                        break
                    }
                    continue
                }
                val frames = n / CH
                if (frames == 0) continue
                n = frames * CH
                if (wantVoice != outVoice) {
                    t = swapTrack(t, wantVoice) ?: break
                    playing = false
                    onsetFrame = -1
                }
                var pk = 0f
                for (i in 0 until n) {
                    val v = s16[i] * (1f / 32768f)
                    buf[i] = v
                    val a = abs(v)
                    if (a > pk) pk = a
                }
                framesIn += frames
                if (pk > peakIn) peakIn = pk

                // 每 0.5 s 量一次：录音端这边读到的数据有多旧
                val now = System.nanoTime()
                val measure = now >= nextMeasure
                if (measure) {
                    nextMeasure = now + 500_000_000L
                    if (rec.getTimestamp(tsIn, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS) {
                        // 刚读到的最后一帧是什么时候录下的
                        val capturedAt = tsIn.nanoTime + (framesIn - tsIn.framePosition) * 1_000_000_000L / SR
                        lagInMs = (now - capturedAt) / 1e6f
                    }
                }

                silentFrames = if (pk == 0f) silentFrames + frames else 0
                if (silentFrames >= GATE_FRAMES) {
                    if (playing) {
                        // 里面只剩静音：清空，下次出声时从头按目标量攒
                        runCatching { t.pause(); t.flush() }
                        playing = false
                        gated = true
                    }
                    continue
                }
                // 测延迟（配合 LatencyProbe）：静了 ≥ 0.2 s 后第一个超过 0.01 的样本算一个声音的开头，
                // 记下它会落在输出流的第几帧，等输出流放到那一帧时报出放出来的时刻
                var onsetIdx = -1
                if (reportOnsets && onsetFrame < 0) {
                    if (pk < 0.01f) {
                        quietRun += frames
                    } else {
                        if (quietRun >= SR / 5) {
                            var i = 0
                            while (i < n && abs(buf[i]) < 0.01f) i++
                            onsetIdx = i / CH
                        }
                        quietRun = 0
                    }
                }
                // 原声时也交给处理器：Rust 那边不进引擎，只乘输出增益、过峰值限制器
                if (proc != 0L) {
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

                val under = runCatching { t.underrunCount }.getOrDefault(0)
                val m: Int
                if (!playing) {
                    // （重新）开始放：先垫静音，让这一块写进去之后正好是目标量。输出流要写满"输出缓冲"才开始放，
                    // 所以先把输出缓冲压到正好这么多，等它放起来再放开上限
                    var prefill = if (pace != 0L) Engine.paceBegin(pace, frames) else max(0, SR * FALLBACK_START_MS / 1000 - frames)
                    m = Engine.pacePush(pace, buf, frames, prefill, under, out)
                    val got = t.setBufferSizeInFrames(min(t.bufferCapacityInFrames, prefill + m))
                    if (got > prefill + m) prefill = got - m
                    startFill = prefill + m
                    if (Build.VERSION.SDK_INT >= 31) runCatching { t.setStartThresholdInFrames(max(1, startFill)) }
                    t.play()
                    playing = true
                    gated = false
                    opened = false
                    written = 0
                    headLast = 0
                    headWraps = 0
                    headSeen = 0
                    headMovedAt = now
                    if (outVoice) checkChannels()
                    var left = prefill
                    while (left > 0 && running) {
                        val k = min(left, MAX_FRAMES)
                        if (!writeAll(t, zeros, k)) break
                        left -= k
                    }
                    written = prefill.toLong()
                    fillSum += prefill + m / 2
                } else {
                    val head = headOf(t)
                    if (head != headSeen) {
                        headSeen = head
                        headMovedAt = now
                        if (!opened) {
                            opened = true
                            t.setBufferSizeInFrames(min(t.bufferCapacityInFrames, startFill + SR * HEADROOM_MS / 1000))
                        }
                    }
                    if (written < head) written = head // 输出流被系统重建过（换了输出设备）：里面的声音没了，从这里接着算
                    val fill = min(written - head, Int.MAX_VALUE.toLong()).toInt()
                    if (fill >= startFill && now - headMovedAt > STUCK_MS * 1_000_000L) {
                        // 在放、里面也有声音，播放位置却不动：清空重新开始（这一块不要了）
                        emit("warn ${name}一路的输出流卡住了，重新开始")
                        runCatching { t.pause(); t.flush() }
                        playing = false
                        continue
                    }
                    m = Engine.pacePush(pace, buf, frames, fill, under, out)
                    fillSum += fill + m / 2
                }
                fillCount++
                val first = written
                if (m > 0) {
                    writeAll(t, out, m)
                    written += m
                }
                if (onsetIdx >= 0) onsetFrame = first + onsetIdx.toLong() * m / frames

                // 每 0.5 s：输出这边攒了多少、设备本身晚多少、输出节拍的状态
                if (measure) {
                    if (fillCount > 0) lagBufMs = fillSum * 1000f / SR / fillCount
                    fillSum = 0
                    fillCount = 0
                    if (t.getTimestamp(tsOut)) {
                        // 已经被取走、还没真正放出来的帧数（两个位置都按 32 位回绕，差值取低 32 位）
                        val presented = tsOut.framePosition + (System.nanoTime() - tsOut.nanoTime) * SR / 1_000_000_000L
                        lagDevMs = max(0, (headOf(t) - presented).toInt()) * 1000f / SR
                    }
                    if (pace != 0L) {
                        val st = Engine.paceStats(pace)
                        targetMs = st[0]
                        trimPpm = st[1]
                        drops = st[2].toInt()
                    }
                }
                if (onsetFrame >= 0 && t.getTimestamp(tsOut) && tsOut.framePosition >= onsetFrame) {
                    val presentNs = tsOut.nanoTime - (tsOut.framePosition - onsetFrame) * 1_000_000_000L / SR +
                        (engineLatencyMs * 1e6f).toLong()
                    emit("onset present_ns=$presentNs")
                    onsetFrame = -1
                }
            }
        }
    }
}
