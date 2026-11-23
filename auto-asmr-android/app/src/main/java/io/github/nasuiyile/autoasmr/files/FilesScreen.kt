package io.github.nasuiyile.autoasmr.files

import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
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
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.nasuiyile.autoasmr.EffectPanel
import io.github.nasuiyile.autoasmr.AppIcons
import io.github.nasuiyile.autoasmr.Ic
import io.github.nasuiyile.autoasmr.PageHeader
import io.github.nasuiyile.autoasmr.HelpItem
import io.github.nasuiyile.autoasmr.Section
import io.github.nasuiyile.autoasmr.Stepper
import io.github.nasuiyile.autoasmr.TipText
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.IconButton
import androidx.compose.material3.Switch
import io.github.nasuiyile.autoasmr.Settings
import io.github.nasuiyile.autoasmr.statusLine
import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale
import kotlin.math.roundToInt

/** 位置预设（同共用库 mix.rs 的 PRESETS 顺序）。 */
private val PRESET_NAMES = listOf("左耳", "右耳", "正前", "正后", "左前", "右前", "左后", "右后")

private fun playbackTime(s: Double): String {
    val t = s.coerceAtLeast(0.0).toLong()
    return "${t / 60}:${(t % 60).toString().padStart(2, '0')}"
}

private fun JSONArray?.floats(): FloatArray = if (this == null) FloatArray(0) else FloatArray(length()) { optDouble(it).toFloat() }

/**
 * 文件转换页，照电脑版「文件转换」页：音频列表（添加文件 / 文件夹、选中试听、移除）、多音频合成（位置、音量、排布、
 * 自动平衡、自动对齐）、左右双人、输出位置、进度和完成说明、声道 / 位深 / 起点 / 时长 / 目标响度 / 同时转换、
 * 试听 / 停止 / 转换 / 取消、试听位置（播放条），下面是和其它页面同一个效果面板（边听边调）。
 * 逻辑都在 Rust（rust/src/files.rs，照搬桌面版），这里只负责显示和把操作交过去（[FileSession]）。
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun FilesScreen(onImport: () -> Unit, onExport: () -> Unit) {
    val ctx = LocalContext.current
    val st = FileSession.st
    DisposableEffect(Unit) {
        FileSession.setVisible(true)
        onDispose { FileSession.setVisible(false) }
    }
    LaunchedEffect(FileSession.message) {
        FileSession.message?.let {
            Toast.makeText(ctx, it, Toast.LENGTH_LONG).show()
            FileSession.message = null
        }
    }
    val addFiles = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris -> if (uris.isNotEmpty()) FileSession.addUris(uris) }
    val addFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri -> if (uri != null) FileSession.addTree(uri) }
    val outFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri -> if (uri != null) FileSession.chooseOutTree(uri) }
    var saveAsKey by remember { mutableStateOf<String?>(null) }
    val saveAs = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("audio/wav")) { uri -> saveAsKey?.let { if (uri != null) FileSession.setSaveAs(it, uri) } }

    val busy = st.optBoolean("busy")
    val multi = st.optBoolean("multi")
    val mixing = st.optBoolean("mixing")
    val tele = st.optJSONArray("tele").floats()
    val status = st.optString("status").ifEmpty { if (st.optBoolean("previewing")) statusLine(tele, null, false) else "添加音频文件后可以试听、转换" }

    EffectPanel(
        tele = tele,
        status = status,
        onImport = onImport,
        onExport = onExport,
        modifier = Modifier.safeDrawingPadding(),
        showDry = false,
        map = if (mixing) ({ MixMap(st) }) else null,
        header = {
            item { PageHeader("文件转换", help = FILES_HELP) }
            item { FileList(st, busy, multi, onAddFiles = { addFiles.launch(arrayOf("audio/*", "video/*", "application/ogg", "application/octet-stream")) }, onAddFolder = { addFolder.launch(null) }) }
            item {
                Section("多人声音") {
                    MixBlock(st, busy)
                    DuoBlock(st, busy)
                }
            }
            item {
                Section("试听与转换") {
                    Buttons(st, busy, multi)
                    PreviewBar(st)
                    Progress(st)
                }
            }
            item {
                Section("输出设置") {
                    OutputRow(st, busy, onSaveAs = { key, name ->
                        saveAsKey = key
                        saveAs.launch(name)
                    }, onOutFolder = { outFolder.launch(null) })
                    Options(st, busy, multi)
                }
            }
        },
    )
}

/** 文件转换页的说明：收在标题右边的「?」里。 */
private val FILES_HELP = listOf(
    HelpItem("文件转换是什么", "把音频文件转成 KU100 双耳 ASMR 效果的 WAV，可以先试听、边听边调参数；支持批量、多音频合成、左右双人（同电脑版）。"),
    HelpItem("添加文件", "「添加文件」可以一次多选；「添加文件夹」加入文件夹里的全部音频（不含子文件夹，跳过已转换的 _asmr 文件）。支持 wav / m4a / mp3 / flac / ogg 等常见格式。"),
    HelpItem("多人声音", "多音频合成：几个音频各在一个位置，同时播放。左右双人：同一个音频当成左右两个人，第 2 个声音晚一点。"),
    HelpItem("音量", "转换出来的文件精确归一化到「目标响度」；效果参数里的「输出增益」只调试听时的音量，不影响文件。"),
)

