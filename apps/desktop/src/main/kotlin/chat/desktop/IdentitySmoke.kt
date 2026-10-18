package chat.desktop

import chat.api.IdentityException
import chat.bridge.createIdentityService
import chat.presentation.display.InviteCheck
import chat.presentation.display.presentContact
import chat.presentation.display.presentInvitePreview
import kotlinx.coroutines.runBlocking

/**
 * P1-02 身份竖直链路冒烟：Kotlin → UniFFI → crates/identity（真实 Ed25519 / inv1 口令 / 联系人状态机）。
 *
 * 用两个独立身份做真实交互：A 生成口令、B 校验并接受，再验证篡改/自签/关闭后的拒绝。
 * 全程本机、无网络；口令是人工传递模型。
 */
fun main() = runBlocking {
    val alice = createIdentityService()
    val bob = createIdentityService()
    // 节点标识必须是 PROTOCOL-001 规范文本；两个对象必须使用同一个 node，
    // 因为 inv1 口令绑定 node，accept 时会校验一致性。
    val node = "node:1:" + "0a".repeat(32)
    try {
        val accountA = alice.create("林然", node)
        val accountB = bob.create("周其", node)
        check(accountA.accountId.startsWith("acct:1:")) { "账户 ID 必须是 PROTOCOL-001 规范文本" }
        check(accountB.deviceId.startsWith("dev:1:"))
        check(accountA.fingerprint.length >= 70) { "指纹应为分组十六进制" }
        check(accountA.fingerprint != accountB.fingerprint)

        val inviteFromA = alice.createInvite(24 * 60 * 60 * 1000L)
        check(inviteFromA.token.startsWith("inv1.")) { "口令应是 inv1 前缀" }

        // B 校验 A 的口令：必须看到 A 的真实指纹
        val summary = bob.previewInvite(inviteFromA.token)
        check(summary.fingerprint == accountA.fingerprint) { "校验结果必须给出邀请方真实指纹" }
        check(summary.accountId == accountA.accountId)
        val ui = presentInvitePreview(summary, nowMillis = System.currentTimeMillis())
        check(ui.check == InviteCheck.VALID && ui.valid)

        // 篡改最后一个字符必须被签名拒绝
        val lastChar = inviteFromA.token.last()
        val replacement = if (lastChar == '0') '1' else '0'
        val tampered = inviteFromA.token.dropLast(1) + replacement
        val tamperedError = runCatching { bob.previewInvite(tampered) }.exceptionOrNull()
        check(tamperedError is IdentityException) { "篡改口令必须抛 IdentityException" }
        check((tamperedError as IdentityException).code == "INVALID_SIGNATURE") {
            "篡改口令应返回 INVALID_SIGNATURE，实际 " + tamperedError.code
        }

        // B 接受 A 的口令 → requested 联系人，且不能发送
        val contact = bob.acceptInvite(inviteFromA.token, "林然")
        check(contact.state == "requested") { "接受后应为 requested，实际 " + contact.state }
        check(!contact.canSend)
        val contactUi = presentContact(contact)
        check(!contactUi.canSend && contactUi.blockingReason != null)
        val contacts = bob.contacts()
        check(contacts.size == 1 && contacts.first().accountId == accountA.accountId)

        // 自己给自己生成的口令不能被自己接受（绑定校验）
        val selfInvite = alice.createInvite(60_000L)
        val selfError = runCatching { alice.acceptInvite(selfInvite.token, "自己") }.exceptionOrNull()
        check(selfError is IdentityException) { "自签口令必须被拒绝" }
        println("      自签口令被拒绝：" + (selfError as IdentityException).code)

        // 关闭后：需要密钥的调用必须失败（CORE_CLOSED），公开快照仍可读
        bob.close()
        val closedError = runCatching { bob.createInvite(60_000L) }.exceptionOrNull()
        check(closedError is IdentityException) { "关闭后需要密钥的调用必须被拒绝" }
        check((closedError as IdentityException).code == "CORE_CLOSED") {
            "关闭后应返回 CORE_CLOSED，实际 " + closedError.code
        }

        println("PASS identity vertical slice: Kotlin -> UniFFI -> identity (invite/tamper/self/closed)")
        println("      fingerprints verified, request state blocks send, tamper -> INVALID_SIGNATURE")
    } finally {
        runCatching { alice.close() }
        runCatching { bob.close() }
    }
}
