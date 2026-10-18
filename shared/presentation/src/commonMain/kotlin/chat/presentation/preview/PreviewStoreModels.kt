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
import chat.presentation.display.StoreEventUi
import chat.presentation.display.toDraftUiState
import chat.presentation.display.toUi
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
    /** 最近事件（CORE-003，拉取式）；只是变化提示，不是完整数据。 */
    val events: List<StoreEventUi> = emptyList(),
    /** 下一次 poll 的游标。 */
    val nextEventSeq: Long = 0,
    /** 事件窗口溢出时界面必须整体重查。 */
    val resyncRequired: Boolean = false,
    /** 发送节点配置（只保存，不联网）。 */
    val sendNodeUrl: String = "",
    val sendNodeId: String = "",
    val sendNodePublicKey: String = "",
    /** 最近一次发送步骤结果（真实状态，不含正文）。 */
    val sendStep: chat.api.SendStep? = null,
    /** 常驻同步状态（SyncFfi 快照）。 */
    val syncStatus: chat.api.SyncStatusView? = null,
    /** 附件导出目标路径（附件面板与本面板共用；空表示未设置）。 */
    val exportTarget: String = "",
    /** 每条消息的附件卡片（来自 AttachmentFfi.records，不含正文）。 */
    val messageCards: Map<String, List<chat.presentation.display.AttachmentCard>> = emptyMap(),
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

/** 用 CORE-010 真实状态刷新草稿条目（按钮态以底层 canRetry/canCancel 为准）。 */
fun PreviewStoreState.withDraftStatus(status: chat.api.StoredDraftStatus): PreviewStoreState {
    val ui = status.toDraftUiState()
    val merged = LinkedHashMap<String, DraftUiState>()
    drafts.forEach { merged[it.draftId] = it }
    merged[status.draftId] = ui
    return copy(
        drafts = merged.values.toList(),
        busy = false,
        notice = "草稿状态：" + status.status +
            "（尝试 " + status.attempts + " 次，序号 " + (status.attemptIndex ?: 0) + "）",
        lastErrorCode = null,
    )
}

/** 最近事件（拉取式，最多保留 20 条）。 */
fun PreviewStoreState.withEvents(batch: chat.api.StoreEventBatch): PreviewStoreState {
    val merged = (events + batch.events.map { it.toUi() }).takeLast(20)
    return copy(
        events = merged,
        nextEventSeq = batch.nextSeq,
        resyncRequired = batch.resyncRequired,
        busy = false,
        notice = if (batch.resyncRequired) {
            "事件窗口已溢出或游标超前：请整体重新查询，事件不是可靠日志。"
        } else {
            notice
        },
    )
}

fun PreviewStoreState.withCancel(result: chat.api.DraftCancelOutcome): PreviewStoreState {
    val ui = result.toUi()
    return copy(
        busy = false,
        notice = ui.title + "：" + ui.detail,
        lastErrorCode = if (result.outcome == "cancel_too_late" || result.outcome == "cancel_unknown") null else null,
    )
}

fun PreviewStoreState.updateSendNodeUrl(value: String): PreviewStoreState = copy(sendNodeUrl = value)

fun PreviewStoreState.updateSendNodeId(value: String): PreviewStoreState = copy(sendNodeId = value)

fun PreviewStoreState.updateSendNodePublicKey(value: String): PreviewStoreState = copy(sendNodePublicKey = value)

/** 记录一次真实发送步骤结果；文案严格按 settled/status 区分"已接受/待对账/失败"。 */
fun PreviewStoreState.withSendStep(step: chat.api.SendStep): PreviewStoreState = copy(
    busy = false,
    sendStep = step,
    notice = when {
        step.status == "accepted" -> "节点已接受（提交 " + (step.submissionId ?: "?") + "）"
        step.status == "unknown" || !step.settled -> "结果未确定：只能对账，不要重发。"
        step.status == "rejected" -> "被节点拒绝，可按提示重试。"
        step.status == "prepared" || step.status == "submitted" -> "已加密待提交（尝试 " + (step.attemptIndex ?: 0) + "）"
        else -> step.detail
    },
    lastErrorCode = null,
)

fun PreviewStoreState.withSyncStatus(status: chat.api.SyncStatusView): PreviewStoreState = copy(
    syncStatus = status,
    busy = false,
    notice = if (status.state == "stopped" && status.stoppedReason != null) {
        "同步已停止（" + status.stoppedReason + "）"
    } else {
        "同步状态：" + status.state + "，已应用 " + status.appliedTotal + " 条"
    },
    lastErrorCode = status.lastErrorCode,
)

/** 写入某条消息的附件卡片（空列表表示"查过，没有附件"）。 */
fun PreviewStoreState.withMessageCards(
    messageId: String,
    cards: List<chat.presentation.display.AttachmentCard>,
): PreviewStoreState {
    val merged = LinkedHashMap(messageCards)
    merged[messageId] = cards
    return copy(
        messageCards = merged,
        busy = false,
        notice = if (cards.isEmpty()) notice else "已加载 " + cards.size + " 个附件卡片（点击可导出）",
    )
}
fun PreviewStoreState.updateExportTarget(value: String): PreviewStoreState = copy(exportTarget = value)
