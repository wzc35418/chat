package chat.desktop

import chat.platform.KeyProtection
import chat.platform.KeyProtectionCodes
import chat.platform.KeyProtectionException
import chat.platform.SecretBuffer
import chat.platform.WrappedSecret
import java.io.File
import java.nio.file.Files
import java.security.SecureRandom
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.PBEKeySpec
import javax.crypto.spec.SecretKeySpec

/**
 * 桌面版密钥封装（PLATFORM-001/002/003 的 JVM 实现）。
 *
 * 保护级别**如实标注**：口令派生的 AES-256-GCM + 本地密文文件，
 * 不是操作系统密钥链（Windows Credential Manager / DPAPI 未接线）。
 * 因此：口令错了就解不开（PBKDF2 迭代 210k），连续失败会锁定（不无限重试）；
 * 磁盘上只有密文与盐，没有明文主密钥。
 */
class PassphraseKeySlot(
    private val directory: File,
    private val passphrase: CharArray,
    private val iterations: Int = 210_000,
) : KeyProtection {

    private val random = SecureRandom()

    override fun platform(): String = "desktop-passphrase"

    override fun protectionSummary(): String =
        "口令派生 AES-256-GCM（PBKDF2 " + iterations + " 轮）；未接操作系统密钥链，磁盘只有密文与盐。"

    override suspend fun wrap(alias: String, secret: SecretBuffer): WrappedSecret {
        requireAlias(alias)
        val plaintext = secret.copyBytes()
        try {
            if (plaintext.isEmpty() || plaintext.size != 32) {
                throw KeyProtectionException(KeyProtectionCodes.INVALID_INPUT, "主密钥必须是 32 字节")
            }
            val salt = ByteArray(16).also(random::nextBytes)
            val key = deriveKey(salt)
            val nonce = ByteArray(12).also(random::nextBytes)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(128, nonce))
            cipher.updateAAD(alias.toByteArray(Charsets.UTF_8))
            val ciphertext = cipher.doFinal(plaintext)
            clear(key)
            writeEnvelope(alias, salt, nonce, ciphertext)
            return WrappedSecret(
                version = 1,
                keyId = alias,
                platform = platform(),
                nonceBase64 = Base64.getEncoder().encodeToString(nonce),
                ciphertextBase64 = Base64.getEncoder().encodeToString(ciphertext),
            )
        } finally {
            plaintext.fill(0)
        }
    }

    override suspend fun unwrap(alias: String, wrapped: WrappedSecret): SecretBuffer {
        requireAlias(alias)
        if (wrapped.version != 1 || wrapped.keyId != alias || wrapped.platform != platform()) {
            throw KeyProtectionException(KeyProtectionCodes.KEY_INVALIDATED, "密钥包元数据与当前平台不匹配")
        }
        val envelope = readEnvelope(alias)
        val attempts = readAttempts(alias)
        if (attempts >= MAX_ATTEMPTS) {
            throw KeyProtectionException(
                KeyProtectionCodes.UNAVAILABLE,
                "尝试次数过多，已锁定；需要重新设置本机口令后才能恢复。",
            )
        }
        val key = deriveKey(envelope.salt)
        return try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(
                Cipher.DECRYPT_MODE,
                SecretKeySpec(key, "AES"),
                GCMParameterSpec(128, envelope.nonce),
            )
            cipher.updateAAD(alias.toByteArray(Charsets.UTF_8))
            val plaintext = cipher.doFinal(envelope.ciphertext)
            if (plaintext.size != 32) {
                throw KeyProtectionException(KeyProtectionCodes.KEY_INVALIDATED, "解出的主密钥长度不符")
            }
            writeAttempts(alias, 0)
            SecretBuffer(plaintext)
        } catch (error: KeyProtectionException) {
            throw error
        } catch (error: Exception) {
            writeAttempts(alias, attempts + 1)
            throw KeyProtectionException(
                KeyProtectionCodes.KEY_INVALIDATED,
                "口令不正确或密钥包已损坏（失败 " + (attempts + 1) + "/" + MAX_ATTEMPTS + "）",
            )
        } finally {
            clear(key)
        }
    }

    override suspend fun unwrapByAlias(alias: String): SecretBuffer {
        requireAlias(alias)
        val envelope = readEnvelope(alias)
        val attempts = readAttempts(alias)
        if (attempts >= MAX_ATTEMPTS) {
            throw KeyProtectionException(
                KeyProtectionCodes.UNAVAILABLE,
                "尝试次数过多，已锁定；需要重新设置本机口令后才能恢复。",
            )
        }
        val key = deriveKey(envelope.salt)
        return try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(
                Cipher.DECRYPT_MODE,
                SecretKeySpec(key, "AES"),
                GCMParameterSpec(128, envelope.nonce),
            )
            cipher.updateAAD(alias.toByteArray(Charsets.UTF_8))
            val plaintext = cipher.doFinal(envelope.ciphertext)
            if (plaintext.size != 32) {
                throw KeyProtectionException(KeyProtectionCodes.KEY_INVALIDATED, "解出的主密钥长度不符")
            }
            writeAttempts(alias, 0)
            SecretBuffer(plaintext)
        } catch (error: KeyProtectionException) {
            throw error
        } catch (error: Exception) {
            writeAttempts(alias, attempts + 1)
            throw KeyProtectionException(
                KeyProtectionCodes.KEY_INVALIDATED,
                "口令不正确或密钥包已损坏（失败 " + (attempts + 1) + "/" + MAX_ATTEMPTS + "）",
            )
        } finally {
            clear(key)
        }
    }

    override suspend fun delete(alias: String) {
        requireAlias(alias)
        envelopeFile(alias).delete()
        attemptFile(alias).delete()
    }

    private fun requireAlias(alias: String) {
        if (alias.isBlank() || alias.length > 64 || alias.any { it == '/' || it == '\\' || it.isISOControl() }) {
            throw KeyProtectionException(KeyProtectionCodes.INVALID_INPUT, "别名不合法")
        }
    }

    private fun deriveKey(salt: ByteArray): ByteArray {
        val factory = SecretKeyFactory.getInstance("PBKDF2WithHmacSHA256")
        val spec = PBEKeySpec(passphrase, salt, iterations, 256)
        return factory.generateSecret(spec).encoded
    }

    private fun envelopeFile(alias: String) = File(directory, alias + ".key")

    private fun attemptFile(alias: String) = File(directory, alias + ".attempts")

    private fun writeEnvelope(alias: String, salt: ByteArray, nonce: ByteArray, ciphertext: ByteArray) {
        if (!directory.exists() && !directory.mkdirs()) {
            throw KeyProtectionException(KeyProtectionCodes.UNAVAILABLE, "无法创建密钥目录")
        }
        val payload = buildString {
            append("v1\n")
            append(Base64.getEncoder().encodeToString(salt)).append('\n')
            append(Base64.getEncoder().encodeToString(nonce)).append('\n')
            append(Base64.getEncoder().encodeToString(ciphertext))
        }
        val target = envelopeFile(alias)
        target.writeText(payload)
        runCatching { Files.setPosixFilePermissions(target.toPath(), emptySet()) }
        writeAttempts(alias, 0)
    }

    private class Envelope(val salt: ByteArray, val nonce: ByteArray, val ciphertext: ByteArray)

    private fun readEnvelope(alias: String): Envelope {
        val file = envelopeFile(alias)
        if (!file.exists()) {
            throw KeyProtectionException(KeyProtectionCodes.UNAVAILABLE, "本机没有该别名的密钥包")
        }
        val parts = file.readLines()
        if (parts.size != 4 || parts[0] != "v1") {
            throw KeyProtectionException(KeyProtectionCodes.KEY_INVALIDATED, "密钥包格式不认识")
        }
        return Envelope(
            salt = Base64.getDecoder().decode(parts[1]),
            nonce = Base64.getDecoder().decode(parts[2]),
            ciphertext = Base64.getDecoder().decode(parts[3]),
        )
    }

    private fun readAttempts(alias: String): Int =
        attemptFile(alias).takeIf { it.exists() }?.readText()?.trim()?.toIntOrNull() ?: 0

    private fun writeAttempts(alias: String, value: Int) {
        runCatching { attemptFile(alias).writeText(value.toString()) }
    }

    private fun clear(bytes: ByteArray) = bytes.fill(0)

    companion object {
        const val MAX_ATTEMPTS = 5
    }
}
