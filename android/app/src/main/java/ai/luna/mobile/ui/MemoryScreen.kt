package ai.luna.mobile.ui

import ai.luna.mobile.MemoryDto
import ai.luna.mobile.NoteDto
import ai.luna.mobile.theme.LunaTheme
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MemoryScreen(
    memories: List<MemoryDto>,
    notes: List<NoteDto>,
    onSearch: (String) -> Unit,
) {
    val colors = LunaTheme.colors
    var query by remember { mutableStateOf("") }
    var showNotes by remember { mutableStateOf(false) }
    var selectedNote by remember { mutableStateOf<NoteDto?>(null) }

    selectedNote?.let { note ->
        NoteReaderSheet(
            note = note,
            onDismiss = { selectedNote = null },
        )
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(colors.canvas),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            OutlinedTextField(
                value = query,
                onValueChange = {
                    query = it
                    onSearch(it)
                },
                modifier = Modifier.fillMaxWidth(),
                placeholder = { Text("Search memories & notes...", color = colors.inkMuted) },
                singleLine = true,
                leadingIcon = { Icon(Icons.Default.Search, contentDescription = null, tint = colors.inkMuted) },
                trailingIcon = {
                    if (query.isNotEmpty()) {
                        IconButton(
                            onClick = {
                                query = ""
                                onSearch("")
                            },
                            modifier = Modifier.size(48.dp),
                        ) {
                            Icon(Icons.Default.Close, contentDescription = "Clear search", tint = colors.inkSecondary)
                        }
                    }
                },
                shape = RoundedCornerShape(14.dp),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedContainerColor = colors.surfaceLow,
                    unfocusedContainerColor = colors.surfaceLow,
                    focusedBorderColor = colors.coral,
                    unfocusedBorderColor = colors.outline,
                ),
            )

            SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
                SegmentedButton(
                    selected = !showNotes,
                    onClick = { showNotes = false },
                    shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2),
                    colors = SegmentedButtonDefaults.colors(
                        activeContainerColor = colors.coralContainer,
                        activeContentColor = colors.coral,
                        inactiveContainerColor = colors.surfaceLow,
                        inactiveContentColor = colors.inkSecondary,
                    ),
                ) {
                    Text(
                        "Memories (${memories.size})",
                        style = MaterialTheme.typography.labelLarge.copy(fontFeatureSettings = "tnum"),
                    )
                }
                SegmentedButton(
                    selected = showNotes,
                    onClick = { showNotes = true },
                    shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
                    colors = SegmentedButtonDefaults.colors(
                        activeContainerColor = colors.coralContainer,
                        activeContentColor = colors.coral,
                        inactiveContainerColor = colors.surfaceLow,
                        inactiveContentColor = colors.inkSecondary,
                    ),
                ) {
                    Text(
                        "Notes (${notes.size})",
                        style = MaterialTheme.typography.labelLarge.copy(fontFeatureSettings = "tnum"),
                    )
                }
            }
        }

        val empty = if (showNotes) notes.isEmpty() else memories.isEmpty()
        if (empty) {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(32.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Icon(
                    imageVector = if (showNotes) Icons.Default.Description else Icons.Default.Psychology,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(40.dp),
                )
                Spacer(Modifier.height(12.dp))
                Text(
                    text = when {
                        query.isNotBlank() -> "No matching results"
                        showNotes -> "No notes yet"
                        else -> "No memories yet"
                    },
                    style = MaterialTheme.typography.titleMedium.copy(color = colors.ink),
                )
                Spacer(Modifier.height(6.dp))
                Text(
                    text = if (showNotes) {
                        "Ask Luna to write a note or spec and it will be saved as Markdown here."
                    } else {
                        "As you work together, Luna saves facts, preferences, and rules to recall across sessions."
                    },
                    style = MaterialTheme.typography.bodyMedium.copy(color = colors.inkSecondary),
                )
            }
        } else {
            LazyColumn(
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 6.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                if (!showNotes) {
                    items(memories, key = { it.id.ifBlank { "${it.subject}.${it.predicate}.${it.object_}" } }) { m ->
                        MemoryCard(m)
                    }
                } else {
                    items(notes, key = { it.id }) { n ->
                        NoteCard(note = n, onClick = { selectedNote = n })
                    }
                }
            }
        }
    }
}

@Composable
private fun MemoryCard(memory: MemoryDto) {
    val colors = LunaTheme.colors
    Surface(
        color = colors.surfaceLow,
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(1.dp, colors.hairline),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(14.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    text = "${memory.subject}.${memory.predicate}",
                    style = MaterialTheme.typography.labelMedium.copy(
                        fontFamily = FontFamily.Monospace,
                        color = colors.celestial,
                    ),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                StatusPill(
                    label = memory.category.replace('_', ' '),
                    foreground = colors.inkSecondary,
                    background = colors.surface,
                )
            }
            Spacer(Modifier.height(6.dp))
            Text(
                text = memory.object_,
                style = MaterialTheme.typography.bodyMedium.copy(color = colors.ink),
            )
        }
    }
}

@Composable
private fun NoteCard(
    note: NoteDto,
    onClick: () -> Unit,
) {
    val colors = LunaTheme.colors
    Surface(
        onClick = onClick,
        color = colors.surfaceLow,
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(1.dp, colors.hairline),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(14.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    text = note.title,
                    style = MaterialTheme.typography.titleSmall.copy(
                        color = colors.ink,
                        fontWeight = FontWeight.SemiBold,
                    ),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                if (note.pointer.isNotBlank()) {
                    StatusPill(
                        label = note.pointer,
                        foreground = colors.noteAccent,
                        background = colors.surface,
                    )
                }
            }
            if (note.content.isNotBlank()) {
                Spacer(Modifier.height(6.dp))
                Text(
                    text = note.content.trim().take(240),
                    style = MaterialTheme.typography.bodySmall.copy(color = colors.inkSecondary),
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NoteReaderSheet(
    note: NoteDto,
    onDismiss: () -> Unit,
) {
    val colors = LunaTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val clipboard = LocalClipboardManager.current
    var copied by remember { mutableStateOf(false) }

    LaunchedEffect(copied) {
        if (copied) {
            delay(1500)
            copied = false
        }
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = colors.surfaceLow,
        contentColor = colors.ink,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp)
                .padding(bottom = 28.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = note.title,
                        style = MaterialTheme.typography.titleLarge.copy(color = colors.ink),
                    )
                    if (note.pointer.isNotBlank()) {
                        Spacer(Modifier.height(4.dp))
                        Text(
                            text = note.pointer,
                            style = MaterialTheme.typography.labelMedium.copy(color = colors.noteAccent),
                        )
                    }
                }
                IconButton(
                    onClick = {
                        clipboard.setText(AnnotatedString(note.content))
                        copied = true
                    },
                    modifier = Modifier.size(48.dp),
                ) {
                    Icon(
                        imageVector = if (copied) Icons.Default.Check else Icons.Default.ContentCopy,
                        contentDescription = "Copy note",
                        tint = if (copied) colors.ok else colors.inkSecondary,
                    )
                }
            }
            HorizontalDivider(
                modifier = Modifier.padding(vertical = 12.dp),
                color = colors.hairline,
            )
            Column(modifier = Modifier.verticalScroll(rememberScrollState())) {
                MarkdownContent(markdown = note.content)
            }
        }
    }
}
