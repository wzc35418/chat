package chat.presentation.preview

import chat.api.AttachmentId
import chat.api.MessageContent
import chat.api.SendState
import chat.presentation.display.LayoutModel
import chat.presentation.display.NavigationState
import chat.presentation.display.PagingHint
import chat.presentation.display.WindowSize
import chat.presentation.display.buildAdaptiveLayout

/**
 * 正式聊天界面的交互预览状态。
 *
 * 所有联系人和消息都是显式样例，只用于观察布局与状态切换；
 * 不得接入 P0 真链路、正式联系人服务、节点回执或持久化历史。
 */
enum class PreviewScenario(val label: String) {
    ACCEPTED("已接受"),
    WAITING_FOR_PEER("待确认"),
    QUEUED_OFFLINE("待联网"),
    IDENTITY_CHANGED("身份变化"),
    LOCKED("锁定"),
    EMPTY("未选择"),
}

enum class PreviewContactState(val label: String) {
    ACCEPTED("已接受"),
    WAITING_FOR_PEER("等待对方确认"),
    WAITING_FOR_BOTH_SIDES("等待双方完成"),
    QUEUED_OFFLINE("请求待联网发送"),
    IDENTITY_CHANGED("身份变化"),
    EXPIRED("口令已失效"),
    FAILED("请求失败"),
}

data class PreviewContact(
    val id: String,
    val displayName: String,
    val accountShortId: String,
    val node: String,
    val fingerprint: String,
    val state: PreviewContactState,
    val lastMessage: String,
    val timeLabel: String,
    val pendingRequests: Int = 0,
)

data class PreviewMessage(
    val id: String,
    val mine: Boolean,
    val author: String,
    val content: MessageContent,
    val sendState: SendState?,
    val timeLabel: String,
)

enum class PreviewInviteStep { INPUT, IDENTITY, RESULT }

/** 预览只接受这一条内置口令，任何其他输入都不会进入身份核对页。 */
const val PREVIEW_SAMPLE_INVITE_TOKEN = "chat-invite:SAMPLE-LIN-7F3A"

enum class PreviewInviteValidation {
    NOT_STARTED,
    EMPTY,
    NON_SAMPLE_INPUT,
    SAMPLE_TOKEN,
}

data class PreviewChatState(
    val contacts: List<PreviewContact>,
    val messagesByContact: Map<String, List<PreviewMessage>>,
    val selectedContactId: String?,
    val scenario: PreviewScenario = PreviewScenario.ACCEPTED,
    val locked: Boolean = false,
    val search: String = "",
    val showInvite: Boolean = false,
    val inviteStep: PreviewInviteStep = PreviewInviteStep.INPUT,
    val inviteText: String = "",
    val inviteOffline: Boolean = false,
    val inviteValidation: PreviewInviteValidation = PreviewInviteValidation.NOT_STARTED,
    val inviteNotice: String? = null,
    /** DS-11 分页提示样例；只在本进程内演示合并器的加载/失败/到底三种提示。 */
    val paging: PreviewPagingState = samplePreviewPaging(),
    /** DS-10 发送状态样例；只在界面里循环展示状态与可用动作，不发送任何内容。 */
    val draftSample: PreviewDraftSample = samplePreviewDraft(),
) {
    val selectedContact: PreviewContact?
        get() = contacts.firstOrNull { it.id == selectedContactId }

    val visibleContacts: List<PreviewContact>
        get() = if (search.isBlank()) contacts else contacts.filter {
            it.state == PreviewContactState.ACCEPTED && it.displayName.contains(search.trim(), ignoreCase = true)
        }

    val pendingRequestCount: Int
        get() = contacts.count { it.pendingRequests > 0 }

    fun messagesForSelected(): List<PreviewMessage> = messagesByContact[selectedContactId].orEmpty()
}

enum class PreviewLayout { TWO_PANE, SINGLE_PANE }

/**
 * 兼容入口：预览仍用 [PreviewLayout] 表达双栏/单栏，实际阈值与合法性判定都交给
 * [buildAdaptiveLayout]，避免界面与展示层各维护一套宽度规则。
 */
fun previewLayoutFor(widthDp: Int): PreviewLayout =
    if (buildAdaptiveLayout(WindowSize(widthDp, 0), NavigationState()).twoPane) {
        PreviewLayout.TWO_PANE
    } else {
        PreviewLayout.SINGLE_PANE
    }

/**
 * 预览页面的布局决策。选中联系人会按已知联系人集合校验，失效选择回退到列表，
 * 而不是显示一个不存在的会话。
 */
