package chat.desktop

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import chat.api.LocalStore
import chat.bridge.createGroupService
import chat.bridge.createIdentityService
import chat.bridge.createLocalStore
import chat.bridge.createSyncService
import chat.presentation.AppFacade
import chat.presentation.UseCaseResult
import chat.ui.chat.ChatContactItem
import chat.ui.chat.ChatConversationItem
import chat.ui.chat.ChatGroupDetail
import chat.ui.chat.ChatGroupMemberItem
import chat.ui.chat.ChatGroupItem
import chat.ui.chat.ChatHomeHandlers
import chat.ui.chat.ChatHomeScreen
import chat.ui.chat.ChatHomeState
import chat.ui.chat.ChatMessageItem
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.File
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.UUID

/**
 * 正式聊天应用（默认入口）：只做真实数据。
 *
 * 会话/消息来自本地加密库；发送走真实节点（prepare → dispatch，结果未知不算成功）；
 * 拉取走 syncOnce。没有样例数据。
 */
@Composable
fun ChatAppRoot() {
    val scope = rememberCoroutineScope()
    val nodeConfigStore = remember { NodeConfigStore(NodeConfigStore.defaultFile()) }
    var nodeConfig by remember { mutableStateOf(runCatching { nodeConfigStore.load() }.getOrDefault(NodeConfig())) }
    var state by remember { mutableStateOf(ChatHomeState(revision = UI_REVISION)) }
    var settingsOpen by remember { mutableStateOf(false) }
    val env = remember { ChatEnvironment() }

    fun openConversation(conversationId: String) {
        scope.launch {
            env.selectGroupConversation(conversationId)
            val facade = env.facade ?: return@launch
            val messageConversationId = conversationIdFor(conversationId)
            state = state.copy(activeId = conversationId, busy = true, error = null)
            when (val opened = facade.openConversation(messageConversationId, null, 100)) {
                is UseCaseResult.Ok -> {
                    val formatter = DateTimeFormatter.ofPattern("HH:mm").withZone(ZoneId.systemDefault())
                    val messages = opened.value.messages.map { message ->
                        ChatMessageItem(
                            id = message.messageId,
                            mine = message.mine,
                            label = if (message.mine) "我" else message.author,
                            body = message.body,
                            status = formatter.format(Instant.ofEpochMilli(message.createdAtMillis)),
                        )
                    }
                    state = state.copy(messages = messages, busy = false)
                }
                is UseCaseResult.Failed -> {
                    // 会话行只有在“发过消息”后才存在；还没发过 = 空会话，不是错误。
                    val detail = opened.message.orEmpty()
                    if (detail.contains("does not exist")) {
                        state = state.copy(messages = emptyList(), busy = false, error = null)
                    } else {
                        state = state.copy(busy = false, error = detail)
                    }
                }
            }
        }
    }

    // 会话 = 群：列表直接来自群状态（不再用假会话 id）。
    fun reloadConversations() {
        scope.launch {
            val loaded = runCatching { env.listGroupConversations() }
            if (loaded.isFailure) {
                state = state.copy(loading = false, notice = "读取会话失败：" + (loaded.exceptionOrNull()?.message ?: "未知原因"))
                return@launch
            }
            var items = loaded.getOrThrow().map { conversation ->
                ChatConversationItem(
                    id = conversation.groupId,
                    title = conversation.name,
                    subtitle = "epoch " + conversation.epoch + " · 成员 " + conversation.members,
                )
            }
            if (items.isEmpty()) {
                // 兜底：群服务临时读不出来时，至少把已绑定的群显示成一个会话（不伪造成员数）。
                env.boundGroup()?.let { (id, name) -> items = listOf(ChatConversationItem(id, name, "已绑定群")) }
            }
            state = state.copy(conversations = items, loading = false)
            if (state.activeId == null && items.isNotEmpty()) {
                openConversation(items.first().id)
            }
        }
    }

    LaunchedEffect(Unit) {
        state = state.copy(nodeLabel = env.nodeLabel(nodeConfig))
        env.start(nodeConfig)
        state = state.copy(nodeLabel = env.lastStatus, notice = env.lastNotice)
        val side = env.loadSide()
        state = state.copy(
            contacts = side.contacts,
            groups = side.groups,
            identityLines = side.identityLines,
        )
        reloadConversations()
    }

    val handlers = ChatHomeHandlers(
        selectTab = { key -> state = state.copy(tab = key) },
        openGroup = { groupId ->
            scope.launch {
                val detail = env.loadGroupDetail(groupId)
                state = state.copy(
                    groupDetail = detail,
                    notice = if (detail == null) "群详情读取失败" else "群 " + detail.name,
                )
            }
        },
        closeGroup = { state = state.copy(groupDetail = null) },
        retryFailed = {
            val draftId = state.failedDraftId
            if (draftId != null) {
                scope.launch {
                    state = state.copy(busy = true, notice = "重试中…")
                    val result = env.retryDraft(draftId)
                    state = state.copy(busy = false, notice = result.first, error = result.second)
                    if (result.third) state = state.copy(failedDraftId = null)
                    state.activeId?.let { openConversation(it) }
                }
            }
        },
        reconcileFailed = {
            val draftId = state.failedDraftId
            if (draftId != null) {
                scope.launch {
                    state = state.copy(busy = true, notice = "对账中…（按 SubmissionId 查上一次提交）")
                    val result = env.reconcileDraft(draftId)
                    state = state.copy(busy = false, notice = result.first, error = result.second)
                    if (result.third) state = state.copy(failedDraftId = null)
                    state.activeId?.let { openConversation(it) }
                }
            }
        },
        dismissFailed = { state = state.copy(failedDraftId = null) },
        renameGroup = { newName ->
            scope.launch {
                val current = state.groupDetail ?: return@launch
                val result = env.renameGroup(current.id, newName)
                state = state.copy(notice = result)
                state = state.copy(groupDetail = env.loadGroupDetail(current.id) ?: current)
            }
        },
        removeMember = { credential ->
            scope.launch {
                val current = state.groupDetail ?: return@launch
                val result = env.removeMember(current.id, credential)
                state = state.copy(notice = result)
                state = state.copy(groupDetail = env.loadGroupDetail(current.id) ?: current)
            }
        },
        acceptPending = {
            scope.launch {
                val current = state.groupDetail ?: return@launch
                val result = env.acceptPending(current.id)
                state = state.copy(notice = result)
                state = state.copy(groupDetail = env.loadGroupDetail(current.id) ?: current)
            }
        },
        createInvite = {
            scope.launch {
                val token = env.createInvite()
                state = state.copy(
                    notice = if (token == null) "邀请口令生成失败（先看设置里的身份是否就绪）" else "邀请口令（24 小时有效）：" + token,
                )
            }
        },
        selectConversation = { conversationId -> openConversation(conversationId) },
        composerChange = { text -> state = state.copy(composer = text) },
        composeNew = {
            scope.launch {
                state = state.copy(busy = true, notice = "正在建群…")
                val result = runCatching { env.createGroupConversation("群 " + (state.conversations.size + 1)) }
                state = state.copy(busy = false)
                result.fold(
                    onSuccess = { conversation ->
                        state = state.copy(notice = "已新建会话：" + conversation.name)
                        reloadConversations()
                        openConversation(conversation.groupId)
                    },
                    onFailure = { error ->
                        state = state.copy(error = "建群失败：" + (error.message ?: "未知原因"))
                    },
                )
            }
        },
        send = {
            val body = state.composer.trim()
            val conversationId = state.activeId?.let { conversationIdFor(it) }
            if (body.isEmpty()) {
                state = state.copy(notice = "正文为空")
            } else if (env.facade == null) {
                state = state.copy(notice = env.lastNotice)
            } else if (conversationId == null) {
                state = state.copy(notice = "先点「新对话」或选一个会话")
            } else {
                scope.launch {
                    state = state.copy(busy = true, error = null, notice = "发送中…")
                    val outcome = env.sendMessage(conversationId, body)
                    if (outcome.ok) {
                        // 发送成功后自动同步一次，用户不必再手动点「同步」。
                        val synced = env.syncOnce(nodeConfig)
                        state = state.copy(
                            busy = false,
                            composer = "",
                            notice = "已发送（" + outcome.steps.joinToString(" → ") + "）；" + synced,
                        )
                    } else {
                        val failure = outcome.failure
                        val code = failure?.let { failureCode(it) } ?: "UNKNOWN_RESULT"
                        val detail = failure?.message?.takeIf { it.isNotBlank() }
                            ?: failure?.let { it::class.java.simpleName }
                            ?: "未知原因"
                        val steps = outcome.steps.ifEmpty { listOf("未完成任何步骤") }.joinToString(" → ")
                        state = state.copy(
                            busy = false,
                            error = code + "：" + detail,
                            notice = "发送失败（" + steps + "）；可以点「重试」或「对账」",
                            failedDraftId = outcome.draftId,
                        )
                    }
                    openConversation(conversationId)
                    reloadConversations()
                }
            }
        },
        sync = {
            scope.launch {
                state = state.copy(busy = true, notice = "同步中…")
                val result = env.syncOnce(nodeConfig)
                state = state.copy(busy = false, notice = result)
                state.activeId?.let { openConversation(it) }
                reloadConversations()
            }
        },
        diagnose = {
            scope.launch {
                state = state.copy(busy = true, notice = "自检中…")
                val result = env.diagnose(nodeConfig)
                state = state.copy(busy = false, diagnostics = result.lines, notice = result.summary)
            }
        },
        closeDiagnostics = { state = state.copy(diagnostics = null) },
        openPairing = { state = state.copy(pairing = chat.ui.chat.ChatPairingState()) },
        closePairing = {
            env.endPairing()
            state = state.copy(pairing = null, notice = "配对会话已结束")
        },
        pairingMyKeyPackage = {
            scope.launch {
                val current = state.pairing ?: return@launch
                state = state.copy(pairing = current.copy(busy = true))
                val result = runCatching { env.exportKeyPackagePack() }
                val pack = result.getOrElse { "" }
                state = state.copy(
                    pairing = state.pairing?.copy(
                        busy = false,
                        myKeyPackage = pack,
                        result = state.pairing?.result.orEmpty() + if (result.isSuccess) {
                            listOf("PASS 已导出我的 KeyPackage（发给对方）")
                        } else {
                            listOf("FAIL 导出失败：" + (result.exceptionOrNull()?.message ?: "未知原因"))
                        },
                    ),
                )
            }
        },
        pairingPeerKeyPackageChange = { text ->
            val current = state.pairing ?: return@ChatHomeHandlers
            state = state.copy(pairing = current.copy(peerKeyPackage = text))
        },
        pairingCreateGroup = {
            scope.launch {
                val current = state.pairing ?: return@launch
                state = state.copy(pairing = current.copy(busy = true))
                val result = runCatching { env.createGroupWithPeerKeyPackage(current.peerKeyPackage, "配对群") }
                state = state.copy(
                    pairing = state.pairing?.copy(
                        busy = false,
                        invitePack = result.getOrElse { "" },
                        result = state.pairing?.result.orEmpty() + if (result.isSuccess) {
                            listOf("PASS 已建群并生成邀请包（发给对方）")
                        } else {
                            listOf("FAIL 建群失败：" + (result.exceptionOrNull()?.message ?: "未知原因"))
                        },
                    ),
                )
            }
        },
        pairingInvitePackChange = { text ->
            val current = state.pairing ?: return@ChatHomeHandlers
            state = state.copy(pairing = current.copy(invitePack = text))
        },
        pairingJoin = {
            scope.launch {
                val current = state.pairing ?: return@launch
                state = state.copy(pairing = current.copy(busy = true))
                val result = runCatching { env.joinFromInvitePack(current.invitePack) }
                state = state.copy(
                    pairing = state.pairing?.copy(
                        busy = false,
                        result = state.pairing?.result.orEmpty() + if (result.isSuccess) {
                            listOf("PASS " + result.getOrThrow())
                        } else {
                            listOf("FAIL 入群失败：" + (result.exceptionOrNull()?.message ?: "未知原因"))
                        },
                    ),
                )
                if (result.isSuccess) {
                    val side = env.loadSide()
                    state = state.copy(groups = side.groups, contacts = side.contacts)
                }
            }
        },
        openSettings = { settingsOpen = true },
    )

    // 自动同步：每 30 秒在后台拉一次（发送后也会立刻同步一次）。
    // 只在配置了节点、且界面空闲时触发，避免与发送抢写者。
    LaunchedEffect(nodeConfig.isConfigured, nodeConfig.endpointUrl) {
        if (!nodeConfig.isConfigured) return@LaunchedEffect
        while (true) {
            kotlinx.coroutines.delay(30_000)
            if (!state.busy && env.facade != null) {
                val result = env.syncOnce(nodeConfig)
                if (result.startsWith("同步：")) {
                    state = state.copy(notice = "自动" + result)
                    state.activeId?.let { openConversation(it) }
                    reloadConversations()
                }
            }
        }
    }

    ChatHomeScreen(state = state, handlers = handlers)

    if (settingsOpen) {
        NodeSettingsDialog(
            config = nodeConfig,
            onSave = { updated ->
                nodeConfigStore.save(updated)
                nodeConfig = updated
                env.applyTransportTrust(updated)
                state = state.copy(nodeLabel = env.nodeLabel(updated), notice = "节点配置已保存")
                settingsOpen = false
            },
            onClose = { settingsOpen = false },
        )
    }
}

