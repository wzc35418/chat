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
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import chat.presentation.preview.PreviewStoreState

/** 本机存储面板的交互回调；由桌面入口接到挂起调用上。 */
data class StoreHandlers(
    val refreshConversations: () -> Unit = {},
    val selectConversation: (String) -> Unit = {},
    val refreshMessages: () -> Unit = {},
    val updateDraftText: (String) -> Unit = {},
    val createDraft: () -> Unit = {},
    val cancelLatestDraft: () -> Unit = {},
    val pollEvents: () -> Unit = {},
    val updateSendNodeUrl: (String) -> Unit = {},
    val updateSendNodeId: (String) -> Unit = {},
    val updateSendNodePublicKey: (String) -> Unit = {},
    val configureSendNode: () -> Unit = {},
    val sendPrepare: () -> Unit = {},
    val sendDispatch: () -> Unit = {},
    val sendRetry: () -> Unit = {},
    val sendReconcile: () -> Unit = {},
) {
    companion object {
        val None = StoreHandlers()
    }
}

/**
 * “本机存储（真实）”面板：显示 Rust formal store 里真实的加密库内容。
 *
 * 草稿只显示本机事实（queued/prepared），不会被显示成已发送；会话与消息只有写入
 * “已验证条目”后才会出现，这是当前发送链路未接的诚实表现。
 */
