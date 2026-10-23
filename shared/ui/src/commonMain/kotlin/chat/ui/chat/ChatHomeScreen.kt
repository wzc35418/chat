package chat.ui.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import chat.presentation.display.renderMarkdown
import chat.presentation.display.shortFingerprint
import chat.ui.MarkdownMessage

data class ChatConversationItem(val id: String, val title: String, val subtitle: String)

data class ChatContactItem(
    val id: String,
    val name: String,
    val fingerprint: String,
    val state: String,
    val canSend: Boolean,
)

data class ChatGroupItem(val id: String, val name: String, val epoch: Int, val role: String, val members: Int)

data class ChatGroupMemberItem(
    val credential: String,
    val role: String,
    val muted: Boolean,
    val isLocal: Boolean,
    val pending: Boolean,
)

data class ChatGroupDetail(
    val id: String,
    val name: String,
    val epoch: Int,
    val role: String,
    val members: List<ChatGroupMemberItem>,
    val pendingCount: Int,
    /** 待处理提案（内核 GroupPending）：operation=add_members/remove_members/self_update。 */
    val pendingOperation: String = "",
    val pendingBaseEpoch: Int = 0,
    val pendingTargetEpoch: Int = 0,
    val pendingAdded: Int = 0,
    val pendingRemoved: Int = 0,
)

data class ChatMessageItem(
    val id: String,
    val mine: Boolean,
    val label: String,
    val body: String,
    val status: String,
)

data class ChatHomeState(
    val loading: Boolean = true,
    val tab: String = "conversations",
    val conversations: List<ChatConversationItem> = emptyList(),
    val contacts: List<ChatContactItem> = emptyList(),
    val groups: List<ChatGroupItem> = emptyList(),
    val identityLines: List<String> = emptyList(),
    val activeId: String? = null,
    val messages: List<ChatMessageItem> = emptyList(),
    val composer: String = "",
    val notice: String = "",
    val error: String? = null,
    val busy: Boolean = false,
    val nodeLabel: String = "节点未配置",
    val revision: String = "",
    val groupDetail: ChatGroupDetail? = null,
    val diagnostics: List<String>? = null,
    /** 失败草稿：失败后仍可重试/对账，避免用户以为只能重发新消息。 */
    val failedDraftId: String? = null,
    /** 接受邀请对话框状态（null = 关闭）。 */
    val invite: ChatInviteState? = null,
    val pairing: ChatPairingState? = null,
)

/** 接受邀请（加好友）对话框状态。 */
data class ChatInviteState(
    val token: String = "",
    val displayName: String = "",
    val result: List<String> = emptyList(),
    val busy: Boolean = false,
)

/** 配对（入群）对话框状态：三个步骤各一块文本，结果逐行显示。 */
data class ChatPairingState(
    val myKeyPackage: String = "",
    val peerKeyPackage: String = "",
    val invitePack: String = "",
    val result: List<String> = emptyList(),
    val busy: Boolean = false,
)

class ChatHomeHandlers(
    val selectTab: (String) -> Unit = {},
    val selectConversation: (String) -> Unit = {},
    val composerChange: (String) -> Unit = {},
    val send: () -> Unit = {},
    val sync: () -> Unit = {},
    val composeNew: () -> Unit = {},
    val openSettings: () -> Unit = {},
    val createInvite: () -> Unit = {},
    val openGroup: (String) -> Unit = {},
    val closeGroup: () -> Unit = { },
    val retryFailed: () -> Unit = { },
    val reconcileFailed: () -> Unit = { },
    val dismissFailed: () -> Unit = { },
    val openInviteDialog: () -> Unit = { },
    val closeInviteDialog: () -> Unit = { },
    val inviteTokenChange: (String) -> Unit = { },
    val inviteNameChange: (String) -> Unit = { },
    val invitePreview: () -> Unit = { },
    val inviteAccept: () -> Unit = { },
    val copyText: (String) -> Unit = { },
    val renameGroup: (String) -> Unit = {},
    val removeMember: (String) -> Unit = {},
    val acceptPending: () -> Unit = {},
    val diagnose: () -> Unit = { },
    val closeDiagnostics: () -> Unit = { },
    val openPairing: () -> Unit = { },
    val closePairing: () -> Unit = { },
    val pairingMyKeyPackage: () -> Unit = { },
    val pairingPeerKeyPackageChange: (String) -> Unit = { },
    val pairingCreateGroup: () -> Unit = { },
    val pairingInvitePackChange: (String) -> Unit = { },
    val pairingJoin: () -> Unit = { },
)

