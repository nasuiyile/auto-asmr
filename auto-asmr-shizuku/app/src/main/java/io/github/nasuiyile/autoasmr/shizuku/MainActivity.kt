package io.github.nasuiyile.autoasmr.shizuku

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import java.io.File
import java.util.Locale

/**
 * 主界面：一页里放下全部东西——服务进程（启动、拦截开关、退出）、后台运行（悬浮球、电池优化、厂商设置）、
 * 效果参数（和安卓版 auto-asmr-android 同一个面板：原声开关、风格、位置图、降噪、各组参数、导入导出配置）、日志。
 *
 * 也可以用 adb 驱动（测试用）：
 *   am start -n io.github.nasuiyile.autoasmr.shizuku/.MainActivity --es launch shizuku|root
 * 其它参数：--es intercept 1|0；--es dry 1|0；--es ball 1|0；--es exit 1；--ei style 0-3；--es action <按钮>；
 * --es params '<RON>'；--es import <文件名> / --es export <文件名>（在 /sdcard/Android/data/<包名>/files/ 里）。
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        handle(intent)
        // 从别的 App「用其他应用打开」.ron 配置文件启动的（进程被系统回收后重建时不再弹一次）
        if (savedInstanceState == null && SharedImport.isOpenFile(intent)) SharedImport.handle(this, intent)
        setContent {
            MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
                Surface(Modifier.fillMaxSize()) { Screen() }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (SharedImport.isOpenFile(intent)) SharedImport.handle(this, intent)
        handle(intent)
    }

    override fun onResume() {
        super.onResume()
        Controller.uiVisible = true
        resumed++ // 从系统设置回来：重新看悬浮窗权限、电池优化
        InterceptService.instance?.refreshBall()
    }

    override fun onPause() {
        Controller.uiVisible = false
        super.onPause()
    }

    /** adb 驱动的参数（见类注释）。 */
    private fun handle(intent: Intent) {
        when (intent.getStringExtra("launch")) {
            "shizuku" -> Controller.startShizuku()
            "root" -> Controller.startRoot()
        }
        intent.getStringExtra("intercept")?.let { Controller.setIntercept(it == "1") }
        intent.getStringExtra("dry")?.let { Settings.setDry(it == "1") }
        intent.getStringExtra("ball")?.let {
            KeepAlive.setBallWanted(this, it == "1")
            InterceptService.instance?.refreshBall()
        }
        if (intent.getStringExtra("exit") == "1") Controller.exit()
        listOf("style_ear", "style_ku100", "style_earclean", "style_breath")
            .getOrNull(intent.getIntExtra("style", -1))?.let { Settings.action(it) }
        intent.getStringExtra("params")?.let { Settings.setRon(it) }
        intent.getStringExtra("action")?.let { Settings.action(it) }
        val dir = getExternalFilesDir(null)
        intent.getStringExtra("import")?.let { name ->
            val f = File(dir, name)
            Settings.importText(f.readText()) { ok -> Log.i(TAG, "导入 ${f.path}：${if (ok) "成功" else "失败"}") }
        }
        intent.getStringExtra("export")?.let { name ->
            val text = Settings.exportText()
            if (text != null) File(dir, name).writeText(text)
            Log.i(TAG, "导出 → ${File(dir, name).path}：${if (text != null) "成功" else "失败"}")
        }
    }

    companion object {
        const val TAG = "AsmrMain"
        var resumed by mutableIntStateOf(0)
    }
}

/** 状态一行（效果面板的标题下面）。 */
fun statusLine(): String {
    val c = Controller
    val tele = c.poll.tele
    return when {
        c.phase == Controller.Phase.STARTING -> "正在启动服务进程…"
        !c.connected -> "服务进程没在运行：先在上面启动"
        c.poll.fatal != null -> "服务进程出错：${c.poll.fatal}"
        !c.poll.running -> "已暂停拦截"
        tele.isEmpty() || tele[0] <= 0f -> if (c.poll.rain) "雨声 · 其他 App 没在出声" else "其他 App 没在出声"
        c.poll.dry -> String.format(Locale.ROOT, "原声（不加效果）· 延迟约 %.0f ms", tele[6])
        else -> String.format(
            Locale.ROOT, "正在处理其他 App 的声音 · 延迟约 %.0f ms · 占用 %.1f%%%s",
            tele[6], tele[5], if (tele[10] > 0f) " · 引擎出错，已改原声" else "",
        )
    }
}

