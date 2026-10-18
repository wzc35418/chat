package chat.presentation.display

/**
 * 消息正文的 Markdown 子集展示模型（PRESENTATION-009 扩展）。
 *
 * 安全边界：
 * - 不解析 HTML：任意尖括号内容（例如 script 标签）只作为普通文字显示，模型中不存在 HTML 节点；
 * - 链接只接受 http/https/mailto，其他协议（javascript:/data:/file: 等）降级为不可点击文字并给出原因；
 * - 输入长度、块数、行内节点数、嵌套深度、代码块行数、链接长度都有上限，超限有界截断；
 * - 控制字符与双向文本控制符在进入模型前已清理。
 *
 * 解析器与界面无关，可在 commonMain 纯函数测试；界面只负责把模型画出来。
 */

/** 行内代码与围栏代码使用的反引号（避免在源码中直接书写，便于统一替换与测试）。 */
const val MARKDOWN_CODE_TICK: String = "\u0060"

object MarkdownLimits {
    const val MAX_INPUT_CHARS: Int = 8192
    const val MAX_BLOCKS: Int = 200
    const val MAX_INLINE_NODES: Int = 600
    const val MAX_INLINE_DEPTH: Int = 3
    const val MAX_LINK_URL_CHARS: Int = 512
    const val MAX_CODE_BLOCK_LINES: Int = 200
    const val MAX_CODE_BLOCK_CHARS: Int = 2000
    const val MAX_LANGUAGE_CHARS: Int = 32

    val ALLOWED_LINK_SCHEMES: List<String> = listOf("http://", "https://", "mailto:")
}

sealed interface MarkdownInline {
    data class Text(val text: String) : MarkdownInline

    data class Strong(val children: List<MarkdownInline>) : MarkdownInline

    data class Emphasis(val children: List<MarkdownInline>) : MarkdownInline

    data class Strikethrough(val children: List<MarkdownInline>) : MarkdownInline

    data class Code(val text: String) : MarkdownInline

    data class Link(val children: List<MarkdownInline>, val url: String) : MarkdownInline

    /** 协议/长度不合规的链接：保留可读文字，明确不可点击。 */
    data class BlockedLink(val children: List<MarkdownInline>, val reason: String) : MarkdownInline
}

sealed interface MarkdownBlock {
    data class Paragraph(val lines: List<List<MarkdownInline>>) : MarkdownBlock

    data class Heading(val level: Int, val inlines: List<MarkdownInline>) : MarkdownBlock

    data class BulletList(val items: List<List<MarkdownInline>>) : MarkdownBlock

    data class OrderedList(val start: Int, val items: List<List<MarkdownInline>>) : MarkdownBlock

    data class Quote(val lines: List<List<MarkdownInline>>) : MarkdownBlock

    data class CodeBlock(val language: String?, val code: String) : MarkdownBlock

    data object Rule : MarkdownBlock
}

/**
 * @param truncated 输入或节点预算被截断时为 true，界面需要提示“内容过长已省略”。
 * @param plainText 供无障碍播报、搜索与纯文本回退使用的扁平文本。
 */
data class MarkdownDocument(
    val blocks: List<MarkdownBlock>,
    val truncated: Boolean,
    val plainText: String,
) {
    val isEmpty: Boolean get() = blocks.isEmpty()
}

/** 文本消息的 Markdown 展示；非文本内容返回 null，调用方继续用 renderMessageContent 的占位/回退。 */
fun renderMarkdownMessage(content: chat.api.MessageContent): MarkdownDocument? =
    when (content) {
        is chat.api.MessageContent.Text -> renderMarkdown(content.text)
        else -> null
    }

/** 界面统一入口：解析不出任何块时返回“空消息”回退文档，保证永远有可见内容。 */
fun markdownPlainTextOrFallback(source: String): MarkdownDocument {
    val document = renderMarkdown(source)
    if (!document.isEmpty) return document
    return MarkdownDocument(
        blocks = listOf(MarkdownBlock.Paragraph(listOf(listOf(MarkdownInline.Text("（空消息）"))))),
        truncated = false,
        plainText = "（空消息）",
    )
}

