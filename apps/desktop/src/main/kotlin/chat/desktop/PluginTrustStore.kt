package chat.desktop

import java.io.File

/**
 * 插件签名信任列表（**非机密**）：插件 id -> 该插件签名证书的 SHA-256 指纹。
 *
 * 语义：
 * - 记录 = 「我确认过这个插件的签名证书就是这个指纹」，下次加载自动按它校验；
 * - 指纹变化（插件换签名密钥）默认**不覆盖**，必须显式更新——否则等于静默接受新密钥；
 * - 写失败抛错，不静默。
 */
class PluginTrustStore(private val file: File) {

    fun load(): Map<String, String> {
        if (!file.isFile) return emptyMap()
        val text = runCatching { file.readText() }.getOrNull() ?: return emptyMap()
        val out = LinkedHashMap<String, String>()
        Regex("\"([a-zA-Z0-9._-]{1,64})\"\\s*:\\s*\"([0-9a-f]{64})\"").findAll(text).forEach { match ->
            out[match.groupValues[1]] = match.groupValues[2]
        }
        return out
    }

    /** 记录（或更新前先确认）某插件的签名指纹。 */
    fun remember(pluginId: String, fingerprint: String): Boolean {
        val normalized = fingerprint.trim().lowercase()
        if (!isHex64(normalized)) return false
        val current = load()
        val next = LinkedHashMap(current)
        next[pluginId] = normalized
        save(next)
        return true
    }

    fun forget(pluginId: String) {
        val next = LinkedHashMap(load())
        if (next.remove(pluginId) != null) save(next)
    }

    fun save(entries: Map<String, String>): Boolean {
        val safe = entries.entries
            .filter { isHex64(it.value.trim().lowercase()) && it.key.length in 1..64 }
            .associate { it.key to it.value.trim().lowercase() }
        return runCatching {
            file.parentFile?.mkdirs()
            val body = buildString {
                appendLine("{")
                val items = safe.entries.toList()
                items.forEachIndexed { index, entry ->
                    appendLine("  \"" + entry.key + "\": \"" + entry.value + "\"" + if (index < items.size - 1) "," else "")
                }
                appendLine("}")
            }
            val tmp = File(file.parentFile, file.name + ".tmp")
            tmp.writeText(body)
            if (!tmp.renameTo(file)) {
                file.delete()
                check(tmp.renameTo(file)) { "无法写入插件信任列表" }
            }
            true
        }.getOrDefault(false)
    }

    companion object {
        fun defaultFile(): File {
            val base = System.getenv("APPDATA")
            val dir = if (base.isNullOrBlank()) File(System.getProperty("user.home"), ".chat") else File(base, "Chat")
            return File(dir, "plugin-trust.json")
        }

        fun isHex64(value: String): Boolean =
            value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }
    }
}
