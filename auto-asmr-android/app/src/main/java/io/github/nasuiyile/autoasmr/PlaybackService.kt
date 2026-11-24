package io.github.nasuiyile.autoasmr

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.media.MediaMetadata
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Bundle
import android.os.IBinder

/**
 * 前台服务（媒体播放类型）：App 开着时通知栏里一直有 auto-asmr，随时可以**启用 / 关闭效果**（什么时候开、关由 [Playback] 决定）。
 *
 * - **网页在出声**（或刚停、用户刚暂停）：带媒体会话的播放通知，通知栏和锁屏上可以暂停 / 继续、启用 / 关闭效果，
 *   耳机按键也能暂停 / 继续（MediaSession）。Android 13 起通知栏顶部的媒体控件只认媒体会话里的按钮，
 *   所以「启用 / 关闭效果」同时作为会话的自定义按钮（[PlaybackState.CustomAction]）放进去。
 * - **没有声音**：普通的常驻通知「auto-asmr 正在运行」，可以启用 / 关闭效果、退出（关掉界面和这条通知）。
 *   媒体控件暂停久了会被系统收起，所以没声音时不用播放通知。
 */
class PlaybackService : Service() {
    private var session: MediaSession? = null
    private var last: String? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, "运行和播放", NotificationManager.IMPORTANCE_LOW).apply {
                description = "App 开着时的常驻通知：启用 / 关闭效果、暂停 / 继续网页声音、退出"
                setShowBadge(false)
            },
        )
        session = MediaSession(this, "auto-asmr").apply {
            setCallback(object : MediaSession.Callback() {
                override fun onPlay() = Playback.resume()
                override fun onPause() = Playback.pause()
                override fun onStop() = Playback.pause()
                override fun onCustomAction(action: String, extras: Bundle?) {
                    if (action == CUSTOM_EFFECT) toggleEffect()
                }
            })
            isActive = true
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACT_TOGGLE -> Playback.togglePause()
            ACT_DRY -> toggleEffect()
            ACT_EXIT -> {
                Playback.exit()
                stopSelf()
                return START_NOT_STICKY
            }
        }
        startForeground(ID, build(), ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        return START_NOT_STICKY
    }

    /** 启用 / 关闭效果（同底栏的 ASMR 开关）。改好后 [Settings] 读回参数时会调 [update]，通知随之换过来。 */
    private fun toggleEffect() = Settings.setDry(!Settings.params.optBoolean("dry"))

    /** 状态变了就更新通知和 MediaSession（[Playback] 每秒调一次、效果开关变了时调一次，没变化时什么都不做）。 */
    fun update() {
        val key = "${Playback.media}|${Playback.pausedByUser}|${Settings.params.optBoolean("dry")}|${Playback.title()}"
        if (key == last) return
        last = key
        getSystemService(NotificationManager::class.java).notify(ID, build())
    }

    /** 重发一次通知（回到 App、刚给了通知权限时调用）。 */
    fun refresh() {
        last = null
        update()
    }

    private fun build(): Notification {
        val dry = Settings.params.optBoolean("dry")
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE)
        fun act(action: String, code: Int) = PendingIntent.getService(this, code, Intent(this, PlaybackService::class.java).setAction(action), PendingIntent.FLAG_IMMUTABLE)
        val effectLabel = if (dry) "启用效果" else "关闭效果"
        val effectIcon = if (dry) R.drawable.ic_effect_off else R.drawable.ic_stat_asmr
        val b = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_asmr)
            .setContentIntent(open)
            .setOngoing(true)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
        val s = session
        if (!Playback.media) {
            // 没有声音：普通常驻通知（不带媒体会话，免得媒体控件里挂着一个不在播放的会话）
            s?.setPlaybackState(PlaybackState.Builder().setState(PlaybackState.STATE_NONE, PlaybackState.PLAYBACK_POSITION_UNKNOWN, 0f).build())
            return b.setContentTitle("auto-asmr 正在运行")
                .setContentText(if (dry) "ASMR 效果已关闭（原声）" else "ASMR 效果已启用")
                .setCategory(Notification.CATEGORY_SERVICE)
                .addAction(Notification.Action.Builder(Icon.createWithResource(this, effectIcon), effectLabel, act(ACT_DRY, 2)).build())
                .addAction(Notification.Action.Builder(Icon.createWithResource(this, R.drawable.ic_stat_asmr), "退出", act(ACT_EXIT, 3)).build())
                .build()
        }
        val paused = Playback.pausedByUser
        val title = Playback.title()
        s?.setMetadata(MediaMetadata.Builder().putString(MediaMetadata.METADATA_KEY_TITLE, title).putString(MediaMetadata.METADATA_KEY_ARTIST, if (dry) "原声（效果已关闭）" else "ASMR 效果").build())
        s?.setPlaybackState(
            PlaybackState.Builder()
                .setActions(PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE or PlaybackState.ACTION_STOP)
                .setState(if (paused) PlaybackState.STATE_PAUSED else PlaybackState.STATE_PLAYING, PlaybackState.PLAYBACK_POSITION_UNKNOWN, 1f)
                .addCustomAction(PlaybackState.CustomAction.Builder(CUSTOM_EFFECT, effectLabel, effectIcon).build())
                .build(),
        )
        val icon = Icon.createWithResource(this, R.drawable.ic_stat_asmr)
        return b.setContentTitle(title)
            .setContentText(if (paused) "已暂停" else if (dry) "原声（效果已关闭）" else "ASMR 效果")
            .setCategory(Notification.CATEGORY_TRANSPORT)
            .addAction(Notification.Action.Builder(icon, if (paused) "继续" else "暂停", act(ACT_TOGGLE, 1)).build())
            .addAction(Notification.Action.Builder(Icon.createWithResource(this, effectIcon), effectLabel, act(ACT_DRY, 2)).build())
            .setStyle(Notification.MediaStyle().setMediaSession(s?.sessionToken).setShowActionsInCompactView(0, 1))
            .build()
    }

    override fun onDestroy() {
        instance = null
        session?.release()
        session = null
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        const val CHANNEL = "playback"
        const val ID = 1
        const val ACT_TOGGLE = "io.github.nasuiyile.autoasmr.TOGGLE"
        const val ACT_DRY = "io.github.nasuiyile.autoasmr.DRY"
        const val ACT_EXIT = "io.github.nasuiyile.autoasmr.EXIT"
        /** 媒体会话里「启用 / 关闭效果」按钮的名字（Android 13 起通知栏的媒体控件显示它）。 */
        const val CUSTOM_EFFECT = "effect"
        @Volatile var instance: PlaybackService? = null
    }
}
