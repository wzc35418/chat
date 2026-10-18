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

/** 存储层状态码（CORE-010 有效状态集合）。 */
object StoredDraftStatusKind {
    const val QUEUED = "queued"
    const val PREPARED = "prepared"
    const val SUBMITTED = "submitted"
    const val ACCEPTED = "accepted"
    const val REJECTED = "rejected"
    const val UNKNOWN = "unknown"
    const val CANCELLED = "cancelled"
    const val FAILED = "failed"
}

/**
 * CORE-010 状态 → 展示状态。
 *
 * 映射原则：只有 accepted 显示“已接受”；unknown 只给对账；rejected 且可重试才给重试；
 * cancelled/failed 不提供动作；按钮可用性最终以底层 canRetry/canCancel 为准（双保险）。
 */
fun chat.api.StoredDraftStatus.toDraftUiState(): DraftUiState {
    val evidence = when (status) {
        StoredDraftStatusKind.ACCEPTED -> DraftEvidence(
            draftId = draftId,
            queuedLocally = true,
            nodeAcceptedReceiptId = submissionId,
        )

        StoredDraftStatusKind.UNKNOWN -> DraftEvidence(
            draftId = draftId,
            queuedLocally = true,
            submissionId = submissionId,
            acceptanceUnknown = true,
        )

        StoredDraftStatusKind.REJECTED, StoredDraftStatusKind.FAILED -> DraftEvidence(
            draftId = draftId,
            queuedLocally = true,
            submissionId = submissionId,
            failure = chat.api.CoreFailure(
                code = "INVALID_STATE_TRANSITION",
                retryable = canRetry,
                retryAfterMillis = null,
                operationId = operationId,
            ),
        )

        StoredDraftStatusKind.CANCELLED -> DraftEvidence(
            draftId = draftId,
            queuedLocally = true,
            submissionId = submissionId,
            cancelRequested = true,
        )

        StoredDraftStatusKind.PREPARED, StoredDraftStatusKind.SUBMITTED -> DraftEvidence(
            draftId = draftId,
            queuedLocally = true,
            submissionId = submissionId,
        )

        else -> DraftEvidence(draftId = draftId, queuedLocally = true)
    }
    val base = presentDraft(evidence)
    // 底层动作可用性是最终裁决：can_retry=false 时不提供重试，can_cancel=false 时不提供取消。
    val actions = base.actions.filter { action ->
        when (action) {
            DraftAction.RETRY -> canRetry
            DraftAction.CANCEL -> canCancel
            DraftAction.RECONCILE -> status == StoredDraftStatusKind.UNKNOWN
            DraftAction.VIEW_DETAILS -> true
        }
    }
    return base.copy(actions = actions)
}

/** 事件 → 可读文案（CORE-003）。不含正文，只解释发生了什么。 */
data class StoreEventUi(
    val kind: String,
    val title: String,
    val detail: String,
    val requiresResync: Boolean,
)

fun chat.api.StoreEvent.toUi(): StoreEventUi = when (kind) {
    "draft_changed" -> StoreEventUi(
        kind = kind,
        title = "草稿状态变化",
        detail = (draftId ?: "草稿") + " → " + (status ?: "未知"),
        requiresResync = false,
    )
    "conversation_changed" -> StoreEventUi(
        kind = kind,
        title = "会话变化",
        detail = conversationId ?: "会话",
        requiresResync = false,
    )
    "resync_required" -> StoreEventUi(
        kind = kind,
        title = "需要重新拉取",
        detail = "事件窗口已溢出或游标超前，界面应整体重查，不能把事件当作完整数据。",
        requiresResync = true,
    )
    "core_closed" -> StoreEventUi(
        kind = kind,
        title = "内核已关闭",
        detail = "本地内核已关闭，之后的操作会失败。",
        requiresResync = false,
    )
    else -> StoreEventUi(
        kind = kind,
        title = "未知事件",
        detail = "收到暂未识别的事件类型，已安全忽略。",
        requiresResync = false,
    )
}

/** 取消结果 → 展示结论（CORE-009 三态）。 */
data class CancelUi(val title: String, val detail: String, val keepMessage: Boolean)

fun chat.api.DraftCancelOutcome.toUi(): CancelUi = when (outcome) {
    "cancel_requested" -> CancelUi(
        title = "已请求取消",
        detail = "取消已记录；不保证撤回已经在途的消息。",
        keepMessage = true,
    )
    "cancel_too_late" -> CancelUi(
        title = "取消未生效：已被接受",
        detail = "消息已被节点接受，取消不能撤回；仍按已接受展示。",
        keepMessage = true,
    )
    "cancel_unknown" -> CancelUi(
        title = "结果未知，已标记待对账",
        detail = "没有拿到确定结果；不会自动重发，等待对账查询更新状态。",
        keepMessage = true,
    )
    else -> CancelUi(
        title = "取消失败",
        detail = "取消没有成功，界面不会把失败当成功。",
        keepMessage = true,
    )
}

/** 附件状态 → 界面徽标（ATTACHMENTS-002）。 */
data class AttachmentBadge(
    val state: String,
    val label: String,
    /** 只有 available 才能声称“对方可下载”。 */
    val downloadable: Boolean,
    val cancellable: Boolean,
    val detail: String,
)

fun chat.api.LocalAttachment.toBadge(): AttachmentBadge = when (state) {
    "imported" -> AttachmentBadge(
        state = state,
        label = "已加密导入",
        downloadable = false,
        cancellable = true,
        detail = "只在本机；尚未上传，对方不可下载。",
    )
    "uploading" -> AttachmentBadge(
        state = state,
        label = "上传中",
        downloadable = false,
        cancellable = true,
        detail = "正在按序提交密文块；失败可重试，已传块不会重传。",
    )
    "available" -> AttachmentBadge(
        state = state,
        label = "已上传",
        downloadable = true,
        cancellable = false,
        detail = "节点已保存全部密文块并校验整体摘要。",
    )
    "downloading" -> AttachmentBadge(
        state = state,
        label = "下载中",
        downloadable = false,
        cancellable = true,
        detail = "正在下载并逐块认证解密；失败会清理临时文件。",
    )
    "cancelled" -> AttachmentBadge(
        state = state,
        label = "已取消",
        downloadable = false,
        cancellable = false,
        detail = "已请求取消；本地缓存与进度保留。",
    )
    "failed" -> AttachmentBadge(
        state = state,
        label = "失败",
        downloadable = false,
        cancellable = false,
        detail = "失败记录不保留缓存，可以重新导入。",
    )
    else -> AttachmentBadge(
        state = state,
        label = "状态未知",
        downloadable = false,
        cancellable = false,
        detail = "收到无法识别的附件状态，已按不可下载处理。",
    )
}

/** 传输进度（0..1）；块数未知时返回 null（界面显示不确定进度，不伪造百分比）。 */
fun chat.api.LocalAttachment.transferProgress(): Float? {
    if (chunkCount <= 0) return null
    if (state != "uploading" && state != "downloading") return null
    val done = transferredChunks.coerceIn(0, chunkCount)
    return done.toFloat() / chunkCount.toFloat()
}

/** 进度文案：明确是"已传块数/总块数"，不是字节数。 */
fun chat.api.LocalAttachment.progressLabel(): String =
    if (state == "uploading" || state == "downloading") {
        transferredChunks.coerceIn(0, chunkCount).toString() + "/" + chunkCount + " 块"
    } else {
        ""
    }