fun renderMarkdown(source: String): MarkdownDocument {
    val cleaned = sanitizeMarkdownSource(source)
    // 只有真正超过输入上限才算截断；清理控制字符导致的长度变化不算。
    val truncatedByInput = source.length > MarkdownLimits.MAX_INPUT_CHARS
    val lines = cleaned.split('\n')
    val blocks = mutableListOf<MarkdownBlock>()
    var truncated = truncatedByInput
    val paragraphs = mutableListOf<List<MarkdownInline>>()
    var index = 0

    fun flushParagraph() {
        if (paragraphs.isEmpty()) return
        blocks += MarkdownBlock.Paragraph(paragraphs.toList())
        paragraphs.clear()
    }

    while (index < lines.size) {
        if (blocks.size >= MarkdownLimits.MAX_BLOCKS) {
            truncated = true
            break
        }
        val line = lines[index]
        val trimmed = line.trim()
        val fenceMarker = fenceOpen(trimmed)
        when {
            trimmed.isEmpty() -> {
                flushParagraph()
                index++
            }

            fenceMarker != null -> {
                flushParagraph()
                val language = trimmed.drop(3).trim()
                    .take(MarkdownLimits.MAX_LANGUAGE_CHARS)
                    .ifEmpty { null }
                val code = mutableListOf<String>()
                index++
                while (index < lines.size) {
                    val candidate = lines[index]
                    if (candidate.trim().startsWith(fenceMarker)) {
                        index++
                        break
                    }
                    if (code.size >= MarkdownLimits.MAX_CODE_BLOCK_LINES) {
                        truncated = true
                        index = lines.size
                        break
                    }
                    code += candidate
                    index++
                }
                blocks += MarkdownBlock.CodeBlock(
                    language = language,
                    code = capCode(code.joinToString("\n")),
                )
            }

            headingLevel(trimmed) > 0 -> {
                flushParagraph()
                val level = headingLevel(trimmed)
                val body = trimmed.drop(level).trimStart()
                val parsed = parseInlines(body)
                truncated = truncated || parsed.truncated
                blocks += MarkdownBlock.Heading(level, parsed.nodes)
                index++
            }

            isRule(trimmed) -> {
                flushParagraph()
                blocks += MarkdownBlock.Rule
                index++
            }

            trimmed.startsWith(">") -> {
                flushParagraph()
                val quoteLines = mutableListOf<List<MarkdownInline>>()
                while (index < lines.size && lines[index].trim().startsWith(">")) {
                    val quoted = lines[index].trim().removePrefix(">").trimStart()
                    val parsed = parseInlines(quoted)
                    truncated = truncated || parsed.truncated
                    quoteLines += parsed.nodes
                    index++
                }
                blocks += MarkdownBlock.Quote(quoteLines)
            }

            bulletItem(trimmed) != null -> {
                flushParagraph()
                val items = mutableListOf<List<MarkdownInline>>()
                while (index < lines.size) {
                    val item = bulletItem(lines[index].trim()) ?: break
                    val parsed = parseInlines(item)
                    truncated = truncated || parsed.truncated
                    items += parsed.nodes
                    index++
                }
                blocks += MarkdownBlock.BulletList(items)
            }

            orderedItem(trimmed) != null -> {
                flushParagraph()
                val start = orderedItem(trimmed) ?: 1
                val items = mutableListOf<List<MarkdownInline>>()
                while (index < lines.size) {
                    val candidate = lines[index].trim()
                    if (orderedItem(candidate) == null) break
                    val body = orderedBody(candidate)
                    val parsed = parseInlines(body)
                    truncated = truncated || parsed.truncated
                    items += parsed.nodes
                    index++
                }
                blocks += MarkdownBlock.OrderedList(start, items)
            }

            else -> {
                val parsed = parseInlines(trimmed)
                truncated = truncated || parsed.truncated
                if (parsed.nodes.isNotEmpty()) paragraphs += parsed.nodes
                index++
            }
        }
    }
    flushParagraph()

    val limited = MarkdownDocument(
        blocks = blocks.take(MarkdownLimits.MAX_BLOCKS),
        truncated = truncated,
        plainText = "",
    )
    return limited.copy(plainText = flattenPlainText(limited.blocks))
}

