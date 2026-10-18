package chat.presentation.preview

import chat.api.IdentityAccount
import chat.api.IdentityContact
import chat.api.IdentityInvite
import chat.presentation.display.ContactUi
import chat.presentation.display.InvitePreviewUi
import chat.presentation.display.presentContact

/**
 * 预览里的“真实身份（本机）”状态。
 *
 * 这里的数据全部来自 crates/identity 的真实密码学结果（账户 ID、指纹、inv1 口令、联系人状态），
 * 只是传输还没有接：口令要人工复制/粘贴，没有节点投递。界面必须标明这一点。
 */
data class PreviewIdentityState(
    val account: IdentityAccount? = null,
    val invite: IdentityInvite? = null,
    val pastedToken: String = "",
    val preview: InvitePreviewUi? = null,
    val contacts: List<ContactUi> = emptyList(),
    val notice: String? = null,
    val lastErrorCode: String? = null,
    val busy: Boolean = false,
    val ready: Boolean = false,
)

/** 打开真实身份：显示脱敏账户信息。 */
fun PreviewIdentityState.openWith(account: IdentityAccount): PreviewIdentityState = copy(
    account = account,
    ready = true,
    busy = false,
    notice = "已在本机创建真实身份；口令需要人工复制，尚未接节点投递。",
    lastErrorCode = null,
)

fun PreviewIdentityState.withInvite(invite: IdentityInvite): PreviewIdentityState = copy(
    invite = invite,
    preview = null,
    busy = false,
    notice = "已生成一次性口令，24 小时内有效；请通过独立渠道发给对方。",
    lastErrorCode = null,
)

fun PreviewIdentityState.withPreview(preview: InvitePreviewUi): PreviewIdentityState = copy(
    preview = preview,
    busy = false,
    notice = null,
    lastErrorCode = null,
)

fun PreviewIdentityState.withError(code: String, preview: InvitePreviewUi? = null): PreviewIdentityState = copy(
    preview = preview,
    busy = false,
    lastErrorCode = code,
    notice = "校验没有通过，界面不会把这次输入当作已验证身份。",
)

fun PreviewIdentityState.withContacts(contacts: List<IdentityContact>): PreviewIdentityState = copy(
    contacts = contacts.map { presentContact(it) },
    busy = false,
)

fun PreviewIdentityState.updatePastedToken(value: String): PreviewIdentityState = copy(
    pastedToken = value,
    preview = null,
    notice = null,
    lastErrorCode = null,
)

fun PreviewIdentityState.beginBusy(): PreviewIdentityState = copy(busy = true, notice = null)

/** 真实身份服务不可用时的显式状态，不使用任何样例身份冒充。 */
fun PreviewIdentityState.unavailable(reason: String): PreviewIdentityState = copy(
    ready = false,
    busy = false,
    account = null,
    contacts = emptyList(),
    preview = null,
    notice = "本机身份服务不可用：" + reason,
)
