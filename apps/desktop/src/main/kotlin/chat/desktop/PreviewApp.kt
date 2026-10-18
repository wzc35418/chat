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
import chat.api.LocalStoreException
import chat.bridge.createIdentityService
import chat.bridge.createLocalStore
import chat.presentation.display.presentInviteError
import chat.presentation.display.presentInvitePreview
import chat.presentation.preview.PreviewIdentityState
import chat.presentation.preview.PreviewStoreState
import chat.presentation.preview.beginBusy
import chat.presentation.preview.opened
import chat.presentation.preview.openWith
import chat.presentation.preview.selecting
import chat.presentation.preview.unavailable
import chat.presentation.preview.updatePastedToken
import chat.presentation.preview.withContacts
import chat.presentation.preview.withConversations
import chat.presentation.preview.withDraft
import chat.presentation.preview.withDraftStatus
import chat.presentation.preview.withError
import chat.presentation.preview.withInvite
import chat.presentation.preview.withMessages
import chat.presentation.preview.withPreview
import chat.ui.IdentityHandlers
import chat.ui.PreviewChatScreen
import chat.ui.StoreHandlers
import kotlinx.coroutines.launch
import java.io.File
import java.nio.file.Files
import kotlin.random.Random

/**
 * 正式聊天界面的独立交互预览入口。
 *
 * 三条并行链路：
 * - 样例数据：联系人/消息/回执仍是显式样例，用于观察布局与状态；
 * - 真实身份：账户/指纹/inv1 口令/联系人来自 Rust identity（本机），口令需人工复制；
 * - 本机存储：Rust formal store 的真实加密 SQLite（本次运行临时库，退出即删），可写入真实草稿。
 */
