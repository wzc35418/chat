package chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp

/** 命令面板的一个条目（只有元数据：谁提供、叫什么）。 */
data class PaletteEntry(val pluginId: String, val command: String)

/** 命令面板回调。 */
data class CommandPaletteHandlers(
    val updateFilter: (String) -> Unit = {},
    val run: (PaletteEntry) -> Unit = {},
    val close: () -> Unit = {},
) {
    companion object {
        val None = CommandPaletteHandlers()
    }
}

/**
 * 命令面板（声明式槽位 `palette.commands`）。
 *
 * 语义边界：宿主只渲染**插件注册的命令名**；命令产物是草稿文本，不是直接发送。
 * 插件拿不到布局，面板样式与位置由宿主决定。
 */
@Composable
fun CommandPalette(
    filter: String,
    entries: List<PaletteEntry>,
    handlers: CommandPaletteHandlers = CommandPaletteHandlers.None,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(Color(0xFFF7F9F7))
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("命令面板", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            TextButton(
                onClick = handlers.close,
                modifier = Modifier.semantics { contentDescription = "关闭命令面板" },
            ) { Text("关闭", style = MaterialTheme.typography.labelMedium) }
        }
        OutlinedTextField(
            value = filter,
            onValueChange = handlers.updateFilter,
            label = { Text("过滤命令") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        val visible = entries.filter { filter.isBlank() || it.command.contains(filter, ignoreCase = true) }
        if (visible.isEmpty()) {
            Text(
                if (entries.isEmpty()) "还没有插件注册命令。" else "没有匹配的命令。",
                style = MaterialTheme.typography.bodySmall,
            )
        } else {
            visible.forEach { entry ->
                TextButton(
                    onClick = { handlers.run(entry) },
                    modifier = Modifier.semantics { contentDescription = "运行命令 " + entry.command },
                ) {
                    Text(entry.command + "（" + entry.pluginId + "）", style = MaterialTheme.typography.bodySmall)
                }
            }
        }
        Text(
            "命令产物只会进入草稿，是否发送由你决定。",
            style = MaterialTheme.typography.labelSmall,
            color = Color(0xFF5F6B63),
        )
    }
}
