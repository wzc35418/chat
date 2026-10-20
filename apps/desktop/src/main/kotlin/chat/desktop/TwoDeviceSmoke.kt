package chat.desktop

import uniffi.chat_ffi.ChatCoreFfi
import uniffi.chat_ffi.IdentityFfi
import uniffi.chat_ffi.SharedInviteFfi
import uniffi.chat_ffi.SharedPullStateFfi
import java.io.File

/**
 * 双设备演示（打包产物内运行）：**两个独立的 ChatCore + 两条独立身份**，
 * 走真实的 建群 -> 登记节点 -> 提交邀请 -> 对方取回并入群 -> 发消息 -> 对方拉取。
 *
 * 进程编排（由 tools/demo-two-devices.ps1 负责）：
 *   bob-wait  <dir> <endpoint> <nodeId>   先跑：导出 .kp，然后等 a2b.txt
 *   alice     <dir> <endpoint> <nodeId>   后跑：用 bob.kp 建群/登记/写 a2b.txt/提交邀请/发消息
 *
 * 语义边界：所有步骤都是真实 FFI + 真实节点；失败以非 0 退出并打印稳定错误码，
 * 不做重试掩盖、不把"本地成功"当成"对方收到"。
 */
private const val MASTER_KEY = "0000000000000000000000000000000000000000000000000000000000000000"

private fun log(message: String) { println("[two-device] " + message) }

private fun fetchNodePublicKey(endpoint: String): String {
    val url = java.net.URI(endpoint.trimEnd('/') + "/v1/health").toURL()
    val connection = url.openConnection() as java.net.HttpURLConnection
    connection.requestMethod = "GET"
    connection.connectTimeout = 5000
    connection.readTimeout = 5000
    val body = connection.inputStream.bufferedReader().readText()
    return Regex("\"node_public_key\"\\s*:\\s*\"([0-9a-fA-F]{64})\"").find(body)?.groupValues?.get(1)
        ?: error("node public key not found in /v1/health")
}

private fun writeInviteHandoff(target: File, welcome: ByteArray, invite: SharedInviteFfi, head: SharedPullStateFfi) {
    val text = buildString {
        appendLine("welcome=" + welcome.joinToString("") { b -> String.format("%02x", b.toInt() and 0xff) })
        appendLine("groupId=" + invite.groupId)
        appendLine("mlsGroupId=" + invite.mlsGroupId)
        appendLine("name=" + invite.name)
        appendLine("inviterCredential=" + invite.inviterCredential)
        appendLine("ownerCredential=" + invite.ownerCredential)
        appendLine("inviteeCredential=" + invite.inviteeCredential)
        appendLine("members=" + invite.members.joinToString(","))
        appendLine("epoch=" + invite.epoch.toString())
        appendLine("cursorSeq=" + head.cursorSeq.toString())
        appendLine("cursorLogHash=" + head.cursorLogHash)
        appendLine("controlHead=" + head.controlHead)
        appendLine("policyVersion=" + head.policyVersion.toString())
        appendLine("mlsEpoch=" + head.mlsEpoch.toString())
    }
    target.writeText(text)
}

private fun readHandoff(source: File): List<Pair<String, String>> =
    source.readLines().filter { it.contains('=') }.map { line ->
        val at = line.indexOf('=')
        line.substring(0, at) to line.substring(at + 1)
    }

private fun Map<String, String>.required(name: String): String =
    this[name] ?: error("handoff is missing " + name)

private fun hexToBytes(text: String): ByteArray =
    ByteArray(text.length / 2) { index ->
        ((Character.digit(text[index * 2], 16) shl 4) or Character.digit(text[index * 2 + 1], 16)).toByte()
    }

private fun bobWait(dir: File, endpoint: String, nodeId: String, nodeKey: String): Int {
    File(dir, "bob.db").also { it.parentFile?.mkdirs() }
    val core = ChatCoreFfi.open(File(dir, "bob-chat.db").absolutePath, MASTER_KEY)
    return try {
        val account = core.enrollLocalIdentity(File(dir, "bob-identity.db").absolutePath, MASTER_KEY)
        log("bob enrolled: " + account.deviceId)
        core.registerDeviceOnNode(endpoint, nodeId, nodeKey)
        log("bob device registered on node (idempotent)")
        val kp = core.exportKeyPackage()
        File(dir, "bob.kp").writeBytes(kp)
        // MLS 里用的是**凭据**而不是设备 ID；把凭据一并交给 alice，否则她会用错凭据建不出群。
        // MLS 口径：KeyPackage 的 leaf credential 是**设备 ID**（dev:1:...），不是账户 ID。
        File(dir, "bob-credential.txt").writeText(core.keyPackageCredential())
        log("bob exported key package: " + kp.size + " bytes")
        val handoff = File(dir, "a2b.txt")
        var waited = 0
        while (!handoff.isFile && waited < 240) { Thread.sleep(250); waited++ }
        if (!handoff.isFile) { log("FAIL timeout waiting for a2b.txt"); return 3 }
        val fields = readHandoff(handoff).toMap()
        val invite = SharedInviteFfi(
            groupId = fields.required("groupId"),
            mlsGroupId = fields.required("mlsGroupId"),
            name = fields.required("name"),
            inviterCredential = fields.required("inviterCredential"),
            ownerCredential = fields.required("ownerCredential"),
            inviteeCredential = fields.required("inviteeCredential"),
            members = fields.required("members").split(",").filter { it.isNotBlank() },
            epoch = fields.required("epoch").toULong(),
            welcomeBytes = hexToBytes(fields.required("welcome")),
        )
        val head = SharedPullStateFfi(
            cursorSeq = fields.required("cursorSeq").toULong(),
            cursorLogHash = fields.required("cursorLogHash"),
            controlHead = fields.required("controlHead"),
            policyVersion = fields.required("policyVersion").toULong(),
            mlsEpoch = fields.required("mlsEpoch").toULong(),
        )
        val joined = core.joinGroupFromWelcome(fields.required("name"), invite, head)
        log("PASS bob joined: " + joined.groupId + " epoch=" + joined.epoch)
        var applied = 0uL
        for (round in 1..20) {
            val once = core.syncOnce(joined.groupId, endpoint, nodeId, nodeKey)
            applied = once.applied
            log("bob pull round " + round + ": fetched=" + once.fetched + " applied=" + once.applied)
            if (once.applied > 0uL) break
            Thread.sleep(500)
        }
        if (applied > 0uL) {
            log("PASS bob applied " + applied + " entries from the node")
            0
        } else {
            log("FAIL bob applied no entries")
            4
        }
    } catch (error: Throwable) {
        log("FAIL bob: " + error::class.java.simpleName + ": " + (error.message ?: ""))
        1
    } finally {
        runCatching { core.close() }
    }
}

