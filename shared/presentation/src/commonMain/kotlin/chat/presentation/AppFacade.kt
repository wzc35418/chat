package chat.presentation

import chat.api.AttachmentService
import chat.api.GroupService
import chat.api.LocalStore
import chat.api.LocalStoreException
import chat.api.PluginService
import chat.api.RecoveryImportView
import chat.api.StoredMessage
import chat.presentation.display.AttachmentCard

/**
 * 应用门面（PRESENTATION 收敛的单一入口）。
 *
 * 设计原则：1) 不做业务判断（状态语义由 core 决定，这里只编排与映射）；
 * 2) 统一结果类型 UseCaseResult，错误码原样透出；3) 缺服务时显式失败（不 mock 成功）。
 */
class AppFacade(
    private val store: LocalStore,
    private val useCases: UseCases = UseCases(store),
    private val groups: GroupService? = null,
    private val attachments: AttachmentService? = null,
    private val plugins: PluginService? = null,
) {
    suspend fun bootstrap(limit: Int = 20): UseCaseResult<BootstrapView> = useCases.bootstrap(limit)

    suspend fun conversationList(cursor: String?, limit: Int = 20): UseCaseResult<BootstrapView> =
        useCases.loadConversationList(cursor, limit)

    /** 打开会话：消息页 + 每条消息的附件卡片（缺失只计数，不伪造）。 */
    suspend fun openConversation(
        conversationId: String,
        cursor: String? = null,
        limit: Int = 30,
    ): UseCaseResult<ConversationView> {
        val page = useCases.loadMessages(conversationId, cursor, limit)
        if (page is UseCaseResult.Failed) return page
        val ok = page as UseCaseResult.Ok
        val service = attachments
        val cards = if (service == null) {
            MessageCardsView(emptyMap(), 0, 0)
        } else {
            when (val loaded = useCases.loadMessageCards(conversationId, service, limit)) {
                is UseCaseResult.Failed -> MessageCardsView(emptyMap(), 0, 1)
                is UseCaseResult.Ok -> loaded.value
            }
        }
        return UseCaseResult.Ok(assembleConversationView(ok.value.messages, ok.value.nextCursor, cards))
    }

    suspend fun submitComposer(conversationId: String, operationId: String, body: String) =
        useCases.submitComposer(conversationId, operationId, body)

    suspend fun retryDraft(draftId: String) = useCases.retryDraft(draftId)

    suspend fun cancelDraft(draftId: String) = useCases.cancelDraft(draftId)

    suspend fun pollEvents(sinceSeq: Long, limit: Int = 64) = useCases.pollEvents(sinceSeq, limit)

    /** 群列表（缺服务时显式失败）。 */
    suspend fun groupList(): UseCaseResult<GroupListView> {
        val service = groups ?: return UseCaseResult.Failed("CORE_CLOSED", "群服务未接线")
        return useCases.loadGroupList(service)
    }

    /** 从节点下载附件（参数校验在用例层）。 */
    suspend fun downloadAttachment(
        attachmentId: String,
        targetPath: String,
        endpointUrl: String,
        nodeId: String,
        nodePublicKeyHex: String?,
        groupId: String,
        sourceSeq: Long,
    ): UseCaseResult<Long> {
        val service = attachments ?: return UseCaseResult.Failed("CORE_CLOSED", "附件服务未接线")
        return useCases.downloadAttachment(
            service, attachmentId, targetPath, endpointUrl, nodeId, nodePublicKeyHex, groupId, sourceSeq,
        )
    }

    /** 插件：结构校验（不执行插件代码）。 */
    suspend fun inspectPlugin(packageBytes: ByteArray): UseCaseResult<chat.api.PluginPackageView> {
        val service = plugins ?: return UseCaseResult.Failed("CORE_CLOSED", "插件服务未接线")
        return try {
            UseCaseResult.Ok(service.inspectPackage(packageBytes))
        } catch (error: LocalStoreException) {
            UseCaseResult.Failed(error.code, error.message)
        }
    }

    /** 身份恢复导入由身份面板独立持有，门面不代理（避免两个对象抢写锁）。 */
    suspend fun importRecovery(passphrase: String, bundle: ByteArray): UseCaseResult<RecoveryImportView> =
        UseCaseResult.Failed("CORE_CLOSED", "身份服务不在门面内（由身份面板独立持有）")
}

/** 会话视图：消息 + 附件卡片 + 缺失计数。 */
data class ConversationView(
    val messages: List<StoredMessage>,
    val nextCursor: String?,
    val cardsByMessage: Map<String, List<AttachmentCard>>,
    val messagesWithoutAttachments: Int,
    val messagesWithFailedAttachments: Int,
)

/**
 * 纯函数：把消息与附件卡片合并成视图（不依赖服务，便于单测）。
 * 语义：卡片表里没有该消息 → 计入 messagesWithoutAttachments（不当作"有附件但空"）。
 */
fun assembleConversationView(
    messages: List<StoredMessage>,
    nextCursor: String?,
    cards: MessageCardsView,
): ConversationView {
    var withoutAttachments = 0
    for (message in messages) {
        if (cards.cardsByMessage[message.messageId] == null) withoutAttachments += 1
    }
    return ConversationView(
        messages = messages,
        nextCursor = nextCursor,
        cardsByMessage = cards.cardsByMessage,
        messagesWithoutAttachments = withoutAttachments,
        messagesWithFailedAttachments = cards.failedMessages,
    )
}
