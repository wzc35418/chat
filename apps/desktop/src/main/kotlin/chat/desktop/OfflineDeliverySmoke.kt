package chat.desktop

import uniffi.chat_ffi.ChatCoreFfi
import uniffi.chat_ffi.IdentityFfi
import uniffi.chat_ffi.SharedInviteFfi
import uniffi.chat_ffi.SharedPullStateFfi
import java.io.File

/**
 * **错时投递**冒烟：证明"发送方写完时接收方不在线，接收方后来上线仍然收到"。
 *
 * 三个模式由 `tools/verify-offline-delivery.ps1` **顺序启动三个独立进程**（不并发）：
 *   bob-prepare  <dir> <endpoint> <nodeId>   导出 .kp + 设备凭据，然后**进程退出**（此后不在线）
 *   alice-send   <dir> <endpoint> <nodeId>   建群/登记/加成员/提交邀请/发消息（此时 bob 进程不存在）
 *   bob-receive  <dir> <endpoint> <nodeId>   新进程重新打开 bob 的库，凭 handoff 入群并拉取
 *
 * 语义边界：全程真实 FFI + 真实节点；bob 的接收进程与 alice 的发送进程**不重叠**，
 * 因此"送达"只能来自节点侧的存储转发，而不是内存里传过去。
 */
private const val MASTER_KEY = "0000000000000000000000000000000000000000000000000000000000000000"

private fun log(message: String) { println("[offline] " + message) }

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

private fun writeHandoff(target: File, welcome: ByteArray, invite: SharedInviteFfi, head: SharedPullStateFfi) {
    target.writeText(
        buildString {
            appendLine("welcome=" + welcome.joinToString("") { b -> String.format("%02x", b.toInt() and 0xff) })
            appendLine("groupId=" + invite.groupId)
            appendLine("mlsGroupId=" + invite.mlsGroupId)
            appendLine("name=" + invite.name)
            appendLine("inviterCredential=" + invite.inviterCredential)
            appendLine("ownerCredential=" + invite.ownerCredential)
            appendLine("inviteeCredential=" + invite.inviteeCredential)
            appendLine("members=" + invite.members.joinToString(","))
            appendLine("epoch=" + invite.epoch)
            appendLine("cursorSeq=" + head.cursorSeq)
            appendLine("cursorLogHash=" + head.cursorLogHash)
            appendLine("controlHead=" + head.controlHead)
            appendLine("policyVersion=" + head.policyVersion)
            appendLine("mlsEpoch=" + head.mlsEpoch)
        },
    )
}

private fun readFields(source: File): Map<String, String> =
    source.readLines().filter { it.contains('=') }.associate { line ->
        val at = line.indexOf('=')
        line.substring(0, at) to line.substring(at + 1)
    }

private fun Map<String, String>.required(name: String): String = this[name] ?: error("handoff missing " + name)

private fun hexToBytes(text: String): ByteArray =
    ByteArray(text.length / 2) { index ->
        ((Character.digit(text[index * 2], 16) shl 4) or Character.digit(text[index * 2 + 1], 16)).toByte()
    }

private fun bobPrepare(dir: File, endpoint: String, nodeId: String, nodeKey: String): Int {
    val core = ChatCoreFfi.open(File(dir, "bob-chat.db").absolutePath, MASTER_KEY)
    return try {
        val account = core.enrollLocalIdentity(File(dir, "bob-identity.db").absolutePath, MASTER_KEY)
        log("bob enrolled: " + account.deviceId)
        File(dir, "bob-device.txt").writeText(account.deviceId)
        core.registerDeviceOnNode(endpoint, nodeId, nodeKey)
        log("bob device registered on node")
        val kp = core.exportKeyPackage()
        File(dir, "bob.kp").writeBytes(kp)
        File(dir, "bob-credential.txt").writeText(core.keyPackageCredential())
        log("bob exported key package: " + kp.size + " bytes")
        log("PASS bob prepared and goes OFFLINE")
        0
    } catch (error: Throwable) {
        log("FAIL bob-prepare: " + error::class.java.simpleName + ": " + (error.message ?: ""))
        1
    } finally {
        runCatching { core.close() }
    }
}

