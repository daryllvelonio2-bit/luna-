package ai.luna.mobile.ui

import ai.luna.mobile.ChatMessageDto
import ai.luna.mobile.ToolCallDto
import ai.luna.mobile.ToolResultDto
import ai.luna.mobile.theme.LunaTheme
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
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
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.ArrowDownward
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

data class LiveToolItem(
    val name: String,
    val arguments: String,
    val output: String = "",
    val done: Boolean = false,
    val isError: Boolean = false,
)

private data class PairedToolCall(
    val call: ToolCallDto,
    val result: ToolResultDto?,
)

private sealed interface ChatEntry {
    data class User(val id: String, val text: String) : ChatEntry
    data class Summary(val id: String, val text: String) : ChatEntry
    data class AssistantTurn(
        val id: String,
        val reasoningParts: List<String>,
        val tools: List<PairedToolCall>,
        val reply: String,
    ) : ChatEntry
}

private fun groupTranscript(messages: List<ChatMessageDto>): List<ChatEntry> {
    val entries = mutableListOf<ChatEntry>()
    var i = 0
    while (i < messages.size) {
        val msg = messages[i]
        when (msg.role) {
            "user" -> {
                entries += ChatEntry.User(id = "u_$i", text = msg.content)
                i++
            }
            "system" -> {
                if (msg.content.isNotBlank()) {
                    entries += ChatEntry.Summary(id = "s_$i", text = msg.content)
                }
                i++
            }
            "assistant" -> {
                val turnStart = i
                val reasonings = mutableListOf<String>()
                val toolCalls = mutableListOf<ToolCallDto>()
                val toolResultsById = mutableMapOf<String, ToolResultDto>()
                val replyParts = mutableListOf<String>()

                while (i < messages.size && (messages[i].role == "assistant" || messages[i].role == "tool")) {
                    val cur = messages[i]
                    if (cur.role == "assistant") {
                        cur.reasoning?.takeIf { it.isNotBlank() }?.let { reasonings += it }
                        toolCalls += cur.toolCalls
                        if (cur.content.isNotBlank()) {
                            replyParts += cur.content
                        }
                    } else if (cur.role == "tool") {
                        val tr = cur.toolResult ?: ToolResultDto(
                            callId = "tool_$i",
                            content = cur.content,
                            isError = false,
                        )
                        toolResultsById[tr.callId] = tr
                    }
                    i++
                }

                val paired = toolCalls.map { tc ->
                    PairedToolCall(call = tc, result = toolResultsById[tc.id])
                }
                val orphanResults = toolResultsById.values.filter { res ->
                    toolCalls.none { it.id == res.callId }
                }.map { res ->
                    PairedToolCall(
                        call = ToolCallDto(id = res.callId, name = "tool", arguments = ""),
                        result = res,
                    )
                }

                entries += ChatEntry.AssistantTurn(
                    id = "a_$turnStart",
                    reasoningParts = reasonings,
                    tools = paired + orphanResults,
                    reply = replyParts.joinToString("\n\n"),
                )
            }
            else -> {
                if (msg.content.isNotBlank()) {
                    entries += ChatEntry.AssistantTurn(
                        id = "o_$i",
                        reasoningParts = emptyList(),
                        tools = emptyList(),
                        reply = msg.content,
                    )
                }
                i++
            }
        }
    }
    return entries
}

