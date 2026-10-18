package chat.presentation.preview

import chat.api.CoreFailure
import chat.presentation.display.DraftEvidence
import chat.presentation.display.DraftInteraction
import chat.presentation.display.DraftUiState
import chat.presentation.display.presentDraft

/**
 * 预览用发送状态样例（DS-10）。
 *
 * 按固定顺序循环展示“待发送 → 已接受（节点回执）→ 结果待对账 → 可重试失败 →
 * 权限阻断 → 已送达”。每一步都只是显式样例证据，不调用任何发送接口，
 * 也不写入正式会话状态。
 */
data class PreviewDraftSample(
    val interaction: DraftInteraction,
    val step: Int = 0,
) {
    val ui: DraftUiState get() = presentDraft(interaction.evidence)
}

private const val PREVIEW_DRAFT_ID = "sample-draft-1"

private val sampleDraftEvidence = listOf(
    DraftEvidence(
        draftId = PREVIEW_DRAFT_ID,
        queuedLocally = true,
    ),
    DraftEvidence(
        draftId = PREVIEW_DRAFT_ID,
        queuedLocally = true,
        nodeAcceptedReceiptId = "receipt-sample-node-accepted",
    ),
    DraftEvidence(
        draftId = PREVIEW_DRAFT_ID,
        queuedLocally = true,
        acceptanceUnknown = true,
    ),
    DraftEvidence(
        draftId = PREVIEW_DRAFT_ID,
        queuedLocally = true,
        failure = CoreFailure(
            code = "NETWORK_UNAVAILABLE",
            retryable = true,
            retryAfterMillis = 2000,
            operationId = "op-sample-network",
        ),
    ),
    DraftEvidence(
        draftId = PREVIEW_DRAFT_ID,
        queuedLocally = true,
        failure = CoreFailure(
            code = "PERMISSION_DENIED",
            retryable = false,
            retryAfterMillis = null,
            operationId = "op-sample-permission",
        ),
    ),
    DraftEvidence(
        draftId = PREVIEW_DRAFT_ID,
        nodeAcceptedReceiptId = "receipt-sample-node-accepted",
        deliveredReceiptId = "receipt-sample-delivered",
    ),
)

fun samplePreviewDraft(): PreviewDraftSample = PreviewDraftSample(
    interaction = DraftInteraction(sampleDraftEvidence.first()),
)

/** 推到下一个样例状态；到达“已送达”后回到“待发送”，便于反复观察。 */
fun PreviewDraftSample.next(): PreviewDraftSample {
    val nextStep = (step + 1) % sampleDraftEvidence.size
    return PreviewDraftSample(
        interaction = DraftInteraction(sampleDraftEvidence[nextStep]),
        step = nextStep,
    )
}
