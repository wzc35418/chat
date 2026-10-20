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
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import chat.presentation.preview.PreviewAboutState

/** 诊断回调。 */
data class AboutHandlers(
    val refresh: () -> Unit = {},
    /** 界面缩放（0.9 / 1.0 / 1.15 / 1.3）。 */
    val setScale: (Float) -> Unit = {},
    /** 请求迁移密钥槽（第一步：显示确认）。 */
    val requestKeySlotMigration: () -> Unit = {},
    /** 确认迁移。 */
    val confirmKeySlotMigration: () -> Unit = {},
    /** 取消迁移。 */
    val cancelKeySlotMigration: () -> Unit = {},
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
    /** 当前密钥槽后端（人类可读）。 */
    keySlotBackend: String = "",
    /** 探测依据（为什么选了这个后端）。 */
    keySlotDetail: String = "",
    /** 密钥槽保护说明。 */
    keySlotSummary: String = "",
    /** 是否正在等待"确认迁移"。 */
    keySlotMigrationPending: Boolean = false,
    /** 迁移目标后端是否可用（不可用时按钮禁用并说明）。 */
    keySlotMigrationAvailable: Boolean = true,
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
        Text("密钥槽（主密钥保护）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
        LabeledRow("当前后端", keySlotBackend.ifBlank { "（未探测）" })
        if (keySlotDetail.isNotBlank()) LabeledRow("探测依据", keySlotDetail)
        if (keySlotSummary.isNotBlank()) {
            Text(keySlotSummary, style = MaterialTheme.typography.labelSmall, color = Color(0xFF5F6B63))
        }
        if (keySlotMigrationPending) {
            Text(
                "确认迁移到 Windows 凭据管理器？迁移后旧 DPAPI 文件会被删除；换 Windows 用户或换机器将无法解包。",
                style = MaterialTheme.typography.labelSmall,
                color = Color(0xFFB3261E),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                TextButton(
                    onClick = handlers.confirmKeySlotMigration,
                    modifier = Modifier.semantics { contentDescription = "确认迁移密钥槽" },
                ) { Text("确认迁移", style = MaterialTheme.typography.labelMedium) }
                TextButton(
                    onClick = handlers.cancelKeySlotMigration,
                    modifier = Modifier.semantics { contentDescription = "取消迁移密钥槽" },
                ) { Text("取消", style = MaterialTheme.typography.labelMedium) }
            }
        } else {
            TextButton(
                onClick = handlers.requestKeySlotMigration,
                enabled = keySlotMigrationAvailable,
                modifier = Modifier.semantics { contentDescription = "迁移密钥槽到凭据管理器" },
            ) {
                Text(
                    if (keySlotMigrationAvailable) "迁移到凭据管理器" else "（已是凭据管理器或不可用）",
                    style = MaterialTheme.typography.labelMedium,
                )
            }
        }

        HorizontalDivider(color = Color(0xFFE1E6E1))
        Text("界面缩放", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            listOf(0.9f to "小", 1.0f to "标准", 1.15f to "大", 1.3f to "特大").forEach { (factor, label) ->
                TextButton(
                    onClick = { handlers.setScale(factor) },
                    modifier = Modifier.semantics { contentDescription = "界面缩放 " + label },
                ) { Text(label + "（" + factor + "）", style = MaterialTheme.typography.labelSmall) }
            }
        }
        Text(
            "缩放只影响显示；偏好保存在本机 ui-preferences.json（非机密）。",
            style = MaterialTheme.typography.labelSmall,
            color = Color(0xFF5F6B63),
        )
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
