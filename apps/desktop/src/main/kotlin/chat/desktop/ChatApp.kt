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
import chat.ui.chat.ChatGroupItem
import chat.ui.chat.ChatHomeHandlers
import chat.ui.chat.ChatHomeScreen
import chat.ui.chat.ChatHomeState
import chat.ui.chat.ChatMessageItem
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
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
            val facade = env.facade
            val store = env.store
            val conversationId = state.activeId
            if (body.isEmpty()) {
                state = state.copy(notice = "正文为空")
            } else if (facade == null || store == null) {
                state = state.copy(notice = env.lastNotice)
            } else if (conversationId == null) {
                state = state.copy(notice = "先点「新对话」或选一个会话")
            } else {
                scope.launch {
                    state = state.copy(busy = true, error = null, notice = "发送中…")
                    val hex = UUID.randomUUID().toString().replace("-", "") + UUID.randomUUID().toString().replace("-", "")
                    when (val draft = facade.submitComposer(conversationId, "op:1:" + hex.substring(0, 64), body)) {
                        is UseCaseResult.Failed -> state = state.copy(busy = false, error = draft.message)
                        is UseCaseResult.Ok -> {
                            val outcome = sendWithRecovery(store, draft.value.draftId)
                            val notice = if (outcome.ok) {
                                "已发送（" + outcome.steps.joinToString(" → ") + "）"
                            } else {
                                "发送失败：" + (outcome.failure?.message ?: "未知原因")
                            }
                            state = state.copy(busy = false, composer = "", notice = notice)
                            openConversation(conversationId)
                            reloadConversations()
                        }
                    }
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

    ChatHomeScreen(state = state, handlers = handlers)

    if (settingsOpen) {
        NodeSettingsDialog(
            config = nodeConfig,
            onSave = { updated ->
                nodeConfigStore.save(updated)
                nodeConfig = updated
                state = state.copy(nodeLabel = env.nodeLabel(updated), notice = "节点配置已保存")
                settingsOpen = false
            },
            onClose = { settingsOpen = false },
        )
    }
}

/** 应用级服务：identity / store / facade / 群与同步。 */
private class ChatEnvironment {
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

    /** 同步必须独占写者：关 store → 同步 → 重开 store（与既有约束一致）。 */
    suspend fun syncOnce(config: NodeConfig): String = withContext(Dispatchers.IO) {
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
                Text(
                    "证书指纹在「设置 → 传输安全」里配置（此窗口只改地址与身份）。",
                    style = MaterialTheme.typography.labelSmall,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    onSave(
                        config.copy(
                            endpointUrl = endpoint.trim(),
                            nodeId = nodeId.trim(),
                            nodePublicKeyHex = publicKey.trim(),
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
