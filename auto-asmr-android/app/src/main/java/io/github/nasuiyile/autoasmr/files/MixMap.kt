package io.github.nasuiyile.autoasmr.files

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.nasuiyile.autoasmr.Radar
import io.github.nasuiyile.autoasmr.Settings
import org.json.JSONObject
import java.util.Locale
import kotlin.math.cos
import kotlin.math.sin

/** 几个声音在位置图上的颜色（同电脑版：1 号橙、2 号蓝、3 号绿、4 号紫）。 */
val VOICE_COLORS = listOf(Color(0xFFFF7828), Color(0xFF4696F0), Color(0xFF46B464), Color(0xFFBE64E1))

/** 轨迹留多少个点（界面约 0.15 秒轮询一次，约 2 秒）。 */
private const val TRAIL = 14

/** 各声音最近走过的位置（方位, 水平距离），试听时随状态更新。 */
private class Trails {
    val pts = List(4) { ArrayDeque<Pair<Float, Float>>() }
}

/**
 * 多音频合成 / 左右双人的位置图（俯视，上 = 正前方，刻度和单个声音的位置图一样），照电脑版 `ui_radar` 的合成部分：
 * - 彩色实心点 = 各声音此刻的位置（俯视投影，试听中带一小段轨迹），旁边是编号；同色的圈 = 摆好的位置（拖它）。
 * - 各自固定：按下时抓住离按下点最近的那个声音，拖到哪里它就摆到哪里（方位和距离）。
 * - 一起移动 + 手动摆位：圈是整组摆好的位置，拖任意一个，整组跟着挪（其余声音保持错开的角度）；右侧竖条上拖高度，
 *   双击竖条回到与耳朵齐平。其它移动方式下不能拖。
 * - 右侧竖条：各声音此刻的高度。
 * 图下面是选中 / 正在拖的声音的读数，没选中时是操作提示。
 */
