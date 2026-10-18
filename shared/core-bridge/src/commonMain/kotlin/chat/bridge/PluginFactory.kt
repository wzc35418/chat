package chat.bridge

import chat.api.PluginService

/** 创建纯内存插件宿主（不落库；重启后状态消失——已在文档标注）。 */
expect fun createPluginService(): PluginService
