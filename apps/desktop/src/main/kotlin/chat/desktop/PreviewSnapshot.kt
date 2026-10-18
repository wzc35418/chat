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
import chat.api.IdentityAccount
import chat.bridge.createIdentityService
import chat.bridge.createLocalStore
import chat.presentation.preview.PreviewStoreState
import chat.presentation.preview.opened
import chat.presentation.preview.withConversations
import chat.presentation.preview.withDraft
import chat.presentation.preview.withError
import chat.presentation.display.presentContact
import chat.presentation.display.presentInvitePreview
import chat.presentation.preview.PreviewIdentityState
import chat.presentation.preview.openWith
import chat.presentation.preview.samplePreviewState
import chat.presentation.preview.updateInviteText
import chat.presentation.preview.validateInvite
import chat.presentation.preview.unavailable
import chat.presentation.preview.withContacts
import chat.presentation.preview.withInvite
import chat.presentation.preview.withPreview
import chat.ui.PreviewChatScreen
import kotlinx.coroutines.runBlocking
import org.jetbrains.skia.EncodedImageFormat
import java.io.File
import java.nio.file.Files
import kotlin.random.Random

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

    val realIdentity = realIdentityState()
    println("real identity state ready=" + realIdentity.ready + " contacts=" + realIdentity.contacts.size)
    val realStore = realStoreState()
    println("real store state ready=" + realStore.ready + " drafts=" + realStore.drafts.size)
    val identityRenders = listOf(
        Render(
            "preview-desktop-real-identity",
            1024,
            860,
            base,
            identityState = realIdentity,
            identityOpen = true,
        ),
        Render(
            "preview-compact-real-identity",
            390,
            844,
            base,
            identityState = realIdentity,
            identityOpen = true,
        ),
    )

    val storeRenders = listOf(
        Render(
            "preview-desktop-local-store",
            1024,
            860,
            base,
            storeState = realStore,
            storeOpen = true,
        ),
    )

    for (render in renders + identityRenders + storeRenders) {
        renderScene(render)
    }
}

/**
 * 用**真实身份服务**构造面板状态：两个独立身份互相校验口令。
 * 任何一步失败都退回“服务不可用”的显式状态，不伪造指纹或口令。
 */
/**
 * 用真实 formal store 构造“本机存储”面板状态：临时加密库、真实草稿写入、真实会话分页。
 * 任何一步失败都退回显式错误状态，不伪造数据。
 */
private fun realStoreState(): PreviewStoreState {
    val directory = runCatching { Files.createTempDirectory("chat-preview-store-snap-").toFile() }.getOrNull()
        ?: return PreviewStoreState().withError("STORAGE_FAILURE", "无法创建临时目录")
    val key = Random.nextBytes(32).joinToString("") { "%02x".format(it) }
    val path = File(directory, "snapshot.db").absolutePath
    return runCatching {
        runBlocking {
            val store = createLocalStore()
            try {
                store.open(path, key)
                var state = PreviewStoreState().opened(path, "截图用真实加密库（临时，渲染后删除）。")
                val conversations = store.listConversations(null, 50)
                state = state.withConversations(conversations.nextCursor, conversations.items)
                val operationId = "op:1:" + Random.nextBytes(32).joinToString("") { "%02x".format(it) }
                val draft = store.createDraft("conv:1:" + "aa".repeat(32), operationId, "## 截图草稿\n本机真实写入，未发送。")
                state.withDraft(draft)
            } finally {
                runCatching { store.close() }
                directory.deleteRecursively()
            }
        }
    }.getOrElse { error -> PreviewStoreState().withError("UNKNOWN_RESULT", error.message) }
}

private fun realIdentityState(): PreviewIdentityState {
    val node = "node:1:" + "0a".repeat(32)
    return runCatching {
        runBlocking {
            val alice = createIdentityService()
            val bob = createIdentityService()
            try {
                val accountA = alice.create("本机账户", node)
                bob.create("对方账户", node)
                val inviteA = alice.createInvite(24L * 60 * 60 * 1000)
                val inviteB = bob.createInvite(24L * 60 * 60 * 1000)
                val summary = alice.previewInvite(inviteB.token)
                val contact = alice.acceptInvite(inviteB.token, "对方")
                var state = PreviewIdentityState().openWith(accountA)
                state = state.withInvite(inviteA)
                state = state.withPreview(presentInvitePreview(summary, System.currentTimeMillis()))
                state = state.copy(pastedToken = inviteB.token)
                state = state.withContacts(listOf(contact))
                state
            } finally {
                runCatching { alice.close() }
                runCatching { bob.close() }
            }
        }
    }.getOrElse { error -> PreviewIdentityState().unavailable(error.message ?: "真实身份服务不可用") }
}

private data class Render(
    val name: String,
    val width: Int,
    val height: Int,
    val state: PreviewChatState,
    val fontScale: Float = 1f,
    val identityState: PreviewIdentityState = PreviewIdentityState(),
    val identityOpen: Boolean = false,
    val storeState: PreviewStoreState = PreviewStoreState(),
    val storeOpen: Boolean = false,
)

@OptIn(ExperimentalComposeUiApi::class)
private fun renderScene(render: Render) {
    val scene = ImageComposeScene(render.width, render.height, Density(1f, render.fontScale)) {
        PreviewChatScreen(
            initialState = render.state,
            identityState = render.identityState,
            initialIdentityOpen = render.identityOpen,
            storeState = render.storeState,
            initialStoreOpen = render.storeOpen,
        )
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