@Composable
fun MixMap(st: JSONObject) {
    val positions = st.optJSONArray("positions")
    val voices = st.optJSONArray("voices")
    val offsets = st.optJSONArray("follow_offsets")
    val n = minOf(positions?.length() ?: 0, VOICE_COLORS.size)
    val p = Settings.params
    val follow = p.optBoolean("mix_follow")
    val manual = p.optString("mode") == "Manual"
    val draggable = !follow || manual
    val active = st.optBoolean("previewing") && st.optBoolean("rt_mix")
    val mAz = p.optDouble("manual_az", 270.0).toFloat()
    val mDist = p.optDouble("manual_dist", 0.105).toFloat()
    val mTilt = p.optDouble("manual_tilt", 0.0).toFloat()
    val line = MaterialTheme.colorScheme.outline
    val text = MaterialTheme.colorScheme.onSurface
    val accent = MaterialTheme.colorScheme.primary
    val measurer = rememberTextMeasurer()
    var dragK by remember { mutableStateOf<Int?>(null) }
    val sel = dragK ?: st.optInt("mix_sel", -1).takeIf { it >= 0 && st.has("mix_sel") && !st.isNull("mix_sel") }
    var lastSend by remember { mutableStateOf(0L) }

    fun pos(k: Int): FloatArray {
        val q = positions?.optJSONArray(k) ?: return floatArrayOf(0f, 0f, 0.1f, 0f)
        return FloatArray(4) { q.optDouble(it).toFloat() }
    }
    fun offset(k: Int) = offsets?.optDouble(k)?.toFloat() ?: 0f
    // 每来一份新状态记一个点（同一份状态只记一次）；不在试听时清掉
    val trails = remember { Trails() }
    remember(st) {
        for (k in 0 until 4) {
            val tr = trails.pts[k]
            if (!active || k >= n) {
                tr.clear()
                continue
            }
            val q = pos(k)
            tr.addLast(q[0] to q[2] * cos(Math.toRadians(q[1].toDouble())).toFloat())
            while (tr.size > TRAIL) tr.removeFirst()
        }
        0
    }

    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Canvas(
                Modifier.size(236.dp).pointerInput(draggable, n, follow) {
                    if (!draggable || n == 0) return@pointerInput
                    awaitEachGesture {
                        val down = awaitFirstDown()
                        val c = Offset(size.width / 2f, size.height / 2f)
                        val rmax = size.width * 0.47f
                        fun at(az: Float, r: Float): Offset {
                            val a = Math.toRadians(az.toDouble())
                            val m = Radar.map(r) * rmax
                            return c + Offset((sin(a) * m).toFloat(), (-cos(a) * m).toFloat())
                        }
                        // 各自固定：按摆好的位置找最近的；一起移动：按此刻的位置找（同电脑版）
                        val home = { k: Int ->
                            if (!follow) {
                                val v = voices?.optJSONObject(k)
                                at(v?.optDouble("az")?.toFloat() ?: 0f, v?.optDouble("dist")?.toFloat() ?: 0.1f)
                            } else {
                                val q = pos(k)
                                at(q[0], q[2] * cos(Math.toRadians(q[1].toDouble())).toFloat())
                            }
                        }
                        val k = (0 until n).minByOrNull { (home(it) - down.position).getDistance() } ?: return@awaitEachGesture
                        dragK = k
                        if (!follow) FileSession.cmd("mix_sel", "index" to k)
                        val apply = { o: Offset, force: Boolean ->
                            val now = System.currentTimeMillis()
                            if (force || now - lastSend > 40) {
                                lastSend = now
                                val (az, r) = Radar.pick(o.x - c.x, o.y - c.y, rmax)
                                if (follow) {
                                    Settings.setNumber("manual_az", (((az - offset(k)) % 360f + 360f) % 360f).toDouble())
                                    Settings.setNumber("manual_dist", r.toDouble())
                                } else {
                                    FileSession.cmd("voice", "index" to k, "az" to az.toDouble(), "dist" to r.toDouble())
                                }
                            }
                        }
                        apply(down.position, true)
                        var last = down.position
                        while (true) {
                            val ev = awaitPointerEvent()
                            val ch = ev.changes.firstOrNull() ?: break
                            if (!ch.pressed) break
                            last = ch.position
                            apply(last, false)
                            ch.consume()
                        }
                        apply(last, true)
                        dragK = null
                    }
                },
            ) {
                val c = Offset(size.width / 2f, size.height / 2f)
                val rmax = size.width * 0.47f
                fun at(az: Float, r: Float): Offset {
                    val a = Math.toRadians(az.toDouble())
                    val m = Radar.map(r) * rmax
                    return c + Offset((sin(a) * m).toFloat(), (-cos(a) * m).toFloat())
                }
                for (r in floatArrayOf(0.105f, 0.2f, 0.4f, 0.8f)) {
                    drawCircle(line.copy(alpha = 0.5f), Radar.map(r) * rmax, c, style = Stroke(1.dp.toPx()))
                }
                drawLine(line.copy(alpha = 0.3f), c - Offset(rmax, 0f), c + Offset(rmax, 0f))
                drawLine(line.copy(alpha = 0.3f), c - Offset(0f, rmax), c + Offset(0f, rmax))
                val head = Radar.map(Radar.HEAD_A) * rmax
                drawCircle(line, head, c, style = Stroke(2.dp.toPx()))
                val nose = Path().apply {
                    moveTo(c.x - head * 0.18f, c.y - head * 0.97f)
                    lineTo(c.x, c.y - head * 1.22f)
                    lineTo(c.x + head * 0.18f, c.y - head * 0.97f)
                }
                drawPath(nose, line, style = Stroke(2.dp.toPx()))
                for (s in floatArrayOf(-1f, 1f)) {
                    drawOval(line, Offset(c.x + s * head - 4.dp.toPx(), c.y - 9.dp.toPx()), Size(8.dp.toPx(), 18.dp.toPx()))
                }
                for (k in 0 until n) {
                    val col = VOICE_COLORS[k]
                    // 轨迹：越早的越淡
                    val tr = trails.pts[k]
                    for (i in 1 until tr.size) {
                        val a = i.toFloat() / tr.size * 0.6f
                        drawLine(col.copy(alpha = a), at(tr[i - 1].first, tr[i - 1].second), at(tr[i].first, tr[i].second), 2.dp.toPx())
                    }
                    val q = pos(k)
                    val pt = at(q[0], q[2] * cos(Math.toRadians(q[1].toDouble())).toFloat())
                    // 摆好的位置（空心圈，可拖）：各自固定时是各自的位置，一起移动 + 手动时是整组的位置
                    val ring = if (!follow) {
                        voices?.optJSONObject(k)?.let { v -> at(v.optDouble("az").toFloat(), v.optDouble("dist").toFloat()) }
                    } else if (manual) {
                        at(mAz + offset(k), mDist)
                    } else null
                    if (ring != null) drawCircle(col, 12.dp.toPx(), ring, style = Stroke(if (sel == k) 3.dp.toPx() else 1.5.dp.toPx()))
                    val speaking = active && q[3] > 0.5f
                    drawCircle(if (active) col else col.copy(alpha = 0.75f), if (speaking) 7.dp.toPx() else 5.5.dp.toPx(), pt)
                    val label = ring ?: pt
                    drawText(measurer, (k + 1).toString(), label + Offset(8.dp.toPx(), -22.dp.toPx()), TextStyle(color = col, fontSize = 11.sp))
                }
            }
            Spacer(Modifier.width(6.dp))
            // 高度竖条：-80° … 85°，中间 = 与耳朵齐平；一起移动 + 手动摆位时可以拖（双击回到齐平）
            val tiltEditable = follow && manual
            Canvas(
                Modifier.size(width = 22.dp, height = 200.dp)
                    .pointerInput(tiltEditable) {
                        if (!tiltEditable) return@pointerInput
                        detectTapGestures(onDoubleTap = { Settings.setNumber("manual_tilt", 0.0) }, onTap = { o ->
                            Settings.setNumber("manual_tilt", ((size.height / 2f - o.y) / (size.height / 2f) * 90f).coerceIn(-80f, 85f).toDouble())
                        })
                    }
                    .pointerInput(tiltEditable) {
                        if (!tiltEditable) return@pointerInput
                        detectDragGestures { change, _ ->
                            val t = ((size.height / 2f - change.position.y) / (size.height / 2f) * 90f).coerceIn(-80f, 85f)
                            Settings.setNumber("manual_tilt", t.toDouble())
                        }
                    },
            ) {
                val x = size.width / 2f
                drawLine(line, Offset(x, 0f), Offset(x, size.height), 2.dp.toPx())
                drawLine(line, Offset(x - 8.dp.toPx(), size.height / 2f), Offset(x + 8.dp.toPx(), size.height / 2f), 1.dp.toPx())
                fun y(deg: Float) = size.height / 2f - deg.coerceIn(-80f, 85f) / 90f * size.height / 2f
                if (tiltEditable) drawCircle(accent, 7.dp.toPx(), Offset(x, y(mTilt)), style = Stroke(2.dp.toPx()))
                for (k in 0 until n) {
                    val q = pos(k)
                    val col = VOICE_COLORS[k]
                    drawCircle(if (active) col else col.copy(alpha = 0.75f), if (active && q[3] > 0.5f) 5.dp.toPx() else 4.dp.toPx(), Offset(x, y(q[1])))
                }
            }
        }
        val readout = sel?.takeIf { it < n }?.let { k ->
            val q = pos(k)
            val plan = q[2] * cos(Math.toRadians(q[1].toDouble())).toFloat()
            String.format(Locale.ROOT, "%d 号：离耳 %.1f cm · 方位 %.0f°%s", k + 1, ((plan - Radar.HEAD_A) * 100f).coerceAtLeast(0f), q[0], if (kotlin.math.abs(q[1]) >= 1f) String.format(Locale.ROOT, " · %s %.0f°", if (q[1] > 0) "上" else "下", kotlin.math.abs(q[1])) else "")
        } ?: when {
            follow && !manual -> "一起移动：按「左右移动」的方式一起走，彼此相对；切到手动摆位可以拖着挪整组"
            follow -> "一起移动 + 手动摆位：拖任意一个，整组跟着挪 · 右侧拖高度，双击回到齐平"
            else -> "各自固定：拖圆圈摆位置，下拉框也可以选"
        }
        Text(readout, style = MaterialTheme.typography.bodySmall, color = sel?.let { VOICE_COLORS[it % VOICE_COLORS.size] } ?: text)
    }
}
