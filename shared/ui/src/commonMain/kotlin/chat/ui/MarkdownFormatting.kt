package chat.ui

import androidx.compose.ui.text.TextRange

/** 一次 Markdown 行内格式化后的纯文本与选区（**纯函数，便于单测**）。 */
data class MarkdownEdit(val text: String, val selection: TextRange)

/**
 * Markdown 行内格式化的**纯逻辑**（不含 UI）：把选区包上标记，或若已被同一标记包裹则取消。
 *
 * 语义细节：
 * - 空选区：插入成对标记并把光标放到中间（`**|**`），用户直接接着打字；
 * - 选区首尾有空白：把空白留在标记外面（`a **b** c` 而不是 `a** b **c`）；
 * - **toggle**：选区两侧已有完全相同的标记 → 去掉（再次点按钮就是取消）；
 * - 跨行选区：按 Markdown 语义**逐行**分别包裹（每行独立成 `**…**`），不产生一个跨行强调；
 * - 代码块不处理（` ``` ` 内部由调用方决定是否禁用；本函数不猜测上下文）。
 */
object MarkdownFormatting {

    /** 加粗：`**…**` */
    fun toggleBold(text: String, selection: TextRange): MarkdownEdit = toggleInline(text, selection, "**")

    /** 斜体：`*…*` */
    fun toggleItalic(text: String, selection: TextRange): MarkdownEdit = toggleInline(text, selection, "*")

    /** 删除线：`~~…~~` */
    fun toggleStrike(text: String, selection: TextRange): MarkdownEdit = toggleInline(text, selection, "~~")

    /** 行内代码：`` `…` `` */
    fun toggleCode(text: String, selection: TextRange): MarkdownEdit = toggleInline(text, selection, "`")

    fun toggleInline(text: String, selection: TextRange, marker: String): MarkdownEdit {
        require(marker.isNotEmpty()) { "marker must not be empty" }
        val start = selection.min.coerceIn(0, text.length)
        val end = selection.max.coerceIn(0, text.length)

        // 空选区：插入成对标记，光标落在中间。
        if (start == end) {
            val inserted = text.substring(0, start) + marker + marker + text.substring(start)
            return MarkdownEdit(inserted, TextRange(start + marker.length))
        }

        val selected = text.substring(start, end)
        val before = text.substring(0, start)
        val after = text.substring(end)

        // toggle：选区两侧已有同一标记 → 去掉。
        if (before.endsWith(marker) && after.startsWith(marker)) {
            val newText = before.removeSuffix(marker) + selected + after.removePrefix(marker)
            val newStart = start - marker.length
            return MarkdownEdit(newText, TextRange(newStart, newStart + selected.length))
        }

        // 跨行：逐行包裹，行间换行保持不变。
        val lines = selected.split("\n")
        if (lines.size > 1) {
            val wrapped = lines.joinToString("\n") { line -> wrapLine(line, marker) }
            val newText = before + wrapped + after
            return MarkdownEdit(newText, TextRange(start, start + wrapped.length))
        }

        val wrapped = wrapLine(selected, marker)
        val newText = before + wrapped + after
        val newStart = start + (wrapped.length - selected.length) / 2
        return MarkdownEdit(newText, TextRange(newStart, newStart + selected.length))
    }

    /** 行内包裹：把首尾空白留在标记外；整行空白则原样返回。 */
    private fun wrapLine(line: String, marker: String): String {
        if (line.isBlank()) return line
        val leading = line.takeWhile { it == ' ' || it == '\t' }
        val trailing = line.takeLastWhile { it == ' ' || it == '\t' }
        val core = line.substring(leading.length, line.length - trailing.length)
        if (core.isEmpty()) return line
        return leading + marker + core + marker + trailing
    }
}
