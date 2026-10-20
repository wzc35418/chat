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
import chat.bridge.createPluginService
import chat.bridge.createSyncService
import chat.bridge.createIdentityService
import chat.bridge.createLocalStore
import chat.presentation.display.presentInviteError
import chat.presentation.display.presentInvitePreview
import chat.presentation.preview.PreviewAttachmentState
import chat.presentation.preview.GroupSummary
import chat.presentation.preview.PreviewGroupState
import chat.presentation.preview.selectGroup
import chat.presentation.preview.withSummaries
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
import chat.presentation.preview.withSyncOnce
import chat.presentation.preview.withMessageCards
import chat.presentation.preview.advance
import chat.presentation.preview.updateEndpoint
import chat.presentation.preview.updateInvitee
import chat.presentation.preview.updateNodeId
import chat.presentation.preview.updateNodePublicKey
import chat.presentation.preview.withOnboardingError
import chat.presentation.preview.updateInviteeKeyPackagePath
import chat.presentation.preview.withAboutError
import chat.presentation.preview.withAppInfo
import chat.presentation.preview.withCreatedGroup
import chat.presentation.preview.withDataDir
import chat.presentation.preview.withHostLibraryPath
import chat.presentation.preview.withFetchedInvite
import chat.presentation.preview.updateMemberDeviceId
import chat.presentation.preview.withMemberAddReceipt
import chat.presentation.preview.withRegistration
import chat.presentation.preview.withInviteReceipt
import chat.presentation.preview.withMessagePreview
import chat.presentation.preview.withSyncStatus
import chat.presentation.display.toCard
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
import chat.presentation.preview.updateManifest
import chat.presentation.preview.updateRecoveryPassphrase
import chat.presentation.preview.withDeviceRevoked
import chat.presentation.preview.withDevices
import chat.presentation.preview.withRecoveryExported
import chat.presentation.preview.withError
import chat.presentation.preview.withInstalled
import chat.presentation.preview.toggleTrusted
import chat.presentation.preview.updateJarPath
import chat.presentation.preview.withLoadedPlugins
import chat.presentation.preview.withPackage
import chat.presentation.preview.withPlan
import chat.presentation.preview.withPlugins
import chat.presentation.preview.withStateNotice
import chat.presentation.preview.withStorageValue
import chat.ui.PluginHandlers
import chat.ui.PreviewChatScreen
import chat.ui.AboutHandlers
import chat.ui.OnboardingHandlers
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
    var aboutState by remember { mutableStateOf(chat.presentation.preview.PreviewAboutState()) }
    // 节点配置持久化（非机密）：首启读取，未配置则弹引导。
    val nodeConfigStore = remember { NodeConfigStore(NodeConfigStore.defaultFile()) }
    val initialNodeConfig = remember { runCatching { nodeConfigStore.load() }.getOrDefault(NodeConfig()) }
    var onboardingState by remember {
        mutableStateOf(
            if (initialNodeConfig.isConfigured) null else chat.presentation.preview.PreviewOnboardingState(),
        )
    }
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

    // PLATFORM-001/002 接线：Windows 用 DPAPI（当前用户作用域），其它平台回退口令派生槽。
    // 磁盘只存密文/blob；启动时 unwrap，首次运行生成并 wrap。
    LaunchedEffect(Unit) {
        if (identityDir == null) {
            keySlotSummary = "密钥槽不可用（目录创建失败）"
            return@LaunchedEffect
        }
        val slotDirectory = File(identityDir, "keyslot").apply { mkdirs() }
        val slot: chat.platform.KeyProtection = if (System.getProperty("os.name", "").lowercase().contains("windows")) {
            DpapiKeySlot(slotDirectory)
        } else {
            PassphraseKeySlot(slotDirectory, DEMO_KEY_PASSPHRASE.toCharArray())
        }
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
        updateRecoveryPassphrase = { value -> identityState = identityState.updateRecoveryPassphrase(value) },
        exportKeyPackage = {
            scope.launch {
                val service = identity ?: return@launch
                runCatching {
                    val bytes = service.exportKeyPackage()
                    val picker = DesktopFilePicker(null)
                    val target = picker.chooseExportTarget("my-key-package.kp")
                    java.io.File(target.path).writeBytes(bytes)
                    bytes.size
                }.onSuccess { size ->
                    identityState = identityState.copy(busy = false, notice = "已导出 KeyPackage（" + size + " 字节）")
                }.onFailure { error ->
                    identityState = identityState.copy(
                        busy = false,
                        notice = error.message ?: "导出失败",
                        lastErrorCode = (error as? IdentityException)?.code ?: "UNKNOWN_RESULT",
                    )
                }
            }
        },
        loadDevices = {
            scope.launch {
                val service = identity ?: return@launch
                identityState = identityState.copy(busy = true, lastErrorCode = null)
                runCatching { service.listDevices(includeRevoked = true) }
                    .onSuccess { identityState = identityState.withDevices(it) }
                    .onFailure { error ->
                        identityState = identityState.copy(
                            busy = false,
                            notice = error.message ?: "加载设备失败",
                            lastErrorCode = (error as? IdentityException)?.code ?: "UNKNOWN_RESULT",
                        )
                    }
            }
        },
        revokeDevice = { deviceId ->
            scope.launch {
                val service = identity ?: return@launch
                identityState = identityState.copy(busy = true, lastErrorCode = null)
                runCatching { service.revokeDevice(deviceId, "user requested") }
                    .onSuccess { revoked ->
                        identityState = identityState.withDeviceRevoked(deviceId, revoked)
                        runCatching { service.listDevices(includeRevoked = true) }
                            .onSuccess { identityState = identityState.withDevices(it) }
                    }
                    .onFailure { error ->
                        identityState = identityState.copy(
                            busy = false,
                            notice = error.message ?: "撤销失败",
                            lastErrorCode = (error as? IdentityException)?.code ?: "UNKNOWN_RESULT",
                        )
                    }
            }
        },
        importRecovery = {
            scope.launch {
                val service = identity ?: return@launch
                val database = identityDatabase
                val key = identityKey
                if (database == null || key == null) {
                    identityState = identityState.copy(
                        busy = false,
                        notice = "导入需要已打开的本机密钥槽",
                        lastErrorCode = "INVALID_STATE_TRANSITION",
                    )
                    return@launch
                }
                if (identityState.account != null) {
                    // 诚实拒绝：目标库已有身份，导入不会覆盖。
                    identityState = identityState.copy(
                        busy = false,
                        notice = "当前库已有身份；导入必须在空库上执行（请换一个库路径）",
                        lastErrorCode = "RECOVERY_TARGET_NOT_EMPTY",
                    )
                    return@launch
                }
                val picker = runCatching { DesktopFilePicker(null) }.getOrNull() ?: return@launch
                val passphrase = identityState.recoveryPassphrase
                identityState = identityState.copy(busy = true, lastErrorCode = null)
                runCatching {
                    val chosen = picker.chooseImportFile("选择身份恢复文件")
                    val bytes = File(chosen.path).readBytes()
                    val view = service.importRecoveryBundle(passphrase, bytes)
                    view
                }.onSuccess { view ->
                    val reopened = runCatching { service.openPersisted(database, key, node) }
                    identityState = identityState.copy(
                        busy = false,
                        account = reopened.getOrNull() ?: identityState.account,
                        notice = "已导入身份 " + view.deviceId + "（受限显示）；旧库消息与附件仍不可读",
                        lastErrorCode = reopened.exceptionOrNull()?.let { (it as? LocalStoreException)?.code },
                    )
                    runCatching { refreshContacts() }
                }.onFailure { error ->
                    identityState = identityState.copy(
                        busy = false,
                        notice = error.message ?: "导入失败",
                        lastErrorCode = (error as? IdentityException)?.code
                            ?: (error as? LocalStoreException)?.code
                            ?: "UNKNOWN_RESULT",
                    )
                }
            }
        },
        exportRecovery = {
            scope.launch {
                val service = identity ?: return@launch
                val picker = runCatching { DesktopFilePicker(null) }.getOrNull() ?: return@launch
                val passphrase = identityState.recoveryPassphrase
                identityState = identityState.copy(busy = true, lastErrorCode = null)
                runCatching {
                    val bundle = service.exportRecoveryBundle(passphrase)
                    val chosen = picker.chooseExportTarget("identity-recovery.chatrecovery")
                    File(chosen.path).writeBytes(bundle)
                    bundle.size to chosen.path
                }.onSuccess { (bytes, target) ->
                    identityState = identityState.withRecoveryExported(bytes, target)
                }.onFailure { error ->
                    identityState = identityState.copy(
                        busy = false,
                        notice = error.message ?: "导出失败",
                        lastErrorCode = (error as? IdentityException)?.code
                            ?: (error as? LocalStoreException)?.code
                            ?: "UNKNOWN_RESULT",
                    )
                }
            }
        },
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

    var pluginState by remember { mutableStateOf(chat.presentation.preview.PreviewPluginState.Empty) }
    val pluginService = remember { runCatching { createPluginService() }.getOrNull() }
    val pluginJvmHost = remember { runCatching { PluginJvmHost() }.getOrNull() }

    val pluginHandlers = object {
        fun refresh() = scope.launch {
            val service = pluginService ?: return@launch
            pluginState = pluginState.beginBusy()
            runCatching { service.listPlugins() }
                .onSuccess { pluginState = pluginState.withPlugins(it) }
                .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
        }

        fun install() = scope.launch {
            val service = pluginService ?: return@launch
            pluginState = pluginState.beginBusy()
            runCatching {
                val manifest = service.validateManifest(pluginState.manifestJson)
                service.install(pluginState.manifestJson)
                manifest.id
            }.onSuccess { pluginState = pluginState.withInstalled(it) }
                .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
            runCatching { service.listPlugins() }.onSuccess { pluginState = pluginState.withPlugins(it) }
        }

        fun start(id: String) = scope.launch {
            val service = pluginService ?: return@launch
            pluginState = pluginState.beginBusy()
            runCatching { service.start(id) }
                .onSuccess { pluginState = pluginState.withStateNotice("插件 " + id + " 已启动（宿主内状态）") }
                .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
            runCatching { service.listPlugins() }.onSuccess { pluginState = pluginState.withPlugins(it) }
        }

        fun stop(id: String) = scope.launch {
            val service = pluginService ?: return@launch
            pluginState = pluginState.beginBusy()
            runCatching { service.stop(id) }
                .onSuccess { pluginState = pluginState.withStateNotice("插件 " + id + " 已停止") }
                .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
            runCatching { service.listPlugins() }.onSuccess { pluginState = pluginState.withPlugins(it) }
        }

        fun uninstall(id: String) = scope.launch {
            val service = pluginService ?: return@launch
            pluginState = pluginState.beginBusy()
            runCatching { service.uninstall(id, false) }
                .onSuccess { pluginState = pluginState.withStateNotice("插件 " + id + " 已卸载（保留私有存储）") }
                .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
            runCatching { service.listPlugins() }.onSuccess { pluginState = pluginState.withPlugins(it) }
        }

        fun dispatch(id: String) = scope.launch {
            val service = pluginService ?: return@launch
            pluginState = pluginState.beginBusy()
            runCatching { service.dispatch(id, "composer.command", "ping") }
                .onSuccess { pluginState = pluginState.withPlan(it) }
                .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
        }

        fun storage(pluginId: String) = scope.launch {
            val service = pluginService ?: return@launch
            pluginState = pluginState.beginBusy()
            runCatching { service.storageGet(pluginId, "probe") }
                .onSuccess { pluginState = pluginState.withStorageValue(it) }
                .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
        }
    }

    var syncService by remember { mutableStateOf<chat.api.SyncService?>(null) }
    val trayNotifications = remember {
        runCatching { TrayNotifications().also { runCatching { it.installIcon() } } }.getOrNull()
    }
    val lifecycle = remember {
        DesktopLifecycle { exitApplication() }
    }

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
        updateMemberDeviceId = { value -> groupState = groupState.updateMemberDeviceId(value) },
        addNodeMember = {
            scope.launch {
                val snapshot = groupState.snapshot ?: return@launch
                val deviceId = groupState.memberDeviceId.trim()
                if (deviceId.isBlank()) {
                    groupState = groupState.withError("INVALID_INPUT", "先填对方设备 ID（dev:1:...）")
                    return@launch
                }
                groupState = groupState.beginBusy()
                runCatching {
                    withGroup { service ->
                        service.submitNodeMemberAdd(
                            snapshot.groupId,
                            deviceId,
                            storeState.sendNodeUrl.trim(),
                            storeState.sendNodeId.trim(),
                            storeState.sendNodePublicKey.trim().ifEmpty { null },
                        )
                    }.getOrThrow()
                }.onSuccess { groupState = groupState.withMemberAddReceipt(it) }
                    .onFailure { error ->
                        groupState = groupState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        registerOnNode = {
            scope.launch {
                val snapshot = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                runCatching {
                    withGroup { service ->
                        service.registerOnNode(
                            snapshot.groupId,
                            storeState.sendNodeUrl.trim(),
                            storeState.sendNodeId.trim(),
                            storeState.sendNodePublicKey.trim().ifEmpty { null },
                        )
                    }.getOrThrow()
                }.onSuccess { groupState = groupState.withRegistration(it) }
                    .onFailure { error ->
                        groupState = groupState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        loadGroupList = {
            scope.launch {
                groupState = groupState.beginBusy()
                withGroup { service -> service.listGroups() }
                    .onSuccess { list ->
                        groupState = groupState.withSummaries(
                            list.groups.map {
                                GroupSummary(
                                    groupId = it.groupId,
                                    name = it.name,
                                    epoch = it.epoch,
                                    role = it.role,
                                    policyVersion = it.policyVersion,
                                    dissolved = it.dissolved,
                                    left = it.left,
                                    pending = it.pending,
                                    revokedDevicesCount = it.revokedDevicesCount,
                                )
                            },
                            list.skipped,
                        )
                    }
                    .onFailure { error ->
                        groupState = groupState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        selectGroup = { groupId ->
            groupState = groupState.selectGroup(groupId)
            scope.launch {
                groupState = groupState.beginBusy()
                withGroup { service -> service.snapshot(groupId) }
                    .onSuccess { groupState = groupState.withSnapshot(it) }
                    .onFailure { error ->
                        groupState = groupState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        updateInvitee = { value -> groupState = groupState.updateInvitee(value) },
        chooseInviteeKeyPackage = {
            runCatching {
                val picker = DesktopFilePicker(null)
                val chosen = picker.chooseImportFile("选择被邀请者的 KeyPackage（.kp）")
                groupState = groupState.updateInviteeKeyPackagePath(chosen.path)
            }.onFailure { groupState = groupState.withError("INVALID_INPUT", it.message) }
        },
        createGroupWithKeyPackage = {
            scope.launch {
                val path = groupState.inviteeKeyPackagePath
                if (path.isBlank()) {
                    groupState = groupState.withError("INVALID_INPUT", "先选择被邀请者的 KeyPackage 文件")
                    return@launch
                }
                groupState = groupState.beginBusy()
                runCatching {
                    val bytes = java.io.File(path).readBytes()
                    withGroup { service ->
                        service.createWithInviteeKeyPackage(
                            name = groupState.createName.ifBlank { "新群" },
                            inviteeCredential = groupState.inviteeCredential.ifBlank { "invitee" },
                            keyPackageBytes = bytes,
                            localCredential = groupState.inviteeCredential.ifBlank { "local" },
                        )
                    }.getOrThrow()
                }.onSuccess { created ->
                    groupState = groupState.withCreatedGroup(created)
                }.onFailure { error ->
                    groupState = groupState.withError(
                        (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                        error.message,
                    )
                }
            }
        },
        submitInvite = {
            scope.launch {
                val created = groupState.createdGroup ?: return@launch
                groupState = groupState.beginBusy()
                runCatching {
                    withGroup { service ->
                        service.submitInvite(
                            created.invite.groupId,
                            created.invite,
                            created.welcomeBytes,
                            storeState.sendNodeUrl.trim(),
                            storeState.sendNodeId.trim(),
                            storeState.sendNodePublicKey.trim().ifEmpty { null },
                        )
                    }.getOrThrow()
                }.onSuccess { groupState = groupState.withInviteReceipt(it) }
                    .onFailure { error ->
                        groupState = groupState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        fetchInvite = {
            scope.launch {
                val snapshot = groupState.snapshot ?: return@launch
                groupState = groupState.beginBusy()
                withGroup { service ->
                    service.fetchInvite(
                        snapshot.groupId,
                        storeState.sendNodeUrl.trim(),
                        storeState.sendNodeId.trim(),
                        storeState.sendNodePublicKey.trim().ifEmpty { null },
                    )
                }.onSuccess { groupState = groupState.withFetchedInvite(it) }
                    .onFailure { error ->
                        groupState = groupState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
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

    // 附件服务在后面创建（需要缓存目录），这里先留引用给早先定义的回调使用。
    val attachmentServiceHolder = remember { mutableStateOf<chat.api.AttachmentService?>(null) }

    /** PRESENTATION 门面：单点编排（缺服务时显式失败，不 mock 成功）。 */
    fun facadeOf(local: chat.api.LocalStore): chat.presentation.AppFacade = chat.presentation.AppFacade(
        store = local,
        attachments = attachmentServiceHolder.value,
        plugins = pluginService,
    )

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
        loadMessageAttachments = {
            scope.launch {
                val local = store ?: return@launch
                val conversationId = storeState.selectedConversationId ?: return@launch
                val attachmentService = attachmentServiceHolder.value ?: return@launch
                storeState = storeState.beginBusy()
                // 走 PRESENTATION 门面：消息 + 附件卡片一次编排，缺失/失败显式计数。
                when (val opened = facadeOf(local).openConversation(conversationId, null, 20)) {
                    is chat.presentation.UseCaseResult.Failed -> {
                        storeState = storeState.withError(opened.code, opened.message)
                    }
                    is chat.presentation.UseCaseResult.Ok -> {
                        var updated = storeState
                        for (message in opened.value.messages) {
                            updated = updated.withMessagePreview(message.messageId, message.body)
                            updated = updated.withMessageCards(
                                message.messageId,
                                opened.value.cardsByMessage[message.messageId] ?: emptyList(),
                            )
                        }
                        storeState = updated.copy(
                            busy = false,
                            notice = "已加载 " + opened.value.messages.size + " 条消息；无附件 " +
                                opened.value.messagesWithoutAttachments + " 条，取卡片失败 " +
                                opened.value.messagesWithFailedAttachments + " 条",
                        )
                    }
                }
            }
        },
        downloadCard = { attachmentId ->
            scope.launch {
                val attachmentService = attachmentServiceHolder.value ?: return@launch
                val endpoint = storeState.sendNodeUrl.trim()
                val nodeId = storeState.sendNodeId.trim()
                val groupId = groupState.snapshot?.groupId ?: demoConversation
                if (endpoint.isBlank() || nodeId.isBlank()) {
                    storeState = storeState.withError("INVALID_INPUT", "请先在发送区填写节点地址与节点 ID")
                    return@launch
                }
                val target = storeState.exportTarget.trim()
                if (target.isBlank()) {
                    storeState = storeState.withError("INVALID_INPUT", "请先填写下载目标路径（附件面板可用来选择）")
                    return@launch
                }
                storeState = storeState.beginBusy()
                val pubkey = storeState.sendNodePublicKey.trim().ifEmpty { null }
                runCatching {
                    attachmentService.download(attachmentId, target, endpoint, nodeId, pubkey, groupId, 1L)
                }.onSuccess { written ->
                    storeState = storeState.copy(
                        busy = false,
                        notice = "已从节点下载 " + written + " 字节并校验解密，目标 " + target,
                    )
                }.onFailure { error ->
                    storeState = storeState.withError(
                        (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                        error.message,
                    )
                }
            }
        },
        exportCard = { attachmentId ->
            scope.launch {
                val attachmentService = attachmentServiceHolder.value ?: return@launch
                val target = storeState.exportTarget.trim()
                if (target.isBlank()) {
                    storeState = storeState.withError("INVALID_INPUT", "请先在附件面板填写导出目标路径")
                    return@launch
                }
                runCatching { attachmentService.exportFile(attachmentId, target) }
                    .onSuccess { written ->
                        storeState = storeState.copy(busy = false, notice = "已导出 " + written + " 字节到 " + target)
                    }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        notify = { body ->
            scope.launch {
                val notifications = trayNotifications
                if (notifications == null || !notifications.isSupported()) {
                    // 无系统托盘：显式回退到应用内提示，不假装已弹系统通知。
                    storeState = storeState.copy(
                        busy = false,
                        notice = "系统通知不可用（当前环境无托盘）；已改为应用内提示：" + body,
                    )
                    return@launch
                }
                runCatching { notifications.show(chat.platform.NotificationRequest("Chat", body, "preview-test")) }
                    .onSuccess {
                        storeState = storeState.copy(busy = false, notice = "已尝试弹出系统通知：" + body)
                    }
                    .onFailure { error ->
                        storeState = storeState.withError(
                            (error as? chat.platform.NotificationException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
            }
        },
        syncStart = { syncHandlers.start() },
        syncStop = { syncHandlers.stop() },
        syncRefresh = { syncHandlers.refresh() },
        syncOnce = {
            scope.launch {
                val service = syncService
                if (service == null) {
                    storeState = storeState.withError("CORE_CLOSED", "同步服务未创建（需要节点配置与已初始化的游标）")
                    return@launch
                }
                storeState = storeState.beginBusy()
                runCatching { service.syncOnce() }
                    .onSuccess { once ->
                        storeState = storeState.withSyncOnce(once)
                        // 拉回来的条目要能看见：收完一轮顺手把当前会话的消息重载一遍。
                        val local = store
                        val conversationId = storeState.selectedConversationId
                        if (local != null && conversationId != null) {
                            when (val opened = facadeOf(local).openConversation(conversationId, null, 50)) {
                                is chat.presentation.UseCaseResult.Ok -> {
                                    var updated = storeState
                                    for (message in opened.value.messages) {
                                        updated = updated.withMessagePreview(message.messageId, message.body)
                                    }
                                    storeState = updated.copy(
                                        busy = false,
                                        notice = "收一轮：拉取 " + once.fetched + " 条 / 落库 " + once.applied +
                                            " 条；当前会话共 " + opened.value.messages.size + " 条消息",
                                    )
                                }
                                is chat.presentation.UseCaseResult.Failed -> {
                                    storeState = storeState.withError(opened.code, opened.message)
                                }
                            }
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
    LaunchedEffect(attachmentService) { attachmentServiceHolder.value = attachmentService }

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
            onboardingState = onboardingState,
            onboardingHandlers = OnboardingHandlers(
                updateEndpoint = { value -> onboardingState = onboardingState?.updateEndpoint(value) },
                updateNodeId = { value -> onboardingState = onboardingState?.updateNodeId(value) },
                updateNodePublicKey = { value -> onboardingState = onboardingState?.updateNodePublicKey(value) },
                save = {
                    val current = onboardingState ?: return@OnboardingHandlers
                    if (current.step < 2) {
                        onboardingState = current.advance()
                    } else {
                        runCatching {
                            nodeConfigStore.save(
                                NodeConfig(
                                    endpointUrl = current.endpointUrl.trim(),
                                    nodeId = current.nodeId.trim(),
                                    nodePublicKeyHex = current.nodePublicKeyHex.trim(),
                                ),
                            )
                        }.onSuccess {
                            storeState = storeState
                                .updateSendNodeUrl(current.endpointUrl.trim())
                                .updateSendNodeId(current.nodeId.trim())
                                .updateSendNodePublicKey(current.nodePublicKeyHex.trim())
                            onboardingState = null
                        }.onFailure { error ->
                            onboardingState = current.withOnboardingError("CONFIG_WRITE_FAILED", error.message)
                        }
                    }
                },
                skip = {
                    storeState = storeState
                        .updateSendNodeUrl(initialNodeConfig.endpointUrl)
                        .updateSendNodeId(initialNodeConfig.nodeId)
                        .updateSendNodePublicKey(initialNodeConfig.nodePublicKeyHex)
                    onboardingState = null
                },
                close = { onboardingState = null },
            ),
            aboutState = aboutState,
            aboutHandlers = AboutHandlers(
                refresh = {
                    aboutState = aboutState.withDataDir(storeDirectory?.absolutePath ?: "（本机存储未初始化）")
                    runCatching {
                        val diagnostics = chat.bridge.NativeDiagnosticsService()
                        aboutState = aboutState
                            .withAppInfo(diagnostics.appInfo())
                            .withHostLibraryPath(
                                runCatching { diagnostics.hostLibraryPath("chat_ffi") }
                                    .getOrElse { error -> "解析失败：" + (error.message ?: "unknown") },
                            )
                    }.onFailure { error ->
                        aboutState = aboutState.withAboutError(
                            (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT",
                            error.message,
                        )
                    }
                },
            ),
            pluginState = pluginState,
            pluginHandlers = PluginHandlers(
                updateManifest = { pluginState = pluginState.updateManifest(it) },
                chooseManifestFile = {
                    val picker = filePicker ?: return@PluginHandlers
                    runCatching {
                        val chosen = picker.chooseImportFile("选择插件清单 JSON")
                        pluginState = pluginState.updateManifest(File(chosen.path).readText())
                    }.onFailure { pluginState = pluginState.withError("INVALID_INPUT", it.message) }
                },
                loadList = { pluginHandlers.refresh() },
                install = { pluginHandlers.install() },
                start = { pluginHandlers.start(it) },
                stop = { pluginHandlers.stop(it) },
                uninstall = { pluginHandlers.uninstall(it) },
                dispatch = { pluginHandlers.dispatch(it) },
                storageGet = { pluginHandlers.storage(it) },
                chooseJar = {
                    runCatching {
                        val picker = DesktopFilePicker(null)
                        val chosen = picker.chooseImportFile("选择插件 JAR（.jar）")
                        pluginState = pluginState.updateJarPath(chosen.path)
                    }.onFailure { pluginState = pluginState.withError("INVALID_INPUT", it.message) }
                },
                inspectPackage = {
                    scope.launch {
                        val service = pluginService ?: return@launch
                        pluginState = pluginState.beginBusy()
                        runCatching {
                            val bytes = File(pluginState.jarPath).readBytes()
                            service.inspectPackage(bytes)
                        }.onSuccess { pluginState = pluginState.withPackage(it) }
                            .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
                    }
                },
                toggleTrusted = { value -> pluginState = pluginState.toggleTrusted(value) },
                loadJar = {
                    scope.launch {
                        val host = pluginJvmHost ?: return@launch
                        val view = pluginState.packageView ?: return@launch
                        pluginState = pluginState.beginBusy()
                        runCatching {
                            // 清单声明的权限 → 沙箱：只把四个沙箱权限透给类加载器（其余是用户可见权限）。
                            val sandboxPermissions = view.permissions
                                .map { it.trim() }
                                .filter { it in PluginSandboxClassLoader.KNOWN }
                                .toSet()
                            host.load(pluginState.jarPath, view.pluginId, pluginState.trusted, sandboxPermissions)
                            host.loadedPluginIds()
                        }.onSuccess { pluginState = pluginState.withLoadedPlugins(it) }
                            .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
                    }
                },
                runAction = {
                    scope.launch {
                        val host = pluginJvmHost ?: return@launch
                        val id = pluginState.loadedPlugins.firstOrNull() ?: return@launch
                        pluginState = pluginState.beginBusy()
                        runCatching { host.runSelectedTextAction(id, "echo", "ping") }
                            .onSuccess { pluginState = pluginState.withStateNotice("插件输出：" + it) }
                            .onFailure { pluginState = pluginState.withError(codeOf(it), it.message) }
                    }
                },
                unloadJar = {
                    scope.launch {
                        val host = pluginJvmHost ?: return@launch
                        val id = pluginState.loadedPlugins.firstOrNull() ?: return@launch
                        runCatching { host.unload(id) }
                        pluginState = pluginState.withLoadedPlugins(host.loadedPluginIds())
                    }
                },
            ),
        )
    }
}

/** 附件卡片/插件面板共用的稳定错误码提取。 */
private fun codeOf(error: Throwable): String = (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT"

/** 演示口令：正式产品必须改为用户输入或操作系统密钥链（PLATFORM-001 未接线部分）。 */
private const val DEMO_KEY_PASSPHRASE = "chat-preview-demo-passphrase"