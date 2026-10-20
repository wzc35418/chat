package chat.desktop

import chat.platform.KeyProtection
import chat.platform.KeyProtectionCodes
import chat.platform.KeyProtectionException
import chat.platform.SecretBuffer
import chat.platform.WrappedSecret
import com.sun.jna.Library
import com.sun.jna.Memory
import com.sun.jna.Native
import com.sun.jna.Pointer
import com.sun.jna.Structure
import com.sun.jna.WString

/**
 * Windows 凭据管理器（Credential Manager）密钥封装 —— PLATFORM-001/002 的**OS 凭据库**实现。
 *
 * 与 DPAPI 文件方案的差别（如实标注）：
 * - 密钥明文**不再落盘**：交给系统凭据库（`CredWriteW`），本进程与磁盘上都没有 blob 文件；
 * - 访问由系统按**当前用户**管控，换用户/换机器读不到；
 * - 仍不是硬件级保护（有用户态注入权即可读），防护级别写在 [protectionSummary]。
 *
 * 写入的凭据类型为 `CRED_TYPE_GENERIC`、持久性 `CRED_PERSIST_LOCAL_MACHINE`，
 * 目标名统一加前缀 `chat/key-slot/`，避免与其它应用冲突。
 */
class WinCredKeySlot : KeyProtection {

    override fun platform(): String = "windows-credential-manager"

    override fun protectionSummary(): String =
        "Windows Credential Manager（当前用户作用域）；密钥明文不落盘，本机换用户或换机器读不到。"

    override suspend fun wrap(alias: String, secret: SecretBuffer): WrappedSecret {
        requireAlias(alias)
        requireWindows()
        val plaintext = secret.copyBytes()
        try {
            if (plaintext.size != SECRET_BYTES) {
                throw KeyProtectionException(KeyProtectionCodes.INVALID_INPUT, "主密钥必须是 32 字节")
            }
            write(target(alias), plaintext)
            return WrappedSecret(
                version = 1,
                keyId = alias,
                platform = platform(),
                nonceBase64 = "",
                // OS 凭据库不把密文交回调用方：这里刻意留空，避免有人误以为能离线解密。
                ciphertextBase64 = "",
            )
        } finally {
            plaintext.fill(0)
        }
    }

    override suspend fun unwrap(alias: String, wrapped: WrappedSecret): SecretBuffer = unwrapByAlias(alias)

    override suspend fun unwrapByAlias(alias: String): SecretBuffer {
        requireAlias(alias)
        requireWindows()
        val secret = read(target(alias))
        if (secret.size != SECRET_BYTES) {
            secret.fill(0)
            throw KeyProtectionException(KeyProtectionCodes.KEY_INVALIDATED, "凭据库里的密钥长度不符")
        }
        return SecretBuffer(secret)
    }

    override suspend fun delete(alias: String) {
        requireAlias(alias)
        requireWindows()
        // 不存在视为已删除（幂等）；其它失败显式抛错。
        if (!Advapi32.INSTANCE.CredDeleteW(WString(target(alias)), CRED_TYPE_GENERIC, 0)) {
            val error = Native.getLastError()
            if (error != ERROR_NOT_FOUND) {
                throw KeyProtectionException(
                    KeyProtectionCodes.UNAVAILABLE,
                    "凭据删除失败（Win32 错误 " + error + "）",
                )
            }
        }
    }

    /** 只读探测：当前用户下是否存在该别名的凭据。 */
    fun exists(alias: String): Boolean {
        requireAlias(alias)
        requireWindows()
        val out = com.sun.jna.ptr.PointerByReference()
        val ok = Advapi32.INSTANCE.CredReadW(WString(target(alias)), CRED_TYPE_GENERIC, 0, out)
        if (!ok) return false
        try {
            val credential = fromPointer(out.value)
            credential.read()
        } finally {
            Advapi32.INSTANCE.CredFree(out.value)
        }
        return true
    }

    private fun target(alias: String): String = TARGET_PREFIX + alias

    private fun requireAlias(alias: String) {
        if (alias.isBlank() || alias.length > 64 || alias.any { it == '/' || it == '\\' || it.isISOControl() }) {
            throw KeyProtectionException(KeyProtectionCodes.INVALID_INPUT, "别名不合法")
        }
    }