private val BgColor = Color(0xFFF6F7F5)
private val BarColor = Color(0xFFEDF1EC)
private val PanelColor = Color(0xFFF2F4F1)
private val LineColor = Color(0xFFDCE3DC)
private val MutedColor = Color(0xFF6C776E)

/**
 * 正式聊天主界面。
 *
 * 结构固定为四块，避免“某块没画出来就整片空白”：
 * 顶栏（节点/同步/设置）｜左栏（页签列表 300dp）｜右栏（标题 + 消息/空状态 + 输入）。
 */
@Composable
fun ChatHomeScreen(state: ChatHomeState, handlers: ChatHomeHandlers, modifier: Modifier = Modifier) {
    Surface(modifier.fillMaxSize(), color = BgColor) {
        Column(Modifier.fillMaxSize()) {
            TopBar(state, handlers)
            HorizontalDivider(color = LineColor)
            Row(Modifier.fillMaxWidth().weight(1f)) {
                LeftPane(state, handlers)
                // 注意：Row 里必须用 VerticalDivider；HorizontalDivider 会撑满整行宽度，把右栏挤成 0 宽（这就是“右边空白”）
                VerticalDivider(color = LineColor)
                RightPane(state = state, handlers = handlers, modifier = Modifier.weight(1f).fillMaxHeight())
            }
        }
    }
    state.groupDetail?.let { detail ->
        GroupDetailDialog(detail, handlers)
    }
    state.diagnostics?.let { lines ->
        DiagnosticsDialog(lines, handlers.closeDiagnostics)
    }
    state.pairing?.let { pairing ->
        PairingDialog(pairing, handlers)
    }
    state.invite?.let { invite ->
        AcceptInviteDialog(invite, handlers)
    }
}

@Composable
private fun TopBar(state: ChatHomeState, handlers: ChatHomeHandlers) {
    Row(
        Modifier.fillMaxWidth().background(BarColor).padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("Chat", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.width(16.dp))
        Text(
            state.nodeLabel,
            style = MaterialTheme.typography.labelMedium.copy(fontFamily = FontFamily.Monospace),
            color = Color(0xFF5F6B63),
        )
        if (state.revision.isNotEmpty()) {
            Spacer(Modifier.width(10.dp))
            Text(
                "· " + state.revision,
                style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                color = Color(0xFF9AA39C),
            )
        }
        Spacer(Modifier.weight(1f))
        TextButton(onClick = handlers.openPairing) { Text("配对") }
        TextButton(onClick = handlers.diagnose) { Text("自检") }
        TextButton(onClick = handlers.sync, enabled = !state.busy) { Text("同步") }
        TextButton(onClick = handlers.openSettings) { Text("设置") }
    }
}

@Composable
private fun LeftPane(state: ChatHomeState, handlers: ChatHomeHandlers) {
    Column(Modifier.width(300.dp).fillMaxHeight().background(PanelColor)) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            for ((key, label) in listOf("conversations" to "会话", "contacts" to "联系人", "groups" to "群组")) {
                TextButton(onClick = { handlers.selectTab(key) }) {
                    Text(
                        label,
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = if (state.tab == key) FontWeight.SemiBold else FontWeight.Normal,
                    )
                }
            }
        }
        HorizontalDivider(color = LineColor)
        Text(
            when (state.tab) {
                "contacts" -> if (state.contacts.isEmpty()) "还没有联系人" else "联系人 " + state.contacts.size
                "groups" -> if (state.groups.isEmpty()) "还没有群组" else "群组 " + state.groups.size
                else -> if (state.conversations.isEmpty()) "还没有会话" else "会话 " + state.conversations.size
            },
            style = MaterialTheme.typography.labelSmall,
            color = MutedColor,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
        )
        LazyColumn(Modifier.fillMaxWidth().weight(1f)) {
            if (state.tab == "conversations") {
                if (state.activeId != null && state.conversations.none { it.id == state.activeId }) {
                    item {
                        Column(Modifier.fillMaxWidth().background(Color(0xFFDDE7DC)).padding(12.dp)) {
                            Text("新对话（未发送）", style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                            Text(
                                shortFingerprint(state.activeId ?: ""),
                                style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                color = MutedColor,
                            )
                        }
                        HorizontalDivider(color = LineColor)
                    }
                }
                items(state.conversations) { item ->
                    val selected = item.id == state.activeId
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .background(if (selected) Color(0xFFDDE7DC) else Color.Transparent)
                            .clickable { handlers.selectConversation(item.id) }
                            .padding(12.dp),
                    ) {
                        Text(item.title, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                        Text(
                            item.subtitle,
                            style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                            color = MutedColor,
                        )
                    }
                    HorizontalDivider(color = LineColor)
                }
            } else if (state.tab == "contacts") {
                items(state.contacts) { contact ->
                    Column(Modifier.fillMaxWidth().padding(12.dp)) {
                        Text(contact.name, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                        Text(
                            shortFingerprint(contact.fingerprint),
                            style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                            color = MutedColor,
                        )
                        Text(
                            contact.state + (if (contact.canSend) "" else "（暂不可发送）"),
                            style = MaterialTheme.typography.labelSmall,
                            color = if (contact.canSend) Color(0xFF3E6B4A) else Color(0xFF9A6A00),
                        )
                    }
                    HorizontalDivider(color = LineColor)
                }
            } else {
                items(state.groups) { group ->
                    Column(Modifier.fillMaxWidth().clickable { handlers.openGroup(group.id) }.padding(12.dp)) {
                        Text(group.name, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                        Text(
                            "epoch " + group.epoch + " · 成员 " + group.members + " · " + group.role,
                            style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                            color = MutedColor,
                        )
                    }
                    HorizontalDivider(color = LineColor)
                }
            }
        }
        HorizontalDivider(color = LineColor)
        Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
            when (state.tab) {
                "contacts" -> {
                    TextButton(onClick = handlers.createInvite) { Text("生成邀请") }
                    TextButton(onClick = handlers.openInviteDialog) { Text("接受邀请") }
                }
                "groups" -> Text("点群可查看成员", style = MaterialTheme.typography.labelSmall, color = MutedColor)
                else -> TextButton(onClick = handlers.composeNew) { Text("新对话") }
            }
        }
    }
}

