package chat.presentation.preview

import chat.api.DispatchPlanView
import chat.api.PluginStateView

/** 插件面板状态（诚实：不加载/不执行插件代码）。 */
data class PreviewPluginState(
    val manifestJson: String = "",
    val plugins: List<PluginStateView> = emptyList(),
    val lastPlan: DispatchPlanView? = null,
    val lastStorageValue: String? = null,
    val busy: Boolean = false,
    val notice: String = "",
    val lastErrorCode: String? = null,
) {
    fun beginBusy(): PreviewPluginState = copy(busy = true, notice = "", lastErrorCode = null)

    fun withError(code: String, message: String?): PreviewPluginState = copy(
        busy = false,
        notice = message ?: "插件操作失败",
        lastErrorCode = code,
    )

    companion object {
        val Empty = PreviewPluginState()
    }
}

fun PreviewPluginState.updateManifest(value: String): PreviewPluginState = copy(manifestJson = value, lastErrorCode = null)

fun PreviewPluginState.withPlugins(list: List<PluginStateView>): PreviewPluginState = copy(
    plugins = list,
    busy = false,
    notice = if (list.isEmpty()) "还没有安装任何插件" else "已列出 " + list.size + " 个插件",
    lastErrorCode = null,
)

fun PreviewPluginState.withInstalled(id: String): PreviewPluginState = copy(
    busy = false,
    notice = "插件 " + id + " 已安装并随库持久化（运行状态需重新启动）",
    lastErrorCode = null,
)

fun PreviewPluginState.withPlan(plan: DispatchPlanView): PreviewPluginState = copy(
    lastPlan = plan,
    busy = false,
    notice = "已生成派发计划（本层不执行插件代码）",
    lastErrorCode = null,
)

fun PreviewPluginState.withStateNotice(text: String): PreviewPluginState = copy(busy = false, notice = text, lastErrorCode = null)

fun PreviewPluginState.withStorageValue(value: String?): PreviewPluginState = copy(
    lastStorageValue = value ?: "（键不存在）",
    busy = false,
    notice = "已读取私有存储",
    lastErrorCode = null,
)
