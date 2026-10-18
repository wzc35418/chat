package chat.bridge

import chat.api.DraftCancelOutcome
import chat.api.LocalStore
import chat.api.LocalStoreException
import chat.api.StoreEvent
import chat.api.StoreEventBatch
import chat.api.StoredConversation
import chat.api.StoredDraft
import chat.api.StoredDraftStatus
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

    override suspend fun draftStatusView(draftId: String): StoredDraftStatus? = withStore { store ->
        store.draftStatusView(draftId)?.let { view ->
            StoredDraftStatus(
                draftId = view.draftId,
                conversationId = view.conversationId,
                operationId = view.operationId,
                status = view.status,
                submissionId = view.submissionId,
                attemptIndex = view.attemptIndex?.toInt(),
                attempts = view.attempts.toInt(),
                updatedAtMillis = view.updatedAtMs.toLong(),
                canRetry = view.canRetry,
                canCancel = view.canCancel,
            )
        }
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


    override suspend fun cancelDraft(draftId: String): DraftCancelOutcome = withStore { store ->
        val view = store.cancelDraft(draftId)
        DraftCancelOutcome(
            draftId = view.draftId,
            outcome = view.outcome,
            submissionId = view.submissionId,
        )
    }

    override suspend fun pollEvents(sinceSeq: Long, limit: Int): StoreEventBatch = withStore { store ->
        require(limit in 1..256) { "limit 必须在 1..256" }
        val batch = store.pollEvents(sinceSeq.toULong(), limit.toUInt())
        // 原生事件不携带序号：按批次末尾序号与数组位置推导（1 起、单调）。
        val firstSequence = batch.nextSeq.toLong() - batch.events.size + 1
        StoreEventBatch(
            events = batch.events.mapIndexed { index, event ->
                StoreEvent(
                    kind = event.kind,
                    draftId = event.draftId,
                    conversationId = event.conversationId,
                    status = event.status,
                    submissionId = event.submissionId,
                    sequence = firstSequence + index,
                )
            },
            nextSeq = batch.nextSeq.toLong(),
            resyncRequired = batch.resyncRequired,
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