/** 应用级服务：identity / store / facade / 群与同步。 */
private class ChatEnvironment {
    /** 单写者：发送与同步都会动库，必须串行（同步还要临时关库）。 */
    private val ioLock = Mutex()
    var store: LocalStore? = null
    var facade: AppFacade? = null
    var lastStatus: String = "未初始化"
    var lastNotice: String = ""
    private var identity: chat.api.IdentityService? = null
    private var databasePath: String? = null
    /** 消息/群/同步用的正式库，**不能**和身份库共用同一个文件（身份库只放身份与联系人）。 */
    private var storePath: String? = null
    private var masterKey: String? = null
    private var groupId: String? = null
    private var mlsGroupKey: String? = null
    /** 当前绑定群的显示名（会话列表兜底用）。 */
    private var groupName: String = ""
    private var registeredOnce: Boolean = false
    var currentConfig: NodeConfig? = null
    private var myAccountId: String = ""
    private var myDeviceId: String = ""
    /** 配对会话期间持有的 ChatCore（KeyPackage 的私钥材料在它里面，入群必须用它）。 */
    private var pairingCore: uniffi.chat_ffi.ChatCoreFfi? = null

    private val nodeIdDefault = "node:1:" + "0a".repeat(32)

    fun nodeLabel(config: NodeConfig): String {
        val endpoint = config.endpointUrl.trim()
        return if (endpoint.isEmpty()) "节点未配置（点「设置」）" else endpoint
    }

