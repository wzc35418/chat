package chat.api

/** 已登记设备（只读投影；不含任何私钥材料）。 */
data class IdentityDevice(
    val deviceId: String,
    val accountId: String,
    val label: String,
    val devicePublicKeyHex: String,
    val addedAtMillis: Long,
    val revokedAtMillis: Long?,
    val revokedReason: String?,
    val isLocal: Boolean,
)
