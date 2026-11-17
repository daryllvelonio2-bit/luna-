package ai.luna.mobile

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import java.io.File

private val jsonCodec = Json {
    ignoreUnknownKeys = true
    isLenient = true
}

// Note: MemoryDto uses `object` in Rust JSON; map `object` safely via SerialName if needed
data class LiveToolItem(
    val name: String,
    val arguments: String,
    val output: String = "",
    val done: Boolean = false,
    val isError: Boolean = false,
)

enum class AppTab(val label: String) {
    Chat("Chat"),
    Sessions("Sessions"),
    Memory("Memory"),
    Settings("Settings"),
}

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            LunaMobileTheme {
                LunaApp()
            }
        }
    }
}

@Composable
fun LunaMobileTheme(content: @Composable () -> Unit) {
    val colorScheme = darkColorScheme(
        primary = Color(0xFF7C9EFF),
        onPrimary = Color(0xFF0B1020),
        secondary = Color(0xFF8BE9FD),
        background = Color(0xFF0E1017),
        surface = Color(0xFF161923),
        surfaceVariant = Color(0xFF1F2433),
        onBackground = Color(0xFFE6EAF5),
        onSurface = Color(0xFFE6EAF5),
        onSurfaceVariant = Color(0xFFA0A8C0),
        error = Color(0xFFFF6B81),
    )
    MaterialTheme(
        colorScheme = colorScheme,
        content = content,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LunaApp() {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    var currentTab by remember { mutableStateOf(AppTab.Chat) }
    var initState by remember { mutableStateOf<InitResponse?>(null) }
    var initError by remember { mutableStateOf<String?>(null) }
    var messages by remember { mutableStateOf<List<ChatMessageDto>>(emptyList()) }
    var statusBanner by remember { mutableStateOf<String?>(null) }

    var inputText by remember { mutableStateOf("") }
    var isRunning by remember { mutableStateOf(false) }
    var streamingContent by remember { mutableStateOf("") }
    var streamingReasoning by remember { mutableStateOf("") }
    var liveTools by remember { mutableStateOf<List<LiveToolItem>>(emptyList()) }
    var tokensUsed by remember { mutableStateOf(0) }

    var pendingApproval by remember { mutableStateOf<Pair<ApprovalPayload, CompletableDeferred<Int>>?>(null) }

    var memories by remember { mutableStateOf<List<MemoryDto>>(emptyList()) }
    var notes by remember { mutableStateOf<List<NoteDto>>(emptyList()) }

    fun refreshMessages() {
        scope.launch {
            val res = withContext(Dispatchers.IO) {
                runCatching {
                    jsonCodec.decodeFromString<MessagesResponse>(LunaBridge.nativeGetMessages())
                }
            }
            res.onSuccess {
                if (it.ok) messages = it.messages
            }
        }
    }

    fun refreshKnowledge(query: String = "") {
        scope.launch {
            withContext(Dispatchers.IO) {
                runCatching {
                    val mem = jsonCodec.decodeFromString<MemoriesResponse>(
                        LunaBridge.nativeListMemories(query)
                    )
                    val note = jsonCodec.decodeFromString<NotesResponse>(
                        LunaBridge.nativeListNotes(query)
                    )
                    mem to note
                }
            }.onSuccess { (mem, note) ->
                if (mem.ok) memories = mem.memories
                if (note.ok) notes = note.notes
            }
        }
    }

    LaunchedEffect(Unit) {
        val result = withContext(Dispatchers.IO) {
            runCatching {
                val filesDir = context.filesDir.absolutePath
                val workspaceDir = File(context.filesDir, "workspace").apply { mkdirs() }.absolutePath
                jsonCodec.decodeFromString<InitResponse>(
                    LunaBridge.nativeInit(filesDir, workspaceDir)
                )
            }
        }
        result.onSuccess { init ->
            if (init.ok) {
                initState = init
                if (init.providers.none { it.hasKey }) {
                    currentTab = AppTab.Settings
                    statusBanner = "Add a provider API key in Settings to start chatting."
                }
                refreshMessages()
                refreshKnowledge()
            } else {
                initError = init.error ?: "Initialization failed"
            }
        }.onFailure {
            initError = it.message ?: "Failed to load native library"
        }
    }

    val callback = remember {
        object : LunaCallback {
            override fun onEvent(jsonPayload: String) {
                val ev = runCatching {
                    jsonCodec.decodeFromString<StreamEventDto>(jsonPayload)
                }.getOrNull() ?: return

                scope.launch(Dispatchers.Main) {
                    when (ev.type) {
                        "reasoning_delta" -> streamingReasoning += ev.text.orEmpty()
                        "content_delta" -> streamingContent += ev.text.orEmpty()
                        "tool_executing" -> {
                            liveTools = liveTools + LiveToolItem(
                                name = ev.name.orEmpty(),
                                arguments = ev.arguments.orEmpty(),
                            )
                        }
                        "tool_output" -> {
                            val idx = liveTools.indexOfLast { !it.done && it.name == ev.name }
                            if (idx >= 0) {
                                val list = liveTools.toMutableList()
                                list[idx] = list[idx].copy(output = list[idx].output + ev.chunk.orEmpty())
                                liveTools = list
                            }
                        }
                        "tool_executed" -> {
                            val idx = liveTools.indexOfLast { !it.done && it.name == ev.name }
                            if (idx >= 0) {
                                val list = liveTools.toMutableList()
                                list[idx] = list[idx].copy(
                                    output = ev.output.orEmpty(),
                                    done = true,
                                    isError = ev.isError == true,
                                )
                                liveTools = list
                            }
                        }
                        "context_tokens" -> {
                            tokensUsed = ev.tokensUsed ?: tokensUsed
                        }
                        "compaction_started" -> {
                            statusBanner = "Compacting conversation..."
                        }
                        "compaction_finished" -> {
                            statusBanner = ev.error?.let { "Compact: $it" } ?: "Compacted conversation."
                        }
                    }
                }
            }

            override fun requestApproval(requestJson: String): Int {
                val payload = runCatching {
                    jsonCodec.decodeFromString<ApprovalPayload>(requestJson)
                }.getOrDefault(ApprovalPayload(tool = "tool"))
                val deferred = CompletableDeferred<Int>()
                scope.launch(Dispatchers.Main) {
                    pendingApproval = payload to deferred
                }
                return runBlocking { deferred.await() }
            }
        }
    }

    fun sendCurrentPrompt() {
        val prompt = inputText.trim()
        if (prompt.isEmpty() || isRunning) return
        inputText = ""
        isRunning = true
        streamingContent = ""
        streamingReasoning = ""
        liveTools = emptyList()
        statusBanner = null
        messages = messages + ChatMessageDto(role = "user", content = prompt)

        scope.launch {
            val res = withContext(Dispatchers.IO) {
                runCatching {
                    jsonCodec.decodeFromString<SendResponse>(
                        LunaBridge.nativeSendMessage(prompt, callback)
                    )
                }
            }
            isRunning = false
            streamingContent = ""
            streamingReasoning = ""
            liveTools = emptyList()
            res.onSuccess {
                if (!it.ok) {
                    statusBanner = it.error ?: "Turn failed"
                }
            }.onFailure {
                statusBanner = it.message ?: "Turn error"
            }
            refreshMessages()
            refreshKnowledge()
        }
    }

    pendingApproval?.let { (req, deferred) ->
        AlertDialog(
            onDismissRequest = {
                pendingApproval = null
                deferred.complete(0)
            },
            title = { Text("Approve ${req.tool}?") },
            text = {
                Column(modifier = Modifier.verticalScroll(rememberScrollState())) {
                    req.target?.let {
                        Text(
                            text = it,
                            fontFamily = FontFamily.Monospace,
                            fontSize = 12.sp,
                            color = MaterialTheme.colorScheme.secondary,
                        )
                        Spacer(Modifier.height(8.dp))
                    }
                    req.diff?.let {
                        Text(
                            text = it.take(1200),
                            fontFamily = FontFamily.Monospace,
                            fontSize = 11.sp,
                        )
                    } ?: run {
                        Text(
                            text = req.arguments?.toString()?.take(600).orEmpty(),
                            fontFamily = FontFamily.Monospace,
                            fontSize = 11.sp,
                        )
                    }
                }
            },
            confirmButton = {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = {
                        pendingApproval = null
                        deferred.complete(2)
                    }) { Text("Session") }
                    Button(onClick = {
                        pendingApproval = null
                        deferred.complete(1)
                    }) { Text("Allow") }
                }
            },
            dismissButton = {
                TextButton(onClick = {
                    pendingApproval = null
                    deferred.complete(0)
                }) { Text("Deny", color = MaterialTheme.colorScheme.error) }
            },
        )
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Luna", fontWeight = FontWeight.Bold, fontSize = 18.sp)
                        initState?.let { st ->
                            val ctxPct = if (st.contextWindow > 0 && tokensUsed > 0) {
                                " · ${(tokensUsed * 100 / st.contextWindow)}%"
                            } else ""
                            Text(
                                "${st.provider} / ${st.model}$ctxPct",
                                fontSize = 11.sp,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                },
                actions = {
                    if (currentTab == AppTab.Chat && initState != null) {
                        IconButton(onClick = {
                            scope.launch {
                                withContext(Dispatchers.IO) {
                                    LunaBridge.nativeCompactSession(callback)
                                }
                                refreshMessages()
                            }
                        }) {
                            Icon(Icons.Default.Compress, contentDescription = "Compact")
                        }
                        IconButton(onClick = {
                            scope.launch {
                                val res = withContext(Dispatchers.IO) {
                                    runCatching {
                                        jsonCodec.decodeFromString<InitResponse>(
                                            LunaBridge.nativeNewSession()
                                        )
                                    }
                                }
                                res.onSuccess { if (it.ok) { initState = it; messages = emptyList(); tokensUsed = 0 } }
                            }
                        }) {
                            Icon(Icons.Default.AddComment, contentDescription = "New Chat")
                        }
                    }
                },
            )
        },
        bottomBar = {
            NavigationBar {
                NavigationBarItem(
                    selected = currentTab == AppTab.Chat,
                    onClick = { currentTab = AppTab.Chat },
                    icon = { Icon(Icons.Default.ChatBubbleOutline, contentDescription = "Chat") },
                    label = { Text("Chat") },
                )
                NavigationBarItem(
                    selected = currentTab == AppTab.Sessions,
                    onClick = { currentTab = AppTab.Sessions },
                    icon = { Icon(Icons.Default.History, contentDescription = "Sessions") },
                    label = { Text("Sessions") },
                )
                NavigationBarItem(
                    selected = currentTab == AppTab.Memory,
                    onClick = {
                        refreshKnowledge()
                        currentTab = AppTab.Memory
                    },
                    icon = { Icon(Icons.Default.Psychology, contentDescription = "Memory") },
                    label = { Text("Memory") },
                )
                NavigationBarItem(
                    selected = currentTab == AppTab.Settings,
                    onClick = { currentTab = AppTab.Settings },
                    icon = { Icon(Icons.Default.Settings, contentDescription = "Settings") },
                    label = { Text("Settings") },
                )
            }
        },
    ) { padding ->
        Box(modifier = Modifier.fillMaxSize().padding(padding)) {
            if (initError != null) {
                Column(
                    modifier = Modifier.fillMaxSize().padding(24.dp),
                    verticalArrangement = Arrangement.Center,
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text("Failed to initialize Luna", color = MaterialTheme.colorScheme.error, fontWeight = FontWeight.Bold)
                    Spacer(Modifier.height(8.dp))
                    Text(initError.orEmpty(), fontSize = 13.sp)
                }
            } else if (initState == null) {
                CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
            } else {
                when (currentTab) {
                    AppTab.Chat -> ChatScreen(
                        messages = messages,
                        isRunning = isRunning,
                        streamingContent = streamingContent,
                        streamingReasoning = streamingReasoning,
                        liveTools = liveTools,
                        statusBanner = statusBanner,
                        inputText = inputText,
                        onInputChange = { inputText = it },
                        onSend = ::sendCurrentPrompt,
                        onCancel = {
                            scope.launch(Dispatchers.IO) { LunaBridge.nativeCancelTurn() }
                        },
                        onDismissBanner = { statusBanner = null },
                    )

                    AppTab.Sessions -> SessionsScreen(
                        sessions = initState?.sessions.orEmpty(),
                        activeSessionId = initState?.sessionId.orEmpty(),
                        onSelect = { sid ->
                            scope.launch {
                                val res = withContext(Dispatchers.IO) {
                                    runCatching {
                                        jsonCodec.decodeFromString<InitResponse>(
                                            LunaBridge.nativeSelectSession(sid)
                                        )
                                    }
                                }
                                res.onSuccess {
                                    if (it.ok) {
                                        initState = it
                                        tokensUsed = 0
                                        refreshMessages()
                                        currentTab = AppTab.Chat
                                    }
                                }
                            }
                        },
                        onDelete = { sid ->
                            scope.launch {
                                val res = withContext(Dispatchers.IO) {
                                    runCatching {
                                        jsonCodec.decodeFromString<InitResponse>(
                                            LunaBridge.nativeDeleteSession(sid)
                                        )
                                    }
                                }
                                res.onSuccess {
                                    if (it.ok) {
                                        initState = it
                                        refreshMessages()
                                    }
                                }
                            }
                        },
                        onNew = {
                            scope.launch {
                                val res = withContext(Dispatchers.IO) {
                                    runCatching {
                                        jsonCodec.decodeFromString<InitResponse>(
                                            LunaBridge.nativeNewSession()
                                        )
                                    }
                                }
                                res.onSuccess {
                                    if (it.ok) {
                                        initState = it
                                        messages = emptyList()
                                        tokensUsed = 0
                                        currentTab = AppTab.Chat
                                    }
                                }
                            }
                        },
                    )

                    AppTab.Memory -> MemoryScreen(
                        memories = memories,
                        notes = notes,
                        onSearch = { q -> refreshKnowledge(q) },
                    )

                    AppTab.Settings -> SettingsScreen(
                        state = initState!!,
                        onUpdated = { updated ->
                            initState = updated
                            statusBanner = "Updated ${updated.provider} / ${updated.model}"
                        },
                        onError = { msg -> statusBanner = msg },
                    )
                }
            }
        }
    }
}

