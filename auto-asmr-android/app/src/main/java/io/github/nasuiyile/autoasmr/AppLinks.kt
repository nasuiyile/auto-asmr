package io.github.nasuiyile.autoasmr

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log

/**
 * 项目的 GitHub 和研发人的 B 站主页（同电脑版右下角）。
 * B 站：先用 `bilibili://space/<uid>` 唤起 B 站 App（手机版、HD 版、国际版都认这个地址）直接进研发人的主页；
 * 没装 B 站 App 时退回网页版主页。
 */
object AppLinks {
    const val GITHUB_URL = "https://github.com/nasuiyile/auto-asmr"
    const val BILIBILI_UID = "79356601"
    const val BILIBILI_URL = "https://space.bilibili.com/$BILIBILI_UID"
    const val BILIBILI_APP_URI = "bilibili://space/$BILIBILI_UID"

    /** 打开研发人的 B 站主页：优先 B 站 App；没装时调 [fallback]（默认用系统浏览器打开网页版）。 */
    fun openBilibili(ctx: Context, fallback: (() -> Unit)? = null) {
        if (start(ctx, BILIBILI_APP_URI)) return
        Log.i(MainActivity.TAG, "没有能打开 bilibili:// 的 App，改开网页版主页")
        if (fallback != null) fallback() else start(ctx, BILIBILI_URL)
    }

    /** 用系统里能处理这个地址的 App（浏览器等）打开；没有时返回 false。 */
    fun start(ctx: Context, uri: String): Boolean = try {
        val i = Intent(Intent.ACTION_VIEW, Uri.parse(uri)).addCategory(Intent.CATEGORY_BROWSABLE)
        if (ctx !is android.app.Activity) i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        ctx.startActivity(i)
        true
    } catch (e: ActivityNotFoundException) {
        false
    }
}
