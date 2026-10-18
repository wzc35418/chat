package chat.api

/**
 * 身份与联系人服务的 Kotlin 视图（P1-02 接线）。
 *
 * 全部来自 crates/identity 的真实密码学结果：账户/设备 ID、指纹、邀请口令与联系人状态。
 * 这里不提供“先展示后验证”的入口：口令必须先验签通过才返回摘要。
 */

data class IdentityAccount(
    val accountId: String,
    val deviceId: String,
    val displayName: String,
    /** 16 组 4 位大写十六进制，供人工核对；昵称不能用于验证身份。 */
    val fingerprint: String,
)

data class IdentityInvite(
    val token: String,
    val inviteId: String,
    val expiresAtMillis: Long,
    val accountId: String,
    val deviceId: String,
    val fingerprint: String,
)

/** 已验签通过的口令摘要；只有校验成功才会产生。 */
data class IdentityInviteSummary(
    val inviteId: String,
    val accountId: String,
    val deviceId: String,
    val fingerprint: String,
    val node: String,
    val expiresAtMillis: Long,
)

data class IdentityContact(
    val contactId: String,
    val accountId: String,
    val displayName: String,
    val fingerprint: String,
    /** requested / accepted / declined / blocked / identity_changed */
    val state: String,
    val canSend: Boolean,
)

/** code 是稳定错误码（PROTOCOL-014）；message 只作展示。 */
class IdentityException(
    val code: String,
    override val message: String,
) : Exception(message)

interface IdentityService {
    suspend fun create(displayName: String, node: String): IdentityAccount

    suspend fun account(): IdentityAccount

    suspend fun createInvite(ttlMillis: Long): IdentityInvite

    /** 只验签 + 过期 + 绑定检查，不建立联系人；失败抛 [IdentityException]。 */
    suspend fun previewInvite(token: String): IdentityInviteSummary

    suspend fun acceptInvite(token: String, displayName: String): IdentityContact

    suspend fun contacts(): List<IdentityContact>

    suspend fun confirmIdentityChange(contactId: String): IdentityContact

    suspend fun close()
}
