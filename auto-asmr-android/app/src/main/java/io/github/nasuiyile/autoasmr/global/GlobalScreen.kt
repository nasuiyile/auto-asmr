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
import io.github.nasuiyile.autoasmr.AppIcons
import io.github.nasuiyile.autoasmr.Ic
import io.github.nasuiyile.autoasmr.PageHeader
import io.github.nasuiyile.autoasmr.HelpItem
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextOverflow
import io.github.nasuiyile.autoasmr.Section
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.TextButton
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
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
            item { PageHeader("全局模式", help = globalHelp()) }
            item { ServiceCard() }
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

/** 全局页的说明：都收在标题右边的「?」里，页面上只留控件和状态。 */
private fun globalHelp(): List<HelpItem> = listOf(
    HelpItem("全局模式是什么", "把其他 App 播放的声音截下来，做成 KU100 双耳 ASMR 效果再放出来。戴耳机听。"),
    HelpItem("需要什么", "需要 Shizuku 或 root。都没有的话，用「网页」页在内嵌浏览器里听网页里的声音。"),
    HelpItem(
        "Shizuku 和 root 启动的区别",
        "Shizuku 启动的服务进程是常驻的：App 被系统清掉后声音照常处理，重新打开 App 会自动连回去。\n" +
            "root 启动的服务进程跟着 App 走：App 被杀时它也退出。想让它常驻，用 Shizuku（Shizuku 本身也可以用 root 启动）。",
    ),
    HelpItem("拦截其他 App 的声音", "打开时，各 App 的原声不再直接进耳机，听到的是处理后的声音。关掉（暂停）时各 App 的声音照常直接放出，服务进程还在。"),
    HelpItem("拦截时的那行数字", "峰值 a → b：截到的声音（a）和放出去的声音（b）有多响，1 = 满幅。断流：输出没来得及送声音的次数，一直涨说明手机忙不过来。输出缓冲：放出去之前排队的声音有多长，越长延迟越大。"),
    HelpItem("效果参数", "下面的效果参数和「网页」页共用一份配置；输出增益只调实时听到的音量，不影响文件转换。"),
    HelpItem("版本", BuildConfig.VERSION_NAME),
)

/** 状态小圆点 + 文字。 */
@Composable
private fun StatusDot(color: Color, text: String, modifier: Modifier = Modifier) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = modifier) {
        androidx.compose.foundation.layout.Box(Modifier.size(8.dp).background(color, CircleShape))
        Text(text, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(start = 6.dp))
    }
}

@Composable
private fun SmallNote(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier,
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ServiceCard() {
    val c = Controller
    val p = c.poll
    val ok = Color(0xFF3FA66B)
    val warn = Color(0xFFE0A030)
    val off = MaterialTheme.colorScheme.outline
    Section(
        "服务进程",
        actions = {
            if (c.phase == Controller.Phase.CONNECTED) {
                TextButton(onClick = { c.exit() }, modifier = Modifier.height(32.dp), contentPadding = PaddingValues(horizontal = 10.dp)) { Text("退出服务") }
            }
        },
    ) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(14.dp), modifier = Modifier.padding(top = 2.dp)) {
            StatusDot(if (c.shizukuUp) ok else off, "Shizuku：${c.shizuku}")
            when (c.phase) {
                Controller.Phase.IDLE -> StatusDot(off, "服务没在运行")
                Controller.Phase.STARTING -> StatusDot(warn, "正在启动（${c.how?.label}）…")
                Controller.Phase.CONNECTED -> StatusDot(ok, "已连上（${c.how?.label}）")
            }
        }
        if (c.connected) c.hello?.let { SmallNote(it, Modifier.padding(top = 2.dp)) }
        p.fatal?.let {
            Text(
                "出错：$it",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
                modifier = Modifier.fillMaxWidth().padding(top = 6.dp)
                    .background(MaterialTheme.colorScheme.errorContainer, MaterialTheme.shapes.small).padding(8.dp),
            )
        }
        when (c.phase) {
            Controller.Phase.IDLE -> Row(Modifier.fillMaxWidth().padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = { c.startShizuku() }, modifier = Modifier.weight(1f)) { Text("用 Shizuku 启动") }
                OutlinedButton(onClick = { c.startRoot() }, modifier = Modifier.weight(1f)) { Text("用 root 启动") }
            }
            Controller.Phase.STARTING -> LinearProgressIndicator(Modifier.fillMaxWidth().padding(top = 10.dp))
            Controller.Phase.CONNECTED -> {
                Row(
                    Modifier.fillMaxWidth().padding(top = 6.dp).clip(MaterialTheme.shapes.small)
                        .background(if (p.running) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceContainerHighest)
                        .clickable { c.setIntercept(!p.running) }.padding(horizontal = 12.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f).padding(end = 8.dp)) {
                        Text("拦截其他 App 的声音", style = MaterialTheme.typography.bodyLarge)
                        Text(
                            if (p.running) {
                                String.format(Locale.ROOT, "峰值 %.2f → %.2f · 断流 %d · 缓冲 %.0f ms", p.inPeak, p.outPeak, p.underruns, p.outBuf)
                            } else {
                                "已暂停：各 App 的声音照常直接放出"
                            },
                            style = MaterialTheme.typography.bodySmall,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    Switch(checked = p.running, onCheckedChange = { c.setIntercept(it) })
                }
            }
        }
    }
}

@Composable
private fun LogCard() {
    var open by rememberSaveable { mutableStateOf(false) }
    val turn by animateFloatAsState(if (open) 180f else 0f, label = "chevron")
    Card(
        Modifier.fillMaxWidth().padding(vertical = 4.dp),
        shape = MaterialTheme.shapes.large,
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
    ) {
        Row(
            Modifier.fillMaxWidth().clickable { open = !open }.padding(horizontal = 14.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("日志", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
            Ic(AppIcons.ExpandMore, Modifier.rotate(turn), size = 24.dp, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (open) {
            SelectionContainer {
                Text(
                    Controller.logs.takeLast(60).joinToString("\n").ifEmpty { "（还没有）" },
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 14.dp),
                )
            }
        }
    }
}
