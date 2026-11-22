package io.github.nasuiyile.autoasmr

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.lazy.LazyListScope
import org.json.JSONObject
import java.util.Locale

/**
 * 效果面板：按参数表（和电脑版界面同样的分组、标签、范围、说明）生成。网页模式（底部抽屉）和全局模式（整页）共用。
 * 从上到下：[header]（全局模式的服务和保活卡片）、输出效果 / 监听原声、风格按钮、位置图和手动摆位、降噪、各组参数、
 * 导入 / 导出配置、[footer]（全局模式的日志）。[status] 是标题下面那一行状态。
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun EffectPanel(
    tele: FloatArray,
    status: String,
    onImport: () -> Unit,
    onExport: () -> Unit,
    modifier: Modifier = Modifier,
    header: LazyListScope.() -> Unit = {},
    footer: LazyListScope.() -> Unit = {},
    /** 显示「效果 / 原声」开关（文件转换页不显示：同电脑版，试听总是带效果）。 */
    showDry: Boolean = true,
    /** 换掉位置图（文件转换页的多音频合成 / 左右双人：几个声音的位置图）。 */
    map: (@Composable () -> Unit)? = null,
) {
    val spec = Settings.spec
    val p = Settings.params
    val groups = if (spec != null) SpecUi.items(spec.optJSONArray("groups")) else emptyList()
    val manual = p.optString("mode") == "Manual"
    LazyColumn(modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
        header()
        if (!Settings.ready || spec == null) {
            item { Text("正在加载 KU100 数据…", Modifier.padding(24.dp)) }
            footer()
            return@LazyColumn
        }
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("ASMR 效果", style = MaterialTheme.typography.titleMedium)
                    Text(
                        status,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (showDry) {
                    Text(if (p.optBoolean("dry")) "原声" else "效果", Modifier.padding(end = 8.dp))
                    Switch(checked = !p.optBoolean("dry"), onCheckedChange = { Settings.setDry(!it) })
                }
            }
            Spacer(Modifier.height(8.dp))
            Text("风格", style = MaterialTheme.typography.labelLarge)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                // 选中的风格（参数和它一致）用实心按钮标出来；手动调过风格管的参数后都不标
                val active = p.optString("active_style")
                for (s in SpecUi.items(spec.optJSONArray("styles"))) {
                    val action = s.optString("action")
                    if (action == active) {
                        Button(onClick = { Settings.action(action) }) { Text("✓ " + s.optString("label")) }
                    } else {
                        OutlinedButton(onClick = { Settings.action(action) }) { Text(s.optString("label")) }
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
            Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { if (map != null) map() else PositionMap(p, tele, manual) }
            val m = spec.optJSONObject("manual")
            val toggleManual = { on: Boolean ->
                val pose = if (tele.isNotEmpty() && tele[0] > 0f) floatArrayOf(tele[1], tele[2], tele[3]) else null
                Settings.setManual(on, pose)
            }
            Row(Modifier.fillMaxWidth().clickable { toggleManual(!manual) }, verticalAlignment = Alignment.CenterVertically) {
                Checkbox(checked = manual, onCheckedChange = { toggleManual(it) })
                Text(m?.optString("label") ?: "手动摆位", style = MaterialTheme.typography.bodyMedium)
            }
            spec.optJSONObject("denoise")?.let { SliderItem(it, p, true) }
            HorizontalDivider(Modifier.padding(vertical = 8.dp))
        }
        items(groups, key = { it.optString("id") }) { g -> Group(g, p, tele) }
        item {
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = onImport) { Text("导入配置") }
                OutlinedButton(onClick = onExport) { Text("导出配置") }
            }
            Text(
                "配置文件和电脑版通用：电脑版「导出配置」的 .ron 可以直接导入；这里导出的文件也能在电脑版里导入。" +
                    "电脑版独有的设置（音频设备、文件转换等）会原样保留。",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(vertical = 8.dp),
            )
            Spacer(Modifier.height(8.dp))
        }
        footer()
        item { Spacer(Modifier.height(32.dp)) }
    }
}

