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

    override suspend fun shutdown(): Unit = mutex.withLock {
        withContext(Dispatchers.IO) { runCatching { native.shutdown() } }
        Unit
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
