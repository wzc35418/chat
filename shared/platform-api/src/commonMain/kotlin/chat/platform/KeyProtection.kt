package chat.platform

/** 受控秘密缓冲：只能被平台封装层读取，不进入普通 DTO/日志。 */
class SecretBuffer(bytes: ByteArray) {
    private val inner = bytes.copyOf()

    /** 仅平台封装边界可调用；调用方负责尽快用 [close]。 */
    fun copyBytes(): ByteArray = inner.copyOf()

    fun size(): Int = inner.size

    /** 尽力清零（JVM 不保证对象被复制前的内容一定被擦除，但不主动保留明文）。 */
    fun close() {
        inner.fill(0)
    }
}

/** 系统封装后的主密钥包：只含密文与平台元数据，绝不含明文。 */
data class WrappedSecret(
    val version: Int,
    val keyId: String,
    val platform: String,
    val nonceBase64: String,
    val ciphertextBase64: String,
)

/** 平台密钥保护稳定错误码。 */
object KeyProtectionCodes {
    const val UNAVAILABLE = "KEY_PROTECTION_UNAVAILABLE"
    const val USER_AUTH_REQUIRED = "USER_AUTH_REQUIRED"
    const val USER_AUTH_CANCELLED = "USER_AUTH_CANCELLED"
    const val KEY_INVALIDATED = "KEY_INVALIDATED"
    const val STORAGE_LOCKED = "STORAGE_LOCKED"
    const val INVALID_INPUT = "INVALID_INPUT"
}

class KeyProtectionException(val code: String, message: String) : Exception(message)

/**
 * PLATFORM-001/002/003：主密钥的系统级封装。
 *
 * 契约：wrap 返回的包不含明文；unwrap 失败必须显式失败（可选错误码），
 * 绝不在缺少授权时降级为明文密钥；delete 后旧包不可解。
 */
interface KeyProtection {
    /** 返回平台名称（desktop 实现必须如实说明保护级别）。 */
    fun platform(): String

    /** 保护级别说明：供界面/诊断显示，不得夸大。 */
    fun protectionSummary(): String

    suspend fun wrap(alias: String, secret: SecretBuffer): WrappedSecret

    suspend fun unwrap(alias: String, wrapped: WrappedSecret): SecretBuffer

    suspend fun delete(alias: String)
}