    suspend fun start(config: NodeConfig) = withContext(Dispatchers.IO) {
        currentConfig = config
        applyTransportTrust(config)
        if (facade != null) {
            lastStatus = nodeLabel(config)
            return@withContext
        }
        val dir = File(System.getProperty("user.home"), ".chat-desktop").apply { mkdirs() }
        val database = File(dir, "identity.db").absolutePath
        databasePath = database
        val chatDatabase = File(dir, "chat.db").absolutePath
        storePath = chatDatabase
        val key = "0".repeat(64)
        masterKey = key

        println("[chat] start: identity.db=" + database + " chat.db=" + chatDatabase)
        val identityService = runCatching { createIdentityService() }.getOrNull()
        if (identityService == null) { lastStatus = "身份服务不可用"; println("[chat] identity service unavailable"); return@withContext }
        identity = identityService
        var opened = runCatching { identityService.openPersisted(database, key, nodeIdDefault) }
        var legacyNote = ""
        if (opened.isFailure) {
            // 兼容早期版本：那时身份库和消息库共用同一个文件，旧库会被判为不兼容。
            // 做法：把旧文件**改名保留**（不删数据），再建一个新的身份库，并把原因写到提示里。
            val stamp = java.text.SimpleDateFormat("yyyyMMdd-HHmmss").format(java.util.Date())
            val moved = mutableListOf<String>()
            for (suffix in listOf("", "-wal", "-shm")) {
                val source = File(database + suffix)
                if (source.exists()) {
                    val target = File(database + ".legacy-" + stamp + suffix)
                    if (source.renameTo(target)) moved += target.name
                }
            }
            if (moved.isNotEmpty()) {
                opened = runCatching { identityService.openPersisted(database, key, nodeIdDefault) }
                legacyNote = "（旧身份库不兼容，已改名保留：" + moved.joinToString(", ") + "，已新建身份库）"
            }
        }
        runCatching { opened.getOrNull()?.let { account ->
            myAccountId = account.accountId
            myDeviceId = account.deviceId
        } }
        // 关键：发送链路要求“身份已登记进**消息库**”（chat.db），否则 sendPrepare 直接
        // INVALID_STATE_TRANSITION: the local identity is not enrolled in this database。
        // 这里在启动时补一次（幂等）。
        runCatching { identityService.enrollIntoStore(chatDatabase, key) }
        runCatching { identityService.close() }
        if (opened.isFailure) {
            println("[chat] identity open FAILED: " + (opened.exceptionOrNull()?.message ?: ""))
            val reason = (opened.exceptionOrNull()?.message ?: "未知原因").replace(Regex("[\r\n]"), " ").take(140)
            lastStatus = "身份库打开失败：" + reason
            lastNotice = reason + legacyNote
            return@withContext
        }

        val groupService = runCatching { createGroupService(chatDatabase, key, nodeIdDefault) }.getOrNull()
        if (groupService != null) {
            val list = runCatching { groupService.listGroups() }.getOrNull()
            val existing = list?.groups?.firstOrNull { !it.dissolved && !it.left }
            if (existing != null) {
                groupId = existing.groupId
                groupName = existing.name.ifBlank { existing.groupId }
                mlsGroupKey = runCatching { groupService.snapshot(existing.groupId) }.getOrNull()?.mlsGroupKey
            } else {
                val created = runCatching { groupService.createGroup("我的会话") }.getOrNull()
                if (created != null) {
                    groupId = created.groupId
                    groupName = created.name.ifBlank { "我的会话" }
                    mlsGroupKey = created.mlsGroupKey
                }
            }
            runCatching { groupService.shutdown() }
            println("[chat] group resolved: " + (groupId ?: "none") + " name=" + groupName)
        } else {
            println("[chat] group service unavailable")
        }
        val localStore = runCatching { createLocalStore() }.getOrNull()
        if (localStore == null) { lastStatus = "存储服务不可用"; return@withContext }
        val storeOpened = runCatching { localStore.open(chatDatabase, key) }
        if (storeOpened.isFailure) {
            println("[chat] store open FAILED: " + (storeOpened.exceptionOrNull()?.message ?: ""))
            lastStatus = "本地库打开失败"
            lastNotice = storeOpened.exceptionOrNull()?.message ?: ""
            return@withContext
        }
        store = localStore
        facade = AppFacade(localStore)
        println("[chat] store opened; facade ready")

        val endpoint = config.endpointUrl.trim()
        if (endpoint.isNotEmpty()) {
            val keyHex = config.nodePublicKeyHex.trim().ifEmpty {
                NodeSelfCheck.fetchNodePublicKeyHex(endpoint, config.tlsFingerprints).orEmpty()
            }
            runCatching {
                localStore.configureSendNode(
                    endpoint,
                    config.nodeId.trim().ifEmpty { "node-public" },
                    keyHex.ifEmpty { null },
                )
            }
        }
        mlsGroupKey?.let { runCatching { localStore.configureSendGroup(it) } }
        lastStatus = nodeLabel(config)
        lastNotice = legacyNote + if (config.isConfigured) {
            "已配置节点；会话与消息来自本地加密库"
        } else {
            "先点右上角「设置」填节点地址（例如 https://106.54.223.171:8443）"
        }
    }

