package chat.desktop

import chat.api.AttachmentId
import chat.api.CoreFailure
import chat.api.MessageContent
import chat.api.P0Core
import chat.api.P0Delivery
import chat.api.P0SessionInfo
import chat.api.IdentityContact
import chat.api.IdentityInviteSummary
import chat.api.StoredConversation
import chat.api.StoredDraft
import chat.api.LocalAttachment
import chat.api.SendStep
import chat.api.StoredDraftStatus
import chat.presentation.display.toBadge
import chat.api.StoredMessage
import chat.api.StoredPage
import chat.presentation.P0Presenter
import chat.presentation.P0Operation
import chat.presentation.P0UiState
import chat.presentation.p0Utf8ByteCount
import chat.presentation.display.AttachmentDisplayState
import chat.presentation.display.ConversationListDisplayState
import chat.presentation.display.ConversationMessagesDisplayState
import chat.presentation.display.ConversationSummaryView
import chat.presentation.display.StoredDraftState
import chat.presentation.display.StoredDraftStatusKind
import chat.presentation.display.toDraftUiState
import chat.presentation.display.toConversationDisplayPage
import chat.presentation.display.toDraftEvidence
import chat.presentation.display.toMessageDisplayPage
import chat.presentation.display.DisplayPage
import chat.presentation.display.DisplayQueryIntent
import chat.presentation.display.DraftAction
import chat.presentation.display.DraftEvidence
import chat.presentation.display.DraftInteraction
import chat.presentation.display.ContactStateUi
import chat.presentation.display.DraftPhase
import chat.presentation.display.ErrorAction
import chat.presentation.display.InviteCheck
import chat.presentation.display.presentContact
import chat.presentation.display.presentInviteError
import chat.presentation.display.presentInvitePreview
import chat.presentation.display.FROZEN_AUTO_RETRY_CODES
import chat.presentation.display.FROZEN_ERROR_CODES
import chat.presentation.display.LayoutMode
import chat.presentation.display.MarkdownBlock
import chat.presentation.display.MarkdownCommand
import chat.presentation.display.MarkdownDraft
import chat.presentation.display.MarkdownInline
import chat.presentation.display.MarkdownLimits
import chat.presentation.display.MessageDisplayView
import chat.presentation.display.MessageRenderModel
import chat.presentation.display.NavigationState
import chat.presentation.display.PagingHint
import chat.presentation.display.SinglePanePage
import chat.presentation.display.WindowSize
import chat.presentation.display.applyConversationListPage
import chat.presentation.display.applyMarkdownCommand
import chat.presentation.display.applyMessagePage
import chat.presentation.display.blocksNewSend
import chat.presentation.display.buildAdaptiveLayout
import chat.presentation.display.failConversationListQuery
import chat.presentation.display.failMessageQuery
import chat.presentation.display.initialConversationDisplayState
import chat.presentation.display.isCompactWidth
import chat.presentation.display.isTwoPaneWidth
import chat.presentation.display.loadMoreConversations
import chat.presentation.display.loadOlderMessages
import chat.presentation.display.markdownPlainTextOrFallback
import chat.presentation.display.morePagingPresentation
import chat.presentation.display.olderPagingPresentation
import chat.presentation.display.presentDraft
import chat.presentation.display.presentError
import chat.presentation.display.retryAllowed
import chat.presentation.display.refreshConversationList
import chat.presentation.display.refreshSelectedConversation
import chat.presentation.display.renderMarkdown
import chat.presentation.display.renderMarkdownMessage
import chat.presentation.display.renderMessageContent
import chat.presentation.display.renderUnknownMessageContent
import chat.presentation.display.sanitizeDisplayText
import chat.presentation.display.selectConversation
import chat.presentation.preview.ComposerPresentation
import chat.presentation.preview.PREVIEW_SAMPLE_INVITE_TOKEN
import chat.presentation.preview.PreviewContactState
import chat.presentation.preview.PreviewInviteStep
import chat.presentation.preview.PreviewInviteValidation
import chat.presentation.preview.PreviewLayout
import chat.presentation.preview.PreviewScenario
import chat.presentation.preview.PreviewStoreState
import chat.presentation.preview.opened
import chat.presentation.preview.selecting
import chat.presentation.preview.withConversations
import chat.presentation.preview.withDraft
import chat.presentation.preview.withDraftStatus
import chat.presentation.preview.withSendStep
import chat.presentation.preview.withError
import chat.presentation.preview.withMessages
import chat.presentation.preview.addSampleMessage
import chat.presentation.preview.advanceDraftSample
import chat.presentation.preview.advancePagingSample
import chat.presentation.preview.applyScenario
import chat.presentation.preview.backToInviteInput
import chat.presentation.preview.closeInvite
import chat.presentation.preview.composerPresentation
import chat.presentation.preview.confirmInviteRequest
import chat.presentation.preview.fillSampleInviteToken
import chat.presentation.preview.layoutModel
import chat.presentation.preview.next
import chat.presentation.preview.nextSampleStep
import chat.presentation.preview.openInvite
import chat.presentation.preview.pagingPresentation
import chat.presentation.preview.previewLayoutFor
import chat.presentation.preview.samplePreviewPaging
import chat.presentation.preview.samplePreviewState
import chat.presentation.preview.selectContact
import chat.presentation.preview.updateInviteText
import chat.presentation.preview.validateInvite
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout

/**
 * DS-02 / DS-04 的局部验证程序。
 *
 * 使用可控假 P0Core 验证连点、迟到成功/失败、取消与关闭竞争、输入上限；
 * 纯函数部分覆盖未知展示类型、未知错误码和不可重试错误。
 */
fun main() = runBlocking {
    println("START DS-02/DS-04/DS-05/DS-07/DS-08/DS-09/DS-10/DS-11/DS-12 presentation checks")
    checkDuplicateClickAndLateEditing()
    checkUnknownSendResult()
    checkCloseDuringSend()
    checkInputLimit()
    checkRenderModels()
    checkErrorPresentation()
    checkFrozenErrorCoverage()
    checkCoreProjection()
    checkIdentityPresentation()
    checkStorePanelState()
    checkDraftStatusProjection()
    checkAttachmentBadges()
    checkSendStepProjection()
    checkPreviewStates()
    checkPreviewInviteValidation()
    checkConversationDisplayMerger()
    checkAdaptiveLayout()
    checkDraftSendPresentation()
    checkPagingPresentation()
    checkMarkdown()
    println("PASS DS-02/DS-04/DS-05/DS-07/DS-08/DS-09/DS-10/DS-11/DS-12 presentation checks")
}

private class FakeP0Core : P0Core {
    val sendGate = CompletableDeferred<Unit>()
    @Volatile var sendCalls = 0
    @Volatile var closeCalls = 0
    @Volatile var lastSender: String? = null
    @Volatile var failure: Throwable? = null

    override suspend fun connect(): P0SessionInfo {
        return P0SessionInfo(
            aliceEndpoint = "alice-endpoint-sample",
            bobEndpoint = "bob-endpoint-sample",
            groupId = "group-sample",
            epoch = 1,
            ciphersuite = "MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519",
            transport = "iroh / loopback",
            publicInfrastructure = false,
        )
    }

    override suspend fun send(sender: String, text: String): P0Delivery {
        sendCalls++
        lastSender = sender
        sendGate.await()
        failure?.let { throw it }
        return P0Delivery(
            sender = sender,
            receiver = if (sender == "Alice") "Bob" else "Alice",
            text = text,
            ciphertextBytes = 128 + text.encodeToByteArray().size,
            ciphertextSha256 = "sample-hash",
            epoch = 1,
            exchangeIndex = sendCalls.toLong(),
            elapsedMs = 7,
        )
    }

    override suspend fun close() {
        closeCalls++
    }
}

