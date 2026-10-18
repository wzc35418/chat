package chat.examples

import chat.plugin.*
class HelloPlugin : DesktopPlugin {
    override val id = "dev.example.hello-command"
    private var registration: Registration? = null
    override fun start(context: PluginContext) {
        check(registration == null)
        registration = context.registerCommand("hello") { "你好，这是待确认的草稿。" }
    }
    override fun stop() { registration?.dispose(); registration = null }
}
