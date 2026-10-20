package chat.presentation.preview

/** 首启引导状态（三步：说明 → 节点配置 → 完成）。 */
data class PreviewOnboardingState(
    val step: Int = 0,
    val endpointUrl: String = "",
    val nodeId: String = "",
    val nodePublicKeyHex: String = "",
    val busy: Boolean = false,
    val notice: String? = null,
    val lastErrorCode: String? = null,
)

fun PreviewOnboardingState.updateEndpoint(value: String): PreviewOnboardingState =
    copy(endpointUrl = value, lastErrorCode = null)

fun PreviewOnboardingState.updateNodeId(value: String): PreviewOnboardingState =
    copy(nodeId = value, lastErrorCode = null)

fun PreviewOnboardingState.updateNodePublicKey(value: String): PreviewOnboardingState =
    copy(nodePublicKeyHex = value, lastErrorCode = null)

fun PreviewOnboardingState.advance(): PreviewOnboardingState =
    copy(step = (step + 1).coerceAtMost(2), busy = false, notice = null, lastErrorCode = null)

fun PreviewOnboardingState.withOnboardingError(code: String, notice: String?): PreviewOnboardingState =
    copy(busy = false, lastErrorCode = code, notice = notice)
