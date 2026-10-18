package chat.api

/**
 * 本地正式存储的 Kotlin 视图（P2-01 第一条正式接线）。
 *
 * 这些类型只镜像 formal store 通过 UniFFI 暴露的事实：草稿意图、已验证消息与会话分页。
 * 它们不是线协议 DTO，也不代表节点已接受或对端已送达；状态语义由展示层映射。
 */

/** 草稿意图。state 取值来自存储层："queued"（仅本端意图）/"prepared"（已生成提交尝试）。 */
data class StoredDraft(
    val draftId: String,
    val operationId: String,
    val conversationId: String,
    val body: String,
    val state: String,
    val submissionId: String?,
)

data class StoredConversation(
    val conversationId: String,
    val title: String,
    val preview: String,
    /** null 表示存储层没有 unread 计数（不得伪造成 0）。 */
    val unreadCount: Int?,
    val updatedAtMillis: Long,
)

data class StoredMessage(
    val messageId: String,
    val conversationId: String,
    val author: String,
    val body: String,
    val mine: Boolean,
    val createdAtMillis: Long,
)

data class StoredPage<T>(
    val items: List<T>,
    val nextCursor: String?,
)

/** 已通过加密校验的入站/出站消息写入请求（调用方必须已完成 MLS 校验）。 */
data class VerifiedMessageInput(
    val conversationId: String,
    val title: String,
    val messageId: String,
    val authorAccount: String,
    val authorDevice: String,
    val body: String,
    val mine: Boolean,
    val createdAtMillis: Long,
)

/** 本地存储错误：code 是稳定错误码（见 PROTOCOL-014 码表），message 只作展示。 */
class LocalStoreException(
    val code: String,
    override val message: String,
) : Exception(message)

/**
 * 本地存储服务。所有方法都是 suspend，由 bridge 切到 IO 调度并在关闭后拒绝调用。
 * 不提供“自动重发”“推断送达”之类的能力：发送语义仍属 sync 模块。
 */
interface LocalStore {
    suspend fun open(databasePath: String, masterKeyHex: String)

    suspend fun createDraft(conversationId: String, operationId: String, body: String): StoredDraft

    suspend fun draftStatus(draftId: String): StoredDraft?

    suspend fun applyVerifiedMessage(input: VerifiedMessageInput): StoredMessage

    suspend fun listConversations(cursor: String?, limit: Int): StoredPage<StoredConversation>

    suspend fun listMessages(conversationId: String, cursor: String?, limit: Int): StoredPage<StoredMessage>

    suspend fun close()
}
