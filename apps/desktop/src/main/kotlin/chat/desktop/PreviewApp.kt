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
import chat.bridge.createAttachmentService
import chat.bridge.createGroupService
import chat.bridge.createSyncService
import chat.bridge.createIdentityService
import chat.bridge.createLocalStore
import chat.presentation.display.presentInviteError
import chat.presentation.display.presentInvitePreview
import chat.presentation.preview.PreviewAttachmentState
import chat.presentation.preview.PreviewGroupState
import chat.presentation.preview.beginBusy
import chat.presentation.preview.updateCreateName
import chat.presentation.preview.updateNewName
import chat.presentation.preview.withError
import chat.presentation.preview.withSnapshot
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
import chat.presentation.preview.updateExportPath
import chat.presentation.preview.updateImportPath
import chat.presentation.preview.updateSendNodeId
import chat.presentation.preview.updateSendNodePublicKey
import chat.presentation.preview.updateSendNodeUrl
import chat.presentation.preview.withSendStep
import chat.presentation.preview.withSyncStatus
import chat.presentation.preview.withCancel
import chat.presentation.preview.withDraftStatus
import chat.presentation.preview.withExported
import chat.presentation.preview.withImported
import chat.presentation.preview.withUpdated
import chat.presentation.preview.withEvents
import chat.presentation.preview.withError
import chat.presentation.preview.withInvite
import chat.presentation.preview.withMessages
import chat.presentation.preview.withPreview
import chat.ui.AttachmentHandlers
import chat.ui.GroupHandlers
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
    var identityKey by remember { mutableStateOf<String?>(null) }
    var keySlotSummary by remember { mutableStateOf("密钥槽未初始化") }
    var groupState by remember { mutableStateOf(PreviewGroupState()) }
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

    // PLATFORM-001/002 接线：主密钥由桌面密钥槽封装（口令派生 AES-256-GCM），
    // 磁盘只留盐与密文；启动时 unwrap，首次运行生成并 wrap。演示口令固定，
    // 正式产品应改为用户输入或操作系统密钥链（未接线，如实标注）。
    LaunchedEffect(Unit) {
        if (identityDir == null) {
            keySlotSummary = "密钥槽不可用（目录创建失败）"
            return@LaunchedEffect
        }
        val slot = PassphraseKeySlot(
            File(identityDir, "keyslot").apply { mkdirs() },
            DEMO_KEY_PASSPHRASE.toCharArray(),
        )
        keySlotSummary = slot.protectionSummary()
        val alias = "storage-main"
        val restored = runCatching {
            val buffer = slot.unwrapByAlias(alias)
            try {
                buffer.copyBytes().joinToString("") { byte -> "%02x".format(byte) }
            } finally {
                buffer.close()
            }
        }
        if (restored.isSuccess) {
            identityKey = restored.getOrThrow()
        } else {
            val fresh = ByteArray(32).also { java.security.SecureRandom().nextBytes(it) }
            runCatching {
                slot.wrap(alias, chat.platform.SecretBuffer(fresh))
                fresh.joinToString("") { "%02x".format(it) }
            }.getOrElse { error ->
                keySlotSummary = "密钥封装失败：" + ((error as? chat.platform.KeyProtectionException)?.code ?: "UNKNOWN")
                null
            }
        }
    }

    LaunchedEffect(identityKey) {
        val database = identityDatabase
        val opened = identityKey
        if (identity == null || database == null || opened == null) {
            identityState = identityState.unavailable("原生身份服务未加载")
        } else {
            runCatching { identity.openPersisted(database, opened, node) }
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

    // 群与身份共用同一个加密库（独占单写者）：进入群操作前先关闭身份服务，
    // 操作完再重新打开身份，避免两个 FFI 对象争同一把写锁。
    suspend fun <T> withGroup(block: suspend (chat.api.GroupService) -> T): Result<T> {
        val database = identityDatabase
        val key = identityKey
        if (database == null || key == null) {
            return Result.failure(LocalStoreException("INVALID_STATE_TRANSITION", "身份库尚未就绪"))
        }
        val identityService = identity
        runCatching { identityService?.close() }
        val service = runCatching { createGroupService(database, key, node) }
        return try {
            service.mapCatching { block(it) }
        } finally {
            service.getOrNull()?.let { runCatching { it.shutdown() } }
            if (identityService != null) {
                runCatching { identityService.openPersisted(database, key, node) }
                    .onSuccess { account ->
                        identityState = identityState.openWith(account)
                        refreshContacts()
                    }
            }
        }
    }

    var syncService by remember { mutableStateOf<chat.api.SyncService?>(null) }

    suspend fun ensureSyncService(): chat.api.SyncService? {
        syncService?.let { return it }
        val database = identityDatabase
        val key = identityKey
        if (database == null || key == null) return null
        runCatching { identity?.close() }
        val created = runCatching {
            createSyncService(database, key, "http://127.0.0.1:8080", node, "", demoConversation)
        }.getOrNull()
        runCatching { identity?.openPersisted(database, key, node) }
        syncService = created
        return created
    }

    val syncHandlers = object {
        fun start() = scope.launch {
            val service = ensureSyncService() ?: run {
                storeState = storeState.withError("INVALID_STATE_TRANSITION", "同步服务不可用（缺少节点或游标未初始化）")
                return@launch
            }
            storeState = storeState.beginBusy()
            runCatching { service.start() }
                .onSuccess { storeState = storeState.withSyncStatus(it) }
                .onFailure { error ->
                    storeState = storeState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                }
        }

        fun stop() = scope.launch {
            val service = syncService ?: return@launch
            storeState = storeState.beginBusy()
            runCatching { service.stop() }
                .onSuccess { storeState = storeState.withSyncStatus(it) }
                .onFailure { error ->
                    storeState = storeState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                }
        }

        fun refresh() = scope.launch {
            val service = syncService ?: return@launch
            runCatching { service.status() }
                .onSuccess { storeState = storeState.withSyncStatus(it) }
                .onFailure { error ->
                    storeState = storeState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                }
        }
    }

    val groupHandlers = GroupHandlers(
        updateCreateName = { value -> groupState = groupState.updateCreateName(value) },
        createGroup = {
            scope.launch {
                groupState = groupState.beginBusy()
                withGroup { service -> service.createGroup(groupState.createName.trim()) }
                    .onSuccess { groupState = groupState.withSnapshot(it).copy(ready = true) }
                    .onFailure { error ->
                        groupState = groupState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        refresh = {
            scope.launch {
                val current = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                withGroup { service -> service.snapshot(current.groupId) }
                    .onSuccess { groupState = groupState.withSnapshot(it).copy(ready = true) }
                    .onFailure { error ->
                        groupState = groupState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        updateNewName = { value -> groupState = groupState.updateNewName(value) },
        rename = {
            scope.launch {
                val current = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                withGroup { service -> service.rename(current.groupId, groupState.newName.trim()) }
                    .onSuccess { groupState = groupState.withSnapshot(it) }
                    .onFailure { error ->
                        groupState = groupState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                    }
            }
        },
        toggleMute = { credential, muted ->
            scope.launch {
                val current = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                withGroup { service -> service.setMuted(current.groupId, credential, muted) }
                    .onSuccess { groupState = groupState.withSnapshot(it) }
                    .onFailure { error ->
                        groupState = groupState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                    }
            }
        },
        setRole = { credential, role ->
            scope.launch {
                val current = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                withGroup { service -> service.setRole(current.groupId, credential, role) }
                    .onSuccess { groupState = groupState.withSnapshot(it) }
                    .onFailure { error ->
                        groupState = groupState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                    }
            }
        },
        removeMember = { credential ->
            scope.launch {
                val current = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                withGroup { service -> service.removeMember(current.groupId, credential) }
                    .onSuccess { groupState = groupState.withSnapshot(it) }
                    .onFailure { error ->
                        groupState = groupState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                    }
            }
        },
        acceptPending = {
            scope.launch {
                val current = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                withGroup { service -> service.acceptPending(current.groupId) }
                    .onSuccess { groupState = groupState.withSnapshot(it) }
                    .onFailure { error ->
                        groupState = groupState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                    }
            }
        },
        discardPending = {
            scope.launch {
                val current = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                withGroup { service -> service.discardPending(current.groupId, "operator_discarded") }
                    .onSuccess { groupState = groupState.withSnapshot(it) }
                    .onFailure { error ->
                        groupState = groupState.withError((error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT", error.message)
                    }
            }
        },
    )

    LaunchedEffect(Unit) { groupState = groupState.copy(ready = true) }

    val storeHandlers = StoreHandlers(
        cancelLatestDraft = {
            scope.launch {
                if (store == null) return@launch
                val latest = storeState.drafts.lastOrNull() ?: return@launch
                storeState = storeState.beginBusy()
                runCatching { store.cancelDraft(latest.draftId) }
                    .onSuccess { storeState = storeState.withCancel(it) }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        updateSendNodeUrl = { value -> storeState = storeState.updateSendNodeUrl(value) },
        updateSendNodeId = { value -> storeState = storeState.updateSendNodeId(value) },
        updateSendNodePublicKey = { value -> storeState = storeState.updateSendNodePublicKey(value) },
        configureSendNode = {
            scope.launch {
                if (store == null) return@launch
                storeState = storeState.beginBusy()
                val key = storeState.sendNodePublicKey.trim().ifEmpty { null }
                runCatching { store.configureSendNode(storeState.sendNodeUrl.trim(), storeState.sendNodeId.trim(), key) }
                    .onSuccess {
                        storeState = storeState.copy(
                            busy = false,
                            notice = "节点配置已保存（未联网校验）；发送需要设备已注册且已加入群。",
                        )
                    }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        sendPrepare = {
            scope.launch {
                if (store == null) return@launch
                val draft = storeState.drafts.lastOrNull() ?: return@launch
                storeState = storeState.beginBusy()
                runCatching { store.sendPrepare(draft.draftId) }
                    .onSuccess { storeState = storeState.withSendStep(it) }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        sendDispatch = {
            scope.launch {
                if (store == null) return@launch
                val draft = storeState.drafts.lastOrNull() ?: return@launch
                storeState = storeState.beginBusy()
                runCatching { store.sendDispatch(draft.draftId) }
                    .onSuccess { storeState = storeState.withSendStep(it) }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        sendRetry = {
            scope.launch {
                if (store == null) return@launch
                val draft = storeState.drafts.lastOrNull() ?: return@launch
                storeState = storeState.beginBusy()
                runCatching { store.sendRetry(draft.draftId) }
                    .onSuccess { storeState = storeState.withSendStep(it) }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        syncStart = { syncHandlers.start() },
        syncStop = { syncHandlers.stop() },
        syncRefresh = { syncHandlers.refresh() },
        sendReconcile = {
            scope.launch {
                if (store == null) return@launch
                val draft = storeState.drafts.lastOrNull() ?: return@launch
                storeState = storeState.beginBusy()
                runCatching { store.sendReconcile(draft.draftId) }
                    .onSuccess { storeState = storeState.withSendStep(it) }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        pollEvents = {
            scope.launch {
                if (store == null) return@launch
                storeState = storeState.beginBusy()
                runCatching { store.pollEvents(storeState.nextEventSeq, 64) }
                    .onSuccess { storeState = storeState.withEvents(it) }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
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

    var attachmentState by remember { mutableStateOf(PreviewAttachmentState()) }
    val attachmentService = remember {
        runCatching {
            createAttachmentService(
                java.io.File("build/preview-attachments").apply { mkdirs() }.absolutePath,
            )
        }.getOrNull()
    }

    LaunchedEffect(Unit) {
        attachmentState = if (attachmentService == null) {
            PreviewAttachmentState().withError("INTERNAL_FAILURE", "附件服务未加载")
        } else {
            PreviewAttachmentState(ready = true)
        }
    }

    val filePicker = remember { runCatching { DesktopFilePicker(null) }.getOrNull() }

    val attachmentHandlers = AttachmentHandlers(
        chooseImportFile = {
            scope.launch {
                val picker = filePicker ?: return@launch
                runCatching { picker.chooseImportFile() }
                    .onSuccess { chosen ->
                        attachmentState = attachmentState.updateImportPath(chosen.path)
                        attachmentState = attachmentState.copy(
                            notice = "已选择 " + chosen.displayName + "（" + chosen.sizeBytes + " 字节，令牌 " +
                                chosen.token.take(16) + "…）",
                        )
                    }
                    .onFailure { error ->
                        val code = (error as? PlatformFileException)?.code ?: "INVALID_FILE_TOKEN"
                        if (code != chat.platform.KeyProtectionCodes.USER_AUTH_CANCELLED) {
                            attachmentState = attachmentState.withError(code, error.message)
                        }
                    }
            }
        },
        chooseExportTarget = {
            scope.launch {
                val picker = filePicker ?: return@launch
                runCatching { picker.chooseExportTarget() }
                    .onSuccess { chosen -> attachmentState = attachmentState.updateExportPath(chosen.path) }
                    .onFailure { error ->
                        val code = (error as? PlatformFileException)?.code ?: "INVALID_FILE_TOKEN"
                        if (code != chat.platform.KeyProtectionCodes.USER_AUTH_CANCELLED) {
                            attachmentState = attachmentState.withError(code, error.message)
                        }
                    }
            }
        },
        updateImportPath = { value -> attachmentState = attachmentState.updateImportPath(value) },
        importFile = {
            scope.launch {
                val service = attachmentService ?: return@launch
                attachmentState = attachmentState.beginBusy()
                val path = attachmentState.importPath.trim()
                val displayName = File(path).name.ifEmpty { "附件" }
                runCatching { service.importFile(path, displayName) }
                    .onSuccess { attachmentState = attachmentState.withImported(it) }
                    .onFailure { error ->
                        attachmentState = attachmentState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        updateExportPath = { value -> attachmentState = attachmentState.updateExportPath(value) },
        exportFile = { attachmentId ->
            scope.launch {
                val service = attachmentService ?: return@launch
                attachmentState = attachmentState.beginBusy()
                val target = attachmentState.exportPath.trim()
                runCatching { service.exportFile(attachmentId, target) }
                    .onSuccess { attachmentState = attachmentState.withExported(attachmentId, it, target) }
                    .onFailure { error ->
                        attachmentState = attachmentState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        cancel = { attachmentId ->
            scope.launch {
                val service = attachmentService ?: return@launch
                attachmentState = attachmentState.beginBusy()
                runCatching { service.cancel(attachmentId) }
                    .onSuccess { attachmentState = attachmentState.withUpdated(it) }
                    .onFailure { error ->
                        attachmentState = attachmentState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
    )

    Window(
        title = "Chat · 交互预览（样例消息 + 真实身份 + 本机存储 + 附件）",
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
            attachmentState = attachmentState,
            attachmentHandlers = attachmentHandlers,
            groupState = groupState,
            groupHandlers = groupHandlers,
        )
    }
}

/** 演示口令：正式产品必须改为用户输入或操作系统密钥链（PLATFORM-001 未接线部分）。 */
private const val DEMO_KEY_PASSPHRASE = "chat-preview-demo-passphrase"