    private fun requireWindows() {
        if (!System.getProperty("os.name", "").lowercase().contains("windows")) {
            throw KeyProtectionException(
                KeyProtectionCodes.UNAVAILABLE,
                "凭据管理器只在 Windows 可用；请改用口令派生密钥槽",
            )
        }
    }

    private fun write(target: String, secret: ByteArray) {
        val blob = Memory(secret.size.toLong())
        blob.write(0, secret, 0, secret.size)
        val credential = CredentialW()
        credential.Flags = 0
        credential.Type = CRED_TYPE_GENERIC
        credential.TargetName = WString(target)
        credential.Comment = WString("chat master key slot")
        credential.CredentialBlobSize = secret.size
        credential.CredentialBlob = blob
        credential.Persist = CRED_PERSIST_LOCAL_MACHINE
        credential.AttributeCount = 0
        credential.Attributes = null
        credential.TargetAlias = null
        credential.UserName = WString(System.getProperty("user.name") ?: "chat")
        credential.write()
        if (!Advapi32.INSTANCE.CredWriteW(credential, 0)) {
            val error = Native.getLastError()
            throw KeyProtectionException(
                KeyProtectionCodes.UNAVAILABLE,
                "凭据写入失败（Win32 错误 " + error + "）",
            )
        }
    }

    private fun read(target: String): ByteArray {
        val out = com.sun.jna.ptr.PointerByReference()
        if (!Advapi32.INSTANCE.CredReadW(WString(target), CRED_TYPE_GENERIC, 0, out)) {
            val error = Native.getLastError()
            if (error == ERROR_NOT_FOUND) {
                throw KeyProtectionException(KeyProtectionCodes.UNAVAILABLE, "本机凭据库里没有该别名的密钥")
            }
            throw KeyProtectionException(
                KeyProtectionCodes.KEY_INVALIDATED,
                "凭据读取失败（Win32 错误 " + error + "）",
            )
        }
        return try {
            val credential = fromPointer(out.value)
            credential.read()
            val size = credential.CredentialBlobSize
            if (size <= 0) ByteArray(0) else {
                val bytes = ByteArray(size)
                credential.CredentialBlob?.read(0, bytes, 0, size)
                bytes
            }
        } finally {
            Advapi32.INSTANCE.CredFree(out.value)
        }
    }

    private companion object {
        const val SECRET_BYTES = 32
        const val CRED_TYPE_GENERIC = 1
        const val CRED_PERSIST_LOCAL_MACHINE = 2
        const val ERROR_NOT_FOUND = 1168
        const val TARGET_PREFIX = "chat/key-slot/"
    }
}

/** CREDENTIALW（自带定义，避免额外依赖 jna-platform）。 */
@Structure.FieldOrder(
    "Flags", "Type", "TargetName", "Comment", "LastWrittenLow", "LastWrittenHigh", "CredentialBlobSize",
    "CredentialBlob", "Persist", "AttributeCount", "Attributes", "TargetAlias", "UserName",
)
class CredentialW : Structure() {
    @JvmField var Flags: Int = 0
    @JvmField var Type: Int = 0
    @JvmField var TargetName: WString? = null
    @JvmField var Comment: WString? = null
    @JvmField var LastWrittenLow = 0
    @JvmField var LastWrittenHigh = 0
    @JvmField var CredentialBlobSize: Int = 0
    @JvmField var CredentialBlob: Pointer? = null
    @JvmField var Persist: Int = 0
    @JvmField var AttributeCount: Int = 0
    @JvmField var Attributes: Pointer? = null
    @JvmField var TargetAlias: WString? = null
    @JvmField var UserName: WString? = null

    // 只用隐式无参构造；"从已有内存读"用 Structure.newInstance 分配，避免重载歧义。
}

private fun fromPointer(pointer: Pointer?): CredentialW =
    Structure.newInstance(CredentialW::class.java, pointer) as CredentialW

private interface Advapi32 : Library {
    fun CredWriteW(credential: CredentialW, flags: Int): Boolean
    fun CredReadW(target: WString, type: Int, flags: Int, credential: com.sun.jna.ptr.PointerByReference): Boolean
    fun CredDeleteW(target: WString, type: Int, flags: Int): Boolean
    fun CredFree(buffer: Pointer?)

    companion object {
        val INSTANCE: Advapi32 = Native.load("advapi32", Advapi32::class.java)
    }
}
