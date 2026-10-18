package chat.presentation.preview

import chat.api.StoredConversation
import chat.api.StoredDraft
import chat.api.StoredMessage
import chat.presentation.display.ContactUi
import chat.presentation.display.ConversationSummaryView
import chat.presentation.display.DraftPhase
import chat.presentation.display.DraftUiState
import chat.presentation.display.MessageDisplayView
import chat.presentation.display.presentDraft
import chat.presentation.display.toConversationDisplayPage
import chat.presentation.display.toDraftEvidence
import chat.presentation.display.toMessageDisplayPage

/**
 * 预览里的“本机存储（真实）”状态：来自 Rust formal store 的真实加密库内容。
 *
 * 诚实边界：这是本机真实数据，但没有 MLS 发送链路——草稿只会停在 queued/prepared，
 * 会话与消息只在写入“已验证条目”后才出现。界面不会把草稿显示成已发送。
 */
data class PreviewStoreState(
    val ready: Boolean = false,
    val busy: Boolean = false,
    val conversations: List<ConversationSummaryView> = emptyList(),
    val selectedConversationId: String? = null,
    val messages: List<MessageDisplayView> = emptyList(),
    val drafts: List<DraftUiState> = emptyList(),
    val nextConversationCursor: String? = null,
    val nextMessageCursor: String? = null,
    val notice: String? = null,
    val lastErrorCode: String? = null,
    val databasePath: String? = null,
)

fun PreviewStoreState.opened(databasePath: String, notice: String): PreviewStoreState = copy(
    ready = true,
    busy = false,
    databasePath = databasePath,
    notice = notice,
    lastErrorCode = null,
)

fun PreviewStoreState.withError(code: String, message: String? = null): PreviewStoreState = copy(
    busy = false,
    lastErrorCode = code,
    notice = message ?: "本机存储操作没有成功；界面不会把失败当成功。",
)

fun PreviewStoreState.beginBusy(): PreviewStoreState = copy(busy = true, notice = null)

fun PreviewStoreState.withConversations(cursor: String?, items: List<StoredConversation>): PreviewStoreState {
    val page = chat.api.StoredPage(items, cursor).toConversationDisplayPage()
    val merged = LinkedHashMap<String, ConversationSummaryView>()
    conversations.forEach { merged[it.id] = it }
    page.items.forEach { merged[it.id] = it }
    return copy(
        conversations = merged.values.toList(),
        nextConversationCursor = page.nextCursor,
        busy = false,
        notice = if (page.items.isEmpty()) "本机库里还没有会话（写入已验证条目后才会出现）。" else null,
        lastErrorCode = null,
    )
}

fun PreviewStoreState.selecting(conversationId: String): PreviewStoreState = copy(
    selectedConversationId = conversationId,
    messages = emptyList(),
    nextMessageCursor = null,
    notice = null,
    lastErrorCode = null,
)

fun PreviewStoreState.withMessages(conversationId: String, cursor: String?, items: List<StoredMessage>): PreviewStoreState {
    if (selectedConversationId != conversationId) return this
    val page = chat.api.StoredPage(items, cursor).toMessageDisplayPage()
    val merged = LinkedHashMap<String, MessageDisplayView>()
    messages.forEach { merged[it.id] = it }
    page.items.forEach { merged[it.id] = it }
    return copy(
        messages = merged.values.toList(),
        nextMessageCursor = page.nextCursor,
        busy = false,
        lastErrorCode = null,
    )
}

/** 记录一条刚创建的真实草稿；状态只用 DraftEvidence 投影，绝不显示已接受/已送达。 */
fun PreviewStoreState.withDraft(draft: StoredDraft): PreviewStoreState {
    val evidence = draft.toDraftEvidence()
    val ui = presentDraft(evidence)
    val merged = LinkedHashMap<String, DraftUiState>()
    drafts.forEach { merged[it.draftId] = it }
    merged[draft.draftId] = ui
    return copy(
        drafts = merged.values.toList(),
        busy = false,
        notice = "已在本机创建草稿（" + ui.label + "）；发送链路未接入，不会离开本机。",
        lastErrorCode = null,
    )
}

/** 草稿状态只允许停在 queued/prepared（本地事实）。 */
val DraftUiState.blocksUiSend: Boolean get() = phase != DraftPhase.LOCAL_ONLY && phase != DraftPhase.QUEUED

fun PreviewStoreState.contactless(accountId: String?): PreviewStoreState = copy(
    notice = if (accountId == null) "本机属性化身份未就绪；本机存储面板只展示数据库内容。" else null,
    busy = false,
)