@Composable
fun ChatScreen(
    messages: List<ChatMessageDto>,
    provider: String,
    model: String,
    hasConfiguredKey: Boolean,
    isRunning: Boolean,
    streamingContent: String,
    streamingReasoning: String,
    liveTools: List<LiveToolItem>,
    inputText: String,
    onInputChange: (String) -> Unit,
    onSend: (String?) -> Unit,
    onCancel: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val colors = LunaTheme.colors
    val entries = remember(messages) { groupTranscript(messages) }
    val listState = rememberLazyListState()
    val coroutineScope = rememberCoroutineScope()

    val showScrollToBottom by remember {
        derivedStateOf {
            val total = listState.layoutInfo.totalItemsCount
            if (total == 0) false
            else {
                val lastVisible = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
                lastVisible < total - 2
            }
        }
    }

    LaunchedEffect(entries.size, streamingContent.length, liveTools.size, isRunning) {
        val total = entries.size + if (isRunning) 1 else 0
        if (total > 0) {
            listState.animateScrollToItem(total - 1)
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(colors.canvas),
    ) {
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            if (entries.isEmpty() && !isRunning) {
                ChatEmptyState(
                    provider = provider,
                    model = model,
                    hasConfiguredKey = hasConfiguredKey,
                    onPickPrompt = { prompt ->
                        onSend(prompt)
                    },
                    onOpenSettings = onOpenSettings,
                )
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
                    verticalArrangement = Arrangement.spacedBy(18.dp),
                ) {
                    items(entries, key = { it.id }) { entry ->
                        when (entry) {
                            is ChatEntry.User -> UserMessageBubble(entry.text)
                            is ChatEntry.Summary -> CompactSummaryCard(entry.text)
                            is ChatEntry.AssistantTurn -> AssistantTurnView(entry)
                        }
                    }
                    if (isRunning) {
                        item(key = "live_turn") {
                            LiveAssistantTurn(
                                reasoning = streamingReasoning,
                                content = streamingContent,
                                tools = liveTools,
                            )
                        }
                    }
                }
            }

            if (showScrollToBottom && entries.isNotEmpty()) {
                SmallFloatingActionButton(
                    onClick = {
                        coroutineScope.launch {
                            val total = entries.size + if (isRunning) 1 else 0
                            if (total > 0) listState.animateScrollToItem(total - 1)
                        }
                    },
                    containerColor = colors.surfaceHigh,
                    contentColor = colors.ink,
                    shape = CircleShape,
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .padding(end = 16.dp, bottom = 12.dp)
                        .size(48.dp),
                ) {
                    Icon(
                        imageVector = Icons.Default.ArrowDownward,
                        contentDescription = "Jump to latest",
                        modifier = Modifier.size(20.dp),
                    )
                }
            }
        }

        ChatComposer(
            value = inputText,
            onValueChange = onInputChange,
            isRunning = isRunning,
            onSend = { onSend(null) },
            onCancel = onCancel,
        )
    }
}