private suspend fun P0Presenter.awaitState(
    timeoutMillis: Long = 5_000,
    predicate: (P0UiState) -> Boolean,
): P0UiState = withTimeout(timeoutMillis) { state.first(predicate) }

private suspend fun FakeP0Core.awaitSendStarted() {
    withTimeout(5_000) {
        while (sendCalls == 0) delay(10)
    }
}

private suspend fun checkDuplicateClickAndLateEditing() {
    val core = FakeP0Core()
    val presenter = P0Presenter(core)
    try {
        presenter.connect()
        presenter.awaitState { !it.busy && it.canSend }

        presenter.updateComposer("第一条")
        presenter.send()
        presenter.awaitState { it.busy && it.operation == P0Operation.SENDING }
        core.awaitSendStarted()

        presenter.send()
        presenter.selectSender("Bob")
        presenter.updateComposer("正在输入的下一段")
        check(core.sendCalls == 1) { "连点不应产生第二次 send 调用" }

        core.sendGate.complete(Unit)
        val done = presenter.awaitState { !it.busy && it.messages.size == 1 }
        check(core.lastSender == "Alice") { "发送者应在点击时冻结" }
        check(done.messages.single().text == "第一条")
        check(done.composer == "正在输入的下一段") { "迟到成功不应清掉新输入" }
        println("PASS DS-02 duplicate click, frozen sender, late editing")
    } finally {
        core.sendGate.complete(Unit)
        withTimeout(3_000) { presenter.close() }
        presenter.dispose()
    }
}

private suspend fun checkUnknownSendResult() {
    val core = FakeP0Core().apply { failure = IllegalStateException("internal secret detail 42") }
    val presenter = P0Presenter(core)
    try {
        presenter.connect()
        presenter.awaitState { !it.busy && it.canSend }
        presenter.updateComposer("会失败的消息")
        presenter.send()
        presenter.awaitState { it.busy && it.operation == P0Operation.SENDING }
        core.awaitSendStarted()
        core.sendGate.complete(Unit)

        val done = presenter.awaitState { !it.busy && it.error != null }
        check(done.error?.code == "P0_SEND_UNKNOWN")
        check(done.error?.detail?.contains("internal secret") != true) { "异常原文不得进入 UI" }
        check(!done.canSend)
        check(done.requiresRebuild) { "结果未知必须进入不可关闭的重建状态" }
        check(done.composer == "会失败的消息") { "结果未知时保留用户输入" }
        check(core.sendCalls == 1)

        presenter.dismissError()
        val dismissed = presenter.state.value
        check(dismissed.error == null)
        check(dismissed.requiresRebuild) { "关闭错误横幅不能清掉重建状态" }
        check(!dismissed.canSend)
        check(dismissed.composer == "会失败的消息")

        presenter.updateComposer("结果未知后继续编辑")
        presenter.selectSender("Bob")
        presenter.send()
        delay(100)
        check(core.sendCalls == 1) { "结果未知后不得自动或静默重发" }
        val stillUnknown = presenter.state.value
        check(stillUnknown.error == null) { "结果未知下再次点击发送不应生成新的错误横幅" }
        check(stillUnknown.requiresRebuild && !stillUnknown.canSend)
        check(stillUnknown.composer == "结果未知后继续编辑") { "编辑草稿不能被重建状态清掉" }
        check(stillUnknown.sender == "Bob")

        presenter.connect()
        val reconnected = presenter.awaitState { !it.busy && it.canSend }
        check(!reconnected.requiresRebuild)
        check(reconnected.session != null)
        check(reconnected.composer == "结果未知后继续编辑") { "重建会话不得丢失用户草稿" }
        println("PASS DS-02/DS-05 unknown send result, non-dismissible rebuild state, draft preserved")
    } finally {
        core.sendGate.complete(Unit)
        withTimeout(3_000) { presenter.close() }
        presenter.dispose()
    }
}

private suspend fun checkCloseDuringSend() {
    val core = FakeP0Core()
    val presenter = P0Presenter(core)
    try {
        presenter.connect()
        presenter.awaitState { !it.busy && it.canSend }
        presenter.updateComposer("关闭中的消息")
        presenter.send()
        presenter.awaitState { it.busy && it.operation == P0Operation.SENDING }
        core.awaitSendStarted()

        val closeJob = coroutineScope {
            val job = launch { presenter.close() }
            delay(100)
            check(!job.isCompleted) { "关闭应等待在途发送结束" }

            core.sendGate.complete(Unit)
            withTimeout(5_000) { job.join() }
            job
        }
        check(closeJob.isCompleted)

        val done = presenter.state.value
        check(done.closed)
        check(!done.busy && done.operation == null)
        check(done.session == null && !done.canSend)
        check(!done.requiresRebuild) { "页面关闭后不再展示可重建状态" }
        check(done.status == "会话已关闭")
        check(core.closeCalls >= 2) { "连接前清理与最终关闭都应调用 core.close" }
        println("PASS DS-02 close waits for in-flight send; final state closed")
    } finally {
        core.sendGate.complete(Unit)
        presenter.dispose()
    }
}

private fun checkInputLimit() {
    val core = FakeP0Core()
    val presenter = P0Presenter(core)
    check(p0Utf8ByteCount("a") == 1)
    check(p0Utf8ByteCount("你") == 3)

    presenter.updateComposer("你".repeat(2_730))
    check(presenter.state.value.composerBytes == 8_190)
    check(presenter.state.value.error == null)

    presenter.updateComposer("你".repeat(2_731))
    check(presenter.state.value.composerBytes == 8_190)
    check(presenter.state.value.error?.code == "P0_INPUT_TOO_LARGE")
    println("PASS DS-02 UTF-8 byte limit keeps previous draft and shows friendly error")
    presenter.dispose()
}

private fun checkRenderModels() {
    val text = renderMessageContent(MessageContent.Text("  hello\u0000\u202E\nworld  "))
    check(text is MessageRenderModel.Text && text.text == "hello\nworld")

    val attachment = renderMessageContent(
        MessageContent.Attachment(AttachmentId("att-1"), "  设计稿  "),
    )
    check(attachment is MessageRenderModel.Attachment)
    check(attachment.state == AttachmentDisplayState.CAPABILITY_PENDING)
    check(attachment.caption == "设计稿")
    check(attachment.badge.contains("下载") && attachment.badge.contains("尚未接入"))

    val extension = renderMessageContent(
        MessageContent.Extension(
            namespace = "unknown.sample",
            schemaVersion = 99,
            payload = byteArrayOf(1, 2, 3),
            fallbackText = "<b>回退</b>\u0007",
        ),
    )
    check(extension is MessageRenderModel.ExtensionFallback)
    check(extension.title == "不支持的扩展消息")
    check(extension.text == "<b>回退</b>") { "扩展 fallback 只按纯文本展示，不解析富文本" }

    val unknown = renderUnknownMessageContent()
    check(unknown is MessageRenderModel.ExtensionFallback && unknown.text.contains("安全回退"))

    val sanitized = sanitizeDisplayText("x".repeat(5_000))
    check(sanitized.length == 4_001) { "超长内容应截断并加省略号" }
    println("PASS DS-04 text/attachment/extension/unknown render models")
}

