package ai.luna.mobile.ui

import ai.luna.mobile.SessionInfo
import ai.luna.mobile.theme.LunaTheme
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.DeleteOutline
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

@Composable
fun SessionsScreen(
    sessions: List<SessionInfo>,
    activeSessionId: String,
    onSelect: (String) -> Unit,
    onDelete: (String) -> Unit,
    onNew: () -> Unit,
) {
    val colors = LunaTheme.colors
    var search by remember { mutableStateOf("") }
    var deleteTarget by remember { mutableStateOf<SessionInfo?>(null) }

    val filtered = remember(sessions, search) {
        val q = search.trim().lowercase()
        if (q.isEmpty()) sessions
        else sessions.filter {
            it.title.lowercase().contains(q) ||
                it.name.lowercase().contains(q) ||
                it.model.orEmpty().lowercase().contains(q) ||
                it.id.lowercase().contains(q)
        }
    }

    deleteTarget?.let { target ->
        val label = target.title.ifBlank { target.name.ifBlank { target.id } }
        ConfirmDialog(
            title = "Delete session?",
            body = "Remove \"$label\" and its message history from this device. This cannot be undone.",
            confirmLabel = "Delete",
            destructive = true,
            onConfirm = {
                deleteTarget = null
                onDelete(target.id)
            },
            onDismiss = { deleteTarget = null },
        )
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(colors.canvas),
    ) {
        Column(modifier = Modifier.fillMaxSize()) {
            if (sessions.size >= 4 || search.isNotBlank()) {
                OutlinedTextField(
                    value = search,
                    onValueChange = { search = it },
                    placeholder = { Text("Filter sessions...", color = colors.inkMuted) },
                    leadingIcon = { Icon(Icons.Default.Search, contentDescription = null, tint = colors.inkMuted) },
                    trailingIcon = {
                        if (search.isNotEmpty()) {
                            IconButton(onClick = { search = "" }, modifier = Modifier.size(48.dp)) {
                                Icon(Icons.Default.Close, contentDescription = "Clear search", tint = colors.inkSecondary)
                            }
                        }
                    },
                    singleLine = true,
                    shape = RoundedCornerShape(14.dp),
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedContainerColor = colors.surfaceLow,
                        unfocusedContainerColor = colors.surfaceLow,
                        focusedBorderColor = colors.coral,
                        unfocusedBorderColor = colors.outline,
                    ),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 10.dp),
                )
            }

            if (filtered.isEmpty()) {
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(32.dp),
                    verticalArrangement = Arrangement.Center,
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Icon(
                        imageVector = Icons.Default.History,
                        contentDescription = null,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(40.dp),
                    )
                    Spacer(Modifier.height(12.dp))
                    Text(
                        text = if (search.isBlank()) "No saved sessions yet" else "No matching sessions",
                        style = MaterialTheme.typography.titleMedium.copy(color = colors.ink),
                    )
                    Spacer(Modifier.height(4.dp))
                    Text(
                        text = "Start a new conversation to keep separate threads per task.",
                        style = MaterialTheme.typography.bodyMedium.copy(color = colors.inkSecondary),
                    )
                }
            } else {
                LazyColumn(
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 92.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    items(filtered, key = { it.id }) { session ->
                        val isActive = session.id == activeSessionId
                        SessionCard(
                            session = session,
                            isActive = isActive,
                            onSelect = { onSelect(session.id) },
                            onDelete = { deleteTarget = session },
                        )
                    }
                }
            }
        }

        ExtendedFloatingActionButton(
            onClick = onNew,
            icon = { Icon(Icons.Default.Add, contentDescription = null) },
            text = { Text("New Chat") },
            containerColor = colors.coral,
            contentColor = colors.onCoral,
            modifier = Modifier
                .align(Alignment.BottomEnd)
                .padding(16.dp),
        )
    }
}

@Composable
private fun SessionCard(
    session: SessionInfo,
    isActive: Boolean,
    onSelect: () -> Unit,
    onDelete: () -> Unit,
) {
    val colors = LunaTheme.colors
    val title = session.title.ifBlank { session.name.ifBlank { "Untitled session" } }

    Surface(
        onClick = onSelect,
        color = if (isActive) colors.surface else colors.surfaceLow,
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(
            width = 1.dp,
            color = if (isActive) colors.coral.copy(alpha = 0.55f) else colors.hairline,
        ),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(start = 14.dp, end = 6.dp, top = 12.dp, bottom = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier
                    .width(4.dp)
                    .height(38.dp)
                    .clip(RoundedCornerShape(2.dp))
                    .background(if (isActive) colors.coral else colors.outline),
            )
            Spacer(Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        text = title,
                        style = MaterialTheme.typography.titleSmall.copy(
                            color = colors.ink,
                            fontWeight = FontWeight.SemiBold,
                        ),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                    if (isActive) {
                        StatusPill(
                            label = "Active",
                            foreground = colors.coral,
                            background = colors.coralContainer,
                        )
                    }
                }
                Spacer(Modifier.height(4.dp))
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        text = "${session.messageCount} msgs",
                        style = MaterialTheme.typography.bodySmall.copy(
                            color = colors.inkSecondary,
                            fontFeatureSettings = "tnum",
                        ),
                    )
                    Text("·", style = MaterialTheme.typography.bodySmall.copy(color = colors.inkMuted))
                    Text(
                        text = listOfNotNull(session.provider, session.model)
                            .joinToString(" / ")
                            .ifBlank { session.id },
                        style = MaterialTheme.typography.bodySmall.copy(
                            fontFamily = if (session.model == null) FontFamily.Monospace else FontFamily.SansSerif,
                            color = colors.inkMuted,
                        ),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            IconButton(
                onClick = onDelete,
                modifier = Modifier.size(48.dp),
            ) {
                Icon(
                    imageVector = Icons.Default.DeleteOutline,
                    contentDescription = "Delete session",
                    tint = colors.inkMuted,
                )
            }
        }
    }
}
