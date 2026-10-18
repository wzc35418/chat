package chat.presentation

import chat.api.P0Core
import chat.api.P0Delivery
import chat.api.P0SessionInfo
import chat.presentation.display.UiCopy
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.withContext

const val P0_MAX_MESSAGE_BYTES = 8192
const val P0_HISTORY_LIMIT = 100

val P0_SENDERS: List<String> = listOf("Alice", "Bob")

enum class P0Operation { CONNECTING, SENDING }

data class P0UiError(
    val code: String,
    val title: String,
    val detail: String,
    val action: String,
)

data class P0UiState(
    val busy: Boolean = false,
    val operation: P0Operation? = null,
    val session: P0SessionInfo? = null,
    val messages: List<P0Delivery> = emptyList(),
    val sender: String = "Alice",
    val composer: String = "",
    val error: P0UiError? = null,
    val status: String = "尚未连接",
    val canSend: Boolean = false,
    val requiresRebuild: Boolean = false,
    val closed: Boolean = false,
) {
    val composerBytes: Int get() = p0Utf8ByteCount(composer)
}

fun p0Utf8ByteCount(text: String): Int = text.encodeToByteArray().size

/**
 * P0 展示状态。
 *
 * - 发送成功只表示 P0 接收端已经验证并解密，不代表正式“已接受”或“已送达”。
 * - 发送结果未知时不会自动重发或重新加密；本端 P0 会话需要重建。
 * - `requiresRebuild` 是不可关闭的展示状态：关闭错误横幅、编辑草稿或切换发送者
 *   都不会把它清掉，只有成功重建会话才会解除。
 * - 输入内容在点击发送时冻结；迟到结果不会清掉之后继续输入的文字。
 */
