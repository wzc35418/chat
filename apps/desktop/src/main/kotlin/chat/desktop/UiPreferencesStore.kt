package chat.desktop

import java.io.File

/**
 * 界面偏好（**非机密**）：字号缩放与面板显隐。
 *
 * 语义：只表达"界面怎么显示"，不影响任何业务语义；写失败必须显式告警，不静默。
 */
data class UiPreferences(
    /** 界面缩放（1.0 = 标准）。范围 0.85..1.4，越界回落默认。 */
    val scale: Float = 1.0f,
    /** 是否显示「关于/诊断」入口。 */
    val showAbout: Boolean = true,
    /** 是否显示「插件」入口。 */
    val showPlugins: Boolean = true,
)

class UiPreferencesStore(private val file: File) {
    fun load(): UiPreferences {
        if (!file.isFile) return UiPreferences()
        val text = runCatching { file.readText() }.getOrNull() ?: return UiPreferences()
        fun num(name: String): Float? = Regex("\"" + name + "\"\\s*:\\s*([0-9.]+)").find(text)?.groupValues?.get(1)?.toFloatOrNull()
        fun flag(name: String): Boolean? = Regex("\"" + name + "\"\\s*:\\s*(true|false)").find(text)?.groupValues?.get(1)?.toBooleanStrictOrNull()
        val defaults = UiPreferences()
        val scale = num("scale")?.takeIf { it in 0.85f..1.4f } ?: defaults.scale
        return UiPreferences(
            scale = scale,
            showAbout = flag("showAbout") ?: defaults.showAbout,
            showPlugins = flag("showPlugins") ?: defaults.showPlugins,
        )
    }

    /** 写入；失败抛出（调用方必须提示，不允许假成功）。 */
    fun save(preferences: UiPreferences) {
        file.parentFile?.mkdirs()
        val scale = preferences.scale.coerceIn(0.85f, 1.4f)
        val text = buildString {
            appendLine("{")
            appendLine("  \"scale\": " + scale + ",")
            appendLine("  \"showAbout\": " + preferences.showAbout + ",")
            appendLine("  \"showPlugins\": " + preferences.showPlugins)
            appendLine("}")
        }
        val tmp = File(file.parentFile, file.name + ".tmp")
        tmp.writeText(text)
        if (!tmp.renameTo(file)) {
            file.delete()
            if (!tmp.renameTo(file)) throw IllegalStateException("无法写入界面偏好：" + file.absolutePath)
        }
    }

    companion object {
        fun defaultFile(): File {
            val base = System.getenv("APPDATA")
            val dir = if (base.isNullOrBlank()) File(System.getProperty("user.home"), ".chat") else File(base, "Chat")
            return File(dir, "ui-preferences.json")
        }
    }
}
