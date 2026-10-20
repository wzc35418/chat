package chat.desktop

import chat.platform.SecretBuffer
import kotlinx.coroutines.runBlocking
import java.nio.file.Files

/**
 * 密钥槽探测与迁移冒烟：**真的在 DPAPI 文件 ↔ Windows 凭据管理器之间搬一次密钥**。
 *
 * 断言：
 * 1) 空目录探测 = None；
 * 2) 用 DPAPI 写入后探测 = DpapiFile；
 * 3) 迁移到凭据管理器后：读回逐字节相同、DPAPI blob **已删除**、探测 = CredentialManager；
 * 4) 迁移失败（源别名不存在）→ 显式失败且**目标不残留**。
 */
fun main() = runBlocking {
    if (!System.getProperty("os.name", "").lowercase().contains("windows")) {
        println("SKIP key-slot selector smoke: 非 Windows 平台")
        return@runBlocking
    }
    val directory = Files.createTempDirectory("chat-keyslot-").toFile()
    val alias = "sel-" + System.nanoTime()
    val selector = KeySlotSelector.default(directory)
    try {
        check(selector.probe(alias).backend == KeySlotBackend.None) { "空环境应探测为 None" }

        val dpapi = DpapiKeySlot(directory)
        val secret = ByteArray(32) { index -> (index * 11 + 5).toByte() }
        dpapi.wrap(alias, SecretBuffer(secret.copyOf()))
        check(selector.probe(alias).backend == KeySlotBackend.DpapiFile) { "写入 DPAPI 后应探测为 DpapiFile" }
        println("PASS key-slot probe: dpapi-file")

        val credential = WinCredKeySlot()
        val message = selector.migrate(alias, dpapi, credential)
        check(message.contains("credential-manager")) { "迁移结果应说明后端，实际：" + message }
        val readBack = credential.unwrapByAlias(alias).copyBytes()
        try {
            check(readBack.contentEquals(secret)) { "迁移后读回的密钥必须逐字节相同" }
        } finally {
            readBack.fill(0)
        }
        check(!dpapi.exists(alias)) { "迁移成功后旧 DPAPI blob 必须删除" }
        check(selector.probe(alias).backend == KeySlotBackend.CredentialManager) { "迁移后探测应为凭据管理器" }
        println("PASS key-slot migrate: dpapi-file -> credential-manager (verify then delete)")

        // 失败路径：源别名不存在 → 显式失败，目标不残留。
        val ghost = "ghost-" + System.nanoTime()
        val failure = runCatching { selector.migrate(ghost, dpapi, credential) }.exceptionOrNull()
        check(failure != null) { "源不存在时迁移必须失败" }
        check(!credential.exists(ghost)) { "失败的迁移不应在目标留下密钥" }
        println("PASS key-slot migrate: failure keeps target clean")

        // 清理
        credential.delete(alias)
        check(!credential.exists(alias))
    } finally {
        runCatching { WinCredKeySlot().delete(alias) }
        directory.deleteRecursively()
    }
    println("PASS key-slot selector slice")
}