@Composable
private fun ChatEmptyState(
    provider: String,
    model: String,
    hasConfiguredKey: Boolean,
    onPickPrompt: (String) -> Unit,
    onOpenSettings: () -> Unit,
) {
    val colors = LunaTheme.colors
    val starterPrompts = remember {
        listOf(
            "What tools and files can you access on my device?" to "Capabilities & workspace",
            "Save a note summarizing my top priorities today" to "Create a Markdown note",
            "Remember that I prefer concise answers with examples" to "Save a memory preference",
        )
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(horizontal = 24.dp, vertical = 20.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Surface(
            color = colors.coralContainer,
            shape = RoundedCornerShape(20.dp),
            modifier = Modifier.size(64.dp),
        ) {
            Box(contentAlignment = Alignment.Center) {
                LunaCrescentMark(size = 34.dp, tint = colors.coral)
            }
        }
        Spacer(Modifier.height(16.dp))
        Text(
            text = "Ready when you are",
            style = MaterialTheme.typography.headlineSmall.copy(color = colors.ink),
        )
        Spacer(Modifier.height(6.dp))
        Text(
            text = if (hasConfiguredKey) {
                "Running on $provider · $model"
            } else {
                "Connect an AI provider key to start chatting locally on your phone."
            },
            style = MaterialTheme.typography.bodyMedium.copy(color = colors.inkSecondary),
        )
        Spacer(Modifier.height(20.dp))

        if (!hasConfiguredKey) {
            Button(
                onClick = onOpenSettings,
                colors = ButtonDefaults.buttonColors(
                    containerColor = colors.coral,
                    contentColor = colors.onCoral,
                ),
                shape = RoundedCornerShape(14.dp),
                modifier = Modifier.height(48.dp),
            ) {
                Icon(Icons.Default.Key, contentDescription = null, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(8.dp))
                Text("Configure API Key")
            }
        } else {
            Column(
                modifier = Modifier.fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                starterPrompts.forEach { (prompt, subtitle) ->
                    Surface(
                        onClick = { onPickPrompt(prompt) },
                        color = colors.surfaceLow,
                        shape = RoundedCornerShape(14.dp),
                        border = BorderStroke(1.dp, colors.hairline),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(modifier = Modifier.padding(horizontal = 14.dp, vertical = 12.dp)) {
                            Text(
                                text = prompt,
                                style = MaterialTheme.typography.bodyMedium.copy(
                                    color = colors.ink,
                                    fontWeight = FontWeight.Medium,
                                ),
                            )
                            Spacer(Modifier.height(2.dp))
                            Text(
                                text = subtitle,
                                style = MaterialTheme.typography.bodySmall.copy(color = colors.inkMuted),
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun UserMessageBubble(text: String) {
    val colors = LunaTheme.colors
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.End,
    ) {
        Surface(
            color = colors.surface,
            shape = RoundedCornerShape(20.dp, 20.dp, 6.dp, 20.dp),
            border = BorderStroke(1.dp, colors.hairline),
            modifier = Modifier.widthIn(max = 310.dp),
        ) {
            SelectionContainer {
                Text(
                    text = text,
                    style = MaterialTheme.typography.bodyLarge.copy(color = colors.ink),
                    modifier = Modifier.padding(horizontal = 15.dp, vertical = 11.dp),
                )
            }
        }
    }
}

@Composable
private fun CompactSummaryCard(text: String) {
    val colors = LunaTheme.colors
    var expanded by remember { mutableStateOf(false) }
    Surface(
        onClick = { expanded = !expanded },
        color = colors.surfaceLow,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, colors.hairline),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                StatusPill(
                    label = "Compacted summary",
                    foreground = colors.amber,
                    background = colors.amberContainer,
                )
                Spacer(Modifier.weight(1f))
                Icon(
                    imageVector = if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore,
                    contentDescription = if (expanded) "Collapse summary" else "Expand summary",
                    tint = colors.inkMuted,
                    modifier = Modifier.size(18.dp),
                )
            }
            if (expanded) {
                Spacer(Modifier.height(8.dp))
                MarkdownContent(
                    markdown = text,
                    textStyle = MaterialTheme.typography.bodySmall.copy(color = colors.inkSecondary),
                )
            }
        }
    }
}

@Composable
private fun AssistantTurnView(turn: ChatEntry.AssistantTurn) {
    val colors = LunaTheme.colors
    val clipboard = LocalClipboardManager.current
    val reduceMotion = LunaTheme.reduceMotion
    var copied by remember { mutableStateOf(false) }

    LaunchedEffect(copied) {
        if (copied) {
            delay(1500)
            copied = false
        }
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        if (turn.reasoningParts.isNotEmpty()) {
            ReasoningAccordion(
                reasoning = turn.reasoningParts.joinToString("\n\n"),
                reduceMotion = reduceMotion,
            )
        }

        if (turn.tools.isNotEmpty()) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                turn.tools.forEach { paired ->
                    ToolActivityRow(
                        name = paired.call.name,
                        arguments = paired.call.arguments,
                        output = paired.result?.content.orEmpty(),
                        done = paired.result != null,
                        isError = paired.result?.isError == true,
                        reduceMotion = reduceMotion,
                    )
                }
            }
        }

        if (turn.reply.isNotBlank()) {
            MarkdownContent(
                markdown = turn.reply,
                modifier = Modifier.fillMaxWidth(),
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.Start,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                IconButton(
                    onClick = {
                        clipboard.setText(AnnotatedString(turn.reply))
                        copied = true
                    },
                    modifier = Modifier.size(40.dp),
                ) {
                    Icon(
                        imageVector = if (copied) Icons.Default.Check else Icons.Default.ContentCopy,
                        contentDescription = "Copy response",
                        tint = if (copied) colors.ok else colors.inkMuted,
                        modifier = Modifier.size(16.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun LiveAssistantTurn(
    reasoning: String,
    content: String,
    tools: List<LiveToolItem>,
) {
    val colors = LunaTheme.colors
    val reduceMotion = LunaTheme.reduceMotion
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            CircularProgressIndicator(
                modifier = Modifier.size(14.dp),
                strokeWidth = 2.dp,
                color = colors.coral,
            )
            Text(
                text = if (tools.any { !it.done }) "Running tools..." else "Luna is thinking...",
                style = MaterialTheme.typography.labelMedium.copy(color = colors.coral),
            )
        }

        if (reasoning.isNotBlank()) {
            ReasoningAccordion(
                reasoning = reasoning,
                reduceMotion = reduceMotion,
                defaultExpanded = content.isBlank() && tools.isEmpty(),
            )
        }

        if (tools.isNotEmpty()) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                tools.forEach { tool ->
                    ToolActivityRow(
                        name = tool.name,
                        arguments = tool.arguments,
                        output = tool.output,
                        done = tool.done,
                        isError = tool.isError,
                        reduceMotion = reduceMotion,
                    )
                }
            }
        }

        if (content.isNotBlank()) {
            MarkdownContent(
                markdown = content,
                streaming = true,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

@Composable
private fun ReasoningAccordion(
    reasoning: String,
    reduceMotion: Boolean,
    defaultExpanded: Boolean = false,
) {
    val colors = LunaTheme.colors
    var expanded by remember { mutableStateOf(defaultExpanded) }

    Surface(
        onClick = { expanded = !expanded },
        color = colors.surfaceLow,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, colors.hairline),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    imageVector = Icons.Default.Psychology,
                    contentDescription = null,
                    tint = colors.celestial,
                    modifier = Modifier.size(16.dp),
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    text = "Reasoning",
                    style = MaterialTheme.typography.labelMedium.copy(color = colors.inkSecondary),
                    modifier = Modifier.weight(1f),
                )
                Icon(
                    imageVector = if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore,
                    contentDescription = if (expanded) "Hide reasoning" else "Show reasoning",
                    tint = colors.inkMuted,
                    modifier = Modifier.size(18.dp),
                )
            }
            AnimatedVisibility(
                visible = expanded,
                enter = if (reduceMotion) fadeIn(tween(0)) else fadeIn(tween(150)) + expandVertically(tween(180)),
                exit = if (reduceMotion) fadeOut(tween(0)) else fadeOut(tween(120)) + shrinkVertically(tween(150)),
            ) {
                Text(
                    text = reasoning.trim(),
                    style = MaterialTheme.typography.bodySmall.copy(color = colors.inkSecondary),
                    modifier = Modifier.padding(top = 8.dp),
                )
            }
        }
    }
}

@Composable
private fun ToolActivityRow(
    name: String,
    arguments: String,
    output: String,
    done: Boolean,
    isError: Boolean,
    reduceMotion: Boolean,
) {
    val colors = LunaTheme.colors
    var expanded by remember { mutableStateOf(false) }

    Surface(
        onClick = { if (output.isNotBlank() || arguments.isNotBlank()) expanded = !expanded },
        color = colors.surfaceLow,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(
            width = 1.dp,
            color = if (isError) colors.bad.copy(alpha = 0.45f) else colors.hairline,
        ),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                when {
                    !done -> CircularProgressIndicator(
                        modifier = Modifier.size(15.dp),
                        strokeWidth = 2.dp,
                        color = colors.celestial,
                    )
                    isError -> Icon(
                        imageVector = Icons.Default.Error,
                        contentDescription = "Tool failed",
                        tint = colors.bad,
                        modifier = Modifier.size(16.dp),
                    )
                    else -> Icon(
                        imageVector = Icons.Default.CheckCircle,
                        contentDescription = "Tool completed",
                        tint = colors.ok,
                        modifier = Modifier.size(16.dp),
                    )
                }
                Spacer(Modifier.width(8.dp))
                Icon(
                    imageVector = Icons.Default.Terminal,
                    contentDescription = null,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(14.dp),
                )
                Spacer(Modifier.width(6.dp))
                Text(
                    text = name,
                    style = MaterialTheme.typography.labelMedium.copy(
                        fontFamily = FontFamily.Monospace,
                        color = colors.ink,
                    ),
                )
                if (arguments.isNotBlank()) {
                    Spacer(Modifier.width(8.dp))
                    Text(
                        text = arguments.replace('\n', ' ').trim(),
                        style = MaterialTheme.typography.bodySmall.copy(
                            fontFamily = FontFamily.Monospace,
                            color = colors.inkMuted,
                        ),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                } else {
                    Spacer(Modifier.weight(1f))
                }
                if (output.isNotBlank() || arguments.isNotBlank()) {
                    Icon(
                        imageVector = if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore,
                        contentDescription = if (expanded) "Hide details" else "Show details",
                        tint = colors.inkMuted,
                        modifier = Modifier.size(18.dp),
                    )
                }
            }

            AnimatedVisibility(
                visible = expanded,
                enter = if (reduceMotion) fadeIn(tween(0)) else fadeIn(tween(150)) + expandVertically(tween(180)),
                exit = if (reduceMotion) fadeOut(tween(0)) else fadeOut(tween(120)) + shrinkVertically(tween(150)),
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 8.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    if (arguments.isNotBlank()) {
                        Box(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clip(RoundedCornerShape(8.dp))
                                .background(colors.codeBg)
                                .padding(8.dp),
                        ) {
                            Text(
                                text = arguments.take(800),
                                style = MaterialTheme.typography.bodySmall.copy(
                                    fontFamily = FontFamily.Monospace,
                                    fontSize = 11.sp,
                                    color = colors.celestial,
                                ),
                            )
                        }
                    }
                    if (output.isNotBlank()) {
                        SelectionContainer {
                            Box(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .clip(RoundedCornerShape(8.dp))
                                    .background(colors.codeBg)
                                    .padding(8.dp),
                            ) {
                                Text(
                                    text = output.take(2000),
                                    style = MaterialTheme.typography.bodySmall.copy(
                                        fontFamily = FontFamily.Monospace,
                                        fontSize = 11.sp,
                                        color = if (isError) colors.bad else colors.inkSecondary,
                                    ),
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ChatComposer(
    value: String,
    onValueChange: (String) -> Unit,
    isRunning: Boolean,
    onSend: () -> Unit,
    onCancel: () -> Unit,
) {
    val colors = LunaTheme.colors
    Surface(
        color = colors.canvas,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Surface(
            color = colors.surfaceLow,
            shape = RoundedCornerShape(26.dp),
            border = BorderStroke(1.dp, colors.outline),
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 12.dp, vertical = 8.dp),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = 8.dp, end = 6.dp, top = 4.dp, bottom = 4.dp),
                verticalAlignment = Alignment.Bottom,
            ) {
                TextField(
                    value = value,
                    onValueChange = onValueChange,
                    modifier = Modifier.weight(1f),
                    placeholder = {
                        Text(
                            "Message Luna...",
                            style = MaterialTheme.typography.bodyLarge.copy(color = colors.inkMuted),
                        )
                    },
                    textStyle = MaterialTheme.typography.bodyLarge.copy(color = colors.ink),
                    maxLines = 5,
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.Sentences,
                        imeAction = ImeAction.Default,
                    ),
                    keyboardActions = KeyboardActions.Default,
                    colors = TextFieldDefaults.colors(
                        focusedContainerColor = Color.Transparent,
                        unfocusedContainerColor = Color.Transparent,
                        disabledContainerColor = Color.Transparent,
                        focusedIndicatorColor = Color.Transparent,
                        unfocusedIndicatorColor = Color.Transparent,
                        cursorColor = colors.coral,
                    ),
                )
                Spacer(Modifier.width(4.dp))
                if (isRunning) {
                    FilledIconButton(
                        onClick = onCancel,
                        colors = IconButtonDefaults.filledIconButtonColors(
                            containerColor = colors.bad,
                            contentColor = Color.White,
                        ),
                        modifier = Modifier
                            .padding(bottom = 3.dp)
                            .size(48.dp),
                    ) {
                        Icon(Icons.Default.Stop, contentDescription = "Stop generating")
                    }
                } else {
                    val canSend = value.isNotBlank()
                    FilledIconButton(
                        onClick = onSend,
                        enabled = canSend,
                        colors = IconButtonDefaults.filledIconButtonColors(
                            containerColor = colors.coral,
                            contentColor = colors.onCoral,
                            disabledContainerColor = colors.surfaceHigh,
                            disabledContentColor = colors.inkMuted,
                        ),
                        modifier = Modifier
                            .padding(bottom = 3.dp)
                            .size(48.dp),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send message")
                    }
                }
            }
        }
    }
}
