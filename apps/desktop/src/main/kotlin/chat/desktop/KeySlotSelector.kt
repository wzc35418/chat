package chat.desktop

import chat.platform.KeyProtection
import chat.platform.KeyProtectionCodes
import chat.platform.KeyProtectionException
import chat.platform.SecretBuffer
import java.io.File

/** 密钥槽后端。 */
enum class KeySlotBackend(val id: String, val label: String) {
    CredentialManager("windows-credential-manager", "Windows 凭据管理器"),
    DpapiFile("windows-dpapi", "DPAPI 文件（当前用户作用域）"),
    None("none", "（本机还没有密钥）"),
}

/** 探测结果：选了哪个后端、依据是什么。 */
data class KeySlotSelection(val backend: KeySlotBackend, val detail: String)

/**
 * 密钥槽后端探测与迁移。
 *
 * 探测顺序（**只看本机实际有什么，不猜**）：
 * 1) 凭据管理器里有 → 用它；
 * 2) 否则磁盘上有 DPAPI blob → 用它（兼容老版本）；
 * 3) 都没有 → None，detail 说明首次运行会新建。
 *
 * 迁移语义：**读旧 → 写新 → 读回校验 → 才删旧**；任一步失败都保留旧密钥。
 */
class KeySlotSelector(
    private val credentialManager: WinCredKeySlot,
    private val dpapi: DpapiKeySlot,
    private val isWindows: Boolean = System.getProperty("os.name", "").lowercase().contains("windows"),
) {
    fun probe(alias: String): KeySlotSelection {
        if (!isWindows) return KeySlotSelection(KeySlotBackend.None, "非 Windows 平台：本实现不适用")
        if (credentialManager.exists(alias)) {
            return KeySlotSelection(KeySlotBackend.CredentialManager, "凭据管理器里已有该别名的密钥")
        }
        if (dpapi.exists(alias)) {
            return KeySlotSelection(KeySlotBackend.DpapiFile, "磁盘上存在 DPAPI blob（老版本写入）")
        }
        return KeySlotSelection(KeySlotBackend.None, "本机还没有该别名的密钥；首次运行会按偏好新建")
    }

    fun slotFor(backend: KeySlotBackend): KeyProtection = when (backend) {
        KeySlotBackend.CredentialManager -> credentialManager
        KeySlotBackend.DpapiFile -> dpapi
        KeySlotBackend.None -> throw KeyProtectionException(
            KeyProtectionCodes.INVALID_INPUT,
            "没有可用的密钥槽后端",
        )
    }

    /** 把 alias 的密钥从 from 迁移到 to；失败保留旧密钥。 */
    suspend fun migrate(alias: String, from: KeyProtection, to: KeyProtection): String {
        if (!isWindows) {
            throw KeyProtectionException(KeyProtectionCodes.UNAVAILABLE, "密钥槽迁移只在 Windows 上实现")
        }
        val secret = from.unwrapByAlias(alias)
        val bytes = secret.copyBytes()
        try {
            to.wrap(alias, SecretBuffer(bytes.copyOf()))
        } catch (error: Throwable) {
            runCatching { to.delete(alias) }
            throw KeyProtectionException(
                (error as? KeyProtectionException)?.code ?: KeyProtectionCodes.UNAVAILABLE,
                "迁移写入失败，旧密钥保留：" + (error.message ?: error::class.simpleName),
            )
        }
        val readBack = try {
            to.unwrapByAlias(alias)
        } catch (error: Throwable) {
            runCatching { to.delete(alias) }
            throw KeyProtectionException(
                KeyProtectionCodes.KEY_INVALIDATED,
                "迁移后读回失败，旧密钥保留：" + (error.message ?: error::class.simpleName),
            )
        }
        val verify = readBack.copyBytes()
        val identical = verify.contentEquals(bytes)
        verify.fill(0)
        if (!identical) {
            runCatching { to.delete(alias) }
            throw KeyProtectionException(KeyProtectionCodes.KEY_INVALIDATED, "迁移后读回不一致，旧密钥保留")
        }
        from.delete(alias)
        return "已迁移到 " + to.platform()
    }

    companion object {
        fun default(credentialDirectory: File): KeySlotSelector =
            KeySlotSelector(WinCredKeySlot(), DpapiKeySlot(credentialDirectory))
    }
}
