package chat.desktop

import chat.bridge.createIdentityService
import chat.bridge.createLocalStore
import chat.api.LocalStoreException
import kotlinx.coroutines.runBlocking

/**
 * 交付物级端到端冒烟：**打包出来的 jar** 里跑真实「建草稿 -> 提交到真节点 -> 拿回执」。
 *
 * 用法（由 tools/verify-package-e2e.ps1 调用，节点需已启动）：
 *   java -Djna.library.path=<app>/native -cp app.jar chat.desktop.PackageE2eSmokeKt \
 *        <endpointUrl> <nodeId> [nodePublicKeyHex]
 *
 * 语义边界：只用真实 FFI + 真实 HTTP 节点；任何一步失败都以非 0 退出并打印**明确的稳定错误码**，
 * 不做重试掩盖、不把"本地准备好了"当成"节点收了"。
 */
fun main(args: Array<String>): Unit = runBlocking {
    if (args.size < 2) {
        println("usage: PackageE2eSmokeKt <endpointUrl> <nodeId> [nodePublicKeyHex]")
        kotlin.system.exitProcess(2)
    }
    val endpoint = args[0]
    val nodeId = args[1]
    val nodeKey = if (args.size >= 3) args[2] else null
    val tempDir = java.io.File(System.getProperty("java.io.tmpdir"), "chat-pkg-e2e-" + System.nanoTime())
    tempDir.mkdirs()
    val dbPath = java.io.File(tempDir, "pkg-e2e.sqlite").absolutePath
    val identity = createIdentityService()
    val store = createLocalStore()
    var failures = 0
    try {
        // 顺序很重要：消息库同一时刻只允许一个写者，必须先登记身份再 open store。
        val identityDir = java.io.File(tempDir, "identity")
        identityDir.mkdirs()
        val identityPath = java.io.File(identityDir, "identity.sqlite").absolutePath
        val account = identity.openPersisted(identityPath, "1".repeat(64), "node:1:" + "0a".repeat(32))
        println("PASS identity opened: " + account.deviceId)
        val enrolled = identity.enrollIntoStore(dbPath, "0".repeat(64))
        println("PASS identity enrolled into message store: " + enrolled.accountId)

        store.open(dbPath, "0".repeat(64))
        println("PASS store opened: " + dbPath)

        // PROTOCOL-001：id 必须是 prefix:version:64-lowercase-hex，不能用随意字符串。
        val hex = java.util.UUID.randomUUID().toString().replace("-", "") +
            java.util.UUID.randomUUID().toString().replace("-", "").substring(0, 0)
        val idHex = (hex + hex).substring(0, 64)
        val conversationId = "conv:1:" + idHex
        val operationId = "op:1:" + idHex
        val draft = store.createDraft(conversationId, operationId, "packaged e2e hello")
        println("PASS draft created: " + draft.draftId + " state=" + draft.state)

        store.configureSendNode(endpoint, nodeId, nodeKey)
        println("PASS send node configured: " + endpoint)

        val prepared = store.sendPrepare(draft.draftId)
        println("PASS sendPrepare: " + prepared.status)

        val dispatched = store.sendDispatch(draft.draftId)
        println("PASS sendDispatch: " + dispatched.status)

        val draftStatus = store.draftStatusView(draft.draftId)
        println("INFO draft status after dispatch: " + (draftStatus?.status ?: "unknown"))
        if (dispatched.status == "accepted" && dispatched.settled) {
            println("PASS node accepted the packaged submit")
        } else {
            println("FAIL node did not accept (status=" + dispatched.status + ", settled=" + dispatched.settled + ", detail=" + dispatched.detail + ")")
            failures += 1
        }
    } catch (error: LocalStoreException) {
        println("FAIL " + error.code + ": " + (error.message ?: ""))
        failures += 1
    } catch (error: Throwable) {
        println("FAIL " + (error::class.java.simpleName) + ": " + (error.message ?: ""))
        failures += 1
    } finally {
        runCatching { store.close() }
        runCatching { identity.close() }
        tempDir.deleteRecursively()
    }
    if (failures == 0) {
        println("PASS package e2e slice")
        kotlin.system.exitProcess(0)
    } else {
        println("FAIL package e2e slice")
        kotlin.system.exitProcess(1)
    }
}