private fun aliceSend(dir: File, endpoint: String, nodeId: String, nodeKey: String): Int {
    val core = ChatCoreFfi.open(File(dir, "alice-chat.db").absolutePath, MASTER_KEY)
    return try {
        val account = core.enrollLocalIdentity(File(dir, "alice-identity.db").absolutePath, MASTER_KEY)
        log("alice enrolled: " + account.deviceId)
        val kp = File(dir, "bob.kp").readBytes()
        val bobDevice = File(dir, "bob-device.txt").readText().trim()
        log("bob device (offline): " + bobDevice)
        val created = core.createGroupWithInviteeKeyPackage(
            "offline delivery",
            bobDevice,
            kp,
            core.keyPackageCredential(),
        )
        log("PASS alice created group: " + created.group.groupId + " epoch=" + created.group.epoch)
        core.configureSendGroup(created.group.mlsGroupKey)
        core.configureSendNode(endpoint, nodeId, nodeKey)
        val registered = core.groupRegisterOnNode(created.group.groupId, endpoint, nodeId, nodeKey)
        log("PASS alice registered group on node: seq=" + registered.registeredSeq)
        val head = core.pullState(created.group.groupId)
            ?: run { log("FAIL no pull state"); return 5 }
        writeHandoff(File(dir, "a2b.txt"), created.invite.welcomeBytes, created.invite, head)
        val memberAdd = core.submitMemberAdd(created.group.groupId, bobDevice, endpoint, nodeId, nodeKey)
        log("PASS alice added offline bob as node member: seq=" + memberAdd.seq)
        val receipt = core.submitInvite(created.group.groupId, created.invite, endpoint, nodeId, nodeKey)
        log("PASS alice submitted invite: seq=" + receipt.seq)
        core.configureSendNode(endpoint, nodeId, nodeKey)
        core.configureSendGroup(created.group.mlsGroupKey)
        val draft = core.createDraft("conv:1:" + "33".repeat(32), "op:1:" + "44".repeat(32), "message while bob was offline")
        core.sendPrepare(draft.draftId)
        val dispatched = core.sendDispatch(draft.draftId)
        log("alice send dispatch: " + dispatched.status)
        if (dispatched.status != "accepted") {
            log("FAIL alice dispatch status=" + dispatched.status)
            return 6
        }
        log("PASS alice message accepted by node while bob was offline")
        0
    } catch (error: Throwable) {
        log("FAIL alice-send: " + error::class.java.simpleName + ": " + (error.message ?: ""))
        1
    } finally {
        runCatching { core.close() }
    }
}

private fun bobReceive(dir: File, endpoint: String, nodeId: String, nodeKey: String): Int {
    val core = ChatCoreFfi.open(File(dir, "bob-chat.db").absolutePath, MASTER_KEY)
    return try {
        val fields = readFields(File(dir, "a2b.txt"))
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
        log("PASS bob joined after coming back online: " + joined.groupId)
        var applied = 0uL
        for (round in 1..20) {
            val once = core.syncOnce(joined.groupId, endpoint, nodeId, nodeKey)
            applied = once.applied
            log("bob pull round " + round + ": fetched=" + once.fetched + " applied=" + once.applied)
            if (once.applied > 0uL) break
            Thread.sleep(500)
        }
        if (applied == 0uL) {
            log("FAIL bob applied nothing (offline delivery did not reach him)")
            return 4
        }
        // 投递证明用**落库计数**（applied>0），不依赖具体会话 ID：
        // 会话是本地概念，跨进程验证里没有稳定的会话标识可引用。
        log("PASS bob received " + applied + " entries that were sent while he was offline")
        0
    } catch (error: Throwable) {
        log("FAIL bob-receive: " + error::class.java.simpleName + ": " + (error.message ?: ""))
        1
    } finally {
        runCatching { core.close() }
    }
}

/** 先把两份身份库建出来（enroll 需要已有身份种子；与双设备演示的 identities 步骤等价）。 */
private fun createIdentities(dir: File): Int = try {
    listOf("alice-identity.db", "bob-identity.db").forEach { name ->
        val identity = IdentityFfi.openPersisted(File(dir, name).absolutePath, MASTER_KEY, "node:1:" + "0a".repeat(32))
        log("identity ready: " + name + " device=" + identity.account().deviceId)
        runCatching { identity.close() }
    }
    log("PASS identities created")
    0
} catch (error: Throwable) {
    log("FAIL identities: " + (error.message ?: ""))
    1
}

fun main(args: Array<String>): Unit {
    if (args.size < 4) {
        println("usage: OfflineDeliverySmokeKt <bob-prepare|alice-send|bob-receive> <dir> <endpoint> <nodeId>")
        kotlin.system.exitProcess(2)
    }
    val mode = args[0]
    if (mode == "identities") {
        kotlin.system.exitProcess(createIdentities(File(args[1]).also { it.mkdirs() }))
    }
    val dir = File(args[1]).also { it.mkdirs() }
    val endpoint = args[2]
    val nodeId = args[3]
    val nodeKey = fetchNodePublicKey(endpoint)
    val code = when (mode) {
        "bob-prepare" -> bobPrepare(dir, endpoint, nodeId, nodeKey)
        "alice-send" -> aliceSend(dir, endpoint, nodeId, nodeKey)
        "bob-receive" -> bobReceive(dir, endpoint, nodeId, nodeKey)
        else -> { println("unknown mode: " + mode); 2 }
    }
    kotlin.system.exitProcess(code)
}
