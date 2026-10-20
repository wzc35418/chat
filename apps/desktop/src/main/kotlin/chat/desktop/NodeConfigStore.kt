package chat.desktop

import java.io.File

/**
 * 节点连接配置的本机持久化（**非机密**：地址、节点 ID、节点公钥都是公开信息）。
 *
 * 语义边界：只保存这三项；口令与任何密钥材料都不进这个文件。读写失败都不静默吞掉，
 * 由调用方决定如何提示（读失败视为"未配置"，写失败必须显式告警）。
 */
data class NodeConfig(
    val endpointUrl: String = "",
    val nodeId: String = "",
    val nodePublicKeyHex: String = "",
    /** 传输信任：证书 SHA-256 指纹集合（空 = 明文 HTTP；非空 = 只接受这些证书的 https）。 */
    val tlsFingerprints: List<String> = emptyList(),
) {
    val isConfigured: Boolean get() = endpointUrl.isNotBlank() && nodeId.isNotBlank()
}

class NodeConfigStore(private val file: File) {
    fun load(): NodeConfig {
        if (!file.isFile) return NodeConfig()
        val text = runCatching { file.readText() }.getOrNull() ?: return NodeConfig()
        return parse(text)
    }

    /** 写入；失败抛出，调用方必须提示（不允许假成功）。 */
    fun save(config: NodeConfig) {
        file.parentFile?.mkdirs()
        val tmp = File(file.parentFile, file.name + ".tmp")
        tmp.writeText(render(config))
        if (!tmp.renameTo(file)) {
            file.delete()
            if (!tmp.renameTo(file)) throw IllegalStateException("无法写入配置：" + file.absolutePath)
        }
    }

    companion object {
        fun defaultFile(): File {
            val base = System.getenv("APPDATA")
            val dir = if (base.isNullOrBlank()) File(System.getProperty("user.home"), ".chat") else File(base, "Chat")
            return File(dir, "node-config.json")
        }

        fun parse(text: String): NodeConfig {
            fun field(name: String): String {
                val needle = "\"" + name + "\""
                val at = text.indexOf(needle)
                if (at < 0) return ""
                val colon = text.indexOf(':', at + needle.length)
                if (colon < 0) return ""
                val start = text.indexOf('"', colon + 1)
                if (start < 0) return ""
                val end = text.indexOf('"', start + 1)
                if (end < 0) return ""
                return text.substring(start + 1, end)
            }
            return NodeConfig(
                endpointUrl = field("endpointUrl"),
                nodeId = field("nodeId"),
                nodePublicKeyHex = field("nodePublicKeyHex"),
                tlsFingerprints = parseFingerprintList(text),
            )
        }

        /** 解析 `tlsFingerprints` 数组（只取 64 位小写十六进制项；其它忽略）。 */
        fun parseFingerprintList(text: String): List<String> {
            val at = text.indexOf("\"tlsFingerprints\"")
            if (at < 0) return emptyList()
            val open = text.indexOf('[', at)
            if (open < 0) return emptyList()
            val close = text.indexOf(']', open)
            if (close < 0) return emptyList()
            return text.substring(open + 1, close)
                .split(',')
                .map { it.trim().trim('"').trim().lowercase() }
                .filter { it.length == 64 && it.all { c -> c in "0123456789abcdef" } }
                .distinct()
        }

        fun render(config: NodeConfig): String = buildString {
            appendLine("{")
            appendLine("  \"endpointUrl\": \"" + escape(config.endpointUrl) + "\",")
            appendLine("  \"nodeId\": \"" + escape(config.nodeId) + "\",")
            appendLine("  \"nodePublicKeyHex\": \"" + escape(config.nodePublicKeyHex) + "\",")
            val fingerprints = config.tlsFingerprints
                .map { it.trim().lowercase() }
                .filter { it.length == 64 && it.all { c -> c in "0123456789abcdef" } }
                .distinct()
            appendLine("  \"tlsFingerprints\": [" + fingerprints.joinToString(",") { "\"" + it + "\"" } + "]")
            appendLine("}")
        }

        private fun escape(value: String): String = value.replace("\\", "\\\\").replace("\"", "\\\"")
    }
}
