package io.github.nasuiyile.autoasmr

import android.content.Intent
import android.os.Bundle
import android.util.Log
import android.view.ViewGroup
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import io.github.nasuiyile.autoasmr.files.FilesScreen
import io.github.nasuiyile.autoasmr.global.Controller
import io.github.nasuiyile.autoasmr.global.GlobalScreen
import io.github.nasuiyile.autoasmr.global.InterceptService
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import kotlinx.coroutines.delay
import java.io.File

/**
 * 主界面，两个页面：
 * - **网页**：浏览器（地址栏、标签页）+ 底栏（ASMR 开关、处理状态、标签页、效果面板、菜单）。没有 Shizuku 时用它听效果。
 * - **全局**（global/GlobalScreen.kt）：有 Shizuku 或 root 时处理任意 App 的声音。
 * - **文件转换**（files/FilesScreen.kt）：和电脑版的文件转换页一样，把音频文件转成双耳效果，可以试听。
 * 三个页面共用一份参数（[Settings]），顶上都有「网页 | 全局（Shizuku） | 文件转换」切换条（[ModeSwitch]）。
 * 第一次打开时，有 Shizuku 就进全局页，否则进网页；之后记住用户上次选的页面。
 *
 * 也可以用 adb 驱动（tools/hook_test.sh）：
 *   am start -n io.github.nasuiyile.autoasmr/.MainActivity --es url <网址>
 * 其它参数（都作用于当前标签页）：--ei mode 0-3（调试：直通 / ASMR / 左右互换 / 静音）；--ei style 0-3；
 * --es action <按钮>（style_ear / style_ku100 / style_earclean / style_breath / reset / bg_rain_night / bg_off）；
 * --es params '<RON>'；--ei rec <秒>（录下处理前后的声音）；--es ua desktop|mobile；--es mute 1|0；
 * --es import <文件名> / --es export <文件名>（在 /sdcard/Android/data/<包名>/files/ 里，导入 / 导出配置）。
 * 页面：--es page web|global|files。文件转换：--es files_add <文件名,…>（外部文件目录里的）；--es files_op '<JSON 命令>'。全局模式：--es launch shizuku|root；--es intercept 1|0；--es dry 1|0；--es exit 1。
 */
class MainActivity : ComponentActivity() {
    private lateinit var browser: Browser

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        WebViewCompat.getCurrentWebViewPackage(this)?.let { Log.i(TAG, "WebView 提供者：${it.packageName} ${it.versionName}") }
        browser = Browser(this)
        Playback.browser = browser
        browser.newTab(intent.getStringExtra("url") ?: Browser.HOME)
        handle(intent)
        // 从别的 App「用其他应用打开」.ron 配置文件启动的（进程被系统回收后重建时不再弹一次）
        if (savedInstanceState == null && SharedImport.isOpenFile(intent)) SharedImport.handle(this, intent)
        setContent {
            MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
                Surface(Modifier.fillMaxSize()) { Root(browser, ::record) }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (SharedImport.isOpenFile(intent)) SharedImport.handle(this, intent)
        intent.getStringExtra("url")?.let { u -> browser.current?.let { browser.open(it, u) } }
        handle(intent)
    }

    override fun onResume() {
        super.onResume()
        Controller.uiVisible = true
        InterceptService.instance?.refresh() // 从系统设置回来时可能刚打开了通知
    }

    override fun onPause() {
        Controller.uiVisible = false
        super.onPause()
    }

