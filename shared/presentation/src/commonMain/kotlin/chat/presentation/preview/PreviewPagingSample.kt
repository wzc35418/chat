package chat.presentation.preview

import chat.presentation.display.ConversationDisplayState
import chat.presentation.display.DisplayPage
import chat.presentation.display.DisplayQueryIntent
import chat.presentation.display.MessageDisplayView
import chat.presentation.display.PagingPresentation
import chat.presentation.display.applyMessagePage
import chat.presentation.display.failMessageQuery
import chat.presentation.display.initialConversationDisplayState
import chat.presentation.display.loadOlderMessages
import chat.presentation.display.olderPagingPresentation
import chat.presentation.display.selectConversation

/**
 * 预览用分页样例：真正调用 DS-08 的展示合并器跑一遍
 * “请求 → 成功 / 失败 → 重试 → 到底”，数据是固定样例。
 *
 * 它不查询任何服务，也不影响上方展示的 [PreviewMessage] 样例消息；
 * 只用来在界面里观察“加载中 / 失败重试 / 没有更多”三种提示。
 * 固定脚本：第 1 次加载成功、第 2 次失败、第 3 次成功并到达开头。
 */
const val PREVIEW_PAGING_SAMPLE_CONVERSATION = "lin"

data class PreviewPagingState(
    val display: ConversationDisplayState,
    val attempts: Int = 0,
)

fun samplePreviewPaging(conversationId: String = PREVIEW_PAGING_SAMPLE_CONVERSATION): PreviewPagingState {
    val selected = initialConversationDisplayState().selectConversation(conversationId)
    val request = selected.intent as DisplayQueryIntent.LoadMessagePage
    val applied = selected.state.applyMessagePage(
        request,
        DisplayPage(
            items = previewSeedPage(conversationId),
            nextCursor = "older-1",
            reset = true,
        ),
    )
    return PreviewPagingState(display = applied.state)
}

fun PreviewPagingState.pagingPresentation(): PagingPresentation =
    display.conversation.olderPagingPresentation()

/**
 * 推进一步样例脚本。加载中重复点击返回原状态，不产生第二次请求意图。
 */
fun PreviewPagingState.nextSampleStep(): PreviewPagingState {
    if (display.conversation.loading) return this
    val loading = display.loadOlderMessages()
    val request = loading.intent as? DisplayQueryIntent.LoadMessagePage
        ?: return this.copy(display = loading.state)

    val attempt = attempts + 1
    val transition = if (attempt == SECOND_ATTEMPT_FAILS) {
        loading.state.failMessageQuery(request, "网络不可用（样例）")
    } else {
        loading.state.applyMessagePage(request, previewOlderPage(request))
    }
    return PreviewPagingState(display = transition.state, attempts = attempt)
}

private const val SECOND_ATTEMPT_FAILS = 2

private fun previewSeedPage(conversationId: String): List<MessageDisplayView> = listOf(
    previewDisplayMessage(conversationId, "seed-1", "最早的样例消息"),
    previewDisplayMessage(conversationId, "seed-2", "随后的样例消息"),
    previewDisplayMessage(conversationId, "seed-3", "最新的样例消息"),
)

private fun previewOlderPage(
    request: DisplayQueryIntent.LoadMessagePage,
): DisplayPage<MessageDisplayView> = when (request.before) {
    "older-1" -> DisplayPage(
        items = listOf(
            previewDisplayMessage(request.conversationId, "older-1a", "更早的样例消息 A"),
            previewDisplayMessage(request.conversationId, "older-1b", "更早的样例消息 B"),
        ),
        nextCursor = "older-2",
        prepend = true,
    )

    else -> DisplayPage(
        items = listOf(
            previewDisplayMessage(request.conversationId, "older-2a", "最早的样例消息 A"),
            previewDisplayMessage(request.conversationId, "older-2b", "最早的样例消息 B"),
        ),
        nextCursor = null,
        prepend = true,
    )
}

private fun previewDisplayMessage(
    conversationId: String,
    id: String,
    body: String,
): MessageDisplayView = MessageDisplayView(
    id = "$conversationId-$id",
    conversationId = conversationId,
    author = "样例",
    body = body,
    mine = false,
)
