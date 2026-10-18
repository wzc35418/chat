package chat.bridge

import chat.api.LocalStore
import chat.api.LocalStoreException
import chat.api.StoredConversation
import chat.api.StoredDraft
import chat.api.StoredMessage
import chat.api.StoredPage
import chat.api.VerifiedMessageInput
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.chat_ffi.FfiException
import uniffi.chat_ffi.LocalStoreFfi

actual fun createLocalStore(): LocalStore = NativeLocalStore()

/**
 * UniFFI 本地存储桥。
 *
 * - 所有原生调用切到 IO 调度；同一实例用互斥锁串行化（Rust 侧是独占写库）。
 * - 原生错误统一转成 [LocalStoreException]，code 是稳定错误码（PROTOCOL-014）。
 * - close 之后拒绝任何调用，并释放原生句柄；重复 close 幂等。
 */
class NativeLocalStore : LocalStore {
    private val mutex = Mutex()
    private var native: LocalStoreFfi? = null

    override suspend fun open(databasePath: String, masterKeyHex: String) = mutex.withLock {
        check(native == null) { "本地存储已经打开" }
        native = withContext(Dispatchers.IO) {
            guard { LocalStoreFfi.open(databasePath, masterKeyHex) }
        }
    }

    override suspend fun createDraft(
        conversationId: String,
        operationId: String,
        body: String,
    ): StoredDraft = withStore { store ->
        store.createDraft(conversationId, operationId, body).toStoredDraft()
    }

    override suspend fun draftStatus(draftId: String): StoredDraft? = withStore { store ->
        store.draftStatus(draftId)?.toStoredDraft()
    }

    override suspend fun applyVerifiedMessage(input: VerifiedMessageInput): StoredMessage = withStore { store ->
        store.applyVerifiedEntry(
            conversationId = input.conversationId,
            title = input.title,
            messageId = input.messageId,
            authorAccount = input.authorAccount,
            authorDevice = input.authorDevice,
            body = input.body,
            mine = input.mine,
            createdAtMs = input.createdAtMillis.toULong(),
        ).toStoredMessage()
    }

    override suspend fun listConversations(
        cursor: String?,
        limit: Int,
    ): StoredPage<StoredConversation> = withStore { store ->
        require(limit in 1..200) { "limit 必须在 1..200" }
        val page = store.listConversations(cursor, limit.toUInt())
        StoredPage(
            items = page.items.map { row ->
                StoredConversation(
                    conversationId = row.conversationId,
                    title = row.title,
                    preview = row.preview,
                    unreadCount = row.unreadCount?.toInt(),
                    updatedAtMillis = row.updatedAtMs.toLong(),
                )
            },
            nextCursor = page.nextCursor,
        )
    }

    override suspend fun listMessages(
        conversationId: String,
        cursor: String?,
        limit: Int,
    ): StoredPage<StoredMessage> = withStore { store ->
        require(limit in 1..200) { "limit 必须在 1..200" }
        val page = store.listMessages(conversationId, cursor, limit.toUInt())
        StoredPage(
            items = page.items.map { row -> row.toStoredMessage() },
            nextCursor = page.nextCursor,
        )
    }

    override suspend fun close(): Unit = withContext(NonCancellable) {
        mutex.withLock {
            withContext(Dispatchers.IO) {
                val current = native
                native = null
                if (current != null) {
                    try {
                        current.shutdown()
                    } catch (_: Throwable) {
                        // 关闭时的原生错误不再上抛：句柄随即销毁，避免泄漏更重要。
                    } finally {
                        current.destroy()
                    }
                }
            }
        }
    }

    private suspend fun <T> withStore(block: (LocalStoreFfi) -> T): T = mutex.withLock {
        val store = native ?: throw LocalStoreException("CORE_CLOSED", "本地存储尚未打开或已关闭")
        withContext(Dispatchers.IO) { guard { block(store) } }
    }

    private inline fun <T> guard(block: () -> T): T = try {
        block()
    } catch (error: FfiException.Store) {
        throw LocalStoreException(error.code, error.reason)
    }
}

private fun uniffi.chat_ffi.DraftView.toStoredDraft() = StoredDraft(
    draftId = draftId,
    operationId = operationId,
    conversationId = conversationId,
    body = body,
    state = state,
    submissionId = submissionId,
)

private fun uniffi.chat_ffi.MessageRowView.toStoredMessage() = StoredMessage(
    messageId = messageId,
    conversationId = conversationId,
    author = author,
    body = body,
    mine = mine,
    createdAtMillis = createdAtMs.toLong(),
)
