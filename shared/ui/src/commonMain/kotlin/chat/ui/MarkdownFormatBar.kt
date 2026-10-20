package chat.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.unit.dp

/**
 * 行内格式化工具条：加粗 / 斜体 / 删除线 / 行内代码。
 *
 * 语义边界：只操作 **Markdown 源文本的选区**（不改变渲染管线），空选区插入成对标记。
 * 桌面可用 Ctrl+B / Ctrl+I（由宿主接线），Android 无右键但同一排按钮可用。
 */
@Composable
fun MarkdownFormatBar(
    onFormat: (String) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    Row(modifier = modifier, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        FormatButton("加粗", "把选区加粗（Markdown **）", enabled) { onFormat(MARKER_BOLD) }
        FormatButton("斜体", "把选区变成斜体（Markdown *）", enabled) { onFormat(MARKER_ITALIC) }
        FormatButton("删除线", "把选区加删除线（Markdown ~~）", enabled) { onFormat(MARKER_STRIKE) }
        FormatButton("代码", "把选区变成行内代码（Markdown `）", enabled) { onFormat(MARKER_CODE) }
    }
}

const val MARKER_BOLD = "**"
const val MARKER_ITALIC = "*"
const val MARKER_STRIKE = "~~"
const val MARKER_CODE = "`"

@Composable
private fun FormatButton(label: String, description: String, enabled: Boolean, onClick: () -> Unit) {
    TextButton(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.semantics { contentDescription = description },
    ) { Text(label, style = MaterialTheme.typography.labelMedium, color = Color(0xFF246B58)) }
}

/** 把一次格式化结果应用回 `(text, selection)`；纯函数封装，便于宿主与测试共用。 */
fun applyMarker(text: String, selection: TextRange, marker: String): MarkdownEdit =
    MarkdownFormatting.toggleInline(text, selection, marker)
