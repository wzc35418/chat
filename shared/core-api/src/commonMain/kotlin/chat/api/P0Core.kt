package chat.api

data class P0SessionInfo(
    val aliceEndpoint: String, val bobEndpoint: String, val groupId: String,
    val epoch: Long, val ciphersuite: String, val transport: String,
    val publicInfrastructure: Boolean,
)
data class P0Delivery(
    val sender: String, val receiver: String, val text: String,
    val ciphertextBytes: Int, val ciphertextSha256: String,
    val epoch: Long, val exchangeIndex: Long, val elapsedMs: Long,
)

// Separate from ConversationService: P0 has no coordinator or durable delivery status.
interface P0Core {
    suspend fun connect(): P0SessionInfo
    suspend fun send(sender: String, text: String): P0Delivery
    suspend fun close()
}