@Composable
private fun RightPane(state: ChatHomeState, handlers: ChatHomeHandlers, modifier: Modifier = Modifier) {
    val activeTitle = state.conversations.firstOrNull { it.id == state.activeId }?.title
        ?: if (state.activeId != null) "新对话（未发送）" else "未选择会话"
    Column(modifier) {
        Row(
            Modifier.fillMaxWidth().background(PanelColor).padding(horizontal = 16.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(activeTitle, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.width(12.dp))
            if (state.busy) {
                Text("处理中…", style = MaterialTheme.typography.labelSmall, color = Color(0xFF9A6A00))
            }
        }
        HorizontalDivider(color = LineColor)
        if (state.error != null) {
            Text(
                state.error,
                style = MaterialTheme.typography.bodySmall,
                color = Color(0xFFB3261E),
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
        if (state.messages.isEmpty()) {
            Column(
                Modifier.fillMaxWidth().weight(1f).padding(24.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    if (state.activeId == null) "先在左边选一个会话，或点左下角「新对话」" else "还没有消息",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MutedColor,
                    textAlign = TextAlign.Center,
                )
                Spacer(Modifier.height(6.dp))
                if (state.activeId != null) {
                    Text(
                        "在下面输入框写第一条消息，按 Enter 或点「发送」",
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF8A948C),
                        textAlign = TextAlign.Center,
                    )
                }
            }
        } else {
            val listState = rememberLazyListState()
            LaunchedEffect(state.messages.size, state.activeId) {
                listState.animateScrollToItem(state.messages.size - 1)
            }
            LazyColumn(
                state = listState,
                modifier = Modifier.fillMaxWidth().weight(1f).padding(horizontal = 16.dp, vertical = 8.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(state.messages) { message ->
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = if (message.mine) Arrangement.End else Arrangement.Start,
                    ) {
                        Column(
                            Modifier
                                .width(460.dp)
                                .background(if (message.mine) Color(0xFFDCEBDD) else Color.White)
                                .padding(10.dp),
                        ) {
                            Text(
                                message.label + "  " + message.status + (if (message.mine) "  · 已发送" else ""),
                                style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                color = if (message.mine) Color(0xFF3E6B4A) else MutedColor,
                            )
                            MarkdownMessage(document = renderMarkdown(message.body))
                        }
                    }
                }
            }
        }
        if (state.failedDraftId != null) {
            Row(
                Modifier.fillMaxWidth().background(Color(0xFFFFF4E5)).padding(horizontal = 16.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("上一条没发出去", style = MaterialTheme.typography.labelSmall, color = Color(0xFF9A6A00))
                Spacer(Modifier.width(12.dp))
                TextButton(onClick = handlers.retryFailed) { Text("重试") }
                TextButton(onClick = handlers.reconcileFailed) { Text("对账") }
                TextButton(onClick = handlers.dismissFailed) { Text("忽略") }
            }
        }
        if (state.notice.isNotEmpty()) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    state.notice,
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF5F6B63),
                    modifier = Modifier.weight(1f),
                )
                // 邀请口令、错误码都靠“复制”交给别人；没有复制按钮时用户只能手选文本。
                TextButton(onClick = { handlers.copyText(state.notice) }) { Text("复制提示") }
            }
        }
        HorizontalDivider(color = LineColor)
        val composerFocus = remember { FocusRequester() }
        LaunchedEffect(state.activeId) { runCatching { composerFocus.requestFocus() } }
        Row(Modifier.fillMaxWidth().padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = state.composer,
                onValueChange = handlers.composerChange,
                modifier = Modifier.weight(1f).focusRequester(composerFocus),
                placeholder = { Text("输入消息，Enter 发送") },
                maxLines = 4,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { handlers.send() }),
                enabled = !state.busy,
            )
            Spacer(Modifier.width(8.dp))
            Button(onClick = handlers.send, enabled = !state.busy && state.composer.isNotBlank()) { Text("发送") }
        }
    }
}