@Composable
private fun Note(text: String, color: Color = MaterialTheme.colorScheme.onSurfaceVariant) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = color)
}

/** 标签，旁边的 ⓘ 点开说明（同效果面板）。 */
@Composable
private fun TipLabel(label: String, tip: String, modifier: Modifier = Modifier) {
    var open by remember { mutableStateOf(false) }
    Column(modifier) {
        Row(Modifier.clickable { open = !open }, verticalAlignment = Alignment.CenterVertically) {
            Text(label, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f, fill = false))
            Ic(AppIcons.Info, Modifier.padding(start = 4.dp), size = 15.dp, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (open) TipText(tip)
    }
}

@Composable
private fun Badge(k: Int, selected: Boolean, onClick: (() -> Unit)? = null) {
    val col = VOICE_COLORS[k % VOICE_COLORS.size]
    Box(
        Modifier.size(22.dp).clip(CircleShape).background(col).let { if (onClick != null) it.clickable { onClick() } else it },
        contentAlignment = Alignment.Center,
    ) {
        Text("${k + 1}", style = MaterialTheme.typography.labelSmall, color = Color.White)
    }
    if (selected) Spacer(Modifier.width(2.dp))
}

@Composable
private fun FileList(st: JSONObject, busy: Boolean, multi: Boolean, onAddFiles: () -> Unit, onAddFolder: () -> Unit) {
    val files = st.optJSONArray("files") ?: JSONArray()
    val n = files.length()
    var detail by remember { mutableStateOf<Pair<String, String>?>(null) }
    Section(
        "音频文件",
        subtitle = if (n > 0) "$n 个" else null,
        actions = { if (n > 0) TextButton(onClick = { FileSession.cmd("clear") }, enabled = !busy) { Text("清空") } },
    ) {
        Column {
            Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilledTonalButton(onClick = onAddFiles, enabled = !busy, modifier = Modifier.weight(1f), contentPadding = PaddingValues(horizontal = 12.dp)) {
                    Ic(AppIcons.Add, size = 18.dp)
                    Spacer(Modifier.width(6.dp))
                    Text("添加文件")
                }
                FilledTonalButton(onClick = onAddFolder, enabled = !busy, modifier = Modifier.weight(1f), contentPadding = PaddingValues(horizontal = 12.dp)) {
                    Ic(AppIcons.Folder, size = 18.dp)
                    Spacer(Modifier.width(6.dp))
                    Text("添加文件夹")
                }
            }
            if (n == 0) Note("支持 wav / m4a / mp3 / flac / ogg 等常见格式")
            val voices = st.optJSONArray("voices")
            val offsets = st.optJSONArray("follow_offsets")
            val follow = Settings.params.optBoolean("mix_follow")
            val selected = st.optInt("selected", -1).takeIf { st.has("selected") && !st.isNull("selected") }
            val mixSel = st.optInt("mix_sel", -1).takeIf { st.has("mix_sel") && !st.isNull("mix_sel") }
            for (i in 0 until n) {
                val f = files.getJSONObject(i)
                val inMix = f.optBoolean("in_mix")
                val sel = if (inMix) mixSel == i else selected == i
                Row(
                    Modifier.fillMaxWidth().padding(vertical = 2.dp).clip(MaterialTheme.shapes.small)
                        .background(if (sel) MaterialTheme.colorScheme.secondaryContainer else Color.Transparent)
                        .clickable(enabled = !multi || inMix) { FileSession.cmd("select", "index" to i) }
                        .padding(start = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (inMix) Badge(i, sel)
                    Text(
                        f.optString("name"),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        style = if (sel) MaterialTheme.typography.titleSmall else MaterialTheme.typography.bodyMedium,
                        color = if (multi && !inMix) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
                        modifier = Modifier.weight(1f).padding(start = 6.dp, top = 10.dp, bottom = 10.dp),
                    )
                    when {
                        inMix -> VoiceControls(i, voices?.optJSONObject(i), busy, if (follow) offsets?.optDouble(i)?.toFloat() else null)
                        multi -> Note("不参与合成")
                        else -> StatusCell(f.optJSONObject("status")) { title, body -> detail = title to body }
                    }
                    IconButton(onClick = { FileSession.cmd("remove", "index" to i) }, enabled = !busy, modifier = Modifier.size(40.dp)) {
                        Ic(AppIcons.Close, size = 18.dp, tint = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
            if (multi && n > 4) Note("合成最多用前 4 个音频，其余的不参与")
        }
    }
    detail?.let { (title, body) ->
        AlertDialog(
            onDismissRequest = { detail = null },
            title = { Text(title) },
            text = { Text(body) },
            confirmButton = { TextButton(onClick = { detail = null }) { Text("知道了") } },
        )
    }
}

/** 列表右边的状态：等待 / 进度 / 完成 / 失败（点完成、失败看输出位置或原因）。 */
@Composable
private fun StatusCell(s: JSONObject?, onDetail: (String, String) -> Unit) {
    if (s == null) return
    Box(Modifier.width(110.dp)) {
        when (s.optString("kind")) {
            "waiting" -> Note("等待中")
            "running" -> Column {
                LinearProgressIndicator(progress = { s.optDouble("f").toFloat() }, modifier = Modifier.fillMaxWidth())
                Note(String.format(Locale.ROOT, "%s %.0f%%", s.optString("stage"), s.optDouble("f") * 100))
            }
            "done" -> Text(
                String.format(Locale.ROOT, "✓ 完成 · %.0f s", s.optDouble("elapsed")),
                style = MaterialTheme.typography.bodySmall,
                color = Color(0xFF5ABE78),
                modifier = Modifier.clickable {
                    val note = s.optString("channel_note").let { if (it.isEmpty()) "" else "\n$it" }
                    onDetail("已保存", String.format(Locale.ROOT, "%s\n%.1f s，%.1f LUFS，真峰值 %.1f dBTP%s", s.optString("out"), s.optDouble("seconds"), s.optDouble("lufs"), s.optDouble("peak"), note))
                },
            )
            "failed" -> Text("× 失败", style = MaterialTheme.typography.bodySmall, color = Color(0xFFE65A3C), modifier = Modifier.clickable { onDetail("失败", s.optString("error")) })
        }
    }
}

/** 一个合成音源的位置下拉框和音量（一起移动时位置由走位决定，显示它比 1 号转过的角度）。 */
@Composable
private fun VoiceControls(k: Int, v: JSONObject?, busy: Boolean, followOff: Float?) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        if (followOff != null) {
            Note(if (k == 0) "领头" else String.format(Locale.ROOT, "+%.0f°", followOff), MaterialTheme.colorScheme.onSurfaceVariant)
        } else {
            var open by remember { mutableStateOf(false) }
            Box {
                TextButton(onClick = { open = true }, enabled = !busy) { Text(v?.optString("name") ?: "—", maxLines = 1) }
                DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                    PRESET_NAMES.forEachIndexed { p, name ->
                        DropdownMenuItem(text = { Text((if (v?.optInt("preset", -1) == p && !v.isNull("preset")) "✓ " else "") + name) }, onClick = {
                            open = false
                            FileSession.cmd("voice", "index" to k, "preset" to p)
                        })
                    }
                }
            }
        }
        val g = v?.optDouble("gain") ?: 0.0
        TextButton(onClick = { FileSession.cmd("voice", "index" to k, "gain" to (g - 1.0).coerceAtLeast(-30.0)) }, enabled = !busy, modifier = Modifier.width(36.dp)) { Text("−") }
        Text(String.format(Locale.ROOT, "%+.0f dB", g), style = MaterialTheme.typography.bodySmall)
        TextButton(onClick = { FileSession.cmd("voice", "index" to k, "gain" to (g + 1.0).coerceAtMost(12.0)) }, enabled = !busy, modifier = Modifier.width(36.dp)) { Text("+") }
    }
}

/** 多音频合成开关、排布和自动平衡、自动对齐。 */
@Composable
private fun MixBlock(st: JSONObject, busy: Boolean) {
    val p = Settings.params
    val on = p.optBoolean("mix")
    val n = st.optJSONArray("files")?.length() ?: 0
    Column(Modifier.padding(top = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TipLabel(
                "多音频合成：几个音频各在一个位置，同时播放",
                "勾上后，列表里的前 4 个音频各放一个位置（默认一个在左耳、一个在右耳，再多就放正前、正后），「试听」时一起播放，「合成并保存」输出一个文件。下面的「排布」可以选各自固定（位置在列表里选或在位置图上拖）或一起移动（按「左右移动」的方式一起走，彼此始终相对）。各音频先自动对齐到一样响，列表里的 dB 是在这之上的加减",
                Modifier.weight(1f).padding(end = 12.dp),
            )
            Switch(checked = on, onCheckedChange = { Settings.setBool("mix", it) }, enabled = !busy)
        }
        if (on) Note(if (n < 2) "再添加一个音频" else if (n > 4) "只用前 4 个" else "最多 4 个", MaterialTheme.colorScheme.onSurfaceVariant)
        if (on) LayoutRow(busy)
        if (on) {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 6.dp)) {
                TipLabel(
                    "自动对齐：同一首歌的几个版本，从一起开始唱的地方叠加",
                    "同一首歌的几个版本（比如一段前奏长、一段前奏短，或者不同的编曲）：按旋律、和弦和节奏自动找到它们一起开始唱的地方，从那里叠加；对不上的开头和结尾裁掉（裁开的地方淡入淡出），下面会写出每段从哪里开始用、裁掉多少。试听和合成并保存用的是同一份对齐结果；「从第几秒 / 处理多少秒」和播放条都按对齐之后的时间算。几段对不上（不像同一首歌）时不裁剪，按原样合成",
                    Modifier.weight(1f).padding(end = 12.dp),
                )
                Switch(checked = p.optBoolean("mix_align"), onCheckedChange = { Settings.setBool("mix_align", it) }, enabled = !busy)
            }
            val al = st.optJSONObject("align")
            if (al != null) {
                val warn = MaterialTheme.colorScheme.error
                when (al.optString("state")) {
                    "ok" -> {
                        val lines = al.optJSONArray("lines") ?: JSONArray()
                        for (i in 0 until lines.length()) {
                            val l = lines.optString(i)
                            Note(l, if (l.contains('!') || l.contains('！')) warn else MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                    "err" -> Note(al.optString("error"), warn)
                    else -> Note(al.optString("note"))
                }
            }
        }
    }
}

/** 排布（各自固定 / 一起移动）和自动平衡：多音频合成和左右双人共用。 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LayoutRow(busy: Boolean) {
    val p = Settings.params
    val follow = p.optBoolean("mix_follow")
    Column {
        FlowRow(verticalArrangement = Arrangement.Center, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            TipLabel("排布", "各自固定：每个声音固定在自己的位置（在列表里选，或在位置图上拖），只做忽远忽近、上下、自然晃动。一起移动：几个声音按「左右移动」的方式（穿梭、随机走位、环绕、固定、手动）一起走，彼此始终相隔 360°/个数：两个时正好在头的两侧相对，换边时一个从脸前、一个从脑后绕过去，不会撞在一起")
            FilterChip(selected = !follow, onClick = { Settings.setBool("mix_follow", false) }, label = { Text("各自固定") }, enabled = !busy)
            FilterChip(selected = follow, onClick = { Settings.setBool("mix_follow", true) }, label = { Text("一起移动") }, enabled = !busy)
        }
        val bal = p.optDouble("mix_balance", 0.0)
        var drag by remember { mutableStateOf<Float?>(null) }
        val shown = drag?.toDouble() ?: bal
        Row(verticalAlignment = Alignment.CenterVertically) {
            TipLabel(
                "自动平衡",
                "让几个声音每一段时间听起来都差不多一样响。合成本来就按整段平均响度对齐，但两个声音各自忽远忽近，常常一个退开了另一个正贴着耳朵，内容本身也有响有轻。这里按每个声音此刻听起来多响往平均拉，最多 ±15 dB，0.25 秒平滑；停顿里的声音不往上拉。100% = 一样响，50% = 差距减半，0 = 关闭。试听时实时生效，合成完成后的说明里有平衡前后的响度差",
                Modifier.weight(1f),
            )
            Text(if (shown < 0.5) "0 关" else "${shown.roundToInt()}%", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.primary)
        }
        Slider(value = (drag ?: bal.toFloat()) / 100f, onValueChange = { t ->
            drag = t * 100f
            Settings.setNumber("mix_balance", (t * 100f).roundToInt().toDouble())
        }, onValueChangeFinished = { drag = null }, enabled = !busy, modifier = Modifier.height(32.dp))
    }
}

/** 左右双人：一个音频当成两个人（多音频合成生效时不显示）。 */
@Composable
private fun DuoBlock(st: JSONObject, busy: Boolean) {
    if (st.optBoolean("multi")) return
    val p = Settings.params
    val ms = p.optDouble("duo_ms", 0.0)
    var drag by remember { mutableStateOf<Float?>(null) }
    val shown = drag?.toDouble() ?: ms
    val n = st.optJSONArray("files")?.length() ?: 0
    Column(Modifier.padding(top = 12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TipLabel(
                "左右双人",
                "把一个音频当成两个人：同一个声音一个在左耳、一个在右耳（和导入两个音频的多音频合成一样，各自忽远忽近），第 2 个整体晚这么多毫秒，听得出是两个人一前一后地说。30 ms 以下两边容易融成一个声音，50–120 ms 像两个人，150 ms 以上像回声。打开后下面可以改两个声音的位置、音量、排布和自动平衡；试听中拖这个滑条实时生效。输出比原音频长出这段延迟。列表里有几个音频时，试听的是选中的那个，「全部转换」时每个文件都这样做。0 = 关闭",
                Modifier.weight(1f),
            )
            Text(if (shown < 0.5) "0 关" else "${shown.roundToInt()} ms", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.primary)
        }
        Slider(value = ((drag ?: ms.toFloat()) / 300f).coerceIn(0f, 1f), onValueChange = { t ->
            drag = (t * 300f).roundToInt().toFloat()
            Settings.setNumber("duo_ms", drag!!.toDouble())
        }, onValueChangeFinished = { drag = null }, enabled = !busy, modifier = Modifier.height(32.dp))
        Note(if (ms <= 0.0) "同一个声音一左一右两个人说" else if (n > 1) "转换时每个文件都这样" else "50–120 ms 最像两个人")
        if (st.optBoolean("duo")) {
            val voices = st.optJSONArray("voices")
            val offsets = st.optJSONArray("follow_offsets")
            val follow = p.optBoolean("mix_follow")
            val mixSel = st.optInt("mix_sel", -1).takeIf { st.has("mix_sel") && !st.isNull("mix_sel") }
            for (k in 0 until 2) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Badge(k, mixSel == k) { FileSession.cmd("mix_sel", "index" to k) }
                    Text(if (k == 0) "  1 号：原声（准时）" else String.format(Locale.ROOT, "  2 号：晚 %.0f ms", ms), style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
                    VoiceControls(k, voices?.optJSONObject(k), busy, if (follow) offsets?.optDouble(k)?.toFloat() else null)
                }
            }
            if (!p.optBoolean("mix")) LayoutRow(busy)
        }
    }
}

/** 输出位置：合成是一个文件；一个文件时可以「输出到…」；多个文件时「输出文件夹…」。 */
@Composable
private fun OutputRow(st: JSONObject, busy: Boolean, onSaveAs: (String, String) -> Unit, onOutFolder: () -> Unit) {
    val files = st.optJSONArray("files") ?: JSONArray()
    val n = files.length()
    if (n == 0) return
    val multi = st.optBoolean("multi")
    val outName = st.optString("out_name")
    Column(Modifier.padding(top = 4.dp)) {
        if (multi || n == 1) {
            val key = if (multi) FileSession.MIX_KEY else files.getJSONObject(st.optInt("selected", 0).coerceIn(0, n - 1)).optString("path")
            val chosen = FileSession.saveAsName[key]
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedButton(onClick = { onSaveAs(key, outName) }, enabled = !busy) { Text("输出到…") }
                Text(chosen ?: outName, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f).padding(start = 8.dp))
                if (chosen != null) TextButton(onClick = { FileSession.setSaveAs(key, null) }, enabled = !busy) { Text("默认") }
            }
            if (chosen == null) Note("默认：${if (multi) "第一个音频" else "源文件"}所在的文件夹（添加文件夹时），否则「音乐/auto-asmr」")
        } else {
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedButton(onClick = onOutFolder, enabled = !busy) { Text("输出文件夹…") }
                Text(FileSession.outTreeName ?: "默认位置", style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f).padding(start = 8.dp))
                if (FileSession.outTree != null) TextButton(onClick = { FileSession.chooseOutTree(null) }, enabled = !busy) { Text("放回默认") }
            }
            Note(if (FileSession.outTree != null) "名字加 _asmr，重名时加 _2、_3" else "默认：各自源文件所在的文件夹（添加文件夹时），否则「音乐/auto-asmr」；名字加 _asmr")
        }
    }
}

