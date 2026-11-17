package ai.luna.mobile.ui

import ai.luna.mobile.theme.LunaColors
import ai.luna.mobile.theme.LunaTheme
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import org.commonmark.ext.gfm.strikethrough.Strikethrough
import org.commonmark.ext.gfm.strikethrough.StrikethroughExtension
import org.commonmark.ext.gfm.tables.TableBlock
import org.commonmark.ext.gfm.tables.TableBody
import org.commonmark.ext.gfm.tables.TableCell
import org.commonmark.ext.gfm.tables.TableHead
import org.commonmark.ext.gfm.tables.TableRow
import org.commonmark.ext.gfm.tables.TablesExtension
import org.commonmark.node.BlockQuote
import org.commonmark.node.BulletList
import org.commonmark.node.Code
import org.commonmark.node.Emphasis
import org.commonmark.node.FencedCodeBlock
import org.commonmark.node.HardLineBreak
import org.commonmark.node.Heading
import org.commonmark.node.HtmlBlock
import org.commonmark.node.HtmlInline
import org.commonmark.node.Image
import org.commonmark.node.IndentedCodeBlock
import org.commonmark.node.Link
import org.commonmark.node.ListBlock
import org.commonmark.node.ListItem
import org.commonmark.node.Node
import org.commonmark.node.OrderedList
import org.commonmark.node.Paragraph
import org.commonmark.node.SoftLineBreak
import org.commonmark.node.StrongEmphasis
import org.commonmark.node.Text
import org.commonmark.node.ThematicBreak
import org.commonmark.parser.Parser

sealed interface MdBlock {
    data class Para(val text: AnnotatedString) : MdBlock
    data class HeadingBlock(val level: Int, val text: AnnotatedString) : MdBlock
    data class CodeBlock(val language: String, val code: String) : MdBlock
    data class Quote(val blocks: List<MdBlock>) : MdBlock
    data class ListSection(
        val ordered: Boolean,
        val startNumber: Int,
        val tight: Boolean,
        val items: List<List<MdBlock>>,
    ) : MdBlock
    data class Table(
        val headers: List<AnnotatedString>,
        val rows: List<List<AnnotatedString>>,
    ) : MdBlock
    data object Divider : MdBlock
}

private object MarkdownParser {
    private val parser: Parser = Parser.builder()
        .extensions(
            listOf(
                TablesExtension.create(),
                StrikethroughExtension.create(),
            ),
        )
        .build()

    fun parse(raw: String, colors: LunaColors): List<MdBlock> {
        val doc = parser.parse(raw)
        return collectBlocks(doc, colors)
    }

    fun parseStreaming(raw: String, colors: LunaColors): List<MdBlock> {
        // Close any open fenced code block so streaming previews render cleanly.
        val fenceCount = Regex("(?m)^```").findAll(raw).count()
        val closed = if (fenceCount % 2 == 1) "$raw\n```" else raw
        return parse(closed, colors)
    }

    private fun collectBlocks(parent: Node, colors: LunaColors): List<MdBlock> {
        val out = mutableListOf<MdBlock>()
        var node = parent.firstChild
        while (node != null) {
            when (node) {
                is Paragraph -> {
                    val text = renderInline(node, colors)
                    if (text.isNotEmpty()) out += MdBlock.Para(text)
                }
                is Heading -> {
                    out += MdBlock.HeadingBlock(node.level, renderInline(node, colors))
                }
                is FencedCodeBlock -> {
                    out += MdBlock.CodeBlock(
                        language = node.info?.trim().orEmpty(),
                        code = node.literal.trimEnd('\n'),
                    )
                }
                is IndentedCodeBlock -> {
                    out += MdBlock.CodeBlock(
                        language = "",
                        code = node.literal.trimEnd('\n'),
                    )
                }
                is BlockQuote -> {
                    out += MdBlock.Quote(collectBlocks(node, colors))
                }
                is BulletList -> {
                    out += MdBlock.ListSection(
                        ordered = false,
                        startNumber = 1,
                        tight = node.isTight,
                        items = collectListItems(node, colors),
                    )
                }
                is OrderedList -> {
                    out += MdBlock.ListSection(
                        ordered = true,
                        startNumber = node.markerStartNumber ?: 1,
                        tight = node.isTight,
                        items = collectListItems(node, colors),
                    )
                }
                is TableBlock -> {
                    out += parseTable(node, colors)
                }
                is ThematicBreak -> {
                    out += MdBlock.Divider
                }
                is HtmlBlock -> {
                    val trimmed = node.literal.trim()
                    if (trimmed.isNotEmpty()) {
                        out += MdBlock.Para(AnnotatedString(trimmed))
                    }
                }
            }
            node = node.next
        }
        return out
    }

