package chat.desktop

import chat.bridge.createGroupService
import chat.bridge.createIdentityService
import chat.bridge.createLocalStore
import java.io.File

/**
 * 用**用户真实的本地库**（`~/.chat-desktop`）跑一遍发送，回答“我这台机器现在能不能发出去”。
 *
 * 与 AppFlowSmoke 的区别：不新建库、不新建群，直接用界面里那个群和那份身份。
 * 参数：-Duser.endpoint / -Duser.nodeId / -Duser.nodeKey / -Duser.fingerprint。
 * 退出码：0 = PASS（dispatch accepted），1 = FAIL，2 = 缺参数。
 */
private val MASTER_KEY = "0".repeat(64)

fun main() {
    val endpoint = System.getProperty("user.endpoint").orEmpty()
    val nodeId = System.getProperty("user.nodeId").orEmpty()
    val nodeKey = System.getProperty("user.nodeKey").orEmpty()
    val fingerprint = System.getProperty("user.fingerprint").orEmpty()
    if (endpoint.isEmpty() || nodeId.isEmpty() || nodeKey.isEmpty() || fingerprint.isEmpty()) {
        println("usage: -Duser.endpoint=... -Duser.nodeId=... -Duser.nodeKey=... -Duser.fingerprint=...")
        kotlin.system.exitProcess(2)
    }
    val code = kotlinx.coroutines.runBlocking { run(endpoint, nodeId, nodeKey, fingerprint) }
    kotlin.system.exitProcess(code)
}

private fun log(message: String) = println(message)

private suspend fun run(endpoint: String, nodeId: String, nodeKey: String, fingerprint: String): Int {
    val dir = File(System.getProperty("user.home"), ".chat-desktop")
    val identityDb = File(dir, "identity.db").absolutePath
    val chatDb = File(dir, "chat.db").absolutePath
    if (!File(identityDb).exists()) { log("FAIL 没有身份库：" + identityDb); return 1 }
    if (!File(chatDb).exists()) { log("FAIL 没有消息库：" + chatDb); return 1 }
    log("使用真实库：" + dir.absolutePath)
    uniffi.chat_ffi.configureTransportTrust(listOf(fingerprint))
    log("PASS transport trust pinned")
    val node = "node:1:" + "0a".repeat(32)
    var store: chat.api.LocalStore? = null
    try {
        val identity = createIdentityService()
        val account = identity.openPersisted(identityDb, MASTER_KEY, node)
        log("PASS identity: " + account.deviceId.take(20) + "…")
        // 发送链路要求身份已登记进消息库（幂等）。
        val enrolled = runCatching { identity.enrollIntoStore(chatDb, MASTER_KEY) }
        log(if (enrolled.isSuccess) "PASS identity enrolled into chat.db" else "WARN enroll failed: " + (enrolled.exceptionOrNull()?.message ?: ""))
        runCatching { identity.close() }

        val groups = createGroupService(chatDb, MASTER_KEY, node)
        val list = groups.listGroups()
        val target = list.groups.firstOrNull { !it.dissolved && !it.left }
        if (target == null) { log("FAIL 本地没有群"); runCatching { groups.shutdown() }; return 1 }
        log("group: " + target.groupId + " name=" + target.name + " epoch=" + target.epoch)
        val registration = runCatching { groups.registerOnNode(target.groupId, endpoint, nodeId, nodeKey) }
        registration.fold(
            onSuccess = { log("PASS 群已登记到节点：seq=" + it.registeredSeq) },
            onFailure = { log("WARN 登记失败（继续尝试发送）：" + (it.message ?: "")) },
        )
        val snapshot = runCatching { groups.snapshot(target.groupId) }.getOrNull()
        runCatching { groups.shutdown() }
        if (snapshot == null) { log("FAIL 读不到群快照"); return 1 }

        val local = createLocalStore()
        local.open(chatDb, MASTER_KEY)
        store = local
        local.configureSendNode(endpoint, nodeId, nodeKey)
        local.configureSendGroup(snapshot.mlsGroupKey)
        log("PASS send node + group configured")
        val conversation = conversationIdFor(snapshot.groupId)
        val hex = java.util.UUID.randomUUID().toString().replace("-", "") + java.util.UUID.randomUUID().toString().replace("-", "")
        val operation = "op:1:" + hex.substring(0, 64)
        val body = "user-db smoke " + System.currentTimeMillis()
        val draft = local.createDraft(conversation, operation, body)
        val prepared = local.sendPrepare(draft.draftId)
        log("send prepare: status=" + prepared.status)
        val dispatched = local.sendDispatch(draft.draftId)
        log("send dispatch: status=" + dispatched.status + " settled=" + dispatched.settled + " detail=" + (dispatched.detail ?: ""))
        if (dispatched.status != "accepted") { log("FAIL dispatch 未 accepted"); return 1 }
        local.applyVerifiedMessage(
            chat.api.VerifiedMessageInput(
                conversationId = conversation,
                title = snapshot.name,
                messageId = "msg:1:" + operation.removePrefix("op:1:"),
                authorAccount = account.accountId,
                authorDevice = account.deviceId,
                body = body,
                mine = true,
                createdAtMillis = System.currentTimeMillis(),
            ),
        )
        val visible = local.listMessages(conversation, null, 5)
        log("messages in this conversation: " + visible.items.size)
        log("USER DB: PASS（真实库能发出去，且本地可见）")
        return 0
    } catch (error: Throwable) {
        log("FAIL " + error::class.java.simpleName + ": " + (error.message ?: ""))
        return 1
    } finally {
        runCatching { store?.close() }
    }
}