/** 总进度和完成后的说明。 */
@Composable
private fun Progress(st: JSONObject) {
    Column(Modifier.padding(top = 4.dp)) {
        st.optJSONObject("job")?.let { j ->
            LinearProgressIndicator(progress = { j.optDouble("frac").toFloat() }, modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp))
            Note(j.optString("text"))
            if (j.optInt("n") > 1) Note("后台 ${j.optInt("workers")} 个线程并行，长的文件先处理")
        }
        val report = st.optString("report")
        if (report.isNotEmpty()) Text(report, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(vertical = 4.dp))
    }
}

/** 取哪个声道、位深、从第几秒、处理多少秒、目标响度、同时转换。 */
@Composable
private fun Options(st: JSONObject, busy: Boolean, multi: Boolean) {
    val p = Settings.params
    Column(Modifier.padding(top = 4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("取哪个声道", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
            val names = listOf("自动（更响的）", "左", "右", "混合")
            var open by remember { mutableStateOf(false) }
            Box {
                OutlinedButton(onClick = { open = true }) { Text(names[p.optInt("file_channel", 0).coerceIn(0, 3)]) }
                DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                    names.forEachIndexed { i, nm -> DropdownMenuItem(text = { Text(nm) }, onClick = { open = false; Settings.set("file_channel", i.toString()) }) }
                }
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("位深", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
            val bits = p.optInt("bits", 24)
            FilterChip(selected = bits == 24, onClick = { Settings.set("bits", "24") }, label = { Text("24 bit") })
            Spacer(Modifier.width(6.dp))
            FilterChip(selected = bits == 16, onClick = { Settings.set("bits", "16") }, label = { Text("16 bit") })
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            SecondsField("从第几秒", st.optDouble("start_s", 0.0), Modifier.weight(1f)) { FileSession.cmd("range", "start" to it) }
            SecondsField("处理多少秒（0 = 到结尾）", st.optDouble("dur_s", 0.0), Modifier.weight(1f)) { FileSession.cmd("range", "dur" to it) }
        }
        val lufs = p.optDouble("file_lufs", -16.0)
        var drag by remember { mutableStateOf<Float?>(null) }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("目标响度", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
            Text(String.format(Locale.ROOT, "%.0f LUFS", drag?.toDouble() ?: lufs), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.primary)
        }
        Slider(value = (((drag ?: lufs.toFloat()) + 24f) / 14f).coerceIn(0f, 1f), onValueChange = { t ->
            drag = (-24f + 14f * t).roundToInt().toFloat()
            Settings.setNumber("file_lufs", drag!!.toDouble())
        }, onValueChangeFinished = { drag = null }, modifier = Modifier.height(32.dp))
        if (!multi) {
            // 合成只输出一个文件（几个音源的引擎已经各用一个核并行），没有"同时转换几个文件"
            val cores = st.optInt("cores", 1)
            val n = st.optInt("threads", cores)
            Row(verticalAlignment = Alignment.CenterVertically) {
                TipLabel("同时转换", "批量转换时同时处理几个文件，每个文件占一个核。默认 = 本机逻辑核数 $cores。整段读进内存，长音频每小时约占 0.7 GB，内存紧张时调小", Modifier.weight(1f))
                Stepper(
                    "$n 个",
                    !busy,
                    onMinus = { val v = (n - 1).coerceAtLeast(1); Settings.set("jobs", (if (v == cores) 0 else v).toString()) },
                    onPlus = { val v = (n + 1).coerceAtMost(maxOf(cores, 64)); Settings.set("jobs", (if (v == cores) 0 else v).toString()) },
                )
                if (p.optInt("jobs", 0) != 0) TextButton(onClick = { Settings.set("jobs", "0") }, enabled = !busy) { Text("自动") }
            }
        }
    }
}

@Composable
private fun SecondsField(label: String, value: Double, modifier: Modifier, onSet: (Double) -> Unit) {
    var text by remember(value) { mutableStateOf(if (value == 0.0) "0" else String.format(Locale.ROOT, "%.0f", value)) }
    OutlinedTextField(
        value = text,
        onValueChange = { t ->
            text = t
            t.toDoubleOrNull()?.let { onSet(it.coerceIn(0.0, 36000.0)) }
        },
        label = { Text(label, maxLines = 1) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = modifier,
    )
}

/** 试听 / 停止 / 转换（合成并保存、全部转换并保存）/ 取消。 */
@Composable
private fun Buttons(st: JSONObject, busy: Boolean, multi: Boolean) {
    val n = st.optJSONArray("files")?.length() ?: 0
    val previewing = st.optBoolean("previewing")
    val label = when {
        multi -> "合成并保存（${minOf(n, 4)}）"
        n > 1 -> "全部转换并保存（$n）"
        else -> "转换并保存"
    }
    Row(Modifier.fillMaxWidth().padding(top = 10.dp, bottom = 4.dp), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        // 试听 / 停止合成一个按钮
        FilledTonalButton(
            onClick = { FileSession.cmd(if (previewing) "stop" else "preview") },
            enabled = previewing || st.optBoolean("can_preview"),
            contentPadding = PaddingValues(horizontal = 16.dp),
        ) {
            Ic(if (previewing) AppIcons.Stop else AppIcons.Play, size = 18.dp)
            Spacer(Modifier.width(6.dp))
            Text(if (previewing) "停止" else "试听")
        }
        if (busy) {
            OutlinedButton(onClick = { FileSession.cmd("cancel") }, modifier = Modifier.weight(1f)) { Text("取消转换") }
        } else {
            Button(onClick = { FileSession.convert() }, enabled = n > 0, modifier = Modifier.weight(1f), contentPadding = PaddingValues(horizontal = 12.dp)) {
                Ic(AppIcons.Save, size = 18.dp)
                Spacer(Modifier.width(6.dp))
                Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}

/** 试听位置：点击或拖动播放条，立即试听该位置（试听中直接跳过去）。 */
@Composable
private fun PreviewBar(st: JSONObject) {
    val pv = st.optJSONObject("preview") ?: return
    val len = pv.optDouble("len")
    var drag by remember { mutableStateOf<Float?>(null) }
    val pos = drag?.toDouble() ?: pv.optDouble("pos")
    Column {
        Note("${if (pv.optBoolean("aligned")) "试听位置（对齐后）" else "试听位置"}  ${playbackTime(pos)} / ${playbackTime(len)}", MaterialTheme.colorScheme.onSurface)
        Slider(
            value = if (len > 0) (pos / len).toFloat().coerceIn(0f, 1f) else 0f,
            onValueChange = { t ->
                if (drag == null) FileSession.cmd("drag", "on" to true)
                drag = (t * len).toFloat()
                FileSession.cmd("seek", "s" to t * len)
            },
            onValueChangeFinished = {
                drag = null
                FileSession.cmd("drag", "on" to false)
            },
            enabled = len > 0,
        )
    }
}