fun main() = application {
    var identityState by remember { mutableStateOf(PreviewIdentityState(busy = true)) }
    var storeState by remember { mutableStateOf(PreviewStoreState(busy = true)) }
    var storeDraftText by remember { mutableStateOf("") }
    var selectedConversation by remember { mutableStateOf<String?>(null) }
    val identity = remember { runCatching { createIdentityService() }.getOrNull() }
    // 持久身份库：演示用固定目录（build/preview-identity），主密钥首次生成后复用，
    // 因此重启预览仍显示同一指纹。正式产品应由平台密钥库保护（P0-04 未完成部分）。
    val identityDir = remember {
        runCatching { File("build/preview-identity").apply { mkdirs() } }.getOrNull()
    }
    val identityDatabase = remember { identityDir?.let { File(it, "identity.db").absolutePath } }
    val identityKey = remember {
        identityDir?.let { dir ->
            val keyFile = File(dir, "demo-master-key.hex")
            if (keyFile.exists()) {
                keyFile.readText().trim()
            } else {
                val bytes = ByteArray(32).also { java.security.SecureRandom().nextBytes(it) }
                val hex = bytes.joinToString("") { "%02x".format(it) }
                keyFile.writeText(hex)
                hex
            }
        }
    }
    val store = remember { runCatching { createLocalStore() }.getOrNull() }
    val storeDirectory = remember { runCatching { Files.createTempDirectory("chat-preview-store-").toFile() }.getOrNull() }
    val scope = rememberCoroutineScope()
    val node = "node:1:" + "0a".repeat(32)
    // 演示库密钥只在本次进程内存中；退出即删除库文件，避免弱口令派生。
    val storeKey = remember { Random.nextBytes(32).joinToString("") { "%02x".format(it) } }
    val storePath = remember { storeDirectory?.let { File(it, "preview.db").absolutePath } }
    val demoConversation = "conv:1:" + "aa".repeat(32)

    suspend fun refreshContacts() {
        if (identity == null) return
        runCatching { identity.contacts() }
            .onSuccess { identityState = identityState.withContacts(it) }
            .onFailure { error -> identityState = identityState.unavailable(error.message ?: "联系人读取失败") }
    }

    suspend fun refreshConversations() {
        val local = store ?: return
        storeState = storeState.beginBusy()
        runCatching { local.listConversations(null, 50) }
            .onSuccess { storeState = storeState.withConversations(it.nextCursor, it.items) }
            .onFailure { error ->
                storeState = storeState.withError(
                    (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                    error.message,
                )
            }
    }

    suspend fun refreshMessages(conversationId: String) {
        val local = store ?: return
        runCatching { local.listMessages(conversationId, null, 50) }
            .onSuccess { storeState = storeState.withMessages(conversationId, it.nextCursor, it.items) }
            .onFailure { error ->
                storeState = storeState.withError(
                    (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                    error.message,
                )
            }
    }

    LaunchedEffect(Unit) {
        if (identity == null || identityDatabase == null || identityKey == null) {
            identityState = identityState.unavailable("原生身份服务未加载")
        } else {
            runCatching { identity.openPersisted(identityDatabase, identityKey, node) }
                .onSuccess { account ->
                    identityState = identityState.openWith(account)
                    identityState = identityState.copy(
                        notice = "已从本机加密库恢复/创建身份；重启预览仍是同一指纹。口令需要人工复制，节点投递未接。",
                    )
                    refreshContacts()
                }
                .onFailure { error ->
                    identityState = identityState.unavailable(
                        (error as? IdentityException)?.code ?: (error.message ?: "身份创建失败"),
                    )
                }
        }

        if (store == null || storePath == null) {
            storeState = storeState.withError("CORE_CLOSED", "本机存储服务未加载")
        } else {
            runCatching { store.open(storePath, storeKey) }
                .onSuccess {
                    storeState = storeState.opened(storePath, "已打开本次运行的真实加密库；会话为空属正常（发送链路未接入）。")
                    refreshConversations()
                }
                .onFailure { error ->
                    storeState = storeState.withError(
                        (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                        error.message,
                    )
                }
        }
    }

    val identityHandlers = IdentityHandlers(
        generateInvite = {
            scope.launch {
                if (identity == null) return@launch
                identityState = identityState.beginBusy()
                runCatching { identity.createInvite(24L * 60 * 60 * 1000) }
                    .onSuccess { identityState = identityState.withInvite(it) }
                    .onFailure { error ->
                        identityState = identityState.withError((error as? IdentityException)?.code ?: "UNKNOWN_RESULT")
                    }
            }
        },
        updatePastedToken = { value -> identityState = identityState.updatePastedToken(value) },
        previewInvite = {
            scope.launch {
                if (identity == null) return@launch
                identityState = identityState.beginBusy()
                runCatching { identity.previewInvite(identityState.pastedToken.trim()) }
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
                if (identity == null) return@launch
                identityState = identityState.beginBusy()
                runCatching { identity.acceptInvite(identityState.pastedToken.trim(), "对方") }
                    .onSuccess { identityState = identityState.withContacts(identity.contacts()) }
                    .onFailure { error ->
                        identityState = identityState.withError((error as? IdentityException)?.code ?: "UNKNOWN_RESULT")
                    }
            }
        },
        refreshContacts = { scope.launch { refreshContacts() } },
    )

    val storeHandlers = StoreHandlers(
        refreshConversations = { scope.launch { refreshConversations() } },
        selectConversation = { id ->
            selectedConversation = id
            storeState = storeState.selecting(id)
            scope.launch { refreshMessages(id) }
        },
        refreshMessages = {
            val id = selectedConversation
            if (id != null) scope.launch { refreshMessages(id) }
        },
        updateDraftText = { value -> storeDraftText = value.take(8 * 1024) },
        createDraft = {
            scope.launch {
                if (store == null) return@launch
                storeState = storeState.beginBusy()
                val operationId = "op:1:" + Random.nextBytes(32).joinToString("") { "%02x".format(it) }
                runCatching { store.createDraft(demoConversation, operationId, storeDraftText) }
                    .onSuccess { draft ->
                        storeState = storeState.withDraft(draft)
                        storeDraftText = ""
                        // 立刻用 CORE-010 查一次真实状态（不含正文，展示按钮态）
                        runCatching { store.draftStatusView(draft.draftId) }
                            .onSuccess { status ->
                                if (status != null) storeState = storeState.withDraftStatus(status)
                            }
                    }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
    )

    Window(
        title = "Chat · 交互预览（样例消息 + 真实身份 + 本机存储）",
        state = rememberWindowState(width = 1100.dp, height = 800.dp),
        onCloseRequest = {
            kotlinx.coroutines.runBlocking {
                runCatching { identity?.close() }
                runCatching { store?.close() }
            }
            storeDirectory?.deleteRecursively()
            exitApplication()
        },
    ) {
        PreviewChatScreen(
            identityState = identityState,
            identityHandlers = identityHandlers,
            storeState = storeState,
            storeHandlers = storeHandlers,
            storeDraftText = storeDraftText,
        )
    }
}
