package io.github.nasuiyile.autoasmr

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.unit.dp
import org.json.JSONObject
import java.util.Locale
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.exp
import kotlin.math.hypot
import kotlin.math.ln
import kotlin.math.sin

/**
 * 位置图（俯视，上 = 正前方），刻度和电脑版一样：到头中心的距离按对数压缩，2 cm 在中心、80 cm 在边上。
 * 圆点是声源此刻的位置（播放中才有）；手动摆位时蓝圈是摆好的位置，在图里点 / 拖改方位和距离，在右边竖条上拖改高度
 * （双击竖条回到与耳朵齐平）。
 */
object Radar {
    const val MAX_R = 0.8f
    const val HEAD_A = 0.0875f
    const val R_MIN = 0.095f

    fun map(r: Float): Float = (ln((r / 0.02f).coerceAtLeast(1f)) / ln(MAX_R / 0.02f)).coerceIn(0f, 1f)

    /** 图上相对中心的偏移 → (方位角 度, 到头中心的距离 米)。 */
    fun pick(dx: Float, dy: Float, rmax: Float): Pair<Float, Float> {
        val az = Math.toDegrees(atan2(dx, -dy).toDouble()).toFloat().let { (it % 360 + 360) % 360 }
        val r = 0.02f * exp(ln(MAX_R / 0.02f) * (hypot(dx, dy) / rmax).coerceIn(0f, 1f))
        return az to r.coerceIn(R_MIN, MAX_R)
    }
}

