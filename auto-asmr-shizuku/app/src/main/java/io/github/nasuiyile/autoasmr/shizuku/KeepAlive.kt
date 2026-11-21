package io.github.nasuiyile.autoasmr.shizuku

import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings as SysSettings

/**
 * 防止被杀后台的几样东西（界面上「后台运行」那张卡片）：
 * 1. 引擎在 Shizuku 的常驻进程里（见 [Controller]），App 进程被杀不影响声音——这是最主要的一条，不用设置。
 * 2. 悬浮球（[FloatingBall]）：App 有一个显示在最上层的窗口，系统把它当"用户看得见"的进程，很少回收；也方便随手切换。
 * 3. 忽略电池优化：系统的省电策略不再限制 App 在后台运行。
 * 4. 厂商的后台管理（荣耀 / 华为的「应用启动管理」、小米的「省电策略」等）：只能由用户在系统设置里改，这里给出入口和说明。
 */
object KeepAlive {
    private fun prefs(ctx: Context) = ctx.getSharedPreferences("ui", 0)

    fun ballWanted(ctx: Context) = prefs(ctx).getBoolean("ball", false)
    fun setBallWanted(ctx: Context, on: Boolean) = prefs(ctx).edit().putBoolean("ball", on).apply()

    fun canOverlay(ctx: Context) = SysSettings.canDrawOverlays(ctx)

    fun overlayIntent(ctx: Context) =
        Intent(SysSettings.ACTION_MANAGE_OVERLAY_PERMISSION, Uri.parse("package:${ctx.packageName}"))

    fun ignoringBattery(ctx: Context) =
        ctx.getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(ctx.packageName)

    @SuppressLint("BatteryLife")
    fun batteryIntent(ctx: Context) =
        Intent(SysSettings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${ctx.packageName}"))

    fun appDetailsIntent(ctx: Context) =
        Intent(SysSettings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${ctx.packageName}"))

    /** 这台手机的厂商后台管理怎么设（按厂商给说明）。 */
    fun vendorHint(): String = when (Build.MANUFACTURER.lowercase()) {
        "honor", "huawei" -> "荣耀 / 华为：设置 → 应用和服务 → 应用启动管理 → global-asmr，关掉「自动管理」，打开「允许自启动」「允许后台活动」。最近任务里下拉 global-asmr 的卡片可以加锁。"
        "xiaomi", "redmi", "poco" -> "小米：应用设置 → 省电策略改成「无限制」，打开「自启动」。最近任务里长按卡片可以锁定。"
        "oppo", "oneplus", "realme" -> "OPPO / 一加 / realme：应用信息 → 耗电管理，打开「允许后台运行」「允许自启动」。"
        "vivo", "iqoo" -> "vivo / iQOO：设置 → 电池 → 后台耗电管理 → 允许后台高耗电；i 管家里允许自启动。"
        "samsung" -> "三星：应用信息 → 电池 → 选「不受限制」。"
        else -> "在系统设置的应用信息里，允许后台运行、自启动，电池选「不受限制」。"
    }
}
