package chat.desktop

import chat.bridge.createPluginService
import chat.api.LocalStoreException
import kotlinx.coroutines.runBlocking

/**
 * PLUGINS 竖直冒烟：清单校验 → 安装 → 启动 → 派发计划 → 私有存储 → 停止 → 卸载。
 *
 * 诚实边界：本冒烟**不加载也不执行插件代码**；它断言的是宿主状态机与计划字段，
 * 以及错误码在 Kotlin 侧可见（不把失败吞成成功）。
 */
private const val MANIFEST = """
{
  "schemaVersion": 1,
  "id": "notes.v1",
  "version": "1.2.3",
  "sdkVersion": 1,
  "platform": "desktop",
  "capabilities": ["PrivateStorage", "Command"],
  "permissions": ["UserSelectedContent"],
  "extensionPoints": ["Command"],
  "entryClass": "com.example.notes.NotesPlugin"
}
"""

fun main() = runBlocking {
    val service = createPluginService()
    try {
        // 1) 清单校验：不安装。
        val manifest = service.validateManifest(MANIFEST)
        check(manifest.id == "notes.v1") { "id 解析错误: " + manifest.id }
        check(manifest.version == "1.2.3")
        check(manifest.entryClass == "com.example.notes.NotesPlugin")
        check(service.listPlugins().isEmpty()) { "校验不应安装插件" }

        // 2) 安装 + 启动。
        service.install(MANIFEST)
        val installed = service.listPlugins()
        check(installed.size == 1 && installed.first().id == "notes.v1")
        service.start("notes.v1")
        val started = service.listPlugins().first()
        check(started.state == "started") { "启动后状态应为 started，实际 " + started.state }

        // 3) 派发计划：只返回计划，不执行插件代码。
        val plan = service.dispatch("notes.v1", "Command", "ping")
        check(plan.pluginId == "notes.v1" && plan.extensionPoint == "Command")
        check(plan.operationId.isNotBlank())
        check(plan.inputBytes == 4) { "输入字节数应为 4，实际 " + plan.inputBytes }
        check(plan.outputLimitBytes > 0 && plan.timeoutMillis > 0)
        service.finishDispatch(plan.operationId, "pong")

        // 4) 幂等：同 operation 同输入重复 finish 不报错；不同输入必须显式冲突。
        service.finishDispatch(plan.operationId, "pong")

        // 5) 私有存储：能力已声明 → 可写可读。
        service.storageSet("notes.v1", "probe", "value-1")
        check(service.storageGet("notes.v1", "probe") == "value-1")

        // 6) 停止 → 派发必须显式失败（不是静默成功）。
        service.stop("notes.v1")
        val afterStop = service.listPlugins().first()
        check(afterStop.state != "started") { "停止后状态不应是 started" }
        val stoppedError = runCatching { service.dispatch("notes.v1", "Command", "ping") }.exceptionOrNull()
        check(stoppedError is LocalStoreException) { "停止后派发必须显式失败" }
        println("      停止后派发错误码：" + (stoppedError as LocalStoreException).code)

        // 7) 未声明扩展点 → 显式错误码。
        service.start("notes.v1")
        val unknownPoint = runCatching { service.dispatch("notes.v1", "ComposerAction", "ping") }.exceptionOrNull()
        check(unknownPoint is LocalStoreException)
        println("      未知扩展点错误码：" + (unknownPoint as LocalStoreException).code)

        // 8) 卸载（保留私有存储）。
        service.uninstall("notes.v1", false)
        check(service.listPlugins().isEmpty()) { "卸载后列表应为空" }

        println("PASS plugin slice: validate/install/start/plan/finish/storage/stop/uninstall")
    } finally {
        service.shutdown()
    }
}