private fun checkErrorPresentation() {
    val stale = presentError(CoreFailure("STALE_EPOCH", true, null, "op-123"))
    check(stale.action == ErrorAction.WAIT)
    check(stale.title.contains("同步"))

    val conflict = presentError(CoreFailure("LOG_CONFLICT", false, null, null))
    check(conflict.detail.contains("暂停"))
    check(conflict.action != ErrorAction.RETRY && conflict.actionLabel == null)

    val denied = presentError(CoreFailure("PERMISSION_DENIED", false, null, null))
    check(denied.action == ErrorAction.NONE && denied.actionLabel == null)

    val unknown = presentError(CoreFailure("<script>", false, null, "bad id with spaces"))
    check(unknown.action == ErrorAction.NONE)
    check(unknown.diagnosticCode == null && unknown.diagnosticId == null)
    check(!unknown.detail.contains("<script>"))

    val retryableUnknown = presentError(CoreFailure("SOMETHING_NEW", true, 1_000, "op-1"))
    check(retryableUnknown.action == ErrorAction.RETRY && retryableUnknown.retryable)

    val nonRetryableStorage = presentError(CoreFailure("STORAGE_FAILURE", false, null, null))
    check(nonRetryableStorage.action == ErrorAction.NONE)
    println("PASS DS-04 known/unknown/non-retryable error mapping")
}

private fun checkSendStepProjection() {
    fun step(status: String, settled: Boolean, submissionId: String? = "sub-1") = SendStep(
        draftId = "draft-" + "ab".repeat(16),
        status = status,
        submissionId = submissionId,
        attemptIndex = 0,
        detail = "静态说明",
        settled = settled,
    )

    var state = PreviewStoreState().opened("/tmp/demo.db", "opened")
    state = state.withSendStep(step("accepted", settled = true))
    check(state.notice?.contains("节点已接受") == true)
    check(state.sendStep?.settled == true)

    state = state.withSendStep(step("unknown", settled = false))
    check(state.notice?.contains("只能对账") == true) { "未知结果必须提示只对账，不能提示成功" }

    state = state.withSendStep(step("rejected", settled = true))
    check(state.notice?.contains("拒绝") == true)

    state = state.withSendStep(step("prepared", settled = true))
    check(state.notice?.contains("已加密待提交") == true) { "prepared 不得显示为已接受" }

    state = state.withSendStep(step("submitted", settled = false))
    check(state.notice?.contains("只能对账") == true)

    println("PASS send steps: accepted/unknown/rejected/prepared map to honest labels (unknown never claims success)")
}

private fun checkAttachmentBadges() {
    fun attachment(state: String) = LocalAttachment(
        attachmentId = "att-" + "ab".repeat(16),
        plainSize = 1024,
        chunkSize = 64 * 1024,
        chunkCount = 2,
        contentDigest = "cd".repeat(32),
        displayName = "示例.bin",
        state = state,
        availableChunks = 2,
        transferredChunks = if (state == "available") 2 else 0,
        cancelRequested = false,
    )

    check(!attachment("imported").toBadge().downloadable) { "只导入未上传时不得声称对方可下载" }
    check(attachment("imported").toBadge().cancellable)
    check(!attachment("uploading").toBadge().downloadable) { "上传中不得声称可下载" }
    check(attachment("uploading").toBadge().cancellable) { "上传中允许取消" }
    val available = attachment("available").toBadge()
    check(available.downloadable) { "节点已保存全部密文块后才可声称可下载" }
    check(!available.cancellable) { "已上传的传输不提供取消" }
    check(available.label == "已上传")
    check(!attachment("downloading").toBadge().downloadable)
    check(!attachment("cancelled").toBadge().downloadable)
    check(!attachment("failed").toBadge().downloadable)
    val unknown = attachment("weird-status").toBadge()
    check(!unknown.downloadable) { "未知状态必须按不可下载处理" }
    check(unknown.label == "状态未知")

    println("PASS attachment badges: only available is downloadable; uploading/cancelled/failed/unknown never claim delivery")
}

private fun checkDraftStatusProjection() {
    fun status(
        state: String,
        canRetry: Boolean,
        canCancel: Boolean,
        submissionId: String? = "sub-1",
        attempts: Int = 1,
    ) = StoredDraftStatus(
        draftId = "draft-" + "ab".repeat(16),
        conversationId = "conv:1:" + "aa".repeat(32),
        operationId = "op:1:" + "cd".repeat(32),
        status = state,
        submissionId = submissionId,
        attemptIndex = 0,
        attempts = attempts,
        updatedAtMillis = 1_700_000_000_000,
        canRetry = canRetry,
        canCancel = canCancel,
    )

    val accepted = status(StoredDraftStatusKind.ACCEPTED, canRetry = false, canCancel = false).toDraftUiState()
    check(accepted.phase == DraftPhase.ACCEPTED)
    check(accepted.label == "已接受")
    check(!accepted.canRetry && !accepted.canCancel) { "已接受不得提供重试或取消" }

    val unknown = status(StoredDraftStatusKind.UNKNOWN, canRetry = false, canCancel = true).toDraftUiState()
    check(unknown.phase == DraftPhase.ACCEPTANCE_UNKNOWN)
    check(unknown.actions == listOf(DraftAction.RECONCILE)) { "未知结果只能对账（取消不是重发）" }
    check(!unknown.canRetry)

    val rejectedRetryable = status(StoredDraftStatusKind.REJECTED, canRetry = true, canCancel = true).toDraftUiState()
    check(rejectedRetryable.phase == DraftPhase.FAILED_RETRYABLE)
    check(rejectedRetryable.canRetry) { "可重试的拒绝才给重试" }

    val rejectedBlocked = status(StoredDraftStatusKind.REJECTED, canRetry = false, canCancel = true).toDraftUiState()
    check(!rejectedBlocked.canRetry) { "底层 canRetry=false 时不得提供重试" }

    val cancelled = status(StoredDraftStatusKind.CANCELLED, canRetry = false, canCancel = false).toDraftUiState()
    check(cancelled.phase == DraftPhase.CANCEL_PENDING)
    check(cancelled.actions.isEmpty())

    val prepared = status(StoredDraftStatusKind.PREPARED, canRetry = true, canCancel = true).toDraftUiState()
    check(prepared.phase == DraftPhase.QUEUED) { "prepared/submitted 只是本机已加密待发，不能显示已接受" }

    // 状态直接喂进预览面板状态
    var store = PreviewStoreState().opened("/tmp/demo.db", "opened")
    store = store.withDraftStatus(status(StoredDraftStatusKind.ACCEPTED, false, false))
    check(store.drafts.size == 1)
    check(store.drafts.first().phase == DraftPhase.ACCEPTED)
    check(store.notice?.contains("accepted") == true)

    println("PASS CORE-010 projection: accepted/unknown/rejected/cancelled map to actions, canRetry/canCancel are authoritative")
}

private fun checkStorePanelState() {
    var state = PreviewStoreState()
    state = state.opened("/tmp/demo.db", "已打开")
    check(state.ready && state.lastErrorCode == null)
    check(state.databasePath == "/tmp/demo.db")

    // 真实会话/消息投影：游标透传、去重、unread 保持 null
    state = state.withConversations(
        cursor = "cur-2",
        items = listOf(
            StoredConversation("c1", "林然", "第一条", unreadCount = null, updatedAtMillis = 10),
            StoredConversation("c1", "林然", "更新后", unreadCount = null, updatedAtMillis = 11),
            StoredConversation("c2", "周其", "另一条", unreadCount = 2, updatedAtMillis = 9),
        ),
    )
    check(state.conversations.map { it.id } == listOf("c1", "c2")) { "重复会话必须合并" }
    check(state.conversations.first().preview == "更新后")
    check(state.conversations.first().unreadCount == null) { "没有 unread 计数时必须保持 null" }
    check(state.nextConversationCursor == "cur-2")

    state = state.selecting("c1")
    check(state.selectedConversationId == "c1" && state.messages.isEmpty())
    state = state.withMessages(
        "c1",
        cursor = null,
        items = listOf(
            StoredMessage("m1", "c1", "林然", "你好", mine = false, createdAtMillis = 1),
            StoredMessage("m1", "c1", "林然", "你好（更新）", mine = false, createdAtMillis = 1),
        ),
    )
    check(state.messages.map { it.id } == listOf("m1"))
    check(state.messages.first().body == "你好（更新）")

    // 选中会话以外的迟到结果不得覆盖当前会话
    val otherConversation = state.withMessages("c2", null, listOf(StoredMessage("x", "c2", "周其", "别的", false, 2)))
    check(otherConversation.messages == state.messages) { "非当前会话的结果必须被忽略" }

    // 草稿状态只显示本机事实：queued 不得变成已接受
    val draft = StoredDraft("draft-" + "ab".repeat(16), "op:1:" + "cd".repeat(32), "c1", "**草稿**", "queued", null)
    state = state.withDraft(draft)
    check(state.drafts.size == 1)
    check(state.drafts.first().phase == DraftPhase.QUEUED)
    check(state.drafts.first().label == "待发送")
    check(!state.drafts.first().canRetry) { "本机排队草稿不得出现重试" }

    val prepared = StoredDraft("draft-" + "ef".repeat(16), "op:1:" + "12".repeat(32), "c1", "x", "prepared", "sub-1")
    state = state.withDraft(prepared)
    check(state.drafts.size == 2)
    check(state.drafts.first { it.draftId == prepared.draftId }.phase == DraftPhase.QUEUED) {
        "prepared 仍只是本机尝试，不得升级为已接受"
    }

    // 错误态不得被当成成功
    val failed = state.withError("STORAGE_LOCKED", "库被占用")
    check(failed.lastErrorCode == "STORAGE_LOCKED" && failed.notice == "库被占用")

    println("PASS P2-01 store panel: real page dedupe, cursor passthrough, null unread, draft stays queued")
}