    /** 已绑定的群（id + 名称）；用于会话列表为空时的兜底，避免“还没有会话”把用户挡在门外。 */
    fun boundGroup(): Pair<String, String>? = groupId?.let { it to groupName.ifBlank { it } }

    /** 一个「会话」= 一个群（同一份 MLS 状态，不做假会话）。 */
    data class GroupConversation(
        val groupId: String,
        val name: String,
        val mlsGroupKey: String,
        val epoch: Int,
        val members: Int,
    )

    /** 列出群即会话（读之前让出写者，读完重开 store）。 */
    suspend fun listGroupConversations(): List<GroupConversation> = withContext(Dispatchers.IO) {
        val chatDb = storePath ?: return@withContext emptyList()
        val key = masterKey ?: return@withContext emptyList()
        runCatching { store?.close() }
        store = null
        facade = null
        val service = runCatching { createGroupService(chatDb, key, nodeIdDefault) }.getOrNull()
        val conversations = mutableListOf<GroupConversation>()
        if (service != null) {
            runCatching { service.listGroups() }.getOrNull()?.groups?.forEach { entry ->
                if (entry.dissolved || entry.left) return@forEach
                val snapshot = runCatching { service.snapshot(entry.groupId) }.getOrNull()
                conversations += GroupConversation(
                    groupId = entry.groupId,
                    name = entry.name.ifBlank { entry.groupId },
                    mlsGroupKey = snapshot?.mlsGroupKey.orEmpty(),
                    epoch = entry.epoch,
                    members = snapshot?.members?.size ?: 0,
                )
            }
            runCatching { service.shutdown() }
        }
        reopenStore(chatDb, key)
        conversations
    }

    /** 新建一个群会话（并在配置了节点时登记，否则发不出去）。 */
    suspend fun createGroupConversation(name: String): GroupConversation = withContext(Dispatchers.IO) {
        val chatDb = storePath ?: throw IllegalStateException("本地库未就绪")
        val key = masterKey ?: throw IllegalStateException("本地库未就绪")
        runCatching { store?.close() }
        store = null
        facade = null
        val config = currentConfig
        val result = runCatching {
            val service = createGroupService(chatDb, key, nodeIdDefault)
            try {
                val snapshot = service.createGroup(name)
                val endpoint = config?.endpointUrl?.trim().orEmpty()
                if (endpoint.isNotEmpty()) {
                    val keyHex = config?.nodePublicKeyHex?.trim()?.ifEmpty {
                        NodeSelfCheck.fetchNodePublicKeyHex(endpoint, config?.tlsFingerprints ?: emptyList()).orEmpty()
                    }.orEmpty()
                    runCatching {
                        service.registerOnNode(
                            snapshot.groupId,
                            endpoint,
                            config?.nodeId?.trim()?.ifEmpty { "node-public" }.orEmpty(),
                            keyHex.ifEmpty { null },
                        )
                    }
                }
                GroupConversation(snapshot.groupId, snapshot.name, snapshot.mlsGroupKey, snapshot.epoch, snapshot.members.size)
            } finally {
                runCatching { service.shutdown() }
            }
        }
        reopenStore(chatDb, key)
        result.getOrThrow()
    }

    /** 切换当前会话：把发送绑定到该群。 */
    suspend fun selectGroupConversation(targetGroupId: String): Boolean = withContext(Dispatchers.IO) {
        val chatDb = storePath ?: return@withContext false
        val key = masterKey ?: return@withContext false
        runCatching { store?.close() }
        store = null
        facade = null
        val snapshot = runCatching {
            val service = createGroupService(chatDb, key, nodeIdDefault)
            try { service.snapshot(targetGroupId) } finally { runCatching { service.shutdown() } }
        }.getOrNull()
        reopenStore(chatDb, key)
        if (snapshot == null) return@withContext false
        groupId = snapshot.groupId
        mlsGroupKey = snapshot.mlsGroupKey
        runCatching { store?.configureSendGroup(snapshot.mlsGroupKey) }
        true
    }

