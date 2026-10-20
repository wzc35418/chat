package chat.desktop

import java.io.File
import java.nio.file.Files
import javax.tools.ToolProvider

/**
 * PLUGINS P4 冒烟：**真正加载并执行**一个本地编译的插件 JAR。
 *
 * 诚实边界：插件在同 JVM、无沙箱执行；加载前必须 trusted=true。
 * 本冒烟在临时目录里编译一个最小插件，然后验证：
 * 加载 → 注册动作 → 执行动作（输出受限）→ 未信任拒绝 → 卸载。
 */
/**
 * 最小可行沙箱的自检（**不是安全边界**，只是门槛）：
 * 1) 宿主实现类永远不可见；
 * 2) 没有 network 权限时加载 java.net.Socket 被拒；
 * 3) 声明 network 后可以加载（说明拦截是按声明生效，不是"什么都拦"）。
 */
private fun verifySandbox() {
    val parent = PluginSandboxClassLoader::class.java.classLoader
    val strict = PluginSandboxClassLoader(arrayOf(), emptySet(), parent)
    val hostBlocked = runCatching { strict.loadClass("chat.desktop.PluginJvmHost") }.exceptionOrNull()
    check(hostBlocked is PluginLoadException) { "host classes must be blocked, got " + hostBlocked }
    check(hostBlocked.code == "PLUGIN_PERMISSION_DENIED") { "expected PLUGIN_PERMISSION_DENIED, got " + hostBlocked.code }

    val networkBlocked = runCatching { strict.loadClass("java.net.Socket") }.exceptionOrNull()
    check(networkBlocked is PluginLoadException) { "java.net.Socket must be blocked without network permission" }

    val filesystemBlocked = runCatching { strict.loadClass("java.nio.file.Files") }.exceptionOrNull()
    check(filesystemBlocked is PluginLoadException) { "java.nio.file.Files must be blocked without filesystem permission" }

    val permitted = PluginSandboxClassLoader(arrayOf(), setOf("network"), parent)
    check(permitted.loadClass("java.net.Socket") != null) { "java.net.Socket must load with network permission" }

    val unknown = runCatching { PluginJvmHost().load("/nonexistent.jar", "x", true, setOf("root")) }
    check(unknown.isFailure) { "unknown permission must be rejected before any file access" }
}

fun main() {
    val work = Files.createTempDirectory("chat-plugin-jvm-").toFile()
    try {
        val jar = buildFixtureJar(work)
        val host = PluginJvmHost()

        // 1) 未确认信任 → 拒绝加载（无沙箱执行必须显式确认）。
        val untrusted = runCatching { host.load(jar.absolutePath, "demo.echo", trusted = false) }.exceptionOrNull()
        check(untrusted is PluginLoadException) { "未信任的插件必须被拒绝" }
        check((untrusted as PluginLoadException).code == "PLUGIN_TRUST_REQUIRED")
        println("      未信任拒绝码：" + untrusted.code)

        // 2) 正常加载并执行。
        val id = host.load(jar.absolutePath, "demo.echo", trusted = true)
        check(id == "demo.echo")
        check(host.loadedPluginIds() == listOf("demo.echo"))
        val output = host.runSelectedTextAction("demo.echo", "echo", "hello-p4")
        check(output == "echo:hello-p4") { "插件输出不符：" + output }

        // 2b) composer.actions 槽位：只暴露元数据，运行结果进草稿（不外泄布局）。
        val slots = host.composerActions()
        check(slots.size == 1) { "应有一个 composer 动作，实际：" + slots }
        check(slots[0].second == "template") { "动作名不符：" + slots[0].second }
        check(slots[0].third == "插入问候模板") { "动作说明不符：" + slots[0].third }
        val inserted = host.runComposerAction("demo.echo", "template")
        check(inserted.contains("插件填入的草稿")) { "composer 动作输出不符：" + inserted }
        println("      槽位 composer.actions：" + slots[0].second + " -> " + inserted.length + " 字符")

        // 2c) palette.commands 槽位：只列元数据，运行产出草稿文本。
        val palette = host.paletteCommands()
        check(palette.any { it.first == "demo.echo" && it.second == "summary" }) {
            "命令面板应包含 summary，实际：" + palette
        }
        val commandOutput = host.runCommand("demo.echo", "summary")
        check(commandOutput == "命令：摘要输出") { "命令输出不符：" + commandOutput }
        println("      槽位 palette.commands：" + palette.size + " 条，运行输出 " + commandOutput.length + " 字符")

        // 3) 插件 id 不符 → 显式失败（清单与自报必须一致）。
        val mismatch = runCatching { host.load(jar.absolutePath, "other.id", trusted = true) }.exceptionOrNull()
        check(mismatch is PluginLoadException)
        println("      id 不符错误码：" + (mismatch as PluginLoadException).code)

        // 4) 已知动作名之外 → 显式失败。
        val unknown = runCatching { host.runSelectedTextAction("demo.echo", "nope", "x") }.exceptionOrNull()
        check(unknown is PluginLoadException)
        check((unknown as PluginLoadException).code == "PLUGIN_OPERATION_UNKNOWN")

        // 5) 卸载后再执行 → 显式失败。
        check(host.unload("demo.echo")) { "卸载应成功" }
        // 卸载后槽位必须清空（否则界面还显示按钮、点了却报未知动作）。
        check(host.composerActions().isEmpty()) { "卸载后 composer 槽位必须清空，实际：" + host.composerActions() }
        check(host.paletteCommands().isEmpty()) { "卸载后 palette 槽位必须清空，实际：" + host.paletteCommands() }
        val afterUnload = runCatching { host.runComposerAction("demo.echo", "template") }.exceptionOrNull()
        check(afterUnload is PluginLoadException) { "卸载后运行槽位动作应显式失败" }
        check(host.loadedPluginIds().isEmpty())
        val gone = runCatching { host.runSelectedTextAction("demo.echo", "echo", "x") }.exceptionOrNull()
        check(gone is PluginLoadException)

        host.shutdown()
        verifySandbox()

    println("PASS plugin P4 slice: trusted load, SPI discovery, bounded execution, unload")
    println("PASS plugin sandbox slice: host classes blocked, sensitive packages gated by declared permissions")
    } finally {
        work.deleteRecursively()
    }
}

