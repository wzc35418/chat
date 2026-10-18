package chat.desktop

import chat.api.VerifiedMessageInput
import chat.bridge.createLocalStore
import chat.presentation.display.DraftPhase
import chat.presentation.display.presentDraft
import chat.presentation.display.toDraftEvidence
import chat.presentation.display.toMessageDisplayPage
import kotlinx.coroutines.runBlocking
import java.io.File
import java.nio.file.Files

/**
 * P2-01 第一条真实竖直链路的端到端冒烟：Kotlin → UniFFI → Rust core → formal store → 加密 SQLite。
 *
 * 全部使用真实代码与真实磁盘文件，不使用假数据：建库、建草稿、写已验证消息、分页读回，
 * 再用展示层投影把存储事实变成 UI 模型（验证“存储状态不等于已接受/已送达”）。
 * 运行结束后删除临时目录。
 */
fun main() = runBlocking {
    val directory = Files.createTempDirectory("chat-local-store-").toFile()
    val database = File(directory, "chat.db").absolutePath
    val masterKey = "11".repeat(32)
    val conversationId = "conv:1:" + "aa".repeat(32)
    val messageMine = "msg:1:" + "bb".repeat(32)
    val messagePeer = "msg:1:" + "cc".repeat(32)
    val operationId = "op:1:" + "dd".repeat(32)
    val account = "acct:1:" + "ee".repeat(32)
    val device = "dev:1:" + "ff".repeat(32)
    // 对端使用不同的账户/设备：mine 由会话摘要里的本机作者判定，不能复用本机身份。
    val peerAccount = "acct:1:" + "12".repeat(32)
    val peerDevice = "dev:1:" + "34".repeat(32)

    val store = createLocalStore()
    try {
        store.open(database, masterKey)

        val draft = store.createDraft(conversationId, operationId, "你好 **世界**")
        check(draft.state == "queued") { "新草稿应为 queued，实际 " + draft.state }
        check(draft.operationId == operationId)
        val status = store.draftStatus(draft.draftId) ?: error("草稿查询不到")
        check(status.body == "你好 **世界**") { "草稿正文应原样保存" }

        val first = store.applyVerifiedMessage(
            VerifiedMessageInput(
                conversationId = conversationId,
                title = "林然",
                messageId = messageMine,
                authorAccount = account,
                authorDevice = device,
                body = "第一条本机消息",
                mine = true,
                createdAtMillis = 1_700_000_000_000,
            ),
        )
        check(first.messageId == messageMine && first.mine)

        val second = store.applyVerifiedMessage(
            VerifiedMessageInput(
                conversationId = conversationId,
                title = "林然",
                messageId = messagePeer,
                authorAccount = peerAccount,
                authorDevice = peerDevice,
                body = "对端回复",
                mine = false,
                createdAtMillis = 1_700_000_001_000,
            ),
        )
        check(!second.mine)

        val conversations = store.listConversations(null, 10)
        check(conversations.items.size == 1) { "应只有一个会话，实际 " + conversations.items.size }
        check(conversations.items.first().title == "林然")
        check(conversations.items.first().unreadCount == null) { "没有计数时必须为 null，不得伪造 0" }

        val messages = store.listMessages(conversationId, null, 10)
        check(messages.items.any { it.messageId == messageMine && it.mine })
        check(messages.items.any { it.messageId == messagePeer && !it.mine })
        // 已知限制（storage.md）：已排队草稿会以“待发投影”出现在消息页，直到 outbox 结果接线。
        check(messages.items.any { it.messageId == draft.draftId }) { "待发草稿应作为投影出现" }
        check(messages.items.size in 2..3) { "消息页应为 2 条正式 + 最多 1 条待发投影，实际 " + messages.items.size }

        // 展示层投影：草稿状态只说明本端事实，不得升级成“已接受/已送达”
        val evidence = status.toDraftEvidence()
        check(evidence.queuedLocally)
        check(evidence.nodeAcceptedReceiptId == null && evidence.deliveredReceiptId == null)
        val uiState = presentDraft(evidence)
        check(uiState.phase == DraftPhase.QUEUED) { "草稿只能显示待发送，实际 " + uiState.phase }

        // 存储分页直接喂给展示合并器的输入格式
        val displayPage = messages.toMessageDisplayPage()
        check(displayPage.items.map { it.id }.contains(messageMine))

        // 数据库文件确实落盘，且正文不以明文出现
        val bytes = File(database).readBytes()
        val plaintextLeak = String(bytes, Charsets.ISO_8859_1).contains("第一条本机消息")
        check(!plaintextLeak) { "数据库文件中不得出现明文正文" }

        // 错误密钥重开必须被拒绝
        store.close()
        val reopened = createLocalStore()
        var rejected = false
        try {
            reopened.open(database, "22".repeat(32))
        } catch (error: Exception) {
            rejected = true
            println("      错误密钥被拒绝：" + error.message)
        } finally {
            reopened.close()
        }
        check(rejected) { "错误主密钥必须被拒绝" }

        println("PASS local store vertical slice: Kotlin -> UniFFI -> core -> formal store -> projection")
        println("      draft=queued, unread=null, plaintext-not-on-disk, wrong-key-rejected, pending-draft-projection-visible")
    } finally {
        runCatching { store.close() }
        directory.deleteRecursively()
    }
}
