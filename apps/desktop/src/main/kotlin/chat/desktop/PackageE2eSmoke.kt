package chat.desktop

import chat.bridge.createGroupService
import chat.bridge.createSyncService
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
/**
 * 在 https 端点上取节点公钥：**按指纹固定证书**（只接受 SHA-256 匹配的那张）。
 *
 * 语义：这里不能用 TrustManager 全信任——那会让"公钥自动获取"变成绕过 pinning 的后门。
 */
private fun fetchNodePublicKeyPinned(endpoint: String, fingerprints: List<String>): String? = runCatching {
    val expected = fingerprints.map { it.trim().lowercase() }.filter { it.isNotEmpty() }.toSet()
    val trust = object : javax.net.ssl.X509TrustManager {
        override fun checkClientTrusted(chain: Array<java.security.cert.X509Certificate>, authType: String) =
            throw java.security.cert.CertificateException("client auth not supported")

        override fun checkServerTrusted(chain: Array<java.security.cert.X509Certificate>, authType: String) {
            val der = chain.first().encoded
            val actual = java.security.MessageDigest.getInstance("SHA-256").digest(der)
                .joinToString("") { b -> String.format("%02x", b.toInt() and 0xff) }
            if (actual !in expected) {
                throw java.security.cert.CertificateException("PIN_MISMATCH: $actual")
            }
        }

        override fun getAcceptedIssuers(): Array<java.security.cert.X509Certificate> = emptyArray()
    }
    val context = javax.net.ssl.SSLContext.getInstance("TLS")
    context.init(null, arrayOf<javax.net.ssl.TrustManager>(trust), java.security.SecureRandom())
    val url = java.net.URI(endpoint.trimEnd('/') + "/v1/health").toURL()
    val connection = url.openConnection() as javax.net.ssl.HttpsURLConnection
    connection.sslSocketFactory = context.socketFactory
    connection.requestMethod = "GET"
    connection.connectTimeout = 5000
    connection.readTimeout = 5000
    val body = connection.inputStream.bufferedReader().readText()
    Regex("\"node_public_key\"\\s*:\\s*\"([0-9a-fA-F]{64})\"").find(body)?.groupValues?.get(1)
}.getOrNull()

private fun fetchNodePublicKey(endpoint: String): String? = runCatching {
    val url = java.net.URI(endpoint.trimEnd('/') + "/v1/health").toURL()
    val connection = url.openConnection() as java.net.HttpURLConnection
    connection.requestMethod = "GET"
    connection.connectTimeout = 5000
    connection.readTimeout = 5000
    val body = connection.inputStream.bufferedReader().readText()
    Regex("\"node_public_key\"\\s*:\\s*\"([0-9a-fA-F]{64})\"").find(body)?.groupValues?.get(1)
}.getOrNull()

