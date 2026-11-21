package io.github.nasuiyile.autoasmr.shizuku.server

import java.io.BufferedReader
import java.io.FileDescriptor
import java.io.FileOutputStream
import java.io.InputStreamReader
import java.io.PrintStream
import java.util.concurrent.LinkedBlockingQueue
import kotlin.system.exitProcess

/**
 * 服务进程的标准输入 / 输出入口：用 app_process 直接起，root 的「用 root 启动」（su）和 adb 调试用它。
 * Shizuku 走的是 [EngineService]（常驻），命令和回复两边一样（见 [EngineCore]）。
 *
 * ```
 * CLASSPATH=<apk> app_process /system/bin io.github.nasuiyile.autoasmr.shizuku.server.ServerMain \
 *     --apk <apk> [--lib <libasmrshizuku.so>] [--client-uid <App 的 UID>] [--start] [--seconds N] [--onset]
 * ```
 *
 * - 标准输入一行一条命令。App 发的带编号：`#12 poll`，回复写成一行 `=12 <回复>`（回复里的换行写成 `\n`、反斜杠写成 `\\`）；
 *   不带编号（手敲调试）时直接把回复写出来。
 * - 标准输出另外还有：`hello …`、`log …`、`warn …`、`error …`、`state …`、拦截中每秒一行 `stat …`、`--onset` 时的 `onset …`。
 * - 标准输入关掉（App 退出、被杀）时停止拦截并退出，各 App 的声音恢复原样。
 */
object ServerMain {
    private const val EOF = "\u0000eof"
    private val outQueue = LinkedBlockingQueue<String>(1024)

    /** 写一行：放进队列由单独的线程写标准输出，读的一方卡住时不影响音频线程。 */
    private fun write(line: String) {
        outQueue.offer(line)
    }

    @JvmStatic
    fun main(args: Array<String>) {
        val opts = args.toList()
        fun opt(name: String): String? = opts.indexOf(name).takeIf { it >= 0 && it + 1 < opts.size }?.let { opts[it + 1] }
        val apk = opt("--apk") ?: System.getProperty("java.class.path")?.split(':')?.firstOrNull()
        val seconds = opt("--seconds")?.toLongOrNull()

        val writer = Thread({
            val out = PrintStream(FileOutputStream(FileDescriptor.out), true, "UTF-8")
            while (true) {
                val l = outQueue.take()
                if (l === EOF) break
                out.println(l)
            }
            out.flush()
        }, "asmr-stdout").apply { isDaemon = true; start() }

        if (apk == null) quit(2, "fatal 不知道 APK 路径（--apk）", writer)
        EngineCore.init(apk!!, opt("--lib"), opt("--client-uid")?.toIntOrNull(), ::write, reportOnsets = "--onset" in opts)
        if ("--start" in opts) EngineCore.call("start")

        if (seconds != null) {
            // 调试：不看标准输入，跑 N 秒后退出
            Thread.sleep(seconds * 1000)
        } else {
            val input = BufferedReader(InputStreamReader(System.`in`, Charsets.UTF_8))
            while (true) {
                val line = try {
                    input.readLine()
                } catch (e: Exception) {
                    null
                }?.trim() ?: break
                if (line.isEmpty()) continue
                // 带编号的请求：#<编号> <命令>
                val id = if (line.startsWith("#")) line.substring(1).substringBefore(' ').takeIf { s -> s.isNotEmpty() && s.all { it.isDigit() } } else null
                val cmd = if (id != null) line.substringAfter(' ', "") else line
                if (cmd == "exit" || cmd == "quit") {
                    if (id != null) write("=$id bye")
                    break
                }
                val reply = EngineCore.call(cmd)
                if (id != null) write("=$id " + reply.replace("\\", "\\\\").replace("\n", "\\n")) else write(reply)
            }
        }
        EngineCore.shutdown()
        quit(0, "bye", writer)
    }

    private fun quit(code: Int, msg: String, writer: Thread): Nothing {
        write(msg)
        outQueue.offer(EOF)
        writer.join(1000)
        exitProcess(code)
    }
}
