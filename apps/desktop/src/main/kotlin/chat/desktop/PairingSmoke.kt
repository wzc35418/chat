package chat.desktop

import uniffi.chat_ffi.ChatCoreFfi
import uniffi.chat_ffi.IdentityFfi
import java.io.File
import java.nio.file.Files

private val MASTER_KEY = "0".repeat(64)

/**
 * 配对包往返：**真实的 MLS 邀请 → Welcome 入群**，全程只走 copy/paste 文本（不依赖网络）。
 *
 * 这条冒烟证明 UI 要用的三件事：
 * 1. 被邀请者能导出 KeyPackage 包（凭据 + KeyPackage 原文）；
 * 2. 邀请者能用该包建群并产出**可直接粘贴**的邀请包（Welcome + 元数据 + 已验证起点 head）；
 * 3. 被邀请者用邀请包入群成功，且群列表里真的出现该群。
 *
 * 退出码：0 = PASS，1 = FAIL，3 = 无法运行。
 */
fun main(args: Array<String>) {
    // 三个值可以用参数给，也可以用 -Dpairing.* 系统属性给（Gradle JavaExec 更方便）。
    val endpoint = args.getOrNull(0) ?: System.getProperty("pairing.endpoint")
    val nodeId = args.getOrNull(1) ?: System.getProperty("pairing.nodeId")
    val nodeKey = args.getOrNull(2) ?: System.getProperty("pairing.nodeKey")
    if (endpoint.isNullOrBlank() || nodeId.isNullOrBlank() || nodeKey.isNullOrBlank()) {
        println("usage: PairingSmokeKt <endpoint> <nodeId> <nodeKey> (or -Dpairing.endpoint/... )")
        kotlin.system.exitProcess(2)
    }
    runBlockingMain(endpoint, nodeId, nodeKey)
}

private fun runBlockingMain(endpoint: String, nodeId: String, nodeKey: String): Unit {
    val dir = Files.createTempDirectory("chat-pairing-smoke-").toFile()
    var bob: ChatCoreFfi? = null
    var alice: ChatCoreFfi? = null
    try {
        log("workspace: " + dir.absolutePath)

        // 0) 先建两条身份库：enrollLocalIdentity 是**导入身份库里的种子**，
        //    身份库不存在时会报 INVALID_INPUT（"the source database holds no identity seeds"）。
        val node = "node:1:" + "0a".repeat(32)
        for (name in listOf("alice-identity.db", "bob-identity.db")) {
            val identity = IdentityFfi.openPersisted(File(dir, name).absolutePath, MASTER_KEY, node)
            log("identity ready: " + name + " device=" + identity.account().deviceId)
            runCatching { identity.close() }
        }

        // 1) 被邀请者（bob）：导出 KeyPackage 包
        bob = ChatCoreFfi.open(File(dir, "bob-chat.db").absolutePath, MASTER_KEY)
        val bobAccount = bob.enrollLocalIdentity(File(dir, "bob-identity.db").absolutePath, MASTER_KEY)
        log("bob enrolled: " + bobAccount.deviceId)
        val bobPackText = PairingPack.encodeKeyPackage(bob.keyPackageCredential(), bob.exportKeyPackage())
        val bobPack = PairingPack.parseKeyPackage(bobPackText)
        if (bobPack.keyPackage.isEmpty()) { log("FAIL bob key package is empty"); kotlin.system.exitProcess(1) }
        log("PASS bob key package pack: " + bobPack.credential + " (" + bobPack.keyPackage.size + " bytes)")

        // 2) 邀请者（alice）：用 bob 的包建群，产出邀请包
        alice = ChatCoreFfi.open(File(dir, "alice-chat.db").absolutePath, MASTER_KEY)
        val aliceAccount = alice.enrollLocalIdentity(File(dir, "alice-identity.db").absolutePath, MASTER_KEY)
        log("alice enrolled: " + aliceAccount.deviceId)
        val created = alice.createGroupWithInviteeKeyPackage(
            "pairing smoke",
            bobPack.credential,
            bobPack.keyPackage,
            alice.keyPackageCredential(),
        )
        // 拉取锚点（verified head）只有在**向节点登记群**之后才存在：
        // 设计上就是"不信任节点自报 head"，所以必须真的登记一次。
        alice.groupRegisterOnNode(created.group.groupId, endpoint, nodeId, nodeKey)
        log("PASS alice registered the group on the node")
        val head = alice.pullState(created.group.groupId)
        if (head == null) { log("FAIL alice has no pull state for the new group"); kotlin.system.exitProcess(1) }
        val invitePackText = PairingPack.encodeInvite(created.invite, head)
        val invitePack = PairingPack.parseInvite(invitePackText)
        log("PASS invite pack: group=" + invitePack.invite.groupId + " epoch=" + invitePack.invite.epoch +
            " welcome=" + invitePack.invite.welcomeBytes.size + " bytes, head.seq=" + invitePack.head.cursorSeq)

        // 3) 被邀请者（bob）：用邀请包入群
        // 必须用**同一个** Bob 实例：KeyPackage 的私钥材料就在这个 MLS 设备里；
        // 关掉再开会让 core 去库里找身份种子（库里没有）→ INVALID_INPUT。
        val joined = bob.joinGroupFromWelcome(invitePack.invite.name, invitePack.invite, invitePack.head)
        log("PASS bob joined: " + joined.groupId + " epoch=" + joined.epoch)
        val groups = bob.listGroups()
        val found = groups.firstOrNull { it.groupId == joined.groupId }
        if (found == null) { log("FAIL joined group missing from groupList"); kotlin.system.exitProcess(1) }
        log("PASS groupList shows the joined group: " + found.name + " epoch=" + found.epoch)

        // 4) 反向：被邀请者的凭据/群与邀请者一致
        if (joined.groupId != created.group.groupId) {
            log("FAIL group id mismatch"); kotlin.system.exitProcess(1)
        }
        log("PASS invite round-trip: same group on both sides")
        log("PAIRING SMOKE: PASS")
        kotlin.system.exitProcess(0)
    } catch (error: Throwable) {
        log("FAIL " + error::class.java.simpleName + ": " + (error.message ?: ""))
        kotlin.system.exitProcess(1)
    } finally {
        runCatching { bob?.close() }
        runCatching { alice?.close() }
    }
}

private fun log(message: String) = println(message)
