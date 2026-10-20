package chat.bridge

import chat.api.LocalStoreException
import chat.api.SyncService
import chat.api.SyncStatusView
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.chat_ffi.FfiException
import uniffi.chat_ffi.SyncFfi
import uniffi.chat_ffi.SyncStatusFfi

actual fun createSyncService(
    databasePath: String,
    masterKeyHex: String,
    endpointUrl: String,
    nodeId: String,
    nodePublicKeyHex: String,
    groupId: String,
): SyncService = NativeSyncService(
    SyncFfi.open(databasePath, masterKeyHex, endpointUrl, nodeId, nodePublicKeyHex, groupId),
)

/** UniFFI 同步桥：原生调用串行化并切到 IO；错误码原样透传。 */
class NativeSyncService(private val native: SyncFfi) : SyncService {
    private val mutex = Mutex()

    override suspend fun start(): SyncStatusView = call { it.start() }

    override suspend fun stop(): SyncStatusView = call { it.stop() }

    override suspend fun status(): SyncStatusView = call { it.status() }

    override suspend fun syncOnce(): chat.api.SyncOnceView = callOnce {
        val view = it.syncOnce()
        chat.api.SyncOnceView(
            fetched = view.fetched.toLong(),
            applied = view.applied.toLong(),
            skipped = view.skipped.toLong(),
            nextCursor = view.nextCursor.toLong(),
            settled = view.settled,
        )
    }

    override suspend fun shutdown(): Unit = mutex.withLock {
        withContext(Dispatchers.IO) { runCatching { native.shutdown() } }
        Unit
    }

    /** 单轮拉取用：返回同步视图之外的形状，故不复用 call()。 */
    private suspend fun <T> callOnce(block: (SyncFfi) -> T): T = mutex.withLock {
        withContext(Dispatchers.IO) {
            try {
                block(native)
            } catch (error: FfiException.Store) {
                throw LocalStoreException(error.code, error.reason)
            }
        }
    }

    private suspend fun call(block: (SyncFfi) -> SyncStatusFfi): SyncStatusView = mutex.withLock {
        withContext(Dispatchers.IO) {
            try {
                block(native).toView()
            } catch (error: FfiException.Store) {
                throw LocalStoreException(error.code, error.reason)
            }
        }
    }
}

private fun SyncStatusFfi.toView() = SyncStatusView(
    state = state,
    appliedTotal = appliedTotal.toLong(),
    lastErrorCode = lastErrorCode,
    nextAttemptAtMillis = nextAttemptAtMs,
    resyncRequired = resyncRequired,
    rounds = rounds.toLong(),
    stoppedReason = stoppedReason,
)