@Composable
fun LocalStorePanel(
    state: PreviewStoreState,
    draftText: String,
    handlers: StoreHandlers,
    onClose: () -> Unit,
) {
    Box(
        Modifier
            .fillMaxSize()
            .padding(16.dp),
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            modifier = Modifier
                .fillMaxWidth(0.96f)
                .widthIn(max = 760.dp)
                .heightIn(max = 700.dp)
                .semantics { contentDescription = "本机存储（真实）面板" },
            color = Color.White,
            shape = RoundedCornerShape(18.dp),
        ) {
            Column(
                Modifier
                    .padding(horizontal = 18.dp, vertical = 16.dp)
                    .verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("本机存储（真实）", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.Bold)
                        Text(
                            "内容来自 Rust formal store 的加密 SQLite；本次预览使用临时演示库，关闭后删除。",
                            style = MaterialTheme.typography.labelSmall,
                            color = Color(0xFF5F6B63),
                        )
                    }
                    TextButton(
                        onClick = onClose,
                        modifier = Modifier.semantics { contentDescription = "关闭本机存储面板" },
                    ) { Text("关闭") }
                }

                state.databasePath?.let { path ->
                    Text(
                        "库文件：" + path,
                        style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                        color = Color(0xFF5F6B63),
                    )
                }
                state.notice?.let { notice ->
                    Surface(
                        color = if (state.lastErrorCode == null) Color(0xFFEDF1E9) else Color(0xFFFDECEA),
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.fillMaxWidth().semantics {
                            liveRegion = LiveRegionMode.Polite
                            contentDescription = notice
                        },
                    ) {
                        Text(notice, Modifier.padding(horizontal = 12.dp, vertical = 8.dp), style = MaterialTheme.typography.bodySmall)
                    }
                }
                state.lastErrorCode?.let { code ->
                    Text("错误码：" + code, style = MaterialTheme.typography.labelSmall, color = Color(0xFFB3261E))
                }

                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Button(
                        onClick = handlers.refreshConversations,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "刷新本机会话列表" },
                    ) { Text("刷新会话") }
                    Text(
                        if (state.busy) "正在读取…" else "会话 " + state.conversations.size + " · 消息 " + state.messages.size + " · 草稿 " + state.drafts.size,
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF5F6B63),
                    )
                }

                HorizontalDivider(color = Color(0xFFE1E6E1))
                Text("会话（真实库）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                if (state.conversations.isEmpty()) {
                    Text(
                        "本机库里还没有会话。写入一条“已验证条目”后才会出现——发送链路尚未接入，这里不会伪造消息。",
                        style = MaterialTheme.typography.bodySmall,
                    )
                } else {
                    state.conversations.forEach { conversation ->
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            OutlinedButton(
                                onClick = { handlers.selectConversation(conversation.id) },
                                modifier = Modifier.semantics {
                                    contentDescription = "打开会话 " + conversation.title
                                },
                            ) { Text(if (conversation.id == state.selectedConversationId) "已选中" else "打开") }
                            Text(
                                "  " + conversation.title + " · " + conversation.preview,
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                    TextButton(
                        onClick = handlers.refreshMessages,
                        enabled = state.selectedConversationId != null && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "刷新当前会话消息" },
                    ) { Text("刷新当前会话消息") }
                    state.messages.forEach { message ->
                        Text(
                            (if (message.mine) "我：" else message.author + "：") + message.body,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }

                HorizontalDivider(color = Color(0xFFE1E6E1))
                Text("新建真实草稿", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                Text(
                    "草稿会写入真实加密库（queued/prepared），但不会发送：发送链路未接入。",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF5F6B63),
                )
                OutlinedTextField(
                    value = draftText,
                    onValueChange = handlers.updateDraftText,
                    modifier = Modifier.fillMaxWidth().semantics { contentDescription = "本机草稿正文" },
                    minLines = 2,
                    maxLines = 4,
                    label = { Text("草稿正文（支持 Markdown，最多 8 KiB）") },
                )
                Button(
                    onClick = handlers.createDraft,
                    enabled = state.ready && !state.busy && draftText.isNotBlank(),
                    modifier = Modifier.semantics { contentDescription = "把草稿写入本机加密库" },
                ) { Text("写入本机库") }

                if (state.drafts.isNotEmpty()) {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text("本次创建的草稿（真实状态）", style = MaterialTheme.typography.labelLarge)
                        TextButton(
                            onClick = handlers.cancelLatestDraft,
                            enabled = state.ready && !state.busy,
                            modifier = Modifier.semantics { contentDescription = "取消最近一条本机草稿" },
                        ) { Text("取消最近草稿", style = MaterialTheme.typography.labelMedium) }
                    }
                    state.drafts.forEach { draft ->
                        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                            Text(draft.draftId + " · " + draft.label, style = MaterialTheme.typography.bodySmall)
                            Text(draft.detail, style = MaterialTheme.typography.labelSmall, color = Color(0xFF5F6B63))
                        }
                    }
                }

                HorizontalDivider(color = Color(0xFFE1E6E1))
                Text("发送（真节点）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                Text(
                    "节点地址只保存不联网；未配置或未加入群时会显式失败，不会伪造成功。",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF5F6B63),
                )
                OutlinedTextField(
                    value = state.sendNodeUrl,
                    onValueChange = handlers.updateSendNodeUrl,
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                    label = { Text("节点地址，例如 http://127.0.0.1:8080") },
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedTextField(
                        value = state.sendNodeId,
                        onValueChange = handlers.updateSendNodeId,
                        modifier = Modifier.weight(1f),
                        singleLine = true,
                        label = { Text("节点 ID") },
                    )
                    OutlinedTextField(
                        value = state.sendNodePublicKey,
                        onValueChange = handlers.updateSendNodePublicKey,
                        modifier = Modifier.weight(1f),
                        singleLine = true,
                        label = { Text("节点公钥（可选）") },
                    )
                }
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    TextButton(
                        onClick = handlers.configureSendNode,
                        enabled = state.ready && !state.busy && state.sendNodeUrl.isNotBlank() && state.sendNodeId.isNotBlank(),
                    ) { Text("保存节点配置", style = MaterialTheme.typography.labelMedium) }
                    TextButton(
                        onClick = handlers.sendPrepare,
                        enabled = state.ready && !state.busy && state.drafts.isNotEmpty(),
                    ) { Text("准备并加密", style = MaterialTheme.typography.labelMedium) }
                    TextButton(
                        onClick = handlers.sendDispatch,
                        enabled = state.ready && !state.busy && state.drafts.isNotEmpty(),
                    ) { Text("发送", style = MaterialTheme.typography.labelMedium) }
                    TextButton(
                        onClick = handlers.sendRetry,
                        enabled = state.ready && !state.busy && state.drafts.isNotEmpty(),
                    ) { Text("重试", style = MaterialTheme.typography.labelMedium) }
                    TextButton(
                        onClick = handlers.sendReconcile,
                        enabled = state.ready && !state.busy && state.drafts.isNotEmpty(),
                    ) { Text("对账", style = MaterialTheme.typography.labelMedium) }
                }
                state.sendStep?.let { step ->
                    Text(
                        "结果：" + step.status + if (step.settled) "（已确定）" else "（未确定，只能对账）",
                        style = MaterialTheme.typography.bodySmall,
                        fontWeight = FontWeight.Medium,
                        color = if (step.status == "accepted") Color(0xFF2E7D32) else Color(0xFF5F6B63),
                    )
                    Text(
                        "提交 " + (step.submissionId ?: "-") + " · 尝试序号 " + (step.attemptIndex ?: 0) + " · " + step.detail,
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF5F6B63),
                    )
                }

                HorizontalDivider(color = Color(0xFFE1E6E1))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text("核心事件（拉取式）", style = MaterialTheme.typography.labelLarge)
                    TextButton(
                        onClick = handlers.pollEvents,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "轮询核心事件" },
                    ) { Text("拉取事件", style = MaterialTheme.typography.labelMedium) }
                    Text(
                        "游标 " + state.nextEventSeq + if (state.resyncRequired) "（需要整体重查）" else "",
                        style = MaterialTheme.typography.labelSmall,
                        color = if (state.resyncRequired) Color(0xFFB3261E) else Color(0xFF5F6B63),
                    )
                }
                if (state.events.isEmpty()) {
                    Text("还没有事件；新建草稿/取消草稿后会出现。", style = MaterialTheme.typography.bodySmall)
                } else {
                    state.events.takeLast(6).forEach { event ->
                        Column(verticalArrangement = Arrangement.spacedBy(1.dp)) {
                            Text(event.title, style = MaterialTheme.typography.bodySmall, fontWeight = FontWeight.Medium)
                            Text(event.detail, style = MaterialTheme.typography.labelSmall, color = Color(0xFF5F6B63))
                        }
                    }
                }
            }
        }
    }
}