private fun checkIdentityPresentation() {
    val now = 1_700_000_000_000
    val summary = IdentityInviteSummary(
        inviteId = "inv-1",
        accountId = "acct:1:" + "aa".repeat(32),
        deviceId = "dev:1:" + "bb".repeat(32),
        fingerprint = "7EC8 C5E1 A37C C538 AFCB A0B0 BD28 C9E1 5A4B 1A3E 53DC 0F18 BE09 6DF9 4742 8EB4",
        node = "node-a.local",
        expiresAtMillis = now + 30 * 60_000,
    )
    val valid = presentInvitePreview(summary, now)
    check(valid.check == InviteCheck.VALID && valid.valid)
    check(valid.fingerprint == summary.fingerprint) { "指纹必须原样展示，不能被截断或改写" }
    check(valid.remainingMinutes == 30L)
    check(valid.accountId == summary.accountId && valid.deviceId == summary.deviceId)

    val expired = presentInvitePreview(summary.copy(expiresAtMillis = now - 1), now)
    check(expired.check == InviteCheck.EXPIRED && !expired.valid)
    check(expired.fingerprint == null) { "过期口令不得展示对方身份" }

    // 错误码到可读结论的映射（不把失败说成成功）
    check(presentInviteError("INVALID_SIGNATURE").check == InviteCheck.TAMPERED)
    check(presentInviteError("CURSOR_EXPIRED").check == InviteCheck.EXPIRED)
    check(presentInviteError("INVALID_TARGET").check == InviteCheck.WRONG_TARGET)
    check(presentInviteError("INVALID_ID").check == InviteCheck.UNKNOWN)
    val unknownError = presentInviteError("SOMETHING_NEW", "op-9")
    check(unknownError.check == InviteCheck.UNKNOWN)
    check(!unknownError.valid)
    check(unknownError.title.isNotBlank() && unknownError.detail.contains("op-9"))

    // 联系人状态：只有 accepted + 底层 canSend 才允许发送
    fun contact(state: String, canSend: Boolean) = IdentityContact(
        contactId = "c-" + state,
        accountId = "acct:1:" + "cc".repeat(32),
        displayName = "林然",
        fingerprint = summary.fingerprint,
        state = state,
        canSend = canSend,
    )
    val accepted = presentContact(contact("accepted", true))
    check(accepted.state == ContactStateUi.ACCEPTED && accepted.canSend)
    check(accepted.blockingReason == null)

    val requested = presentContact(contact("requested", false))
    check(!requested.canSend && requested.blockingReason != null)

    val changed = presentContact(contact("identity_changed", true))
    check(changed.state == ContactStateUi.IDENTITY_CHANGED)
    check(!changed.canSend) { "身份变化必须阻断发送，即使底层误报 canSend=true" }
    check(changed.blockingReason?.contains("指纹") == true)

    val declaredAcceptedButBlocked = presentContact(contact("accepted", false))
    check(!declaredAcceptedButBlocked.canSend) { "底层 canSend=false 时不得放行" }

    val unknownState = presentContact(contact("weird", true))
    check(unknownState.state == ContactStateUi.UNKNOWN && !unknownState.canSend)

    println("PASS P1-02 identity presentation: invite verify result, error mapping, identity-changed blocks send")
}

private fun checkCoreProjection() {
    // 存储层状态只说明本端事实：queued/prepared 都不是"节点已接受"
    val queued = StoredDraft("d1", "op-1", "c1", "hello", StoredDraftState.QUEUED, submissionId = null)
        .toDraftEvidence()
    check(queued.queuedLocally && queued.submissionId == null && !queued.acceptanceUnknown)
    val queuedUi = presentDraft(queued)
    check(queuedUi.phase == DraftPhase.QUEUED) { "queued 只能显示待发送" }
    check(!queuedUi.canRetry) { "本端排队不得出现重试" }

    val prepared = StoredDraft("d2", "op-2", "c1", "hello", StoredDraftState.PREPARED, submissionId = "sub-1")
        .toDraftEvidence()
    check(prepared.queuedLocally && prepared.submissionId == "sub-1")
    check(prepared.nodeAcceptedReceiptId == null && prepared.deliveredReceiptId == null) {
        "提交 ID 不是节点回执，也不是送达证据"
    }
    val preparedUi = presentDraft(prepared)
    check(preparedUi.phase == DraftPhase.QUEUED) { "prepared 仍只显示待发送，不得升级为已接受" }
    check(preparedUi.label != "已接受") { "本地尝试不得显示已接受" }

    // 未知状态安全回退：只承认草稿存在
    val unknown = StoredDraft("d3", "op-3", "c1", "hello", "weird-state", submissionId = null)
        .toDraftEvidence()
    check(!unknown.queuedLocally)
    check(presentDraft(unknown).phase == DraftPhase.LOCAL_ONLY)

    // 会话/消息分页原样透传，不伪造 unread、不改游标
    val conversations = StoredPage(
        items = listOf(
            StoredConversation("c1", "林然", "最新一条", unreadCount = null, updatedAtMillis = 10),
            StoredConversation("c2", "周其", "更旧的", unreadCount = 3, updatedAtMillis = 5),
        ),
        nextCursor = "cur-2",
    ).toConversationDisplayPage()
    check(conversations.items.map { it.id } == listOf("c1", "c2"))
    check(conversations.items[0].unreadCount == null) { "没有计数时必须保持 null，不能伪装成 0" }
    check(conversations.items[1].unreadCount == 3)
    check(conversations.nextCursor == "cur-2")

    val messages = StoredPage(
        items = listOf(
            StoredMessage("m1", "c1", "林然", "第一条", mine = false, createdAtMillis = 1),
            StoredMessage("m2", "c1", "我", "第二条", mine = true, createdAtMillis = 2),
        ),
        nextCursor = null,
    ).toMessageDisplayPage()
    check(messages.items.map { it.id } == listOf("m1", "m2"))
    check(messages.items[1].mine)
    check(messages.nextCursor == null)

    // 真实投影数据直接喂给 DS-08 合并器：去重、追加、代次校验都继续成立
    var display = initialConversationDisplayState()
    val listRequest = display.refreshConversationList()
    display = listRequest.state.applyConversationListPage(
        listRequest.intent as DisplayQueryIntent.LoadConversationList,
        conversations,
    ).state
    check(display.list.conversations.map { it.id } == listOf("c1", "c2"))
    val select = display.selectConversation("c1")
    val pageRequest = select.intent as DisplayQueryIntent.LoadMessagePage
    display = select.state.applyMessagePage(pageRequest, messages).state
    check(display.conversation.messages.map { it.id } == listOf("m1", "m2"))

    println("PASS P2-01 projection: stored facts → display without inferring acceptance, null unread kept")
}