    private fun collectListItems(list: ListBlock, colors: LunaColors): List<List<MdBlock>> {
        val items = mutableListOf<List<MdBlock>>()
        var child = list.firstChild
        while (child != null) {
            if (child is ListItem) {
                items += collectBlocks(child, colors)
            }
            child = child.next
        }
        return items
    }

    private fun parseTable(table: TableBlock, colors: LunaColors): MdBlock.Table {
        val headers = mutableListOf<AnnotatedString>()
        val rows = mutableListOf<List<AnnotatedString>>()
        var section = table.firstChild
        while (section != null) {
            var row = section.firstChild
            while (row != null) {
                if (row is TableRow) {
                    val cells = mutableListOf<AnnotatedString>()
                    var cell = row.firstChild
                    while (cell != null) {
                        if (cell is TableCell) {
                            cells += renderInline(cell, colors)
                        }
                        cell = cell.next
                    }
                    if (section is TableHead) {
                        if (headers.isEmpty()) headers += cells else rows += cells
                    } else if (section is TableBody) {
                        rows += cells
                    }
                }
                row = row.next
            }
            section = section.next
        }
        return MdBlock.Table(headers, rows)
    }

    private fun renderInline(parent: Node, colors: LunaColors): AnnotatedString = buildAnnotatedString {
        appendInlineChildren(parent, colors)
    }

    private fun AnnotatedString.Builder.appendInlineChildren(parent: Node, colors: LunaColors) {
        var child = parent.firstChild
        while (child != null) {
            when (child) {
                is Text -> append(child.literal)
                is SoftLineBreak -> append(' ')
                is HardLineBreak -> append('\n')
                is Code -> {
                    withStyle(
                        SpanStyle(
                            fontFamily = FontFamily.Monospace,
                            fontSize = 13.sp,
                            background = colors.codeBg,
                            color = colors.amber,
                        ),
                    ) {
                        append(" ${child.literal} ")
                    }
                }
                is StrongEmphasis -> {
                    withStyle(SpanStyle(fontWeight = FontWeight.SemiBold, color = colors.ink)) {
                        appendInlineChildren(child, colors)
                    }
                }
                is Emphasis -> {
                    withStyle(SpanStyle(fontStyle = FontStyle.Italic)) {
                        appendInlineChildren(child, colors)
                    }
                }
                is Strikethrough -> {
                    withStyle(SpanStyle(textDecoration = TextDecoration.LineThrough, color = colors.inkMuted)) {
                        appendInlineChildren(child, colors)
                    }
                }
                is Link -> {
                    val url = child.destination.orEmpty()
                    val linkStyle = TextLinkStyles(
                        style = SpanStyle(
                            color = colors.celestial,
                            textDecoration = TextDecoration.Underline,
                        ),
                    )
                    if (url.startsWith("http://") || url.startsWith("https://")) {
                        withLink(LinkAnnotation.Url(url, linkStyle)) {
                            appendInlineChildren(child, colors)
                        }
                    } else {
                        withStyle(SpanStyle(color = colors.celestial)) {
                            appendInlineChildren(child, colors)
                        }
                    }
                }
                is Image -> {
                    val alt = child.title ?: "[image]"
                    withStyle(SpanStyle(color = colors.inkSecondary, fontStyle = FontStyle.Italic)) {
                        append(alt)
                    }
                }
                is HtmlInline -> append(child.literal)
                else -> appendInlineChildren(child, colors)
            }
            child = child.next
        }
    }
}

