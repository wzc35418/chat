package chat.desktop

import chat.platform.KeyProtectionException
import chat.platform.SecretBuffer
import kotlinx.coroutines.runBlocking

/**
 * OS 凭据库冒烟：**真的写入/读取/删除 Windows 凭据管理器**里的 32 字节密钥。
 *
 * 断言：
 * 1) 写入后能读回**逐字节相同**的密钥；
 * 2) 别名不存在 → 显式 `UNAVAILABLE`（不是静默返回空）；
 * 3) 删除后 `exists` 为 false 且读取失败；
 * 4) 长度不是 32 字节 → `INVALID_INPUT`；
 * 5) **磁盘上不产生任何 blob 文件**（与 DpapiKeySlot 的关键区别）。
 */
fun main() = runBlocking {
    if (!System.getProperty("os.name", "").lowercase().contains("windows")) {
        println("SKIP credential-manager smoke: 非 Windows 平台")
        return@runBlocking
    }
    val alias = "smoke-" + System.nanoTime()
    val slot = WinCredKeySlot()
    check(slot.platform() == "windows-credential-manager")
    check(slot.protectionSummary().contains("凭据管理器") || slot.protectionSummary().contains("Credential Manager"))

    val secret = ByteArray(32) { index -> (index * 7 + 3).toByte() }
    try {
        check(!slot.exists(alias)) { "测试别名不应预先存在" }
        val wrapped = slot.wrap(alias, SecretBuffer(secret.copyOf()))
        check(wrapped.platform == "windows-credential-manager")
        // OS 凭据库刻意不回传密文（避免误以为可离线解密）。
        check(wrapped.ciphertextBase64.isEmpty()) { "凭据库实现不应回传密文" }

        check(slot.exists(alias)) { "写入后应存在" }
        val read = slot.unwrapByAlias(alias)
        val bytes = read.copyBytes()
        check(bytes.contentEquals(secret)) { "读回的密钥必须与写入逐字节相同" }
        println("PASS credential-manager: wrap/unwrap 32 bytes via Windows Credential Manager")

        val bad = runCatching { slot.wrap("bad-size", SecretBuffer(ByteArray(16))) }.exceptionOrNull()
        check(bad is KeyProtectionException) { "非 32 字节必须被拒绝" }

        slot.delete(alias)
        check(!slot.exists(alias)) { "删除后应不存在" }
        val missing = runCatching { slot.unwrapByAlias(alias) }.exceptionOrNull()
        check(missing is KeyProtectionException) { "删除后读取必须显式失败" }
        slot.delete(alias) // 幂等
        println("PASS credential-manager: delete is idempotent and missing alias fails explicitly")
    } finally {
        runCatching { slot.delete(alias) }
        secret.fill(0)
    }

    // 磁盘上不应有任何 blob 文件（凭据库实现与 DPAPI 文件实现的关键差异）。
    val leftover = java.io.File(System.getProperty("java.io.tmpdir"))
        .listFiles { file -> file.name.endsWith(".dpapi") && file.name.contains("smoke-") }
        ?: emptyArray()
    check(leftover.isEmpty()) { "凭据库实现不应在磁盘留下 .dpapi 文件：" + leftover.joinToString() }
    println("PASS credential-manager slice")
}
