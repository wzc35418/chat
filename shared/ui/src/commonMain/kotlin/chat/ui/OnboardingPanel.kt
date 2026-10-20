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
import chat.presentation.preview.PreviewOnboardingState

/** 首启引导回调。 */
data class OnboardingHandlers(
    val updateEndpoint: (String) -> Unit = {},
    val updateNodeId: (String) -> Unit = {},
    val updateNodePublicKey: (String) -> Unit = {},
    val save: () -> Unit = {},
    val skip: () -> Unit = {},
    val close: () -> Unit = {},
) {
    companion object {
        val None = OnboardingHandlers()
    }
}

/**
 * 首启引导：说明数据落在哪、加密方式，并要求填（或跳过）节点连接信息。
 *
 * 语义边界：这里**不创建身份**（身份由内核自动建立），也不假装"已联网"；
 * 跳过即明确写入"未配置节点"，之后可在本机存储面板补填。
 */
@Composable
fun OnboardingPanel(
    state: PreviewOnboardingState,
    handlers: OnboardingHandlers = OnboardingHandlers.None,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(Color(0xFFF7F9F7))
            .verticalScroll(rememberScrollState())
            .padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text("欢迎使用 Chat", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
        Text(
            "第 " + (state.step + 1) + " / 3 步",
            style = MaterialTheme.typography.labelSmall,
            color = Color(0xFF5F6B63),
        )
        HorizontalDivider(color = Color(0xFFE1E6E1))

        when (state.step) {
            0 -> Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text("数据存在哪", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                Text(
                    "消息与群状态保存在本机加密数据库（密钥由系统凭据封装）。本版本不会把明文消息上传到任何服务器。",
                    style = MaterialTheme.typography.bodySmall,
                )
                Text(
                    "聊天需要一台「节点」来中继密文；没有节点时只能本机操作（会明确提示，不会假装已发送）。",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            1 -> Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text("连接节点（可跳过）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                OutlinedTextField(
                    value = state.endpointUrl,
                    onValueChange = handlers.updateEndpoint,
                    label = { Text("节点地址，例如 http://127.0.0.1:8080") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = state.nodeId,
                    textStyle = MaterialTheme.typography.bodySmall,
                    onValueChange = handlers.updateNodeId,
                    label = { Text("节点 ID") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = state.nodePublicKeyHex,
                    onValueChange = handlers.updateNodePublicKey,
                    label = { Text("节点公钥（可选，64 hex）") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Text(
                    "这些值只保存在本机配置文件里（地址/ID/公钥都是公开信息，不含任何密钥材料）。",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF5F6B63),
                )
            }
            else -> Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text("可以开始了", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                Text(
                    if (state.endpointUrl.isBlank()) {
                        "你选择了暂时不连接节点：现在可以建群、看本机数据；发消息时会提示未配置节点。"
                    } else {
                        "已记录节点地址。下一步建议：在「群」面板建群，或在「真实身份」面板查看/导出恢复文件。"
                    },
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }

        if (state.lastErrorCode != null) {
            Text(
                "错误码：" + state.lastErrorCode + (state.notice?.let { " · " + it } ?: ""),
                style = MaterialTheme.typography.labelSmall,
                color = Color(0xFFB3261E),
            )
        }

        HorizontalDivider(color = Color(0xFFE1E6E1))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(
                onClick = handlers.skip,
                modifier = Modifier.semantics { contentDescription = "跳过节点配置" },
            ) { Text("跳过") }
            TextButton(
                onClick = handlers.save,
                enabled = !state.busy,
                modifier = Modifier.semantics { contentDescription = "保存并继续" },
            ) { Text(if (state.step >= 2) "完成" else "保存并继续") }
        }
    }
}
