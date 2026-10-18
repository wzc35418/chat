package chat.desktop

import chat.platform.KeyProtectionCodes
import chat.platform.KeyProtectionException
import chat.platform.SecretBuffer
import kotlinx.coroutines.runBlocking
import java.io.File
import java.nio.file.Files

/**
 * PLATFORM-001/002/003 桌面竖切冒烟：口令派生封装主密钥。
 *
 * 真实证据：wrap→磁盘只有密文与盐、unwrap 往返一致、错误口令显式 KEY_INVALIDATED、
 * 连续失败达到上限后锁定 KEY_PROTECTION_UNAVAILABLE、delete 后不可再解。
 */
fun main() = runBlocking {
    val directory = Files.createTempDirectory("chat-keyslot-").toFile()
    val key = ByteArray(32) { index -> (index * 7 + 1).toByte() }
    val alias = "storage-main"
    try {
        val slot = PassphraseKeySlot(directory, "correct horse battery".toCharArray())
        check(slot.platform() == "desktop-passphrase")
        check(slot.protectionSummary().contains("AES-256-GCM"))

        val buffer = SecretBuffer(key)
        val wrapped = slot.wrap(alias, buffer)
        buffer.close()
        check(wrapped.keyId == alias && wrapped.version == 1)

        // 磁盘上只有密文与盐，不得出现明文主密钥。
        val files = directory.listFiles() ?: emptyArray()
        check(files.isNotEmpty())
        for (file in files) {
            val bytes = file.readBytes()
            check(!containsSubsequence(bytes, key)) { "磁盘上不得出现明文主密钥：" + file.name }
        }

        val restored = slot.unwrap(alias, wrapped)
        check(restored.size() == key.size)
        check(restored.copyBytes().contentEquals(key))
        restored.close()

        // 错误口令：显式失败，不降级。
        val wrong = PassphraseKeySlot(directory, "wrong passphrase".toCharArray())
        val failure = runCatching { wrong.unwrap(alias, wrapped) }.exceptionOrNull()
        check(failure is KeyProtectionException) { "错误口令必须显式失败" }
        check((failure as KeyProtectionException).code == KeyProtectionCodes.KEY_INVALIDATED)

        // 连续失败达到上限后锁定，不再无限尝试。
        repeat(PassphraseKeySlot.MAX_ATTEMPTS) {
            runCatching { wrong.unwrap(alias, wrapped) }
        }
        val locked = runCatching { wrong.unwrap(alias, wrapped) }.exceptionOrNull()
        check(locked is KeyProtectionException)
        check((locked as KeyProtectionException).code == KeyProtectionCodes.UNAVAILABLE) {
            "失败达到上限后必须锁定，实际 " + locked.code
        }

        // 正确口令在锁定后也不放行（需要重新设置本机口令）。
        val lockedCorrect = runCatching { slot.unwrap(alias, wrapped) }.exceptionOrNull()
        check(lockedCorrect is KeyProtectionException)
        check((lockedCorrect as KeyProtectionException).code == KeyProtectionCodes.UNAVAILABLE)

        println("PASS key slot: wrap/unwrap roundtrip, no plaintext on disk, wrong passphrase fails, lockout after " + PassphraseKeySlot.MAX_ATTEMPTS + " attempts")
    } finally {
        directory.deleteRecursively()
    }
}

private fun containsSubsequence(haystack: ByteArray, needle: ByteArray): Boolean {
    if (needle.isEmpty() || haystack.size < needle.size) return false
    outer@ for (start in 0..haystack.size - needle.size) {
        for (offset in needle.indices) {
            if (haystack[start + offset] != needle[offset]) continue@outer
        }
        return true
    }
    return false
}