@Composable
private fun GroupDetailDialog(detail: ChatGroupDetail, handlers: ChatHomeHandlers) {
    var name by remember { mutableStateOf(detail.name) }
    AlertDialog(
        onDismissRequest = handlers.closeGroup,
        title = { Text("群组 · " + detail.name) },
        text = {
            Column(Modifier.width(560.dp)) {
                Text(
                    "epoch " + detail.epoch + " · 我的角色 " + detail.role + " · 成员 " + detail.members.size,
                    style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                    color = MutedColor,
                )
                Spacer(Modifier.height(8.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    OutlinedTextField(
                        value = name,
                        onValueChange = { name = it },
                        label = { Text("群名") },
                        modifier = Modifier.weight(1f),
                        singleLine = true,
                    )
                    Spacer(Modifier.width(8.dp))
                    Button(onClick = { handlers.renameGroup(name.trim()) }, enabled = name.trim().isNotEmpty()) { Text("改名") }
                }
                if (detail.pendingCount > 0) {
                    Text(
                        "待处理：" + detail.pendingOperation + "（base epoch " + detail.pendingBaseEpoch +
                            " → target " + detail.pendingTargetEpoch + "，新增 " + detail.pendingAdded +
                            " 人，移除 " + detail.pendingRemoved + " 人）",
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF9A6A00),
                    )
                    TextButton(onClick = handlers.acceptPending) { Text("接受待处理变更") }
                }
                Spacer(Modifier.height(6.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "群 ID：" + shortFingerprint(detail.id),
                        style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                        color = MutedColor,
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(onClick = { handlers.copyText(detail.id) }) { Text("复制群 ID") }
                }
                Text(
                    "说明：当前内核**没有**“把新成员加入已有群”的接口；要邀请别人，只能用「配对」新建一个包含对方的群。",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF7A857C),
                )
                LazyColumn(Modifier.fillMaxWidth().height(240.dp)) {
                    items(detail.members) { member ->
                        Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                            Column(Modifier.weight(1f)) {
                                Text(
                                    shortFingerprint(member.credential),
                                    style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                                )
                                Text(
                                    member.role +
                                        (if (member.isLocal) " · 本机" else "") +
                                        (if (member.muted) " · 已禁言" else "") +
                                        (if (member.pending) " · 待生效" else ""),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MutedColor,
                                )
                            }
                            if (!member.isLocal) {
                                TextButton(onClick = { handlers.removeMember(member.credential) }) { Text("移除") }
                            }
                        }
                        HorizontalDivider(color = LineColor)
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = handlers.closeGroup) { Text("关闭") } },
    )
}

/** 连接自检结果（真实网络请求的逐步结论，失败带稳定原因）。 */
@Composable
private fun DiagnosticsDialog(lines: List<String>, onClose: () -> Unit) {
    AlertDialog(
        onDismissRequest = onClose,
        title = { Text("连接自检") },
        text = {
            Column(Modifier.width(620.dp)) {
                if (lines.isEmpty()) {
                    Text("还没有结果", style = MaterialTheme.typography.bodySmall, color = MutedColor)
                }
                LazyColumn(Modifier.fillMaxWidth().height(320.dp)) {
                    items(lines) { line ->
                        Text(
                            line,
                            style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                            color = when {
                                line.startsWith("FAIL") -> Color(0xFFB3261E)
                                line.startsWith("PASS") -> Color(0xFF3E6B4A)
                                else -> MutedColor
                            },
                            modifier = Modifier.padding(vertical = 2.dp),
                        )
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onClose) { Text("关闭") } },
    )
}

/** 配对对话框：导出我的 KeyPackage → 用对方 KeyPackage 建群 → 用邀请包入群。 */
@Composable
private fun PairingDialog(pairing: ChatPairingState, handlers: ChatHomeHandlers) {
    AlertDialog(
        onDismissRequest = handlers.closePairing,
        title = { Text("配对：两个人进同一个群") },
        text = {
            Column(Modifier.width(680.dp)) {
                Text(
                    "① 点「导出我的 KeyPackage」→ 把文本发给对方\n" +
                        "② 对方粘贴后用「建群」→ 把邀请包发回给你\n" +
                        "③ 你粘贴邀请包点「入群」",
                    style = MaterialTheme.typography.labelSmall,
                    color = MutedColor,
                )
                Spacer(Modifier.height(8.dp))
                Button(onClick = handlers.pairingMyKeyPackage, enabled = !pairing.busy) { Text("① 导出我的 KeyPackage") }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (pairing.myKeyPackage.isNotEmpty()) {
                        TextButton(onClick = { handlers.copyText(pairing.myKeyPackage) }) { Text("复制我的 KeyPackage") }
                    }
                }
                OutlinedTextField(
                    value = pairing.myKeyPackage,
                    onValueChange = { },
                    label = { Text("我的 KeyPackage（复制给对方）") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 3,
                    maxLines = 5,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = pairing.peerKeyPackage,
                    onValueChange = handlers.pairingPeerKeyPackageChange,
                    label = { Text("对方的 KeyPackage（粘贴到这里）") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 3,
                    maxLines = 5,
                )
                Spacer(Modifier.height(4.dp))
                Button(onClick = handlers.pairingCreateGroup, enabled = !pairing.busy) { Text("② 用对方 KeyPackage 建群 → 生成邀请包") }
                if (pairing.invitePack.isNotEmpty()) {
                    TextButton(onClick = { handlers.copyText(pairing.invitePack) }) { Text("复制邀请包") }
                }
                OutlinedTextField(
                    value = pairing.invitePack,
                    onValueChange = handlers.pairingInvitePackChange,
                    label = { Text("邀请包（粘贴到这里，或复制给对方）") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 3,
                    maxLines = 5,
                )
                Spacer(Modifier.height(4.dp))
                Button(onClick = handlers.pairingJoin, enabled = !pairing.busy) { Text("③ 用邀请包入群") }
                if (pairing.result.isNotEmpty()) {
                    Spacer(Modifier.height(8.dp))
                    LazyColumn(Modifier.fillMaxWidth().height(120.dp)) {
                        items(pairing.result) { line ->
                            Text(
                                line,
                                style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                color = if (line.startsWith("FAIL")) Color(0xFFB3261E) else Color(0xFF3E6B4A),
                            )
                        }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = handlers.closePairing) { Text("关闭") } },
    )
}

/** 接受邀请：粘贴口令 → 预览 → 接受（建立联系人）。 */
@Composable
private fun AcceptInviteDialog(invite: ChatInviteState, handlers: ChatHomeHandlers) {
    AlertDialog(
        onDismissRequest = handlers.closeInviteDialog,
        title = { Text("接受邀请（加好友）") },
        text = {
            Column(Modifier.width(620.dp)) {
                Text(
                    "把对方给你的邀请口令（inv1.… ）粘贴到这里，先「预览」确认邀请人，再「接受」。",
                    style = MaterialTheme.typography.labelSmall,
                    color = MutedColor,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = invite.token,
                    onValueChange = handlers.inviteTokenChange,
                    label = { Text("邀请口令") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 2,
                    maxLines = 4,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = invite.displayName,
                    onValueChange = handlers.inviteNameChange,
                    label = { Text("给对方起的名字（可留空）") },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                )
                Spacer(Modifier.height(8.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    TextButton(onClick = handlers.invitePreview, enabled = !invite.busy) { Text("预览") }
                    TextButton(onClick = handlers.inviteAccept, enabled = !invite.busy) { Text("接受") }
                }
                if (invite.result.isNotEmpty()) {
                    LazyColumn(Modifier.fillMaxWidth().height(120.dp)) {
                        items(invite.result) { line ->
                            Text(
                                line,
                                style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                color = if (line.startsWith("FAIL")) Color(0xFFB3261E) else Color(0xFF3E6B4A),
                            )
                        }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = handlers.closeInviteDialog) { Text("关闭") } },
    )
}
