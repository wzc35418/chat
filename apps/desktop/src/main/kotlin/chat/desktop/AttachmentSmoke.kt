package chat.desktop

import chat.api.LocalStoreException
import chat.bridge.createAttachmentService
import kotlinx.coroutines.runBlocking
import java.io.File
import java.nio.file.Files

/**
 * ATTACHMENTS 本机链路冒烟：Kotlin → UniFFI → crates/core 分块加密缓存 → 解密导出。
 *
 * 真实文件、真实加解密：导入后磁盘缓存无明文、导出内容与原文逐字节一致、
 * 超过 25 MiB 显式拒绝、错误 ID 返回稳定错误码。
 */
fun main() = runBlocking {
    val directory = Files.createTempDirectory("chat-attachment-").toFile()
    val cacheRoot = File(directory, "cache").absolutePath
    val source = File(directory, "source.bin")
    val content = ByteArray(300_000) { index -> (index % 251).toByte() }
    source.writeBytes(content)
    val target = File(directory, "exported.bin")
    val service = createAttachmentService(cacheRoot)
    try {
        val attachment = service.importFile(source.absolutePath, "示例附件.bin")
        check(attachment.attachmentId.startsWith("att-"))
        check(attachment.plainSize == content.size.toLong())
        check(attachment.chunkCount >= 2) { "300KB 应被分成多块" }
        check(attachment.state == "imported")

        // 缓存目录里不得出现明文
        val cacheBytes = File(cacheRoot).walkTopDown().filter { it.isFile }
            .flatMap { file -> file.readBytes().asSequence() }.toList().toByteArray()
        val plainLeak = String(cacheBytes, Charsets.ISO_8859_1).contains(String(content, Charsets.ISO_8859_1))
        check(!plainLeak) { "缓存中不得出现明文" }

        val written = service.exportFile(attachment.attachmentId, target.absolutePath)
        check(written == content.size.toLong())
        check(target.readBytes().contentEquals(content)) { "导出内容必须与原文逐字节一致" }

        val status = service.status(attachment.attachmentId)
        check(status.attachmentId == attachment.attachmentId)

        val cancelled = service.cancel(attachment.attachmentId)
        check(cancelled.cancelRequested)

        val wrong = runCatching { service.status("att-ffffffffffffffffffffffffffffffff") }.exceptionOrNull()
        check(wrong is LocalStoreException) { "未知附件必须显式失败" }
        println("      未知附件错误码：" + (wrong as LocalStoreException).code)

        // 超过 25 MiB 必须显式拒绝（只写稀疏文件头，避免真的占 25MiB 内存）
        val huge = File(directory, "huge.bin")
        java.io.RandomAccessFile(huge, "rw").use { it.setLength(25L * 1024 * 1024 + 1) }
        val tooLarge = runCatching { service.importFile(huge.absolutePath, "huge.bin") }.exceptionOrNull()
        check(tooLarge is LocalStoreException) { "超过 25 MiB 必须拒绝" }
        check((tooLarge as LocalStoreException).code == "CONTENT_TOO_LARGE")

        println("PASS attachment local slice: import/export roundtrip, no plaintext cache, 25MiB rejected")
    } finally {
        directory.deleteRecursively()
    }
}