@Composable
fun PositionMap(p: JSONObject, tele: FloatArray, manual: Boolean) {
    val playing = tele.isNotEmpty() && tele[0] > 0f
    // 左右双人：两个声音（颜色同电脑版：1 号橙、2 号蓝），2 号的位置在 tele[12..16]
    val duo = playing && tele.size >= 16 && tele[11] > 0f
    val c1 = if (duo) Color(0xFFFF7828) else Color(0xFFE5484D)
    val c2 = Color(0xFF4696F0)
    val accent = MaterialTheme.colorScheme.primary
    val line = MaterialTheme.colorScheme.outline
    val text = MaterialTheme.colorScheme.onSurface
    val mAz = p.optDouble("manual_az", 270.0).toFloat()
    val mDist = p.optDouble("manual_dist", 0.105).toFloat()
    val mTilt = p.optDouble("manual_tilt", 0.0).toFloat()

    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Canvas(
                Modifier.size(200.dp)
                    .pointerInput(manual) {
                        if (!manual) return@pointerInput
                        val set = { o: Offset ->
                            val rmax = size.width * 0.47f
                            val (az, r) = Radar.pick(o.x - size.width / 2f, o.y - size.height / 2f, rmax)
                            Settings.setNumber("manual_az", az.toDouble())
                            Settings.setNumber("manual_dist", r.toDouble())
                        }
                        detectTapGestures(onTap = set)
                    }
                    .pointerInput(manual) {
                        if (!manual) return@pointerInput
                        detectDragGestures { change, _ ->
                            val rmax = size.width * 0.47f
                            val (az, r) = Radar.pick(change.position.x - size.width / 2f, change.position.y - size.height / 2f, rmax)
                            Settings.setNumber("manual_az", az.toDouble())
                            Settings.setNumber("manual_dist", r.toDouble())
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
                // 距离圈：贴耳（离耳 1.7 cm）、离头 11 / 31 / 71 cm
                for (r in floatArrayOf(0.105f, 0.2f, 0.4f, 0.8f)) {
                    drawCircle(line.copy(alpha = 0.5f), Radar.map(r) * rmax, c, style = Stroke(1.dp.toPx()))
                }
                drawLine(line.copy(alpha = 0.3f), c - Offset(rmax, 0f), c + Offset(rmax, 0f))
                drawLine(line.copy(alpha = 0.3f), c - Offset(0f, rmax), c + Offset(0f, rmax))
                // 头：圆 + 鼻子（朝上）+ 两只耳朵
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
                if (manual) {
                    val tgt = at(mAz, mDist * cos(Math.toRadians(mTilt.toDouble())).toFloat())
                    drawCircle(accent, 11.dp.toPx(), tgt, style = Stroke(2.dp.toPx()))
                }
                if (playing) {
                    val horiz = tele[3] * cos(Math.toRadians(tele[2].toDouble())).toFloat()
                    val pt = at(tele[1], horiz)
                    drawCircle(c1.copy(alpha = 0.45f + 0.55f * tele[4].coerceIn(0f, 1f)), 7.dp.toPx(), pt)
                }
                if (duo) {
                    val horiz = tele[14] * cos(Math.toRadians(tele[13].toDouble())).toFloat()
                    drawCircle(c2.copy(alpha = 0.45f + 0.55f * tele[15].coerceIn(0f, 1f)), 7.dp.toPx(), at(tele[12], horiz))
                }
            }
            Spacer(Modifier.width(6.dp))
            // 高度竖条：-80° … 85°，中间 = 与耳朵齐平
            Canvas(
                Modifier.size(width = 22.dp, height = 180.dp)
                    .pointerInput(manual) {
                        if (!manual) return@pointerInput
                        detectTapGestures(onDoubleTap = { Settings.setNumber("manual_tilt", 0.0) }, onTap = { o ->
                            Settings.setNumber("manual_tilt", ((size.height / 2f - o.y) / (size.height / 2f) * 90f).coerceIn(-80f, 85f).toDouble())
                        })
                    }
                    .pointerInput(manual) {
                        if (!manual) return@pointerInput
                        detectDragGestures { change, _ ->
                            val t = ((size.height / 2f - change.position.y) / (size.height / 2f) * 90f).coerceIn(-80f, 85f)
                            Settings.setNumber("manual_tilt", t.toDouble())
                        }
                    },
            ) {
                val x = size.width / 2f
                drawLine(line, Offset(x, 0f), Offset(x, size.height), 2.dp.toPx())
                drawLine(line, Offset(x - 8.dp.toPx(), size.height / 2f), Offset(x + 8.dp.toPx(), size.height / 2f), 1.dp.toPx())
                fun y(deg: Float) = size.height / 2f - deg / 90f * size.height / 2f
                if (manual) drawCircle(accent, 7.dp.toPx(), Offset(x, y(mTilt)), style = Stroke(2.dp.toPx()))
                if (playing) drawCircle(c1, 5.dp.toPx(), Offset(x, y(tele[2].coerceIn(-80f, 85f))))
                if (duo) drawCircle(c2, 5.dp.toPx(), Offset(x, y(tele[13].coerceIn(-80f, 85f))))
            }
        }
        val read = if (duo) {
            String.format(Locale.ROOT, "左右双人 · 1 号（橙）方位 %.0f° 离头 %.1f cm · 2 号（蓝）方位 %.0f° 离头 %.1f cm", tele[1], (tele[3] - Radar.HEAD_A) * 100f, tele[12], (tele[14] - Radar.HEAD_A) * 100f)
        } else if (playing) {
            val cm = (tele[3] - Radar.HEAD_A) * 100f
            val el = tele[2]
            String.format(Locale.ROOT, "方位 %.0f° · 离头 %.1f cm · %s", tele[1], cm, if (kotlin.math.abs(el) < 1f) "与耳齐平" else if (el > 0) "上 %.0f°".format(Locale.ROOT, el) else "下 %.0f°".format(Locale.ROOT, -el))
        } else if (manual) {
            String.format(Locale.ROOT, "摆好的位置：方位 %.0f° · 离头 %.1f cm · 高度 %.0f°", mAz, (mDist - Radar.HEAD_A) * 100f, mTilt)
        } else "没有正在处理的声音"
        Row(horizontalArrangement = Arrangement.Center) {
            Text(read, style = MaterialTheme.typography.bodySmall, color = text)
        }
    }
}
