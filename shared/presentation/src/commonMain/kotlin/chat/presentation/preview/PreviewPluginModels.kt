package chat.presentation.preview

import chat.api.DispatchPlanView
import chat.api.PluginStateView

/** 插件面板状态（诚实：不加载/不执行插件代码）。 */
data class PreviewPluginState(
    val manifestJson: String = "",
    /** P4：已选插件 JAR 的路径（用户显式选择）。 */
    val jarPath: String = "",
    /** 结构校验结果（来自 Rust 侧纯字节解析）。 */
    val packageView: chat.api.PluginPackageView? = null,
    /** 用户是否已显式确认"无沙箱执行"的信任。 */
    val trusted: Boolean = false,
    /** 已加载（真正在 JVM 里跑）的插件 id。 */
    val loadedPlugins: List<String> = emptyList(),
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
fun PreviewPluginState.updateJarPath(path: String): PreviewPluginState = copy(
    jarPath = path,
    packageView = null,
    busy = false,
    notice = "已选择插件包（尚未校验）",
    lastErrorCode = null,
)

fun PreviewPluginState.withPackage(view: chat.api.PluginPackageView): PreviewPluginState = copy(
    packageView = view,
    busy = false,
    notice = "包结构校验通过：" + view.pluginId + " · 入口 " + view.entryFile + " · " + view.entryCount + " 条目",
    lastErrorCode = null,
)

fun PreviewPluginState.toggleTrusted(value: Boolean): PreviewPluginState = copy(
    trusted = value,
    notice = if (value) "已确认信任：插件将在同一 JVM 无沙箱运行" else "未确认信任，插件不会被加载",
)

fun PreviewPluginState.withLoadedPlugins(ids: List<String>): PreviewPluginState = copy(
    loadedPlugins = ids,
    busy = false,
    notice = if (ids.isEmpty()) "当前没有已加载的插件" else "已加载 " + ids.size + " 个插件（同 JVM 无沙箱）",
    lastErrorCode = null,
)
