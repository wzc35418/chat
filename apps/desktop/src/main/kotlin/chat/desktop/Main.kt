package chat.desktop

import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import chat.bridge.createP0Core
import chat.presentation.P0Presenter
import chat.ui.P0Screen

/**
 * 应用入口。
 *
 * 默认打开**正式聊天界面**（ChatAppRoot）；P0 实验室只在传入 `--p0` 时打开，
 * 避免用户被开发用界面误导。
 */
fun main(args: Array<String>) {
    val showP0 = args.contains("--p0")
    application {
        if (showP0) {
            val presenter = remember { P0Presenter(createP0Core()) }
            Window(
                title = "Chat · P0 实验室",
                state = rememberWindowState(width = 1024.dp, height = 860.dp),
                onCloseRequest = {
                    runCatching { presenter.dispose() }
                    exitApplication()
                },
            ) {
                P0Screen(presenter)
            }
        } else {
            Window(
                title = "Chat",
                state = rememberWindowState(width = 1180.dp, height = 800.dp),
                onCloseRequest = { exitApplication() },
            ) {
                LaunchedEffect(Unit) {
                    window.toFront()
                    window.requestFocus()
                }
                ChatAppRoot()
            }
        }
    }
}
