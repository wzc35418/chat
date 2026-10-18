package chat.desktop

import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import chat.api.IdentityException
import chat.bridge.createIdentityService
import chat.presentation.display.presentInviteError
import chat.presentation.display.presentInvitePreview
import chat.presentation.preview.PreviewIdentityState
import chat.presentation.preview.beginBusy
import chat.presentation.preview.openWith
import chat.presentation.preview.unavailable
import chat.presentation.preview.updatePastedToken
import chat.presentation.preview.withContacts
import chat.presentation.preview.withError
import chat.presentation.preview.withInvite
import chat.presentation.preview.withPreview
import chat.ui.IdentityHandlers
import chat.ui.PreviewChatScreen
import kotlinx.coroutines.launch

/**
 * 正式聊天界面的独立交互预览入口。
 *
 * 联系人/消息/回执仍是显式样例；同时接入**真实身份服务**（本机 Rust identity）：
 * 账户/指纹/一次性口令/联系人状态都来自真实密码学，口令需要人工复制，节点投递尚未接入。
 */
fun main() = application {
    var identityState by remember { mutableStateOf(PreviewIdentityState(busy = true)) }
    val service = remember { runCatching { createIdentityService() }.getOrNull() }
    val scope = rememberCoroutineScope()
    // 两个身份对象必须使用同一个规范 node 标识（inv1 口令绑定它）。
    val node = "node:1:" + "0a".repeat(32)

    suspend fun refreshContacts() {
        if (service == null) return
        runCatching { service.contacts() }
            .onSuccess { identityState = identityState.withContacts(it) }
            .onFailure { error -> identityState = identityState.unavailable(error.message ?: "联系人读取失败") }
    }

    LaunchedEffect(Unit) {
        if (service == null) {
            identityState = identityState.unavailable("原生身份服务未加载")
            return@LaunchedEffect
        }
        runCatching { service.create("本机账户", node) }
            .onSuccess { account ->
                identityState = identityState.openWith(account)
                refreshContacts()
            }
            .onFailure { error ->
                identityState = identityState.unavailable(
                    (error as? IdentityException)?.code ?: (error.message ?: "身份创建失败"),
                )
            }
    }

    val handlers = IdentityHandlers(
        generateInvite = {
            scope.launch {
                if (service == null) return@launch
                identityState = identityState.beginBusy()
                runCatching { service.createInvite(24L * 60 * 60 * 1000) }
                    .onSuccess { identityState = identityState.withInvite(it) }
                    .onFailure { error ->
                        identityState = identityState.withError((error as? IdentityException)?.code ?: "UNKNOWN_RESULT")
                    }
            }
        },
        updatePastedToken = { value -> identityState = identityState.updatePastedToken(value) },
        previewInvite = {
            scope.launch {
                if (service == null) return@launch
                identityState = identityState.beginBusy()
                runCatching { service.previewInvite(identityState.pastedToken.trim()) }
                    .onSuccess { summary ->
                        identityState = identityState.withPreview(
                            presentInvitePreview(summary, System.currentTimeMillis()),
                        )
                    }
                    .onFailure { error ->
                        val exception = error as? IdentityException
                        identityState = identityState.withError(
                            code = exception?.code ?: "UNKNOWN_RESULT",
                            preview = presentInviteError(exception?.code ?: "UNKNOWN_RESULT", exception?.message),
                        )
                    }
            }
        },
        acceptInvite = {
            scope.launch {
                if (service == null) return@launch
                identityState = identityState.beginBusy()
                runCatching { service.acceptInvite(identityState.pastedToken.trim(), "对方") }
                    .onSuccess {
                        identityState = identityState.withContacts(service.contacts())
                    }
                    .onFailure { error ->
                        identityState = identityState.withError((error as? IdentityException)?.code ?: "UNKNOWN_RESULT")
                    }
            }
        },
        refreshContacts = { scope.launch { refreshContacts() } },
    )

    Window(
        title = "Chat · 交互预览（样例消息 + 真实身份）",
        state = rememberWindowState(width = 1100.dp, height = 780.dp),
        onCloseRequest = {
            runCatching { kotlinx.coroutines.runBlocking { service?.close() } }
            exitApplication()
        },
    ) {
        PreviewChatScreen(
            identityState = identityState,
            identityHandlers = handlers,
        )
    }
}