private fun alice(dir: File, endpoint: String, nodeId: String, nodeKey: String): Int {
    File(dir, "alice.db").also { it.parentFile?.mkdirs() }
    val core = ChatCoreFfi.open(File(dir, "alice-chat.db").absolutePath, MASTER_KEY)
    return try {
        val account = core.enrollLocalIdentity(File(dir, "alice-identity.db").absolutePath, MASTER_KEY)
        log("alice enrolled: " + account.deviceId)
        val bobKey = File(dir, "bob.kp")
        var waited = 0
        while (!bobKey.isFile && waited < 240) { Thread.sleep(250); waited++ }
        if (!bobKey.isFile) { log("FAIL timeout waiting for bob.kp"); return 3 }
        val kp = bobKey.readBytes()
        val bobCredential = File(dir, "bob-credential.txt").takeIf { it.isFile }?.readText()?.trim()
        if (bobCredential.isNullOrBlank()) { log("FAIL bob credential missing"); return 3 }
        log("bob credential: " + bobCredential)
        val created = core.createGroupWithInviteeKeyPackage("two-device demo", bobCredential, kp, core.keyPackageCredential())
        log("PASS alice created group: " + created.group.groupId + " epoch=" + created.group.epoch)
        core.configureSendGroup(created.group.mlsGroupKey)
        core.configureSendNode(endpoint, nodeId, nodeKey)
        val registered = core.groupRegisterOnNode(created.group.groupId, endpoint, nodeId, nodeKey)
        log("PASS alice registered group on node: seq=" + registered.registeredSeq)
        val head = core.pullState(created.group.groupId)
            ?: run { log("FAIL no pull state after registration"); return 5 }
        writeInviteHandoff(File(dir, "a2b.txt"), created.invite.welcomeBytes, created.invite, head)
        log("handoff written for bob")
        // 节点成员表：先把 bob 的设备登记为群成员，否则他读不到日志（403/INVALID_CREDENTIAL）。
        val memberAdd = core.submitMemberAdd(created.group.groupId, bobCredential, endpoint, nodeId, nodeKey)
        log("PASS alice added bob as node member: seq=" + memberAdd.seq)
        val receipt = core.submitInvite(created.group.groupId, created.invite, endpoint, nodeId, nodeKey)
        log("PASS alice submitted invite: seq=" + receipt.seq)
        // 发送配置可能被后续的登记/邀请调用重置，发消息前再确认一次（幂等）。
        core.configureSendNode(endpoint, nodeId, nodeKey)
        core.configureSendGroup(created.group.mlsGroupKey)
        val draft = core.createDraft("conv:1:" + "11".repeat(32), "op:1:" + "22".repeat(32), "hello from alice")
        core.sendPrepare(draft.draftId)
        val dispatched = core.sendDispatch(draft.draftId)
        log("alice send dispatch: " + dispatched.status)
        if (dispatched.status == "accepted") {
            log("PASS alice message accepted by node")
            0
        } else {
            log("FAIL alice dispatch status=" + dispatched.status)
            6
        }
    } catch (error: Throwable) {
        log("FAIL alice: " + error::class.java.simpleName + ": " + (error.message ?: ""))
        1
    } finally {
        runCatching { core.close() }
    }
}

private fun createIdentities(dir: File): Int = try {
    listOf("alice-identity.db", "bob-identity.db").forEach { name ->
        val identity = IdentityFfi.openPersisted(File(dir, name).absolutePath, MASTER_KEY, "node:1:" + "0a".repeat(32))
        log("identity ready: " + name + " device=" + identity.account().deviceId)
        runCatching { identity.close() }
    }
    0
} catch (error: Throwable) {
    log("FAIL identities: " + (error.message ?: ""))
    1
}

fun main(args: Array<String>): Unit {
    if (args.size < 2) {
        println("usage: TwoDeviceSmokeKt <bob-wait|alice> <dir> <endpoint> <nodeId>")
        kotlin.system.exitProcess(2)
    }
    val mode = args[0]
    val dir = File(args[1]).also { it.mkdirs() }
    val endpoint = if (args.size >= 4) args[2] else "http://127.0.0.1:8099"
    val nodeId = if (args.size >= 4) args[3] else "node-e2e"
    val nodeKey = if (mode == "identities") "" else fetchNodePublicKey(endpoint)
    val code = when (mode) {
        "identities" -> createIdentities(dir)
        "bob-wait" -> bobWait(dir, endpoint, nodeId, nodeKey)
        "alice" -> alice(dir, endpoint, nodeId, nodeKey)
        else -> { println("unknown mode: " + mode); 2 }
    }
    kotlin.system.exitProcess(code)
}

