package chat.api

/** 恢复导入结果（只读投影，来自 FFI；不含任何种子材料）。 */
data class RecoveryImportView(
    val accountId: String,
    val deviceId: String,
    val accountPublicKey: String,
    val devicePublicKey: String,
    val displayName: String,
    val createdAtMillis: Long,
    val devicesDigest: String,
)