fun PreviewChatState.layoutModel(widthDp: Int, heightDp: Int): LayoutModel = buildAdaptiveLayout(
    window = WindowSize(widthDp, heightDp),
    selection = NavigationState(
        selectedConversationId = selectedContactId,
        knownConversationIds = contacts.map { it.id }.toSet(),
        detailOverlayOpen = showInvite,
    ),
)

sealed interface ComposerPresentation {
    data class Enabled(val hint: String) : ComposerPresentation

    data class Blocked(
        val title: String,
        val detail: String,
        val actionLabel: String?,
    ) : ComposerPresentation
}

fun PreviewChatState.composerPresentation(): ComposerPresentation {
    if (locked) {
        return ComposerPresentation.Blocked(
            title = "界面已锁定",
            detail = "锁定状态下不显示联系人和消息内容，也不能发送。",
            actionLabel = null,
        )
    }
    val contact = selectedContact ?: return ComposerPresentation.Blocked(
        title = "请选择一位好友",
        detail = "选中已接受的好友后才能使用正式发送框。",
        actionLabel = null,
    )
    return when (contact.state) {
        PreviewContactState.ACCEPTED -> ComposerPresentation.Enabled("输入消息（样例，不会发送）")
        PreviewContactState.WAITING_FOR_PEER -> ComposerPresentation.Blocked(
            title = "等待对方完成确认",
            detail = "对方确认前不能打开正式聊天或发送消息。",
            actionLabel = "查看好友申请",
        )
        PreviewContactState.WAITING_FOR_BOTH_SIDES -> ComposerPresentation.Blocked(
            title = "等待双方完成确认",
            detail = "本端已确认，但对方尚未完成确认。",
            actionLabel = "查看状态",
        )
        PreviewContactState.QUEUED_OFFLINE -> ComposerPresentation.Blocked(
            title = "好友请求待联网发送",
            detail = "请求已保存在本端；联网后仍需对方确认，当前不能发送消息。",
            actionLabel = "查看好友申请",
        )
        PreviewContactState.IDENTITY_CHANGED -> ComposerPresentation.Blocked(
            title = "身份变化，已阻断发送",
            detail = "对方指纹与已固定记录不一致。请通过独立渠道核对身份，界面不会用新指纹覆盖本地信任锚。",
            actionLabel = "查看核验信息",
        )
        PreviewContactState.EXPIRED -> ComposerPresentation.Blocked(
            title = "口令已失效",
            detail = "请重新获取一次性加好友口令。",
            actionLabel = null,
        )
        PreviewContactState.FAILED -> ComposerPresentation.Blocked(
            title = "好友请求失败",
            detail = "请求没有完成，请检查本机状态后重新发起。",
            actionLabel = "重新发起",
        )
    }
}

fun samplePreviewState(): PreviewChatState = PreviewChatState(
    contacts = sampleContacts,
    messagesByContact = sampleMessages,
    selectedContactId = "lin",
    scenario = PreviewScenario.ACCEPTED,
)

private val sampleContacts = listOf(
    PreviewContact(
        id = "lin",
        displayName = "林然",
        accountShortId = "acct_7f3a…c92d",
        node = "node-a.local",
        fingerprint = "SHA256 4B7C 91D2 6A0F 38E5 C7B1 2D94 8F60 1A3E 55C8 90BD 7E42",
        state = PreviewContactState.ACCEPTED,
        lastMessage = "收到，明天见。",
        timeLabel = "18:42",
    ),
    PreviewContact(
        id = "zhou",
        displayName = "周其",
        accountShortId = "acct_19be…08aa",
        node = "node-b.local",
        fingerprint = "SHA256 77A0 3CD1 5E28 B946 0F1D A8C3 62E7 4B90 D5F1 28A6 73C0",
        state = PreviewContactState.WAITING_FOR_PEER,
        lastMessage = "好友申请：等待你确认身份",
        timeLabel = "昨天",
        pendingRequests = 1,
    ),
    PreviewContact(
        id = "gu",
        displayName = "顾问",
        accountShortId = "acct_55c1…d731",
        node = "node-c.local",
        fingerprint = "SHA256 91E4 0B2A 6F57 C3D8 4A19 7E60 25B8 9D0C F4A6 17E2 8B55",
        state = PreviewContactState.IDENTITY_CHANGED,
        lastMessage = "指纹与已保存记录不一致",
        timeLabel = "周一",
    ),
    PreviewContact(
        id = "su",
        displayName = "苏禾",
        accountShortId = "acct_2d8f…6b14",
        node = "node-d.local",
        fingerprint = "SHA256 C3A9 6E10 84D2 5B7F 0A36 91C4 E852 7D0B 6F19 A4E8 25C7",
        state = PreviewContactState.QUEUED_OFFLINE,
        lastMessage = "好友申请待联网发送",
        timeLabel = "周一",
    ),
)

