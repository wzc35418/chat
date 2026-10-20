package chat.desktop

import chat.plugin.DesktopPlugin
import chat.plugin.PluginContext
import chat.plugin.Registration
import java.io.File
import java.net.URLClassLoader
import java.util.ServiceLoader
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/** 插件加载失败：稳定码 + 说明。 */
class PluginLoadException(val code: String, message: String) : Exception(message)

/**
 * PLUGINS P4 起步：**真正加载并执行**桌面插件 JAR。
 *
 * 诚实边界（必须知道）：
 * 1) 插件在**同一 JVM、无沙箱**运行；加载前必须由用户显式确认信任（\`trusted = true\`）；
 * 2) 只用独立 ClassLoader 做类隔离（插件之间互不可见），**不是安全边界**；
 * 3) 执行有上限（线程 + 超时 + 输出字节上限），但超时只能"放弃等待"，无法强杀线程；
 * 4) 插件注册的命令产出**只能是草稿供用户确认**（SDK 草案约定），本宿主不直接发送消息。
 */
class PluginJvmHost {

    private data class Loaded(
        val id: String,
        val loader: ClassLoader,
        val plugin: DesktopPlugin,
        val registrations: MutableList<Registration> = mutableListOf(),
    )

    private val loaded = LinkedHashMap<String, Loaded>()
    private val executor = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "chat-plugin-exec").apply { isDaemon = true }
    }

    /** 已加载的插件 id（只读）。 */
    fun loadedPluginIds(): List<String> = loaded.keys.toList()

    /** 插件注册的命令名（只读；不含实现）。 */
    fun commands(pluginId: String): List<String> = commandNames[pluginId]?.toList() ?: emptyList()

    private val commandNames = LinkedHashMap<String, MutableList<String>>()

    private val selectedTextActions = LinkedHashMap<String, MutableMap<String, (String) -> String>>()

    /** 声明式槽位 composer.actions：插件 id -> (动作名 -> 说明)，只存元数据。 */
    private val composerActions = LinkedHashMap<String, MutableMap<String, String>>()

    private val composerImpls = LinkedHashMap<String, MutableMap<String, () -> String>>()

    /**
     * 加载并启动插件。
     *
     * @param jarPath 插件 JAR 路径（必须已通过 Rust 侧结构校验）
     * @param expectedId 清单里的插件 id（必须与插件自报 id 一致）
     * @param trusted 用户是否已确认信任该插件；false 直接拒绝
     */
    fun load(
        jarPath: String,
        expectedId: String,
        trusted: Boolean,
        permissions: Set<String> = emptySet(),
    ): String {
        if (!trusted) {
            throw PluginLoadException("PLUGIN_TRUST_REQUIRED", "插件加载需要用户显式确认信任（无沙箱执行）")
        }
        val jar = File(jarPath)
        if (!jar.isFile) throw PluginLoadException("PLUGIN_PACKAGE_INVALID", "插件 JAR 不存在")
        if (jar.length() > 8L * 1024 * 1024) throw PluginLoadException("PLUGIN_PACKAGE_TOO_LARGE", "插件 JAR 超过 8 MiB")
        loaded[expectedId]?.let { throw PluginLoadException("PLUGIN_STATE_CONFLICT", "插件已加载：" + expectedId) }

        // 父加载器只暴露 SDK 接口；并按清单权限拦截敏感包（最小可行沙箱，不是安全边界）。
        val unknown = permissions - PluginSandboxClassLoader.KNOWN
        if (unknown.isNotEmpty()) {
            throw PluginLoadException("PLUGIN_PERMISSION_DENIED", "未知权限声明：" + unknown.joinToString(","))
        }
        val loader = PluginSandboxClassLoader(
            arrayOf(jar.toURI().toURL()),
            permissions,
            DesktopPlugin::class.java.classLoader,
        )
        val plugin = try {
            ServiceLoader.load(DesktopPlugin::class.java, loader).firstOrNull()
                ?: throw PluginLoadException("PLUGIN_SPI_MISMATCH", "JAR 中找不到 DesktopPlugin 实现")
        } catch (error: PluginLoadException) {
            runCatching { loader.close() }
            throw error
        } catch (error: Throwable) {
            runCatching { loader.close() }
            throw PluginLoadException("PLUGIN_PACKAGE_INVALID", "加载插件类失败：" + (error.message ?: error::class.simpleName))
        }
        if (plugin.id != expectedId) {
            runCatching { loader.close() }
            throw PluginLoadException("PLUGIN_ID_MISMATCH", "插件自报 id 与清单不一致：" + plugin.id)
        }

        val record = Loaded(expectedId, loader, plugin)
        val context = HostContext(record, commandNames, selectedTextActions, composerActions, composerImpls)
        val started = try {
            withDeadline("PLUGIN_START_TIMEOUT") { plugin.start(context) }
            true
        } catch (error: Throwable) {
            runCatching { plugin.stop() }
            runCatching { loader.close() }
            record.registrations.forEach { runCatching { it.dispose() } }
            throw error
        }
        check(started)
        loaded[expectedId] = record
        return expectedId
    }

    /** 停止并卸载插件（尽力而为：dispose 注册项 + 关闭 ClassLoader）。 */
    fun unload(pluginId: String): Boolean {
        val record = loaded.remove(pluginId) ?: return false
        runCatching { record.registrations.forEach { it.dispose() } }
        runCatching { record.plugin.stop() }
        runCatching { (record.loader as? java.io.Closeable)?.close() }
        commandNames.remove(pluginId)
        // 真 bug 修复（冒烟抓到）：卸载必须同时清掉动作表，否则卸载后仍能执行插件动作。
        selectedTextActions.remove(pluginId)
        // 槽位注册也要随卸载清掉（同 selectedTextActions 的教训）。
        composerActions.remove(pluginId)
        composerImpls.remove(pluginId)
        return true
    }

    /** 关闭宿主：卸载全部插件并停掉执行线程池。 */
    fun shutdown() {
        loaded.keys.toList().forEach { unload(it) }
        executor.shutdownNow()
    }

    /** 已注册的 composer.actions（插件 id -> [(名字, 说明)]），供界面渲染按钮。 */
    fun composerActions(): List<Triple<String, String, String>> = buildList {
        for ((pluginId, actions) in composerActions) {
            for ((name, description) in actions) add(Triple(pluginId, name, description))
        }
    }

    /** 运行某个 composer 动作，返回可填入草稿的文本（受大小上限约束）。 */
    fun runComposerAction(pluginId: String, name: String): String {
        val action = composerImpls[pluginId]?.get(name)
            ?: throw PluginLoadException("PLUGIN_OPERATION_UNKNOWN", "未知动作：" + pluginId + "/" + name)
        val output = withDeadline("PLUGIN_TASK_TIMEOUT") { action() }
        val bytes = output.toByteArray(Charsets.UTF_8).size
        if (bytes > MAX_OUTPUT_BYTES) {
            throw PluginLoadException("CONTENT_TOO_LARGE", "插件输出超过上限（" + bytes + " 字节）")
        }
        return output
    }

    /** 在宿主上下文里执行插件的选中文本动作（返回结果受大小上限约束）。 */
    fun runSelectedTextAction(pluginId: String, name: String, input: String): String {
        val action = selectedTextActions[pluginId]?.get(name)
            ?: throw PluginLoadException("PLUGIN_OPERATION_UNKNOWN", "未知动作：" + pluginId + "/" + name)
        if (input.toByteArray(Charsets.UTF_8).size > MAX_INPUT_BYTES) {
            throw PluginLoadException("CONTENT_TOO_LARGE", "输入超过上限")
        }
        val output = withDeadline("PLUGIN_TASK_TIMEOUT") { action(input) }
        val bytes = output.toByteArray(Charsets.UTF_8).size
        if (bytes > MAX_OUTPUT_BYTES) {
            throw PluginLoadException("CONTENT_TOO_LARGE", "插件输出超过上限（" + bytes + " 字节）")
        }
        return output
    }

    private class HostContext(
        private val record: Loaded,
        private val commands: MutableMap<String, MutableList<String>>,
        private val actions: MutableMap<String, MutableMap<String, (String) -> String>>,
        private val composerMeta: MutableMap<String, MutableMap<String, String>>,
        private val composerImpls: MutableMap<String, MutableMap<String, () -> String>>,
    ) : PluginContext {
        override fun registerCommand(name: String, action: () -> String): Registration {
            // 草案阶段：命令产出只能是草稿；这里只记录名字，不执行 action。
            commands.getOrPut(record.id) { mutableListOf() }.add(name)
            return object : Registration {
                override fun dispose() {
                    commands[record.id]?.remove(name)
                }
            }
        }

        override fun registerComposerAction(
            name: String,
            description: String,
            action: () -> String,
        ): Registration {
            composerMeta.getOrPut(record.id) { LinkedHashMap() }[name] = description.ifBlank { name }
            composerImpls.getOrPut(record.id) { LinkedHashMap() }[name] = action
            return object : Registration {
                override fun dispose() {
                    composerMeta[record.id]?.remove(name)
                    composerImpls[record.id]?.remove(name)
                }
            }
        }

        override fun registerSelectedTextAction(name: String, action: (String) -> String): Registration {
            actions.getOrPut(record.id) { LinkedHashMap() }[name] = action
            return object : Registration {
                override fun dispose() {
                    actions[record.id]?.remove(name)
                }
            }
        }
    }

    private fun <T> withDeadline(code: String, block: () -> T): T {
        val future = executor.submit<T> { block() }
        return try {
            future.get(EXECUTION_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        } catch (error: java.util.concurrent.TimeoutException) {
            future.cancel(true)
            throw PluginLoadException(code, "插件执行超时（" + EXECUTION_TIMEOUT_MS + " ms）")
        } catch (error: java.util.concurrent.ExecutionException) {
            val cause = error.cause ?: error
            throw PluginLoadException("PLUGIN_EXECUTION_FAILED", cause.message ?: cause::class.simpleName ?: "插件执行失败")
        }
    }

    companion object {
        private const val EXECUTION_TIMEOUT_MS = 2_000L
        private const val MAX_INPUT_BYTES = 64 * 1024
        private const val MAX_OUTPUT_BYTES = 256 * 1024
    }
}
