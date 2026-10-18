package chat.presentation.display

import chat.api.StoredConversation
import chat.api.StoredDraft
import chat.api.StoredMessage
import chat.api.StoredPage

/**
 * core/存储事实 → 展示层的纯映射（P2-01 展示部分）。
 *
 * 原则：
 * - 存储层状态只说明“本端意图/尝试”，**不代表**节点接受或对端送达；
 * - 没有 unread 计数时保持 null，不伪造 0；
 * - 分页游标原样透传，展示层不重算、不猜测；
 * - 未知状态按“仅本地草稿”安全回退，不升级成已接受。
 */

/** 存储层的草稿状态码（与 formal store 的 p0_intents.state 对齐）。 */
object StoredDraftState {
    const val QUEUED = "queued"
    const val PREPARED = "prepared"
}

fun StoredDraft.toDraftEvidence(): DraftEvidence = when (state) {
    StoredDraftState.QUEUED -> DraftEvidence(
        draftId = draftId,
        queuedLocally = true,
    )

    StoredDraftState.PREPARED -> DraftEvidence(
        draftId = draftId,
        queuedLocally = true,
        submissionId = submissionId,
    )

    // 未知状态：只承认“草稿存在”，不推断排队、接受或送达。
    else -> DraftEvidence(draftId = draftId)
}

fun StoredConversation.toSummaryView(): ConversationSummaryView = ConversationSummaryView(
    id = conversationId,
    title = title,
    preview = preview,
    unreadCount = unreadCount,
)

fun StoredMessage.toDisplayView(): MessageDisplayView = MessageDisplayView(
    id = messageId,
    conversationId = conversationId,
    author = author,
    body = body,
    mine = mine,
)

fun StoredPage<StoredConversation>.toConversationDisplayPage(): DisplayPage<ConversationSummaryView> =
    DisplayPage(
        items = items.map { it.toSummaryView() },
        nextCursor = nextCursor,
    )

fun StoredPage<StoredMessage>.toMessageDisplayPage(): DisplayPage<MessageDisplayView> =
    DisplayPage(
        items = items.map { it.toDisplayView() },
        nextCursor = nextCursor,
    )
