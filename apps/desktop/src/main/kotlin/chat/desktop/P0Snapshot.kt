package chat.desktop

import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.unit.Density
import chat.api.P0Core
import chat.api.P0Delivery
import chat.api.P0SessionInfo
import chat.bridge.createP0Core
import chat.presentation.P0Presenter
import chat.ui.P0Screen
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.jetbrains.skia.EncodedImageFormat
import java.io.File

private data class P0Shot(
    val name: String,
    val width: Int,
    val height: Int,
    val fontScale: Float = 1f,
)

private class SendFailureCore : P0Core {
    override suspend fun connect(): P0SessionInfo = P0SessionInfo(
        aliceEndpoint = "alice-rebuild-sample",
        bobEndpoint = "bob-rebuild-sample",
        groupId = "group-rebuild-sample",
        epoch = 1,
        ciphersuite = "MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519",
        transport = "iroh / loopback（样例）",
        publicInfrastructure = false,
    )

    override suspend fun send(sender: String, text: String): P0Delivery =
        throw IllegalStateException("样例失败：不进入 UI")

    override suspend fun close() = Unit
}

@OptIn(ExperimentalComposeUiApi::class)
fun main() = runBlocking {
    val presenter = P0Presenter(createP0Core())
    try {
        presenter.connect()
        withTimeout(35_000) { presenter.state.first { !it.busy && it.session != null } }
        presenter.send("Alice", "你好，Bob！这条消息已经加密 👋")
        withTimeout(15_000) { presenter.state.first { !it.busy && it.messages.size == 1 } }
        presenter.send("Bob", "收到了，Alice。我的设备已经成功解密。")
        withTimeout(15_000) { presenter.state.first { !it.busy && it.messages.size == 2 } }

        renderShots(
            presenter = presenter,
            shots = listOf(
                P0Shot("desktop", 1024, 860),
                P0Shot("compact", 390, 844),
                P0Shot("desktop-large-font", 1024, 860, fontScale = 1.3f),
                P0Shot("compact-large-font", 390, 844, fontScale = 1.3f),
                P0Shot("desktop-short", 1024, 520),
            ),
        )
    } finally {
        presenter.close()
        presenter.dispose()
    }

    val rebuildPresenter = P0Presenter(SendFailureCore())
    try {
        rebuildPresenter.connect()
        withTimeout(5_000) { rebuildPresenter.state.first { !it.busy && it.canSend } }
        rebuildPresenter.updateComposer("结果未知时保留的草稿")
        rebuildPresenter.send()
        withTimeout(5_000) { rebuildPresenter.state.first { !it.busy && it.requiresRebuild } }
        renderShots(
            presenter = rebuildPresenter,
            shots = listOf(
                P0Shot("desktop-rebuild-required", 1024, 860),
                P0Shot("compact-rebuild-required", 390, 844),
            ),
        )
    } finally {
        rebuildPresenter.close()
        rebuildPresenter.dispose()
    }
}

@OptIn(ExperimentalComposeUiApi::class)
private fun renderShots(presenter: P0Presenter, shots: List<P0Shot>) {
    for (shot in shots) {
        val scene = ImageComposeScene(shot.width, shot.height, Density(1f, shot.fontScale)) { P0Screen(presenter) }
        try {
            val image = scene.render()
            try {
                val data = checkNotNull(image.encodeToData(EncodedImageFormat.PNG))
                try {
                    val file = File("build/p0-snapshots/${shot.name}.png")
                    file.parentFile.mkdirs()
                    file.writeBytes(data.bytes)
                    println("PASS rendered actual Compose UI: ${file.absolutePath}")
                } finally {
                    data.close()
                }
            } finally {
                image.close()
            }
        } finally {
            scene.close()
        }
    }
}
