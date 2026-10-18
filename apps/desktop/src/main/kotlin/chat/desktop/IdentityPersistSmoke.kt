package chat.desktop

import chat.api.IdentityException
import chat.bridge.createIdentityService
import kotlinx.coroutines.runBlocking
import java.io.File
import java.nio.file.Files
import java.security.SecureRandom

/**
 * P1-02 身份持久化竖直链路：Kotlin → UniFFI → core.open_persisted_identity → 加密库。
 *
 * 真实证据：同一库 + 同一主密钥重开后 account_id / device_id / fingerprint 完全一致；
 * 接受的口令联系人重开后仍在；错误主密钥被拒绝；首次打开会生成并写入。
 */
fun main() = runBlocking {
    val directory = Files.createTempDirectory("chat-identity-persist-").toFile()
    val database = File(directory, "identity.db").absolutePath
    val keyBytes = ByteArray(32).also { SecureRandom().nextBytes(it) }
    val masterKey = keyBytes.joinToString("") { "%02x".format(it) }
    val wrongKey = "ff".repeat(32)
    val node = "node:1:" + "0a".repeat(32)

    var fingerprint: String? = null
    var accountId: String? = null
    var deviceId: String? = null
    var contactFingerprint: String? = null

    try {
        // 第一次打开：生成并写入
        val first = createIdentityService()
        val created = first.openPersisted(database, masterKey, node)
        check(created.accountId.startsWith("acct:1:"))
        check(created.fingerprint.isNotBlank())
        fingerprint = created.fingerprint
        accountId = created.accountId
        deviceId = created.deviceId

        // 接受一条真实口令 → 联系人落库
        val peer = createIdentityService()
        val peerAccount = peer.create("对端", node)
        val invite = peer.createInvite(24L * 60 * 60 * 1000)
        val contact = first.acceptInvite(invite.token, "对端")
        check(contact.state == "requested" && !contact.canSend)
        check(contact.fingerprint == peerAccount.fingerprint)
        contactFingerprint = contact.fingerprint
        first.close()
        peer.close()

        // 第二次打开：同一库 + 同一主密钥必须恢复同一身份与联系人
        val second = createIdentityService()
        val reopened = second.openPersisted(database, masterKey, node)
        check(reopened.accountId == accountId) { "重开后账户 ID 必须一致" }
        check(reopened.deviceId == deviceId) { "重开后设备 ID 必须一致" }
        check(reopened.fingerprint == fingerprint) { "重开后指纹必须一致" }
        val contacts = second.contacts()
        check(contacts.size == 1) { "重开后联系人应仍在，实际 " + contacts.size }
        check(contacts.first().fingerprint == contactFingerprint)
        check(contacts.first().state == "requested")
        second.close()

        // 错误主密钥必须被拒绝（且不覆盖已有身份）
        val third = createIdentityService()
        val rejected = runCatching { third.openPersisted(database, wrongKey, node) }.exceptionOrNull()
        check(rejected is IdentityException) { "错误主密钥必须抛 IdentityException" }
        println("      错误主密钥被拒绝：" + (rejected as IdentityException).code)
        third.close()

        // 拒绝后原密钥仍可打开同一身份
        val fourth = createIdentityService()
        val again = fourth.openPersisted(database, masterKey, node)
        check(again.fingerprint == fingerprint)
        fourth.close()

        println("PASS identity persistence: reopen keeps account/device/fingerprint and contacts; wrong key rejected")
    } finally {
        directory.deleteRecursively()
    }
}
