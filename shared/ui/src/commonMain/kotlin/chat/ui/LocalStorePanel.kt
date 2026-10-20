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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
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
    /** 打开命令面板（插件 palette.commands 槽位）。 */
    val openPalette: () -> Unit = {},
    /** 插件声明的 composer.actions（插件 id / 动作名 / 说明）。 */
    val pluginComposerActions: () -> List<Triple<String, String, String>> = { emptyList() },
    /** 运行某个插件 composer 动作；结果由宿主决定如何进草稿。 */
    val runPluginComposerAction: (String, String) -> Unit = { _, _ -> },
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
    val syncStart: () -> Unit = {},
    val syncStop: () -> Unit = {},
    val syncRefresh: () -> Unit = {},
    val syncOnce: () -> Unit = {},
    val loadMessageAttachments: () -> Unit = {},
    val exportCard: (String) -> Unit = {},
    val downloadCard: (String) -> Unit = {},
    val notify: (String) -> Unit = {},
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
                // 选区感知：只有记住 TextFieldValue 才能对"选中文字"施加格式。
                var draft by remember { mutableStateOf(TextFieldValue(draftText, TextRange(draftText.length))) }
                // 外部清空（写入草稿成功后）要同步回来。
                if (draft.text != draftText) {
                    draft = TextFieldValue(draftText, TextRange(draftText.length))
                }
                fun applyFormat(marker: String) {
                    val edit = applyMarker(draft.text, draft.selection, marker)
                    draft = TextFieldValue(edit.text, edit.selection)
                    handlers.updateDraftText(edit.text)
                }
                MarkdownFormatBar(
                    onFormat = { applyFormat(it) },
                    enabled = state.ready && !state.busy,
                )
                TextButton(
                    onClick = handlers.openPalette,
                    enabled = state.ready && !state.busy,
                    modifier = Modifier.semantics { contentDescription = "打开命令面板" },
                ) { Text("命令面板", style = MaterialTheme.typography.labelMedium) }
                // 插件槽位 composer.actions：只放按钮，布局仍由宿主决定（插件不接管 UI）。
                val pluginActions = handlers.pluginComposerActions()
                if (pluginActions.isNotEmpty()) {
                    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        pluginActions.forEach { (pluginId, name, description) ->
                            TextButton(
                                onClick = { handlers.runPluginComposerAction(pluginId, name) },
                                enabled = state.ready && !state.busy,
                                modifier = Modifier.semantics { contentDescription = "插件动作：" + description },
                            ) { Text("插件：" + description, style = MaterialTheme.typography.labelSmall) }
                        }
                    }
                }
                OutlinedTextField(
                    value = draft,
                    onValueChange = { value ->
                        draft = value
                        handlers.updateDraftText(value.text)
                    },
                    modifier = Modifier
                        .fillMaxWidth()
                        .semantics { contentDescription = "本机草稿正文" }
                        .onPreviewKeyEvent { event ->
                            if (event.type != KeyEventType.KeyDown || !event.isCtrlPressed) {
                                false
                            } else when (event.key) {
                                Key.B -> { applyFormat(MARKER_BOLD); true }
                                Key.I -> { applyFormat(MARKER_ITALIC); true }
                                else -> false
                            }
                        },
                    minLines = 2,
                    maxLines = 4,
                    label = { Text("草稿正文（支持 Markdown，最多 8 KiB；Ctrl+B 加粗 / Ctrl+I 斜体）") },
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
                Text("自动同步（常驻循环）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                Text(
                    "start 只在本地循环拉取并验签落库；它不保证对方在线或消息已投递。",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF5F6B63),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    TextButton(
                        onClick = handlers.syncStart,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "启动常驻同步" },
                    ) { Text("启动同步", style = MaterialTheme.typography.labelMedium) }
                    TextButton(
                        onClick = handlers.syncStop,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "停止常驻同步" },
                    ) { Text("停止同步", style = MaterialTheme.typography.labelMedium) }
                    TextButton(
                        onClick = handlers.syncOnce,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "只拉一轮并应用" },
                    ) { Text("收一轮", style = MaterialTheme.typography.labelMedium) }
                    TextButton(
                        onClick = handlers.syncRefresh,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "刷新同步状态" },
                    ) { Text("刷新状态", style = MaterialTheme.typography.labelMedium) }
                    TextButton(
                        onClick = { handlers.notify("同步已完成（测试通知）") },
                        enabled = state.ready,
                        modifier = Modifier.semantics { contentDescription = "发送一条测试系统通知" },
                    ) { Text("测试通知", style = MaterialTheme.typography.labelMedium) }
                }
                state.syncOnce?.let { once ->
                    Text(
                        "本轮：拉取 " + once.fetched + " 条 · 落库 " + once.applied + " 条 · 跳过 " + once.skipped +
                            " 条 · 游标 " + once.nextCursor + (if (once.settled) " · 已追平" else " · 未追平"),
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                state.syncStatus?.let { status ->
                    Text(
                        "状态 " + status.state + " · 已应用 " + status.appliedTotal + " 条 · 轮次 " + status.rounds,
                        style = MaterialTheme.typography.bodySmall,
                        fontWeight = FontWeight.Medium,
                    )
                    Text(
                        (status.lastErrorCode?.let { "错误 " + it + " · " } ?: "") +
                            (status.stoppedReason?.let { "停止原因 " + it } ?: "") +
                            if (status.resyncRequired) "需要追赶（resync）" else "",
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF5F6B63),
                    )
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
                HorizontalDivider(color = Color(0xFFE1E6E1))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text("消息附件卡片", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                    TextButton(
                        onClick = handlers.loadMessageAttachments,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "读取当前会话消息的附件卡片" },
                    ) { Text("读取附件", style = MaterialTheme.typography.labelMedium) }
                }
                if (state.messageCards.isEmpty()) {
                    Text("还没有读取；读取后按消息展示附件（名称/大小/状态）。", style = MaterialTheme.typography.bodySmall)
                } else {
                    state.messageCards.entries.take(6).forEach { (messageId, cards) ->
                        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                            val preview = state.messagePreviews[messageId]
                            Text(
                                if (preview.isNullOrBlank()) {
                                    "消息 " + messageId.take(14) + "…"
                                } else {
                                    "消息「" + preview + "」"
                                },
                                style = MaterialTheme.typography.labelSmall,
                                color = Color(0xFF5F6B63),
                            )
                            if (cards.isEmpty()) {
                                Text("（无附件）", style = MaterialTheme.typography.labelSmall, color = Color(0xFF5F6B63))
                            } else {
                                cards.forEach { card ->
                                    Row(
                                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                                        verticalAlignment = Alignment.CenterVertically,
                                    ) {
                                        Text(
                                            card.displayName + " · " + card.sizeLabel + " · " + card.stateLabel +
                                                if (card.downloadable) "" else " · 不可下载",
                                            style = MaterialTheme.typography.bodySmall,
                                        )
                                        TextButton(
                                            onClick = { handlers.downloadCard(card.attachmentId) },
                                            enabled = state.ready && !state.busy && !card.downloadable,
                                            modifier = Modifier.semantics {
                                                contentDescription = "从节点下载附件 " + card.displayName
                                            },
                                        ) { Text("从节点下载", style = MaterialTheme.typography.labelSmall) }
                                        TextButton(
                                            onClick = { handlers.exportCard(card.attachmentId) },
                                            enabled = state.ready && !state.busy,
                                            modifier = Modifier.semantics {
                                                contentDescription = "导出附件 " + card.displayName
                                            },
                                        ) { Text("导出", style = MaterialTheme.typography.labelSmall) }
                                    }
                                }
                            }
                        }
                    }
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
