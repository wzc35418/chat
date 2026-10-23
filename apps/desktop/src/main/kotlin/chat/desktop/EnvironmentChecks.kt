package chat.desktop

import java.io.File
import java.net.InetSocketAddress
import java.net.Socket
import java.net.URI

/** 一条环境检查结果。 */
data class EnvCheck(val name: String, val ok: Boolean, val detail: String)

/**
 * 环境自检：和**业务无关**但会让人误判的三件事——剪贴板、数据目录可写、磁盘余量，
 * 外加节点端口的可达性（跨洲网络下“连不上”和“慢”要分开）。
 * 纯本地/短超时，跑一次不到 4 秒。
 */
object EnvironmentChecks {
    fun run(dataDir: File, endpointUrl: String, timeoutMillis: Int = 3000): List<EnvCheck> {
        val checks = mutableListOf<EnvCheck>()

        // 1) 剪贴板：邀请口令/KeyPackage 都靠它交付
        val sample = "env-check-" + System.nanoTime()
        val copied = copyToClipboard(sample) && readClipboard() == sample
        checks += EnvCheck(
            "clipboard",
            copied,
            if (copied) "可读写（复制/粘贴可用）" else "不可用：复制按钮会失败（远程桌面/无桌面会话时常见）",
        )

        // 2) 数据目录可写：身份库/消息库都在这里
        val writable = runCatching {
            dataDir.mkdirs()
            val probe = File(dataDir, ".env-check-" + System.nanoTime())
            probe.writeText("ok")
            val readBack = probe.readText()
            probe.delete()
            readBack == "ok"
        }.getOrDefault(false)
        checks += EnvCheck(
            "data_dir",
            writable,
            if (writable) "可写：" + dataDir.absolutePath else "不可写：" + dataDir.absolutePath + "（权限或磁盘满）",
        )

        // 3) 磁盘余量：低于 100 MB 时同步/落库会开始随机失败
        val usable = runCatching { dataDir.usableSpace }.getOrDefault(0L)
        val mb = usable / (1024 * 1024)
        checks += EnvCheck(
            "disk_space",
            mb >= 100,
            mb.toString() + " MB 可用" + (if (mb >= 100) "" else "（低于 100 MB，建议清理）"),
        )

        // 4) 节点端口可达（只做 TCP 握手，不发协议请求）
        val port = runCatching { URI(endpointUrl.trim()).port.takeIf { it > 0 } ?: if (endpointUrl.startsWith("https")) 443 else 80 }.getOrDefault(0)
        val host = runCatching { URI(endpointUrl.trim()).host }.getOrNull()
        if (host.isNullOrBlank() || port == 0) {
            checks += EnvCheck("node_port", false, "节点地址不可解析：" + endpointUrl)
        } else {
            val reachable = runCatching {
                Socket().use { socket ->
                    socket.connect(InetSocketAddress(host, port), timeoutMillis)
                    true
                }
            }.getOrDefault(false)
            checks += EnvCheck(
                "node_port",
                reachable,
                host + ":" + port + if (reachable) " 可达（TCP 握手成功）" else " 连不上（防火墙/进程未起/网络）",
            )
        }
        return checks
    }
}