/**
 * Markdown 输入命令：把选区包进对应标记。
 * 只做纯文本变换，不写入任何正式状态；空选区插入占位文字并选中它，方便直接改写。
 */
enum class MarkdownCommand { BOLD, ITALIC, STRIKE, CODE, LINK }

data class MarkdownDraft(
    val text: String,
    val selectionStart: Int = text.length,
    val selectionEnd: Int = selectionStart,
)

fun MarkdownDraft.applyMarkdownCommand(command: MarkdownCommand): MarkdownDraft {
    val start = selectionStart.coerceIn(0, text.length)
    val end = selectionEnd.coerceIn(start, text.length)
    val selected = text.substring(start, end)
    val prefix: String
    val suffix: String
    val placeholder: String
    when (command) {
        MarkdownCommand.BOLD -> {
            prefix = "**"; suffix = "**"; placeholder = "粗体文字"
        }

        MarkdownCommand.ITALIC -> {
            prefix = "*"; suffix = "*"; placeholder = "斜体文字"
        }

        MarkdownCommand.STRIKE -> {
            prefix = "~~"; suffix = "~~"; placeholder = "删除线文字"
        }

        MarkdownCommand.CODE -> {
            prefix = MARKDOWN_CODE_TICK; suffix = MARKDOWN_CODE_TICK; placeholder = "代码"
        }

        MarkdownCommand.LINK -> {
            prefix = "["; suffix = "](https://)"; placeholder = "链接文字"
        }
    }
    val inner = selected.ifEmpty { placeholder }
    val updated = text.substring(0, start) + prefix + inner + suffix + text.substring(end)
    val innerStart = start + prefix.length
    val innerEnd = innerStart + inner.length
    // 已有选中内容：把光标放到整个标记之后，方便继续输入；空选区：选中占位文字，便于直接改写。
    val cursor = if (selected.isEmpty()) innerEnd else innerEnd + suffix.length
    val selection = if (selected.isEmpty()) innerStart to innerEnd else cursor to cursor
    return MarkdownDraft(updated, selection.first, selection.second)
}

private data class ParsedInlines(val nodes: List<MarkdownInline>, val truncated: Boolean)

private class InlineBudget(var remaining: Int) {
    fun take(): Boolean {
        if (remaining <= 0) return false
        remaining--
        return true
    }
}

private fun parseInlines(source: String, depth: Int = 0): ParsedInlines {
    val budget = InlineBudget(MarkdownLimits.MAX_INLINE_NODES)
    val nodes = parseInlineNodes(source, depth, budget)
    return ParsedInlines(nodes, budget.remaining <= 0)
}