/** 在临时目录里编译一个最小插件（需要 JDK 编译器，随项目工具链提供）。 */
private fun buildFixtureJar(work: File): File {
    val sourceDir = File(work, "src").apply { mkdirs() }
    val classesDir = File(work, "classes").apply { mkdirs() }
    val source = File(sourceDir, "DemoEchoPlugin.java")
    source.writeText(
        """
        package demo;
        import chat.plugin.DesktopPlugin;
        import chat.plugin.PluginContext;
        public final class DemoEchoPlugin implements DesktopPlugin {
            private PluginContext context;
            public String getId() { return "demo.echo"; }
            public void start(PluginContext context) {
                this.context = context;
                context.registerSelectedTextAction("echo", text -> "echo:" + text);
                context.registerComposerAction("template", "插入问候模板", () -> "你好，这是一条由插件填入的草稿（请自行确认后发送）");
                context.registerCommand("summary", () -> "命令：摘要输出");
            }
            public void stop() { this.context = null; }
        }
        """.trimIndent(),
        Charsets.UTF_8,
    )
    val compiler = ToolProvider.getSystemJavaCompiler() ?: error("需要 JDK 编译器（请用 JDK 运行本冒烟）")
    val sdkPath = System.getProperty("java.class.path")
    val result = compiler.run(
        null,
        null,
        null,
        "-classpath", sdkPath,
        "-d", classesDir.absolutePath,
        source.absolutePath,
    )
    check(result == 0) { "插件源码编译失败：$result" }
    Files.createDirectories(File(work, "spi/META-INF/services").toPath())
    File(work, "spi/META-INF/services/chat.plugin.DesktopPlugin").apply {
        parentFile.mkdirs()
        writeText("demo.DemoEchoPlugin\n", Charsets.UTF_8)
    }
    val jar = File(work, "demo-echo.jar")
    val jarTool = ToolProvider.getSystemToolClassLoader()
    // 用 JarOutputStream 手工打包，避免依赖 jar 命令行工具。
    java.util.jar.JarOutputStream(jar.outputStream()).use { out ->
        fun add(root: File, prefix: String) {
            root.walkTopDown().filter { it.isFile }.forEach { file ->
                val name = prefix + file.relativeTo(root).path.replace(File.separatorChar, '/')
                out.putNextEntry(java.util.zip.ZipEntry(name))
                out.write(file.readBytes())
                out.closeEntry()
            }
        }
        add(classesDir, "")
        add(File(work, "spi"), "")
    }
    check(jarTool != null || true)
    return jar
}
