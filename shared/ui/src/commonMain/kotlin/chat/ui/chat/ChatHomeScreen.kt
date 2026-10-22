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
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import chat.presentation.display.renderMarkdown
import chat.ui.MarkdownMessage

/** 左侧会话列表的一项。 */
data class ChatConversationItem(
    val id: String,
    val title: String,
    val subtitle: String,
)

/** 消息气泡的一项。 */
data class ChatMessageItem(
    val id: String,
    val mine: Boolean,
    val label: String,
    val body: String,
    val status: String,
)

/** 真实聊天界面状态（不做业务判断：语义来自 core/presentation）。 */
/** 联系人一行（来自本机身份库，不可发送时如实标注状态）。 */
data class ChatContactItem(
    val id: String,
    val name: String,
    val fingerprint: String,
    val state: String,
    val canSend: Boolean,
)

/** 群组一行。 */
data class ChatGroupItem(
    val id: String,
    val name: String,
    val epoch: Int,
    val role: String,
    val members: Int,
)

/** 群详情里的一位成员。 */
data class ChatGroupMemberItem(
    val credential: String,
    val role: String,
    val muted: Boolean,
    val isLocal: Boolean,
    val pending: Boolean,
)

/** 群详情（打开后才加载；null = 未打开）。 */
data class ChatGroupDetail(
    val id: String,
    val name: String,
    val epoch: Int,
    val role: String,
    val members: List<ChatGroupMemberItem>,
    val pendingCount: Int,
)

data class ChatHomeState(
    val loading: Boolean = true,
    val groupDetail: ChatGroupDetail? = null,
    val tab: String = "conversations",
    val contacts: List<ChatContactItem> = emptyList(),
    val groups: List<ChatGroupItem> = emptyList(),
    val identityLines: List<String> = emptyList(),
    val conversations: List<ChatConversationItem> = emptyList(),
    val activeId: String? = null,
    val messages: List<ChatMessageItem> = emptyList(),
    val composer: String = "",
    val notice: String = "",
    val error: String? = null,
    val busy: Boolean = false,
    val nodeLabel: String = "节点未配置",
)

class ChatHomeHandlers(
    val selectTab: (String) -> Unit = {},
    val openGroup: (String) -> Unit = {},
    val closeGroup: () -> Unit = {},
    val renameGroup: (String) -> Unit = {},
    val removeMember: (String) -> Unit = {},
    val acceptPending: () -> Unit = {},
    val selectConversation: (String) -> Unit = {},
    val createInvite: () -> Unit = {},
    val composerChange: (String) -> Unit = {},
    val send: () -> Unit = {},
    val sync: () -> Unit = {},
    val composeNew: () -> Unit = {},
    val openSettings: () -> Unit = {},
)

/**
 * 正式聊天主界面：左会话列表 + 右消息流 + 底部输入。
 *
 * 语义边界：不做状态推断（不把 queued 画成已发送、不吞失败），
 * 展示全部来自 [ChatHomeState]，由应用层按内核返回值填充。
 */