@Composable
fun ChatScreen(
    messages: List<ChatMessageDto>,
    isRunning: Boolean,
    streamingContent: String,
    streamingReasoning: String,
    liveTools: List<LiveToolItem>,
    statusBanner: String?,
    inputText: String,
    onInputChange: (String) -> Unit,
    onSend: () -> Unit,
    onCancel: () -> Unit,
    onDismissBanner: () -> Unit,
) {
    val listState = rememberLazyListState()

    LaunchedEffect(messages.size, streamingContent.length, liveTools.size) {
        val total = messages.size + if (isRunning) 1 else 0
        if (total > 0) {
            listState.animateScrollToItem(total - 1)
        }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        AnimatedVisibility(visible = statusBanner != null) {
            Surface(
                color = MaterialTheme.colorScheme.surfaceVariant,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
                shape = RoundedCornerShape(8.dp),
            ) {
                Row(
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        statusBanner.orEmpty(),
                        modifier = Modifier.weight(1f),
                        fontSize = 12.sp,
                        color = MaterialTheme.colorScheme.secondary,
                    )
                    IconButton(onClick = onDismissBanner, modifier = Modifier.size(20.dp)) {
                        Icon(Icons.Default.Close, contentDescription = "Dismiss", modifier = Modifier.size(16.dp))
                    }
                }
            }
        }

        LazyColumn(
            state = listState,
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (messages.isEmpty() && !isRunning) {
                item {
                    Box(
                        modifier = Modifier.fillParentMaxSize().padding(24.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            "Start a conversation with Luna.\nTools, memory, and notes run directly on your phone.",
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            fontSize = 14.sp,
                        )
                    }
                }
            }

            items(messages) { msg ->
                MessageCard(msg)
            }

            if (isRunning) {
                item {
                    LiveTurnCard(
                        reasoning = streamingReasoning,
                        content = streamingContent,
                        tools = liveTools,
                    )
                }
            }
        }

        Surface(
            tonalElevation = 3.dp,
            color = MaterialTheme.colorScheme.surface,
        ) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
                verticalAlignment = Alignment.Bottom,
            ) {
                OutlinedTextField(
                    value = inputText,
                    onValueChange = onInputChange,
                    modifier = Modifier.weight(1f),
                    placeholder = { Text("Ask Luna...") },
                    maxLines = 5,
                    shape = RoundedCornerShape(20.dp),
                )
                Spacer(Modifier.width(8.dp))
                if (isRunning) {
                    FilledIconButton(
                        onClick = onCancel,
                        colors = IconButtonDefaults.filledIconButtonColors(
                            containerColor = MaterialTheme.colorScheme.error,
                        ),
                    ) {
                        Icon(Icons.Default.Stop, contentDescription = "Stop")
                    }
                } else {
                    FilledIconButton(
                        onClick = onSend,
                        enabled = inputText.isNotBlank(),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send")
                    }
                }
            }
        }
    }
}

