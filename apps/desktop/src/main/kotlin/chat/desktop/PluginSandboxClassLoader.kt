package chat.desktop

import java.net.URL
import java.net.URLClassLoader

/**
 * 插件类加载沙箱（**最小可行版，不是安全边界**）。
 *
 * 做三件事：
 * 1) **父加载器只给 SDK 与 JDK 基础类**：插件看不到宿主的 `chat.desktop.*` / `chat.bridge.*` 等实现类；
 * 2) **按权限声明拦截敏感包**：没有 `network` 权限就不能加载 `java.net.*`；没有 `filesystem` 就不能
 *    加载 `java.io.File*` / `java.nio.file.*`；没有 `process` 就不能加载 `java.lang.Process*` / `java.lang.Runtime`；
 * 3) 拒绝即抛 `PluginLoadException("PLUGIN_PERMISSION_DENIED", …)`，**不静默放行**。
 *
 * 明确的非能力（交付时必须告知）：JVM 内无法真正隔离——插件仍可通过反射/Unsafe/本地库绕过；
 * 超时无法强杀线程；因此本沙箱只提高门槛，不构成安全边界。
 */
class PluginSandboxClassLoader(
    urls: Array<URL>,
    private val permissions: Set<String>,
    parent: ClassLoader,
) : URLClassLoader(urls, parent) {

    companion object {
        const val PERMISSION_NETWORK = "network"
        const val PERMISSION_FILESYSTEM = "filesystem"
        const val PERMISSION_PROCESS = "process"
        const val PERMISSION_HOST_UI = "host-ui"

        val KNOWN: Set<String> = setOf(PERMISSION_NETWORK, PERMISSION_FILESYSTEM, PERMISSION_PROCESS, PERMISSION_HOST_UI)

        private val NETWORK_PREFIXES = listOf("java.net.", "javax.net.", "sun.net.")
        private val FILESYSTEM_PREFIXES = listOf("java.nio.file.", "java.io.File", "java.io.RandomAccessFile")
        private val PROCESS_PREFIXES = listOf("java.lang.Process", "java.lang.Runtime")

        /** 无论是否有权限都禁止：宿主实现类。 */
        private val HOST_PREFIXES = listOf("chat.desktop.", "chat.bridge.", "chat.presentation.", "chat.ui.")
        /** SDK 与 JDK 基础类始终允许。 */
        private val ALWAYS_ALLOWED = listOf(
            "chat.plugin.",
            "java.lang.",
            "java.util.",
            "java.time.",
            "java.text.",
            "java.math.",
            "kotlin.",
            "kotlinx.coroutines.",
        )

        private fun matches(name: String, prefixes: List<String>): Boolean =
            prefixes.any { name.startsWith(it) }
    }

    override fun loadClass(name: String, resolve: Boolean): Class<*> {
        enforce(name)
        return super.loadClass(name, resolve)
    }

    private fun enforce(name: String) {
        // 宿主类一律拒绝（除了 SDK 接口所在的 chat.plugin 包）。
        if (matches(name, HOST_PREFIXES)) {
            throw denied(name, "host classes are never visible to plugins")
        }
        if (matches(name, ALWAYS_ALLOWED)) return
        if (matches(name, NETWORK_PREFIXES) && !permissions.contains(PERMISSION_NETWORK)) {
            throw denied(name, "missing permission: " + PERMISSION_NETWORK)
        }
        if (matches(name, FILESYSTEM_PREFIXES) && !permissions.contains(PERMISSION_FILESYSTEM)) {
            throw denied(name, "missing permission: " + PERMISSION_FILESYSTEM)
        }
        if (matches(name, PROCESS_PREFIXES) && !permissions.contains(PERMISSION_PROCESS)) {
            throw denied(name, "missing permission: " + PERMISSION_PROCESS)
        }
    }

    private fun denied(name: String, reason: String): PluginLoadException =
        PluginLoadException("PLUGIN_PERMISSION_DENIED", "插件不能加载 " + name + "：" + reason)
}
