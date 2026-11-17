package ai.luna.mobile

import ai.luna.mobile.theme.LunaMobileTheme
import ai.luna.mobile.theme.LunaTheme
import ai.luna.mobile.theme.LunaThemeMode
import ai.luna.mobile.theme.applySystemBars
import ai.luna.mobile.ui.ChatScreen
import ai.luna.mobile.ui.ContextTelemetryPill
import ai.luna.mobile.ui.LiveToolItem
import ai.luna.mobile.ui.LunaCrescentMark
import ai.luna.mobile.ui.MemoryScreen
import ai.luna.mobile.ui.SessionsScreen
import ai.luna.mobile.ui.SettingsScreen
import ai.luna.mobile.ui.formatTokenCount
import android.content.Context
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AddComment
import androidx.compose.material.icons.filled.ChatBubbleOutline
import androidx.compose.material.icons.filled.Compress
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Snackbar
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
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

private const val PREFS_NAME = "luna_ui_prefs"
private const val PREF_THEME_MODE = "theme_mode"

enum class AppTab(val label: String) {
    Chat("Chat"),
    Sessions("Sessions"),
    Memory("Memory"),
    Settings("Settings"),
}

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val prefs = getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val savedMode = runCatching {
            LunaThemeMode.valueOf(prefs.getString(PREF_THEME_MODE, LunaThemeMode.System.name)!!)
        }.getOrDefault(LunaThemeMode.System)

        setContent {
            var themeMode by remember { mutableStateOf(savedMode) }
            val systemDark = isSystemInDarkTheme()
            val useDark = when (themeMode) {
                LunaThemeMode.System -> systemDark
                LunaThemeMode.Dark -> true
                LunaThemeMode.Light -> false
            }

            LaunchedEffect(useDark) {
                applySystemBars(useDark)
            }

            LunaMobileTheme(themeMode = themeMode) {
                LunaApp(
                    themeMode = themeMode,
                    onThemeModeChange = { next ->
                        themeMode = next
                        prefs.edit().putString(PREF_THEME_MODE, next.name).apply()
                    },
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LunaApp(
    themeMode: LunaThemeMode,
    onThemeModeChange: (LunaThemeMode) -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val colors = LunaTheme.colors
    val reduceMotion = LunaTheme.reduceMotion
    val snackbarHostState = remember { SnackbarHostState() }

    var currentTab by remember { mutableStateOf(AppTab.Chat) }
    var initState by remember { mutableStateOf<InitResponse?>(null) }
    var initError by remember { mutableStateOf<String?>(null) }
    var messages by remember { mutableStateOf<List<ChatMessageDto>>(emptyList()) }

    var inputText by remember { mutableStateOf("") }
    var isRunning by remember { mutableStateOf(false) }
    var streamingContent by remember { mutableStateOf("") }
    var streamingReasoning by remember { mutableStateOf("") }
    var liveTools by remember { mutableStateOf<List<LiveToolItem>>(emptyList()) }
    var tokensUsed by remember { mutableIntStateOf(0) }

    var pendingApproval by remember { mutableStateOf<Pair<ApprovalPayload, CompletableDeferred<Int>>?>(null) }
    var showContextSheet by remember { mutableStateOf(false) }

    var memories by remember { mutableStateOf<List<MemoryDto>>(emptyList()) }
    var notes by remember { mutableStateOf<List<NoteDto>>(emptyList()) }

    fun showNotice(message: String) {
        scope.launch {
            snackbarHostState.currentSnackbarData?.dismiss()
            snackbarHostState.showSnackbar(message)
        }
    }

    fun refreshMessages() {
        scope.launch {
            val res = withContext(Dispatchers.IO) {
                runCatching {
                    jsonCodec.decodeFromString<MessagesResponse>(LunaBridge.nativeGetMessages())
                }
            }
            res.onSuccess { if (it.ok) messages = it.messages }
        }
    }

    fun refreshKnowledge(query: String = "") {
        scope.launch {
            withContext(Dispatchers.IO) {
                runCatching {
                    val mem = jsonCodec.decodeFromString<MemoriesResponse>(
                        LunaBridge.nativeListMemories(query),
                    )
                    val note = jsonCodec.decodeFromString<NotesResponse>(
                        LunaBridge.nativeListNotes(query),
                    )
                    mem to note
                }
            }.onSuccess { (mem, note) ->
                if (mem.ok) memories = mem.memories
                if (note.ok) notes = note.notes
            }
        }
    }

    fun startNewSession() {
        scope.launch {
            val res = withContext(Dispatchers.IO) {
                runCatching {
                    jsonCodec.decodeFromString<InitResponse>(LunaBridge.nativeNewSession())
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
    }

    LaunchedEffect(Unit) {
        val result = withContext(Dispatchers.IO) {
            runCatching {
                val filesDir = context.filesDir.absolutePath
                val workspaceDir = File(context.filesDir, "workspace").apply { mkdirs() }.absolutePath
                jsonCodec.decodeFromString<InitResponse>(
                    LunaBridge.nativeInit(filesDir, workspaceDir),
                )
            }
        }
        result.onSuccess { init ->
            if (init.ok) {
                initState = init
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
                            showNotice("Compacting conversation...")
                        }
                        "compaction_finished" -> {
                            showNotice(ev.error?.let { "Compact: $it" } ?: "Conversation compacted")
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

    fun sendPrompt(overridePrompt: String? = null) {
        val prompt = (overridePrompt ?: inputText).trim()
        if (prompt.isEmpty() || isRunning) return
        if (overridePrompt == null) inputText = ""
        isRunning = true
        streamingContent = ""
        streamingReasoning = ""
        liveTools = emptyList()
        messages = messages + ChatMessageDto(role = "user", content = prompt)

        scope.launch {
            val res = withContext(Dispatchers.IO) {
                runCatching {
                    jsonCodec.decodeFromString<SendResponse>(
                        LunaBridge.nativeSendMessage(prompt, callback),
                    )
                }
            }
            isRunning = false
            streamingContent = ""
            streamingReasoning = ""
            liveTools = emptyList()
            res.onSuccess {
                if (!it.ok) {
                    showNotice(it.error ?: "Turn failed")
                }
            }.onFailure {
                showNotice(it.message ?: "Turn error")
            }
            refreshMessages()
            refreshKnowledge()
        }
    }

    BackHandler(enabled = pendingApproval != null || showContextSheet || currentTab != AppTab.Chat) {
        when {
            pendingApproval != null -> {
                val (_, deferred) = pendingApproval!!
                pendingApproval = null
                deferred.complete(0)
            }
            showContextSheet -> showContextSheet = false
            currentTab != AppTab.Chat -> currentTab = AppTab.Chat
        }
    }

    pendingApproval?.let { (req, deferred) ->
        ApprovalDialog(
            request = req,
            onDeny = {
                pendingApproval = null
                deferred.complete(0)
            },
            onAllowOnce = {
                pendingApproval = null
                deferred.complete(1)
            },
            onAllowSession = {
                pendingApproval = null
                deferred.complete(2)
            },
        )
    }

    if (showContextSheet && initState != null) {
        ContextSheet(
            state = initState!!,
            tokensUsed = tokensUsed,
            onCompact = {
                showContextSheet = false
                scope.launch {
                    withContext(Dispatchers.IO) {
                        LunaBridge.nativeCompactSession(callback)
                    }
                    refreshMessages()
                }
            },
            onNewSession = {
                showContextSheet = false
                startNewSession()
            },
            onDismiss = { showContextSheet = false },
        )
    }

    val density = LocalDensity.current
    val imeOpen = WindowInsets.ime.getBottom(density) > WindowInsets.navigationBars.getBottom(density)

    Scaffold(
        containerColor = colors.canvas,
        snackbarHost = {
            SnackbarHost(hostState = snackbarHostState) { data ->
                Snackbar(
                    snackbarData = data,
                    containerColor = colors.surfaceHigh,
                    contentColor = colors.ink,
                    actionColor = colors.coral,
                    shape = RoundedCornerShape(12.dp),
                )
            }
        },
        topBar = {
            Column(modifier = Modifier.background(colors.surfaceLow)) {
                TopAppBar(
                    colors = TopAppBarDefaults.topAppBarColors(
                        containerColor = colors.surfaceLow,
                        titleContentColor = colors.ink,
                        actionIconContentColor = colors.inkSecondary,
                    ),
                    title = {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                        ) {
                            LunaCrescentMark(size = 24.dp, tint = colors.coral)
                            Column {
                                Text(
                                    text = when (currentTab) {
                                        AppTab.Chat -> "Luna"
                                        AppTab.Sessions -> "Sessions"
                                        AppTab.Memory -> "Knowledge"
                                        AppTab.Settings -> "Settings"
                                    },
                                    style = MaterialTheme.typography.titleLarge,
                                )
                                if (currentTab == AppTab.Chat) {
                                    initState?.let { st ->
                                        Text(
                                            text = "${st.provider} · ${st.model}",
                                            style = MaterialTheme.typography.labelSmall.copy(
                                                color = colors.inkMuted,
                                            ),
                                            maxLines = 1,
                                            overflow = TextOverflow.Ellipsis,
                                        )
                                    }
                                }
                            }
                        }
                    },
                    actions = {
                        if (currentTab == AppTab.Chat && initState != null) {
                            ContextTelemetryPill(
                                tokensUsed = tokensUsed,
                                contextWindow = initState!!.contextWindow,
                                onClick = { showContextSheet = true },
                            )
                            Spacer(Modifier.width(4.dp))
                            IconButton(
                                onClick = { startNewSession() },
                                modifier = Modifier.size(48.dp),
                            ) {
                                Icon(Icons.Default.AddComment, contentDescription = "New Chat")
                            }
                        }
                    },
                )
                HorizontalDivider(color = colors.hairline)
            }
        },
        bottomBar = {
            if (!imeOpen) {
                Column(modifier = Modifier.background(colors.surfaceLow)) {
                    HorizontalDivider(color = colors.hairline)
                    NavigationBar(
                        containerColor = colors.surfaceLow,
                        tonalElevation = 0.dp,
                    ) {
                        val itemColors = NavigationBarItemDefaults.colors(
                            selectedIconColor = colors.coral,
                            selectedTextColor = colors.coral,
                            indicatorColor = colors.coralContainer,
                            unselectedIconColor = colors.inkMuted,
                            unselectedTextColor = colors.inkMuted,
                        )
                        NavigationBarItem(
                            selected = currentTab == AppTab.Chat,
                            onClick = { currentTab = AppTab.Chat },
                            icon = { Icon(Icons.Default.ChatBubbleOutline, contentDescription = null) },
                            label = { Text("Chat") },
                            colors = itemColors,
                        )
                        NavigationBarItem(
                            selected = currentTab == AppTab.Sessions,
                            onClick = { currentTab = AppTab.Sessions },
                            icon = { Icon(Icons.Default.History, contentDescription = null) },
                            label = { Text("Sessions") },
                            colors = itemColors,
                        )
                        NavigationBarItem(
                            selected = currentTab == AppTab.Memory,
                            onClick = {
                                refreshKnowledge()
                                currentTab = AppTab.Memory
                            },
                            icon = { Icon(Icons.Default.Psychology, contentDescription = null) },
                            label = { Text("Memory") },
                            colors = itemColors,
                        )
                        NavigationBarItem(
                            selected = currentTab == AppTab.Settings,
                            onClick = { currentTab = AppTab.Settings },
                            icon = { Icon(Icons.Default.Settings, contentDescription = null) },
                            label = { Text("Settings") },
                            colors = itemColors,
                        )
                    }
                }
            }
        },
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .then(if (currentTab == AppTab.Chat) Modifier.imePadding() else Modifier),
        ) {
            if (initError != null) {
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(24.dp),
                    verticalArrangement = Arrangement.Center,
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text(
                        text = "Failed to initialize Luna",
                        style = MaterialTheme.typography.titleMedium.copy(color = colors.bad),
                    )
                    Spacer(Modifier.height(8.dp))
                    Text(
                        text = initError.orEmpty(),
                        style = MaterialTheme.typography.bodyMedium.copy(color = colors.inkSecondary),
                    )
                }
            } else if (initState == null) {
                CircularProgressIndicator(
                    modifier = Modifier.align(Alignment.Center),
                    color = colors.coral,
                )
            } else {
                val state = initState!!
                Crossfade(
                    targetState = currentTab,
                    animationSpec = if (reduceMotion) snap() else tween(160),
                    label = "tab_crossfade",
                ) { tab ->
                    when (tab) {
                        AppTab.Chat -> ChatScreen(
                            messages = messages,
                            provider = state.provider,
                            model = state.model,
                            hasConfiguredKey = state.providers.any { it.hasKey },
                            isRunning = isRunning,
                            streamingContent = streamingContent,
                            streamingReasoning = streamingReasoning,
                            liveTools = liveTools,
                            inputText = inputText,
                            onInputChange = { inputText = it },
                            onSend = { prompt -> sendPrompt(prompt) },
                            onCancel = {
                                scope.launch(Dispatchers.IO) { LunaBridge.nativeCancelTurn() }
                            },
                            onOpenSettings = { currentTab = AppTab.Settings },
                        )

                        AppTab.Sessions -> SessionsScreen(
                            sessions = state.sessions,
                            activeSessionId = state.sessionId,
                            onSelect = { sid ->
                                scope.launch {
                                    val res = withContext(Dispatchers.IO) {
                                        runCatching {
                                            jsonCodec.decodeFromString<InitResponse>(
                                                LunaBridge.nativeSelectSession(sid),
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
                                                LunaBridge.nativeDeleteSession(sid),
                                            )
                                        }
                                    }
                                    res.onSuccess {
                                        if (it.ok) {
                                            initState = it
                                            refreshMessages()
                                            showNotice("Session deleted")
                                        }
                                    }
                                }
                            },
                            onNew = { startNewSession() },
                        )

                        AppTab.Memory -> MemoryScreen(
                            memories = memories,
                            notes = notes,
                            onSearch = { q -> refreshKnowledge(q) },
                        )

                        AppTab.Settings -> SettingsScreen(
                            state = state,
                            themeMode = themeMode,
                            onThemeModeChange = onThemeModeChange,
                            onUpdated = { updated, message ->
                                initState = updated
                                showNotice(message)
                            },
                            onError = { msg -> showNotice(msg) },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ApprovalDialog(
    request: ApprovalPayload,
    onDeny: () -> Unit,
    onAllowOnce: () -> Unit,
    onAllowSession: () -> Unit,
) {
    val colors = LunaTheme.colors
    AlertDialog(
        onDismissRequest = onDeny,
        containerColor = colors.surfaceLow,
        titleContentColor = colors.ink,
        textContentColor = colors.inkSecondary,
        icon = {
            Icon(
                imageVector = Icons.Default.Shield,
                contentDescription = null,
                tint = colors.amber,
            )
        },
        title = {
            Text(
                text = "Approve ${request.tool}?",
                style = MaterialTheme.typography.titleLarge,
            )
        },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                request.target?.let {
                    Surface(
                        color = colors.codeBg,
                        shape = RoundedCornerShape(8.dp),
                        border = BorderStroke(1.dp, colors.hairline),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            text = it,
                            style = MaterialTheme.typography.bodySmall.copy(
                                fontFamily = FontFamily.Monospace,
                                color = colors.celestial,
                            ),
                            modifier = Modifier.padding(10.dp),
                        )
                    }
                }
                val detail = request.diff ?: request.arguments?.toString().orEmpty()
                if (detail.isNotBlank()) {
                    Surface(
                        color = colors.codeBg,
                        shape = RoundedCornerShape(8.dp),
                        border = BorderStroke(1.dp, colors.hairline),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            text = detail.take(1200),
                            style = MaterialTheme.typography.bodySmall.copy(
                                fontFamily = FontFamily.Monospace,
                                color = colors.inkSecondary,
                            ),
                            modifier = Modifier.padding(10.dp),
                        )
                    }
                }
            }
        },
        confirmButton = {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = onAllowSession,
                    border = BorderStroke(1.dp, colors.outline),
                ) {
                    Text("Session", color = colors.ink)
                }
                Button(
                    onClick = onAllowOnce,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.coral,
                        contentColor = colors.onCoral,
                    ),
                ) {
                    Text("Allow")
                }
            }
        },
        dismissButton = {
            TextButton(onClick = onDeny) {
                Text("Deny", color = colors.bad)
            }
        },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ContextSheet(
    state: InitResponse,
    tokensUsed: Int,
    onCompact: () -> Unit,
    onNewSession: () -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = LunaTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val ratio = if (state.contextWindow > 0) {
        (tokensUsed.toFloat() / state.contextWindow.toFloat()).coerceIn(0f, 1f)
    } else 0f
    val pct = (ratio * 100).toInt()

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
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text("Session Context", style = MaterialTheme.typography.titleLarge)

            Surface(
                color = colors.surface,
                shape = RoundedCornerShape(14.dp),
                border = BorderStroke(1.dp, colors.hairline),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(modifier = Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            text = "${formatTokenCount(tokensUsed)} / ${formatTokenCount(state.contextWindow)} tokens",
                            style = MaterialTheme.typography.titleSmall.copy(
                                fontFeatureSettings = "tnum",
                                color = colors.ink,
                            ),
                            modifier = Modifier.weight(1f),
                        )
                        Text(
                            text = "$pct%",
                            style = MaterialTheme.typography.labelLarge.copy(
                                fontFeatureSettings = "tnum",
                                color = if (pct >= 75) colors.coral else colors.celestial,
                            ),
                        )
                    }
                    LinearProgressIndicator(
                        progress = { ratio },
                        color = if (pct >= 75) colors.coral else colors.celestial,
                        trackColor = colors.outline,
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(6.dp)
                            .clip(RoundedCornerShape(3.dp)),
                    )
                    Text(
                        text = "${state.provider} · ${state.model}",
                        style = MaterialTheme.typography.bodySmall.copy(
                            fontFamily = FontFamily.Monospace,
                            color = colors.inkMuted,
                        ),
                    )
                }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                OutlinedButton(
                    onClick = onCompact,
                    border = BorderStroke(1.dp, colors.outline),
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier.weight(1f).height(48.dp),
                ) {
                    Icon(Icons.Default.Compress, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(Modifier.width(6.dp))
                    Text("Compact", color = colors.ink)
                }
                Button(
                    onClick = onNewSession,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.coral,
                        contentColor = colors.onCoral,
                    ),
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier.weight(1f).height(48.dp),
                ) {
                    Icon(Icons.Default.AddComment, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(Modifier.width(6.dp))
                    Text("New Chat")
                }
            }
        }
    }
}