class P0Presenter(
    private val core: P0Core,
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default),
) {
    private val gate = Mutex()
    private val mutableState = MutableStateFlow(P0UiState())
    val state: StateFlow<P0UiState> = mutableState.asStateFlow()

    fun selectSender(sender: String) {
        if (sender !in P0_SENDERS) return
        mutableState.update { current ->
            if (current.closed) current else current.copy(sender = sender)
        }
    }

    fun updateComposer(value: String) {
        val bytes = p0Utf8ByteCount(value)
        mutableState.update { current ->
            when {
                current.closed -> current
                bytes > P0_MAX_MESSAGE_BYTES -> current.copy(
                    error = P0UiError(
                        code = "P0_INPUT_TOO_LARGE",
                        title = "消息超出长度上限",
                        detail = "消息最多 $P0_MAX_MESSAGE_BYTES 个 UTF-8 字节，当前输入为 $bytes 字节。",
                        action = "请删减内容后再发送。",
                    ),
                )
                else -> current.copy(
                    composer = value,
                    error = current.error?.takeUnless { it.code == "P0_INPUT_TOO_LARGE" },
                )
            }
        }
    }

    fun dismissError() {
        mutableState.update { it.copy(error = null) }
    }

    fun connect() {
        perform(P0Operation.CONNECTING) {
            mutableState.update {
                it.copy(
                    session = null,
                    messages = emptyList(),
                    canSend = false,
                    status = "正在建立加密会话…",
                )
            }
            core.close()
            val session = core.connect()
            mutableState.update {
                it.copy(
                    session = session,
                    canSend = true,
                    requiresRebuild = false,
                    status = "两个设备已连接，选择发送设备后开始。",
                    error = null,
                )
            }
        }
    }

    fun send(
        sender: String = mutableState.value.sender,
        text: String = mutableState.value.composer,
    ) {
        val current = mutableState.value
        if (current.closed) {
            showLocalError("P0_CLOSED", "会话已关闭", "当前页面已经关闭，不能发送消息。", "请重新打开应用。")
            return
        }
        // 结果未知期间不进入参数校验或新发送，避免覆盖“需要重建”的阻断状态。
        if (current.requiresRebuild) return

        when {
            sender !in P0_SENDERS -> {
                showLocalError("P0_UNKNOWN_SENDER", "未知发送设备", "只能选择 Alice 或 Bob 作为发送设备。", "请重新选择。")
                return
            }
            text.isBlank() -> {
                showLocalError("P0_INPUT_EMPTY", "还没有内容", "请输入一条消息再发送。", "内容会保留在编辑区。")
                return
            }
            p0Utf8ByteCount(text) > P0_MAX_MESSAGE_BYTES -> {
                showLocalError(
                    "P0_INPUT_TOO_LARGE",
                    "消息超出长度上限",
                    "消息最多 $P0_MAX_MESSAGE_BYTES 个 UTF-8 字节。",
                    "请删减内容后再发送。",
                )
                return
            }
        }
        if (!mutableState.value.canSend) {
            showLocalError("P0_NOT_CONNECTED", "尚未连接", "请先连接两个本机设备，再发送消息。", "连接后可重试。")
            return
        }

        perform(P0Operation.SENDING) {
            // 发送时冻结发送者和正文；期间编辑或切换发送者不影响这一次调用。
            val message = core.send(sender, text)
            mutableState.update { latest ->
                latest.copy(
                    messages = (latest.messages + message).takeLast(P0_HISTORY_LIMIT),
                    composer = if (latest.composer == text) "" else latest.composer,
                    status = "${message.receiver} 的${UiCopy.P0_VERIFIED}",
                    canSend = true,
                    requiresRebuild = false,
                    error = null,
                )
            }
        }
    }

    suspend fun close() = withContext(NonCancellable) {
        gate.lock()
        try {
            // 资源释放失败也不把原生异常原文暴露到界面。
            try {
                core.close()
            } catch (_: Throwable) {
                // 已尝试释放；下方状态仍标记为关闭。
            }
            mutableState.update {
                it.copy(
                    busy = false,
                    operation = null,
                    session = null,
                    canSend = false,
                    requiresRebuild = false,
                    closed = true,
                    status = "会话已关闭",
                    error = null,
                )
            }
        } finally {
            gate.unlock()
        }
    }

    fun dispose() {
        scope.launch {
            try {
                close()
            } finally {
                scope.cancel()
            }
        }
    }

    private fun showLocalError(code: String, title: String, detail: String, action: String) {
        mutableState.update { it.copy(error = P0UiError(code, title, detail, action)) }
    }

    private fun perform(operation: P0Operation, action: suspend () -> Unit) {
        if (!gate.tryLock()) return
        if (mutableState.value.closed) {
            gate.unlock()
            return
        }
        mutableState.update { it.copy(busy = true, operation = operation, error = null) }
        scope.launch {
            try {
                action()
            } catch (cancelled: CancellationException) {
                if (operation == P0Operation.SENDING) {
                    mutableState.update {
                        it.copy(
                            canSend = false,
                            requiresRebuild = true,
                            status = "操作已取消，发送结果未知，需要重建会话",
                            error = P0UiError(
                                code = "P0_SEND_CANCELLED",
                                title = "发送结果未知",
                                detail = "操作在中途取消，接收设备是否已处理这条消息无法确认。",
                                action = "不会自动重发；请重建会话后确认接收端状态。",
                            ),
                        )
                    }
                }
                throw cancelled
            } catch (_: Throwable) {
                mutableState.update { current ->
                    when (operation) {
                        P0Operation.CONNECTING -> current.copy(
                            session = null,
                            canSend = false,
                            status = "连接失败",
                            error = P0UiError(
                                code = "P0_CONNECT_FAILED",
                                title = "无法建立加密会话",
                                detail = "本机两个临时设备没有完成连接。",
                                action = "请重试；若持续失败，检查本机运行环境。",
                            ),
                        )
                        P0Operation.SENDING -> current.copy(
                            canSend = false,
                            requiresRebuild = true,
                            status = "发送结果未知，需重建会话",
                            error = P0UiError(
                                code = "P0_SEND_UNKNOWN",
                                title = "发送结果未知",
                                detail = "接收设备是否已处理这条消息无法确认；没有自动重发或重新加密。",
                                action = "请重建会话，并先确认接收端状态。",
                            ),
                        )
                    }
                }
            } finally {
                mutableState.update { it.copy(busy = false, operation = null) }
                gate.unlock()
            }
        }
    }
}
