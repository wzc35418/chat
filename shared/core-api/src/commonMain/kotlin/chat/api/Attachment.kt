package chat.api

/** 本地附件（ATTACHMENTS-001/002/006 本地部分）；不含 FileKey。 */
data class LocalAttachment(
    val attachmentId: String,
    val plainSize: Long,
    val chunkSize: Int,
    val chunkCount: Int,
    val contentDigest: String,
    val displayName: String,
    /** imported / cancelled … */
    val state: String,
    val availableChunks: Int,
    /** 已传输块数（上传/下载进度；失败后保留）。 */
    val transferredChunks: Int,
    val cancelRequested: Boolean,
)

/**
 * 本地附件服务：加密导入/状态/导出/取消。
 *
 * 网络上传下载（NODE-012~015）已具备节点端点，但尚未接线；本接口只覆盖本机链路。
 */
interface AttachmentService {
    suspend fun importFile(path: String, displayName: String): LocalAttachment

    suspend fun status(attachmentId: String): LocalAttachment

    /** 解密导出到目标路径，返回写出的明文字节数。 */
    suspend fun exportFile(attachmentId: String, targetPath: String): Long

    /** 批量读附件显示信息（消息附件卡片用）：未知 ID 跳过；不解密内容。 */
    suspend fun records(attachmentIds: List<String>): List<LocalAttachment>

    /**
     * 从节点下载并认证解密到目标路径（ATTACHMENTS-005）。
     *
     * 需要节点端点、群 ID 与本设备已接受条目的序号；未配置时显式失败，不伪造下载成功。
     */
    suspend fun download(
        attachmentId: String,
        targetPath: String,
        endpointUrl: String,
        nodeId: String,
        nodePublicKeyHex: String?,
        groupId: String,
        sourceSeq: Long,
    ): Long

    suspend fun cancel(attachmentId: String): LocalAttachment
}
