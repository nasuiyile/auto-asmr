package io.github.nasuiyile.autoasmr

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.IconButton
import androidx.compose.material3.TextButton
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * 界面共用的东西：主题（Android 12 起跟随系统壁纸取色）、图标（Material 图标的路径，免得多引一个库）、分区卡片。
 */
@Composable
fun AsmrTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val ctx = LocalContext.current
    val scheme = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> if (dark) dynamicDarkColorScheme(ctx) else dynamicLightColorScheme(ctx)
        dark -> darkColorScheme()
        else -> lightColorScheme()
    }
    MaterialTheme(
        colorScheme = scheme,
        shapes = Shapes(
            small = RoundedCornerShape(10.dp),
            medium = RoundedCornerShape(16.dp),
            large = RoundedCornerShape(24.dp),
        ),
        content = content,
    )
}

object AppIcons {
    private fun icon(name: String, vararg paths: String): ImageVector {
        val b = ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
        for (d in paths) b.addPath(addPathNodes(d), fill = SolidColor(Color.Black))
        return b.build()
    }

    val Web = icon(
        "web",
        "M12,2C6.48,2 2,6.48 2,12s4.48,10 10,10 10,-4.48 10,-10S17.52,2 12,2zM11,19.93c-3.95,-0.49 -7,-3.85 -7,-7.93 0,-0.62 0.08,-1.21 0.21,-1.79L9,15v1c0,1.1 0.9,2 2,2v1.93zM17.9,17.39c-0.26,-0.81 -1,-1.39 -1.9,-1.39h-1v-3c0,-0.55 -0.45,-1 -1,-1L8,12v-2h2c0.55,0 1,-0.45 1,-1L11,7h2c1.1,0 2,-0.9 2,-2v-0.41c2.93,1.19 5,4.06 5,7.41 0,2.08 -0.8,3.97 -2.1,5.39z",
    )
    val Headset = icon(
        "headset",
        "M12,1c-4.97,0 -9,4.03 -9,9v7c0,1.66 1.34,3 3,3h3v-8H5v-2c0,-3.87 3.13,-7 7,-7s7,3.13 7,7v2h-4v8h3c1.66,0 3,-1.34 3,-3v-7c0,-4.97 -4.03,-9 -9,-9z",
    )
    val Library = icon(
        "library",
        "M20,2H8c-1.1,0 -2,0.9 -2,2v12c0,1.1 0.9,2 2,2h12c1.1,0 2,-0.9 2,-2V4c0,-1.1 -0.9,-2 -2,-2zM18,7h-3v5.5c0,1.38 -1.12,2.5 -2.5,2.5S10,13.88 10,12.5s1.12,-2.5 2.5,-2.5c0.57,0 1.08,0.19 1.5,0.51V5h4v2zM4,6H2v14c0,1.1 0.9,2 2,2h14v-2H4V6z",
    )
    val Back = icon("back", "M15.41,7.41L14,6l-6,6 6,6 1.41,-1.41L10.83,12z")
    val Forward = icon("forward", "M10,6L8.59,7.41 13.17,12l-4.58,4.59L10,18l6,-6z")
    val Refresh = icon(
        "refresh",
        "M17.65,6.35C16.2,4.9 14.21,4 12,4c-4.42,0 -7.99,3.58 -7.99,8s3.57,8 7.99,8c3.73,0 6.84,-2.55 7.73,-6h-2.08c-0.82,2.33 -3.04,4 -5.65,4 -3.31,0 -6,-2.69 -6,-6s2.69,-6 6,-6c1.66,0 3.14,0.69 4.22,1.78L13,11h7V4l-2.35,2.35z",
    )
    val Close = icon("close", "M19,6.41L17.59,5 12,10.59 6.41,5 5,6.41 10.59,12 5,17.59 6.41,19 12,13.41 17.59,19 19,17.59 13.41,12z")
    val More = icon(
        "more",
        "M12,8c1.1,0 2,-0.9 2,-2s-0.9,-2 -2,-2 -2,0.9 -2,2 0.9,2 2,2zM12,10c-1.1,0 -2,0.9 -2,2s0.9,2 2,2 2,-0.9 2,-2 -0.9,-2 -2,-2zM12,16c-1.1,0 -2,0.9 -2,2s0.9,2 2,2 2,-0.9 2,-2 -0.9,-2 -2,-2z",
    )
    val Tune = icon(
        "tune",
        "M3,17v2h6v-2L3,17zM3,5v2h10L13,5L3,5zM13,21v-2h8v-2h-8v-2h-2v6h2zM7,9v2L3,11v2h4v2h2L9,9L7,9zM21,13v-2L11,11v2h10zM15,9h2L17,7h4L21,5h-4L17,3h-2v6z",
    )
    val Add = icon("add", "M19,13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z")
    val Play = icon("play", "M8,5v14l11,-7z")
    val Stop = icon("stop", "M6,6h12v12H6z")
    val ExpandMore = icon("expand", "M16.59,8.59L12,13.17 7.41,8.59 6,10l6,6 6,-6z")
    val Info = icon(
        "info",
        "M11,7h2v2h-2zM11,11h2v6h-2zM12,2C6.48,2 2,6.48 2,12s4.48,10 10,10 10,-4.48 10,-10S17.52,2 12,2zM12,20c-4.41,0 -8,-3.59 -8,-8s3.59,-8 8,-8 8,3.59 8,8 -3.59,8 -8,8z",
    )
    val Folder = icon("folder", "M10,4H4c-1.1,0 -1.99,0.9 -1.99,2L2,18c0,1.1 0.9,2 2,2h16c1.1,0 2,-0.9 2,-2V8c0,-1.1 -0.9,-2 -2,-2h-8l-2,-2z")
    val Save = icon(
        "save",
        "M17,3H5c-1.11,0 -2,0.9 -2,2v14c0,1.1 0.89,2 2,2h14c1.1,0 2,-0.9 2,-2V7l-4,-4zM12,19c-1.66,0 -3,-1.34 -3,-3s1.34,-3 3,-3 3,1.34 3,3 -1.34,3 -3,3zM15,9H5V5h10v4z",
    )
    val Star = icon("star", "M12,17.27L18.18,21l-1.64,-7.03L22,9.24l-7.19,-0.61L12,2 9.19,8.63 2,9.24l5.46,4.73L5.82,21z")
    val Check = icon("check", "M9,16.17L4.83,12l-1.42,1.41L9,19 21,7l-1.41,-1.41z")
    val Help = icon(
        "help",
        "M11,18h2v-2h-2v2zM12,2C6.48,2 2,6.48 2,12s4.48,10 10,10 10,-4.48 10,-10S17.52,2 12,2zM12,20c-4.41,0 -8,-3.59 -8,-8s3.59,-8 8,-8 8,3.59 8,8 -3.59,8 -8,8zM12,6c-2.21,0 -4,1.79 -4,4h2c0,-1.1 0.9,-2 2,-2s2,0.9 2,2c0,2 -3,1.75 -3,5h2c0,-2.25 3,-2.5 3,-5 0,-2.21 -1.79,-4 -4,-4z",
    )
}

