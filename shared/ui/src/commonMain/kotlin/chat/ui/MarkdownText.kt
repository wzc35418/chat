package chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import chat.presentation.display.MarkdownBlock
import chat.presentation.display.MarkdownDocument
import chat.presentation.display.MarkdownInline
import chat.presentation.display.markdownPlainTextOrFallback

/**
 * 消息正文的 Markdown 渲染（安全子集）。
 *
 * 只绘制展示层给出的 [MarkdownDocument]：不存在 HTML、脚本或可执行扩展节点；
 * 链接一律不可点击（正式链接打开能力未接入），只显示文字与地址，危险协议显示停用原因。
 */
@Composable
fun MarkdownMessage(
    document: MarkdownDocument,
    modifier: Modifier = Modifier,
    textColor: Color = Color(0xFF1F2A24),
    mutedColor: Color = Color(0xFF5F6B63),
    accentColor: Color = Color(0xFF2F6B4F),
) {
    val codeBackground = Color(0xFFEDF1E9)
    Column(modifier, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        for (block in document.blocks) {
            when (block) {
                is MarkdownBlock.Paragraph -> Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    for (line in block.lines) {
                        Text(
                            text = buildMarkdownLine(line, textColor, mutedColor, accentColor, codeBackground),
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                }

                is MarkdownBlock.Heading -> Text(
                    text = buildMarkdownLine(block.inlines, textColor, mutedColor, accentColor, codeBackground),
                    style = when (block.level) {
                        1 -> MaterialTheme.typography.titleLarge
                        2 -> MaterialTheme.typography.titleMedium
                        3 -> MaterialTheme.typography.titleSmall
                        else -> MaterialTheme.typography.bodyLarge
                    },
                    fontWeight = FontWeight.SemiBold,
                )

                is MarkdownBlock.BulletList -> Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    for (item in block.items) {
                        Row(verticalAlignment = Alignment.Top) {
                            Text("• ", style = MaterialTheme.typography.bodyMedium, color = mutedColor)
                            Text(
                                text = buildMarkdownLine(item, textColor, mutedColor, accentColor, codeBackground),
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                    }
                }

                is MarkdownBlock.OrderedList -> Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    block.items.forEachIndexed { offset, item ->
                        Row(verticalAlignment = Alignment.Top) {
                            Text(
                                (block.start + offset).toString() + ". ",
                                style = MaterialTheme.typography.bodyMedium,
                                color = mutedColor,
                            )
                            Text(
                                text = buildMarkdownLine(item, textColor, mutedColor, accentColor, codeBackground),
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                    }
                }

                is MarkdownBlock.Quote -> Row(Modifier.fillMaxWidth()) {
                    Box(
                        Modifier
                            .width(3.dp)
                            .height(((block.lines.size.coerceAtLeast(1)) * 20).dp)
                            .background(Color(0xFFC9D4C7), RoundedCornerShape(2.dp)),
                    )
                    Column(Modifier.padding(start = 8.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                        for (line in block.lines) {
                            Text(
                                text = buildMarkdownLine(line, mutedColor, mutedColor, accentColor, codeBackground),
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                }

                is MarkdownBlock.CodeBlock -> Column(Modifier.fillMaxWidth()) {
                    block.language?.let { language ->
                        Text(
                            language,
                            style = MaterialTheme.typography.labelSmall,
                            color = mutedColor,
                        )
                    }
                    Box(
                        Modifier
                            .fillMaxWidth()
                            .background(codeBackground, RoundedCornerShape(8.dp))
                            .padding(horizontal = 10.dp, vertical = 8.dp)
                            .horizontalScroll(rememberScrollState()),
                    ) {
                        Text(
                            block.code,
                            style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                            color = textColor,
                        )
                    }
                }

                MarkdownBlock.Rule -> HorizontalDivider(color = Color(0xFFDCE3DA))
            }
        }
        if (document.truncated) {
            Text(
                "内容过长，已省略其余部分。",
                style = MaterialTheme.typography.labelSmall,
                color = mutedColor,
            )
        }
    }
}

/** 文本消息的统一入口：能解析 Markdown 就渲染，解析不出内容时退回纯文本。 */
@Composable
fun MarkdownMessageBody(
    source: String,
    modifier: Modifier = Modifier,
    textColor: Color = Color(0xFF1F2A24),
) {
    val document = androidx.compose.runtime.remember(source) { markdownPlainTextOrFallback(source) }
    MarkdownMessage(document = document, modifier = modifier, textColor = textColor)
}

private fun buildMarkdownLine(
    inlines: List<MarkdownInline>,
    textColor: Color,
    mutedColor: Color,
    accentColor: Color,
    codeBackground: Color,
): AnnotatedString = buildAnnotatedString {
    appendMarkdownInlines(inlines, textColor, mutedColor, accentColor, codeBackground)
}

private fun AnnotatedString.Builder.appendMarkdownInlines(
    inlines: List<MarkdownInline>,
    textColor: Color,
    mutedColor: Color,
    accentColor: Color,
    codeBackground: Color,
) {
    for (inline in inlines) {
        when (inline) {
            is MarkdownInline.Text -> append(inline.text)

            is MarkdownInline.Strong -> withStyle(SpanStyle(fontWeight = FontWeight.Bold)) {
                appendMarkdownInlines(inline.children, textColor, mutedColor, accentColor, codeBackground)
            }

            is MarkdownInline.Emphasis -> withStyle(
                SpanStyle(fontStyle = FontStyle.Italic),
            ) {
                appendMarkdownInlines(inline.children, textColor, mutedColor, accentColor, codeBackground)
            }

            is MarkdownInline.Strikethrough -> withStyle(
                SpanStyle(textDecoration = TextDecoration.LineThrough),
            ) {
                appendMarkdownInlines(inline.children, textColor, mutedColor, accentColor, codeBackground)
            }

            is MarkdownInline.Code -> withStyle(
                SpanStyle(fontFamily = FontFamily.Monospace, background = codeBackground),
            ) {
                append(inline.text)
            }

            is MarkdownInline.Link -> {
                // 只有展示层白名单（http/https/mailto）里的链接才会生成 LinkAnnotation，
                // 点击由 Compose 交给系统浏览器；危险协议在解析阶段已经降级为 BlockedLink。
                withLink(
                    LinkAnnotation.Url(
                        inline.url,
                        TextLinkStyles(
                            style = SpanStyle(
                                color = accentColor,
                                textDecoration = TextDecoration.Underline,
                                fontWeight = FontWeight.Medium,
                            ),
                        ),
                    ),
                ) {
                    appendMarkdownInlines(inline.children, textColor, mutedColor, accentColor, codeBackground)
                }
                withStyle(SpanStyle(color = mutedColor)) {
                    append(" (" + inline.url + ")")
                }
            }

            is MarkdownInline.BlockedLink -> {
                appendMarkdownInlines(inline.children, textColor, mutedColor, accentColor, codeBackground)
                withStyle(SpanStyle(color = mutedColor, textDecoration = TextDecoration.LineThrough)) {
                    append("（链接未启用：" + inline.reason + "）")
                }
            }
        }
    }
}
