package io.github.nasuiyile.autoasmr.shizuku

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.nasuiyile.autoasmr.shizuku.server.Engine
import java.io.ByteArrayOutputStream

/**
 * 别的 App（QQ、微信、文件管理器……）里对 `.ron` 文件选「用其他应用打开 → global-asmr」时传进来的文件（和安卓版相同）。
 *
 * 读出内容后先只检查、不导入（和电脑版同一份解析代码）：是 auto-asmr 的配置就弹窗显示文件名和概要，
 * 用户点「导入」才导入（等同于菜单里的「导入配置」）；不是就提示。
 */
object SharedImport {
    private const val TAG = "AsmrImport"
    /** 配置文件都只有几 KB；再大的肯定不是。 */
    private const val MAX_BYTES = 1 shl 20
    private const val HINT = "请用电脑版或安卓版「导出配置」得到的 .ron 文件。"

    /** 传进来的文件：[ok] = 是 auto-asmr 的配置（[info] 是概要），否则 [info] 是原因。 */
    data class Incoming(val name: String, val text: String?, val ok: Boolean, val info: String)

    /** 等用户确认的文件（界面上弹窗）。 */
    var pending by mutableStateOf<Incoming?>(null)

    /** 是不是「打开文件」的 intent。 */
    fun isOpenFile(intent: Intent?): Boolean =
        intent?.action == Intent.ACTION_VIEW && intent.data?.scheme.let { it == "content" || it == "file" }

    /** 在后台线程里读文件、检查，然后弹窗。 */
    fun handle(context: Context, intent: Intent) {
        val uri = intent.data ?: return
        val app = context.applicationContext
        Thread({
            val name = displayName(app, uri)
            val result = runCatching { read(app, uri) }.fold(
                onSuccess = { text ->
                    if (text == null) {
                        Incoming(name, null, false, "文件太大，不是 auto-asmr 的配置。$HINT")
                    } else {
                        val r = Engine.checkConfig(text)
                        val (code, info) = r.substringBefore('\n') to r.substringAfter('\n')
                        if (code != "ok") Log.i(TAG, "$name 不是配置：$info")
                        when (code) {
                            "ok" -> Incoming(name, text, true, info)
                            // 比如电脑版只保存了窗口状态、没有设置的 auto-asmr.ron
                            "not" -> Incoming(name, null, false, "文件里没有 auto-asmr 的设置（可能只保存了电脑版的窗口状态）。$HINT")
                            else -> Incoming(name, null, false, "内容不是 auto-asmr 的配置。$HINT")
                        }
                    }
                },
                onFailure = {
                    Log.w(TAG, "读不了 $uri", it)
                    Incoming(name, null, false, "读不了这个文件（发送文件的 App 没有给读取权限，或者文件已经不在了）。可以先把文件保存到手机里，再用菜单里的「导入配置」。")
                },
            )
            Log.i(TAG, "收到文件 $uri（$name）：${if (result.ok) "是配置" else "不是配置"}，${result.info}")
            Handler(Looper.getMainLooper()).post { pending = result }
        }, "asmr-import").start()
    }

    private fun displayName(context: Context, uri: Uri): String {
        val fromProvider = runCatching {
            context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
                if (c.moveToFirst()) c.getString(0) else null
            }
        }.getOrNull()
        return fromProvider ?: uri.lastPathSegment?.substringAfterLast('/') ?: "未命名文件"
    }

    /** 读成文本（UTF-8，去掉 BOM）；超过 [MAX_BYTES] 时返回 null。 */
    private fun read(context: Context, uri: Uri): String? {
        val out = ByteArrayOutputStream()
        context.contentResolver.openInputStream(uri)?.use { input ->
            val buf = ByteArray(8192)
            while (true) {
                val n = input.read(buf)
                if (n < 0) break
                out.write(buf, 0, n)
                if (out.size() > MAX_BYTES) return null
            }
        } ?: error("打不开")
        return out.toString(Charsets.UTF_8.name()).removePrefix("﻿")
    }

    /** 用户点了「导入」。 */
    fun confirm() {
        val p = pending ?: return
        pending = null
        val text = p.text ?: return
        Settings.importText(text) { ok -> Log.i(TAG, "导入 ${p.name}：${if (ok) "成功" else "失败"}") }
    }
}
