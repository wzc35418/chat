package chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import chat.api.SendState
import chat.presentation.P0_MAX_MESSAGE_BYTES
import chat.presentation.display.DraftAction
import chat.presentation.display.DraftPhase
import chat.presentation.display.MarkdownCommand
import chat.presentation.display.MarkdownDraft
import chat.presentation.display.applyMarkdownCommand
import chat.presentation.display.MessageRenderModel
import chat.presentation.display.PagingHint
import chat.presentation.display.UiCopy
import chat.presentation.display.renderMessageContent
import chat.presentation.preview.ComposerPresentation
import chat.presentation.preview.PreviewChatState
import chat.presentation.preview.PreviewContact
import chat.presentation.preview.PreviewAttachmentState
import chat.presentation.preview.PreviewGroupState
import chat.presentation.preview.PreviewContactState
import chat.presentation.preview.PreviewIdentityState
import chat.presentation.preview.PreviewStoreState
import chat.presentation.preview.PreviewDraftSample
import chat.presentation.preview.PreviewInviteStep
import chat.presentation.preview.PreviewInviteValidation
import chat.presentation.preview.PreviewMessage
import chat.presentation.preview.PreviewPagingState
import chat.presentation.preview.PreviewScenario
import chat.presentation.preview.addSampleMessage
import chat.presentation.preview.applyScenario
import chat.presentation.preview.backToInviteInput
import chat.presentation.preview.closeInvite
import chat.presentation.preview.composerPresentation
import chat.presentation.preview.confirmInviteRequest
import chat.presentation.preview.fillSampleInviteToken
import chat.presentation.preview.advanceDraftSample
import chat.presentation.preview.advancePagingSample
import chat.presentation.preview.layoutModel
import chat.presentation.preview.openInvite
import chat.presentation.preview.pagingPresentation
import chat.presentation.preview.samplePreviewState
import chat.presentation.preview.selectContact
import chat.presentation.preview.updateInviteText
import chat.presentation.preview.updateSearch
import chat.presentation.preview.validateInvite

private val PreviewInk = Color(0xFF172B29)
private val PreviewGreen = Color(0xFF246B58)
private val PreviewMuted = Color(0xFF64716B)
private val PreviewCanvas = Color(0xFFF3F5F1)
private val PreviewSoft = Color(0xFFE5EDE5)
private val PreviewWarm = Color(0xFFFFF3D6)
private val PreviewWarmInk = Color(0xFF6B4E12)
private val PreviewError = Color(0xFFFFEDE8)
private val PreviewErrorInk = Color(0xFF943C28)

/**
 * 正式聊天界面的独立交互预览。
 *
 * 此入口只使用显式样例状态，不接收 P0Presenter、P0Core 或真实服务；
 * 预览中的联系人、回执、“已送达”等字样均不得接入真实会话。
 */
