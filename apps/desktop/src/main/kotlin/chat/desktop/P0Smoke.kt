package chat.desktop

import chat.bridge.createP0Core
import kotlinx.coroutines.runBlocking

fun main() = runBlocking {
    val core = createP0Core()
    try {
        val info = core.connect()
        check(!info.publicInfrastructure && info.aliceEndpoint != info.bobEndpoint)
        check(info.epoch == 1L)
        for (sender in listOf("Alice", "Bob")) {
            val text = "Kotlin → Rust → MLS → iroh: $sender 你好 👋"
            val message = core.send(sender, text)
            check(message.text == text && message.sender == sender)
            check(message.ciphertextBytes > text.encodeToByteArray().size)
            println("PASS $sender -> ${message.receiver}; MLS epoch=${message.epoch}; ciphertext=${message.ciphertextBytes} bytes")
        }
        check(runCatching { core.send("Mallory", "wrong sender") }.isFailure)
        check(runCatching { core.send("Alice", " ") }.isFailure)
        check(core.send("Alice", "输入错误后会话仍然可用").exchangeIndex == 3L)
    } finally { core.close(); core.close() }
    check(runCatching { core.send("Alice", "closed") }.isFailure)
    println("PASS Kotlin/native error mapping and close lifecycle")
}