private fun checkFrozenErrorCoverage() {
    check(FROZEN_ERROR_CODES.size == 41) { "冻结错误码应为 41 个，与 crates/protocol/src/error.rs 的 ErrorCode::ALL 对齐" }
    check(FROZEN_AUTO_RETRY_CODES.all { it in FROZEN_ERROR_CODES }) { "自动重试集合必须是冻结码的子集" }
    check(FROZEN_AUTO_RETRY_CODES.size == 7) { "冻结规则只允许 7 个码自动重试" }

    for (code in FROZEN_ERROR_CODES) {
        val presented = presentError(CoreFailure(code, retryable = false, retryAfterMillis = null, operationId = null))
        check(presented.title != "操作未完成") { "冻结码 " + code + " 必须有稳定文案，不能落到未知错误回退" }
        check(presented.detail.isNotBlank())
        check(presented.autoRetryAllowed == (code in FROZEN_AUTO_RETRY_CODES)) {
            "自动重试标记必须严格按冻结规则：" + code
        }
        check(presented.diagnosticCode == code) { "展示保留稳定诊断码：" + code }
    }

    // 手动重试与自动重试是两件事：存储失败可手动重试，但不允许自动退避重试。
    val storageFailure = presentError(CoreFailure("STORAGE_FAILURE", retryable = true, retryAfterMillis = null, operationId = "op-s"))
    check(storageFailure.retryable && storageFailure.action == ErrorAction.RETRY)
    check(!storageFailure.autoRetryAllowed) { "STORAGE_FAILURE 不在冻结自动重试集合内" }

    val network = presentError(CoreFailure("NETWORK_UNAVAILABLE", retryable = true, retryAfterMillis = 1000, operationId = null))
    check(network.retryable && network.autoRetryAllowed)

    val unknownResult = presentError(CoreFailure("UNKNOWN_RESULT", retryable = false, retryAfterMillis = null, operationId = null))
    check(unknownResult.action == ErrorAction.WAIT)
    check(!unknownResult.retryable && !unknownResult.autoRetryAllowed) { "结果未确定只能对账，不能自动重发" }

    val deviceLimit = presentError(CoreFailure("DEVICE_LIMIT_EXCEEDED", retryable = false, retryAfterMillis = null, operationId = null))
    check(deviceLimit.detail.contains("2 台")) { "设备上限文案必须与产品口径一致（每账户 2 台）" }

    val signature = presentError(CoreFailure("INVALID_SIGNATURE", retryable = true, retryAfterMillis = null, operationId = "op-x"))
    check(!signature.autoRetryAllowed) { "签名校验失败绝不允许自动重试" }

    println("PASS PROTOCOL-014 frozen error coverage: 41 stable copies, 7 auto-retry codes, manual vs auto retry split")
}

private fun checkPreviewStates() {
    val base = samplePreviewState()
    check(base.composerPresentation() is ComposerPresentation.Enabled)

    for (contact in base.contacts) {
        val state = base.selectContact(contact.id)
        val composer = state.composerPresentation()
        if (contact.state == PreviewContactState.ACCEPTED) {
            check(composer is ComposerPresentation.Enabled)
        } else {
            check(composer is ComposerPresentation.Blocked) { "${contact.displayName} 不应启用发送框" }
        }
    }

    val identity = base.applyScenario(PreviewScenario.IDENTITY_CHANGED)
    val blocked = identity.composerPresentation()
    check(blocked is ComposerPresentation.Blocked)
    check(blocked.title.contains("身份变化"))
    check(blocked.detail.contains("指纹"))

    check(base.applyScenario(PreviewScenario.LOCKED).locked)
    check(base.applyScenario(PreviewScenario.EMPTY).selectedContact == null)

    check(previewLayoutFor(1024) == PreviewLayout.TWO_PANE)
    check(previewLayoutFor(900) == PreviewLayout.TWO_PANE)
    check(previewLayoutFor(899) == PreviewLayout.SINGLE_PANE)
    check(previewLayoutFor(720) == PreviewLayout.SINGLE_PANE)

    val invite = base.openInvite().fillSampleInviteToken().validateInvite()
    check(invite.inviteStep == PreviewInviteStep.IDENTITY)
    check(invite.backToInviteInput().inviteStep == PreviewInviteStep.INPUT)
    check(invite.confirmInviteRequest().inviteStep == PreviewInviteStep.RESULT)
    check(!invite.closeInvite().showInvite)

    val withSample = base.addSampleMessage("样例新消息")
    check(withSample.messagesForSelected().last().content == MessageContent.Text("样例新消息"))
    println("PASS DS-03 preview state switching, accepted-only composer, 900 dp layout")
}

private fun checkPreviewInviteValidation() {
    val base = samplePreviewState().openInvite()

    val empty = base.updateInviteText("   ").validateInvite()
    check(empty.inviteStep == PreviewInviteStep.INPUT)
    check(empty.inviteValidation == PreviewInviteValidation.EMPTY)
    check(!empty.inviteNotice.isNullOrBlank())

    val arbitrary = base.updateInviteText("any untrusted invite text").validateInvite()
    check(arbitrary.inviteStep == PreviewInviteStep.INPUT) { "任意输入不能进入身份核对页" }
    check(arbitrary.inviteValidation == PreviewInviteValidation.NON_SAMPLE_INPUT)
    check(!arbitrary.inviteNotice.isNullOrBlank())
    check(arbitrary.inviteValidation != PreviewInviteValidation.SAMPLE_TOKEN)

    val filled = base.fillSampleInviteToken()
    check(filled.inviteText == PREVIEW_SAMPLE_INVITE_TOKEN)
    val sample = filled.validateInvite()
    check(sample.inviteStep == PreviewInviteStep.IDENTITY)
    check(sample.inviteValidation == PreviewInviteValidation.SAMPLE_TOKEN)
    check(sample.inviteNotice == null)

    val back = sample.backToInviteInput()
    check(back.inviteStep == PreviewInviteStep.INPUT)
    check(back.inviteValidation == PreviewInviteValidation.NOT_STARTED)
    check(sample.confirmInviteRequest().inviteStep == PreviewInviteStep.RESULT)
    check(!sample.closeInvite().showInvite)
    println("PASS DS-07 invite preview accepts only built-in sample token")
}