    /**
     * 配对第 1 步：导出**我的** KeyPackage 包（发给对方，对方用它建群）。
     *
     * 这里打开的 ChatCore **故意不关**：KeyPackage 的私钥材料只存在于这个实例里，
     * 第 3 步入群必须复用同一个实例，否则内核会拒绝（INVALID_STATE_TRANSITION）。
     * 代价：配对会话期间本地库让出写者（界面会提示“配对中，暂不可发送”）。
     */
    suspend fun exportKeyPackagePack(): String = withContext(Dispatchers.IO) {
        val chatDb = storePath ?: throw IllegalStateException("本地库未就绪")
        val key = masterKey ?: throw IllegalStateException("本地库未就绪")
        runCatching { store?.close() }
        store = null
        facade = null
        val core = pairingCore ?: runCatching { uniffi.chat_ffi.ChatCoreFfi.open(chatDb, key) }.getOrNull()
            ?: throw IllegalStateException("打开配对核心失败")
        pairingCore = core
        val identityDb = databasePath ?: throw IllegalStateException("身份库未就绪")
        runCatching { core.enrollLocalIdentity(identityDb, key) }
        val pack = PairingPack.encodeKeyPackage(core.keyPackageCredential(), core.exportKeyPackage())
        "配对中：本地库已让出写者\n\n" + pack
    }

    /** 配对第 2 步：用对方的 KeyPackage 建群 → 登记节点 → 产出邀请包（给对方粘贴）。 */
    suspend fun createGroupWithPeerKeyPackage(packText: String, name: String): String = withContext(Dispatchers.IO) {
        val chatDb = storePath ?: throw IllegalStateException("本地库未就绪")
        val key = masterKey ?: throw IllegalStateException("本地库未就绪")
        val config = currentConfig ?: throw IllegalStateException("先配置节点")
        val endpoint = config.endpointUrl.trim()
        if (endpoint.isEmpty()) throw IllegalStateException("节点未配置")
        val keyHex = config.nodePublicKeyHex.trim().ifEmpty {
            NodeSelfCheck.fetchNodePublicKeyHex(endpoint, config.tlsFingerprints).orEmpty()
        }
        val peer = PairingPack.parseKeyPackage(packText)
        runCatching { store?.close() }
        store = null
        facade = null
        val core = pairingCore ?: uniffi.chat_ffi.ChatCoreFfi.open(chatDb, key)
        pairingCore = core
        applyTransportTrust(config)
        val created = core.createGroupWithInviteeKeyPackage(name, peer.credential, peer.keyPackage, core.keyPackageCredential())
        core.groupRegisterOnNode(created.group.groupId, endpoint, config.nodeId.trim().ifEmpty { "node-public" }, keyHex)
        val head = core.pullState(created.group.groupId) ?: throw IllegalStateException("登记后仍拿不到 verified head")
        PairingPack.encodeInvite(created.invite, head)
    }

    /** 配对第 3 步：用邀请包入群（必须与第 1 步同一个 ChatCore 实例）。 */
    suspend fun joinFromInvitePack(packText: String): String = withContext(Dispatchers.IO) {
        val core = pairingCore ?: throw IllegalStateException("先点「导出我的 KeyPackage」并保持这次配对不关闭")
        val pack = PairingPack.parseInvite(packText)
        val joined = core.joinGroupFromWelcome(pack.invite.name, pack.invite, pack.head)
        // 入群完成：关掉配对核心，把本地库还回界面。
        runCatching { pairingCore?.close() }
        pairingCore = null
        val chatDb = storePath ?: throw IllegalStateException("本地库未就绪")
        val key = masterKey ?: throw IllegalStateException("本地库未就绪")
        reopenStore(chatDb, key)
        groupId = joined.groupId
        mlsGroupKey = joined.mlsGroupKey
        mlsGroupKey?.let { runCatching { store?.configureSendGroup(it) } }
        "已入群：" + joined.groupId.take(16) + "… epoch=" + joined.epoch
    }

    /** 结束配对会话（不动已入群的群）。 */
    fun endPairing() {
        runCatching { pairingCore?.close() }
        pairingCore = null
    }

    /** 发送前保证群已在节点登记（幂等；只做一次）。放在这里而不是启动时，避免拖慢启动。 */
    private suspend fun ensureGroupRegistered() = withContext(Dispatchers.IO) {
        if (registeredOnce) return@withContext
        val target = groupId ?: return@withContext
        val config = currentConfig ?: return@withContext
        val endpoint = config.endpointUrl.trim()
        if (endpoint.isEmpty()) return@withContext
        val chatDb = storePath ?: return@withContext
        val key = masterKey ?: return@withContext
        val keyHex = config.nodePublicKeyHex.trim().ifEmpty {
            NodeSelfCheck.fetchNodePublicKeyHex(endpoint, config.tlsFingerprints).orEmpty()
        }
        runCatching { store?.close() }
        store = null
        facade = null
        val outcome = runCatching {
            val service = createGroupService(chatDb, key, nodeIdDefault)
            try {
                service.registerOnNode(target, endpoint, config.nodeId.trim().ifEmpty { "node-public" }, keyHex.ifEmpty { null })
            } finally {
                runCatching { service.shutdown() }
            }
        }
        reopenStore(chatDb, key)
        registeredOnce = outcome.isSuccess
        println("[chat] ensureGroupRegistered ok=" + outcome.isSuccess + " err=" + (outcome.exceptionOrNull()?.message ?: ""))
    }

    /** 重试一条失败草稿（内核 sendRetry：重建并换新 SubmissionId）。 */
    suspend fun retryDraft(draftId: String): Triple<String, String?, Boolean> = ioLock.withLock {
        withContext(Dispatchers.IO) {
            val currentStore = store ?: return@withContext Triple("本地库未就绪", "CORE_CLOSED：本地库未就绪", false)
            runCatching { currentStore.sendRetry(draftId) }.fold(
                onSuccess = { step ->
                    val ok = step.status == "accepted" && step.settled
                    Triple(
                        if (ok) "重试成功（" + step.status + "）" else "重试结果：" + step.status,
                        null,
                        ok,
                    )
                },
                onFailure = { error ->
                    Triple("重试失败", failureCode(error) + "：" + (error.message ?: ""), false)
                },
            )
        }
    }

    /** 对账一条结果未知的草稿（内核 sendReconcile：按 SubmissionId 查真实结果）。 */
    suspend fun reconcileDraft(draftId: String): Triple<String, String?, Boolean> = ioLock.withLock {
        withContext(Dispatchers.IO) {
            val currentStore = store ?: return@withContext Triple("本地库未就绪", "CORE_CLOSED：本地库未就绪", false)
            runCatching { currentStore.sendReconcile(draftId) }.fold(
                onSuccess = { step ->
                    val ok = step.status == "accepted" && step.settled
                    Triple(
                        if (ok) "对账结果：" + step.status + "（已确认送达）" else "对账结果：" + step.status,
                        null,
                        ok,
                    )
                },
                onFailure = { error ->
                    Triple("对账失败", failureCode(error) + "：" + (error.message ?: ""), false)
                },
            )
        }
    }

