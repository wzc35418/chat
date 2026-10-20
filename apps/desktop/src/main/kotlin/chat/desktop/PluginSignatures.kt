package chat.desktop

import java.io.File
import java.security.MessageDigest
import java.util.jar.JarFile

/** 签名校验结果。 */
data class PluginSignatureResult(
    /** 签名证书 SHA-256 指纹（小写 hex）；未签名时为 null。 */
    val fingerprint: String?,
    /** 是否通过策略检查。 */
    val accepted: Boolean,
)

/**
 * 插件 JAR 的**签名闸门**（与权限门并列的第二道闸）。
 *
 * 语义：
 * 1. `JarFile(file, verify = true)` 会逐条目验签，**被篡改的 JAR 直接抛 SecurityException** → `PLUGIN_SIGNATURE_INVALID`；
 * 2. 取签名证书 SHA-256 指纹；给了 `expectedFingerprint` 时必须**完全匹配**，否则 `PLUGIN_SIGNATURE_MISMATCH`；
 * 3. `requireSignature = true`（默认）时**未签名 JAR 一律拒绝**（`PLUGIN_SIGNATURE_MISSING`）；
 * 4. 允许未签名时必须**显式**传 `requireSignature = false`，不会因为“没配指纹”就默默放行。
 *
 * 明确边界：签名只证明“这个 JAR 自签名后没被改过、且来自持该私钥的人”，
 * **不**证明作者可信；配合权限门才构成可用的准入策略。
 */
object PluginSignatures {

    fun verify(
        jar: File,
        requireSignature: Boolean,
        expectedFingerprint: String?,
    ): PluginSignatureResult {
        val expected = expectedFingerprint?.trim()?.lowercase()?.takeIf { it.isNotEmpty() }
        if (expected != null && !isHex64(expected)) {
            throw PluginLoadException("PLUGIN_SIGNATURE_INVALID", "期望的签名指纹必须是 64 位小写十六进制")
        }

        var fingerprint: String? = null
        // verify = true：任何条目被改动都会在读取时抛异常，这就是完整性校验。
        val archive = try {
            JarFile(jar, true)
        } catch (error: Throwable) {
            throw PluginLoadException("PLUGIN_SIGNATURE_INVALID", "插件 JAR 无法打开：" + (error.message ?: error::class.simpleName))
        }
        try {
            val entries = archive.entries()
            while (entries.hasMoreElements()) {
                val entry = entries.nextElement()
                // 读取内容以触发逐条目验签（缺失/错误签名会在这一步抛出）。
                archive.getInputStream(entry).use { stream -> stream.readBytes() }
                val signers = entry.codeSigners ?: continue
                // codeSigners 元素是 CodeSigner（不是 Certificate[]）：证书在 signerCertPath 里。
                val certificate = signers
                    .firstOrNull()
                    ?.signerCertPath
                    ?.certificates
                    ?.firstOrNull()
                    ?: continue
                val digest = MessageDigest.getInstance("SHA-256").digest(certificate.encoded)
                fingerprint = digest.joinToString("") { b -> String.format("%02x", b.toInt() and 0xff) }
                break
            }
        } catch (error: SecurityException) {
            throw PluginLoadException(
                "PLUGIN_SIGNATURE_INVALID",
                "插件 JAR 签名校验失败（可能被篡改）：" + (error.message ?: "security exception"),
            )
        } catch (error: java.util.zip.ZipException) {
            // 篡改可能落在 deflate 流上：读条目时就炸（invalid distance code / unexpected end）。
            // 这仍然是「完整性被破坏」，必须收敛到同一个稳定码，而不是把 ZipException 漏出去。
            throw PluginLoadException(
                "PLUGIN_SIGNATURE_INVALID",
                "插件 JAR 读取条目失败（可能被篡改）：" + (error.message ?: "zip error"),
            )
        } finally {
            runCatching { archive.close() }
        }

        return when {
            fingerprint == null && requireSignature -> throw PluginLoadException(
                "PLUGIN_SIGNATURE_MISSING",
                "插件未签名；默认要求签名，如确需加载未签名插件请显式允许（不推荐）",
            )
            fingerprint == null -> PluginSignatureResult(fingerprint = null, accepted = true)
            expected != null && fingerprint != expected -> throw PluginLoadException(
                "PLUGIN_SIGNATURE_MISMATCH",
                "签名证书指纹不匹配：期望 " + expected + "，实际 " + fingerprint,
            )
            else -> PluginSignatureResult(fingerprint = fingerprint, accepted = true)
        }
    }

    private fun isHex64(value: String): Boolean =
        value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }
}
