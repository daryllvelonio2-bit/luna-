package ai.luna.mobile

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class ProviderInfo(
    val id: String,
    val displayName: String,
    val defaultModel: String,
    val hasKey: Boolean,
)

@Serializable
data class SessionInfo(
    val id: String,
    val name: String = "",
    val title: String = "",
    val updatedAt: String = "",
    val messageCount: Int = 0,
    val provider: String? = null,
    val model: String? = null,
)

@Serializable
data class InitResponse(
    val ok: Boolean = false,
    val error: String? = null,
    val sessionId: String = "",
    val provider: String = "",
    val model: String = "",
    val workspaceDir: String = "",
    val autoApprove: Boolean = false,
    val contextWindow: Int = 128000,
    val providers: List<ProviderInfo> = emptyList(),
    val sessions: List<SessionInfo> = emptyList(),
)

@Serializable
data class ToolCallDto(
    val id: String = "",
    val name: String = "",
    val arguments: String = "",
)

@Serializable
data class ToolResultDto(
    val callId: String = "",
    val content: String = "",
    val isError: Boolean = false,
)

@Serializable
data class ChatMessageDto(
    val role: String = "",
    val content: String = "",
    val reasoning: String? = null,
    val toolCalls: List<ToolCallDto> = emptyList(),
    val toolResult: ToolResultDto? = null,
)

@Serializable
data class MessagesResponse(
    val ok: Boolean = false,
    val error: String? = null,
    val messages: List<ChatMessageDto> = emptyList(),
)

@Serializable
data class ModelsResponse(
    val ok: Boolean = false,
    val error: String? = null,
    val models: List<String> = emptyList(),
)

@Serializable
data class SendResponse(
    val ok: Boolean = false,
    val error: String? = null,
    val reply: String = "",
    val durationMs: Long = 0,
)

@Serializable
data class MemoryDto(
    val id: String = "",
    val category: String = "",
    val subject: String = "",
    val predicate: String = "",
    @SerialName("object")
    val object_: String = "",
    val updatedAt: String = "",
)

@Serializable
data class MemoriesResponse(
    val ok: Boolean = false,
    val error: String? = null,
    val memories: List<MemoryDto> = emptyList(),
)

@Serializable
data class NoteDto(
    val id: String = "",
    val title: String = "",
    val pointer: String = "",
    val content: String = "",
    val updatedAt: String = "",
)

@Serializable
data class NotesResponse(
    val ok: Boolean = false,
    val error: String? = null,
    val notes: List<NoteDto> = emptyList(),
)

@Serializable
data class ApprovalPayload(
    val tool: String = "",
    val callId: String = "",
    val target: String? = null,
    val diff: String? = null,
    val arguments: kotlinx.serialization.json.JsonElement? = null,
)

@Serializable
data class StreamEventDto(
    val type: String = "",
    val text: String? = null,
    val name: String? = null,
    val arguments: String? = null,
    val chunk: String? = null,
    val output: String? = null,
    val isError: Boolean? = null,
    val tokensUsed: Int? = null,
    val reset: Boolean? = null,
    val error: String? = null,
)