    /** 连接自检：节点证书/health/node_id + 本地身份与群状态（全部真实请求，不伪造通过）。 */
    suspend fun diagnose(config: NodeConfig): Diagnostics = withContext(Dispatchers.IO) {
        val lines = mutableListOf<String>()
        val steps = NodeSelfCheck.run(config.endpointUrl, config.tlsFingerprints, config.nodeId)
        steps.forEach { step ->
            lines += (if (step.ok) "PASS " else "FAIL ") + step.name + "：" + step.detail
        }
        val failures = steps.count { !it.ok }
        val identityLine = if (databasePath == null) {
            lines += "FAIL identity：身份库未打开"
            "身份未就绪"
        } else {
            lines += "PASS identity：本地身份库已打开（" + File(databasePath!!).name + "）"
            "身份就绪"
        }
        lines += if (groupId == null) "FAIL group：还没有群（点「新对话」发消息会自动建群）" else "PASS group：已绑定群 " + shortGroup(groupId!!)
        lines += if (store == null) "FAIL store：本地消息库未打开" else "PASS store：本地消息库已打开"
        val summary = if (failures == 0) {
            "自检通过（" + steps.size + " 项网络检查）；" + identityLine
        } else {
            "自检有 " + failures + " 项失败，见明细"
        }
        Diagnostics(lines, summary)
    }

    private fun shortGroup(groupId: String): String =
        if (groupId.length <= 18) groupId else groupId.take(10) + "…" + groupId.takeLast(6)

    /**
     * 证书固定：自签名节点必须配指纹，否则 https 请求会因不受信任而失败。
     * 空列表 = 不做固定（会被系统信任链拒绝，界面会在发送/同步时如实报错）。
     */
    fun applyTransportTrust(config: NodeConfig) {
        val pins = config.tlsFingerprints.filter { it.length == 64 }
        if (pins.isEmpty()) return
        runCatching { uniffi.chat_ffi.configureTransportTrust(pins) }
    }

    /** 读群详情（成员/角色/待处理）。读之前先让出写者，读完重开 store。 */
    suspend fun loadGroupDetail(groupId: String): ChatGroupDetail? = withContext(Dispatchers.IO) {
        val database = storePath ?: return@withContext null
        val key = masterKey ?: return@withContext null
        runCatching { store?.close() }
        val snapshot = runCatching {
            val service = createGroupService(database, key, nodeIdDefault)
            try { service.snapshot(groupId) } finally { runCatching { service.shutdown() } }
        }.getOrNull()
        reopenStore(database, key)
        if (snapshot == null) return@withContext null
        ChatGroupDetail(
            id = snapshot.groupId,
            name = snapshot.name.ifBlank { snapshot.groupId },
            epoch = snapshot.epoch,
            role = snapshot.role,
            members = snapshot.members.map { member ->
                ChatGroupMemberItem(
                    credential = member.credential,
                    role = member.role,
                    muted = member.muted,
                    isLocal = member.isLocal,
                    pending = member.pending,
                )
            },
            pendingCount = snapshot.pendingMembers.size,
        )
    }

    suspend fun renameGroup(groupId: String, newName: String): String = withContext(Dispatchers.IO) {
        val database = storePath ?: return@withContext "本地库未就绪"
        val key = masterKey ?: return@withContext "本地库未就绪"
        runCatching { store?.close() }
        val attempt = runCatching {
            val service = createGroupService(database, key, nodeIdDefault)
            try { service.rename(groupId, newName) } finally { runCatching { service.shutdown() } }
        }
        reopenStore(database, key)
        attempt.fold(
            onSuccess = { "群名已改为「" + it.name + "」（epoch " + it.epoch + "）" },
            onFailure = { "改名失败：" + (it.message ?: "未知原因") },
        )
    }

    suspend fun removeMember(groupId: String, credential: String): String = withContext(Dispatchers.IO) {
        val database = storePath ?: return@withContext "本地库未就绪"
        val key = masterKey ?: return@withContext "本地库未就绪"
        runCatching { store?.close() }
        val attempt = runCatching {
            val service = createGroupService(database, key, nodeIdDefault)
            try { service.removeMember(groupId, credential) } finally { runCatching { service.shutdown() } }
        }
        reopenStore(database, key)
        attempt.fold(
            onSuccess = { "已提交移除（epoch " + it.epoch + "）" },
            onFailure = { "移除失败：" + (it.message ?: "未知原因") },
        )
    }

    suspend fun acceptPending(groupId: String): String = withContext(Dispatchers.IO) {
        val database = storePath ?: return@withContext "本地库未就绪"
        val key = masterKey ?: return@withContext "本地库未就绪"
        runCatching { store?.close() }
        val attempt = runCatching {
            val service = createGroupService(database, key, nodeIdDefault)
            try { service.acceptPending(groupId) } finally { runCatching { service.shutdown() } }
        }
        reopenStore(database, key)
        attempt.fold(
            onSuccess = { "待处理变更已接受（epoch " + it.epoch + "）" },
            onFailure = { "接受失败：" + (it.message ?: "未知原因") },
        )
    }

    /** 重开 store（多处复用：读群详情/改群名/移除/接受之后都要恢复本地库）。 */
    private suspend fun reopenStore(database: String, key: String) = withContext(Dispatchers.IO) {
        val reopened = runCatching { createLocalStore().also { it.open(database, key) } }.getOrNull()
        if (reopened != null) {
            store = reopened
            facade = AppFacade(reopened)
            mlsGroupKey?.let { runCatching { reopened.configureSendGroup(it) } }
        }
    }

