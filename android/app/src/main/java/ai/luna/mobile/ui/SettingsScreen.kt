package ai.luna.mobile.ui

import ai.luna.mobile.InitResponse
import ai.luna.mobile.LunaBridge
import ai.luna.mobile.ModelsResponse
import ai.luna.mobile.ProviderInfo
import ai.luna.mobile.theme.LunaTheme
import ai.luna.mobile.theme.LunaThemeMode
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Visibility
import androidx.compose.material.icons.filled.VisibilityOff
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json

private val settingsJson = Json {
    ignoreUnknownKeys = true
    isLenient = true
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    state: InitResponse,
    themeMode: LunaThemeMode,
    onThemeModeChange: (LunaThemeMode) -> Unit,
    onUpdated: (InitResponse, String) -> Unit,
    onError: (String) -> Unit,
) {
    val colors = LunaTheme.colors
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    var selectedProvider by remember(state.provider) { mutableStateOf(state.provider) }
    var apiKey by remember { mutableStateOf("") }
    var showKey by remember { mutableStateOf(false) }
    var models by remember { mutableStateOf<List<String>>(emptyList()) }
    var autoApprove by remember(state.autoApprove) { mutableStateOf(state.autoApprove) }
    var busy by remember { mutableStateOf(false) }

    var showProviderSheet by remember { mutableStateOf(false) }
    var showModelSheet by remember { mutableStateOf(false) }

    val activeProviderInfo = state.providers.find { it.id == selectedProvider }

    fun loadModels(providerId: String, refresh: Boolean) {
        scope.launch {
            busy = true
            val res = withContext(Dispatchers.IO) {
                runCatching {
                    settingsJson.decodeFromString<ModelsResponse>(
                        LunaBridge.nativeListModels(providerId, refresh),
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

    if (showProviderSheet) {
        ProviderPickerSheet(
            providers = state.providers,
            selectedId = selectedProvider,
            onSelect = { chosen ->
                selectedProvider = chosen.id
                showProviderSheet = false
            },
            onDismiss = { showProviderSheet = false },
        )
    }

    if (showModelSheet) {
        ModelPickerSheet(
            providerId = selectedProvider,
            currentModel = state.model,
            models = models,
            busy = busy,
            onRefresh = { loadModels(selectedProvider, true) },
            onSelectModel = { modelId ->
                showModelSheet = false
                scope.launch {
                    busy = true
                    val res = withContext(Dispatchers.IO) {
                        runCatching {
                            settingsJson.decodeFromString<InitResponse>(
                                LunaBridge.nativeSetModel(selectedProvider, modelId),
                            )
                        }
                    }
                    busy = false
                    res.onSuccess {
                        if (it.ok) {
                            onUpdated(it, "Switched to $selectedProvider / $modelId")
                        } else {
                            onError(it.error ?: "Failed to switch model")
                        }
                    }.onFailure {
                        onError(it.message ?: "Error switching model")
                    }
                }
            },
            onDismiss = { showModelSheet = false },
        )
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(colors.canvas)
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        SectionHeader(title = "Connection & Provider")

        Surface(
            color = colors.surfaceLow,
            shape = RoundedCornerShape(16.dp),
            border = BorderStroke(1.dp, colors.hairline),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Column(modifier = Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 52.dp)
                        .clickable { showProviderSheet = true },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            "Provider",
                            style = MaterialTheme.typography.labelMedium.copy(color = colors.inkMuted),
                        )
                        Spacer(Modifier.height(2.dp))
                        Text(
                            activeProviderInfo?.displayName ?: selectedProvider,
                            style = MaterialTheme.typography.titleSmall.copy(color = colors.ink),
                        )
                    }
                    StatusPill(
                        label = if (activeProviderInfo?.hasKey == true) "Configured" else "Needs Key",
                        foreground = if (activeProviderInfo?.hasKey == true) colors.ok else colors.amber,
                        background = if (activeProviderInfo?.hasKey == true) colors.okContainer else colors.amberContainer,
                    )
                    Spacer(Modifier.width(4.dp))
                    Icon(Icons.Default.ChevronRight, contentDescription = null, tint = colors.inkMuted)
                }

                HorizontalDivider(color = colors.hairline)

                OutlinedTextField(
                    value = apiKey,
                    onValueChange = { apiKey = it },
                    label = { Text("API Key for $selectedProvider") },
                    placeholder = {
                        Text(
                            if (activeProviderInfo?.hasKey == true) "Enter a new key to replace existing" else "sk-...",
                            color = colors.inkMuted,
                        )
                    },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                    shape = RoundedCornerShape(12.dp),
                    visualTransformation = if (showKey) VisualTransformation.None else PasswordVisualTransformation(),
                    trailingIcon = {
                        IconButton(onClick = { showKey = !showKey }, modifier = Modifier.size(48.dp)) {
                            Icon(
                                if (showKey) Icons.Default.VisibilityOff else Icons.Default.Visibility,
                                contentDescription = "Toggle API key visibility",
                            )
                        }
                    },
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedBorderColor = colors.coral,
                        unfocusedBorderColor = colors.outline,
                    ),
                )

                Button(
                    onClick = {
                        scope.launch {
                            busy = true
                            val res = withContext(Dispatchers.IO) {
                                runCatching {
                                    settingsJson.decodeFromString<InitResponse>(
                                        LunaBridge.nativeSetApiKey(selectedProvider, apiKey),
                                    )
                                }
                            }
                            busy = false
                            res.onSuccess {
                                if (it.ok) {
                                    apiKey = ""
                                    onUpdated(it, "API key saved for $selectedProvider")
                                    loadModels(selectedProvider, true)
                                } else {
                                    onError(it.error ?: "Failed to save API key")
                                }
                            }.onFailure {
                                onError(it.message ?: "Error saving key")
                            }
                        }
                    },
                    enabled = apiKey.isNotBlank() && !busy,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.coral,
                        contentColor = colors.onCoral,
                    ),
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(48.dp),
                ) {
                    if (busy) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(18.dp),
                            strokeWidth = 2.dp,
                            color = colors.onCoral,
                        )
                        Spacer(Modifier.width(8.dp))
                    }
                    Text("Save API Key & Activate")
                }
            }
        }

        SectionHeader(title = "Model")

        Surface(
            onClick = { showModelSheet = true },
            color = colors.surfaceLow,
            shape = RoundedCornerShape(16.dp),
            border = BorderStroke(1.dp, colors.hairline),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 60.dp)
                    .padding(horizontal = 14.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        "Active Model",
                        style = MaterialTheme.typography.labelMedium.copy(color = colors.inkMuted),
                    )
                    Spacer(Modifier.height(2.dp))
                    Text(
                        text = state.model.ifBlank { activeProviderInfo?.defaultModel.orEmpty() },
                        style = MaterialTheme.typography.titleSmall.copy(
                            fontFamily = FontFamily.Monospace,
                            color = colors.ink,
                        ),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                StatusPill(
                    label = "${formatCompactTokens(state.contextWindow)} ctx",
                    foreground = colors.celestial,
                    background = colors.celestialContainer,
                )
                Spacer(Modifier.width(4.dp))
                Icon(Icons.Default.ChevronRight, contentDescription = "Pick model", tint = colors.inkMuted)
            }
        }

        SectionHeader(title = "Agent & Safety")

        Surface(
            color = colors.surfaceLow,
            shape = RoundedCornerShape(16.dp),
            border = BorderStroke(1.dp, colors.hairline),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable {
                        val next = !autoApprove
                        autoApprove = next
                        scope.launch(Dispatchers.IO) {
                            LunaBridge.nativeSetAutoApprove(next)
                        }
                    }
                    .padding(horizontal = 14.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        "Auto-approve mutating tools",
                        style = MaterialTheme.typography.titleSmall.copy(color = colors.ink),
                    )
                    Spacer(Modifier.height(2.dp))
                    Text(
                        "Allow file writes and shell commands without prompting each call",
                        style = MaterialTheme.typography.bodySmall.copy(color = colors.inkSecondary),
                    )
                }
                Spacer(Modifier.width(12.dp))
                Switch(
                    checked = autoApprove,
                    onCheckedChange = { checked ->
                        autoApprove = checked
                        scope.launch(Dispatchers.IO) {
                            LunaBridge.nativeSetAutoApprove(checked)
                        }
                    },
                    colors = SwitchDefaults.colors(
                        checkedThumbColor = colors.onCoral,
                        checkedTrackColor = colors.coral,
                    ),
                )
            }
        }

        SectionHeader(title = "Appearance")

        SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
            val modes = LunaThemeMode.entries
            modes.forEachIndexed { index, mode ->
                SegmentedButton(
                    selected = themeMode == mode,
                    onClick = { onThemeModeChange(mode) },
                    shape = SegmentedButtonDefaults.itemShape(index = index, count = modes.size),
                    colors = SegmentedButtonDefaults.colors(
                        activeContainerColor = colors.coralContainer,
                        activeContentColor = colors.coral,
                        inactiveContainerColor = colors.surfaceLow,
                        inactiveContentColor = colors.inkSecondary,
                    ),
                ) {
                    Text(mode.label, style = MaterialTheme.typography.labelLarge)
                }
            }
        }

        SectionHeader(title = "Workspace & Runtime")

        Surface(
            color = colors.surfaceLow,
            shape = RoundedCornerShape(16.dp),
            border = BorderStroke(1.dp, colors.hairline),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Row(
                modifier = Modifier.padding(start = 14.dp, end = 6.dp, top = 12.dp, bottom = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        "On-device workspace",
                        style = MaterialTheme.typography.labelMedium.copy(color = colors.inkMuted),
                    )
                    Spacer(Modifier.height(2.dp))
                    Text(
                        text = state.workspaceDir,
                        style = MaterialTheme.typography.bodySmall.copy(
                            fontFamily = FontFamily.Monospace,
                            color = colors.inkSecondary,
                        ),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                IconButton(
                    onClick = { clipboard.setText(AnnotatedString(state.workspaceDir)) },
                    modifier = Modifier.size(48.dp),
                ) {
                    Icon(Icons.Default.ContentCopy, contentDescription = "Copy workspace path", tint = colors.inkMuted)
                }
            }
        }

        Spacer(Modifier.height(16.dp))
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ProviderPickerSheet(
    providers: List<ProviderInfo>,
    selectedId: String,
    onSelect: (ProviderInfo) -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = LunaTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var query by remember { mutableStateOf("") }
    val filtered = remember(providers, query) {
        val q = query.trim().lowercase()
        if (q.isEmpty()) providers
        else providers.filter {
            it.id.lowercase().contains(q) || it.displayName.lowercase().contains(q)
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
                .padding(horizontal = 16.dp)
                .padding(bottom = 24.dp),
        ) {
            Text("Choose Provider", style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(10.dp))
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                placeholder = { Text("Search providers...", color = colors.inkMuted) },
                leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
                singleLine = true,
                shape = RoundedCornerShape(12.dp),
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(10.dp))
            LazyColumn(
                modifier = Modifier.heightIn(max = 420.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                items(filtered, key = { it.id }) { p ->
                    val selected = p.id == selectedId
                    Surface(
                        onClick = { onSelect(p) },
                        color = if (selected) colors.coralContainer else colors.surface,
                        shape = RoundedCornerShape(12.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(min = 52.dp)
                                .padding(horizontal = 14.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = p.displayName,
                                    style = MaterialTheme.typography.titleSmall.copy(
                                        color = if (selected) colors.coral else colors.ink,
                                    ),
                                )
                                Text(
                                    text = "${p.id} · ${p.defaultModel}",
                                    style = MaterialTheme.typography.bodySmall.copy(
                                        fontFamily = FontFamily.Monospace,
                                        color = colors.inkMuted,
                                    ),
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                            if (p.hasKey) {
                                StatusPill(
                                    label = "Ready",
                                    foreground = colors.ok,
                                    background = colors.okContainer,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ModelPickerSheet(
    providerId: String,
    currentModel: String,
    models: List<String>,
    busy: Boolean,
    onRefresh: () -> Unit,
    onSelectModel: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = LunaTheme.colors
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var query by remember { mutableStateOf("") }

    val filtered = remember(models, query) {
        val q = query.trim().lowercase()
        if (q.isEmpty()) models
        else models.filter { it.lowercase().contains(q) }
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
                .padding(horizontal = 16.dp)
                .padding(bottom = 24.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = "Models · $providerId",
                    style = MaterialTheme.typography.titleLarge,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onRefresh, enabled = !busy) {
                    Icon(Icons.Default.Refresh, contentDescription = null, modifier = Modifier.size(18.dp))
                    Spacer(Modifier.width(4.dp))
                    Text("Refresh")
                }
            }
            Spacer(Modifier.height(8.dp))
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                placeholder = { Text("Filter or type custom model ID...", color = colors.inkMuted) },
                leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
                trailingIcon = {
                    if (query.isNotEmpty()) {
                        IconButton(onClick = { query = "" }, modifier = Modifier.size(48.dp)) {
                            Icon(Icons.Default.Close, contentDescription = "Clear")
                        }
                    }
                },
                singleLine = true,
                shape = RoundedCornerShape(12.dp),
                modifier = Modifier.fillMaxWidth(),
            )

            if (query.isNotBlank() && models.none { it.equals(query.trim(), ignoreCase = true) }) {
                Spacer(Modifier.height(8.dp))
                Button(
                    onClick = { onSelectModel(query.trim()) },
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.coral,
                        contentColor = colors.onCoral,
                    ),
                    shape = RoundedCornerShape(12.dp),
                    modifier = Modifier.fillMaxWidth().height(48.dp),
                ) {
                    Text("Use custom model \"${query.trim()}\"")
                }
            }

            Spacer(Modifier.height(10.dp))

            if (busy && models.isEmpty()) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(24.dp),
                    horizontalArrangement = Arrangement.Center,
                ) {
                    CircularProgressIndicator(color = colors.coral)
                }
            } else if (filtered.isEmpty()) {
                Text(
                    text = "No cached models found. Type a model ID above or tap Refresh.",
                    style = MaterialTheme.typography.bodyMedium.copy(color = colors.inkSecondary),
                    modifier = Modifier.padding(16.dp),
                )
            } else {
                LazyColumn(
                    modifier = Modifier.heightIn(max = 380.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    items(filtered, key = { it }) { m ->
                        val selected = m == currentModel
                        Surface(
                            onClick = { onSelectModel(m) },
                            color = if (selected) colors.coralContainer else colors.surface,
                            shape = RoundedCornerShape(12.dp),
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .heightIn(min = 48.dp)
                                    .padding(horizontal = 14.dp, vertical = 10.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Text(
                                    text = m,
                                    style = MaterialTheme.typography.bodyMedium.copy(
                                        fontFamily = FontFamily.Monospace,
                                        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                                        color = if (selected) colors.coral else colors.ink,
                                    ),
                                    modifier = Modifier.weight(1f),
                                )
                                if (selected) {
                                    Icon(
                                        imageVector = Icons.Default.Check,
                                        contentDescription = "Selected",
                                        tint = colors.coral,
                                        modifier = Modifier.size(18.dp),
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
