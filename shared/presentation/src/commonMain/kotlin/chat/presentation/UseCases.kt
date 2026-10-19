package chat.presentation

import chat.api.DraftCancelOutcome
import chat.api.LocalStore
import chat.api.LocalStoreException
import chat.api.SendStep
import chat.api.StoredConversation
import chat.api.StoredDraft
import chat.api.StoredMessage
import chat.api.StoredPage
import chat.presentation.display.DraftUiState
import chat.presentation.display.StoreEventUi
import chat.presentation.display.presentDraft
import chat.presentation.display.toCard
import chat.presentation.display.toDraftEvidence
import chat.presentation.display.toUi

/** 用例层结果：显式可恢复错误，不把失败当成功。 */
sealed interface UseCaseResult<out T> {
    data class Ok<T>(val value: T) : UseCaseResult<T>
    data class Failed(val code: String, val message: String?) : UseCaseResult<Nothing>
}

private inline fun <T> guard(block: () -> T): UseCaseResult<T> = try {
    UseCaseResult.Ok(block())
} catch (error: LocalStoreException) {
    UseCaseResult.Failed(error.code, error.message)
} catch (error: IllegalArgumentException) {
    UseCaseResult.Failed("INVALID_INPUT", error.message)
}

/** 群列表用例返回（含跳过计数，不静默丢）。 */
data class GroupListView(
    val groups: List<chat.api.GroupSummaryEntry>,
    val skipped: Int,
)

/** 消息附件卡片用例返回：每条消息一组卡片（只读投影）。 */
data class MessageCardsView(
    val cardsByMessage: Map<String, List<chat.presentation.display.AttachmentCard>>,
    /** 没有附件或读不到附件 id 的消息数（显式计数，便于界面提示）。 */
    val emptyMessages: Int,
    val failedMessages: Int,
)

/**
 * 应用用例层（PRESENTATION-001~008 的落地骨架）。
 *
 * 只做“调用 + 映射 + 错误归一”，不在这里发明业务状态：状态一律来自 core 的查询与事件。
 * 未接线的能力（例如真实发送需要节点与 MLS 群）把底层错误原样透出，不伪造成功。
 */
class UseCases(
    private val store: LocalStore,
) {
    /** PRESENTATION-001：启动时拉会话列表（不做乐观状态）。 */
    suspend fun bootstrap(limit: Int = 20): UseCaseResult<BootstrapView> = guard {
        val page: StoredPage<StoredConversation> = store.listConversations(null, limit)
        BootstrapView(conversations = page.items, nextCursor = page.nextCursor)
    }

    /** PRESENTATION-002：分页加载会话列表（游标由 core 给，不自造）。 */
    suspend fun loadConversationList(cursor: String?, limit: Int = 20): UseCaseResult<BootstrapView> = guard {
        val page = store.listConversations(cursor, limit)
        BootstrapView(conversations = page.items, nextCursor = page.nextCursor)
    }

    /** PRESENTATION-003：分页读消息。 */
    suspend fun loadMessages(
        conversationId: String,
        cursor: String?,
        limit: Int = 30,
    ): UseCaseResult<MessagePageView> = guard {
        val page = store.listMessages(conversationId, cursor, limit)
        MessagePageView(messages = page.items, nextCursor = page.nextCursor)
    }

    /** PRESENTATION-004 的准备段：把正文变成真实草稿（落库加密），返回可展示状态。 */
    suspend fun submitComposer(
        conversationId: String,
        operationId: String,
        body: String,
    ): UseCaseResult<DraftUiState> = guard {
        require(body.isNotBlank()) { "正文不能为空" }
        require(body.length <= 8 * 1024) { "正文超过 8 KiB 上限" }
        val draft: StoredDraft = store.createDraft(conversationId, operationId, body)
        presentDraft(draft.toDraftEvidence())
    }

    /** PRESENTATION-005：重试只走真实重试路径（accepted/unknown 由底层拒绝）。 */
    suspend fun retryDraft(draftId: String): UseCaseResult<SendStep> = guard {
        store.sendRetry(draftId)
    }

    /** PRESENTATION-006：取消三态原样返回，界面不得把 cancel_too_late 显示成已撤回。 */
    suspend fun cancelDraft(draftId: String): UseCaseResult<DraftCancelOutcome> = guard {
        store.cancelDraft(draftId)
    }

    /** PRESENTATION-009：群列表（只读投影，skipped 原样上报）。 */
    suspend fun loadGroupList(
        groups: chat.api.GroupService,
    ): UseCaseResult<GroupListView> = guard {
        val list = groups.listGroups()
        GroupListView(groups = list.groups, skipped = list.skipped)
    }

    /** PRESENTATION-010：按消息批量取附件卡片（不解密正文，缺失跳过并计数）。 */
    suspend fun loadMessageCards(
        conversationId: String,
        attachments: chat.api.AttachmentService,
        limit: Int = 20,
    ): UseCaseResult<MessageCardsView> = guard {
        val page = store.listMessages(conversationId, null, limit)
        val cards = LinkedHashMap<String, List<chat.presentation.display.AttachmentCard>>()
        var empty = 0
        var failed = 0
        for (message in page.items) {
            val ids = try {
                store.messageAttachmentIds(message.messageId)
            } catch (error: LocalStoreException) {
                failed += 1
                continue
            }
            if (ids.isEmpty()) {
                empty += 1
                cards[message.messageId] = emptyList()
                continue
            }
            val records = try {
                attachments.records(ids)
            } catch (error: LocalStoreException) {
                failed += 1
                continue
            }
            cards[message.messageId] = records.map { it.toCard() }
        }
        MessageCardsView(cardsByMessage = cards, emptyMessages = empty, failedMessages = failed)
    }

    /** PRESENTATION-011：从节点下载附件（返回写入字节数；失败原样透出稳定码）。 */
    suspend fun downloadAttachment(
        attachments: chat.api.AttachmentService,
        attachmentId: String,
        targetPath: String,
        endpointUrl: String,
        nodeId: String,
        nodePublicKeyHex: String?,
        groupId: String,
        sourceSeq: Long,
    ): UseCaseResult<Long> = guard {
        require(targetPath.isNotBlank()) { "下载目标路径不能为空" }
        require(endpointUrl.startsWith("http://") || endpointUrl.startsWith("https://")) {
            "节点地址必须是 http/https"
        }
        attachments.download(attachmentId, targetPath, endpointUrl, nodeId, nodePublicKeyHex, groupId, sourceSeq)
    }

    /** PRESENTATION-008：事件转界面提示；溢出必须整体重查（不是“丢几条”）。 */
    suspend fun pollEvents(sinceSeq: Long, limit: Int = 64): UseCaseResult<EventFeed> = guard {
        val batch = store.pollEvents(sinceSeq, limit)
        EventFeed(
            events = batch.events.map { it.toUi() },
            nextSeq = batch.nextSeq,
            resyncRequired = batch.resyncRequired,
        )
    }
}

data class BootstrapView(
    val conversations: List<StoredConversation>,
    val nextCursor: String?,
)

data class MessagePageView(
    val messages: List<StoredMessage>,
    val nextCursor: String?,
)

data class EventFeed(
    val events: List<StoreEventUi>,
    val nextSeq: Long,
    val resyncRequired: Boolean,
)