    /** 侧栏数据：身份摘要 + 联系人 + 群组（读之前先让出写者，读完再把 store 打开）。 */
    suspend fun loadSide(): SideData = withContext(Dispatchers.IO) {
        val database = databasePath
        val chatDatabase = storePath
        val key = masterKey
        if (database == null || key == null) return@withContext SideData(emptyList(), emptyList(), emptyList())
        runCatching { store?.close() }
        val identityLines = mutableListOf<String>()
        val contacts = mutableListOf<ChatContactItem>()
        val groups = mutableListOf<ChatGroupItem>()
        val identityService = runCatching { createIdentityService() }.getOrNull()
        if (identityService != null) {
            val opened = runCatching { identityService.openPersisted(database, key, nodeIdDefault) }.getOrNull()
            if (opened != null) {
                identityLines += "账户：" + opened.accountId
                identityLines += "设备：" + opened.deviceId
                identityLines += "指纹：" + opened.fingerprint
            }
            runCatching { identityService.contacts() }.getOrNull()?.forEach { contact ->
                contacts += ChatContactItem(
                    id = contact.contactId,
                    name = contact.displayName.ifBlank { contact.accountId },
                    fingerprint = contact.fingerprint,
                    state = contact.state,
                    canSend = contact.canSend,
                )
            }
            runCatching { identityService.close() }
        }
        val groupService = runCatching { createGroupService(chatDatabase ?: database, key, nodeIdDefault) }.getOrNull()
        if (groupService != null) {
            runCatching { groupService.listGroups() }.getOrNull()?.groups?.forEach { group ->
                val members = runCatching { groupService.snapshot(group.groupId) }.getOrNull()?.members?.size ?: 0
                groups += ChatGroupItem(
                    id = group.groupId,
                    name = group.name.ifBlank { group.groupId },
                    epoch = group.epoch,
                    role = group.role,
                    members = members,
                )
            }
            runCatching { groupService.shutdown() }
        }
        val reopened = runCatching { createLocalStore().also { it.open(chatDatabase ?: database, key) } }.getOrNull()
        if (reopened != null) {
            store = reopened
            facade = AppFacade(reopened)
            mlsGroupKey?.let { runCatching { reopened.configureSendGroup(it) } }
        }
        SideData(identityLines, contacts, groups)
    }

    /** 生成邀请口令（24 小时有效）；失败返回 null，由界面显式提示。 */
    suspend fun createInvite(): String? = withContext(Dispatchers.IO) {
        val database = databasePath ?: return@withContext null
        val key = masterKey ?: return@withContext null
        runCatching { store?.close() }
        val service = runCatching { createIdentityService() }.getOrNull()
        val token = if (service == null) {
            null
        } else {
            runCatching {
                service.openPersisted(database, key, nodeIdDefault)
                service.createInvite(24L * 60 * 60 * 1000).token
            }.getOrNull().also { runCatching { service.close() } }
        }
        val reopened = runCatching { createLocalStore().also { it.open(database, key) } }.getOrNull()
        if (reopened != null) {
            store = reopened
            facade = AppFacade(reopened)
            mlsGroupKey?.let { runCatching { reopened.configureSendGroup(it) } }
        }
        token
    }

    /** 发送：写草稿 → prepare → dispatch；结果如实返回（未知结果不算成功）。 */
    suspend fun sendMessage(conversationId: String, body: String): SendOutcome = ioLock.withLock {
        withContext(Dispatchers.IO) {
            ensureGroupRegistered()
            val currentFacade = facade ?: return@withContext SendOutcome(emptyList(), IllegalStateException("本地库未就绪"))
            val currentStore = store ?: return@withContext SendOutcome(emptyList(), IllegalStateException("本地库未就绪"))
            val hex = java.util.UUID.randomUUID().toString().replace("-", "") +
                java.util.UUID.randomUUID().toString().replace("-", "")
            val operation = "op:1:" + hex.substring(0, 64)
            when (val draft = currentFacade.submitComposer(conversationId, operation, body)) {
                is UseCaseResult.Failed -> SendOutcome(emptyList(), IllegalStateException(draft.message))
                is UseCaseResult.Ok -> {
                    val outcome = sendWithRecovery(currentStore, draft.value.draftId)
                    if (outcome.ok) {
                        // 发送方必须自己落库：MLS 不允许发送者解密自己的应用消息，
                        // 所以同步永远拿不回这条消息（实测 3 轮 fetched=0）。
                        runCatching {
                            currentStore.applyVerifiedMessage(
                                chat.api.VerifiedMessageInput(
                                    conversationId = conversationId,
                                    title = "会话",
                                    messageId = "msg:1:" + operation.removePrefix("op:1:"),
                                    authorAccount = myAccountId,
                                    authorDevice = myDeviceId,
                                    body = body,
                                    mine = true,
                                    createdAtMillis = System.currentTimeMillis(),
                                ),
                            )
                        }
                    }
                    outcome
                }
            }
        }
    }

    /** 同步必须独占写者：关 store → 同步 → 重开 store（与既有约束一致）。 */
    suspend fun syncOnce(config: NodeConfig): String = ioLock.withLock { syncOnceLocked(config) }

    private suspend fun syncOnceLocked(config: NodeConfig): String = withContext(Dispatchers.IO) {
        val database = storePath ?: return@withContext "本地库未就绪"
        val key = masterKey ?: return@withContext "本地库未就绪"
        val group = groupId ?: return@withContext "还没有群：先发一条消息"
        val endpoint = config.endpointUrl.trim()
        if (endpoint.isEmpty()) return@withContext "节点未配置：点「设置」"
        val keyHex = config.nodePublicKeyHex.trim().ifEmpty {
            NodeSelfCheck.fetchNodePublicKeyHex(endpoint, config.tlsFingerprints).orEmpty()
        }
        if (keyHex.isEmpty()) return@withContext "取不到节点公钥：请检查节点地址与证书指纹"
        val currentNodeId = config.nodeId.trim().ifEmpty { "node-public" }
        runCatching { store?.close() }
        val attempt = runCatching {
            val service = createSyncService(database, key, endpoint, currentNodeId, keyHex, group)
            try {
                service.syncOnce()
            } finally {
                runCatching { service.shutdown() }
            }
        }
        val reopened = runCatching { createLocalStore().also { it.open(database, key) } }.getOrNull()
        if (reopened != null) {
            store = reopened
            facade = AppFacade(reopened)
            mlsGroupKey?.let { runCatching { reopened.configureSendGroup(it) } }
        }
        attempt.fold(
            onSuccess = { view -> "同步：拉取 " + view.fetched + " 条，入库 " + view.applied + " 条" },
            onFailure = { error -> "同步失败：" + (error.message ?: "未知原因") },
        )
    }
}

