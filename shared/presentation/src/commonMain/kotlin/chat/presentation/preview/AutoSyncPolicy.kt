package chat.presentation.preview

/**
 * 后台自动同步的策略与文案（纯函数，便于断言；不做 IO）。
 *
 * 语义边界：这里只决定「要不要重试 / 界面上怎么说」，不负责实际拉取——
 * 拉取由应用层定时器调用 `SyncService.syncOnce()`；失败不弹错误条（后台噪音），
 * 但必须留下可见状态，不能让用户以为一直在线。
 */
object AutoSyncPolicy {

    /** 后台拉取间隔（毫秒）：60 秒。 */
    const val INTERVAL_MILLIS: Long = 60_000L

    /** 连续失败到第几次开始显示离线提示（前两次视为抖动）。 */
    const val OFFLINE_AFTER_FAILURES: Int = 2

    /** 退避：失败次数越多间隔越长，避免离线时每 60 秒空转。 */
    fun nextDelayMillis(failures: Int): Long {
        val extraSteps = failures.coerceIn(0, 5)
        return INTERVAL_MILLIS * (1L shl extraSteps)
    }

    /** 是否已经看起来离线（>=2 次连续失败）。 */
    fun looksOffline(failures: Int): Boolean = failures >= OFFLINE_AFTER_FAILURES

    /** 一次成功拉取后给用户看的一句话（不说同步完成这种废话，要说新增多少）。 */
    fun describeSuccess(fetched: Int, applied: Int): String {
        return if (fetched <= 0 && applied <= 0) {
            "后台拉取：暂无新消息"
        } else {
            "后台拉取：节点返回 " + fetched + " 条，入库 " + applied + " 条"
        }
    }

    /** 连续失败时的提示（明确离线与会继续重试，不伪装成功）。 */
    fun describeFailure(failures: Int, reason: String?): String {
        val head = if (looksOffline(failures)) {
            "离线（连续 " + failures + " 次拉取失败），恢复后会自动重试"
        } else {
            "拉取失败第 " + failures + " 次（抖动，稍后自动重试）"
        }
        return if (reason.isNullOrBlank()) head else head + "：" + reason
    }
}