private fun checkConversationDisplayMerger() {
    var state = initialConversationDisplayState()

    val listRefresh = state.refreshConversationList()
    val listRequest = listRefresh.intent as DisplayQueryIntent.LoadConversationList
    state = listRefresh.state
    check(state.list.loading)
    state = state.applyConversationListPage(
        listRequest,
        DisplayPage(
            items = listOf(conv("c1", "林然"), conv("c1", "林然-更新"), conv("c2", "周其")),
            nextCursor = "list-2",
            reset = true,
        ),
    ).state
    check(state.list.conversations.map { it.id } == listOf("c1", "c2"))
    check(state.list.conversations.first().title == "林然-更新")
    check(state.list.nextCursor == "list-2")
    check(!state.list.loading)

    val staleList = state.applyConversationListPage(
        DisplayQueryIntent.LoadConversationList(generation = 0, cursor = null),
        DisplayPage(listOf(conv("c9", "迟到列表")), nextCursor = null, reset = true),
    )
    check(staleList.ignored)
    check(state.list.conversations.map { it.id } == listOf("c1", "c2"))

    val repeatedFirstPage = state.applyConversationListPage(
        listRequest,
        DisplayPage(listOf(conv("c1", "林然-重置"), conv("c2", "周其")), nextCursor = "list-2", reset = true),
    )
    check(repeatedFirstPage.state.list.conversations.map { it.id } == listOf("c1", "c2")) { "重复首页不得重复显示" }

    val more = state.loadMoreConversations()
    val moreRequest = more.intent as DisplayQueryIntent.LoadConversationList
    state = more.state
    state = state.applyConversationListPage(
        moreRequest,
        DisplayPage(listOf(conv("c2", "周其-更新"), conv("c3", "苏禾")), nextCursor = null),
    ).state
    check(state.list.conversations.map { it.id } == listOf("c1", "c2", "c3"))
    check(state.list.conversations[1].title == "周其-更新")
    val duplicateListPage = state.applyConversationListPage(
        moreRequest,
        DisplayPage(listOf(conv("c4", "重复页")), nextCursor = null),
    )
    check(duplicateListPage.ignored)
    check(duplicateListPage.state == state)

    val select = state.selectConversation("c1")
    val firstRequest = select.intent as DisplayQueryIntent.LoadMessagePage
    state = select.state
    state = state.applyMessagePage(
        firstRequest,
        DisplayPage(
            items = listOf(msg("m1", "c1"), msg("m2", "c1"), msg("m2", "c1")),
            nextCursor = "older-1",
            reset = true,
        ),
    ).state
    check(state.conversation.messages.map { it.id } == listOf("m1", "m2"))
    check(state.conversation.nextCursor == "older-1")

    val older = state.loadOlderMessages()
    val olderRequest = older.intent as DisplayQueryIntent.LoadMessagePage
    state = older.state
    val olderPage = DisplayPage(
        items = listOf(msg("m0", "c1"), msg("m1", "c1")),
        nextCursor = null,
        prepend = true,
    )
    state = state.applyMessagePage(olderRequest, olderPage).state
    check(state.conversation.messages.map { it.id } == listOf("m0", "m1", "m2"))
    val duplicateOlder = state.applyMessagePage(olderRequest, olderPage)
    check(duplicateOlder.ignored) { "同一分页结果不能显示两次" }

    val refresh = state.refreshSelectedConversation()
    val freshRequest = refresh.intent as DisplayQueryIntent.LoadMessagePage
    state = refresh.state
    state = state.applyMessagePage(
        freshRequest,
        DisplayPage(listOf(msg("m9", "c1")), nextCursor = null, reset = true),
    ).state
    check(state.conversation.messages.map { it.id } == listOf("m9"))
    val lateOlder = state.applyMessagePage(olderRequest, olderPage)
    check(lateOlder.ignored) { "重置后迟到的旧分页结果不得覆盖当前会话" }

    val selectC2 = state.selectConversation("c2")
    val c2Request = selectC2.intent as DisplayQueryIntent.LoadMessagePage
    state = selectC2.state
    val lateC1 = state.applyMessagePage(
        freshRequest,
        DisplayPage(listOf(msg("m10", "c1")), nextCursor = null, reset = true),
    )
    check(lateC1.ignored) { "切换会话后旧会话结果不得覆盖当前会话" }
    check(lateC1.state.conversation.conversationId == "c2")

    val c2Page = DisplayPage(
        items = listOf(msg("x1", "c2"), msg("x1", "c2"), msg("x2", "c2")),
        nextCursor = null,
        reset = true,
    )
    state = state.applyMessagePage(c2Request, c2Page).state
    check(state.conversation.messages.map { it.id } == listOf("x1", "x2"))
    state = state.applyMessagePage(c2Request, c2Page).state
    check(state.conversation.messages.map { it.id } == listOf("x1", "x2")) { "重复页不得显示两次" }

    val failedList = state.failConversationListQuery(listRequest, "列表查询失败")
    check(!failedList.ignored && failedList.state.list.error == "列表查询失败")
    val staleListFailure = state.failConversationListQuery(
        DisplayQueryIntent.LoadConversationList(generation = 0, cursor = null),
        "旧列表查询失败",
    )
    check(staleListFailure.ignored)
    val failedFresh = state.failMessageQuery(c2Request, "消息查询失败")
    check(!failedFresh.ignored && failedFresh.state.conversation.error == "消息查询失败")
    val staleMessageFailure = state.failMessageQuery(firstRequest, "旧消息查询失败")
    check(staleMessageFailure.ignored)
    println("PASS DS-08 display merger deduplicates, appends, resets and ignores late pages")
}

private fun checkAdaptiveLayout() {
    check(buildAdaptiveLayout(WindowSize(900, 800), NavigationState()).mode == LayoutMode.TWO_PANE)
    check(buildAdaptiveLayout(WindowSize(899, 800), NavigationState()).mode == LayoutMode.SINGLE_PANE)
    check(isTwoPaneWidth(900) && !isTwoPaneWidth(899))
    check(!isCompactWidth(720) && isCompactWidth(719))

    val wide = buildAdaptiveLayout(WindowSize(1280, 860), NavigationState(selectedConversationId = "c1"))
    check(wide.showConversationList && wide.showConversation)
    check(!wide.showBackToConversationList)
    check(wide.selectedConversationId == "c1")
    check(wide.listPaneWidthDp == 320)
    check(buildAdaptiveLayout(WindowSize(1440, 900), NavigationState()).listPaneWidthDp == 360)
    check(buildAdaptiveLayout(WindowSize(1000, 800), NavigationState()).listPaneWidthDp == 288)

    val narrowList = buildAdaptiveLayout(WindowSize(390, 844), NavigationState())
    check(narrowList.mode == LayoutMode.SINGLE_PANE)
    check(narrowList.singlePanePage == SinglePanePage.CONVERSATION_LIST)
    check(narrowList.showConversationList && !narrowList.showConversation)
    check(!narrowList.showBackToConversationList)
    check(narrowList.compact)

    val narrowChat = buildAdaptiveLayout(WindowSize(390, 844), NavigationState(selectedConversationId = "c1"))
    check(narrowChat.singlePanePage == SinglePanePage.CONVERSATION)
    check(narrowChat.showConversation && !narrowChat.showConversationList)
    check(narrowChat.showBackToConversationList)

    val blank = buildAdaptiveLayout(WindowSize(390, 844), NavigationState(selectedConversationId = "   "))
    check(blank.selectedConversationId == null)
    check(blank.singlePanePage == SinglePanePage.CONVERSATION_LIST)
    check(!blank.navigationFallback) { "空白选择按未选择处理，不算非法导航" }

    val unknown = buildAdaptiveLayout(
        WindowSize(390, 844),
        NavigationState(selectedConversationId = "gone", knownConversationIds = setOf("c1", "c2")),
    )
    check(unknown.navigationFallback)
    check(unknown.selectedConversationId == null)
    check(unknown.singlePanePage == SinglePanePage.CONVERSATION_LIST)

    val known = buildAdaptiveLayout(
        WindowSize(390, 844),
        NavigationState(selectedConversationId = " c2 ", knownConversationIds = setOf("c1", "c2")),
    )
    check(!known.navigationFallback && known.selectedConversationId == "c2")

    val resized = buildAdaptiveLayout(WindowSize(600, 844), NavigationState(selectedConversationId = "c1"))
    check(resized.selectedConversationId == "c1") { "尺寸变化不得丢失当前选择" }
    check(resized.mode == LayoutMode.SINGLE_PANE)
    check(resized.singlePanePage == SinglePanePage.CONVERSATION)

    val overlay = buildAdaptiveLayout(WindowSize(1280, 860), NavigationState("c1", detailOverlayOpen = true))
    check(overlay == buildAdaptiveLayout(WindowSize(1280, 860), NavigationState("c1"))) {
        "浮层打开不得改变面板结构"
    }

    val negative = buildAdaptiveLayout(WindowSize(-100, -100), NavigationState())
    check(negative.mode == LayoutMode.SINGLE_PANE && negative.compact)

    val preview = samplePreviewState()
    val previewLayout = preview.layoutModel(1280, 860)
    check(previewLayout.twoPane && previewLayout.selectedConversationId == "lin")
    val staleContact = preview.copy(selectedContactId = "removed")
    check(staleContact.layoutModel(1280, 860).navigationFallback)
    check(staleContact.layoutModel(1280, 860).selectedConversationId == null)
    println("PASS DS-09 adaptive layout boundaries, navigation fallback and resize keeping selection")
}

