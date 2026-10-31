package net.grassline.app

import androidx.compose.foundation.border
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.dp
import com.mikepenz.markdown.compose.components.MarkdownComponentModel
import com.mikepenz.markdown.compose.components.markdownComponents
import com.mikepenz.markdown.utils.buildMarkdownAnnotatedString
import org.intellij.markdown.ast.ASTNode
import org.intellij.markdown.flavours.gfm.GFMElementTypes
import org.intellij.markdown.flavours.gfm.GFMTokenTypes

/** The markdown renderer's parts, with tables that show all of their text. */
val chatMarkdown = markdownComponents(table = { MarkdownTable(it) })

/**
 * A table whose columns are as wide as their text, up to a point past which
 * it wraps; wider than the screen, it scrolls sideways. The renderer's own
 * cuts every cell to one line of a fixed width.
 */
@Composable
fun ColumnScope.MarkdownTable(model: MarkdownComponentModel) {
    val rows = model.node.children.filter { it.type == GFMElementTypes.HEADER || it.type == GFMElementTypes.ROW }
    val cells = rows.map { row -> row.children.filter { it.type == GFMTokenTypes.CELL } }
    val columns = cells.maxOfOrNull { it.size } ?: return
    val line = MaterialTheme.colorScheme.outlineVariant
    val text = model.typography.text
    Layout(
        content = {
            cells.forEachIndexed { r, row ->
                for (c in 0 until columns) {
                    val header = rows[r].type == GFMElementTypes.HEADER
                    Text(
                        row.getOrNull(c)?.let { cellText(model.content, it) } ?: AnnotatedString(""),
                        Modifier
                            .then(if (header) Modifier.background(MaterialTheme.colorScheme.surfaceVariant) else Modifier)
                            .border(0.5.dp, line)
                            .padding(horizontal = 8.dp, vertical = 6.dp),
                        style = if (header) text.copy(fontWeight = FontWeight.Bold) else text,
                    )
                }
            }
        },
        modifier = Modifier.padding(vertical = 4.dp).horizontalScroll(rememberScrollState()).border(0.5.dp, line),
    ) { measurables, _ ->
        val cap = 240.dp.roundToPx()
        val widths = IntArray(columns) { c ->
            (cells.indices).maxOf { r -> measurables[r * columns + c].maxIntrinsicWidth(Constraints.Infinity) }.coerceAtMost(cap)
        }
        val heights = IntArray(cells.size) { r ->
            (0 until columns).maxOf { c -> measurables[r * columns + c].minIntrinsicHeight(widths[c]) }
        }
        val placeables = measurables.mapIndexed { i, m -> m.measure(Constraints.fixed(widths[i % columns], heights[i / columns])) }
        layout(widths.sum(), heights.sum()) {
            var y = 0
            for (r in cells.indices) {
                var x = 0
                for (c in 0 until columns) {
                    placeables[r * columns + c].place(x, y)
                    x += widths[c]
                }
                y += heights[r]
            }
        }
    }
}

/** A cell's inline markdown, without the spaces around it. */
@Composable
private fun cellText(content: String, cell: ASTNode): AnnotatedString {
    val s = buildAnnotatedString { buildMarkdownAnnotatedString(content, cell) }
    val start = s.indexOfFirst { !it.isWhitespace() }
    if (start < 0) return AnnotatedString("")
    return s.subSequence(start, s.indexOfLast { !it.isWhitespace() } + 1)
}
