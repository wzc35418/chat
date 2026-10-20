package chat.plugin

interface Registration { fun dispose() }

interface DesktopPlugin {
    val id: String
    fun start(context: PluginContext)
    fun stop()
}

interface PluginContext {
    // SDK 约定：handler 的产物只能是**待用户确认的草稿文本**，宿主不会替插件发送消息。
    fun registerCommand(name: String, action: () -> String): Registration

    fun registerSelectedTextAction(name: String, action: (String) -> String): Registration

    /**
     * 声明式槽位 `composer.actions`：在草稿编辑区旁加一个按钮。
     *
     * - `action` 返回的文本会被**追加/填入草稿**（用户仍可编辑，是否发送由用户决定）；
     * - 返回超过 8 KiB 会被宿主截断并显式报错（不会静默丢）；
     * - 插件**拿不到布局控制权**，宿主决定按钮位置与样式。
     *
     * 默认实现返回空注册：老插件不实现也能照常加载（向后兼容）。
     */
    /**
     * 声明式槽位 `status_bar.items`：在状态栏放一段短文本。
     *
     * **只能读**宿主给的这几个字段（写死在 SDK 里，宿主不会给更多）：
     * 群数、当前群 epoch、同步状态字符串、已应用条数。
     * 文本上限 120 字符（超长由宿主截断）。默认空实现，向后兼容。
     */
    fun registerStatusBarItem(
        name: String,
        render: (PluginStatusFacts) -> String,
    ): Registration = object : Registration {
        override fun dispose() = Unit
    }

    fun registerComposerAction(name: String, description: String, action: () -> String): Registration =
        object : Registration {
            override fun dispose() = Unit
        }
}

/**
 * 状态栏可读事实（**这是插件能看到的全部数据面**）。
 *
 * 刻意不含：群名、成员、消息正文、账户 ID、节点地址、任何密钥材料。
 */
data class PluginStatusFacts(
    val groupCount: Int,
    val currentEpoch: Int,
    val syncState: String,
    val appliedTotal: Long,
)
