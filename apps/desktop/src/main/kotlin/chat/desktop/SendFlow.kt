package chat.desktop

import chat.api.LocalStore
import chat.presentation.display.FROZEN_AUTO_RETRY_CODES

/** 一次「发送」的结果：要么成功（prepare→dispatch 都过），要么带失败步骤与原始异常。 */
data class SendOutcome(val steps: List<String>, val failure: Throwable?) {
    val ok: Boolean get() = failure == null
}

/**
 * 把「准备并加密 → 发送」这两步收成一次操作。
 *
 * 为什么要收敛：界面上分两个按钮时，用户容易只点「准备」就以为发出去了；
 * 而且两步各自处理错误，失败时说不清卡在哪一步。这里统一成：
 *   1. `sendPrepare` → 记录 "prepared"；
 *   2. `sendDispatch` → 记录 "accepted"；
 * 任一步抛错就停在该步，把步骤轨迹和原始异常一起返回（由上层用 `failureCode` 压成稳定码）。
 */
internal suspend fun sendNow(store: LocalStore, draftId: String): SendOutcome {
    val steps = mutableListOf<String>()
    return try {
        store.sendPrepare(draftId)
        steps += "prepared"
        store.sendDispatch(draftId)
        steps += "accepted"
        SendOutcome(steps.toList(), null)
    } catch (error: Throwable) {
        SendOutcome(steps.toList(), error)
    }
}

/**
 * 一步发送 + 自动恢复：
 *   1. prepare → dispatch；
 *   2. dispatch 失败且错误码在**冻结可重试表**里 → 重建一次尝试（retry）再 dispatch；
 *   3. 仍然失败且结果**未确定** → 按 SubmissionId 对账（reconcile），把对账结论带回去。
 *
 * 为什么只在可重试码上重试：协议层只保证这些码"可以安全重试"，其余码重试可能造成重复提交。
 */
internal suspend fun sendWithRecovery(store: LocalStore, draftId: String): SendOutcome {
    val steps = mutableListOf<String>()
    try {
        store.sendPrepare(draftId)
        steps += "prepared"
    } catch (error: Throwable) {
        return SendOutcome(steps.toList(), error)
    }
    try {
        val view = store.sendDispatch(draftId)
        // **不能只看“没抛异常”**：节点结果未确定时内核会返回 status=unknown / settled=false，
        // 那不是成功，而是“必须先对账”。这里必须如实报失败，否则界面会谎报已发送。
        if (view.status != "accepted" || !view.settled) {
            steps += "dispatch-" + view.status
            return SendOutcome(
                steps.toList(),
                chat.api.LocalStoreException(
                    if (view.settled) "SEND_REJECTED" else "UNKNOWN_RESULT",
                    if (view.settled) {
                        "节点未接受这次提交（status=" + view.status + "）：" + view.detail
                    } else {
                        "发送结果未确定（status=" + view.status + "）：必须按 SubmissionId 对账，不要重发"
                    },
                ),
            )
        }
        steps += "accepted"
        return SendOutcome(steps.toList(), null)
    } catch (first: Throwable) {
        val code = failureCode(first)
        if (code !in FROZEN_AUTO_RETRY_CODES) {
            steps += "dispatch-failed(" + code + ")"
            return SendOutcome(steps.toList(), first)
        }
        steps += "retry(" + code + ")"
        return try {
            store.sendRetry(draftId)
            store.sendDispatch(draftId)
            steps += "accepted-after-retry"
            SendOutcome(steps.toList(), null)
        } catch (second: Throwable) {
            val secondCode = failureCode(second)
            steps += "retry-failed(" + secondCode + ")"
            runCatching { store.sendReconcile(draftId) }
                .onSuccess { reconciled ->
                    steps += if (reconciled.settled) "reconciled(" + reconciled.status + ")" else "reconcile-pending"
                }
                .onFailure { steps += "reconcile-failed" }
            SendOutcome(steps.toList(), second)
        }
    }
}
