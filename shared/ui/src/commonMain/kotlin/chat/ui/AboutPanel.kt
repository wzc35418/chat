package chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import chat.presentation.preview.PreviewAboutState

/** 诊断回调。 */
data class AboutHandlers(
    val refresh: () -> Unit = {},
) {
    companion object {
        val None = AboutHandlers()
    }
}

/**
 * 「关于 / 诊断」面板：版本、构建类型、FFI 契约版本、平台、host 库路径、数据目录。
 *
 * 语义边界：这里显示的是**真实**的编译期常量与路径解析结果；解析失败会显示错误本身（含尝试过的路径），
 * 不做任何静默回退。
 */
@Composable
fun AboutPanel(
    state: PreviewAboutState,
    handlers: AboutHandlers = AboutHandlers.None,
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
        Text("关于 / 诊断", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
        Text(
            "这些信息用于排查问题；不含任何密钥或消息内容。",
            style = MaterialTheme.typography.labelSmall,
            color = Color(0xFF5F6B63),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = handlers.refresh) { Text("刷新") }
        }
        HorizontalDivider(color = Color(0xFFE1E6E1))
        state.appInfo?.let { info ->
            LabeledRow("版本", info.version)
            LabeledRow("构建类型", info.buildProfile)
            LabeledRow("FFI 契约版本", info.ffiVersion)
            LabeledRow("平台", info.platform)
        } ?: Text("还没有读取版本信息。", style = MaterialTheme.typography.bodySmall)
        HorizontalDivider(color = Color(0xFFE1E6E1))
        LabeledRow("host 库路径", state.hostLibraryPath.ifBlank { "（未解析）" })
        LabeledRow("数据目录", state.dataDir.ifBlank { "（未知）" })
        state.lastErrorCode?.let { code ->
            LabeledRow("错误码", code)
        }
        state.notice?.let { notice ->
            Text(notice, style = MaterialTheme.typography.labelSmall, color = Color(0xFF5F6B63))
        }
        HorizontalDivider(color = Color(0xFFE1E6E1))
        Text(
            "遇到问题请附带：版本 + 平台 + host 库路径 + 错误码（不要附口令或密钥）。",
            style = MaterialTheme.typography.labelSmall,
            color = Color(0xFF5F6B63),
        )
    }
}

@Composable
private fun LabeledRow(label: String, value: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(label + "：", style = MaterialTheme.typography.labelSmall, color = Color(0xFF5F6B63))
        Text(value, style = MaterialTheme.typography.bodySmall)
    }
}