fun statusLine(tele: FloatArray, audibleTitle: String?, rain: Boolean = false): String {
    if (tele.isEmpty() || tele[0] <= 0f) return if (audibleTitle != null) "等待声音：$audibleTitle" else if (rain) "雨声 · 没有正在出声的网页" else "没有正在出声的网页"
    val rate = if (tele[8] >= 44000f && tele[8] < 45000f) "44.1 kHz" else String.format(Locale.ROOT, "%.0f kHz", tele[8] / 1000f)
    val err = if (tele[10] > 0f) " · 引擎出错，已改原声" else ""
    return String.format(Locale.ROOT, "正在处理%s · %s · 延迟 %.1f ms · 占用 %.1f%%%s", audibleTitle?.let { "：$it" } ?: "", rate, tele[6], tele[5], err)
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Group(g: JSONObject, p: JSONObject, tele: FloatArray) {
    var open by remember { mutableStateOf(g.optBoolean("open") || SpecUi.cond(g.optJSONObject("open_if"), p)) }
    val enabled = SpecUi.cond(g.optJSONObject("enable_if"), p)
    Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Row(
            Modifier.fillMaxWidth().clickable { open = !open }.padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(SpecUi.groupTitle(g, p), style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            Text(if (open) "▲" else "▼", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (open) {
            Column(Modifier.padding(start = 12.dp, end = 12.dp, bottom = 10.dp).alpha(if (enabled) 1f else 0.45f)) {
                val actions = SpecUi.items(g.optJSONArray("actions")).filter { SpecUi.cond(it.optJSONObject("show_if"), p) }
                if (actions.isNotEmpty()) {
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        for (a in actions) OutlinedButton(onClick = { Settings.action(a.optString("action")) }) { Text(a.optString("label")) }
                    }
                }
                for (it in SpecUi.items(g.optJSONArray("items"))) {
                    if (!SpecUi.cond(it.optJSONObject("show_if"), p)) continue
                    val en = enabled && SpecUi.cond(it.optJSONObject("enable_if"), p)
                    when (it.optString("type")) {
                        "slider" -> SliderItem(it, p, en)
                        "check" -> CheckItem(it, p, en)
                        "choice" -> ChoiceItem(it, p, en, tele)
                        "int" -> IntItem(it, p, en)
                        "note" -> Text(
                            it.optString("text"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(vertical = 4.dp),
                        )
                    }
                }
            }
        }
    }
}

/** 标签（点一下展开说明）。 */
@Composable
private fun Label(item: JSONObject, value: String?, modifier: Modifier = Modifier) {
    var tip by remember { mutableStateOf(false) }
    val t = item.optString("tip")
    Column(modifier) {
        Row(Modifier.fillMaxWidth().clickable(enabled = t.isNotEmpty()) { tip = !tip }, verticalAlignment = Alignment.CenterVertically) {
            Text(item.optString("label") + if (t.isNotEmpty()) " ⓘ" else "", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
            if (value != null) Text(value, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.primary)
        }
        if (tip && t.isNotEmpty()) {
            Text(t, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
fun SliderItem(item: JSONObject, p: JSONObject, enabled: Boolean) {
    val key = item.optString("key")
    val stored = p.optDouble(key, item.optDouble("min"))
    var drag by remember(key) { mutableStateOf<Float?>(null) }
    val pos = drag ?: SpecUi.toPos(item, stored)
    val shown = if (drag != null) SpecUi.fromPos(item, drag!!) else stored
    Column(Modifier.padding(top = 6.dp)) {
        Label(item, SpecUi.format(item, shown))
        Slider(
            value = pos,
            onValueChange = { t ->
                drag = t
                Settings.setNumber(key, SpecUi.fromPos(item, t))
            },
            onValueChangeFinished = { drag = null },
            enabled = enabled,
            modifier = Modifier.height(32.dp),
        )
    }
}

@Composable
private fun CheckItem(item: JSONObject, p: JSONObject, enabled: Boolean) {
    val key = item.optString("key")
    val on = p.optBoolean(key)
    Row(Modifier.fillMaxWidth().clickable(enabled = enabled) { Settings.setBool(key, !on) }, verticalAlignment = Alignment.CenterVertically) {
        Checkbox(checked = on, onCheckedChange = { Settings.setBool(key, it) }, enabled = enabled)
        Label(item, null, Modifier.weight(1f))
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ChoiceItem(item: JSONObject, p: JSONObject, enabled: Boolean, tele: FloatArray) {
    val key = item.optString("key")
    val cur = p.opt(key)?.toString()
    val opts = SpecUi.items(item.optJSONArray("options"))
    val choose = { o: JSONObject ->
        val v = o.opt("value")
        if (key == "mode" && (v == "Manual") != (cur == "Manual")) {
            // 进出手动和位置图下面的勾选框是同一套逻辑（同电脑版）
            val pose = if (tele.isNotEmpty() && tele[0] > 0f) floatArrayOf(tele[1], tele[2], tele[3]) else null
            if (v == "Manual") Settings.setManual(true, pose) else Settings.set(key, SpecUi.jsonLiteral(v))
        } else {
            Settings.set(key, SpecUi.jsonLiteral(v))
        }
    }
    Column(Modifier.padding(top = 6.dp)) {
        if (item.optString("style") == "menu") {
            var open by remember { mutableStateOf(false) }
            val label = opts.firstOrNull { it.opt("value")?.toString() == cur }?.optString("label") ?: cur.orEmpty()
            Label(item, null)
            Box {
                OutlinedButton(onClick = { open = true }, enabled = enabled) { Text(label) }
                DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                    for (o in opts) DropdownMenuItem(text = { Text(o.optString("label")) }, onClick = { open = false; choose(o) })
                }
            }
        } else {
            Label(item, null)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (o in opts) {
                    FilterChip(selected = o.opt("value")?.toString() == cur, onClick = { choose(o) }, label = { Text(o.optString("label")) }, enabled = enabled)
                }
            }
        }
    }
}

@Composable
private fun IntItem(item: JSONObject, p: JSONObject, enabled: Boolean) {
    val key = item.optString("key")
    val v = p.optLong(key, 1)
    Row(Modifier.padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Label(item, null, Modifier.weight(1f))
        TextButton(onClick = { Settings.set(key, (v - 1).coerceAtLeast(0).toString()) }, enabled = enabled) { Text("−") }
        Text(v.toString(), Modifier.width(48.dp), style = MaterialTheme.typography.bodyLarge)
        TextButton(onClick = { Settings.set(key, (v + 1).toString()) }, enabled = enabled) { Text("+") }
    }
}
