package chat.presentation.display

/**
 * 分页提示的纯展示映射（PRESENTATION-002 / PRESENTATION-003 的展示部分）。
 *
 * 只把 [ConversationMessagesDisplayState] / [ConversationListDisplayState] 的查询结果
 * 映射成可读提示、可用动作与语义播报文本；不改写合并结果、不推断真实 SendState，
 * 也不因为提示而自动重复请求（加载中不提供可点动作）。
 */
sealed interface PagingHint {
    /** 不需要任何提示（例如空会话的空态由页面自己表达）。 */
    data object Hidden : PagingHint

    data object Loading : PagingHint

    data object LoadMore : PagingHint

    data object NoMore : PagingHint

    data class Failed(val message: String) : PagingHint
}

data class PagingPresentation(
    val hint: PagingHint,
    /** 用户可见的短文案；为 null 表示不显示这一行。 */
    val label: String?,
    /** 可点按钮文案；为 null 表示当前没有可用动作。 */
    val actionLabel: String?,
    /** 语义播报文本（live region），与可见文案一致，不依赖颜色表达状态。 */
    val announcement: String?,
) {
    val actionEnabled: Boolean get() = actionLabel != null
}

fun ConversationMessagesDisplayState.olderPagingPresentation(): PagingPresentation = when {
    loading -> PagingPresentation(
        hint = PagingHint.Loading,
        label = if (messages.isEmpty()) "正在加载消息…" else "正在加载更早的消息…",
        actionLabel = null,
        announcement = if (messages.isEmpty()) "正在加载消息" else "正在加载更早的消息",
    )

    error != null -> PagingPresentation(
        hint = PagingHint.Failed(error),
        label = "更早的消息没有加载成功：$error",
        actionLabel = "重试",
        announcement = "更早的消息加载失败，可以重试",
    )

    messages.isEmpty() -> PagingPresentation(
        hint = PagingHint.Hidden,
        label = null,
        actionLabel = null,
        announcement = null,
    )

    nextCursor != null -> PagingPresentation(
        hint = PagingHint.LoadMore,
        label = "已加载 ${messages.size} 条消息，还有更早的消息",
        actionLabel = "加载更早的消息",
        announcement = "还有更早的消息可以加载",
    )

    else -> PagingPresentation(
        hint = PagingHint.NoMore,
        label = "已加载 ${messages.size} 条消息，没有更早的消息了",
        actionLabel = null,
        announcement = "已经到达这条会话的开头",
    )
}

fun ConversationListDisplayState.morePagingPresentation(): PagingPresentation = when {
    loading -> PagingPresentation(
        hint = PagingHint.Loading,
        label = "正在加载会话列表…",
        actionLabel = null,
        announcement = "正在加载会话列表",
    )

    error != null -> PagingPresentation(
        hint = PagingHint.Failed(error),
        label = "会话列表没有加载成功：$error",
        actionLabel = "重试",
        announcement = "会话列表加载失败，可以重试",
    )

    nextCursor != null -> PagingPresentation(
        hint = PagingHint.LoadMore,
        label = "已加载 ${conversations.size} 个会话，还有更多",
        actionLabel = "加载更多会话",
        announcement = "还有更多会话可以加载",
    )

    conversations.isEmpty() -> PagingPresentation(
        hint = PagingHint.Hidden,
        label = null,
        actionLabel = null,
        announcement = null,
    )

    else -> PagingPresentation(
        hint = PagingHint.NoMore,
        label = "已经是全部会话",
        actionLabel = null,
        announcement = null,
    )
}

/**
 * 失败提示只有在请求已经停下（非加载中）时才提供重试，避免重复请求叠代。
 */
fun PagingPresentation.retryAllowed(): Boolean = hint is PagingHint.Failed && actionEnabled
