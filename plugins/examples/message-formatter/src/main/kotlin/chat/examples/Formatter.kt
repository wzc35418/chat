package chat.examples

import chat.plugin.*

fun quoteSelectedText(text: String): String = text.lineSequence().joinToString("\n") { "> $it" }

class FormatterPlugin : DesktopPlugin {
    override val id = "dev.example.message-formatter"
    private var registration: Registration? = null
    override fun start(context: PluginContext) {
        check(registration == null)
        registration = context.registerSelectedTextAction("quote", ::quoteSelectedText)
    }
    override fun stop() { registration?.dispose(); registration = null }
}
