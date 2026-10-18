package chat.desktop

import androidx.compose.runtime.*
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import chat.bridge.createP0Core
import chat.presentation.P0Presenter
import chat.ui.P0Screen
import kotlinx.coroutines.launch

fun main() = application {
    val presenter = remember { P0Presenter(createP0Core()) }
    val scope = rememberCoroutineScope()
    var closing by remember { mutableStateOf(false) }
    DisposableEffect(presenter) { onDispose { presenter.dispose() } }
    Window(title = "Chat · 加密通信实验室", state = rememberWindowState(width = 1024.dp, height = 860.dp),
        onCloseRequest = {
            if (!closing) {
                closing = true
                scope.launch { try { presenter.close() } finally { exitApplication() } }
            }
        }) {
        LaunchedEffect(Unit) {
            window.toFront()
            window.requestFocus()
            println("Chat window is ready. Gradle stays at :apps:desktop:run until you close it.")
        }
        P0Screen(presenter)
    }
}
