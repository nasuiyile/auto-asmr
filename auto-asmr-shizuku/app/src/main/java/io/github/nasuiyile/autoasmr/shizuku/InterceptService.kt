package io.github.nasuiyile.autoasmr.shizuku

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.os.Handler
import android.os.IBinder
import android.os.Looper

/**
 * 前台服务：连着服务进程时一直开着。通知栏显示状态，可以暂停 / 继续拦截、切效果 / 原声、退出；悬浮球（[FloatingBall]）也挂在这里。
 *
 * 被系统杀掉后会被重新拉起（START_STICKY）：这时 App 进程是新的，[Controller] 收到 Shizuku 的 binder 后会连回还在跑的
 * 服务进程（引擎一直在处理声音，不受影响），通知和悬浮球随之恢复；10 秒内没连上就自己关掉。
 */
class InterceptService : Service() {
    private var ball: FloatingBall? = null
    private var last = ""
    private val main = Handler(Looper.getMainLooper())
    private val listener: () -> Unit = { main.post { update() } }
    private val giveUp = Runnable { if (!Controller.connected) stopSelf() }

    override fun onCreate() {
        super.onCreate()
        instance = this
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, "拦截", NotificationManager.IMPORTANCE_LOW).apply {
                description = "在处理其他 App 的声音时显示，可以暂停、切换原声"
                setShowBadge(false)
            },
        )
        Controller.addListener(listener)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACT_TOGGLE -> Controller.setIntercept(!Controller.poll.running)
            ACT_DRY -> Settings.setDry(!Settings.params.optBoolean("dry"))
            ACT_EXIT -> Controller.exit()
        }
        startForeground(ID, build(), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        if (!Controller.connected) {
            // 被系统重新拉起（intent 为空）或刚开始连：等一会儿
            main.removeCallbacks(giveUp)
            main.postDelayed(giveUp, 10_000)
        }
        refreshBall()
        return START_STICKY
    }

    /** 悬浮球：设置里打开了、而且有悬浮窗权限时显示。 */
    fun refreshBall() {
        val want = KeepAlive.ballWanted(this) && KeepAlive.canOverlay(this)
        if (want && ball == null) {
            ball = FloatingBall(
                this,
                onTap = {
                    if (Controller.connected) Settings.setDry(!Settings.params.optBoolean("dry"))
                    else openApp()
                },
                onLongPress = ::openApp,
            ).also { it.show() }
        } else if (!want && ball != null) {
            ball?.hide()
            ball = null
        }
        last = ""
        update()
    }

    private fun openApp() {
        startActivity(Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP))
    }

    private fun update() {
        val c = Controller
        if (c.connected) main.removeCallbacks(giveUp) // 连上了：不再等
        if (c.phase == Controller.Phase.IDLE && !main.hasCallbacks(giveUp)) {
            stopSelf()
            return
        }
        val dry = Settings.params.optBoolean("dry")
        val sounding = c.poll.tele.isNotEmpty() && c.poll.tele[0] > 0f
        ball?.update(
            when {
                !c.connected -> FloatingBall.Look.OFF
                !c.poll.running -> FloatingBall.Look.PAUSED
                dry -> FloatingBall.Look.DRY
                else -> FloatingBall.Look.EFFECT
            },
            sounding,
        )
        val key = "${c.phase}|${c.poll.running}|$dry|${c.how}"
        if (key == last) return
        last = key
        getSystemService(NotificationManager::class.java).notify(ID, build())
    }

    private fun build(): Notification {
        val c = Controller
        val dry = Settings.params.optBoolean("dry")
        val text = when {
            c.phase == Controller.Phase.STARTING -> "正在连接服务进程…"
            !c.connected -> "服务进程没连上"
            !c.poll.running -> "已暂停拦截（通过 ${c.how?.label}）"
            dry -> "拦截中 · 原声（通过 ${c.how?.label}）"
            else -> "拦截中 · ASMR 效果（通过 ${c.how?.label}）"
        }
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE)
        fun act(action: String, code: Int) = PendingIntent.getService(this, code, Intent(this, InterceptService::class.java).setAction(action), PendingIntent.FLAG_IMMUTABLE)
        val icon = Icon.createWithResource(this, R.drawable.ic_stat_asmr)
        val b = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_asmr)
            .setContentTitle("global-asmr")
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
        if (c.connected) {
            b.addAction(Notification.Action.Builder(icon, if (c.poll.running) "暂停拦截" else "继续拦截", act(ACT_TOGGLE, 1)).build())
            b.addAction(Notification.Action.Builder(icon, if (dry) "切到效果" else "切到原声", act(ACT_DRY, 2)).build())
            b.addAction(Notification.Action.Builder(icon, "退出", act(ACT_EXIT, 3)).build())
        }
        return b.build()
    }

    override fun onDestroy() {
        instance = null
        main.removeCallbacks(giveUp)
        Controller.removeListener(listener)
        ball?.hide()
        ball = null
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        const val CHANNEL = "intercept"
        const val ID = 1
        const val ACT_TOGGLE = "io.github.nasuiyile.autoasmr.shizuku.TOGGLE"
        const val ACT_DRY = "io.github.nasuiyile.autoasmr.shizuku.DRY"
        const val ACT_EXIT = "io.github.nasuiyile.autoasmr.shizuku.EXIT"
        @Volatile var instance: InterceptService? = null

        fun start(ctx: Context) {
            // App 在后台时系统可能不让开前台服务（Android 12 起）；有悬浮窗权限时不受这个限制
            runCatching { ctx.startForegroundService(Intent(ctx, InterceptService::class.java)) }
                .onFailure { Controller.addLog("# 前台服务开不起来：${it.javaClass.simpleName}") }
        }
    }
}