private val sampleMessages = mapOf(
    "lin" to listOf(
        PreviewMessage(
            id = "lin-1",
            mine = false,
            author = "林然",
            content = MessageContent.Text("桌面双栏预览已经整理好了，右栏是当前会话。"),
            sendState = null,
            timeLabel = "18:36",
        ),
        PreviewMessage(
            id = "lin-2",
            mine = true,
            author = "我",
            content = MessageContent.Text("收到。发送框只有好友状态为 accepted 时才会启用。"),
            sendState = SendState.ACCEPTED,
            timeLabel = "18:38",
        ),
        PreviewMessage(
            id = "lin-3",
            mine = true,
            author = "我",
            content = MessageContent.Attachment(
                id = AttachmentId("preview-attachment-1"),
                caption = "界面稿预览（附件能力尚未接入）",
            ),
            sendState = SendState.DELIVERED,
            timeLabel = "18:39",
        ),
        PreviewMessage(
            id = "lin-4",
            mine = false,
            author = "林然",
            content = MessageContent.Extension(
                namespace = "chat.sample.preview",
                schemaVersion = 99,
                payload = "<script>不会执行</script>".encodeToByteArray(),
                fallbackText = "这是一条当前版本不支持的扩展消息，只显示安全文字回退。",
            ),
            sendState = null,
            timeLabel = "18:41",
        ),
        PreviewMessage(
            id = "lin-5",
            mine = false,
            author = "林然",
            content = MessageContent.Text("收到，明天见。"),
            sendState = null,
            timeLabel = "18:42",
        ),
        PreviewMessage(
            id = "lin-6",
            mine = false,
            author = "林然",
            content = MessageContent.Text(
                "## 明天的安排\n" +
                    "- **10:00** 第一节课\n" +
                    "- 11:30 讨论 *组队作业*\n" +
                    "- 17:00 交草稿\n" +
                    "\n" +
                    "> 记得带上学生证\n" +
                    "\n" +
                    "启动命令：\u0060chat-p0\u0060，参考 [课程页](https://example.edu/course)。",
            ),
            sendState = null,
            timeLabel = "18:44",
        ),
    ),
    "zhou" to listOf(
        PreviewMessage(
            id = "zhou-1",
            mine = false,
            author = "周其",
            content = MessageContent.Text("好友申请：请先核对我的指纹，再决定是否确认。"),
            sendState = null,
            timeLabel = "昨天",
        ),
    ),
    "gu" to listOf(
        PreviewMessage(
            id = "gu-1",
            mine = false,
            author = "顾问",
            content = MessageContent.Text("我更换了设备，请重新核验指纹后再继续。"),
            sendState = null,
            timeLabel = "周一",
        ),
    ),
    "su" to listOf(
        PreviewMessage(
            id = "su-1",
            mine = true,
            author = "我",
            content = MessageContent.Text("好友申请已保存在本端，等待联网发送。"),
            sendState = SendState.QUEUED,
            timeLabel = "周一",
        ),
    ),
)

private fun contactForScenario(scenario: PreviewScenario): String? = when (scenario) {
    PreviewScenario.ACCEPTED -> "lin"
    PreviewScenario.WAITING_FOR_PEER -> "zhou"
    PreviewScenario.QUEUED_OFFLINE -> "su"
    PreviewScenario.IDENTITY_CHANGED -> "gu"
    PreviewScenario.LOCKED -> "lin"
    PreviewScenario.EMPTY -> null
}

fun PreviewChatState.applyScenario(scenario: PreviewScenario): PreviewChatState = copy(
    scenario = scenario,
    locked = scenario == PreviewScenario.LOCKED,
    selectedContactId = contactForScenario(scenario),
    paging = samplePreviewPaging(contactForScenario(scenario) ?: PREVIEW_PAGING_SAMPLE_CONVERSATION),
    showInvite = false,
    inviteStep = PreviewInviteStep.INPUT,
    inviteText = "",
    inviteOffline = false,
    inviteValidation = PreviewInviteValidation.NOT_STARTED,
    inviteNotice = null,
)

fun PreviewChatState.selectContact(contactId: String): PreviewChatState {
    if (locked) return this
    val contact = contacts.firstOrNull { it.id == contactId } ?: return this
    val scenario = when (contact.state) {
        PreviewContactState.ACCEPTED -> PreviewScenario.ACCEPTED
        PreviewContactState.WAITING_FOR_PEER,
        PreviewContactState.WAITING_FOR_BOTH_SIDES -> PreviewScenario.WAITING_FOR_PEER
        PreviewContactState.QUEUED_OFFLINE -> PreviewScenario.QUEUED_OFFLINE
        PreviewContactState.IDENTITY_CHANGED -> PreviewScenario.IDENTITY_CHANGED
        PreviewContactState.EXPIRED,
        PreviewContactState.FAILED -> PreviewScenario.ACCEPTED
    }
    return copy(
        selectedContactId = contactId,
        scenario = scenario,
        paging = samplePreviewPaging(contactId),
    )
}

