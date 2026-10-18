package chat.bridge

import chat.api.P0Core
import chat.api.P0Delivery
import chat.api.P0SessionInfo
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.chat_ffi.P0Core as RustP0Core

actual fun createP0Core(): P0Core = NativeP0Core()

class NativeP0Core : P0Core {
    private val mutex = Mutex()
    private var native: RustP0Core? = null

    override suspend fun connect(): P0SessionInfo = mutex.withLock {
        check(native == null) { "会话已经建立" }
        var created: RustP0Core? = null
        try {
            val info = withContext(Dispatchers.IO) {
                val opened = RustP0Core()
                created = opened
                val info = opened.info()
                coroutineContext.ensureActive()
                info
            }
            native = created
            P0SessionInfo(info.aliceEndpoint, info.bobEndpoint, info.groupId, info.epoch.toLong(),
                info.ciphersuite, info.transport, info.publicInfrastructure)
        } catch (error: Throwable) {
            // Cleanup also covers cancellation while returning from the IO dispatcher.
            withContext(NonCancellable + Dispatchers.IO) {
                created?.let { opened -> try { opened.shutdown() } finally { opened.destroy() } }
            }
            throw error
        }
    }

    override suspend fun send(sender: String, text: String): P0Delivery = mutex.withLock {
        withContext(Dispatchers.IO) {
            val message = checkNotNull(native) { "请先连接两个设备" }.send(sender, text)
            P0Delivery(message.sender, message.receiver, message.text, message.ciphertextBytes.toInt(),
                message.ciphertextSha256, message.epoch.toLong(), message.exchangeIndex.toLong(), message.elapsedMs.toLong())
        }
    }

    override suspend fun close(): Unit = withContext(NonCancellable) {
        mutex.withLock {
            withContext(Dispatchers.IO) {
                val current = native
                native = null
                if (current != null) try { current.shutdown() } finally { current.destroy() }
            }
        }
    }
}
