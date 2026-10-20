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
/** 单轮拉取结果（SYNC 单轮 FFI）。 */
data class SyncOnceView(
    /** applied + skipped（本页） */
    val fetched: Long,
    val applied: Long,
    val skipped: Long,
    val nextCursor: Long,
    /** true 表示已追平（无需 resync 且未停止） */
    val settled: Boolean,
)

interface SyncService {
    suspend fun start(): SyncStatusView

    suspend fun stop(): SyncStatusView

    suspend fun status(): SyncStatusView

    /**
     * 只拉一轮并应用（不启动常驻循环）。
     *
     * 常驻循环运行中调用会被拒绝（`INVALID_STATE_TRANSITION`）。
     */
    suspend fun syncOnce(): SyncOnceView

    suspend fun shutdown()
}