    /** adb 驱动的参数（见类注释）；通知栏打开 App 时带 page=global。 */
    private fun handle(intent: Intent) {
        when (intent.getStringExtra("page")) {
            "global" -> setPage(this, Page.GLOBAL)
            "web" -> setPage(this, Page.WEB)
            "files" -> setPage(this, Page.FILES)
        }
        // 文件转换页的测试入口（adb）：files_add = 外部文件目录里的文件名（逗号分隔），files_op = 一条命令（JSON）
        intent.getStringExtra("files_add")?.let { names ->
            setPage(this, Page.FILES)
            val dir = getExternalFilesDir(null)
            io.github.nasuiyile.autoasmr.files.FileSession.addPaths(names.split(',').map { File(dir, it.trim()).path })
        }
        intent.getStringExtra("files_op")?.let { io.github.nasuiyile.autoasmr.files.FileSession.cmdJson(it) }
        // 带网址打开的（adb、别的入口）：要看网页，切到网页页面
        if (intent.getStringExtra("url") != null) setPage(this, Page.WEB)
        when (intent.getStringExtra("launch")) {
            "shizuku" -> Controller.startShizuku()
            "root" -> Controller.startRoot()
        }
        intent.getStringExtra("intercept")?.let { Controller.setIntercept(it == "1") }
        intent.getStringExtra("dry")?.let { Settings.setDry(it == "1") }
        if (intent.getStringExtra("exit") == "1") Controller.exit()
        val tab = browser.current ?: return
        intent.getIntExtra("mode", -1).takeIf { it in 0..3 }?.let { NativeHook.setMode(it) }
        listOf("style_ear", "style_ku100", "style_earclean", "style_breath")
            .getOrNull(intent.getIntExtra("style", -1))?.let { Settings.action(it) }
        intent.getStringExtra("params")?.let { Settings.setRon(it) }
        intent.getStringExtra("action")?.let { Settings.action(it) }
        intent.getStringExtra("ua")?.let { browser.setDesktop(tab, it == "desktop") }
        intent.getStringExtra("mute")?.let {
            if (WebViewFeature.isFeatureSupported(WebViewFeature.MUTE_AUDIO)) WebViewCompat.setAudioMuted(tab.web, it == "1")
        }
        intent.getIntExtra("rec", 0).takeIf { it > 0 }?.let { record(it) }
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

    private fun record(seconds: Int) {
        val dir = getExternalFilesDir(null)?.path ?: filesDir.path
        val r = NativeHook.record(dir, seconds)
        Log.i(TAG, if (r == 0) "录音 $seconds 秒 → $dir" else "已经在录音了")
        Toast.makeText(this, if (r == 0) "录 $seconds 秒处理前后的声音 → $dir" else "已经在录音了", Toast.LENGTH_SHORT).show()
    }

    companion object {
        const val TAG = "AsmrHook"
    }
}

enum class Page { WEB, GLOBAL, FILES }

/** 当前页面；用户选过就记在 ui 偏好里（键 page），没选过时按有没有 Shizuku 决定（见 [Root]）。 */
var currentPage by mutableStateOf(Page.WEB)
    private set

fun setPage(ctx: android.content.Context, p: Page) {
    currentPage = p
    ctx.getSharedPreferences("ui", 0).edit().putString("page", p.name).apply()
}

/**
 * 页面顶上的切换条：网页模式（内嵌浏览器，谁都能用）/ 全局模式（Shizuku 或 root，处理任意 App 的声音）/
 * 文件转换（和电脑版的文件转换页一样）。
 */
@Composable
fun ModeSwitch(modifier: Modifier = Modifier) {
    val ctx = LocalContext.current
    SingleChoiceSegmentedButtonRow(modifier.fillMaxWidth()) {
        val pages = listOf(Page.WEB to "网页", Page.GLOBAL to "全局（Shizuku）", Page.FILES to "文件转换")
        pages.forEachIndexed { i, (p, label) ->
            SegmentedButton(
                selected = currentPage == p,
                onClick = { setPage(ctx, p) },
                shape = SegmentedButtonDefaults.itemShape(index = i, count = pages.size),
                label = { Text(label, maxLines = 1) },
            )
        }
    }
}

/** 两个页面共用的部分：配置文件的导入导出、提示、别的 App 传来的 .ron；按 [currentPage] 显示网页或全局页面。 */
@Composable
private fun Root(b: Browser, record: (Int) -> Unit) {
    val ctx = LocalContext.current
    val prefs = remember { ctx.getSharedPreferences("ui", 0) }
    // 用户没选过页面：有 Shizuku（或服务进程已经在跑）就进全局页，否则进网页
    LaunchedEffect(Unit) {
        prefs.getString("page", null)?.let { runCatching { Page.valueOf(it) }.getOrNull() }?.let { currentPage = it }
    }
    LaunchedEffect(Controller.shizukuUp, Controller.phase) {
        if (prefs.getString("page", null) == null && (Controller.shizukuUp || Controller.phase != Controller.Phase.IDLE)) {
            currentPage = Page.GLOBAL
        }
    }
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
    // 配置文件：系统的文件选择器（和电脑版的 .ron 通用）
    val exportLauncher = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        if (uri != null) {
            val text = Settings.exportText()
            val ok = text != null && runCatching { ctx.contentResolver.openOutputStream(uri)?.use { it.write(text.toByteArray()) } }.isSuccess
            Settings.message = if (ok) "配置已导出" else "导出失败：${NativeHook.lastError()}"
        }
    }
    val importLauncher = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            val text = runCatching { ctx.contentResolver.openInputStream(uri)?.use { it.readBytes().toString(Charsets.UTF_8) } }.getOrNull()
            if (text == null) Settings.message = "读不了这个文件" else Settings.importText(text) {}
        }
    }
    val onImport = { importLauncher.launch(arrayOf("*/*")) }
    val onExport = { exportLauncher.launch("auto-asmr-export.ron") }

    when (currentPage) {
        Page.WEB -> Screen(b, record, onImport, onExport)
        Page.GLOBAL -> {
            BackHandler { (ctx as? ComponentActivity)?.moveTaskToBack(true) }
            GlobalScreen(onImport = onImport, onExport = onExport)
        }
        Page.FILES -> {
            BackHandler { (ctx as? ComponentActivity)?.moveTaskToBack(true) }
            FilesScreen(onImport = onImport, onExport = onExport)
        }
    }

    // 别的 App 传来的 .ron 文件：确认后导入
    SharedImport.pending?.let { f ->
        if (f.ok) {
            AlertDialog(
                onDismissRequest = { SharedImport.pending = null },
                title = { Text("导入 auto-asmr 配置？") },
                text = {
                    Text("「${f.name}」\n\n${f.info}\n\n导入后，当前的效果设置会换成这个文件里的（和菜单里的「导入配置」一样，电脑版导出的文件也行）。")
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

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Screen(b: Browser, record: (Int) -> Unit, onImport: () -> Unit, onExport: () -> Unit) {
    val ctx = LocalContext.current
    val tab = b.current ?: return
    var showEffects by remember { mutableStateOf(false) }
    var showTabs by remember { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var tele by remember { mutableStateOf(FloatArray(0)) }
    // 首页（App 自带的页面）不在地址栏里显示内部网址
    var address by remember(tab.id, tab.url) { mutableStateOf(if (tab.url.startsWith("https://appassets.androidplatform.net/assets/home")) "" else tab.url) }

    // 引擎状态：每 0.1 秒读一次（JNI，很快）
    LaunchedEffect(Unit) {
        while (true) {
            tele = NativeHook.telemetry()
            delay(100)
        }
    }
    // 网页没在出声、全局模式在处理其他 App 的声音时，位置图和状态显示全局模式的
    val shown = if ((tele.isEmpty() || tele[0] <= 0f) && Controller.intercepting) Controller.poll.tele else tele
    val shownTitle = if (shown === tele) b.audibleTab?.let { it.title.ifEmpty { it.url } } else null

    BackHandler {
        when {
            b.fullscreen != null -> b.exitFullscreen()
            tab.web.canGoBack() -> tab.web.goBack()
            else -> (ctx as? ComponentActivity)?.moveTaskToBack(true)
        }
    }

    Column(Modifier.fillMaxSize().safeDrawingPadding().imePadding()) {
        ModeSwitch(Modifier.padding(horizontal = 12.dp, vertical = 4.dp))
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(horizontal = 4.dp)) {
            TextButton(onClick = { tab.web.goBack() }, enabled = tab.canBack) { Text("‹", style = MaterialTheme.typography.titleLarge) }
            TextButton(onClick = { tab.web.goForward() }, enabled = tab.canForward) { Text("›", style = MaterialTheme.typography.titleLarge) }
            OutlinedTextField(
                value = address,
                onValueChange = { address = it },
                singleLine = true,
                placeholder = { Text("输入网址或搜索", style = MaterialTheme.typography.bodyMedium) },
                textStyle = MaterialTheme.typography.bodyMedium,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Go),
                keyboardActions = KeyboardActions(onGo = { b.open(tab, address) }),
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = { if (tab.progress < 100) tab.web.stopLoading() else tab.web.reload() }) {
                Text(if (tab.progress < 100) "✕" else "⟳", style = MaterialTheme.typography.titleMedium)
            }
        }
        if (tab.progress < 100) LinearProgressIndicator(progress = { tab.progress / 100f }, modifier = Modifier.fillMaxWidth())
        Box(Modifier.weight(1f).fillMaxWidth()) {
            // 固定出声的标签页垫在当前标签页下面：看不见，但对 Chromium 来说网页仍然"可见"，视频不会被自动暂停
            val pin = b.pinned
            if (pin != null && pin != tab) {
                key(pin.id) {
                    AndroidView(factory = { (pin.web.parent as? ViewGroup)?.removeView(pin.web); pin.web }, modifier = Modifier.fillMaxSize())
                }
            }
            key(tab.id) {
                AndroidView(factory = { (tab.web.parent as? ViewGroup)?.removeView(tab.web); tab.web }, modifier = Modifier.fillMaxSize())
            }
        }
        Playback.hookProblem?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
                modifier = Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.errorContainer).padding(horizontal = 12.dp, vertical = 6.dp),
            )
        }
        HorizontalDivider()
        val dry = Settings.params.optBoolean("dry")
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(horizontal = 8.dp)) {
            Switch(checked = !dry, onCheckedChange = { Settings.setDry(!it) }, enabled = Settings.ready)
            Column(Modifier.weight(1f).padding(start = 8.dp).clickable { showEffects = true }) {
                Text(if (dry) "原声" else "ASMR 效果", style = MaterialTheme.typography.labelLarge)
                Text(
                    if (Playback.pausedByUser) "已暂停（在通知栏里继续）" else statusLine(shown, shownTitle, Settings.rainOn),
                    style = MaterialTheme.typography.bodySmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            TextButton(onClick = { showTabs = true }) { Text("标签 ${b.tabs.size}") }
            TextButton(onClick = { showEffects = true }) { Text("效果") }
            Box {
                TextButton(onClick = { menu = true }) { Text("⋮", style = MaterialTheme.typography.titleMedium) }
                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                    DropdownMenuItem(text = { Text("新标签页") }, onClick = { menu = false; b.newTab() })
                    DropdownMenuItem(text = { Text((if (tab.desktop) "✓ " else "") + "桌面版网站") }, onClick = { menu = false; b.setDesktop(tab, !tab.desktop) })
                    DropdownMenuItem(text = { Text((if (b.onlyOne) "✓ " else "") + "只放一个标签页的声音") }, onClick = { menu = false; b.changeOnlyOne(!b.onlyOne) })
                    DropdownMenuItem(text = { Text("首页") }, onClick = { menu = false; b.open(tab, Browser.HOME) })
                    DropdownMenuItem(text = { Text("测试页") }, onClick = { menu = false; b.open(tab, Browser.TEST_URL) })
                    DropdownMenuItem(text = { Text("导入配置") }, onClick = { menu = false; onImport() })
                    DropdownMenuItem(text = { Text("导出配置") }, onClick = { menu = false; onExport() })
                    DropdownMenuItem(text = { Text("录 20 秒处理前后的声音") }, onClick = { menu = false; record(20) })
                }
            }
        }
    }

    if (showEffects) {
        ModalBottomSheet(onDismissRequest = { showEffects = false }, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
            EffectPanel(
                tele = shown,
                status = statusLine(shown, shownTitle, Settings.rainOn),
                onImport = onImport,
                onExport = onExport,
            )
        }
    }
    if (showTabs) {
        ModalBottomSheet(onDismissRequest = { showTabs = false }) {
            TabList(b, onPick = { showTabs = false })
        }
    }
    // 网页里的视频全屏：盖住整个界面，隐藏系统栏
    val window = (ctx as? ComponentActivity)?.window
    val full = b.fullscreen
    LaunchedEffect(full) {
        window?.let {
            val c = WindowInsetsControllerCompat(it, it.decorView)
            if (full != null) {
                c.hide(WindowInsetsCompat.Type.systemBars())
                c.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            } else {
                c.show(WindowInsetsCompat.Type.systemBars())
            }
        }
    }
    if (full != null) {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            key(full) {
                AndroidView(factory = { (full.parent as? ViewGroup)?.removeView(full); full }, modifier = Modifier.fillMaxSize())
            }
        }
    }
}

