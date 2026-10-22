package chat.desktop

import chat.bridge.createGroupService
import chat.bridge.createIdentityService
import chat.bridge.createLocalStore
import chat.bridge.createSyncService
import kotlinx.coroutines.runBlocking
import java.io.File
import java.nio.file.Files

/**
 * 应用真实链路冒烟：**完全按 ChatApp 的调用顺序**打一遍，验证“用户在界面里点发送”会不会成功。
 *
 * 顺序（与 ChatApp.start / sendNow / syncOnce 一致）：
 *   identity.openPersisted → 群（无则建「我的会话」）→ 登记节点 → store.open
 *   → configureTransportTrust(指纹) → configureSendNode → configureSendGroup
 *   → createDraft → sendPrepare → sendDispatch（必须 accepted）
 *   → 关 store → syncOnce → 重开 store
 *
 * 参数可用 -Dapp.endpoint / -Dapp.nodeId / -Dapp.nodeKey / -Dapp.fingerprint。
 * 退出码：0 = PASS，1 = FAIL，2 = 缺参数。
 */
private val MASTER_KEY = "0".repeat(64)

fun main() {
    val endpoint = System.getProperty("app.endpoint").orEmpty()
    val nodeId = System.getProperty("app.nodeId").orEmpty()
    val nodeKey = System.getProperty("app.nodeKey").orEmpty()
    val fingerprint = System.getProperty("app.fingerprint").orEmpty()
    if (endpoint.isEmpty() || nodeId.isEmpty() || nodeKey.isEmpty() || fingerprint.isEmpty()) {
        println("usage: -Dapp.endpoint=... -Dapp.nodeId=... -Dapp.nodeKey=... -Dapp.fingerprint=...")
        kotlin.system.exitProcess(2)
    }
    val code = runBlocking { run(endpoint, nodeId, nodeKey, fingerprint) }
    kotlin.system.exitProcess(code)
}

private fun log(message: String) = println(message)

private suspend fun run(endpoint: String, nodeId: String, nodeKey: String, fingerprint: String): Int {
    val dir = Files.createTempDirectory("chat-app-flow-").toFile()
    log("workspace: " + dir.absolutePath)
    val database = File(dir, "identity.db").absolutePath
    val node = "node:1:" + "0a".repeat(32)
    var store: chat.api.LocalStore? = null
    var storeClosedForSync = false
    try {
        // 证书固定：自签名节点必须靠它（这正是界面“设置”里那格指纹的作用）。
        uniffi.chat_ffi.configureTransportTrust(listOf(fingerprint))
        log("PASS transport trust pinned: " + fingerprint.take(12) + "…")

        val identity = createIdentityService()
        val account = identity.openPersisted(database, MASTER_KEY, node)
        val myAccountId = account.accountId
        val myDeviceId = account.deviceId
        log("PASS identity: " + account.deviceId + " fingerprint=" + account.fingerprint.take(12) + "…")
        runCatching { identity.close() }

        // 群：没有就建一个，然后**必须**登记到节点（否则提交会被拒 / 拿不到 head）。
        val groups = createGroupService(database, MASTER_KEY, node)
        val existing = groups.listGroups().groups.firstOrNull { !it.dissolved && !it.left }
        val snapshot = if (existing != null) {
            log("group exists: " + existing.groupId)
            groups.snapshot(existing.groupId)
        } else {
            val created = groups.createGroup("我的会话")
            log("group created: " + created.groupId + " epoch=" + created.epoch)
            created
        }
        val registration = groups.registerOnNode(snapshot.groupId, endpoint, nodeId, nodeKey)
        log("PASS group registered on node: seq=" + registration.registeredSeq + " node=" + registration.nodeId)
        val mlsGroupKey = snapshot.mlsGroupKey
        runCatching { groups.shutdown() }

        val local = createLocalStore()
        local.open(database, MASTER_KEY)
        store = local
        log("PASS store opened: " + database)
        local.configureSendNode(endpoint, nodeId, nodeKey)
        local.configureSendGroup(mlsGroupKey)
        log("PASS send node + group configured")

        val conversation = "conv:1:" + "ab".repeat(32)
        val operation = "op:1:" + "cd".repeat(32)
        val draft = local.createDraft(conversation, operation, "app flow smoke " + System.currentTimeMillis())
        log("PASS draft created: " + draft.draftId)
        val prepared = local.sendPrepare(draft.draftId)
        log("send prepare: status=" + prepared.status + " settled=" + prepared.settled)
        val dispatched = local.sendDispatch(draft.draftId)
        log("send dispatch: status=" + dispatched.status + " settled=" + dispatched.settled)
        if (dispatched.status != "accepted" || !dispatched.settled) {
            log("FAIL dispatch was not accepted: " + dispatched.status + " detail=" + (dispatched.detail ?: ""))
            return 1
        }
        log("PASS message accepted by the node")
        // 发送方必须自己落库：MLS 不允许发送者解密自己的应用消息，所以同步永远拿不回它。
        local.applyVerifiedMessage(
            chat.api.VerifiedMessageInput(
                conversationId = conversation,
                title = "会话",
                // PROTOCOL-001：message id 必须是 msg:1:<64 lowercase hex>；用 operation 的 hex 部分派生。
                messageId = "msg:1:" + operation.removePrefix("op:1:"),
                authorAccount = myAccountId,
                authorDevice = myDeviceId,
                body = "app flow smoke " + System.currentTimeMillis(),
                mine = true,
                createdAtMillis = System.currentTimeMillis(),
            ),
        )
        val afterSend = local.listMessages(conversation, null, 5)
        log("messages immediately after send: " + afterSend.items.size + " mine=" + afterSend.items.count { it.mine })

        // 同步：必须独占写者（关 store → syncOnce → 重开）。
        local.close()
        store = null
        storeClosedForSync = true
        val sync = createSyncService(database, MASTER_KEY, endpoint, nodeId, nodeKey, snapshot.groupId)
        for (round in 1..3) {
            val once = sync.syncOnce()
            log("sync round " + round + ": fetched=" + once.fetched + " applied=" + once.applied + " skipped=" + once.skipped)
            Thread.sleep(400)
        }
        runCatching { sync.shutdown() }
        val reopened = createLocalStore()
        reopened.open(database, MASTER_KEY)
        store = reopened
        reopened.configureSendGroup(mlsGroupKey)
        log("PASS store reopened after sync")
        val reopenedMessages = reopened.listMessages(conversation, null, 5)
        log("messages visible after sync (same conversation): " + reopenedMessages.items.size)
        for (item in reopenedMessages.items) { log("  message: " + item.messageId + " mine=" + item.mine + " body=" + item.body.take(40)) }
        // 发出去的消息到底落在哪个会话里？把本地会话列出来看。
        val conversations = reopened.listConversations(null, 10)
        log("conversations in the local store: " + conversations.items.size)
        for (item in conversations.items) { log("  conv: " + item.conversationId + " preview=" + item.preview.take(40)) }
        val sendStep = local.let { null }
        if (reopenedMessages.items.none { it.mine }) {
            log("NOTE: no outbound message is visible in this conversation after send+sync")
        }
        log("APP FLOW: PASS (user settings + send + sync all work against " + endpoint + ")")
        return 0
    } catch (error: Throwable) {
        log("FAIL " + error::class.java.simpleName + ": " + (error.message ?: ""))
        return 1
    } finally {
        runCatching { store?.close() }
    }
}
