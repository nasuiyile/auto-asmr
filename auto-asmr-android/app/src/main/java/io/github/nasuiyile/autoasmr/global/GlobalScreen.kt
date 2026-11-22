package io.github.nasuiyile.autoasmr.global

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import io.github.nasuiyile.autoasmr.BuildConfig
import io.github.nasuiyile.autoasmr.EffectPanel
import io.github.nasuiyile.autoasmr.ModeSwitch
import java.util.Locale

/**
 * 全局模式（Shizuku / root）的页面：一页里放下服务进程（启动、拦截开关、退出）、
 * 效果参数（和网页模式同一个面板）、日志。
 */
@Composable
fun GlobalScreen(onImport: () -> Unit, onExport: () -> Unit) {
    EffectPanel(
        tele = Controller.poll.tele,
        status = globalStatusLine(),
        onImport = onImport,
        onExport = onExport,
        modifier = Modifier.safeDrawingPadding(),
        header = {
            item { ModeSwitch(Modifier.padding(top = 4.dp)) }
            item { Title() }
            item { ServiceCard() }
            item { HorizontalDivider(Modifier.padding(vertical = 8.dp)) }
        },
        footer = { item { LogCard() } },
    )
}

/** 全局模式的状态一行（效果面板的标题下面）。 */
fun globalStatusLine(): String {
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
private fun Title() {
    Column(Modifier.padding(top = 8.dp, bottom = 4.dp)) {
        Text("auto-asmr · 全局模式", style = MaterialTheme.typography.headlineSmall)
        Text(
            "把其他 App 播放的声音截下来，用 auto-asmr 引擎做成 KU100 双耳 ASMR 效果再放出来。需要 Shizuku 或 root，戴耳机听。" +
                "没有的话切到上面的「网页模式」，在内嵌浏览器里听网页。版本 ${BuildConfig.VERSION_NAME}",
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
