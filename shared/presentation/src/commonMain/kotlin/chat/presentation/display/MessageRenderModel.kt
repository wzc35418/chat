package chat.presentation.display

import chat.api.AttachmentId
import chat.api.MessageContent

enum class AttachmentDisplayState {
    /** 底层附件能力尚未接入，只能显示占位，不提供下载动作。 */
    CAPABILITY_PENDING,
    UNAVAILABLE,
}

/**
 * 只读展示模型。正文只输出纯文本或安全 Markdown 展示模型（见 MarkdownModel.kt）；
 * 界面不得把其中的字符解释为 HTML、脚本或可执行扩展 payload。
 * 文本消息的 Markdown 由 renderMarkdownMessage 单独解析，链接协议白名单、长度与嵌套上限都在展示层强制执行。
 */
sealed interface MessageRenderModel {
    data class Text(val text: String) : MessageRenderModel

    data class Attachment(
        val id: AttachmentId,
        val caption: String?,
        val state: AttachmentDisplayState,
        val badge: String,
    ) : MessageRenderModel

    data class ExtensionFallback(val title: String, val text: String) : MessageRenderModel
}

object RenderLimits {
    const val MAX_DISPLAY_CHARS = 4000
}

fun renderMessageContent(content: MessageContent): MessageRenderModel = when (content) {
    is MessageContent.Text -> MessageRenderModel.Text(
        text = sanitizeDisplayText(content.text).ifEmpty { "（空消息）" },
    )

    is MessageContent.Attachment -> MessageRenderModel.Attachment(
        id = content.id,
        caption = content.caption?.let(::sanitizeDisplayText)?.ifEmpty { null },
        state = AttachmentDisplayState.CAPABILITY_PENDING,
        badge = "附件能力尚未接入，暂不可预览或下载",
    )

    is MessageContent.Extension -> MessageRenderModel.ExtensionFallback(
        title = "不支持的扩展消息",
        text = sanitizeDisplayText(content.fallbackText)
            .ifEmpty { "此消息使用了当前版本不支持的内容格式，无法显示正文。" },
    )
}

/** 输入类型无法识别时的安全回退，调用方不得传入原始 payload。 */
fun renderUnknownMessageContent(): MessageRenderModel = MessageRenderModel.ExtensionFallback(
    title = "未知消息内容",
    text = "这条消息的内容类型无法识别，已按纯文本安全回退显示。",
)

/**
 * 去掉控制字符和双向文本控制符，并限制展示长度。
 * 尖括号等字符可以保留，因为展示模型只输出纯文本，不参与富文本解析。
 */
fun sanitizeDisplayText(raw: String): String {
    val builder = StringBuilder(minOf(raw.length, RenderLimits.MAX_DISPLAY_CHARS))
    for (character in raw) {
        if (builder.length >= RenderLimits.MAX_DISPLAY_CHARS) {
            builder.append('…')
            break
        }
        when {
            character == '\n' || character == '\t' -> builder.append(character)
            character == '\r' -> Unit
            character.isISOControl() -> Unit
            character in '\u202A'..'\u202E' -> Unit
            character in '\u2066'..'\u2069' -> Unit
            character == '\u200E' || character == '\u200F' -> Unit
            else -> builder.append(character)
        }
    }
    return builder.toString().trim()
}
