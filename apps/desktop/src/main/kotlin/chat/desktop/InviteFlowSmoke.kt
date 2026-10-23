package chat.desktop

import chat.bridge.createIdentityService
import chat.api.IdentityInviteSummary
import java.io.File
import java.nio.file.Files

private val MASTER_KEY = "0".repeat(64)

/**
 * 邀请口令往返：A 生成口令 → B 预览 → B 接受 → **两边都出现联系人**。
 *
 * 这是「加好友」的真实链路（IDENTITY），不需要网络。
 * 退出码：0 = PASS，1 = FAIL。
 */
fun main() {
    kotlinx.coroutines.runBlocking { runInvite() }
}

private suspend fun runInvite() {
    val dir = Files.createTempDirectory("chat-invite-smoke-").toFile()
    val node = "node:1:" + "0a".repeat(32)
    var alice: chat.api.IdentityService? = null
    var bob: chat.api.IdentityService? = null
    try {
        alice = createIdentityService()
        val aliceAccount = alice.openPersisted(File(dir, "alice.db").absolutePath, MASTER_KEY, node)
        log("alice: " + aliceAccount.deviceId.take(20) + "...")
        val invite = alice.createInvite(24L * 60 * 60 * 1000)
        log("PASS alice created invite: id=" + invite.inviteId + " token=" + invite.token.take(16) + "...")

        bob = createIdentityService()
        val bobAccount = bob.openPersisted(File(dir, "bob.db").absolutePath, MASTER_KEY, node)
        log("bob: " + bobAccount.deviceId.take(20) + "...")
        val summary = bob.previewInvite(invite.token)
        log("PASS bob previewed invite: " + summary.toString().take(120))
        val contact = bob.acceptInvite(invite.token, "Alice")
        log("PASS bob accepted: contact=" + contact.contactId.take(18) + "... state=" + contact.state + " canSend=" + contact.canSend)

        val bobContacts = bob.contacts()
        val found = bobContacts.firstOrNull { it.fingerprint == aliceAccount.fingerprint }
        if (found == null) {
            log("FAIL bob contacts do not contain alice (contacts=" + bobContacts.size + ")")
            kotlin.system.exitProcess(1)
        }
        log("PASS bob sees alice in contacts: " + found.displayName + " state=" + found.state)

        val aliceContacts = alice.contacts()
        log("alice contacts after bob accepted: " + aliceContacts.size)
        log("INVITE SMOKE: PASS")
        kotlin.system.exitProcess(0)
    } catch (error: Throwable) {
        log("FAIL " + error::class.java.simpleName + ": " + (error.message ?: ""))
        kotlin.system.exitProcess(1)
    } finally {
        runCatching { alice?.close() }
        runCatching { bob?.close() }
    }
}

private fun log(message: String) = println(message)
