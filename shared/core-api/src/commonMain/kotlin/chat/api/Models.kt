package chat.api

import kotlin.jvm.JvmInline

@JvmInline value class AccountId(val value: String)
@JvmInline value class DeviceId(val value: String)
@JvmInline value class ConversationId(val value: String)
@JvmInline value class MessageId(val value: String)
@JvmInline value class DraftId(val value: String)
@JvmInline value class AttachmentId(val value: String)
@JvmInline value class Cursor(val value: String)
enum class SendState { QUEUED, ACCEPTED, DELIVERED, FAILED }
enum class GroupRole { OWNER, ADMIN, MEMBER }
data class Page<T>(val items: List<T>, val nextCursor: Cursor?)
sealed interface MessageContent {
    data class Text(val text: String): MessageContent
    data class Attachment(val id: AttachmentId, val caption: String?): MessageContent
    data class Extension(val namespace: String, val schemaVersion: Int, val payload: ByteArray, val fallbackText: String): MessageContent
}
data class SendRequest(val conversationId: ConversationId, val clientOperationId: String, val content: MessageContent, val replyTo: MessageId? = null)
data class SendTicket(val draftId: DraftId, val state: SendState)
data class MessageView(val id: MessageId, val conversationId: ConversationId, val author: AccountId, val authorDevice: DeviceId, val groupSequence: Long?, val content: MessageContent, val sendState: SendState, val createdAtMillis: Long)
data class CoreFailure(val code: String, val retryable: Boolean, val retryAfterMillis: Long?, val operationId: String?)
// ID 的构造校验、ByteArray 内容相等/防御复制、消息来源模型见函数计划；当前 DTO 不是线协议。
