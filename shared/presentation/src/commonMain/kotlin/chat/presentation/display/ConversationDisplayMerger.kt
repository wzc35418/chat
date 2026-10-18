package chat.presentation.display

/**
 * 会话列表与消息分页的纯展示合并器。
 *
 * 该文件不依赖尚未冻结的 `ConversationService`，不写 SQLite/网络，也不推断真实
 * `SendState`。调用方只负责把正式查询结果转换成这里的显示模型和分页结果。
 */
data class ConversationSummaryView(
    val id: String,
    val title: String,
    val preview: String,
    /** null 表示底层没有 unread 计数；展示层不得伪造 0。 */
    val unreadCount: Int? = null,
)

data class MessageDisplayView(
    val id: String,
    val conversationId: String,
    val author: String,
    val body: String,
    val mine: Boolean,
)

data class DisplayPage<T>(
    val items: List<T>,
    val nextCursor: String?,
    val reset: Boolean = false,
    val prepend: Boolean = false,
)

sealed interface DisplayQueryIntent {
    data class LoadConversationList(
        val generation: Long,
        val cursor: String?,
    ) : DisplayQueryIntent

    data class LoadMessagePage(
        val conversationId: String,
        val generation: Long,
        val before: String?,
    ) : DisplayQueryIntent
}

data class ConversationListDisplayState(
    val conversations: List<ConversationSummaryView> = emptyList(),
    val nextCursor: String? = null,
    val loading: Boolean = false,
    val generation: Long = 0,
    val error: String? = null,
)

data class ConversationMessagesDisplayState(
    val conversationId: String? = null,
    val messages: List<MessageDisplayView> = emptyList(),
    val nextCursor: String? = null,
    val loading: Boolean = false,
    val generation: Long = 0,
    val error: String? = null,
)

data class ConversationDisplayState(
    val list: ConversationListDisplayState = ConversationListDisplayState(),
    val conversation: ConversationMessagesDisplayState = ConversationMessagesDisplayState(),
)

/**
 * `ignored` 表示结果因请求代次、会话或游标不匹配而被丢弃，
 * 用于测试迟到响应，不应作为用户可见错误。
 */
data class DisplayTransition(
    val state: ConversationDisplayState,
    val intent: DisplayQueryIntent? = null,
    val ignored: Boolean = false,
)

fun initialConversationDisplayState(): ConversationDisplayState = ConversationDisplayState()

fun ConversationDisplayState.refreshConversationList(): DisplayTransition {
    if (list.loading) return DisplayTransition(this)
    val generation = list.generation + 1
    val next = copy(
        list = list.copy(
            generation = generation,
            loading = true,
            error = null,
        ),
    )
    return DisplayTransition(next, DisplayQueryIntent.LoadConversationList(generation, null))
}

fun ConversationDisplayState.loadMoreConversations(): DisplayTransition {
    val cursor = list.nextCursor ?: return DisplayTransition(this)
    if (list.loading) return DisplayTransition(this)
    val next = copy(list = list.copy(loading = true, error = null))
    return DisplayTransition(next, DisplayQueryIntent.LoadConversationList(list.generation, cursor))
}

fun ConversationDisplayState.applyConversationListPage(
    request: DisplayQueryIntent.LoadConversationList,
    page: DisplayPage<ConversationSummaryView>,
): DisplayTransition {
    if (request.generation != list.generation) return DisplayTransition(this, ignored = true)
    val reset = page.reset || request.cursor == null
    if (!reset && request.cursor != list.nextCursor) return DisplayTransition(this, ignored = true)

    val conversations = if (reset) {
        dedupeConversations(page.items)
    } else {
        mergeConversations(list.conversations, page.items)
    }
    return DisplayTransition(
        copy(
            list = list.copy(
                conversations = conversations,
                nextCursor = page.nextCursor,
                loading = false,
                error = null,
            ),
        ),
    )
}

fun ConversationDisplayState.failConversationListQuery(
    request: DisplayQueryIntent.LoadConversationList,
    message: String,
): DisplayTransition {
    if (request.generation != list.generation) return DisplayTransition(this, ignored = true)
    return DisplayTransition(copy(list = list.copy(loading = false, error = message)))
}

