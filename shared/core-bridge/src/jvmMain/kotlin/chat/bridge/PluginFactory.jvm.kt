package chat.bridge

import chat.api.PluginService
import uniffi.chat_ffi.PluginsFfi

actual fun createPluginService(): PluginService = NativePluginService(PluginsFfi())
