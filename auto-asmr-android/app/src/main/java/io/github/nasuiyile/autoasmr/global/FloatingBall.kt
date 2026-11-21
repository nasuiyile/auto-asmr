package io.github.nasuiyile.autoasmr.global

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.PixelFormat
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.WindowManager
import kotlin.math.hypot

/**
 * 悬浮球（显示在其他 App 上面，要「显示在其他应用上层」的权限），由 [InterceptService] 管。
 * - 点一下：切换 ASMR 效果 / 原声。长按：打开 auto-asmr。拖动：移到别处，松手贴到屏幕左边或右边（位置会记住）。
 * - 颜色：紫 = 效果，灰 = 原声，浅灰 = 暂停拦截，红 = 服务进程没连上；外圈亮起表示此刻正在处理声音。
 * 有一个看得见的窗口时，系统把 App 进程当作"用户看得见的"，很少回收它。
 */
class FloatingBall(private val ctx: Context, private val onTap: () -> Unit, private val onLongPress: () -> Unit) {
    enum class Look(val color: Int, val label: String, val desc: String) {
        EFFECT(0xFF6750A4.toInt(), "效", "ASMR 效果"),
        DRY(0xFF5F5F5F.toInt(), "原", "原声"),
        PAUSED(0xFF9A9A9A.toInt(), "停", "暂停拦截"),
        OFF(0xFFB3261E.toInt(), "✕", "服务进程没连上"),
    }

    private val wm = ctx.getSystemService(WindowManager::class.java)
    private val prefs = ctx.getSharedPreferences("ui", 0)
    private val density = ctx.resources.displayMetrics.density
    private val size = (46 * density).toInt()
    private var view: BallView? = null
    private val lp = WindowManager.LayoutParams(
        size, size,
        WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
        WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
        PixelFormat.TRANSLUCENT,
    ).apply {
        gravity = Gravity.TOP or Gravity.START
        x = prefs.getInt("ball_x", 0)
        y = prefs.getInt("ball_y", (160 * density).toInt())
    }

    val showing: Boolean get() = view != null

    fun show() {
        if (view != null) return
        val v = BallView(ctx)
        v.setOnTouchListener(Touch(v))
        try {
            wm.addView(v, lp)
            view = v
        } catch (e: Exception) {
            // 没有悬浮窗权限等
            android.util.Log.w("AsmrBall", "addView", e)
        }
    }

    fun hide() {
        view?.let { runCatching { wm.removeView(it) } }
        view = null
    }

    fun update(look: Look, sounding: Boolean) {
        val v = view ?: return
        if (v.look == look && v.sounding == sounding) return
        v.look = look
        v.sounding = sounding
        v.contentDescription = "auto-asmr：${look.desc}"
        v.invalidate()
    }

    private inner class Touch(private val v: View) : View.OnTouchListener {
        private val slop = ViewConfiguration.get(ctx).scaledTouchSlop
        private val handler = Handler(Looper.getMainLooper())
        private var downX = 0f
        private var downY = 0f
        private var startX = 0
        private var startY = 0
        private var dragging = false
        private var longPressed = false
        private val long = Runnable {
            longPressed = true
            v.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            onLongPress()
        }

        @SuppressLint("ClickableViewAccessibility")
        override fun onTouch(view: View, e: MotionEvent): Boolean {
            when (e.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    downX = e.rawX
                    downY = e.rawY
                    startX = lp.x
                    startY = lp.y
                    dragging = false
                    longPressed = false
                    handler.postDelayed(long, ViewConfiguration.getLongPressTimeout().toLong())
                }
                MotionEvent.ACTION_MOVE -> {
                    val dx = e.rawX - downX
                    val dy = e.rawY - downY
                    if (!dragging && hypot(dx, dy) > slop) {
                        dragging = true
                        handler.removeCallbacks(long)
                    }
                    if (dragging) {
                        lp.x = startX + dx.toInt()
                        lp.y = startY + dy.toInt()
                        runCatching { wm.updateViewLayout(v, lp) }
                    }
                }
                MotionEvent.ACTION_UP -> {
                    handler.removeCallbacks(long)
                    if (dragging) {
                        // 贴边：离哪边近贴哪边
                        val bounds = wm.currentWindowMetrics.bounds
                        lp.x = if (lp.x + size / 2 < bounds.width() / 2) 0 else bounds.width() - size
                        lp.y = lp.y.coerceIn(0, (bounds.height() - size).coerceAtLeast(0))
                        runCatching { wm.updateViewLayout(v, lp) }
                        prefs.edit().putInt("ball_x", lp.x).putInt("ball_y", lp.y).apply()
                    } else if (!longPressed) {
                        v.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                        onTap()
                    }
                }
                MotionEvent.ACTION_CANCEL -> handler.removeCallbacks(long)
            }
            return true
        }
    }

    private class BallView(ctx: Context) : View(ctx) {
        var look = Look.OFF
        var sounding = false
        private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
        private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE }
        private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = 0xFFFFFFFF.toInt()
            textAlign = Paint.Align.CENTER
            isFakeBoldText = true
        }

        override fun onDraw(c: Canvas) {
            val r = width / 2f
            val d = resources.displayMetrics.density
            ring.strokeWidth = 3 * d
            fill.color = look.color
            fill.alpha = 225
            c.drawCircle(r, r, r - 3 * d, fill)
            ring.color = if (sounding && look == Look.EFFECT) 0xFFD0BCFF.toInt() else 0x66FFFFFF
            c.drawCircle(r, r, r - 1.5f * d, ring)
            text.textSize = r * 0.9f
            c.drawText(look.label, r, r - (text.descent() + text.ascent()) / 2, text)
        }
    }
}
