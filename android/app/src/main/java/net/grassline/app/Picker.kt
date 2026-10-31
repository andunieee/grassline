package net.grassline.app

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/** An entry of a [PickerList]. */
data class PickerItem(val key: String, val title: String, val detail: String? = null, val selected: Boolean = false)

/** Every word typed must appear in the text. */
fun matches(query: String, text: String): Boolean {
    val t = text.lowercase()
    return query.lowercase().split(' ').filter { it.isNotBlank() }.all { it in t }
}

/** The text field above a [PickerList] that narrows it. */
@Composable
fun FilterField(query: String, onQuery: (String) -> Unit) {
    OutlinedTextField(
        query, onQuery, Modifier.fillMaxWidth(),
        placeholder = { Text("Filter") },
        leadingIcon = { Icon(Icons.Filled.Search, null) },
        singleLine = true,
        shape = RoundedCornerShape(12.dp),
    )
}

/** The entries to pick one of, in a box of their own like an open select field. */
@Composable
fun PickerList(items: List<PickerItem>, modifier: Modifier = Modifier, onPick: (String) -> Unit) {
    Surface(
        modifier.fillMaxWidth(),
        shape = RoundedCornerShape(12.dp),
        color = Color.Transparent,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        if (items.isEmpty()) {
            Text(
                "Nothing matches",
                Modifier.padding(16.dp),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.outline,
            )
            return@Surface
        }
        LazyColumn(Modifier.heightIn(max = 360.dp)) {
            itemsIndexed(items, key = { _, it -> it.key }) { i, item ->
                if (i > 0) HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
                Row(
                    Modifier
                        .fillMaxWidth()
                        .then(if (item.selected) Modifier.background(MaterialTheme.colorScheme.secondaryContainer) else Modifier)
                        .clickable { onPick(item.key) }
                        .padding(horizontal = 14.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(item.title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        item.detail?.let {
                            Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        }
                    }
                    if (item.selected) Icon(Icons.Filled.Check, "Selected", Modifier.padding(start = 8.dp).size(18.dp), tint = MaterialTheme.colorScheme.primary)
                }
            }
        }
    }
}