fun ConversationDisplayState.selectConversation(conversationId: String): DisplayTransition {
    if (conversation.conversationId == conversationId) return DisplayTransition(this)
    val generation = conversation.generation + 1
    val next = copy(
        conversation = ConversationMessagesDisplayState(
            conversationId = conversationId,
            generation = generation,
            loading = true,
        ),
    )
    return DisplayTransition(
        next,
        DisplayQueryIntent.LoadMessagePage(conversationId, generation, null),
    )
}

fun ConversationDisplayState.refreshSelectedConversation(): DisplayTransition {
    val conversationId = conversation.conversationId ?: return DisplayTransition(this)
    val generation = conversation.generation + 1
    val next = copy(
        conversation = conversation.copy(
            generation = generation,
            messages = emptyList(),
            nextCursor = null,
            loading = true,
            error = null,
        ),
    )
    return DisplayTransition(
        next,
        DisplayQueryIntent.LoadMessagePage(conversationId, generation, null),
    )
}

fun ConversationDisplayState.loadOlderMessages(): DisplayTransition {
    val conversationId = conversation.conversationId ?: return DisplayTransition(this)
    val cursor = conversation.nextCursor ?: return DisplayTransition(this)
    if (conversation.loading) return DisplayTransition(this)
    val next = copy(conversation = conversation.copy(loading = true, error = null))
    return DisplayTransition(
        next,
        DisplayQueryIntent.LoadMessagePage(conversationId, conversation.generation, cursor),
    )
}

fun ConversationDisplayState.applyMessagePage(
    request: DisplayQueryIntent.LoadMessagePage,
    page: DisplayPage<MessageDisplayView>,
): DisplayTransition {
    val current = conversation
    if (request.conversationId != current.conversationId || request.generation != current.generation) {
        return DisplayTransition(this, ignored = true)
    }
    val reset = page.reset || request.before == null
    if (!reset && request.before != current.nextCursor) return DisplayTransition(this, ignored = true)

    val messages = if (reset) {
        dedupeMessages(page.items)
    } else {
        mergeMessages(current.messages, page.items, prepend = page.prepend)
    }
    return DisplayTransition(
        copy(
            conversation = current.copy(
                messages = messages,
                nextCursor = page.nextCursor,
                loading = false,
                error = null,
            ),
        ),
    )
}

fun ConversationDisplayState.failMessageQuery(
    request: DisplayQueryIntent.LoadMessagePage,
    message: String,
): DisplayTransition {
    val current = conversation
    if (request.conversationId != current.conversationId || request.generation != current.generation) {
        return DisplayTransition(this, ignored = true)
    }
    return DisplayTransition(
        copy(conversation = current.copy(loading = false, error = message)),
    )
}

private fun dedupeConversations(items: List<ConversationSummaryView>): List<ConversationSummaryView> {
    val merged = LinkedHashMap<String, ConversationSummaryView>()
    for (item in items) merged[item.id] = item
    return merged.values.toList()
}

private fun mergeConversations(
    existing: List<ConversationSummaryView>,
    incoming: List<ConversationSummaryView>,
): List<ConversationSummaryView> {
    val merged = LinkedHashMap<String, ConversationSummaryView>()
    for (item in existing) merged[item.id] = item
    for (item in incoming) merged[item.id] = item
    return merged.values.toList()
}

private fun dedupeMessages(items: List<MessageDisplayView>): List<MessageDisplayView> {
    val merged = LinkedHashMap<String, MessageDisplayView>()
    for (item in items) merged[item.id] = item
    return merged.values.toList()
}

private fun mergeMessages(
    existing: List<MessageDisplayView>,
    incoming: List<MessageDisplayView>,
    prepend: Boolean,
): List<MessageDisplayView> {
    val merged = LinkedHashMap<String, MessageDisplayView>()
    if (prepend) {
        for (item in incoming) merged[item.id] = item
        for (item in existing) merged.putIfAbsent(item.id, item)
    } else {
        for (item in existing) merged[item.id] = item
        for (item in incoming) merged[item.id] = item
    }
    return merged.values.toList()
}