@Composable
private fun Screen() {
    val ctx = LocalContext.current
    LaunchedEffect(Settings.message) {
        Settings.message?.let {
            Toast.makeText(ctx, it, Toast.LENGTH_SHORT).show()
            Settings.message = null
        }
    }
    LaunchedEffect(Controller.message) {
        Controller.message?.let {
            Toast.makeText(ctx, it, Toast.LENGTH_LONG).show()
            Controller.message = null
        }
    }
    // 配置文件：系统的文件选择器（和电脑版、安卓版的 .ron 通用）
    val exportLauncher = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        if (uri != null) {
            val text = Settings.exportText()
            val ok = text != null && runCatching { ctx.contentResolver.openOutputStream(uri)?.use { it.write(text.toByteArray()) } }.isSuccess
            Settings.message = if (ok) "配置已导出" else "导出失败"
        }
    }
    val importLauncher = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            val text = runCatching { ctx.contentResolver.openInputStream(uri)?.use { it.readBytes().toString(Charsets.UTF_8) } }.getOrNull()
            if (text == null) Settings.message = "读不了这个文件" else Settings.importText(text) {}
        }
    }
    // 第一次连上服务进程时请求通知权限（Android 13 起）：通知栏控制要用；拒绝了也照常工作
    val notifPerm = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}
    LaunchedEffect(Controller.connected) {
        if (Controller.connected && Build.VERSION.SDK_INT >= 33 && ctx.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            val prefs = ctx.getSharedPreferences("ui", 0)
            if (!prefs.getBoolean("asked_notifications", false)) {
                prefs.edit().putBoolean("asked_notifications", true).apply()
                notifPerm.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        }
    }

    EffectPanel(
        tele = Controller.poll.tele,
        status = statusLine(),
        onImport = { importLauncher.launch(arrayOf("*/*")) },
        onExport = { exportLauncher.launch("auto-asmr-export.ron") },
        modifier = Modifier.safeDrawingPadding(),
        header = {
            item { Title() }
            item { ServiceCard() }
            item { KeepAliveCard() }
            item { HorizontalDivider(Modifier.padding(vertical = 8.dp)) }
        },
        footer = { item { LogCard() } },
    )

    // 别的 App 传来的 .ron 文件：确认后导入
    SharedImport.pending?.let { f ->
        if (f.ok) {
            AlertDialog(
                onDismissRequest = { SharedImport.pending = null },
                title = { Text("导入 auto-asmr 配置？") },
                text = {
                    Text("「${f.name}」\n\n${f.info}\n\n导入后，当前的效果设置会换成这个文件里的（电脑版、安卓版导出的文件都行）。")
                },
                confirmButton = { TextButton(onClick = { SharedImport.confirm() }) { Text("导入") } },
                dismissButton = { TextButton(onClick = { SharedImport.pending = null }) { Text("取消") } },
            )
        } else {
            AlertDialog(
                onDismissRequest = { SharedImport.pending = null },
                title = { Text("不是 auto-asmr 的配置文件") },
                text = { Text("「${f.name}」\n\n${f.info}") },
                confirmButton = { TextButton(onClick = { SharedImport.pending = null }) { Text("知道了") } },
            )
        }
    }
}

