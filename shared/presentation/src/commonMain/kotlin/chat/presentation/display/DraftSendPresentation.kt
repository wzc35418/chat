package chat.presentation.display

import chat.api.CoreFailure

/**
 * 发送意图（草稿）的纯展示状态，对应 PRESENTATION-005 / PRESENTATION-006 的展示部分。
 *
 * 只根据底层回报的证据决定文案与可用动作：
 * - 本地排队、节点签名回执（只代表节点接受）、独立送达证据是三件不同的事；
 * - 节点接受不能写成“已送达”，界面也不按本地计时器推断接受或送达；
 * - 结果未知时只提供对账查询，不自动重试、不重新加密、不乐观删除消息；
 * - 取消不保证撤回已经在途的消息，最终状态以底层回报为准。
 */

/** 一条发送意图当前可用的确定性证据。没有证据的字段保持 null/false，不用猜测补齐。 */
data class DraftEvidence(
    val draftId: String,
    val queuedLocally: Boolean = false,
    /** 节点签名回执的稳定标识：只说明节点已接受，不代表对端已送达。 */
    val nodeAcceptedReceiptId: String? = null,
    /** 独立送达证据的稳定标识。 */
    val deliveredReceiptId: String? = null,
    /** 本端已生成的提交 ID（submission id）：**不代表**节点接受或对端送达，只用于对账与诊断。 */
    val submissionId: String? = null,
    /** 上一次发送没有拿到确定结果，需要后续对账。 */
    val acceptanceUnknown: Boolean = false,
    val cancelRequested: Boolean = false,
    /** 底层回报的可恢复错误；展示时复用 [presentError] 的稳定文案。 */
    val failure: CoreFailure? = null,
)

enum class DraftPhase {
    LOCAL_ONLY,
    QUEUED,
    ACCEPTED,
    DELIVERED,
    ACCEPTANCE_UNKNOWN,
    CANCEL_PENDING,
    CANCEL_TOO_LATE,
    FAILED_RETRYABLE,
    FAILED_BLOCKED,
}

enum class DraftAction { RETRY, CANCEL, RECONCILE, VIEW_DETAILS }

data class DraftUiState(
    val draftId: String,
    val phase: DraftPhase,
    val label: String,
    val detail: String,
    val actions: List<DraftAction>,
    val diagnosticId: String? = null,
    /** 始终为 false：展示层从不因失败或未知而自动重发，重试只能来自用户动作。 */
    val autoResendAllowed: Boolean = false,
    val retryInFlight: Boolean = false,
    val cancelInFlight: Boolean = false,
) {
    val canRetry: Boolean get() = DraftAction.RETRY in actions

    val canCancel: Boolean get() = DraftAction.CANCEL in actions
}