private fun checkDraftSendPresentation() {
    val queued = presentDraft(DraftEvidence(draftId = "d1", queuedLocally = true))
    check(queued.phase == DraftPhase.QUEUED)
    check(queued.canCancel && !queued.canRetry)
    check(!queued.autoResendAllowed)

    val accepted = presentDraft(DraftEvidence("d1", queuedLocally = true, nodeAcceptedReceiptId = "receipt"))
    check(accepted.phase == DraftPhase.ACCEPTED)
    check(accepted.label == "已接受")
    check(accepted.label != "已送达") { "节点接受不得写成已送达" }
    check(!accepted.canRetry && !accepted.canCancel)

    val delivered = presentDraft(
        DraftEvidence("d1", nodeAcceptedReceiptId = "receipt", deliveredReceiptId = "delivered"),
    )
    check(delivered.phase == DraftPhase.DELIVERED)
    check(delivered.actions.isEmpty())

    val unknownResult = presentDraft(DraftEvidence("d1", queuedLocally = true, acceptanceUnknown = true))
    check(unknownResult.phase == DraftPhase.ACCEPTANCE_UNKNOWN)
    check(unknownResult.actions == listOf(DraftAction.RECONCILE))
    check(!unknownResult.canRetry && !unknownResult.autoResendAllowed) { "结果未知不得自动重发" }
    check(unknownResult.blocksNewSend())

    val reconciled = presentDraft(
        DraftEvidence(
            draftId = "d1",
            queuedLocally = true,
            acceptanceUnknown = true,
            nodeAcceptedReceiptId = "receipt",
        ),
    )
    check(reconciled.phase == DraftPhase.ACCEPTED) { "明确回执优先于待对账标记" }

    val retryable = presentDraft(
        DraftEvidence(
            draftId = "d1",
            queuedLocally = true,
            failure = CoreFailure("NETWORK_UNAVAILABLE", retryable = true, retryAfterMillis = 1000, operationId = "op-1"),
        ),
    )
    check(retryable.phase == DraftPhase.FAILED_RETRYABLE)
    check(retryable.canRetry && retryable.canCancel)
    check(retryable.diagnosticId == "op-1")

    val blocked = presentDraft(
        DraftEvidence(
            draftId = "d1",
            queuedLocally = true,
            failure = CoreFailure("PERMISSION_DENIED", retryable = false, retryAfterMillis = null, operationId = "op-2"),
        ),
    )
    check(blocked.phase == DraftPhase.FAILED_BLOCKED)
    check(!blocked.canRetry) { "无权限或不可重试的失败不得显示重试" }
    check(blocked.diagnosticId == "op-2")

    val unknownCode = presentDraft(
        DraftEvidence(
            draftId = "d1",
            failure = CoreFailure("SOMETHING_NEW", retryable = false, retryAfterMillis = null, operationId = "op-3"),
        ),
    )
    check(unknownCode.phase == DraftPhase.FAILED_BLOCKED)
    check(unknownCode.label == "操作未完成") { "未知错误码使用安全通用文案" }
    check(unknownCode.detail.isNotBlank())

    val unsafeDiagnostic = presentDraft(
        DraftEvidence(
            draftId = "d1",
            failure = CoreFailure("X", retryable = false, retryAfterMillis = null, operationId = "bad id <script>"),
        ),
    )
    check(unsafeDiagnostic.diagnosticId == null) { "诊断标识只保留稳定且无秘密的值" }

    val retrying = DraftInteraction(
        DraftEvidence(
            draftId = "d1",
            queuedLocally = true,
            failure = CoreFailure("NETWORK_UNAVAILABLE", retryable = true, retryAfterMillis = null, operationId = "op-1"),
        ),
    ).beginRetry()
    check(retrying.retryInFlight && retrying.ui.label == "正在重试")
    check(retrying.ui.actions.isEmpty())
    check(retrying.beginRetry() == retrying) { "重试中重复点击不产生第二次请求" }
    val afterRetry = retrying.finishRetry(DraftEvidence("d1", queuedLocally = true))
    check(!afterRetry.retryInFlight && afterRetry.ui.phase == DraftPhase.QUEUED)

    val cancelPending = presentDraft(DraftEvidence("d1", queuedLocally = true, cancelRequested = true))
    check(cancelPending.phase == DraftPhase.CANCEL_PENDING)
    check(cancelPending.detail.contains("不保证撤回")) { "取消文案必须说明不保证撤回在途消息" }

    val cancelLate = presentDraft(
        DraftEvidence("d1", queuedLocally = true, cancelRequested = true, nodeAcceptedReceiptId = "receipt"),
    )
    check(cancelLate.phase == DraftPhase.CANCEL_TOO_LATE)
    check(cancelLate.label.contains("取消未生效"))

    val cancelOnDelivered = presentDraft(DraftEvidence("d1", deliveredReceiptId = "delivered", cancelRequested = true))
    check(cancelOnDelivered.phase == DraftPhase.DELIVERED)
    check(cancelOnDelivered.detail.contains("取消")) { "已送达仍要说明取消不再生效" }

    val cancelBlocked = DraftInteraction(DraftEvidence("d1", nodeAcceptedReceiptId = "receipt")).beginCancel()
    check(!cancelBlocked.cancelInFlight) { "已接受的消息不提供取消动作" }

    val sample = samplePreviewState().draftSample
    check(sample.ui.phase == DraftPhase.QUEUED)
    check(sample.next().ui.phase == DraftPhase.ACCEPTED)
    check(samplePreviewState().advanceDraftSample().draftSample.step == 1)
    println("PASS DS-10 draft states: evidence-first labels, unknown reconcile-only, retry/cancel merging")
}

