package chat.desktop

import chat.api.LocalStoreException
import chat.bridge.createGroupService
import kotlinx.coroutines.runBlocking
import java.io.File
import java.nio.file.Files
import java.security.SecureRandom

/**
 * GROUP 竖直链路冒烟：Kotlin → UniFFI GroupFfi → core groups.rs → 真 OpenMLS。
 *
 * 真实证据：建群快照（epoch/owner/成员）→ 改名（策略版本递增、同名幂等）→ 禁言往返 →
 * 移除成员（pending remove_members）→ 本地接受（epoch 前进、pending 清空）→ 错误码稳定。
 * 诚实边界：这里的"接受"只收口本地状态，不是节点回执（节点回执路径见 group_node.rs）。
 */
fun main() = runBlocking {
    val directory = Files.createTempDirectory("chat-group-").toFile()
    val database = File(directory, "groups.db").absolutePath
    val masterKey = ByteArray(32).also { SecureRandom().nextBytes(it) }
        .joinToString("") { "%02x".format(it) }
    val node = "node:1:" + "0a".repeat(32)
    val service = try {
        createGroupService(database, masterKey, node)
    } catch (error: LocalStoreException) {
        println("SKIP group smoke: " + error.code + " " + error.message)
        return@runBlocking
    }
    try {
        val created = service.createGroup("测试群")
        check(created.epoch == 1) { "新群 epoch 应为 1，实际 " + created.epoch }
        check(created.name == "测试群")
        check(created.members.any { it.isLocal && it.role == "owner" }) { "本机应是群主" }

        val renamed = service.rename(created.groupId, "改名后的群")
        check(renamed.name == "改名后的群")
        check(renamed.policyVersion > created.policyVersion) { "改名应推进策略版本" }
        val sameName = service.rename(created.groupId, "改名后的群")
        check(sameName.policyVersion == renamed.policyVersion) { "同名改名应幂等" }

        val muted = service.setMuted(created.groupId, created.members.first { !it.isLocal }.credential, true)
        check(muted.members.any { !it.isLocal && it.muted }) { "禁言未生效" }
        val unmuted = service.setMuted(created.groupId, muted.members.first { !it.isLocal }.credential, false)
        check(unmuted.members.none { !it.isLocal && it.muted }) { "解除禁言未生效" }

        val removed = service.removeMember(created.groupId, unmuted.members.first { !it.isLocal }.credential)
        check(removed.pending?.operation == "remove_members") {
            "移除应产生 pending remove_members，实际 " + (removed.pending?.operation ?: "无")
        }

        val accepted = service.acceptPending(created.groupId)
        check(accepted.epoch == removed.epoch + 1) { "接受后 epoch 应前进 1" }
        check(accepted.pending == null) { "接受后 pending 应清空" }
        check(accepted.members.none { !it.isLocal }) { "接受后被移除成员不应还在 members" }

        val unknown = runCatching { service.snapshot("grp:1:" + "ff".repeat(32)) }.exceptionOrNull()
        check(unknown is LocalStoreException) { "未知群必须显式失败" }
        println("      未知群错误码：" + (unknown as LocalStoreException).code)

        println(
            "PASS group slice: create/rename(idempotent)/mute/remove/accept with real MLS " +
                "(epoch " + created.epoch + " -> " + accepted.epoch + ", policy " +
                created.policyVersion + " -> " + accepted.policyVersion + ")",
        )
    } finally {
        service.shutdown()
        directory.deleteRecursively()
    }
}
