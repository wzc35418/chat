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
import java.io.File
import java.util.Base64

/** Win32 DATA_BLOB（自带定义，避免额外依赖 jna-platform）。 */
@Structure.FieldOrder("cbData", "pbData")
class DataBlob : Structure() {
    @JvmField var cbData: Int = 0
    @JvmField var pbData: Pointer? = null

    fun setBytes(bytes: ByteArray) {
        val memory = Memory(bytes.size.toLong().coerceAtLeast(1))
        memory.write(0, bytes, 0, bytes.size)
        pbData = memory
        cbData = bytes.size
        write()
    }

    fun readBytes(): ByteArray {
        read()
        val size = cbData
        if (size <= 0) return ByteArray(0)
        val bytes = ByteArray(size)
        pbData?.read(0, bytes, 0, size)
        return bytes
    }
}

private interface Crypt32 : Library {
    fun CryptProtectData(
        dataIn: DataBlob,
        description: String?,
        entropy: DataBlob?,
        reserved: Pointer?,
        prompt: Pointer?,
        flags: Int,
        dataOut: DataBlob,
    ): Boolean

    fun CryptUnprotectData(
        dataIn: DataBlob,
        description: Pointer?,
        entropy: DataBlob?,
        reserved: Pointer?,
        prompt: Pointer?,
        flags: Int,
        dataOut: DataBlob,
    ): Boolean

    companion object {
        val INSTANCE: Crypt32 = Native.load("crypt32", Crypt32::class.java)
    }
}

/**
 * Windows DPAPI 密钥封装（PLATFORM-001/002 的桌面实现）。
 *
 * 保护级别如实标注：密文由**当前 Windows 用户**的 DPAPI 主密钥保护，换用户/换机器无法解包；
 * 磁盘上只有密文 blob。非 Windows 平台应回退到口令派生方案（PassphraseKeySlot）。
 */
class DpapiKeySlot(private val directory: File) : KeyProtection {

    override fun platform(): String = "windows-dpapi"

    override fun protectionSummary(): String =
        "Windows DPAPI（当前用户作用域）；换用户或换机器无法解包，磁盘只存密文 blob。"

    override suspend fun wrap(alias: String, secret: SecretBuffer): WrappedSecret {
        requireAlias(alias)
        requireWindows()
        val plaintext = secret.copyBytes()
        try {
            if (plaintext.size != 32) {
                throw KeyProtectionException(KeyProtectionCodes.INVALID_INPUT, "主密钥必须是 32 字节")
            }
            val input = DataBlob().apply { setBytes(plaintext) }
            val output = DataBlob()
            val ok = Crypt32.INSTANCE.CryptProtectData(
                input,
                alias,
                null,
                null,
                null,
                CRYPTPROTECT_UI_FORBIDDEN,
                output,
            )
            if (!ok) {
                throw KeyProtectionException(KeyProtectionCodes.UNAVAILABLE, "DPAPI 加密失败")
            }
            val cipher = output.readBytes()
            writeBlob(alias, cipher)
            return WrappedSecret(
                version = 1,
                keyId = alias,
                platform = platform(),
                nonceBase64 = "",
                ciphertextBase64 = Base64.getEncoder().encodeToString(cipher),
            )
        } finally {
            plaintext.fill(0)
        }
    }

    override suspend fun unwrap(alias: String, wrapped: WrappedSecret): SecretBuffer = unwrapByAlias(alias)

    override suspend fun unwrapByAlias(alias: String): SecretBuffer {
        requireAlias(alias)
        requireWindows()
        val blob = readBlob(alias)
        val input = DataBlob().apply { setBytes(blob) }
        val output = DataBlob()
        val ok = Crypt32.INSTANCE.CryptUnprotectData(input, null, null, null, null, CRYPTPROTECT_UI_FORBIDDEN, output)
        if (!ok) {
            throw KeyProtectionException(KeyProtectionCodes.KEY_INVALIDATED, "DPAPI 解包失败（不是当前用户或数据损坏）")
        }
        val plaintext = output.readBytes()
        if (plaintext.size != 32) {
            plaintext.fill(0)
            throw KeyProtectionException(KeyProtectionCodes.KEY_INVALIDATED, "解出的主密钥长度不符")
        }
        return SecretBuffer(plaintext)
    }

    override suspend fun delete(alias: String) {
        requireAlias(alias)
        blobFile(alias).delete()
    }

    private fun requireAlias(alias: String) {
        if (alias.isBlank() || alias.length > 64 || alias.any { it == '/' || it == '\\' || it.isISOControl() }) {
            throw KeyProtectionException(KeyProtectionCodes.INVALID_INPUT, "别名不合法")
        }
    }

    private fun requireWindows() {
        if (!System.getProperty("os.name", "").lowercase().contains("windows")) {
            throw KeyProtectionException(
                KeyProtectionCodes.UNAVAILABLE,
                "DPAPI 只在 Windows 可用；请改用工口令派生密钥槽",
            )
        }
    }

    /** 只读探测：该别名在本机是否已有 DPAPI blob 文件。 */
    fun exists(alias: String): Boolean = blobFile(alias).isFile

    private fun blobFile(alias: String) = File(directory, alias + ".dpapi")

    private fun writeBlob(alias: String, cipher: ByteArray) {
        if (!directory.exists() && !directory.mkdirs()) {
            throw KeyProtectionException(KeyProtectionCodes.UNAVAILABLE, "无法创建密钥目录")
        }
        blobFile(alias).writeBytes(cipher)
    }

    private fun readBlob(alias: String): ByteArray {
        val file = blobFile(alias)
        if (!file.exists()) {
            throw KeyProtectionException(KeyProtectionCodes.UNAVAILABLE, "本机没有该别名的 DPAPI blob")
        }
        return file.readBytes()
    }

    companion object {
        private const val CRYPTPROTECT_UI_FORBIDDEN = 0x1
    }
}
