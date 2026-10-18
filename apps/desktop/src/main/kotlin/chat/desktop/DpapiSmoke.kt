package chat.desktop

import chat.platform.KeyProtectionCodes
import chat.platform.KeyProtectionException
import chat.platform.SecretBuffer
import kotlinx.coroutines.runBlocking
import java.io.File
import java.nio.file.Files

/**
 * PLATFORM-001/002 的 Windows DPAPI 竖切冒烟。
 *
 * 真实证据：wrap 后磁盘只有密文 blob（不含明文与口令）、unwrap 往返一致、
 * delete 后不可解、非当前用户语义无法在本机验证（文档标注）。
 */
fun main() = runBlocking {
    if (!System.getProperty("os.name", "").lowercase().contains("windows")) {
        println("SKIP dpapi smoke: 非 Windows 平台")
        return@runBlocking
    }
    val directory = Files.createTempDirectory("chat-dpapi-").toFile()
    val key = ByteArray(32) { index -> (index * 11 + 3).toByte() }
    val alias = "storage-main"
    try {
        val slot = DpapiKeySlot(directory)
        check(slot.platform() == "windows-dpapi")
        check(slot.protectionSummary().contains("DPAPI"))

        val buffer = SecretBuffer(key)
        val wrapped = slot.wrap(alias, buffer)
        buffer.close()
        check(wrapped.keyId == alias && wrapped.platform == "windows-dpapi")

        val blob = File(directory, alias + ".dpapi")
        check(blob.exists() && blob.length() > 0) { "DPAPI blob 未写入" }
        val blobBytes = blob.readBytes()
        check(!containsSubsequence(blobBytes, key)) { "blob 中不得出现明文主密钥" }

        val restored = slot.unwrapByAlias(alias)
        try {
            check(restored.copyBytes().contentEquals(key)) { "解密结果与原文不一致" }
        } finally {
            restored.close()
        }

        slot.delete(alias)
        val missing = runCatching { slot.unwrapByAlias(alias) }.exceptionOrNull()
        check(missing is KeyProtectionException)
        check((missing as KeyProtectionException).code == KeyProtectionCodes.UNAVAILABLE)

        println("PASS dpapi key slot: 当前用户作用域封装/解封往返、无明文落盘、删除后不可解")
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