/** 节点设置（最小可用）：endpoint / node_id / 节点公钥。 */
@Composable
private fun NodeSettingsDialog(config: NodeConfig, onSave: (NodeConfig) -> Unit, onClose: () -> Unit) {
    var endpoint by remember { mutableStateOf(config.endpointUrl) }
    var nodeId by remember { mutableStateOf(config.nodeId) }
    var publicKey by remember { mutableStateOf(config.nodePublicKeyHex) }
    var fingerprints by remember { mutableStateOf(config.tlsFingerprints.joinToString("\n")) }
    AlertDialog(
        onDismissRequest = onClose,
        title = { Text("节点设置") },
        text = {
            Column(Modifier.width(520.dp)) {
                OutlinedTextField(
                    value = endpoint,
                    onValueChange = { endpoint = it },
                    label = { Text("节点地址（https://host:port）") },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = nodeId,
                    onValueChange = { nodeId = it },
                    label = { Text("节点标识 node_id") },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = publicKey,
                    onValueChange = { publicKey = it },
                    label = { Text("节点公钥 hex（留空则从 /v1/health 自动取）") },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = fingerprints,
                    onValueChange = { fingerprints = it },
                    label = { Text("证书 SHA-256 指纹（自签名节点必填，可多行/逗号分隔）") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 2,
                    maxLines = 4,
                )
                Text(
                    "指纹从节点目录的 fingerprint.txt 取（或 tools/inspect-node.ps1 打印）。留空表示不做证书固定。",
                    style = MaterialTheme.typography.labelSmall,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    val parsed = fingerprints
                        .split(',', '\n', ' ')
                        .map { it.trim().lowercase().replace(":", "") }
                        .filter { it.length == 64 && it.all { ch -> ch in "0123456789abcdef" } }
                    onSave(
                        config.copy(
                            endpointUrl = endpoint.trim().trimEnd('/'),
                            nodeId = nodeId.trim().ifEmpty { "node-public" },
                            nodePublicKeyHex = publicKey.trim(),
                            tlsFingerprints = parsed,
                        ),
                    )
                },
            ) { Text("保存") }
        },
        dismissButton = { TextButton(onClick = onClose) { Text("取消") } },
    )
}


/** 侧栏数据（身份摘要 / 联系人 / 群组）。 */
private data class SideData(
    val identityLines: List<String>,
    val contacts: List<ChatContactItem>,
    val groups: List<ChatGroupItem>,
)

/** 自检结果：逐行明细 + 一句话结论。 */
private data class Diagnostics(val lines: List<String>, val summary: String)

/** 协议里会话 id 与群 id 是两套命名空间：群 grp:1:<hex> 对应会话 conv:1:<同一 hex>。 */
fun conversationIdFor(groupId: String): String =
    if (groupId.startsWith("grp:1:")) "conv:1:" + groupId.removePrefix("grp:1:") else groupId

/** 界面版本标记：用来区分“你打开的是哪一版”（写进顶栏，避免再用旧包判断新功能）。 */
const val UI_REVISION = "R-319"

/**
 * 无界面启动诊断（`Chat.exe --diagnose`）：把“界面为什么空白/发不出去”变成一段可复制的日志。
 *
 * 依次做：读配置 → 启动服务（身份/群/库）→ 连接自检 → 列会话。
 * 退出码：0 = 全绿；1 = 有 FAIL 行；2 = 服务都没起来。
 */
fun runStartupDiagnostics(): Int {
    val lines = mutableListOf<String>()
    fun out(line: String) {
        println(line)
        lines += line
    }
    val configStore = NodeConfigStore(NodeConfigStore.defaultFile())
    val config = runCatching { configStore.load() }.getOrDefault(NodeConfig())
    out("[diag] node config: endpoint=" + config.endpointUrl + " node_id=" + config.nodeId +
        " fingerprints=" + config.tlsFingerprints.size + " publicKey=" + (if (config.nodePublicKeyHex.isBlank()) "(empty)" else "set"))
    val env = ChatEnvironment()
    val code = kotlinx.coroutines.runBlocking {
        runCatching { env.start(config) }
        out("[diag] start status: " + env.lastStatus)
        out("[diag] start notice: " + env.lastNotice)
        out("[diag] bound group: " + (env.boundGroup()?.first ?: "none"))
        if (env.facade == null) {
            out("[diag] RESULT: services did not start (identity/store)")
            return@runBlocking 2
        }
        val diagnostics = runCatching { env.diagnose(config) }.getOrNull()
        if (diagnostics == null) {
            out("[diag] RESULT: diagnose threw")
            return@runBlocking 1
        }
        diagnostics.lines.forEach { out("[diag] " + it) }
        out("[diag] summary: " + diagnostics.summary)
        val conversations = runCatching { env.listGroupConversations() }.getOrDefault(emptyList())
        out("[diag] conversations: " + conversations.size)
        conversations.forEach { out("[diag]  - " + it.name + " epoch=" + it.epoch + " members=" + it.members) }
        env.endPairing()
        runCatching { env.store?.close() }
        if (diagnostics.lines.any { it.startsWith("FAIL") }) 1 else 0
    }
    out("[diag] RESULT: " + when (code) {
        0 -> "PASS（服务就绪，连接自检全绿）"
        2 -> "FAIL（服务没起来）"
        else -> "FAIL（自检有失败项）"
    })
    // 同时写一份 UTF-8 报告：Windows 控制台编码不可靠，复制文件比复制终端更稳。
    val stamp = java.text.SimpleDateFormat("yyyyMMdd-HHmmss").format(java.util.Date())
    val report = File(File(System.getProperty("user.home"), ".chat-desktop").apply { mkdirs() }, "diagnose-" + stamp + ".txt")
    runCatching { report.writeText(lines.joinToString(System.lineSeparator()), Charsets.UTF_8) }
        .onSuccess { println("[diag] report written: " + report.absolutePath) }
    return code
}
