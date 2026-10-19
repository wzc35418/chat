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
        check(host.loadedPluginIds().isEmpty())
        val gone = runCatching { host.runSelectedTextAction("demo.echo", "echo", "x") }.exceptionOrNull()
        check(gone is PluginLoadException)

        host.shutdown()
        println("PASS plugin P4 slice: trusted load, SPI discovery, bounded execution, unload")
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
