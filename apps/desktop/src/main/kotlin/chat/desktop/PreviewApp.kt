package chat.desktop

import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import chat.ui.PreviewChatScreen

/**
 * 正式聊天界面的独立交互预览入口。
 * 与 chat.desktop.MainKt 的 P0 真链路分开启动，全部使用样例数据。
 */
fun main() = application {
    Window(
        title = "Chat · 交互预览（样例数据）",
        state = rememberWindowState(width = 1100.dp, height = 780.dp),
        onCloseRequest = ::exitApplication,
    ) {
        PreviewChatScreen()
    }
}
