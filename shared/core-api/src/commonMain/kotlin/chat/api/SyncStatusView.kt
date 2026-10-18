package chat.api

/** 常驻同步状态快照（SYNC-002/006 运行时）；只读，不含密钥。 */
data class SyncStatusView(
    /** idle / pulling / backoff / stopped */
    val state: String,
    val appliedTotal: Long,
    val lastErrorCode: String?,
    val nextAttemptAtMillis: Long,
    val resyncRequired: Boolean,
    val rounds: Long,
    val stoppedReason: String?,
)

/**
 * 常驻同步服务（界面用）。
 *
 * 诚实语义：start 只启动本地循环；它不保证“对方在线”或“消息已投递”，
 * 状态里的 applied_total 是“本机已验签并落库的条目数”。
 */
interface SyncService {
    suspend fun start(): SyncStatusView

    suspend fun stop(): SyncStatusView

    suspend fun status(): SyncStatusView

    suspend fun shutdown()
}