@Composable
fun PreviewChatScreen(
    initialState: PreviewChatState = samplePreviewState(),
    identityState: PreviewIdentityState = PreviewIdentityState(),
    identityHandlers: IdentityHandlers = IdentityHandlers.None,
    initialIdentityOpen: Boolean = false,
    storeState: PreviewStoreState = PreviewStoreState(),
    storeHandlers: StoreHandlers = StoreHandlers.None,
    storeDraftText: String = "",
    initialStoreOpen: Boolean = false,
    attachmentState: PreviewAttachmentState = PreviewAttachmentState(),
    attachmentHandlers: AttachmentHandlers = AttachmentHandlers.None,
    initialAttachmentOpen: Boolean = false,
    groupState: PreviewGroupState = PreviewGroupState(),
    groupHandlers: GroupHandlers = GroupHandlers.None,
    initialGroupOpen: Boolean = false,
) {
    var state by remember { mutableStateOf(initialState) }
    // 真实身份面板是独立浮层：默认关闭，由侧栏按钮打开；截图入口可指定初始打开。
    var showIdentity by remember { mutableStateOf(initialIdentityOpen) }
    var showStore by remember { mutableStateOf(initialStoreOpen) }
    var showAttachments by remember { mutableStateOf(initialAttachmentOpen) }
    var showGroups by remember { mutableStateOf(initialGroupOpen) }

    MaterialTheme(
        colorScheme = lightColorScheme(
            primary = PreviewGreen,
            surface = Color.White,
            background = PreviewCanvas,
        ),
    ) {
        BoxWithConstraints(
            Modifier
                .fillMaxSize()
                .background(PreviewCanvas)
                .safeDrawingPadding()
                .imePadding(),
        ) {
            val layout = state.layoutModel(maxWidth.value.toInt(), maxHeight.value.toInt())
            Column(Modifier.fillMaxSize()) {
                PreviewBanner(compact = layout.compact)
                if (state.locked) {
                    LockedPane(
                        modifier = Modifier.weight(1f).fillMaxWidth(),
                        onExit = { state = state.applyScenario(PreviewScenario.ACCEPTED) },
                    )
                } else {
                    Box(Modifier.weight(1f).fillMaxWidth()) {
                        if (layout.twoPane) {
                            Row(Modifier.fillMaxSize()) {
                                Sidebar(
                                    state = state,
                                    modifier = Modifier.width(layout.listPaneWidthDp.dp).fillMaxHeight(),
                                    onSelect = { state = state.selectContact(it) },
                                    onScenario = { state = state.applyScenario(it) },
                                    onSearch = { state = state.updateSearch(it) },
                                    onOpenInvite = { state = state.openInvite() },
                                    onOpenIdentity = { showIdentity = true },
                                    onOpenStore = { showStore = true },
                                    onOpenAttachments = { showAttachments = true },
                                    onOpenGroups = { showGroups = true },
                                )
                                Box(Modifier.width(1.dp).fillMaxHeight().background(Color(0xFFE1E6E1)))
                                ChatPane(
                                    state = state,
                                    modifier = Modifier.weight(1f).fillMaxHeight(),
                                    showBack = false,
                                    onBack = {},
                                    onOpenInvite = { state = state.openInvite() },
                                    onSendSample = { text -> state = state.addSampleMessage(text) },
                                    onAdvancePaging = { state = state.advancePagingSample() },
                                    onAdvanceDraft = { state = state.advanceDraftSample() },
                                )
                            }
                        } else {
                            if (layout.showConversation) {
                                ChatPane(
                                    state = state,
                                    modifier = Modifier.fillMaxSize(),
                                    showBack = layout.showBackToConversationList,
                                    onBack = {
                                        state = state.copy(
                                            selectedContactId = null,
                                            scenario = PreviewScenario.EMPTY,
                                        )
                                    },
                                    onOpenInvite = { state = state.openInvite() },
                                    onSendSample = { text -> state = state.addSampleMessage(text) },
                                    onAdvancePaging = { state = state.advancePagingSample() },
                                    onAdvanceDraft = { state = state.advanceDraftSample() },
                                )
                            } else {
                                Sidebar(
                                    state = state,
                                    modifier = Modifier.fillMaxSize(),
                                    onSelect = { state = state.selectContact(it) },
                                    onScenario = { state = state.applyScenario(it) },
                                    onSearch = { state = state.updateSearch(it) },
                                    onOpenInvite = { state = state.openInvite() },
                                    onOpenIdentity = { showIdentity = true },
                                    onOpenStore = { showStore = true },
                                    onOpenAttachments = { showAttachments = true },
                                    onOpenGroups = { showGroups = true },
                                )
                            }
                        }
                    }
                }
            }
            if (showGroups) {
                Box(
                    Modifier
                        .fillMaxSize()
                        .background(Color(0x66000000))
                        .pointerInput(Unit) { detectTapGestures(onTap = { showGroups = false }) }
                        .semantics { contentDescription = "群面板遮罩" },
                ) {
                    GroupPanel(
                        state = groupState,
                        handlers = groupHandlers,
                        onClose = { showGroups = false },
                    )
                }
            }
            if (showAttachments) {
                Box(
                    Modifier
                        .fillMaxSize()
                        .background(Color(0x66000000))
                        .pointerInput(Unit) { detectTapGestures(onTap = { showAttachments = false }) }
                        .semantics { contentDescription = "附件面板遮罩" },
                ) {
                    AttachmentPanel(
                        state = attachmentState,
                        handlers = attachmentHandlers,
                        onClose = { showAttachments = false },
                    )
                }
            }
            if (showStore) {
                Box(
                    Modifier
                        .fillMaxSize()
                        .background(Color(0x66000000))
                        .pointerInput(Unit) { detectTapGestures(onTap = { showStore = false }) }
                        .semantics { contentDescription = "本机存储面板遮罩" },
                ) {
                    LocalStorePanel(
                        state = storeState,
                        draftText = storeDraftText,
                        handlers = storeHandlers,
                        onClose = { showStore = false },
                    )
                }
            }
            if (showIdentity) {
                Box(
                    Modifier
                        .fillMaxSize()
                        .background(Color(0x66000000))
                        .pointerInput(Unit) { detectTapGestures(onTap = { showIdentity = false }) }
                        .semantics { contentDescription = "真实身份面板遮罩" },
                ) {
                    RealIdentityPanel(
                        state = identityState,
                        handlers = identityHandlers,
                        onClose = { showIdentity = false },
                    )
                }
            }
            if (state.showInvite) {
                InviteOverlay(
                    state = state,
                    onClose = { state = state.closeInvite() },
                    onInviteText = { state = state.updateInviteText(it) },
                    onOffline = { offline ->
                        state = if (state.inviteOffline == offline) state else state.copy(inviteOffline = offline)
                    },
                    onValidate = { state = state.validateInvite() },
                    onFillSampleToken = { state = state.fillSampleInviteToken() },
                    onBackToInput = { state = state.backToInviteInput() },
                    onConfirm = { state = state.confirmInviteRequest() },
                )
            }
        }
    }
}