fun main(args: Array<String>): Unit = runBlocking {
    if (args.size < 2) {
        println("usage: PackageE2eSmokeKt <endpointUrl> <nodeId> [nodePublicKeyHex] [tlsFingerprintHex]")
        kotlin.system.exitProcess(2)
    }
    val endpoint = args[0]
    val nodeId = args[1]
    // 可选：进程级传输信任（https 节点必须给；给了指纹再连明文端点会被内核拒绝）。
    // 位置：0=endpoint 1=nodeId 2=nodePublicKey(可空) 3=tlsFingerprint
    // 位置：0=endpoint 1=nodeId 2=nodePublicKey("-"=自动) 3=tlsFingerprint("-"=无)
    val tlsFingerprint = if (args.size >= 4) args[3].trim().takeIf { it.isNotEmpty() && it != "-" } else null
    if (tlsFingerprint != null) {
        // 证书轮换期间要同时信任新旧两张：这里支持逗号分隔的指纹列表（内核按**集合**匹配）。
        val pins = tlsFingerprint.split(",").map { it.trim() }.filter { it.isNotEmpty() }
        uniffi.chat_ffi.configureTransportTrust(pins)
        println("INFO transport trust configured: pinned " + pins.size + " certificate(s)")
    }
    // 节点公钥从 /v1/health 的公开身份取（不靠人工传参，避免 pinning 被跳过）。
    val nodeKey = if (args.size >= 3 && args[2].trim().isNotEmpty() && args[2].trim() != "-") {
        args[2].trim()
    } else if (tlsFingerprint != null) {
        // https：按指纹取公钥（不是"信任一切"）。
        fetchNodePublicKeyPinned(endpoint, tlsFingerprint.split(",").map { it.trim() })
    } else {
        fetchNodePublicKey(endpoint)
    }
    println("INFO node public key: " + (nodeKey ?: "(unavailable)"))
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

        // 发送必须要有本机已加入的 MLS 群（sendPrepare 会检查），而入库是单写者：
        // 所以按"建群 -> 关闭句柄 -> 打开消息库"的顺序做，模拟真实的一次性初始化。
        val groups = createGroupService(dbPath, "0".repeat(64), null)
        val snapshot = groups.createGroup("pkg-e2e-group")
        println("PASS group created: " + snapshot.groupId + " epoch=" + snapshot.epoch)
        val groupKey = snapshot.mlsGroupKey
        // 关键一步：本地建群必须在节点登记，否则节点会 rejected（R-182 的缺口）。
        // CHAT_DOUBLE_TRY=1 时跳过节点登记：这个模式只测“链路已死”时的失败语义，
        // 登记本身要走网络，节点停着的话会在 setup 阶段就失败，测不到发送路径。
        val doubleTryMode = System.getenv("CHAT_DOUBLE_TRY") == "1"
        val syncOnlyMode = System.getenv("CHAT_SYNC_ONLY") == "1"
        if (doubleTryMode || syncOnlyMode) {
            println("INFO local-only mode: skipping node registration (setup stays local)")
        }
        if (!doubleTryMode && !syncOnlyMode) {
        val registered = groups.registerOnNode(snapshot.groupId, endpoint, nodeId, nodeKey)
        println("PASS group registered on node: " + registered.groupId + " seq=" + registered.registeredSeq)
        }
        groups.shutdown()

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

        // 库里有多个群时必须显式选择发送群（内层会 INVALID_TARGET 拒绝猜测）。
        store.configureSendGroup(groupKey)
        println("PASS send group configured")
        store.configureSendNode(endpoint, nodeId, nodeKey)
        println("PASS send node configured: " + endpoint)

        // CHAT_SYNC_ONLY=1：只测“拉取路径”在死链路下的失败语义（不发送）。
        if (System.getenv("CHAT_SYNC_ONLY") == "1") {
            runCatching { store.close() }
            val dummyKey = "00".repeat(32)
            val sync = createSyncService(dbPath, "0".repeat(64), endpoint, nodeId, dummyKey, snapshot.groupId)
            // 第一次拉取（链路正常）用来**初始化游标**；随后外部会把代理切成黑障。
            val first = runCatching { sync.syncOnce() }
            val gapMs = System.getenv("CHAT_SYNC_GAP_MS")?.toLongOrNull() ?: 3000L
            Thread.sleep(gapMs)
            val second = runCatching { sync.syncOnce() }
            runCatching { sync.shutdown() }
            println("SYNC_ONLY first_ok=" + first.isSuccess + " second_ok=" + second.isSuccess)
            println("SYNC_ONLY second_reason=" +
                (second.exceptionOrNull()?.let { failureCode(it) + " [" + (it.message ?: "") + "]" } ?: "none"))
            if (first.isFailure) {
                println("FAIL sync-only: the first pull (healthy link) already failed")
                kotlin.system.exitProcess(1)
            }
            if (second.isSuccess) {
                val applied = second.getOrNull()?.applied ?: 0L
                println("FAIL sync-only: the pull CLAIMED success on a dead link (applied=" + applied + ")")
                kotlin.system.exitProcess(1)
            }
            println("PASS sync-only: pull failed honestly after the link died")
            kotlin.system.exitProcess(0)
        }

        // CHAT_SENDFLOW=1 时走界面用的「一步发送 + 自动恢复」路径（sendWithRecovery），
        // 这样这条冒烟证明的就是**用户点按钮时实际执行的那段代码**。
        val useSendFlow = System.getenv("CHAT_SENDFLOW") == "1"
        var dispatched: chat.api.SendStep? = null
        if (useSendFlow) {
            val outcome = sendWithRecovery(store, draft.draftId)
            println("INFO send flow steps: " + outcome.steps.joinToString(" -> "))
            if (!outcome.ok) {
                println("FAIL send flow: " + (outcome.failure?.message ?: "unknown"))
                kotlin.system.exitProcess(1)
            }
            println("PASS send flow accepted after " + outcome.steps.size + " step(s)")
            dispatched = chat.api.SendStep(draft.draftId, "accepted", null, null, "send flow", true)
        } else {
        val prepared = store.sendPrepare(draft.draftId)
        println("PASS sendPrepare: " + prepared.status)

        dispatched = store.sendDispatch(draft.draftId)
        println("PASS sendDispatch: " + dispatched.status)
        }

        // CHAT_DOUBLE_TRY=1：同一进程内“先失败一次、再试一次”，用来观察失败窗口内的自适应预检。
        // 只能在节点不可达的场景下跑（见 tools/soak-faults.ps1 的 phase D）。
        if (System.getenv("CHAT_DOUBLE_TRY") == "1") {
            val firstStarted = System.nanoTime()
            val firstHex = (java.util.UUID.randomUUID().toString().replace("-", "") +
                java.util.UUID.randomUUID().toString().replace("-", "")).substring(0, 64)
            val firstDraft = store.createDraft("conv:1:" + firstHex, "op:1:" + firstHex, "double-try first")
            val first = sendWithRecovery(store, firstDraft.draftId)
            val firstMs = (System.nanoTime() - firstStarted) / 1_000_000
            val secondStarted = System.nanoTime()
            val secondHex = (java.util.UUID.randomUUID().toString().replace("-", "") +
                java.util.UUID.randomUUID().toString().replace("-", "")).substring(0, 64)
            val secondDraft = store.createDraft("conv:1:" + secondHex, "op:1:" + secondHex, "double-try second")
            val second = sendWithRecovery(store, secondDraft.draftId)
            val secondMs = (System.nanoTime() - secondStarted) / 1_000_000
            println("DOUBLE_TRY first=" + firstMs + "ms second=" + secondMs + "ms")
            println("DOUBLE_TRY first_ok=" + first.ok + " second_ok=" + second.ok)
            println("DOUBLE_TRY second_reason=" + (second.failure?.message ?: "none"))
            if (first.ok || second.ok) {
                println("FAIL double-try: an attempt claimed success while the node was down")
                failures += 1
            } else if (secondMs >= firstMs * 7 / 10) {
                println("FAIL double-try: second attempt was not faster (" + secondMs + "ms vs " + firstMs + "ms)")
                failures += 1
            } else {
                println("PASS double-try: second attempt faster (" + secondMs + "ms vs " + firstMs + "ms)")
            }
        }

        // CHAT_REPEAT=N：同一身份/群**稳态**连发 N 条（用于 soak 的 -Reuse 模式）。
        val repeatCount = System.getenv("CHAT_REPEAT")?.toIntOrNull() ?: 0
        if (repeatCount > 0 && dispatched != null) {
            var sentReuse = 0
            for (index in 1..repeatCount) {
                val started = System.nanoTime()
                val reuseHex = (java.util.UUID.randomUUID().toString().replace("-", "") +
                    java.util.UUID.randomUUID().toString().replace("-", "")).substring(0, 64)
                val reuse = store.createDraft("conv:1:" + reuseHex, "op:1:" + reuseHex, "reuse hello " + index)
                val outcome = sendWithRecovery(store, reuse.draftId)
                val ms = (System.nanoTime() - started) / 1_000_000
                if (!outcome.ok) {
                    println("FAIL repeat " + index + " failed: " + (outcome.failure?.message ?: "unknown"))
                    failures += 1
                    break
                }
                sentReuse += 1
                println("REPEAT " + index + " ok ms=" + ms + " steps=" + outcome.steps.joinToString("->"))
            }
            println("PASS repeat flow: " + sentReuse + "/" + repeatCount + " sent in one process")
        }

        val draftStatus = store.draftStatusView(draft.draftId)
        println("INFO draft status after dispatch: " + (draftStatus?.status ?: "unknown"))
        if (dispatched != null && dispatched.status == "accepted" && dispatched.settled) {
            println("PASS node accepted the packaged submit")

            // 收消息：关掉 store（单写者）后用同一份库开同步对象，拉一轮。
            runCatching { store.close() }
            if (nodeKey == null) {
                println("FAIL node public key unavailable (cannot sync without it)")
                failures += 1
                kotlin.system.exitProcess(1)
            }
            val sync = createSyncService(dbPath, "0".repeat(64), endpoint, nodeId, nodeKey, snapshot.groupId)
            val once = sync.syncOnce()
            println(
                "PASS syncOnce: fetched=" + once.fetched + " applied=" + once.applied +
                    " skipped=" + once.skipped + " cursor=" + once.nextCursor + " settled=" + once.settled,
            )
            if (once.fetched < 1) {
                println("FAIL node returned no entries for the pull (expected at least the accepted one)")
                failures += 1
            } else {
                println("PASS packaged artifact pulled entries from the node")
            }
            runCatching { sync.shutdown() }
        } else {
            println("FAIL node did not accept (status=" + (dispatched?.status ?: "none") + ")")
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