@Composable
fun ChatHomeScreen(state: ChatHomeState, handlers: ChatHomeHandlers, modifier: Modifier = Modifier) {
    Surface(modifier = modifier.fillMaxSize(), color = Color(0xFFF6F7F5)) {
        Column(Modifier.fillMaxSize()) {
            Row(
                Modifier.fillMaxWidth().background(Color(0xFFEDF1EC)).padding(horizontal = 16.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Chat", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.width(16.dp))
                Text(
                    state.nodeLabel,
                    style = MaterialTheme.typography.labelMedium.copy(fontFamily = FontFamily.Monospace),
                    color = Color(0xFF5F6B63),
                )
                Spacer(Modifier.weight(1f))
                TextButton(onClick = handlers.sync, enabled = !state.busy) { Text("同步") }
                TextButton(onClick = handlers.openSettings) { Text("设置") }
            }
            HorizontalDivider(color = Color(0xFFDCE3DC))
            Row(Modifier.fillMaxSize()) {
                Column(Modifier.width(280.dp).fillMaxHeight().background(Color(0xFFF2F4F1))) {
                    Row(
                        Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        for ((key, label) in listOf("conversations" to "会话", "contacts" to "联系人", "groups" to "群组")) {
                            val active = state.tab == key
                            TextButton(onClick = { handlers.selectTab(key) }) {
                                Text(
                                    label,
                                    style = MaterialTheme.typography.labelMedium,
                                    fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
                                )
                            }
                        }
                        Spacer(Modifier.weight(1f))
                        if (state.tab == "conversations") {
                            TextButton(onClick = handlers.composeNew) { Text("新对话", style = MaterialTheme.typography.labelMedium) }
                        }
                        if (state.tab == "contacts") {
                            TextButton(onClick = handlers.createInvite) { Text("邀请", style = MaterialTheme.typography.labelMedium) }
                        }
                    }
                    if (state.conversations.isEmpty() && !state.loading) {
                        Text(
                            "还没有会话。点「新对话」发第一条消息。",
                            style = MaterialTheme.typography.bodySmall,
                            color = Color(0xFF7A857C),
                            modifier = Modifier.padding(horizontal = 12.dp),
                        )
                    }
                    when (state.tab) {
                        "contacts" -> LazyColumn(Modifier.fillMaxSize()) {
                            if (state.contacts.isEmpty()) {
                                item {
                                    Text(
                                        "还没有联系人。点「邀请」生成口令发给对方。",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = Color(0xFF7A857C),
                                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                                    )
                                }
                            }
                            items(state.contacts) { contact ->
                                Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp)) {
                                    Text(contact.name, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                                    Text(
                                        chat.presentation.display.shortFingerprint(contact.fingerprint),
                                        style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                        color = Color(0xFF6C776E),
                                    )
                                    Text(
                                        contact.state + (if (contact.canSend) "" else "（暂不可发送）"),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = if (contact.canSend) Color(0xFF3E6B4A) else Color(0xFF9A6A00),
                                    )
                                }
                                HorizontalDivider(color = Color(0xFFE3E8E2))
                            }
                        }
                        "groups" -> LazyColumn(Modifier.fillMaxSize()) {
                            items(state.groups) { group ->
                                Column(
                                    Modifier
                                        .fillMaxWidth()
                                        .clickable { handlers.openGroup(group.id) }
                                        .padding(horizontal = 12.dp, vertical = 10.dp),
                                ) {
                                    Text(group.name, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                                    Text(
                                        "epoch " + group.epoch + " · 成员 " + group.members + " · " + group.role,
                                        style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                        color = Color(0xFF6C776E),
                                    )
                                }
                                HorizontalDivider(color = Color(0xFFE3E8E2))
                            }
                        }
                        else -> LazyColumn(Modifier.fillMaxSize()) {
                            if (state.conversations.isEmpty() && !state.loading) {
                                item {
                                    Text(
                                        "还没有会话。点「新对话」发第一条消息。",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = Color(0xFF7A857C),
                                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                                    )
                                }
                            }
                            items(state.conversations) { item ->
                                val selected = item.id == state.activeId
                                Column(
                                    Modifier
                                        .fillMaxWidth()
                                        .background(if (selected) Color(0xFFDDE7DC) else Color.Transparent)
                                        .clickable { handlers.selectConversation(item.id) }
                                        .padding(horizontal = 12.dp, vertical = 10.dp),
                                ) {
                                    Text(item.title, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                                    Text(
                                        item.subtitle,
                                        style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                        color = Color(0xFF6C776E),
                                    )
                                }
                                HorizontalDivider(color = Color(0xFFE3E8E2))
                            }
                        }
                    }
                }
                HorizontalDivider(color = Color(0xFFDCE3DC))
                Column(Modifier.weight(1f).fillMaxHeight()) {
                    if (state.error != null) {
                        Text(
                            state.error,
                            style = MaterialTheme.typography.bodySmall,
                            color = Color(0xFFB3261E),
                            modifier = Modifier.fillMaxWidth().padding(12.dp),
                        )
                    }
                    val listState = rememberLazyListState()
                    LaunchedEffect(state.messages.size, state.activeId) {
                        if (state.messages.isNotEmpty()) listState.animateScrollToItem(state.messages.size - 1)
                    }
                    LazyColumn(
                        state = listState,
                        modifier = Modifier.weight(1f).fillMaxWidth().padding(horizontal = 16.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        items(state.messages) { message ->
                            Row(
                                Modifier.fillMaxWidth(),
                                horizontalArrangement = if (message.mine) Arrangement.End else Arrangement.Start,
                            ) {
                                Column(
                                    Modifier
                                        .width(420.dp)
                                        .background(if (message.mine) Color(0xFFDCEBDD) else Color(0xFFFFFFFF))
                                        .padding(10.dp),
                                ) {
                                    Text(
                                        message.label + "  " + message.status,
                                        style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                                        color = Color(0xFF6C776E),
                                    )
                                    // 真实聊天里也走 Markdown 安全子集渲染（与预览一致：无 HTML、链接不可点）。
                                    MarkdownMessage(document = renderMarkdown(message.body))
                                }
                            }
                        }
                    }
                    if (state.notice.isNotEmpty()) {
                        Text(
                            state.notice,
                            style = MaterialTheme.typography.labelSmall,
                            color = Color(0xFF5F6B63),
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                        )
                    }
                    Row(Modifier.fillMaxWidth().padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
                        OutlinedTextField(
                            value = state.composer,
                            onValueChange = handlers.composerChange,
                            modifier = Modifier.weight(1f),
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
        }
    }
}
/** 群详情对话框：成员列表 + 改群名 + 移除成员 + 接受待处理变更。 */
@Composable
fun GroupDetailDialog(
    detail: ChatGroupDetail,
    handlers: ChatHomeHandlers,
    onClose: () -> Unit,
) {
    var name by remember { mutableStateOf(detail.name) }
    AlertDialog(
        onDismissRequest = onClose,
        title = { Text("群组 · " + detail.name) },
        text = {
            Column(Modifier.width(560.dp)) {
                Text(
                    "epoch " + detail.epoch + " · 我的角色 " + detail.role + " · 成员 " + detail.members.size,
                    style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                    color = Color(0xFF6C776E),
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
                    Spacer(Modifier.height(8.dp))
                    Text(
                        "有 " + detail.pendingCount + " 条待处理变更（他人提案需要本机打包）",
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF9A6A00),
                    )
                    TextButton(onClick = { handlers.acceptPending() }) { Text("接受待处理变更") }
                }
                Spacer(Modifier.height(8.dp))
                LazyColumn(Modifier.height(240.dp).fillMaxWidth()) {
                    items(detail.members) { member ->
                        Row(
                            Modifier.fillMaxWidth().padding(vertical = 6.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(
                                    chat.presentation.display.shortFingerprint(member.credential),
                                    style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                                )
                                Text(
                                    member.role +
                                        (if (member.isLocal) " · 本机" else "") +
                                        (if (member.muted) " · 已禁言" else "") +
                                        (if (member.pending) " · 待生效" else ""),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = Color(0xFF6C776E),
                                )
                            }
                            if (!member.isLocal) {
                                TextButton(onClick = { handlers.removeMember(member.credential) }) { Text("移除") }
                            }
                        }
                        HorizontalDivider(color = Color(0xFFE3E8E2))
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onClose) { Text("关闭") } },
    )
}