@Composable
private fun PreviewBanner(compact: Boolean) {
    Surface(
        color = PreviewWarm,
        shape = RoundedCornerShape(10.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 8.dp),
    ) {
        Row(Modifier.padding(horizontal = 12.dp, vertical = 8.dp), verticalAlignment = Alignment.Top) {
            Text(
                UiCopy.PREVIEW_BADGE,
                color = PreviewWarmInk,
                style = MaterialTheme.typography.labelLarge,
                fontWeight = FontWeight.Bold,
            )
            Spacer(Modifier.width(10.dp))
            Text(
                UiCopy.PREVIEW_NOTICE,
                color = PreviewWarmInk,
                style = MaterialTheme.typography.labelSmall,
                maxLines = if (compact) 3 else 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Sidebar(
    state: PreviewChatState,
    modifier: Modifier,
    onSelect: (String) -> Unit,
    onScenario: (PreviewScenario) -> Unit,
    onSearch: (String) -> Unit,
    onOpenInvite: () -> Unit,
    onOpenIdentity: () -> Unit,
    onOpenStore: () -> Unit,
    onOpenAttachments: () -> Unit,
    onOpenGroups: () -> Unit,
) {
    Surface(modifier, color = Color.White) {
        Column(
            Modifier.fillMaxSize().padding(horizontal = 14.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Avatar("我", PreviewGreen, 38)
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        "本机样例账户",
                        color = PreviewInk,
                        style = MaterialTheme.typography.titleSmall,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        "acct_self…0001 · 仅预览",
                        color = PreviewMuted,
                        style = MaterialTheme.typography.labelSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                TextButton(
                    onClick = { onScenario(PreviewScenario.LOCKED) },
                    modifier = Modifier.semantics { contentDescription = "锁定界面样例" },
                ) {
                    Text("锁定", style = MaterialTheme.typography.labelMedium)
                }
            }
            Text("预览状态", color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                PreviewScenario.entries.forEach { scenario ->
                    ScenarioChip(scenario, state.scenario, onScenario)
                }
            }
            OutlinedTextField(
                value = state.search,
                onValueChange = onSearch,
                modifier = Modifier.fillMaxWidth().semantics {
                    contentDescription = "搜索已接受好友，仅预览"
                },
                singleLine = true,
                textStyle = MaterialTheme.typography.bodySmall,
                placeholder = { Text(UiCopy.PREVIEW_SEARCH, style = MaterialTheme.typography.bodySmall) },
            )
            Button(
                onClick = onOpenInvite,
                modifier = Modifier.fillMaxWidth().semantics {
                    contentDescription = "加好友，打开样例核对流程"
                },
                contentPadding = PaddingValues(vertical = 10.dp),
            ) {
                Text(UiCopy.PREVIEW_ADD_FRIEND)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.fillMaxWidth()) {
                OutlinedButton(
                    onClick = onOpenIdentity,
                    modifier = Modifier.weight(1f).semantics {
                        contentDescription = "打开真实身份（本机）面板"
                    },
                    contentPadding = PaddingValues(vertical = 8.dp, horizontal = 6.dp),
                ) {
                    Text("真实身份", style = MaterialTheme.typography.labelMedium)
                }
                OutlinedButton(
                    onClick = onOpenStore,
                    modifier = Modifier.weight(1f).semantics {
                        contentDescription = "打开本机存储（真实）面板"
                    },
                    contentPadding = PaddingValues(vertical = 8.dp, horizontal = 6.dp),
                ) {
                    Text("本机存储", style = MaterialTheme.typography.labelMedium)
                }
                OutlinedButton(
                    onClick = onOpenAttachments,
                    modifier = Modifier.weight(1f).semantics {
                        contentDescription = "打开附件（本机）面板"
                    },
                    contentPadding = PaddingValues(vertical = 8.dp, horizontal = 6.dp),
                ) {
                    Text("附件", style = MaterialTheme.typography.labelMedium)
                }
                OutlinedButton(
                    onClick = onOpenGroups,
                    modifier = Modifier.weight(1f).semantics {
                        contentDescription = "打开群（真实 MLS）面板"
                    },
                    contentPadding = PaddingValues(vertical = 8.dp, horizontal = 6.dp),
                ) {
                    Text("群", style = MaterialTheme.typography.labelMedium)
                }
            }
            Surface(color = PreviewCanvas, shape = RoundedCornerShape(10.dp)) {
                Row(Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(UiCopy.PREVIEW_FRIEND_REQUESTS, color = PreviewInk, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
                    Text(
                        "${state.pendingRequestCount} 待处理",
                        color = PreviewErrorInk,
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
            }
            HorizontalDivider(color = Color(0xFFE7EBE7))
            LazyColumn(
                modifier = Modifier.weight(1f).fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                items(state.visibleContacts, key = { it.id }) { contact ->
                    ContactRow(
                        contact = contact,
                        selected = contact.id == state.selectedContactId,
                        onClick = { onSelect(contact.id) },
                    )
                }
            }
            Text("样例联系人 · 不连接真实账户或节点", color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
        }
    }
}

@Composable
private fun ScenarioChip(
    scenario: PreviewScenario,
    selected: PreviewScenario,
    onScenario: (PreviewScenario) -> Unit,
) {
    FilterChip(
        selected = scenario == selected,
        onClick = { onScenario(scenario) },
        modifier = Modifier.semantics {
            contentDescription = "预览状态 ${scenario.label}"
            stateDescription = if (scenario == selected) "已选择" else "未选择"
        },
        label = {
            Text(
                scenario.label,
                style = MaterialTheme.typography.labelSmall,
                maxLines = 1,
            )
        },
    )
}

@Composable
private fun ContactRow(contact: PreviewContact, selected: Boolean, onClick: () -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .background(if (selected) PreviewSoft else Color.Transparent, RoundedCornerShape(12.dp))
            .clickable(role = Role.Button, onClick = onClick)
            .semantics {
                contentDescription = "联系人 ${contact.displayName}，状态 ${contact.state.label}"
                stateDescription = if (selected) "当前会话" else "未选择"
            }
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Avatar(contact.displayName.take(1), avatarColor(contact.state), 34)
            Spacer(Modifier.width(8.dp))
            Text(
                contact.displayName,
                color = PreviewInk,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            Text(contact.timeLabel, color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
        }
        Text(
            contact.lastMessage,
            color = PreviewMuted,
            style = MaterialTheme.typography.labelSmall,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        if (contact.state != PreviewContactState.ACCEPTED) {
            StateBadge(contact.state)
        }
    }
}

@Composable
private fun ChatPane(
    state: PreviewChatState,
    modifier: Modifier,
    showBack: Boolean,
    onBack: () -> Unit,
    onOpenInvite: () -> Unit,
    onSendSample: (String) -> Unit,
    onAdvancePaging: () -> Unit,
    onAdvanceDraft: () -> Unit,
) {
    val contact = state.selectedContact
    var showVerification by remember(contact?.id) { mutableStateOf(false) }
    Surface(modifier, color = Color(0xFFF7F8F5)) {
        Column(Modifier.fillMaxSize()) {
            ChatHeader(
                contact = contact,
                showBack = showBack,
                showVerification = showVerification,
                onBack = onBack,
                onToggleVerification = { showVerification = !showVerification },
            )
            HorizontalDivider(color = Color(0xFFE1E6E1))
            if (contact == null) {
                EmptyConversation(Modifier.weight(1f).fillMaxWidth(), onOpenInvite)
            } else {
                if (showVerification) {
                    VerificationCard(contact, Modifier.fillMaxWidth())
                }
                PagingSampleStrip(
                    paging = state.paging,
                    onAdvance = onAdvancePaging,
                )
                DraftSampleStrip(
                    sample = state.draftSample,
                    onAdvance = onAdvanceDraft,
                )
                MessageList(
                    messages = state.messagesForSelected(),
                    modifier = Modifier.weight(1f).fillMaxWidth(),
                )
                val composer = state.composerPresentation()
                if (composer is ComposerPresentation.Blocked) {
                    BlockedBanner(
                        composer = composer,
                        identityChanged = contact.state == PreviewContactState.IDENTITY_CHANGED,
                        onAction = { showVerification = true },
                    )
                }
                SampleComposer(
                    contactId = contact.id,
                    enabled = composer is ComposerPresentation.Enabled,
                    onSend = onSendSample,
                )
            }
        }
    }
}

@Composable
private fun ChatHeader(
    contact: PreviewContact?,
    showBack: Boolean,
    showVerification: Boolean,
    onBack: () -> Unit,
    onToggleVerification: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (showBack) {
            TextButton(
                onClick = onBack,
                modifier = Modifier.semantics { contentDescription = "返回好友列表" },
                contentPadding = PaddingValues(horizontal = 6.dp),
            ) {
                Text("← 返回", style = MaterialTheme.typography.labelMedium)
            }
        }
        Column(Modifier.weight(1f)) {
            Text(
                contact?.displayName ?: "未选择好友",
                color = PreviewInk,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                if (contact == null) "样例会话" else "${contact.accountShortId} · ${contact.node}",
                color = PreviewMuted,
                style = MaterialTheme.typography.labelSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        contact?.let { StateBadge(it.state) }
        if (contact != null) {
            TextButton(
                onClick = onToggleVerification,
                modifier = Modifier.semantics {
                    contentDescription = if (showBack) "身份核验" else "展开或收起身份核验"
                    stateDescription = if (showVerification) "已展开" else "已收起"
                },
                contentPadding = PaddingValues(horizontal = 6.dp),
            ) {
                Text(
                    if (showVerification) {
                        "收起"
                    } else if (showBack) {
                        "核验"
                    } else {
                        "身份核验"
                    },
                    style = MaterialTheme.typography.labelMedium,
                )
            }
        }
    }
}

@Composable
private fun VerificationCard(contact: PreviewContact, modifier: Modifier) {
    Surface(modifier = modifier.padding(horizontal = 12.dp, vertical = 6.dp), color = PreviewSoft, shape = RoundedCornerShape(10.dp)) {
        Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text("身份核验（样例）", color = PreviewInk, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
            Text("账户 ID：${contact.accountShortId}", color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
            Text("节点：${contact.node}", color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
            Text("指纹：${contact.fingerprint}", color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
            Text("昵称只作展示，不能用于验证身份。", color = PreviewErrorInk, style = MaterialTheme.typography.labelSmall)
        }
    }
}

/**
 * DS-11 分页提示样例。文案、可用动作与语义播报都来自展示层的纯映射
 * （[chat.presentation.display.PagingPresentation]）：加载中不给可点动作，
 * 失败只提供一次重试，到达开头后按钮变成重置样例。
 */
@Composable
private fun PagingSampleStrip(
    paging: PreviewPagingState,
    onAdvance: () -> Unit,
) {
    val presentation = paging.pagingPresentation()
    val label = presentation.label ?: return
    val action = presentation.actionLabel
        ?: if (presentation.hint == PagingHint.NoMore) "重置分页样例" else null
    Surface(
        color = PreviewSoft,
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 6.dp)
            .semantics {
                contentDescription = "分页提示样例，不影响上方样例消息"
                stateDescription = presentation.announcement ?: label
            },
    ) {
        Row(
            Modifier.padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Column(Modifier.weight(1f)) {
                Text(
                    "分页样例（合并器驱动）",
                    color = PreviewMuted,
                    style = MaterialTheme.typography.labelSmall,
                )
                Text(
                    label,
                    color = if (presentation.hint is PagingHint.Failed) PreviewErrorInk else PreviewInk,
                    style = MaterialTheme.typography.labelSmall,
                )
                Text(
                    "已合并 ${paging.display.conversation.messages.size} 条样例结果",
                    color = PreviewMuted,
                    style = MaterialTheme.typography.labelSmall,
                )
            }
            if (action != null) {
                TextButton(
                    onClick = onAdvance,
                    modifier = Modifier.semantics { contentDescription = action },
                    contentPadding = PaddingValues(horizontal = 8.dp),
                ) {
                    Text(action, style = MaterialTheme.typography.labelMedium)
                }
            }
        }
    }
}

/**
 * DS-10 发送状态样例。展示的是 [chat.presentation.display.presentDraft] 的结论：
 * 节点接受不等于送达、结果未知只提供对账、不可重试的失败不显示重试。
 * 这里只切换样例状态，不会真的发送任何内容。
 */
@Composable
private fun DraftSampleStrip(
    sample: PreviewDraftSample,
    onAdvance: () -> Unit,
) {
    val ui = sample.ui
    Surface(
        color = PreviewCanvas,
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 2.dp)
            .semantics {
                contentDescription = "发送状态样例，不会发送任何内容"
                stateDescription = ui.label
            },
    ) {
        Row(
            Modifier.padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Column(Modifier.weight(1f)) {
                Text(
                    "发送状态样例（不会发送）",
                    color = PreviewMuted,
                    style = MaterialTheme.typography.labelSmall,
                )
                Text(
                    ui.label,
                    color = if (ui.phase == DraftPhase.FAILED_BLOCKED) PreviewErrorInk else PreviewInk,
                    style = MaterialTheme.typography.labelSmall,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    ui.detail,
                    color = PreviewMuted,
                    style = MaterialTheme.typography.labelSmall,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    "该状态允许的动作：" + if (ui.actions.isEmpty()) {
                        "无"
                    } else {
                        ui.actions.joinToString(" / ") { draftActionLabel(it) }
                    },
                    color = PreviewMuted,
                    style = MaterialTheme.typography.labelSmall,
                )
            }
            TextButton(
                onClick = onAdvance,
                modifier = Modifier.semantics { contentDescription = "切换到下一个发送状态样例" },
                contentPadding = PaddingValues(horizontal = 8.dp),
            ) {
                Text("下一状态", style = MaterialTheme.typography.labelMedium)
            }
        }
    }
}

private fun draftActionLabel(action: DraftAction): String = when (action) {
    DraftAction.RETRY -> "重试"
    DraftAction.CANCEL -> "取消"
    DraftAction.RECONCILE -> "对账查询"
    DraftAction.VIEW_DETAILS -> "查看详情"
}

@Composable
private fun EmptyConversation(modifier: Modifier, onOpenInvite: () -> Unit) {
    Box(modifier.padding(24.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(UiCopy.PREVIEW_EMPTY_TITLE, color = PreviewInk, style = MaterialTheme.typography.titleLarge)
            Text(UiCopy.PREVIEW_EMPTY_DETAIL, color = PreviewMuted, style = MaterialTheme.typography.bodySmall)
            Button(
                onClick = onOpenInvite,
                modifier = Modifier.semantics { contentDescription = "加好友，打开样例核对流程" },
            ) {
                Text(UiCopy.PREVIEW_ADD_FRIEND)
            }
        }
    }
}

@Composable
private fun MessageList(messages: List<PreviewMessage>, modifier: Modifier) {
    if (messages.isEmpty()) {
        Box(modifier.padding(24.dp), contentAlignment = Alignment.Center) {
            Text("还没有样例消息", color = PreviewMuted, style = MaterialTheme.typography.bodySmall)
        }
        return
    }
    LazyColumn(
        modifier = modifier,
        contentPadding = PaddingValues(horizontal = 14.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
        reverseLayout = true,
    ) {
        items(messages.asReversed(), key = { it.id }) { message ->
            MessageCard(message)
        }
    }
}

@Composable
private fun MessageCard(message: PreviewMessage) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = if (message.mine) Arrangement.End else Arrangement.Start,
    ) {
        Surface(
            shape = RoundedCornerShape(14.dp),
            color = if (message.mine) Color(0xFFDCEBE1) else Color.White,
            modifier = Modifier.widthIn(max = 460.dp),
        ) {
            Column(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Text(
                    if (message.mine) "我" else message.author,
                    color = PreviewGreen,
                    style = MaterialTheme.typography.labelMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                when (val model = renderMessageContent(message.content)) {
                    // 文本消息按安全 Markdown 子集渲染（无 HTML、链接不可点击、超限有界截断）。
                    is MessageRenderModel.Text -> MarkdownMessageBody(model.text, textColor = PreviewInk)
                    is MessageRenderModel.Attachment -> {
                        Text("附件", color = PreviewInk, style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                        model.caption?.let { Text(it, color = PreviewInk, style = MaterialTheme.typography.bodySmall) }
                        Text(model.badge, color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
                    }
                    is MessageRenderModel.ExtensionFallback -> {
                        Text(model.title, color = PreviewErrorInk, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold)
                        Text(model.text, color = PreviewInk, style = MaterialTheme.typography.bodySmall)
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(message.timeLabel, color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
                    message.sendState?.let { state ->
                        Spacer(Modifier.width(6.dp))
                        Text(
                            "${UiCopy.PREVIEW_SAMPLE_STATUS} · ${sendStateLabel(state)}",
                            color = sendStateColor(state),
                            style = MaterialTheme.typography.labelSmall,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun BlockedBanner(
    composer: ComposerPresentation.Blocked,
    identityChanged: Boolean,
    onAction: () -> Unit,
) {
    val prefix = if (identityChanged) "安全阻断" else "提示"
    Surface(
        color = if (identityChanged) PreviewError else PreviewWarm,
        shape = RoundedCornerShape(10.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 6.dp)
            .semantics {
                liveRegion = LiveRegionMode.Assertive
                contentDescription = "$prefix：${composer.title}。${composer.detail}"
            },
    ) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                "$prefix · ${composer.title}",
                color = if (identityChanged) PreviewErrorInk else PreviewWarmInk,
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                composer.detail,
                color = if (identityChanged) PreviewErrorInk else PreviewWarmInk,
                style = MaterialTheme.typography.bodySmall,
            )
            composer.actionLabel?.let { label ->
                TextButton(onClick = onAction, contentPadding = PaddingValues(horizontal = 0.dp)) {
                    Text(label, style = MaterialTheme.typography.labelMedium, color = if (identityChanged) PreviewErrorInk else PreviewWarmInk)
                }
            }
        }
    }
}

/**
 * 样例发送框：支持 Markdown 输入（格式工具栏 + 预览）与 UTF-8 字节上限。
 * 只追加到预览样例列表，不会发送到任何服务。
 */
@Composable
private fun SampleComposer(contactId: String, enabled: Boolean, onSend: (String) -> Unit) {
    var draft by remember(contactId) { mutableStateOf(TextFieldValue("")) }
    var previewing by remember(contactId) { mutableStateOf(false) }
    val byteCount = draft.text.encodeToByteArray().size
    val withinLimit = byteCount <= P0_MAX_MESSAGE_BYTES

    fun applyCommand(command: MarkdownCommand) {
        if (!enabled) return
        val updated = MarkdownDraft(
            text = draft.text,
            selectionStart = draft.selection.start,
            selectionEnd = draft.selection.end,
        ).applyMarkdownCommand(command)
        if (updated.text.encodeToByteArray().size > P0_MAX_MESSAGE_BYTES) return
        draft = TextFieldValue(updated.text, TextRange(updated.selectionStart, updated.selectionEnd))
        previewing = false
    }

    Column(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(2.dp), verticalAlignment = Alignment.CenterVertically) {
            MarkdownCommand.entries.forEach { command ->
                TextButton(
                    onClick = { applyCommand(command) },
                    enabled = enabled,
                    modifier = Modifier.semantics { contentDescription = "插入 Markdown " + markdownCommandLabel(command) },
                    contentPadding = PaddingValues(horizontal = 8.dp, vertical = 2.dp),
                ) {
                    Text(markdownCommandLabel(command), style = MaterialTheme.typography.labelMedium)
                }
            }
            Spacer(Modifier.weight(1f))
            TextButton(
                onClick = { previewing = !previewing },
                modifier = Modifier.semantics {
                    contentDescription = if (previewing) "切回编辑 Markdown" else "预览 Markdown 渲染结果"
                    stateDescription = if (previewing) "预览中" else "编辑中"
                },
                contentPadding = PaddingValues(horizontal = 8.dp, vertical = 2.dp),
            ) {
                Text(if (previewing) "编辑" else "预览", style = MaterialTheme.typography.labelMedium)
            }
        }
        if (previewing) {
            Surface(
                color = Color.White,
                shape = RoundedCornerShape(10.dp),
                modifier = Modifier.fillMaxWidth().semantics { contentDescription = "Markdown 预览结果" },
            ) {
                Box(Modifier.padding(horizontal = 12.dp, vertical = 10.dp)) {
                    if (draft.text.isBlank()) {
                        Text("还没有内容可预览。", color = PreviewMuted, style = MaterialTheme.typography.bodySmall)
                    } else {
                        MarkdownMessageBody(draft.text, textColor = PreviewInk)
                    }
                }
            }
        } else {
            OutlinedTextField(
                value = draft,
                onValueChange = { next ->
                    if (next.text.encodeToByteArray().size <= P0_MAX_MESSAGE_BYTES) draft = next
                },
                modifier = Modifier.fillMaxWidth().semantics {
                    contentDescription = if (enabled) "样例消息输入框，支持 Markdown 语法" else "样例消息输入框，当前状态不可发送"
                },
                enabled = enabled,
                minLines = 1,
                maxLines = 3,
                textStyle = MaterialTheme.typography.bodyMedium,
                label = { Text(if (enabled) "写一条消息（样例，支持 Markdown）" else "当前状态不可发送") },
                supportingText = {
                    Text(
                        if (!enabled) {
                            "只有好友状态为 accepted 时才会启用发送框。"
                        } else if (!withinLimit) {
                            "已超过 8192 UTF-8 字节上限，请缩短内容。"
                        } else {
                            UiCopy.PREVIEW_SAMPLE_HINT
                        },
                        style = MaterialTheme.typography.labelSmall,
                    )
                },
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                byteCount.toString() + " / 8192 UTF-8 字节 · 支持 Markdown：粗体、斜体、列表、引用、代码",
                color = if (withinLimit) PreviewMuted else PreviewErrorInk,
                style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.weight(1f),
            )
            Button(
                onClick = {
                    onSend(draft.text)
                    draft = TextFieldValue("")
                    previewing = false
                },
                enabled = enabled && draft.text.isNotBlank() && withinLimit,
                modifier = Modifier.semantics { contentDescription = "发送样例消息" },
                contentPadding = PaddingValues(horizontal = 14.dp, vertical = 10.dp),
            ) {
                Text(UiCopy.PREVIEW_SAMPLE_SEND)
            }
        }
    }
}

private fun markdownCommandLabel(command: MarkdownCommand): String = when (command) {
    MarkdownCommand.BOLD -> "粗体"
    MarkdownCommand.ITALIC -> "斜体"
    MarkdownCommand.STRIKE -> "删除线"
    MarkdownCommand.CODE -> "代码"
    MarkdownCommand.LINK -> "链接"
}

@Composable
private fun LockedPane(modifier: Modifier, onExit: () -> Unit) {
    Box(modifier.padding(24.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Surface(color = PreviewSoft, shape = CircleShape, modifier = Modifier.size(64.dp)) {
                Box(contentAlignment = Alignment.Center) {
                    Text("锁", color = PreviewGreen, style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
                }
            }
            Text("界面已锁定", color = PreviewInk, style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
            Text("锁定状态下不加载联系人、消息或身份信息。", color = PreviewMuted, style = MaterialTheme.typography.bodyMedium)
            Text(
                "真实解锁需要底层密钥保护与恢复服务；当前仅展示交互状态。",
                color = PreviewMuted,
                style = MaterialTheme.typography.labelSmall,
            )
            Button(
                onClick = {},
                enabled = false,
                modifier = Modifier.semantics { contentDescription = "解锁，尚未接入" },
            ) {
                Text("解锁（尚未接入）")
            }
            TextButton(
                onClick = onExit,
                modifier = Modifier.semantics { contentDescription = "返回交互预览" },
            ) {
                Text("返回交互预览")
            }
        }
    }
}

@Composable
private fun InviteOverlay(
    state: PreviewChatState,
    onClose: () -> Unit,
    onInviteText: (String) -> Unit,
    onOffline: (Boolean) -> Unit,
    onValidate: () -> Unit,
    onFillSampleToken: () -> Unit,
    onBackToInput: () -> Unit,
    onConfirm: () -> Unit,
) {
    Box(
        Modifier
            .fillMaxSize()
            .background(Color(0x66000000))
            .pointerInput(Unit) { detectTapGestures(onTap = { onClose() }) },
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            modifier = Modifier
                .fillMaxWidth(0.92f)
                .widthIn(max = 520.dp)
                .pointerInput(Unit) { detectTapGestures { } },
            color = Color.White,
            shape = RoundedCornerShape(18.dp),
        ) {
            Column(
                Modifier
                    .verticalScroll(rememberScrollState())
                    .padding(20.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "加好友 · 交互预览",
                        color = PreviewInk,
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(
                        onClick = onClose,
                        modifier = Modifier.semantics { contentDescription = "关闭加好友预览" },
                    ) {
                        Text("关闭")
                    }
                }
                when (state.inviteStep) {
                    PreviewInviteStep.INPUT -> InviteInputStep(
                        state = state,
                        onInviteText = onInviteText,
                        onOffline = onOffline,
                        onValidate = onValidate,
                        onFillSampleToken = onFillSampleToken,
                        onClose = onClose,
                    )
                    PreviewInviteStep.IDENTITY -> InviteIdentityStep(
                        onBack = onBackToInput,
                        onConfirm = onConfirm,
                    )
                    PreviewInviteStep.RESULT -> InviteResultStep(state.inviteOffline, onClose)
                }
            }
        }
    }
}

@Composable
private fun InviteInputStep(
    state: PreviewChatState,
    onInviteText: (String) -> Unit,
    onOffline: (Boolean) -> Unit,
    onValidate: () -> Unit,
    onFillSampleToken: () -> Unit,
    onClose: () -> Unit,
) {
    Text(
        "这里只演示口令核对流程，不连接真实联系人服务。样例只接受一条内置口令。",
        color = PreviewMuted,
        style = MaterialTheme.typography.bodySmall,
    )
    OutlinedTextField(
        value = state.inviteText,
        onValueChange = onInviteText,
        modifier = Modifier.fillMaxWidth().semantics {
            contentDescription = "样例好友口令输入框"
        },
        minLines = 3,
        maxLines = 5,
        label = { Text("样例好友口令") },
        placeholder = { Text("chat-invite:…（仅布局预览）", style = MaterialTheme.typography.bodySmall) },
    )
    TextButton(
        onClick = onFillSampleToken,
        modifier = Modifier.semantics { contentDescription = "填入内置样例口令" },
        contentPadding = PaddingValues(horizontal = 0.dp),
    ) {
        Text("填入内置样例口令", style = MaterialTheme.typography.labelMedium)
    }
    state.inviteNotice?.let { notice ->
        val isWarning = state.inviteValidation == PreviewInviteValidation.EMPTY ||
            state.inviteValidation == PreviewInviteValidation.NON_SAMPLE_INPUT
        Surface(
            color = if (isWarning) PreviewError else PreviewWarm,
            shape = RoundedCornerShape(10.dp),
            modifier = Modifier.semantics {
                liveRegion = LiveRegionMode.Polite
                contentDescription = notice
            },
        ) {
            Text(
                notice,
                color = if (isWarning) PreviewErrorInk else PreviewWarmInk,
                style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.padding(10.dp),
            )
        }
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text("模拟离线", color = PreviewInk, style = MaterialTheme.typography.bodySmall)
            Text("用于观察“待联网发送”结果。", color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
        }
        Switch(checked = state.inviteOffline, onCheckedChange = onOffline)
    }
    Text("样例不会校验签名、有效期或一次性使用。", color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        TextButton(
            onClick = onClose,
            modifier = Modifier.semantics { contentDescription = "取消加好友预览" },
        ) {
            Text("取消")
        }
        Spacer(Modifier.width(8.dp))
        Button(
            onClick = onValidate,
            modifier = Modifier.semantics { contentDescription = "查看样例核对流程" },
        ) {
            Text("查看样例核对流程")
        }
    }
}

@Composable
private fun InviteIdentityStep(onBack: () -> Unit, onConfirm: () -> Unit) {
    Text(
        "样例身份（未验证）",
        color = PreviewInk,
        style = MaterialTheme.typography.titleMedium,
        fontWeight = FontWeight.SemiBold,
    )
    Text(
        "以下身份由内置样例生成，仅用于观察交互；没有执行签名或有效期验证。",
        color = PreviewMuted,
        style = MaterialTheme.typography.labelSmall,
    )
    IdentityField("昵称", "林然（样例）")
    IdentityField("账户 ID", "acct_7f3a…c92d")
    IdentityField("节点", "node-a.local")
    IdentityField("指纹", "SHA256 4B7C 91D2 6A0F 38E5 C7B1 2D94 8F60 1A3E 55C8 90BD 7E42")
    Surface(
        color = PreviewWarm,
        shape = RoundedCornerShape(10.dp),
        modifier = Modifier.semantics {
            contentDescription = "身份提示：昵称只作展示，不能用于验证身份。首次接触建议通过独立渠道比对指纹。"
        },
    ) {
        Text(
            "昵称只作展示，不能用于验证身份。首次接触建议通过独立渠道比对指纹。",
            color = PreviewWarmInk,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(10.dp),
        )
    }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        TextButton(
            onClick = onBack,
            modifier = Modifier.semantics { contentDescription = "返回口令输入" },
        ) {
            Text("返回")
        }
        Spacer(Modifier.width(8.dp))
        Button(
            onClick = onConfirm,
            modifier = Modifier.semantics { contentDescription = "发送样例好友申请" },
        ) {
            Text("发送好友申请")
        }
    }
}

@Composable
private fun InviteResultStep(offline: Boolean, onClose: () -> Unit) {
    Text(
        if (offline) "请求已保存，待联网发送" else "请求已保存，等待对方确认",
        color = PreviewInk,
        style = MaterialTheme.typography.titleMedium,
        fontWeight = FontWeight.SemiBold,
    )
    Text(
        if (offline) {
            "本端只保存了请求；联网后仍需对方确认，当前不能打开正式聊天。"
        } else {
            "在对方确认前，正式发送框保持禁用。"
        },
        color = PreviewMuted,
        style = MaterialTheme.typography.bodySmall,
    )
    Text("样例结果：不会产生真实好友请求，也不会打开正式会话。", color = PreviewErrorInk, style = MaterialTheme.typography.labelSmall)
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        Button(
            onClick = onClose,
            modifier = Modifier.semantics { contentDescription = "关闭样例请求结果" },
        ) {
            Text("关闭")
        }
    }
}

@Composable
private fun IdentityField(label: String, value: String) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(label, color = PreviewMuted, style = MaterialTheme.typography.labelSmall)
        Text(value, color = PreviewInk, style = MaterialTheme.typography.bodySmall)
    }
}

@Composable
private fun Avatar(text: String, color: Color, size: Int) {
    Surface(color = color, shape = CircleShape, modifier = Modifier.size(size.dp)) {
        Box(contentAlignment = Alignment.Center) {
            Text(text, color = Color.White, style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.Bold)
        }
    }
}

@Composable
private fun StateBadge(state: PreviewContactState) {
    val (background, foreground) = when (state) {
        PreviewContactState.ACCEPTED -> PreviewSoft to PreviewGreen
        PreviewContactState.WAITING_FOR_PEER,
        PreviewContactState.WAITING_FOR_BOTH_SIDES,
        PreviewContactState.QUEUED_OFFLINE -> PreviewWarm to PreviewWarmInk
        PreviewContactState.IDENTITY_CHANGED,
        PreviewContactState.FAILED -> PreviewError to PreviewErrorInk
        PreviewContactState.EXPIRED -> Color(0xFFECEFEC) to PreviewMuted
    }
    Surface(color = background, shape = RoundedCornerShape(6.dp)) {
        Text(
            state.label,
            color = foreground,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
            maxLines = 1,
        )
    }
}

private fun avatarColor(state: PreviewContactState): Color = when (state) {
    PreviewContactState.ACCEPTED -> PreviewGreen
    PreviewContactState.IDENTITY_CHANGED, PreviewContactState.FAILED -> PreviewErrorInk
    else -> Color(0xFF8A7350)
}

private fun sendStateLabel(state: SendState): String = when (state) {
    SendState.QUEUED -> "待发送"
    SendState.ACCEPTED -> "已接受"
    SendState.DELIVERED -> "已送达"
    SendState.FAILED -> "失败"
}

private fun sendStateColor(state: SendState): Color = when (state) {
    SendState.DELIVERED, SendState.ACCEPTED -> PreviewGreen
    SendState.QUEUED -> PreviewWarmInk
    SendState.FAILED -> PreviewErrorInk
}
