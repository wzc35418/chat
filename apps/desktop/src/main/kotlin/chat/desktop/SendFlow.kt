package chat.desktop

import chat.api.LocalStore

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