private fun parseInlineNodes(source: String, depth: Int, budget: InlineBudget): List<MarkdownInline> {
    val nodes = mutableListOf<MarkdownInline>()
    val text = StringBuilder()

    fun flushText() {
        if (text.isEmpty()) return
        val value = sanitizeInlineText(text.toString())
        if (value.isNotEmpty()) nodes += MarkdownInline.Text(value)
        text.clear()
    }

    var index = 0
    while (index < source.length) {
        if (!budget.take()) {
            text.append(source, index, source.length)
            break
        }
        val character = source[index]
        if (character == '\\' && index + 1 < source.length) {
            text.append(source[index + 1])
            index += 2
            continue
        }
        if (character == '\u0060') {
            val closing = source.indexOf('\u0060', index + 1)
            if (closing > index + 1) {
                flushText()
                nodes += MarkdownInline.Code(sanitizeInlineText(source.substring(index + 1, closing)))
                index = closing + 1
                continue
            }
        }
        if (depth < MarkdownLimits.MAX_INLINE_DEPTH) {
            val strong = matchWrapped(source, index, "**", "**") ?: matchWrapped(source, index, "__", "__")
            if (strong != null) {
                flushText()
                nodes += MarkdownInline.Strong(parseInlineNodes(strong.first, depth + 1, budget))
                index = strong.second
                continue
            }
            val strike = matchWrapped(source, index, "~~", "~~")
            if (strike != null) {
                flushText()
                nodes += MarkdownInline.Strikethrough(parseInlineNodes(strike.first, depth + 1, budget))
                index = strike.second
                continue
            }
            val single = matchWrapped(source, index, "*", "*") ?: matchWrapped(source, index, "_", "_")
            if (single != null) {
                flushText()
                nodes += MarkdownInline.Emphasis(parseInlineNodes(single.first, depth + 1, budget))
                index = single.second
                continue
            }
        }
        if (character == '[') {
            val closingBracket = source.indexOf(']', index + 1)
            if (closingBracket > index + 1 && closingBracket + 1 < source.length && source[closingBracket + 1] == '(') {
                val closingParen = source.indexOf(')', closingBracket + 2)
                if (closingParen > closingBracket + 2) {
                    flushText()
                    val label = parseInlineNodes(source.substring(index + 1, closingBracket), depth + 1, budget)
                    val rawUrl = source.substring(closingBracket + 2, closingParen).trim()
                    val blockedReason = validateLinkUrl(rawUrl)
                    nodes += if (blockedReason == null) {
                        MarkdownInline.Link(label, rawUrl)
                    } else {
                        MarkdownInline.BlockedLink(label, blockedReason)
                    }
                    index = closingParen + 1
                    continue
                }
            }
        }
        text.append(character)
        index++
    }
    flushText()
    return mergeAdjacentText(nodes)
}

private fun matchWrapped(source: String, index: Int, open: String, close: String): Pair<String, Int>? {
    if (!source.startsWith(open, index)) return null
    if (open.length == 1) {
        val doubled = open + open
        if (source.startsWith(doubled, index)) return null
    }
    val contentStart = index + open.length
    if (contentStart >= source.length) return null
    var search = contentStart
    while (search < source.length) {
        val found = source.indexOf(close, search)
        if (found < 0) return null
        if (open.length == 1 && found + 1 < source.length && source[found + 1] == close[0]) {
            search = found + 2
            continue
        }
        if (found == contentStart) {
            search = found + open.length
            continue
        }
        return source.substring(contentStart, found) to (found + open.length)
    }
    return null
}

/** 返回 null 表示可点击；返回字符串表示降级原因。相对路径/锚点暂不支持，按不可点击处理。 */
private fun validateLinkUrl(raw: String): String? {
    if (raw.isEmpty()) return "链接地址为空"
    if (raw.length > MarkdownLimits.MAX_LINK_URL_CHARS) return "链接过长"
    val lowered = raw.lowercase()
    if (MarkdownLimits.ALLOWED_LINK_SCHEMES.any { lowered.startsWith(it) }) return null
    return if (lowered.contains(':')) "链接协议不在允许范围内" else "相对链接暂不支持"
}

private fun mergeAdjacentText(nodes: List<MarkdownInline>): List<MarkdownInline> {
    val merged = mutableListOf<MarkdownInline>()
    for (node in nodes) {
        val last = merged.lastOrNull()
        if (node is MarkdownInline.Text && last is MarkdownInline.Text) {
            merged[merged.lastIndex] = MarkdownInline.Text(last.text + node.text)
        } else {
            merged += node
        }
    }
    return merged
}