@Composable
private fun TabList(b: Browser, onPick: () -> Unit) {
    Column(Modifier.padding(horizontal = 16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("标签页", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
            TextButton(onClick = { b.newTab(); onPick() }) { Text("＋ 新标签页") }
        }
        Text(
            when {
                !b.onlyOne -> "所有标签页的声音都会处理"
                b.pinned != null -> "固定出声：${b.pinned?.title}（切到别的标签页时它继续出声）"
                else -> "只放当前标签页的声音；点 📍 可以固定某个标签页一直出声"
            },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        LazyColumn {
            items(b.tabs, key = { it.id }) { t ->
                Row(
                    Modifier.fillMaxWidth().clickable { b.select(t); onPick() }.padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text(if (t.audible) "🔊" else "　")
                    Column(Modifier.weight(1f)) {
                        Text(
                            t.title.ifEmpty { t.url },
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            style = if (t == b.current) MaterialTheme.typography.titleSmall else MaterialTheme.typography.bodyMedium,
                        )
                        Text(t.url, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    TextButton(onClick = { b.togglePin(t) }) { Text(if (b.pinned == t) "📌" else "📍") }
                    TextButton(onClick = { b.close(t) }) { Text("✕") }
                }
            }
        }
        Spacer(Modifier.height(24.dp))
    }
}
