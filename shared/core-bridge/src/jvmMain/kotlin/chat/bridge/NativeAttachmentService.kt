package chat.bridge

import chat.api.AttachmentService
import chat.api.LocalAttachment
import chat.api.LocalStoreException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.chat_ffi.AttachmentFfi
import uniffi.chat_ffi.FfiException

actual fun createAttachmentService(cacheRoot: String): AttachmentService =
    NativeAttachmentService(cacheRoot)

/**
 * UniFFI 附件桥：原生调用切到 IO 调度并串行化；错误转成带稳定码的 LocalStoreException。
 */
class NativeAttachmentService(cacheRoot: String) : AttachmentService {
    private val store: AttachmentFfi = try {
        AttachmentFfi(cacheRoot)
    } catch (error: FfiException.Store) {
        throw LocalStoreException(error.code, error.reason)
    }
    private val mutex = Mutex()

    override suspend fun importFile(path: String, displayName: String): LocalAttachment = mutex.withLock {
        withContext(Dispatchers.IO) {
            guard { store.importFile(path, displayName).toLocal() }
        }
    }

    override suspend fun status(attachmentId: String): LocalAttachment = mutex.withLock {
        withContext(Dispatchers.IO) { guard { store.status(attachmentId).toLocal() } }
    }

    override suspend fun exportFile(attachmentId: String, targetPath: String): Long = mutex.withLock {
        withContext(Dispatchers.IO) {
            guard { store.exportFile(attachmentId, targetPath) }.toLong()
        }
    }

    override suspend fun cancel(attachmentId: String): LocalAttachment = mutex.withLock {
        withContext(Dispatchers.IO) { guard { store.cancel(attachmentId).toLocal() } }
    }

    private inline fun <T> guard(block: () -> T): T = try {
        block()
    } catch (error: FfiException.Store) {
        throw LocalStoreException(error.code, error.reason)
    }
}

private fun uniffi.chat_ffi.AttachmentView.toLocal() = LocalAttachment(
    attachmentId = attachmentId,
    plainSize = plainSize.toLong(),
    chunkSize = chunkSize.toInt(),
    chunkCount = chunkCount.toInt(),
    contentDigest = contentDigest,
    displayName = displayName,
    state = state,
    availableChunks = availableChunks.toInt(),
    transferredChunks = transferredChunks.toInt(),
    cancelRequested = cancelRequested,
)
