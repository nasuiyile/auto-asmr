package io.github.nasuiyile.autoasmr

import androidx.compose.foundation.clickable
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.IconButton
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.text.style.TextAlign
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
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
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
    // 输出增益从「输出」组里拿出来，放到降噪下面（同电脑版左下角）
    val outGain = groups.firstNotNullOfOrNull { g -> SpecUi.items(g.optJSONArray("items")).firstOrNull { it.optString("key") == OUT_GAIN } }
    val manual = p.optString("mode") == "Manual"
    LazyColumn(modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
        header()
        if (!Settings.ready || spec == null) {
            item { Text("正在加载 KU100 数据…", Modifier.padding(24.dp)) }
            footer()
            return@LazyColumn
        }
        item {
            // 效果开关和状态：开着时用主色的卡片，一眼看出是不是在加效果
            val on = !showDry || !p.optBoolean("dry")
            Card(
                Modifier.fillMaxWidth().padding(vertical = 4.dp),
                shape = MaterialTheme.shapes.large,
                colors = CardDefaults.cardColors(
                    containerColor = if (on) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceContainerHigh,
                ),
            ) {
                Row(
                    Modifier.fillMaxWidth().let { m -> if (showDry) m.clickable { Settings.setDry(on) } else m }.padding(horizontal = 14.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f).padding(end = 8.dp)) {
                        Text(if (on) "ASMR 效果" else "原声（不加效果）", style = MaterialTheme.typography.titleSmall)
                        Text(status, style = MaterialTheme.typography.bodySmall, maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.alpha(0.8f))
                    }
                    if (showDry) Switch(checked = on, onCheckedChange = { Settings.setDry(!it) })
                }
            }
        }
        item {
            // 「恢复默认」放在标题行右边，几个风格排成一行
            val styles = SpecUi.items(spec.optJSONArray("styles"))
            val reset = styles.firstOrNull { it.optString("action") == "reset" }
            Section("风格", actions = {
                if (reset != null) {
                    TextButton(onClick = { Settings.action("reset") }, modifier = Modifier.height(32.dp), contentPadding = PaddingValues(horizontal = 10.dp)) {
                        Text(reset.optString("label"))
                    }
                }
            }) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    // 选中的风格（参数和它一致）标出来；手动调过风格管的参数后都不标
                    val active = p.optString("active_style")
                    for (s in styles) {
                        val action = s.optString("action")
                        if (action == "reset") continue
                        val sel = action == active
                        FilterChip(
                            selected = sel,
                            onClick = { Settings.action(action) },
                            label = { Text(s.optString("label")) },
                            leadingIcon = if (sel) ({ Ic(AppIcons.Check, size = 18.dp) }) else null,
                        )
                    }
                }
            }
        }
        item {
            val m = spec.optJSONObject("manual")
            val toggleManual = { on: Boolean ->
                val pose = if (tele.isNotEmpty() && tele[0] > 0f) floatArrayOf(tele[1], tele[2], tele[3]) else null
                Settings.setManual(on, pose)
            }
            Section("声源位置") {
                Box(Modifier.fillMaxWidth().padding(top = 2.dp), contentAlignment = Alignment.Center) { if (map != null) map() else PositionMap(p, tele, manual) }
                Row(
                    Modifier.fillMaxWidth().clickable { toggleManual(!manual) }.padding(top = 2.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    // 标签是「手动摆位：说明」：冒号前面当标题，后面小字
                    val label = m?.optString("label") ?: "手动摆位"
                    Column(Modifier.weight(1f).padding(end = 12.dp)) {
                        Text(label.substringBefore("："), style = MaterialTheme.typography.bodyMedium)
                        if (label.contains("：")) {
                            Text(label.substringAfter("："), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                    Switch(checked = manual, onCheckedChange = { toggleManual(it) })
                }
            }
        }
        item {
            // 降噪和输出增益放在一起：都是随手要调的。输出增益只管实时听到的音量（文件转换不用它），所以不放在「输出」组里
            Section {
                spec.optJSONObject("denoise")?.let { SliderItem(it, p, true) }
                outGain?.let {
                    SliderItem(it, p, true)
                    Text(
                        "只在实时 / 试听时起作用（相当于耳机音量）；转换文件不用它",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
        item {
            Text(
                "全部参数",
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(start = 4.dp, top = 8.dp, bottom = 2.dp),
            )
        }
        items(groups, key = { it.optString("id") }) { g -> Group(g, p, tele) }
        item {
            Section("配置文件", modifier = Modifier.padding(top = 4.dp)) {
                Text(
                    "和电脑版通用（.ron），电脑版独有的设置会原样保留",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 2.dp, bottom = 6.dp),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = onImport, modifier = Modifier.weight(1f)) {
                        Ic(AppIcons.Folder, size = 18.dp)
                        Spacer(Modifier.width(6.dp))
                        Text("导入配置")
                    }
                    OutlinedButton(onClick = onExport, modifier = Modifier.weight(1f)) {
                        Ic(AppIcons.Save, size = 18.dp)
                        Spacer(Modifier.width(6.dp))
                        Text("导出配置")
                    }
                }
            }
        }
        footer()
        item { ProjectLinks(Modifier.padding(top = 6.dp)) }
        item { Spacer(Modifier.height(32.dp)) }
    }
}

/** 输出增益的参数名：效果面板里放在降噪下面，不在「输出」组里重复。 */
private const val OUT_GAIN = "out_gain_db"

/** 项目的 GitHub 和研发人的 B 站主页（同电脑版右下角）：GitHub 用系统浏览器打开，B 站优先唤起 B 站 App（见 [AppLinks]）。 */
@Composable
fun ProjectLinks(modifier: Modifier = Modifier) {
    val ctx = LocalContext.current
    Section("关于", modifier) {
        LinkRow(AppIcons.Star, "给个star", "GitHub · 觉得好用就点个 Star") { AppLinks.start(ctx, AppLinks.GITHUB_URL) }
        LinkRow(AppIcons.Play, "随意Official", "研发人 B站主页 · 装了 B 站 App 时直接在 App 里打开") { AppLinks.openBilibili(ctx) }
    }
}

@Composable
private fun LinkRow(icon: androidx.compose.ui.graphics.vector.ImageVector, title: String, sub: String, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clip(MaterialTheme.shapes.small).clickable(onClick = onClick).padding(vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Ic(icon, tint = MaterialTheme.colorScheme.primary)
        Column(Modifier.padding(start = 14.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Bold, color = MaterialTheme.colorScheme.primary)
            Text(sub, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
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
    val turn by animateFloatAsState(if (open) 180f else 0f, label = "chevron")
    Card(
        Modifier.fillMaxWidth().padding(vertical = 3.dp),
        shape = MaterialTheme.shapes.large,
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
    ) {
        Row(
            Modifier.fillMaxWidth().clickable { open = !open }.padding(horizontal = 14.dp, vertical = 11.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(SpecUi.groupTitle(g, p), style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            Ic(AppIcons.ExpandMore, Modifier.rotate(turn), size = 24.dp, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (open) {
            Column(Modifier.padding(start = 14.dp, end = 14.dp, bottom = 10.dp).alpha(if (enabled) 1f else 0.45f)) {
                val actions = SpecUi.items(g.optJSONArray("actions")).filter { SpecUi.cond(it.optJSONObject("show_if"), p) }
                if (actions.isNotEmpty()) {
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        for (a in actions) FilledTonalButton(onClick = { Settings.action(a.optString("action")) }) { Text(a.optString("label")) }
                    }
                }
                for (it in SpecUi.items(g.optJSONArray("items"))) {
                    if (!SpecUi.cond(it.optJSONObject("show_if"), p) || it.optString("key") == OUT_GAIN) continue
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
            Row(Modifier.weight(1f).padding(end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(item.optString("label"), style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f, fill = false))
                if (t.isNotEmpty()) Ic(AppIcons.Info, Modifier.padding(start = 4.dp), size = 15.dp, tint = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (value != null) Text(value, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
        }
        if (tip && t.isNotEmpty()) TipText(t)
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
    Row(Modifier.fillMaxWidth().padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Label(item, null, Modifier.weight(1f).padding(end = 12.dp))
        Switch(checked = on, onCheckedChange = { Settings.setBool(key, it) }, enabled = enabled)
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
                OutlinedButton(onClick = { open = true }, enabled = enabled) {
                    Text(label)
                    Ic(AppIcons.ExpandMore, Modifier.padding(start = 4.dp), size = 18.dp)
                }
                DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                    for (o in opts) DropdownMenuItem(text = { Text(o.optString("label")) }, onClick = { open = false; choose(o) })
                }
            }
        } else {
            Label(item, null)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (o in opts) {
                    val sel = o.opt("value")?.toString() == cur
                    FilterChip(
                        selected = sel,
                        onClick = { choose(o) },
                        label = { Text(o.optString("label")) },
                        enabled = enabled,
                        leadingIcon = if (sel) ({ Ic(AppIcons.Check, size = 18.dp) }) else null,
                    )
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
        Stepper(v.toString(), enabled, onMinus = { Settings.set(key, (v - 1).coerceAtLeast(0).toString()) }, onPlus = { Settings.set(key, (v + 1).toString()) })
    }
}

/** 标签下面展开的说明。 */
@Composable
fun TipText(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.fillMaxWidth().padding(top = 4.dp, bottom = 2.dp)
            .background(MaterialTheme.colorScheme.surfaceContainerHighest, MaterialTheme.shapes.small)
            .padding(horizontal = 10.dp, vertical = 8.dp),
    )
}

/** − 数值 + 的步进器。 */
@Composable
fun Stepper(text: String, enabled: Boolean, onMinus: () -> Unit, onPlus: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.background(MaterialTheme.colorScheme.surfaceContainerHighest, CircleShape),
    ) {
        IconButton(onClick = onMinus, enabled = enabled, modifier = Modifier.size(36.dp)) { Text("−", style = MaterialTheme.typography.titleMedium) }
        Text(text, style = MaterialTheme.typography.labelLarge, textAlign = TextAlign.Center, modifier = Modifier.widthIn(min = 40.dp))
        IconButton(onClick = onPlus, enabled = enabled, modifier = Modifier.size(36.dp)) { Text("+", style = MaterialTheme.typography.titleMedium) }
    }
}
