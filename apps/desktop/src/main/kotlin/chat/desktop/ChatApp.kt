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
import chat.ui.chat.ChatGroupDetail
import chat.ui.chat.ChatGroupMemberItem
import chat.ui.chat.GroupDetailDialog
import chat.ui.chat.ChatConversationItem
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
    var state by remember { mutableStateOf(ChatHomeState()) }
    var settingsOpen by remember { mutableStateOf(false) }
    val env = remember { ChatEnvironment() }

    fun reloadConversations() {
        scope.launch {
            val facade = env.facade ?: return@launch
            when (val loaded = facade.bootstrap(50)) {
                is UseCaseResult.Ok -> {
                    val items = loaded.value.conversations.map {
                        ChatConversationItem(it.conversationId, it.title.ifBlank { it.conversationId }, it.preview)
                    }
                    state = state.copy(conversations = items, loading = false)
                }
                is UseCaseResult.Failed -> state = state.copy(loading = false, error = loaded.message)
            }
        }
    }

    fun openConversation(conversationId: String) {
        scope.launch {
            val facade = env.facade ?: return@launch
            state = state.copy(activeId = conversationId, busy = true, error = null)
            when (val opened = facade.openConversation(conversationId, null, 100)) {
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
                is UseCaseResult.Failed -> state = state.copy(busy = false, error = opened.message)
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
            val hex = UUID.randomUUID().toString().replace("-", "") + UUID.randomUUID().toString().replace("-", "")
            state = state.copy(
                activeId = "conv:1:" + hex.substring(0, 64),
                messages = emptyList(),
                composer = "",
                notice = "新对话：输入内容后点发送",
                error = null,
            )
        },
        send = {
            val body = state.composer.trim()
            val conversationId = state.activeId
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
                        val code = outcome.failure?.let { failureCode(it) } ?: "UNKNOWN"
                        state = state.copy(
                            busy = false,
                            error = code + "：" + (outcome.failure?.message ?: "未知原因"),
                            notice = "发送失败（" + outcome.steps.joinToString(" → ") + "）",
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

    state.groupDetail?.let { detail ->
        GroupDetailDialog(detail = detail, handlers = handlers, onClose = { state = state.copy(groupDetail = null) })
    }

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
    private var masterKey: String? = null
    private var groupId: String? = null
    private var mlsGroupKey: String? = null

    private val nodeIdDefault = "node:1:" + "0a".repeat(32)

    fun nodeLabel(config: NodeConfig): String {
        val endpoint = config.endpointUrl.trim()
        return if (endpoint.isEmpty()) "节点未配置（点「设置」）" else endpoint
    }

    suspend fun start(config: NodeConfig) = withContext(Dispatchers.IO) {
        applyTransportTrust(config)
        if (facade != null) {
            lastStatus = nodeLabel(config)
            return@withContext
        }
        val dir = File(System.getProperty("user.home"), ".chat-desktop").apply { mkdirs() }
        val database = File(dir, "identity.db").absolutePath
        databasePath = database
        val key = "0".repeat(64)
        masterKey = key

        val identityService = runCatching { createIdentityService() }.getOrNull()
        if (identityService == null) { lastStatus = "身份服务不可用"; return@withContext }
        identity = identityService
        val opened = runCatching { identityService.openPersisted(database, key, nodeIdDefault) }
        runCatching { identityService.close() }
        if (opened.isFailure) {
            lastStatus = "身份库打开失败";
            lastNotice = opened.exceptionOrNull()?.message ?: ""
            return@withContext
        }

        val groupService = runCatching { createGroupService(database, key, nodeIdDefault) }.getOrNull()
        if (groupService != null) {
            val list = runCatching { groupService.listGroups() }.getOrNull()
            val existing = list?.groups?.firstOrNull { !it.dissolved && !it.left }
            if (existing != null) {
                groupId = existing.groupId
                mlsGroupKey = runCatching { groupService.snapshot(existing.groupId) }.getOrNull()?.mlsGroupKey
            } else {
                val created = runCatching { groupService.createGroup("我的会话") }.getOrNull()
                if (created != null) {
                    groupId = created.groupId
                    mlsGroupKey = created.mlsGroupKey
                }
            }
            runCatching { groupService.shutdown() }
        }

        val localStore = runCatching { createLocalStore() }.getOrNull()
        if (localStore == null) { lastStatus = "存储服务不可用"; return@withContext }
        val storeOpened = runCatching { localStore.open(database, key) }
        if (storeOpened.isFailure) {
            lastStatus = "本地库打开失败";
            lastNotice = storeOpened.exceptionOrNull()?.message ?: ""
            return@withContext
        }
        store = localStore
        facade = AppFacade(localStore)

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
        lastNotice = if (config.isConfigured) {
            "已配置节点；会话与消息来自本地加密库"
        } else {
            "先点右上角「设置」填节点地址（例如 https://106.54.223.171:8443）"
        }
    }

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
        val database = databasePath ?: return@withContext null
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
        val database = databasePath ?: return@withContext "本地库未就绪"
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
        val database = databasePath ?: return@withContext "本地库未就绪"
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
        val database = databasePath ?: return@withContext "本地库未就绪"
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
        val groupService = runCatching { createGroupService(database, key, nodeIdDefault) }.getOrNull()
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
        val reopened = runCatching { createLocalStore().also { it.open(database, key) } }.getOrNull()
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
            val currentFacade = facade ?: return@withContext SendOutcome(emptyList(), IllegalStateException("本地库未就绪"))
            val currentStore = store ?: return@withContext SendOutcome(emptyList(), IllegalStateException("本地库未就绪"))
            val hex = java.util.UUID.randomUUID().toString().replace("-", "") +
                java.util.UUID.randomUUID().toString().replace("-", "")
            when (val draft = currentFacade.submitComposer(conversationId, "op:1:" + hex.substring(0, 64), body)) {
                is UseCaseResult.Failed -> SendOutcome(emptyList(), IllegalStateException(draft.message))
                is UseCaseResult.Ok -> sendWithRecovery(currentStore, draft.value.draftId)
            }
        }
    }

    /** 同步必须独占写者：关 store → 同步 → 重开 store（与既有约束一致）。 */
    suspend fun syncOnce(config: NodeConfig): String = ioLock.withLock { syncOnceLocked(config) }

    private suspend fun syncOnceLocked(config: NodeConfig): String = withContext(Dispatchers.IO) {
        val database = databasePath ?: return@withContext "本地库未就绪"
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
