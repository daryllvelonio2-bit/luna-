package ai.luna.mobile

interface LunaCallback {
    fun onEvent(jsonPayload: String)
    fun requestApproval(requestJson: String): Int
}

object LunaBridge {
    init {
        System.loadLibrary("luna_android")
    }

    external fun nativeInit(filesDir: String, workspaceDir: String): String
    external fun nativeSetApiKey(providerId: String, apiKey: String): String
    external fun nativeListModels(providerId: String, refresh: Boolean): String
    external fun nativeSetModel(providerId: String, modelId: String): String
    external fun nativeSetAutoApprove(enabled: Boolean): String
    external fun nativeNewSession(): String
    external fun nativeSelectSession(sessionId: String): String
    external fun nativeDeleteSession(sessionId: String): String
    external fun nativeGetMessages(): String
    external fun nativeSendMessage(prompt: String, callback: LunaCallback): String
    external fun nativeCancelTurn(): Int
    external fun nativeCompactSession(callback: LunaCallback): String
    external fun nativeListMemories(query: String): String
    external fun nativeListNotes(query: String): String
}