@Composable
fun MarkdownContent(
    markdown: String,
    modifier: Modifier = Modifier,
    streaming: Boolean = false,
    textStyle: TextStyle = MaterialTheme.typography.bodyLarge.copy(color = LunaTheme.colors.ink),
) {
    val colors = LunaTheme.colors
    val blocks = if (streaming) {
        val parsed by produceState<List<MdBlock>>(emptyList(), markdown, colors) {
            value = withContext(Dispatchers.Default) {
                MarkdownParser.parseStreaming(markdown, colors)
            }
        }
        parsed
    } else {
        remember(markdown, colors) {
            MarkdownParser.parse(markdown, colors)
        }
    }

    SelectionContainer {
        MarkdownBlockColumn(blocks = blocks, textStyle = textStyle, modifier = modifier)
    }
}

@Composable
private fun MarkdownBlockColumn(
    blocks: List<MdBlock>,
    textStyle: TextStyle,
    modifier: Modifier = Modifier,
    spacingDp: Int = 10,
) {
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(spacingDp.dp),
    ) {
        blocks.forEach { block ->
            MarkdownBlockItem(block, textStyle)
        }
    }
}

@Composable
private fun MarkdownBlockItem(block: MdBlock, textStyle: TextStyle) {
    val colors = LunaTheme.colors
    when (block) {
        is MdBlock.Para -> {
            Text(text = block.text, style = textStyle)
        }
        is MdBlock.HeadingBlock -> {
            val headingStyle = when (block.level) {
                1 -> MaterialTheme.typography.titleLarge.copy(color = colors.ink)
                2 -> MaterialTheme.typography.titleMedium.copy(color = colors.ink)
                else -> MaterialTheme.typography.titleSmall.copy(color = colors.ink)
            }
            Text(
                text = block.text,
                style = headingStyle,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
        is MdBlock.CodeBlock -> {
            CodeBlockCard(language = block.language, code = block.code)
        }
        is MdBlock.Quote -> {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(IntrinsicSize.Min),
            ) {
                Box(
                    modifier = Modifier
                        .width(3.dp)
                        .fillMaxHeight()
                        .clip(RoundedCornerShape(2.dp))
                        .background(colors.celestial.copy(alpha = 0.7f)),
                )
                MarkdownBlockColumn(
                    blocks = block.blocks,
                    textStyle = textStyle.copy(color = colors.inkSecondary),
                    modifier = Modifier.padding(start = 12.dp),
                    spacingDp = 6,
                )
            }
        }
        is MdBlock.ListSection -> {
            Column(verticalArrangement = Arrangement.spacedBy(if (block.tight) 4.dp else 8.dp)) {
                block.items.forEachIndexed { idx, itemBlocks ->
                    Row(modifier = Modifier.fillMaxWidth()) {
                        val marker = if (block.ordered) "${block.startNumber + idx}." else "•"
                        Text(
                            text = marker,
                            style = textStyle.copy(
                                color = colors.coral,
                                fontWeight = FontWeight.SemiBold,
                            ),
                            modifier = Modifier.width(if (block.ordered) 26.dp else 18.dp),
                        )
                        MarkdownBlockColumn(
                            blocks = itemBlocks,
                            textStyle = textStyle,
                            modifier = Modifier.weight(1f),
                            spacingDp = 4,
                        )
                    }
                }
            }
        }
        is MdBlock.Table -> {
            MarkdownTable(headers = block.headers, rows = block.rows, textStyle = textStyle)
        }
        is MdBlock.Divider -> {
            HorizontalDivider(
                modifier = Modifier.padding(vertical = 4.dp),
                color = colors.hairline,
            )
        }
    }
}

@Composable
private fun CodeBlockCard(language: String, code: String) {
    val colors = LunaTheme.colors
    val clipboard = LocalClipboardManager.current
    var copied by remember { mutableStateOf(false) }

    LaunchedEffect(copied) {
        if (copied) {
            delay(1600)
            copied = false
        }
    }

    Surface(
        color = colors.codeBg,
        shape = RoundedCornerShape(12.dp),
        border = androidx.compose.foundation.BorderStroke(1.dp, colors.hairline),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(colors.surfaceLow)
                    .padding(start = 12.dp, end = 4.dp, top = 2.dp, bottom = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = language.ifBlank { "code" }.uppercase(),
                    style = MaterialTheme.typography.labelSmall.copy(
                        fontFamily = FontFamily.Monospace,
                        color = colors.inkMuted,
                    ),
                    modifier = Modifier.weight(1f),
                )
                IconButton(
                    onClick = {
                        clipboard.setText(AnnotatedString(code))
                        copied = true
                    },
                    modifier = Modifier.size(36.dp),
                ) {
                    Icon(
                        imageVector = if (copied) Icons.Default.Check else Icons.Default.ContentCopy,
                        contentDescription = "Copy code",
                        tint = if (copied) colors.ok else colors.inkSecondary,
                        modifier = Modifier.size(16.dp),
                    )
                }
            }
            HorizontalDivider(color = colors.hairline)
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = 12.dp, vertical = 10.dp),
            ) {
                Text(
                    text = code,
                    style = MaterialTheme.typography.bodySmall.copy(
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.5.sp,
                        lineHeight = 18.sp,
                        color = colors.ink,
                    ),
                    softWrap = false,
                )
            }
        }
    }
}

