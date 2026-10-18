package chat.plugin

interface Registration { fun dispose() }
interface DesktopPlugin {
    val id: String
    fun start(context: PluginContext)
    fun stop()
}
interface PluginContext {
    // SDK 草案：handler 的结果只能成为待用户确认的草稿，宿主接入在 P4。
    fun registerCommand(name: String, action: () -> String): Registration
    fun registerSelectedTextAction(name: String, action: (String) -> String): Registration
}
