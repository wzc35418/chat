package chat.presentation.display

/**
 * PRESENTATION-011 build_adaptive_layout 的纯布局决策。
 *
 * 只根据可用窗口尺寸和导航选择决定结构：宽屏同时显示会话列表与会话，
 * 窄屏二选一。本文件不持有 Compose 状态、不触发查询、不关闭 core，
 * 布局变化因此不会重启核心或重发消息。
 */

/** 900 dp 及以上使用双栏。与预览既有阈值保持一致。 */
const val TWO_PANE_MIN_WIDTH_DP: Int = 900

/** 低于 720 dp 视为最紧凑宽度，用于文案换行与按钮宽度收窄。 */
const val COMPACT_MAX_WIDTH_DP: Int = 720

/** 可用窗口尺寸，单位 dp；负值按 0 处理，避免非法测量值影响布局选择。 */
data class WindowSize(
    val widthDp: Int,
    val heightDp: Int,
) {
    val safeWidthDp: Int get() = widthDp.coerceAtLeast(0)

    val safeHeightDp: Int get() = heightDp.coerceAtLeast(0)
}

/**
 * 导航选择。
 *
 * [knownConversationIds] 为当前页面已有的稳定会话 ID 集合；传入后不在集合中的
 * 选择按非法导航处理并回退到会话列表。为 null 时不额外校验（列表可能尚未加载）。
 * [detailOverlayOpen] 只表示是否有覆盖在页面之上的浮层，不改变面板结构。
 */
data class NavigationState(
    val selectedConversationId: String? = null,
    val knownConversationIds: Set<String>? = null,
    val detailOverlayOpen: Boolean = false,
)

enum class LayoutMode { TWO_PANE, SINGLE_PANE }

/** 窄屏当前显示的页面。宽屏下该字段只反映选中状态，两个面板都可见。 */
enum class SinglePanePage { CONVERSATION_LIST, CONVERSATION }

data class LayoutModel(
    val mode: LayoutMode,
    val selectedConversationId: String?,
    val singlePanePage: SinglePanePage,
    val showConversationList: Boolean,
    val showConversation: Boolean,
    val showBackToConversationList: Boolean,
    val listPaneWidthDp: Int,
    val compact: Boolean,
    /** true 表示传入的选中会话无效（空白或不在已知集合内），已回退到会话列表。 */
    val navigationFallback: Boolean,
) {
    val twoPane: Boolean get() = mode == LayoutMode.TWO_PANE
}

fun buildAdaptiveLayout(
    window: WindowSize,
    selection: NavigationState = NavigationState(),
): LayoutModel {
    val width = window.safeWidthDp
    val rawId = selection.selectedConversationId?.trim()?.takeIf { it.isNotEmpty() }
    val known = selection.knownConversationIds
    val valid = rawId != null && (known == null || rawId in known)
    val fallback = rawId != null && !valid
    val selectedId = if (valid) rawId else null
    val compact = width < COMPACT_MAX_WIDTH_DP

    if (width >= TWO_PANE_MIN_WIDTH_DP) {
        return LayoutModel(
            mode = LayoutMode.TWO_PANE,
            selectedConversationId = selectedId,
            singlePanePage = if (selectedId == null) {
                SinglePanePage.CONVERSATION_LIST
            } else {
                SinglePanePage.CONVERSATION
            },
            showConversationList = true,
            showConversation = true,
            showBackToConversationList = false,
            listPaneWidthDp = listPaneWidthFor(width),
            compact = compact,
            navigationFallback = fallback,
        )
    }

    val showConversation = selectedId != null
    return LayoutModel(
        mode = LayoutMode.SINGLE_PANE,
        selectedConversationId = selectedId,
        singlePanePage = if (showConversation) {
            SinglePanePage.CONVERSATION
        } else {
            SinglePanePage.CONVERSATION_LIST
        },
        showConversationList = !showConversation,
        showConversation = showConversation,
        showBackToConversationList = showConversation,
        listPaneWidthDp = width,
        compact = compact,
        navigationFallback = fallback,
    )
}

fun isTwoPaneWidth(widthDp: Int): Boolean = widthDp.coerceAtLeast(0) >= TWO_PANE_MIN_WIDTH_DP

fun isCompactWidth(widthDp: Int): Boolean = widthDp.coerceAtLeast(0) < COMPACT_MAX_WIDTH_DP

/** 侧栏按可用宽度分档；窄屏直接占满。窄屏以外的值只影响侧栏宽度，不影响状态。 */
private fun listPaneWidthFor(widthDp: Int): Int = when {
    widthDp >= 1440 -> 360
    widthDp >= 1100 -> 320
    else -> 288
}