/** 推进一步发送状态样例（待发送 → 已接受 → 待对账 → 可重试失败 → 权限阻断 → 已送达）。 */
fun PreviewChatState.advanceDraftSample(): PreviewChatState =
    copy(draftSample = draftSample.next())

/**
 * 推进一步分页样例：加载成功 / 失败 / 到底，都由合并器真实执行。
 * 到达“没有更早的消息”后，再次点击会重置回样例开头，便于反复观察提示。
 */
fun PreviewChatState.advancePagingSample(): PreviewChatState =
    if (paging.pagingPresentation().hint == PagingHint.NoMore) {
        copy(paging = samplePreviewPaging(selectedContactId ?: PREVIEW_PAGING_SAMPLE_CONVERSATION))
    } else {
        copy(paging = paging.nextSampleStep())
    }

fun PreviewChatState.updateSearch(value: String): PreviewChatState = copy(search = value)

fun PreviewChatState.openInvite(): PreviewChatState = copy(
    showInvite = true,
    inviteStep = PreviewInviteStep.INPUT,
    inviteText = "",
    inviteOffline = false,
    inviteValidation = PreviewInviteValidation.NOT_STARTED,
    inviteNotice = null,
)

fun PreviewChatState.updateInviteText(value: String): PreviewChatState = copy(
    inviteText = value,
    inviteValidation = PreviewInviteValidation.NOT_STARTED,
    inviteNotice = null,
)

fun PreviewChatState.fillSampleInviteToken(): PreviewChatState = copy(
    inviteText = PREVIEW_SAMPLE_INVITE_TOKEN,
    inviteValidation = PreviewInviteValidation.NOT_STARTED,
    inviteNotice = "已填入内置样例口令，可查看样例核对流程。",
)

/**
 * 只接受内置样例 token。任意输入不会伪装成已验证身份，也不会执行签名/有效期校验。
 */
fun PreviewChatState.validateInvite(): PreviewChatState {
    val candidate = inviteText.trim()
    if (candidate.isEmpty()) {
        return copy(
            inviteStep = PreviewInviteStep.INPUT,
            inviteValidation = PreviewInviteValidation.EMPTY,
            inviteNotice = "请先粘贴口令，或使用“填入内置样例口令”。",
        )
    }
    if (candidate != PREVIEW_SAMPLE_INVITE_TOKEN) {
        return copy(
            inviteStep = PreviewInviteStep.INPUT,
            inviteValidation = PreviewInviteValidation.NON_SAMPLE_INPUT,
            inviteNotice = "样例只接受内置口令；任意输入不会被当作已验证身份，也不会执行真实校验。",
        )
    }
    return copy(
        inviteStep = PreviewInviteStep.IDENTITY,
        inviteValidation = PreviewInviteValidation.SAMPLE_TOKEN,
        inviteNotice = null,
    )
}

fun PreviewChatState.backToInviteInput(): PreviewChatState = copy(
    inviteStep = PreviewInviteStep.INPUT,
    inviteValidation = PreviewInviteValidation.NOT_STARTED,
    inviteNotice = null,
)

fun PreviewChatState.confirmInviteRequest(): PreviewChatState {
    if (inviteStep != PreviewInviteStep.IDENTITY) return this
    return copy(inviteStep = PreviewInviteStep.RESULT)
}

fun PreviewChatState.closeInvite(): PreviewChatState = copy(
    showInvite = false,
    inviteValidation = PreviewInviteValidation.NOT_STARTED,
    inviteNotice = null,
)

fun PreviewChatState.openLockedPreview(): PreviewChatState = applyScenario(PreviewScenario.LOCKED)

fun PreviewChatState.addSampleMessage(text: String): PreviewChatState {
    val contactId = selectedContactId ?: return this
    val contact = selectedContact ?: return this
    if (contact.state != PreviewContactState.ACCEPTED || text.isBlank()) return this
    val message = PreviewMessage(
        id = "sample-${messagesByContact[contactId].orEmpty().size + 1}",
        mine = true,
        author = "我",
        content = MessageContent.Text(text),
        sendState = SendState.QUEUED,
        timeLabel = "现在",
    )
    return copy(
        messagesByContact = messagesByContact + (contactId to messagesByContact[contactId].orEmpty() + message),
    )
}