fun presentDraft(
    evidence: DraftEvidence,
    retryInFlight: Boolean = false,
    cancelInFlight: Boolean = false,
): DraftUiState {
    val base = when {
        evidence.deliveredReceiptId != null -> DraftUiState(
            draftId = evidence.draftId,
            phase = DraftPhase.DELIVERED,
            label = "已送达",
            detail = if (evidence.cancelRequested) {
                "对端设备已回报送达；取消不再生效，也不会重新发送。"
            } else {
                "对端设备已回报送达。"
            },
            actions = emptyList(),
        )

        evidence.nodeAcceptedReceiptId != null -> if (evidence.cancelRequested) {
            DraftUiState(
                draftId = evidence.draftId,
                phase = DraftPhase.CANCEL_TOO_LATE,
                label = "取消未生效：已被接受",
                detail = "节点已签名接受这条消息，取消不能撤回；消息仍按已接受展示。",
                actions = listOf(DraftAction.VIEW_DETAILS),
            )
        } else {
            DraftUiState(
                draftId = evidence.draftId,
                phase = DraftPhase.ACCEPTED,
                label = "已接受",
                detail = "节点已签名接受这条消息；这不等于对端已送达。",
                actions = listOf(DraftAction.VIEW_DETAILS),
            )
        }

        evidence.acceptanceUnknown -> DraftUiState(
            draftId = evidence.draftId,
            phase = DraftPhase.ACCEPTANCE_UNKNOWN,
            label = "发送结果待对账",
            detail = "没有拿到确定结果：不会自动重发，也不会重新加密；请等待对账查询更新状态。",
            actions = listOf(DraftAction.RECONCILE),
        )

        evidence.failure != null -> {
            val error = presentError(evidence.failure)
            val cancel = if (evidence.queuedLocally) listOf(DraftAction.CANCEL) else emptyList()
            if (error.retryable) {
                DraftUiState(
                    draftId = evidence.draftId,
                    phase = DraftPhase.FAILED_RETRYABLE,
                    label = error.title,
                    detail = error.detail,
                    actions = listOf(DraftAction.RETRY) + cancel,
                    diagnosticId = error.diagnosticId,
                )
            } else {
                DraftUiState(
                    draftId = evidence.draftId,
                    phase = DraftPhase.FAILED_BLOCKED,
                    label = error.title,
                    detail = error.detail,
                    actions = listOf(DraftAction.VIEW_DETAILS) + cancel,
                    diagnosticId = error.diagnosticId,
                )
            }
        }

        evidence.cancelRequested -> DraftUiState(
            draftId = evidence.draftId,
            phase = DraftPhase.CANCEL_PENDING,
            label = "正在取消",
            detail = "取消不保证撤回已在途的消息，最终状态以节点结果为准。",
            actions = emptyList(),
        )

        evidence.queuedLocally -> DraftUiState(
            draftId = evidence.draftId,
            phase = DraftPhase.QUEUED,
            label = "待发送",
            detail = "已保存在本端，联网后提交；提交后仍需节点回执才算已接受。",
            actions = listOf(DraftAction.CANCEL),
        )

        else -> DraftUiState(
            draftId = evidence.draftId,
            phase = DraftPhase.LOCAL_ONLY,
            label = "本地草稿",
            detail = "还没有提交到发送队列。",
            actions = emptyList(),
        )
    }

    if (retryInFlight && base.phase == DraftPhase.FAILED_RETRYABLE) {
        return base.copy(label = "正在重试", actions = emptyList(), retryInFlight = true)
    }
    if (cancelInFlight && (base.canCancel || base.phase == DraftPhase.QUEUED)) {
        return base.copy(label = "正在取消", actions = emptyList(), cancelInFlight = true)
    }
    return base
}

/**
 * 结果未知期间不允许出现可发送状态：新消息会与在途意图混淆，
 * 也会让“未知是否已接受”变得无法对账。
 */
fun DraftUiState.blocksNewSend(): Boolean = phase == DraftPhase.ACCEPTANCE_UNKNOWN

/**
 * 交互状态：只在用户动作后进入“重试中/取消中”，重复点击合并为一次。
 * 该类型不持久化、不发起网络调用，调用方负责把结果转回 [DraftEvidence]。
 */
data class DraftInteraction(
    val evidence: DraftEvidence,
    val retryInFlight: Boolean = false,
    val cancelInFlight: Boolean = false,
) {
    val ui: DraftUiState get() = presentDraft(evidence, retryInFlight, cancelInFlight)

    fun beginRetry(): DraftInteraction {
        if (retryInFlight || cancelInFlight) return this
        if (!ui.canRetry) return this
        return copy(retryInFlight = true)
    }

    fun finishRetry(next: DraftEvidence): DraftInteraction =
        copy(evidence = next, retryInFlight = false)

    fun beginCancel(): DraftInteraction {
        if (retryInFlight || cancelInFlight) return this
        if (!ui.canCancel) return this
        return copy(cancelInFlight = true)
    }

    fun finishCancel(next: DraftEvidence): DraftInteraction =
        copy(evidence = next, cancelInFlight = false)

    /** 对账结果同样只更新证据，不在展示层推断接受或送达。 */
    fun applyReconciled(next: DraftEvidence): DraftInteraction =
        copy(evidence = next, retryInFlight = false, cancelInFlight = false)
}
