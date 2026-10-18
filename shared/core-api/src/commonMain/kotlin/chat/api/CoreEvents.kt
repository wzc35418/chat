package chat.api

/** 取消草稿的三态结果（CORE-009）。 */
data class DraftCancelOutcome(
    val draftId: String,
    /** cancel_requested / cancel_too_late / cancel_unknown */
    val outcome: String,
    val submissionId: String?,
)

/** 核心事件（CORE-003）：只含 ID 与稳定状态文本，不含正文/密钥。 */
data class StoreEvent(
    /** draft_changed / conversation_changed / resync_required / core_closed */
    val kind: String,
    val draftId: String?,
    val conversationId: String?,
    val status: String?,
    val submissionId: String?,
    /** 事件在总线里的单调序号（1 起）。 */
    val sequence: Long,
)

data class StoreEventBatch(
    val events: List<StoreEvent>,
    val nextSeq: Long,
    val resyncRequired: Boolean,
)