@Composable
private fun MarkdownTable(
    headers: List<AnnotatedString>,
    rows: List<List<AnnotatedString>>,
    textStyle: TextStyle,
) {
    val colors = LunaTheme.colors
    val colCount = maxOf(headers.size, rows.maxOfOrNull { it.size } ?: 0)
    if (colCount == 0) return

    Surface(
        color = colors.surfaceLow,
        shape = RoundedCornerShape(10.dp),
        border = androidx.compose.foundation.BorderStroke(1.dp, colors.hairline),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.horizontalScroll(rememberScrollState())) {
            if (headers.isNotEmpty()) {
                Row(modifier = Modifier.background(colors.surface)) {
                    for (c in 0 until colCount) {
                        Text(
                            text = headers.getOrElse(c) { AnnotatedString("") },
                            style = textStyle.copy(
                                fontSize = 13.sp,
                                lineHeight = 18.sp,
                                fontWeight = FontWeight.SemiBold,
                                color = colors.ink,
                            ),
                            modifier = Modifier
                                .widthIn(min = 96.dp, max = 220.dp)
                                .padding(horizontal = 10.dp, vertical = 8.dp),
                        )
                    }
                }
                HorizontalDivider(color = colors.outline)
            }
            rows.forEachIndexed { rIdx, row ->
                Row {
                    for (c in 0 until colCount) {
                        Text(
                            text = row.getOrElse(c) { AnnotatedString("") },
                            style = textStyle.copy(
                                fontSize = 13.sp,
                                lineHeight = 18.sp,
                                color = colors.inkSecondary,
                            ),
                            modifier = Modifier
                                .widthIn(min = 96.dp, max = 220.dp)
                                .padding(horizontal = 10.dp, vertical = 8.dp),
                        )
                    }
                }
                if (rIdx < rows.lastIndex) {
                    HorizontalDivider(color = colors.hairline)
                }
            }
        }
    }
}