@Composable
private fun Title() {
    Column(Modifier.padding(top = 12.dp, bottom = 4.dp)) {
        Text("global-asmr", style = MaterialTheme.typography.headlineSmall)
        Text(
            "把其他 App 播放的声音截下来，用 auto-asmr 引擎做成 KU100 双耳 ASMR 效果再放出来。需要 Shizuku 或 root，戴耳机听。版本 ${BuildConfig.VERSION_NAME}",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun SmallNote(text: String) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun ServiceCard() {
    val c = Controller
    val p = c.poll
    Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Column(Modifier.padding(12.dp)) {
            Text("服务进程", style = MaterialTheme.typography.titleSmall)
            SmallNote("Shizuku：${c.shizuku}")
            SmallNote(
                when (c.phase) {
                    Controller.Phase.IDLE -> "服务进程：没在运行"
                    Controller.Phase.STARTING -> "服务进程：正在启动（通过 ${c.how?.label}）…"
                    Controller.Phase.CONNECTED -> "服务进程：已连上（通过 ${c.how?.label}）"
                },
            )
            if (c.connected) c.hello?.let { SmallNote(it) }
            p.fatal?.let {
                Text(
                    "出错：$it",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onErrorContainer,
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp).background(MaterialTheme.colorScheme.errorContainer).padding(6.dp),
                )
            }
            when (c.phase) {
                Controller.Phase.IDLE -> Row(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { c.startShizuku() }) { Text("用 Shizuku 启动") }
                    OutlinedButton(onClick = { c.startRoot() }) { Text("用 root 启动") }
                }
                Controller.Phase.STARTING -> LinearProgressIndicator(Modifier.fillMaxWidth().padding(top = 8.dp))
                Controller.Phase.CONNECTED -> {
                    Row(Modifier.fillMaxWidth().padding(top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("拦截其他 App 的声音", style = MaterialTheme.typography.bodyMedium)
                            SmallNote(if (p.running) "原声不再直接进耳机，听到的是处理后的" else "已暂停：各 App 的声音照常直接放出")
                        }
                        Switch(checked = p.running, onCheckedChange = { c.setIntercept(it) })
                    }
                    if (p.running) {
                        SmallNote(
                            String.format(
                                Locale.ROOT, "输入峰值 %.2f · 输出峰值 %.2f · 断流 %d · 输出缓冲 %.0f ms",
                                p.inPeak, p.outPeak, p.underruns, p.outBuf,
                            ),
                        )
                    }
                    Row(Modifier.padding(top = 4.dp)) {
                        OutlinedButton(onClick = { c.exit() }) { Text("退出服务") }
                    }
                }
            }
            Spacer(Modifier.height(4.dp))
            SmallNote(
                if (c.how == Controller.How.ROOT) {
                    "root 启动的服务进程跟着 App 走：App 被杀时它也退出。想让它常驻，用 Shizuku（Shizuku 本身也可以用 root 启动）。"
                } else {
                    "Shizuku 启动的服务进程是常驻的：App 被系统清掉后声音照常处理，重新打开 App 会自动连回去。"
                },
            )
        }
    }
}

@Composable
private fun KeepAliveCard() {
    val ctx = LocalContext.current
    val tick = MainActivity.resumed // 从设置页回来时重新读下面几项
    val canOverlay = remember(tick) { KeepAlive.canOverlay(ctx) }
    val battery = remember(tick) { KeepAlive.ignoringBattery(ctx) }
    var ball by remember { mutableStateOf(KeepAlive.ballWanted(ctx)) }
    var open by rememberSaveable { mutableStateOf(!battery || !ball) }
    Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Row(
            Modifier.fillMaxWidth().clickable { open = !open }.padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("后台运行（防止被杀）", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            Text(if (open) "▲" else "▼", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (!open) return@Card
        Column(Modifier.padding(start = 12.dp, end = 12.dp, bottom = 10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("悬浮球", style = MaterialTheme.typography.bodyMedium)
                    SmallNote(
                        if (ball && !canOverlay) "还没有「显示在其他应用上层」的权限，点开关去授权"
                        else "点一下切换效果 / 原声，长按打开 App，可拖动。有悬浮窗时系统很少清掉 App",
                    )
                }
                Switch(checked = ball && canOverlay, onCheckedChange = { on ->
                    ball = on
                    KeepAlive.setBallWanted(ctx, on)
                    if (on && !canOverlay) runCatching { ctx.startActivity(KeepAlive.overlayIntent(ctx)) }
                    InterceptService.instance?.refreshBall()
                })
            }
            Row(Modifier.padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("忽略电池优化", style = MaterialTheme.typography.bodyMedium)
                    SmallNote(if (battery) "已允许" else "没允许：省电时系统可能限制或清掉后台的 App")
                }
                if (!battery) TextButton(onClick = { runCatching { ctx.startActivity(KeepAlive.batteryIntent(ctx)) } }) { Text("去允许") }
            }
            Row(Modifier.padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("系统的后台管理", style = MaterialTheme.typography.bodyMedium)
                    SmallNote(KeepAlive.vendorHint())
                }
                TextButton(onClick = { runCatching { ctx.startActivity(KeepAlive.appDetailsIntent(ctx)) } }) { Text("应用设置") }
            }
        }
    }
}

@Composable
private fun LogCard() {
    var open by rememberSaveable { mutableStateOf(false) }
    Card(Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
        Row(
            Modifier.fillMaxWidth().clickable { open = !open }.padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("日志", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            Text(if (open) "▲" else "▼", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (open) {
            SelectionContainer {
                Text(
                    Controller.logs.takeLast(60).joinToString("\n").ifEmpty { "（还没有）" },
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    modifier = Modifier.padding(start = 12.dp, end = 12.dp, bottom = 10.dp),
                )
            }
        }
    }
}
