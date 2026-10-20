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

    /**
     * 持久身份：打开（或首次创建）受主密钥保护的本地身份库。
     *
     * - 已有记录 → 用恢复出的账户/设备密钥（重启后指纹不变）；
     * - 没有记录 → 生成新账户并写入；
     * - 错误主密钥 → 抛出 [IdentityException]（不会静默新建身份）；
     * - 联系人随 accept/confirm 落库，重开后仍在。
     */
    suspend fun openPersisted(databasePath: String, masterKeyHex: String, node: String): IdentityAccount

    suspend fun account(): IdentityAccount

    suspend fun createInvite(ttlMillis: Long): IdentityInvite

    /** 只验签 + 过期 + 绑定检查，不建立联系人；失败抛 [IdentityException]。 */
    suspend fun previewInvite(token: String): IdentityInviteSummary

    suspend fun acceptInvite(token: String, displayName: String): IdentityContact

    suspend fun contacts(): List<IdentityContact>

    suspend fun confirmIdentityChange(contactId: String): IdentityContact

    /**
     * IDENTITY-013：导出加密恢复文件（口令派生 KDF + AEAD，**不含主密钥**）。
     *
     * 只恢复身份（账户/设备种子），旧库消息与附件仍不可读；未打开持久身份时显式失败。
     */
    suspend fun exportRecoveryBundle(passphrase: String): ByteArray

    /**
     * IDENTITY-014：在**空库**导入恢复文件。
     *
     * 目标库已有身份时显式拒绝（不覆盖）；导入后需重新打开才能使用身份。
     */
    suspend fun importRecoveryBundle(passphrase: String, bundle: ByteArray): RecoveryImportView

    /**
     * 导出本机 KeyPackage 字节（只含公钥材料）。**单次使用**：每次导出都是新的。
     *
     * 用于把邀请发给对方：对方拿它建群后会得到可用的 Welcome。
     */
    suspend fun exportKeyPackage(): ByteArray

    /**
     * 把本机身份登记进另一个库（通常是消息库）。
     *
     * 语义：幂等；目标库已绑定另一账户 → `IDENTITY_ACCOUNT_CONFLICT`（绝不覆盖）；
     * 目标主密钥错 → 存储层错误码原样透出。**不返回任何种子材料**。
     */
    suspend fun enrollIntoStore(targetDatabasePath: String, targetMasterKeyHex: String): IdentityAccount

    /** IDENTITY-011：本账户已登记设备（includeRevoked 为真时含已撤销）。 */
    suspend fun listDevices(includeRevoked: Boolean = false): List<IdentityDevice>

    /** 登记第二台设备（幂等；撤销不可逆，已撤销设备重新登记不复活）。 */
    suspend fun registerDevice(deviceId: String, devicePublicKeyHex: String, label: String): IdentityDevice

    /** IDENTITY-012：撤销某台设备（不能撤销本机；撤销不可逆）。 */
    suspend fun revokeDevice(deviceId: String, reason: String): Boolean

    suspend fun close()
}