/** 图标（默认 20 dp，跟随内容颜色）。 */
@Composable
fun Ic(v: ImageVector, modifier: Modifier = Modifier, size: Dp = 20.dp, tint: Color = Color.Unspecified) {
    if (tint == Color.Unspecified) Icon(v, null, modifier.size(size)) else Icon(v, null, modifier.size(size), tint = tint)
}

/** 页面的分区：圆角卡片，上面一行标题（可选右边的操作），下面是内容。排得紧凑些，第一屏能放下更多东西。 */
@Composable
fun Section(
    title: String? = null,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    actions: @Composable RowScope.() -> Unit = {},
    content: @Composable ColumnScope.() -> Unit,
) {
    Card(
        modifier.fillMaxWidth().padding(vertical = 4.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
        shape = MaterialTheme.shapes.large,
    ) {
        Column(Modifier.padding(horizontal = 14.dp, vertical = 10.dp)) {
            if (title != null) {
                Row(Modifier.heightIn(min = 28.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(title, style = MaterialTheme.typography.titleSmall)
                        if (subtitle != null) {
                            Text(subtitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                    actions()
                }
            }
            content()
        }
    }
}

/** 说明对话框里的一段：小标题 + 正文。 */
class HelpItem(val title: String, val text: String)

/**
 * 页面顶上的标题行：标题、一行很短的提示（可选），右边一个「?」按钮。
 * 页面上的说明文字都收进点「?」打开的对话框里，页面本身只留控件。
 */
@Composable
fun PageHeader(title: String, hint: String? = null, help: List<HelpItem> = emptyList(), trailing: @Composable RowScope.() -> Unit = {}) {
    var open by remember { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth().padding(top = 6.dp, bottom = 2.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleLarge)
            if (hint != null) Text(hint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        trailing()
        if (help.isNotEmpty()) {
            IconButton(onClick = { open = true }) { Ic(AppIcons.Help, size = 24.dp, tint = MaterialTheme.colorScheme.primary) }
        }
    }
    if (open) HelpDialog(title, help) { open = false }
}

/** 「?」打开的说明：可滚动的几段文字。 */
@Composable
fun HelpDialog(title: String, help: List<HelpItem>, onClose: () -> Unit) {
    AlertDialog(
        onDismissRequest = onClose,
        confirmButton = { TextButton(onClick = onClose) { Text("知道了") } },
        icon = { Ic(AppIcons.Help, size = 24.dp) },
        title = { Text(title) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                for ((i, h) in help.withIndex()) {
                    Text(h.title, style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(top = if (i == 0) 0.dp else 12.dp))
                    Text(h.text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = 2.dp))
                }
            }
        },
    )
}