@Composable
fun MessageCard(msg: ChatMessageDto) {
    when (msg.role) {
        "user" -> {
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Surface(
                    color = MaterialTheme.colorScheme.primary.copy(alpha = 0.22f),
                    shape = RoundedCornerShape(16.dp, 16.dp, 4.dp, 16.dp),
                    modifier = Modifier.widthIn(max = 320.dp),
                ) {
                    SelectionContainer {
                        Text(
                            msg.content,
                            modifier = Modifier.padding(12.dp),
                            fontSize = 14.sp,
                        )
                    }
                }
            }
        }

        "tool" -> {
            val tr = msg.toolResult
            var expanded by remember { mutableStateOf(false) }
            Surface(
                color = MaterialTheme.colorScheme.surfaceVariant,
                shape = RoundedCornerShape(10.dp),
                modifier = Modifier.fillMaxWidth().clickable { expanded = !expanded },
            ) {
                Column(modifier = Modifier.padding(10.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(
                            if (tr?.isError == true) Icons.Default.ErrorOutline else Icons.Default.CheckCircleOutline,
                            contentDescription = null,
                            tint = if (tr?.isError == true) MaterialTheme.colorScheme.error else Color(0xFF50FA7B),
                            modifier = Modifier.size(16.dp),
                        )
                        Spacer(Modifier.width(6.dp))
                        Text(
                            "Tool result (${tr?.callId.orEmpty()})",
                            fontSize = 12.sp,
                            fontWeight = FontWeight.Medium,
                            modifier = Modifier.weight(1f),
                        )
                        Text(
                            if (expanded) "Hide" else "Show",
                            fontSize = 11.sp,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    }
                    if (expanded) {
                        Spacer(Modifier.height(6.dp))
                        SelectionContainer {
                            Text(
                                (tr?.content ?: msg.content).take(2000),
                                fontFamily = FontFamily.Monospace,
                                fontSize = 11.sp,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
        }

        else -> {
            var showReasoning by remember { mutableStateOf(false) }
            Column(modifier = Modifier.fillMaxWidth()) {
                if (!msg.reasoning.isNullOrBlank()) {
                    Surface(
                        color = MaterialTheme.colorScheme.surface,
                        shape = RoundedCornerShape(8.dp),
                        modifier = Modifier.fillMaxWidth().clickable { showReasoning = !showReasoning },
                    ) {
                        Column(modifier = Modifier.padding(8.dp)) {
                            Text(
                                if (showReasoning) "Thinking (tap to hide)" else "Thinking (tap to view)",
                                fontSize = 11.sp,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            if (showReasoning) {
                                Spacer(Modifier.height(4.dp))
                                Text(
                                    msg.reasoning,
                                    fontSize = 12.sp,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                    Spacer(Modifier.height(4.dp))
                }

                if (msg.toolCalls.isNotEmpty()) {
                    msg.toolCalls.forEach { tc ->
                        Surface(
                            color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.7f),
                            shape = RoundedCornerShape(8.dp),
                            modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp),
                        ) {
                            Text(
                                "Calling ${tc.name}(${tc.arguments.take(120)})",
                                modifier = Modifier.padding(8.dp),
                                fontFamily = FontFamily.Monospace,
                                fontSize = 11.sp,
                                color = MaterialTheme.colorScheme.secondary,
                            )
                        }
                    }
                }

                if (msg.content.isNotBlank()) {
                    Surface(
                        color = MaterialTheme.colorScheme.surface,
                        shape = RoundedCornerShape(16.dp, 16.dp, 16.dp, 4.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        SelectionContainer {
                            Text(
                                msg.content,
                                modifier = Modifier.padding(12.dp),
                                fontSize = 14.sp,
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
fun LiveTurnCard(
    reasoning: String,
    content: String,
    tools: List<LiveToolItem>,
) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shape = RoundedCornerShape(14.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                CircularProgressIndicator(modifier = Modifier.size(14.dp), strokeWidth = 2.dp)
                Spacer(Modifier.width(8.dp))
                Text("Luna is working...", fontSize = 12.sp, color = MaterialTheme.colorScheme.primary)
            }
            if (reasoning.isNotBlank()) {
                Text(
                    reasoning.takeLast(600),
                    fontSize = 12.sp,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            tools.forEach { tool ->
                Surface(
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    shape = RoundedCornerShape(8.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(modifier = Modifier.padding(8.dp)) {
                        Text(
                            "${tool.name} ${if (tool.done) (if (tool.isError) "failed" else "done") else "running..."}",
                            fontSize = 11.sp,
                            fontWeight = FontWeight.SemiBold,
                            color = if (tool.isError) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.secondary,
                        )
                        if (tool.output.isNotBlank()) {
                            Text(
                                tool.output.takeLast(300),
                                fontFamily = FontFamily.Monospace,
                                fontSize = 10.sp,
                            )
                        }
                    }
                }
            }
            if (content.isNotBlank()) {
                Text(content, fontSize = 14.sp)
            }
        }
    }
}

@Composable
fun SessionsScreen(
    sessions: List<SessionInfo>,
    activeSessionId: String,
    onSelect: (String) -> Unit,
    onDelete: (String) -> Unit,
    onNew: () -> Unit,
) {
    Column(modifier = Modifier.fillMaxSize().padding(12.dp)) {
        Button(onClick = onNew, modifier = Modifier.fillMaxWidth()) {
            Icon(Icons.Default.Add, contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text("New Session")
        }
        Spacer(Modifier.height(12.dp))
        LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(sessions, key = { it.id }) { s ->
                val isActive = s.id == activeSessionId
                Surface(
                    color = if (isActive) MaterialTheme.colorScheme.primary.copy(alpha = 0.16f) else MaterialTheme.colorScheme.surface,
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier.fillMaxWidth().clickable { onSelect(s.id) },
                ) {
                    Row(
                        modifier = Modifier.padding(12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                s.title.ifBlank { s.name.ifBlank { s.id } },
                                fontWeight = FontWeight.SemiBold,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                "${s.messageCount} msgs · ${s.provider ?: "-"} / ${s.model ?: "-"}",
                                fontSize = 12.sp,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        IconButton(onClick = { onDelete(s.id) }) {
                            Icon(Icons.Default.DeleteOutline, contentDescription = "Delete")
                        }
                    }
                }
            }
        }
    }
}

@Composable
fun MemoryScreen(
    memories: List<MemoryDto>,
    notes: List<NoteDto>,
    onSearch: (String) -> Unit,
) {
    var query by remember { mutableStateOf("") }
    var showNotes by remember { mutableStateOf(false) }

    Column(modifier = Modifier.fillMaxSize().padding(12.dp)) {
        OutlinedTextField(
            value = query,
            onValueChange = {
                query = it
                onSearch(it)
            },
            modifier = Modifier.fillMaxWidth(),
            placeholder = { Text("Search memories & notes...") },
            singleLine = true,
            leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
        )
        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilterChip(
                selected = !showNotes,
                onClick = { showNotes = false },
                label = { Text("Memories (${memories.size})") },
            )
            FilterChip(
                selected = showNotes,
                onClick = { showNotes = true },
                label = { Text("Notes (${notes.size})") },
            )
        }
        Spacer(Modifier.height(8.dp))
        LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (!showNotes) {
                items(memories) { m ->
                    Surface(
                        color = MaterialTheme.colorScheme.surface,
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(modifier = Modifier.padding(12.dp)) {
                            Text(
                                "${m.subject}.${m.predicate}",
                                fontWeight = FontWeight.SemiBold,
                                fontSize = 13.sp,
                                color = MaterialTheme.colorScheme.primary,
                            )
                            Text(m.object_, fontSize = 13.sp)
                            Text(m.category, fontSize = 11.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            } else {
                items(notes) { n ->
                    Surface(
                        color = MaterialTheme.colorScheme.surface,
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(modifier = Modifier.padding(12.dp)) {
                            Text(n.title, fontWeight = FontWeight.SemiBold, fontSize = 14.sp)
                            if (n.pointer.isNotBlank()) {
                                Text(n.pointer, fontSize = 12.sp, color = MaterialTheme.colorScheme.secondary)
                            }
                            Spacer(Modifier.height(4.dp))
                            Text(n.content.take(400), fontSize = 12.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            }
        }
    }
}

@Composable
fun SettingsScreen(
    state: InitResponse,
    onUpdated: (InitResponse) -> Unit,
    onError: (String) -> Unit,
) {
    val scope = rememberCoroutineScope()
    var selectedProvider by remember(state.provider) { mutableStateOf(state.provider) }
    var apiKey by remember { mutableStateOf("") }
    var showKey by remember { mutableStateOf(false) }
    var models by remember { mutableStateOf<List<String>>(emptyList()) }
    var customModel by remember(state.model) { mutableStateOf(state.model) }
    var autoApprove by remember(state.autoApprove) { mutableStateOf(state.autoApprove) }
    var busy by remember { mutableStateOf(false) }

    fun loadModels(providerId: String, refresh: Boolean) {
        scope.launch {
            busy = true
            val res = withContext(Dispatchers.IO) {
                runCatching {
                    jsonCodec.decodeFromString<ModelsResponse>(
                        LunaBridge.nativeListModels(providerId, refresh)
                    )
                }
            }
            busy = false
            res.onSuccess { if (it.ok) models = it.models }
        }
    }

    LaunchedEffect(selectedProvider) {
        loadModels(selectedProvider, false)
    }

    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Text("Provider", fontWeight = FontWeight.Bold, fontSize = 15.sp)
        Row(
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            state.providers.forEach { p ->
                FilterChip(
                    selected = p.id == selectedProvider,
                    onClick = { selectedProvider = p.id },
                    label = {
                        Text(if (p.hasKey) "${p.id} ✓" else p.id)
                    },
                )
            }
        }

        OutlinedTextField(
            value = apiKey,
            onValueChange = { apiKey = it },
            label = { Text("API Key for $selectedProvider") },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
            visualTransformation = if (showKey) VisualTransformation.None else PasswordVisualTransformation(),
            trailingIcon = {
                IconButton(onClick = { showKey = !showKey }) {
                    Icon(
                        if (showKey) Icons.Default.VisibilityOff else Icons.Default.Visibility,
                        contentDescription = "Toggle key",
                    )
                }
            },
        )
        Button(
            onClick = {
                scope.launch {
                    busy = true
                    val res = withContext(Dispatchers.IO) {
                        runCatching {
                            jsonCodec.decodeFromString<InitResponse>(
                                LunaBridge.nativeSetApiKey(selectedProvider, apiKey)
                            )
                        }
                    }
                    busy = false
                    res.onSuccess {
                        if (it.ok) {
                            apiKey = ""
                            onUpdated(it)
                            loadModels(selectedProvider, true)
                        } else {
                            onError(it.error ?: "Failed to save key")
                        }
                    }.onFailure { onError(it.message ?: "Error saving key") }
                }
            },
            enabled = apiKey.isNotBlank() && !busy,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Save API Key & Use $selectedProvider")
        }

        HorizontalDivider()

        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Model", fontWeight = FontWeight.Bold, fontSize = 15.sp, modifier = Modifier.weight(1f))
            TextButton(onClick = { loadModels(selectedProvider, true) }, enabled = !busy) {
                Text("Refresh")
            }
        }
        OutlinedTextField(
            value = customModel,
            onValueChange = { customModel = it },
            label = { Text("Model ID") },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
        )
        Button(
            onClick = {
                scope.launch {
                    busy = true
                    val res = withContext(Dispatchers.IO) {
                        runCatching {
                            jsonCodec.decodeFromString<InitResponse>(
                                LunaBridge.nativeSetModel(selectedProvider, customModel)
                            )
                        }
                    }
                    busy = false
                    res.onSuccess {
                        if (it.ok) onUpdated(it) else onError(it.error ?: "Failed to set model")
                    }
                }
            },
            enabled = customModel.isNotBlank() && !busy,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Apply Model")
        }

        if (models.isNotEmpty()) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                models.take(25).forEach { m ->
                    val selected = m == state.model && selectedProvider == state.provider
                    Surface(
                        color = if (selected) MaterialTheme.colorScheme.primary.copy(alpha = 0.2f) else MaterialTheme.colorScheme.surface,
                        shape = RoundedCornerShape(8.dp),
                        modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(8.dp)).clickable {
                            customModel = m
                            scope.launch {
                                val res = withContext(Dispatchers.IO) {
                                    runCatching {
                                        jsonCodec.decodeFromString<InitResponse>(
                                            LunaBridge.nativeSetModel(selectedProvider, m)
                                        )
                                    }
                                }
                                res.onSuccess { if (it.ok) onUpdated(it) }
                            }
                        },
                    ) {
                        Text(m, modifier = Modifier.padding(10.dp), fontSize = 13.sp)
                    }
                }
            }
        }

        HorizontalDivider()

        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text("Auto-approve mutating tools", fontWeight = FontWeight.Medium)
                Text(
                    "Allow file writes and shell commands without prompting",
                    fontSize = 12.sp,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Switch(
                checked = autoApprove,
                onCheckedChange = { checked ->
                    autoApprove = checked
                    scope.launch(Dispatchers.IO) {
                        LunaBridge.nativeSetAutoApprove(checked)
                    }
                },
            )
        }

        Text(
            "Workspace: ${state.workspaceDir}",
            fontSize = 11.sp,
            fontFamily = FontFamily.Monospace,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
