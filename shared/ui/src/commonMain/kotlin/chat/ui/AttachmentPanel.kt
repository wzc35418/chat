package chat.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import chat.presentation.display.toBadge
import chat.presentation.preview.PreviewAttachmentState

/** 附件面板交互回调。 */
data class AttachmentHandlers(
    val updateImportPath: (String) -> Unit = {},
    val importFile: () -> Unit = {},
    val updateExportPath: (String) -> Unit = {},
    val exportFile: (String) -> Unit = {},
    val cancel: (String) -> Unit = {},
    val chooseImportFile: () -> Unit = {},
    val chooseExportTarget: () -> Unit = {},
) {
    companion object {
        val None = AttachmentHandlers()
    }
}

/**
 * “附件（本机）”面板：加密导入、状态、解密导出、取消。
 *
 * 上传/下载未接线，所有条目都明确标注“未上传”，不会暗示可被对方下载。
 */
@Composable
fun AttachmentPanel(
    state: PreviewAttachmentState,
    handlers: AttachmentHandlers,
    onClose: () -> Unit,
) {
    Box(
        Modifier.fillMaxSize().padding(16.dp),
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            modifier = Modifier
                .fillMaxWidth(0.96f)
                .widthIn(max = 760.dp)
                .heightIn(max = 700.dp)
                .semantics { contentDescription = "附件（本机）面板" },
            color = Color.White,
            shape = RoundedCornerShape(18.dp),
        ) {
            Column(
                Modifier.padding(horizontal = 18.dp, vertical = 16.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("附件（本机）", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.Bold)
                        Text(
                            "分块加密缓存 + 解密导出；节点上传/下载端点已就绪但尚未接线，条目不会离开本机。",
                            style = MaterialTheme.typography.labelSmall,
                            color = Color(0xFF5F6B63),
                        )
                    }
                    TextButton(
                        onClick = onClose,
                        modifier = Modifier.semantics { contentDescription = "关闭附件面板" },
                    ) { Text("关闭") }
                }

                state.notice?.let { notice ->
                    Surface(
                        color = if (state.lastErrorCode == null) Color(0xFFEDF1E9) else Color(0xFFFDECEA),
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.fillMaxWidth().semantics {
                            liveRegion = LiveRegionMode.Polite
                            stateDescription = notice
                        },
                    ) {
                        Text(notice, Modifier.padding(12.dp), style = MaterialTheme.typography.bodySmall)
                    }
                }
                state.lastErrorCode?.let { code ->
                    Text("错误码：" + code, style = MaterialTheme.typography.labelSmall, color = Color(0xFFB3261E))
                }

                OutlinedTextField(
                    value = state.importPath,
                    onValueChange = handlers.updateImportPath,
                    modifier = Modifier.fillMaxWidth().semantics { contentDescription = "要导入的本机文件绝对路径" },
                    singleLine = true,
                    label = { Text("本机文件绝对路径（演示用输入框）") },
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(
                        onClick = handlers.chooseImportFile,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "打开文件选择器选择要导入的文件" },
                    ) { Text("选择文件…", style = MaterialTheme.typography.labelMedium) }
                    Button(
                        onClick = handlers.importFile,
                        enabled = state.ready && !state.busy && state.importPath.isNotBlank(),
                        modifier = Modifier.semantics { contentDescription = "加密导入该文件" },
                    ) { Text("加密导入") }
                }

                HorizontalDivider(color = Color(0xFFE1E6E1))
                Text("本机附件（真实状态）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                if (state.attachments.isEmpty()) {
                    Text("还没有附件；导入后会出现条数与加密摘要。", style = MaterialTheme.typography.bodySmall)
                } else {
                    state.attachments.forEach { attachment ->
                        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                            val badge = attachment.toBadge()
                            Text(
                                attachment.displayName + " · " + badge.label + (if (badge.downloadable) "" else " · 对方不可下载"),
                                style = MaterialTheme.typography.bodySmall,
                                fontWeight = FontWeight.Medium,
                            )
                            Text(
                                attachment.attachmentId + " · " + attachment.plainSize + " 字节 / " +
                                    attachment.chunkCount + " 块 · 摘要 " + attachment.contentDigest.take(16) + "…",
                                style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                color = Color(0xFF5F6B63),
                            )
                            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                TextButton(
                                    onClick = { handlers.cancel(attachment.attachmentId) },
                                    enabled = state.ready && !state.busy,
                                    modifier = Modifier.semantics { contentDescription = "取消该附件" },
                                ) { Text("取消", style = MaterialTheme.typography.labelMedium) }
                            }
                        }
                    }
                }

                HorizontalDivider(color = Color(0xFFE1E6E1))
                OutlinedTextField(
                    value = state.exportPath,
                    onValueChange = handlers.updateExportPath,
                    modifier = Modifier.fillMaxWidth().semantics { contentDescription = "导出目标绝对路径" },
                    singleLine = true,
                    label = { Text("导出目标绝对路径") },
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(
                        onClick = handlers.chooseExportTarget,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "选择导出目标位置" },
                    ) { Text("选择导出位置…", style = MaterialTheme.typography.labelMedium) }
                }
                state.attachments.lastOrNull()?.let { latest ->
                    Button(
                        onClick = { handlers.exportFile(latest.attachmentId) },
                        enabled = state.ready && !state.busy && state.exportPath.isNotBlank(),
                        modifier = Modifier.semantics { contentDescription = "解密导出最近导入的附件" },
                    ) { Text("导出最近附件") }
                }
            }
        }
    }
}
