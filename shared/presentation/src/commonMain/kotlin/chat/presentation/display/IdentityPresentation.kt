package chat.presentation.display

import chat.api.IdentityContact
import chat.api.IdentityInviteSummary

/**
 * 身份/口令/联系人的纯展示映射（P1-02 展示部分）。
 *
 * 原则：
 * - 口令只有验签成功才是 VALID；任何错误码都给出可理解的稳定文案与下一步动作；
 * - 指纹是唯一的身份依据，昵称只作展示；
 * - identity_changed 一律阻断发送（即使底层误报 can_send=true，展示层也强制 false）；
 * - 未知状态按“不可发送”安全回退，不猜测成已接受。
 */
enum class InviteCheck {
    VALID,
    EMPTY,
    TAMPERED,
    EXPIRED,
    WRONG_TARGET,
    UNKNOWN,
}

data class InvitePreviewUi(
    val check: InviteCheck,
    val title: String,
    val detail: String,
    val accountId: String? = null,
    val deviceId: String? = null,
    val fingerprint: String? = null,
    /** 距离过期的整分钟数；仅 VALID 时有值，null 表示不适用。 */
    val remainingMinutes: Long? = null,
) {
    val valid: Boolean get() = check == InviteCheck.VALID
}

fun presentInvitePreview(summary: IdentityInviteSummary, nowMillis: Long): InvitePreviewUi {
    val remaining = summary.expiresAtMillis - nowMillis
    if (remaining <= 0) {
        return InvitePreviewUi(
            check = InviteCheck.EXPIRED,
            title = "口令已失效",
            detail = "这条口令已经过期，请让对方重新生成一条一次性口令。",
        )
    }
    return InvitePreviewUi(
        check = InviteCheck.VALID,
        title = "口令已通过签名校验",
        detail = "核对指纹后确认加好友；昵称只作展示，不能用于验证身份。",
        accountId = summary.accountId,
        deviceId = summary.deviceId,
        fingerprint = summary.fingerprint,
        remainingMinutes = remaining / 60_000,
    )
}

fun presentInviteError(code: String, rawMessage: String? = null): InvitePreviewUi {
    val trimmed = rawMessage?.trim().orEmpty()
    val detail = if (trimmed.isEmpty()) "" else "（诊断：" + trimmed.take(120) + "）"
    return when (code) {
        "INVALID_SIGNATURE", "INVALID_CREDENTIAL" -> InvitePreviewUi(
            check = InviteCheck.TAMPERED,
            title = "口令校验失败",
            detail = "口令内容与签名不一致，可能被修改过。请让对方重新生成，不要继续核对。" + detail,
        )

        "CURSOR_EXPIRED" -> InvitePreviewUi(
            check = InviteCheck.EXPIRED,
            title = "口令已失效",
            detail = "这条口令已经过期，请让对方重新生成一条一次性口令。" + detail,
        )

        "INVALID_TARGET" -> InvitePreviewUi(
            check = InviteCheck.WRONG_TARGET,
            title = "口令不属于这台设备",
            detail = "口令绑定的账户或设备与当前身份不一致，请确认没有复制错。" + detail,
        )

        "INVALID_ID" -> InvitePreviewUi(
            check = InviteCheck.UNKNOWN,
            title = "口令格式无法识别",
            detail = "请完整复制对方提供的口令；格式不正确的输入不会被当作身份。" + detail,
        )

        else -> InvitePreviewUi(
            check = InviteCheck.UNKNOWN,
            title = "无法校验这条口令",
            detail = "遇到暂未识别的校验错误，请稍后重试或让对方重新生成口令。" + detail,
        )
    }
}

enum class ContactStateUi {
    REQUESTED,
    ACCEPTED,
    DECLINED,
    BLOCKED,
    IDENTITY_CHANGED,
    UNKNOWN,
}

data class ContactUi(
    val contactId: String,
    val displayName: String,
    val fingerprint: String,
    val state: ContactStateUi,
    /** 展示层最终结论：identity_changed/未知状态一律 false。 */
    val canSend: Boolean,
    val badge: String,
    val blockingReason: String? = null,
)

fun presentContact(contact: IdentityContact): ContactUi {
    val state = when (contact.state) {
        "requested" -> ContactStateUi.REQUESTED
        "accepted" -> ContactStateUi.ACCEPTED
        "declined" -> ContactStateUi.DECLINED
        "blocked" -> ContactStateUi.BLOCKED
        "identity_changed" -> ContactStateUi.IDENTITY_CHANGED
        else -> ContactStateUi.UNKNOWN
    }
    val (badge, reason) = when (state) {
        ContactStateUi.REQUESTED -> "等待确认" to "双方都确认后才能发送消息。"
        ContactStateUi.ACCEPTED -> "已接受" to null
        ContactStateUi.DECLINED -> "已拒绝" to "对方拒绝了这次请求，不能发送消息。"
        ContactStateUi.BLOCKED -> "已拉黑" to "该联系人已被拉黑，不能发送消息。"
        ContactStateUi.IDENTITY_CHANGED ->
            "身份变化" to "对方指纹与已固定记录不一致，已阻断发送；请通过独立渠道核对后再恢复。"

        ContactStateUi.UNKNOWN -> "状态未知" to "联系人状态无法识别，已按不可发送处理，请刷新后重试。"
    }
    // 双保险：只有 accepted 且底层确认可发送时才允许发送。
    val canSend = state == ContactStateUi.ACCEPTED && contact.canSend
    return ContactUi(
        contactId = contact.contactId,
        displayName = contact.displayName,
        fingerprint = contact.fingerprint,
        state = state,
        canSend = canSend,
        badge = badge,
        blockingReason = reason,
    )
}