private fun fenceOpen(trimmed: String): String? = when {
    trimmed.startsWith(MARKDOWN_CODE_TICK.repeat(3)) -> MARKDOWN_CODE_TICK.repeat(3)
    trimmed.startsWith("~~~") -> "~~~"
    else -> null
}

private fun headingLevel(trimmed: String): Int {
    var level = 0
    while (level < trimmed.length && level < 6 && trimmed[level] == '#') level++
    if (level == 0) return 0
    if (level >= trimmed.length || trimmed[level] != ' ') return 0
    return level
}

private fun isRule(trimmed: String): Boolean {
    val compact = trimmed.replace(" ", "")
    return compact.length >= 3 &&
        (compact.all { it == '-' } || compact.all { it == '*' } || compact.all { it == '_' })
}

private fun bulletItem(trimmed: String): String? {
    if (trimmed.length < 2) return null
    val marker = trimmed[0]
    if (marker != '-' && marker != '*' && marker != '+') return null
    if (trimmed[1] != ' ') return null
    return trimmed.drop(2)
}

private fun orderedItem(trimmed: String): Int? {
    var digits = 0
    while (digits < trimmed.length && digits < 3 && trimmed[digits].isDigit()) digits++
    if (digits == 0 || digits >= trimmed.length) return null
    val separator = trimmed[digits]
    if (separator != '.' && separator != ')') return null
    if (digits + 1 >= trimmed.length || trimmed[digits + 1] != ' ') return null
    return trimmed.take(digits).toIntOrNull()
}

private fun orderedBody(trimmed: String): String {
    var digits = 0
    while (digits < trimmed.length && trimmed[digits].isDigit()) digits++
    return trimmed.drop(digits + 1).trimStart()
}

private fun capCode(code: String): String =
    if (code.length <= MarkdownLimits.MAX_CODE_BLOCK_CHARS) {
        code
    } else {
        code.take(MarkdownLimits.MAX_CODE_BLOCK_CHARS) + "\n…"
    }

private fun sanitizeInlineText(raw: String): String {
    val builder = StringBuilder(raw.length)
    for (character in raw) {
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
    return builder.toString()
}

private fun sanitizeMarkdownSource(raw: String): String {
    val limited = if (raw.length > MarkdownLimits.MAX_INPUT_CHARS) {
        raw.take(MarkdownLimits.MAX_INPUT_CHARS)
    } else {
        raw
    }
    return sanitizeInlineText(limited.replace("\r\n", "\n").replace('\r', '\n'))
}

private fun flattenPlainText(blocks: List<MarkdownBlock>): String {
    fun flatten(inlines: List<MarkdownInline>): String = inlines.joinToString("") { inline ->
        when (inline) {
            is MarkdownInline.Text -> inline.text
            is MarkdownInline.Strong -> flatten(inline.children)
            is MarkdownInline.Emphasis -> flatten(inline.children)
            is MarkdownInline.Strikethrough -> flatten(inline.children)
            is MarkdownInline.Code -> inline.text
            is MarkdownInline.Link -> flatten(inline.children)
            is MarkdownInline.BlockedLink -> flatten(inline.children)
        }
    }

    return blocks.joinToString("\n") { block ->
        when (block) {
            is MarkdownBlock.Paragraph -> block.lines.joinToString("\n") { flatten(it) }
            is MarkdownBlock.Heading -> flatten(block.inlines)
            is MarkdownBlock.BulletList -> block.items.joinToString("\n") { "• " + flatten(it) }
            is MarkdownBlock.OrderedList -> block.items.withIndex().joinToString("\n") { indexed ->
                (block.start + indexed.index).toString() + ". " + flatten(indexed.value)
            }
            is MarkdownBlock.Quote -> block.lines.joinToString("\n") { "> " + flatten(it) }
            is MarkdownBlock.CodeBlock -> block.code
            is MarkdownBlock.Rule -> "—"
        }
    }
}
