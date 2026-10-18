package chat.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import chat.bridge.createP0Core
import chat.presentation.P0Presenter
import chat.ui.P0Screen

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            val presenter = remember { P0Presenter(createP0Core()) }
            DisposableEffect(presenter) { onDispose { presenter.dispose() } }
            P0Screen(presenter)
        }
    }
}