private fun checkPagingPresentation() {
    val loading = ConversationMessagesDisplayState(conversationId = "c1", loading = true).olderPagingPresentation()
    check(loading.hint == PagingHint.Loading)
    check(!loading.actionEnabled) { "加载中不得再提供可点动作" }
    check(!loading.retryAllowed())

    val loadingMore = ConversationMessagesDisplayState(
        conversationId = "c1",
        messages = listOf(msg("m1", "c1")),
        loading = true,
    ).olderPagingPresentation()
    check(loadingMore.label == "正在加载更早的消息…")

    val failed = ConversationMessagesDisplayState(
        conversationId = "c1",
        messages = listOf(msg("m1", "c1")),
        error = "网络不可用",
    ).olderPagingPresentation()
    check(failed.hint == PagingHint.Failed("网络不可用"))
    check(failed.actionEnabled && failed.retryAllowed())
    check(failed.announcement != null) { "失败提示需要可播报文本，不能只靠颜色" }

    val more = ConversationMessagesDisplayState(
        conversationId = "c1",
        messages = listOf(msg("m1", "c1")),
        nextCursor = "older-1",
    ).olderPagingPresentation()
    check(more.hint == PagingHint.LoadMore && more.actionEnabled)

    val noMore = ConversationMessagesDisplayState(
        conversationId = "c1",
        messages = listOf(msg("m1", "c1")),
        nextCursor = null,
    ).olderPagingPresentation()
    check(noMore.hint == PagingHint.NoMore && !noMore.actionEnabled)

    val emptyMessages = ConversationMessagesDisplayState().olderPagingPresentation()
    check(emptyMessages.hint == PagingHint.Hidden && emptyMessages.label == null)

    val listLoading = ConversationListDisplayState(loading = true).morePagingPresentation()
    check(listLoading.hint == PagingHint.Loading && !listLoading.actionEnabled)
    val listMore = ConversationListDisplayState(
        conversations = listOf(conv("c1", "林然")),
        nextCursor = "next",
    ).morePagingPresentation()
    check(listMore.hint == PagingHint.LoadMore && listMore.actionEnabled)
    check(ConversationListDisplayState().morePagingPresentation().hint == PagingHint.Hidden)

    var sample = samplePreviewPaging()
    val seedCount = sample.display.conversation.messages.size
    check(seedCount == 3)
    check(sample.pagingPresentation().hint == PagingHint.LoadMore)

    val inFlight = sample.copy(
        display = sample.display.copy(conversation = sample.display.conversation.copy(loading = true)),
    )
    check(inFlight.nextSampleStep() == inFlight) { "加载中重复点击不得产生第二次请求" }

    sample = sample.nextSampleStep()
    check(sample.display.conversation.messages.size == seedCount + 2)
    check(sample.pagingPresentation().hint == PagingHint.LoadMore)

    sample = sample.nextSampleStep()
    check(sample.pagingPresentation().hint is PagingHint.Failed) { "第 2 次样例加载演示失败提示" }
    check(sample.display.conversation.messages.size == seedCount + 2) { "失败不得丢弃已加载结果" }

    sample = sample.nextSampleStep()
    check(sample.display.conversation.messages.size == seedCount + 4)
    check(sample.display.conversation.error == null)
    check(sample.pagingPresentation().hint == PagingHint.NoMore)

    val ended = samplePreviewState().copy(paging = sample)
    check(ended.advancePagingSample().paging.display.conversation.messages.size == seedCount) {
        "到底后再次点击重置样例"
    }
    println("PASS DS-11 paging presentations and preview paging sample through the display merger")
}

private fun checkMarkdown() {
    val fence = "\u0060\u0060\u0060"
    val inlineCode = "\u0060"
    val doc = renderMarkdown(
        "# 标题\n" +
            "普通 **粗体** 与 *斜体* 和 " + inlineCode + "代码" + inlineCode + "。\n" +
            "- 列表一\n" +
            "- 列表二\n" +
            "\n" +
            "> 引用一行\n" +
            "\n" +
            "---\n" +
            "\n" +
            fence + "kotlin\n" +
            "val x = 1\n" +
            fence,
    )
    check(!doc.isEmpty)
    check(doc.blocks.first() is MarkdownBlock.Heading)
    check((doc.blocks.first() as MarkdownBlock.Heading).level == 1)
    check(doc.blocks.any { it is MarkdownBlock.BulletList }) { "无序列表应解析为 BulletList" }
    check(doc.blocks.any { it is MarkdownBlock.Quote })
    check(doc.blocks.any { it is MarkdownBlock.Rule })
    val code = doc.blocks.filterIsInstance<MarkdownBlock.CodeBlock>().single()
    check(code.language == "kotlin")
    check(code.code == "val x = 1") { "代码块内容必须原样保留" }

    val paragraph = doc.blocks.filterIsInstance<MarkdownBlock.Paragraph>().first()
    val spans = paragraph.lines.flatMap { it }
    check(spans.any { it is MarkdownInline.Strong }) { "粗体应解析为 Strong" }
    check(spans.any { it is MarkdownInline.Emphasis })
    check(spans.any { it is MarkdownInline.Code })
    check(doc.plainText.contains("粗体") && doc.plainText.contains("• 列表一"))

    // 不解析 HTML：脚本标签只作为文字，模型里没有 HTML 节点
    val html = renderMarkdown("<script>alert(1)</script> 正常文字")
    val htmlText = html.blocks.filterIsInstance<MarkdownBlock.Paragraph>()
        .flatMap { it.lines }.flatMap { it }
        .filterIsInstance<MarkdownInline.Text>()
        .joinToString("") { it.text }
    check(htmlText.contains("<script>")) { "尖括号内容按普通文字保留，不得执行或吞掉" }
    check(html.plainText.contains("alert(1)"))

    // 链接协议白名单：https 保留为链接模型，javascript: 降级为不可点击
    val linkDoc = renderMarkdown("[课程页](https://example.edu/a) 和 [危险](javascript:alert(1))")
    val linkSpans = linkDoc.blocks.filterIsInstance<MarkdownBlock.Paragraph>()
        .flatMap { it.lines }.flatMap { it }
    val link = linkSpans.filterIsInstance<MarkdownInline.Link>().single()
    check(link.url == "https://example.edu/a")
    val blocked = linkSpans.filterIsInstance<MarkdownInline.BlockedLink>().single()
    check(blocked.reason.contains("协议")) { "非白名单协议必须给出停用原因" }

    // 代码围栏内的 Markdown 标记不得被解析
    val fenced = renderMarkdown(fence + "\n**不应加粗**\n" + fence)
    val fencedCode = fenced.blocks.filterIsInstance<MarkdownBlock.CodeBlock>().single()
    check(fencedCode.code.contains("**不应加粗**"))
    check(fencedCode.code.isNotBlank())

    // 转义与闭合缺失都按普通文字处理，不吞内容
    val escaped = renderMarkdown("\\*不是斜体\\*")
    check(escaped.plainText == "*不是斜体*") { "反斜杠转义必须原样显示星号" }
    val unclosed = renderMarkdown("**没有闭合")
    check(unclosed.plainText.contains("**没有闭合"))

    // 超长输入有界截断并标记 truncated
    val long = renderMarkdown("字".repeat(MarkdownLimits.MAX_INPUT_CHARS + 100))
    check(long.truncated) { "超过输入上限必须标记截断" }
    check(long.plainText.length <= MarkdownLimits.MAX_INPUT_CHARS)

    // 空内容回退，保证界面永远有可见内容
    check(markdownPlainTextOrFallback("   ").plainText == "（空消息）")

    // 只有文本消息进入 Markdown；附件/扩展继续走既有占位与回退
    check(renderMarkdownMessage(MessageContent.Text("**粗**")) != null)
    check(renderMarkdownMessage(MessageContent.Attachment(AttachmentId("a1"), null)) == null)

    // 输入：包裹选区、空选区插入占位并选中、链接模板、越界选区夹取
    val bold = MarkdownDraft("你好世界", 0, 2).applyMarkdownCommand(MarkdownCommand.BOLD)
    check(bold.text == "**你好**世界")
    check(bold.selectionStart == 6 && bold.selectionEnd == 6)
    val empty = MarkdownDraft("abc", 1, 1).applyMarkdownCommand(MarkdownCommand.CODE)
    check(empty.text == "a" + inlineCode + "代码" + inlineCode + "bc")
    check(empty.selectionEnd - empty.selectionStart == 2) { "空选区应选中占位文字，便于直接改写" }
    val linked = MarkdownDraft("看这里", 0, 3).applyMarkdownCommand(MarkdownCommand.LINK)
    check(linked.text == "[看这里](https://)")
    val clamped = MarkdownDraft("abc", 99, 99).applyMarkdownCommand(MarkdownCommand.ITALIC)
    check(clamped.text == "abc*斜体文字*") { "越界选区必须夹到文本末尾" }
    println("PASS DS-12 markdown: safe subset parsing, no HTML, link allowlist, bounded input, input commands")
}

private fun conv(id: String, title: String): ConversationSummaryView =
    ConversationSummaryView(id = id, title = title, preview = "preview-$id")

private fun msg(id: String, conversationId: String): MessageDisplayView =
    MessageDisplayView(
        id = id,
        conversationId = conversationId,
        author = "author-$id",
        body = "body-$id",
        mine = false,
    )
