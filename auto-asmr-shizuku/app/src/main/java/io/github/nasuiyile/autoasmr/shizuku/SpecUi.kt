package io.github.nasuiyile.autoasmr.shizuku

import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale
import kotlin.math.abs
import kotlin.math.ln
import kotlin.math.pow
import kotlin.math.roundToInt

/** 参数表（ui_spec.json）的小工具：数值显示、显示 / 可用条件、对数滑条。显示格式和电脑版界面一致。 */
object SpecUi {
    fun str(o: JSONObject, key: String): String = o.optString(key, "")

    fun items(a: JSONArray?): List<JSONObject> = if (a == null) emptyList() else (0 until a.length()).map { a.getJSONObject(it) }

    /** 条件：{"key":…, "in"/"not_in":[…]} 或 {"key":…, "gt":数} 或 {"key":…, "eq":值}；没有条件 = 成立。 */
    fun cond(c: JSONObject?, p: JSONObject): Boolean {
        if (c == null) return true
        val v = p.opt(c.optString("key"))
        c.optJSONArray("in")?.let { a -> return (0 until a.length()).any { a.opt(it)?.toString() == v?.toString() } }
        c.optJSONArray("not_in")?.let { a -> return (0 until a.length()).none { a.opt(it)?.toString() == v?.toString() } }
        if (c.has("gt")) return (v as? Number)?.toDouble()?.let { it > c.getDouble("gt") } ?: false
        if (c.has("eq")) return v == c.get("eq")
        return true
    }

    private fun f(fmt: String, vararg a: Any) = String.format(Locale.ROOT, fmt, *a)

    /** 滑条旁边显示的数值（和电脑版一样：小数位、单位，背景音和降噪有自己的写法）。 */
    fun format(item: JSONObject, v: Double): String = when (item.optString("fmt")) {
        "denoise" -> when {
            v < 0.5 -> "0 关"
            v >= 99.5 -> "100 最大"
            else -> "${v.roundToInt()} dB"
        }
        "bg_level" -> if (v < 0.5) "0 关" else f("%.0f · −%.0f dB", v, 40 - 0.4 * v)
        "rain_mmh" -> f("%.0f · %.1f mm/h", v, 0.3 * 200.0.pow(v / 100))
        "pan" -> when {
            abs(v) < 0.5 -> "中"
            v < 0 -> f("左 %.0f", -v)
            else -> f("右 %.0f", v)
        }
        "flow" -> if (v < 0.5) "0 不动" else f("%.0f", v)
        "land" -> if (v < 0.5) "0 无" else f("%.0f · %+.0f dB", v, 0.2 * (v - 60))
        "drip" -> if (v < 0.5) "0 无" else f("%.0f · %d 处", v, if (v < 34) 1 else if (v < 67) 2 else 3)
        "thunder" -> if (v < 0.5) "0 无" else {
            val i = 240.0 * (25.0 / 240.0).pow(v / 100)
            val d = 8.0 * (1.5 / 8.0).pow(v / 100)
            if (i >= 90) f("%.0f · %.0f 分钟 · %.1f km", v, i / 60, d) else f("%.0f · %.0f s · %.1f km", v, i, d)
        }
        "plain" -> f("%.0f", v)
        "duo" -> if (v < 0.5) "0 关" else f("%.0f ms", v)
        "balance" -> if (v < 0.5) "0 关" else f("%.0f%%", v)
        else -> {
            val unit = item.optString("unit")
            val num = f("%.${item.optInt("decimals", 2)}f", v)
            when {
                unit.isEmpty() -> num
                unit == "°" -> num + unit
                else -> "$num $unit"
            }
        }
    }

    /** 分组标题（背景音的标题带上「比人声低多少 dB」）。 */
    fun groupTitle(g: JSONObject, p: JSONObject): String {
        val t = str(g, "title")
        if (g.optString("title_fmt") != "bg_title") return t
        val lvl = p.optDouble("bg_level", 0.0)
        return if (lvl > 0) f("%s · 比人声低 %.0f dB", t, 40 - 0.4 * lvl.coerceAtMost(100.0)) else "$t · 关"
    }

    /** 滑条位置（0–1）↔ 数值；对数滑条按比例。 */
    fun toPos(item: JSONObject, v: Double): Float {
        val (lo, hi) = item.getDouble("min") to item.getDouble("max")
        val x = v.coerceIn(lo, hi)
        return if (item.optBoolean("log") && lo > 0) (ln(x / lo) / ln(hi / lo)).toFloat() else ((x - lo) / (hi - lo)).toFloat()
    }

    fun fromPos(item: JSONObject, t: Float): Double {
        val (lo, hi) = item.getDouble("min") to item.getDouble("max")
        var v = if (item.optBoolean("log") && lo > 0) lo * (hi / lo).pow(t.toDouble()) else lo + (hi - lo) * t
        val step = item.optDouble("step", 0.0)
        if (step > 0) v = (v / step).roundToInt() * step
        return v
    }

    /** 选项的值写成 JSON 字面量（字符串加引号，对象原样）。 */
    fun jsonLiteral(v: Any?): String = when (v) {
        is String -> JSONObject.quote(v)
        null -> "null"
        else -> v.toString()
    }
}
