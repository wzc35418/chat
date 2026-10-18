package chat.desktop

import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.unit.Density
import chat.presentation.preview.PreviewChatState
import chat.presentation.preview.PreviewScenario
import chat.presentation.preview.advanceDraftSample
import chat.presentation.preview.advancePagingSample
import chat.presentation.preview.applyScenario
import chat.presentation.preview.fillSampleInviteToken
import chat.presentation.preview.openInvite
import chat.presentation.preview.samplePreviewState
import chat.presentation.preview.updateInviteText
import chat.presentation.preview.validateInvite
import chat.ui.PreviewChatScreen
import org.jetbrains.skia.EncodedImageFormat
import java.io.File

/**
 * DS-03/DS-06/DS-07 预览壳的离屏截图入口，与 P0 真链路分开运行。
 * 输出只包含显式样例数据。
 */
@OptIn(ExperimentalComposeUiApi::class)
fun main() {
    val base = samplePreviewState()
    val renders = listOf(
        Render("preview-desktop-accepted", 1024, 860, base),
        Render(
            "preview-desktop-identity-changed",
            1024,
            860,
            base.applyScenario(PreviewScenario.IDENTITY_CHANGED),
        ),
        Render(
            "preview-desktop-invite-warning",
            1024,
            860,
            base.openInvite().updateInviteText("任意输入不会进入身份页").validateInvite(),
        ),
        Render(
            "preview-desktop-invite-identity",
            1024,
            860,
            base.openInvite().fillSampleInviteToken().validateInvite(),
        ),
        Render("preview-compact-chat", 390, 844, base),
        Render("preview-compact-list", 390, 844, base.applyScenario(PreviewScenario.EMPTY)),
        Render("preview-compact-locked", 390, 844, base.applyScenario(PreviewScenario.LOCKED)),
        Render("preview-desktop-large-font", 1024, 860, base, fontScale = 1.3f),
        Render("preview-compact-large-font", 390, 844, base, fontScale = 1.3f),
        Render("preview-desktop-short", 1024, 520, base),
        // DS-11：分页提示的失败与到底两种可见状态（合并器真实执行）。
        Render("preview-desktop-paging-failed", 1024, 860, base.advancePagingSample().advancePagingSample()),
        Render(
            "preview-desktop-paging-no-more",
            1024,
            860,
            base.advancePagingSample().advancePagingSample().advancePagingSample(),
        ),
        Render("preview-compact-paging-failed", 390, 844, base.advancePagingSample().advancePagingSample()),
        // DS-10：发送状态样例中的“结果待对账”和“不可重试的失败”。
        Render("preview-desktop-draft-unknown", 1024, 860, base.advanceDraftSample().advanceDraftSample()),
        Render(
            "preview-desktop-draft-blocked",
            1024,
            860,
            base.advanceDraftSample()
                .advanceDraftSample()
                .advanceDraftSample()
                .advanceDraftSample(),
        ),
        Render(
            "preview-compact-draft-delivered",
            390,
            844,
            base.advanceDraftSample()
                .advanceDraftSample()
                .advanceDraftSample()
                .advanceDraftSample()
                .advanceDraftSample(),
        ),
    )

    for (render in renders) {
        renderScene(render)
    }
}

private data class Render(
    val name: String,
    val width: Int,
    val height: Int,
    val state: PreviewChatState,
    val fontScale: Float = 1f,
)

@OptIn(ExperimentalComposeUiApi::class)
private fun renderScene(render: Render) {
    val scene = ImageComposeScene(render.width, render.height, Density(1f, render.fontScale)) {
        PreviewChatScreen(initialState = render.state)
    }
    try {
        val image = scene.render()
        try {
            val data = checkNotNull(image.encodeToData(EncodedImageFormat.PNG))
            try {
                val file = File("build/preview-snapshots/${render.name}.png")
                file.parentFile.mkdirs()
                file.writeBytes(data.bytes)
                println("PASS rendered preview: ${file.absolutePath}")
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
