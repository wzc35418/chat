package chat.api

/** 发送步骤结果（CORE/SYNC 发送链路的界面投影）；不含正文。 */
data class SendStep(
    val draftId: String,
    /** queued / prepared / submitted / accepted / rejected / unknown / cancelled / failed */
    val status: String,
    val submissionId: String?,
    val attemptIndex: Int?,
    /** 静态说明（不含正文/密钥）。 */
    val detail: String,
    /** 结果是否已确定；false 表示仍需对账。 */
    val settled: Boolean,
)
