package io.github.nasuiyile.autoasmr.global

import android.os.IBinder
import android.os.Parcel
import android.os.RemoteException
import io.github.nasuiyile.autoasmr.global.server.EngineService
import java.io.InputStream
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/** 到服务进程的连接：发一条命令，等它的回复（命令和回复见 server/EngineCore.kt）。 */
interface Link {
    /** 返回回复；连接断了时返回 null。 */
    fun call(cmd: String): String?
    fun close()
}

/** Shizuku 的 UserService（server/EngineService.kt）：一次 binder 调用。 */
class BinderLink(private val binder: IBinder) : Link {
    override fun call(cmd: String): String? {
        val data = Parcel.obtain()
        val reply = Parcel.obtain()
        return try {
            data.writeInterfaceToken(EngineService.DESCRIPTOR)
            data.writeString(cmd)
            if (!binder.transact(EngineService.TX_CALL, data, reply, 0)) return null
            reply.readException()
            reply.readString()
        } catch (e: RemoteException) {
            null
        } catch (e: Exception) {
            "error ${e.javaClass.simpleName}: ${e.message}"
        } finally {
            data.recycle()
            reply.recycle()
        }
    }

    override fun close() {}
}

/**
 * su 起的服务进程（server/ServerMain.kt）：标准输入写 `#编号 命令`，标准输出读 `=编号 回复`。
 * 其它输出行（hello、log、stat……）交给 [onLine]。服务进程退出（管道断开）时调 [onDead]。
 */
class PipeLink(private val process: Process, private val onLine: (String) -> Unit, private val onDead: () -> Unit) : Link {
    private val nextId = AtomicInteger(1)
    private val waiting = ConcurrentHashMap<Int, ArrayBlockingQueue<String>>()
    @Volatile private var alive = true

    init {
        Thread({ read(process.inputStream, true) }, "server-stdout").start()
        Thread({ read(process.errorStream, false) }, "server-stderr").start()
    }

    private fun read(input: InputStream, isStdout: Boolean) {
        try {
            input.bufferedReader().forEachLine { line ->
                if (!isStdout) {
                    onLine("stderr: $line")
                } else if (line.startsWith("=")) {
                    val id = line.substring(1).substringBefore(' ').toIntOrNull()
                    val reply = unescape(line.substringAfter(' ', ""))
                    if (id != null) waiting[id]?.offer(reply)
                } else if (!line.startsWith("stat ")) {
                    onLine(line)
                }
            }
        } catch (_: Exception) {
        }
        if (!isStdout) return
        alive = false
        waiting.values.forEach { it.offer(DEAD) }
        onDead()
    }

    override fun call(cmd: String): String? {
        if (!alive) return null
        val id = nextId.getAndIncrement()
        val q = ArrayBlockingQueue<String>(1)
        waiting[id] = q
        try {
            synchronized(this) {
                process.outputStream.write("#$id $cmd\n".toByteArray())
                process.outputStream.flush()
            }
            val r = q.poll(10, TimeUnit.SECONDS) ?: return "error 服务进程 10 秒没回复"
            return if (r === DEAD) null else r
        } catch (e: Exception) {
            return null
        } finally {
            waiting.remove(id)
        }
    }

    override fun close() {
        runCatching { process.outputStream.close() }
        Thread({
            Thread.sleep(2000)
            runCatching { process.destroy() }
        }, "server-close").start()
    }

    private fun unescape(s: String): String {
        if ('\\' !in s) return s
        val b = StringBuilder(s.length)
        var i = 0
        while (i < s.length) {
            val c = s[i]
            if (c == '\\' && i + 1 < s.length) {
                b.append(if (s[i + 1] == 'n') '\n' else s[i + 1])
                i += 2
            } else {
                b.append(c)
                i++
            }
        }
        return b.toString()
    }

    companion object {
        private val DEAD = String(charArrayOf('\u0000'))
    }
}
