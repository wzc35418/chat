package chat.api

interface ConversationService {
    suspend fun openDirect(accountId: AccountId): ConversationId
    suspend fun listMessages(conversationId: ConversationId, before: Cursor?, limit: Int): Page<MessageView>
    suspend fun send(request: SendRequest): SendTicket
    suspend fun retry(draftId: DraftId): SendTicket
    suspend fun cancelQueued(draftId: DraftId)
}
// 其余服务按 docs/design/CORE_INTERFACES.md 和 docs/implementation/core.md 扩展。
