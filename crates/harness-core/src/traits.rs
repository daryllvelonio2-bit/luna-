use async_trait::async_trait;
use futures_util::stream::BoxStream;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::capability::ModelCapabilities;
use crate::error::HarnessError;
use crate::types::*;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Unique provider identifier (e.g., "openai", "anthropic", "gemini", "ollama")
    fn provider_id(&self) -> &str;

    /// Dynamically query available models from the provider endpoint
    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError>;

    /// Non-streaming completion call
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError>;

    /// Token-by-token streaming completion.
    ///
    /// Defaulted to "unsupported": the agent loop falls back to [`Self::complete`] when this
    /// returns an error before producing anything, so a provider that only implements the
    /// buffered call still runs a turn. Implementors that *can* stream should override it —
    /// it is what makes reasoning visible while the model is still writing it.
    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        Err(HarnessError::ProviderError {
            provider: self.provider_id().to_string(),
            message: "streaming is not supported by this provider".to_string(),
        })
    }

    /// Vector embeddings for memory indexing
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError>;

    /// What this provider's resolved model can be asked to do.
    ///
    /// Defaulted to [`ModelCapabilities::default`] so an implementor that has nothing to say gets
    /// the documented conservative answer instead of a compile error. The turn loop reads
    /// `tool_use` before it sends the tool list; the provider client reads `vision` before it
    /// lifts pasted images into content parts.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

#[async_trait]
impl ModelProvider for Arc<dyn ModelProvider> {
    fn provider_id(&self) -> &str {
        (**self).provider_id()
    }
    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        (**self).fetch_models().await
    }
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        (**self).complete(messages, tools, options).await
    }
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        options: &CompletionOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, HarnessError>>, HarnessError> {
        (**self).stream(messages, tools, options).await
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        (**self).embed(texts).await
    }
    fn capabilities(&self) -> ModelCapabilities {
        (**self).capabilities()
    }
}

#[async_trait]
pub trait MemoryEngine: Send + Sync {
    /// Retrieve semantically and relationally relevant memories
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError>;

    /// Save a new memory record, resolving contradictions and superseding previous matches
    async fn save(&self, record: &MemoryRecord) -> Result<(), HarnessError>;

    /// Update an existing memory, superseding the original
    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError>;

    /// Deactivate a memory record
    async fn forget(&self, id: &str) -> Result<(), HarnessError>;

    /// Register a 1-5 word note pointer memory record
    async fn add_note_pointer(&self, note_id: &str, pointer: &str) -> Result<(), HarnessError>;

    /// Remove the pointer that mirrors a note, so deleting a note leaves no dangling memory.
    ///
    /// Defaulted: the pointer id is derived from the note id (`mem_ptr_{note_id}`), so a plain
    /// `forget` is sufficient and implementors need not override it.
    async fn remove_note_pointer(&self, note_id: &str) -> Result<(), HarnessError> {
        // A missing pointer is not a failure — `forget` is idempotent — but a store error is:
        // returning `Ok` there is how a dangling memory pointer survives a note deletion.
        self.forget(&format!("mem_ptr_{}", note_id)).await?;
        self.forget(note_id).await?;
        Ok(())
    }

    /// Retrieve a specific active memory record by ID or subject
    async fn get_record(&self, _id: &str) -> Result<Option<MemoryRecord>, HarnessError> {
        Ok(None)
    }

    /// Render memories into a system prompt block
    fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String;
}

#[async_trait]
pub trait NotesEngine: Send + Sync {
    /// Save a new large note/document and distill 1-5 word summary pointer
    async fn save_note(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<Note, HarnessError>;

    /// Search notes using FTS5 full-text search
    async fn search_notes(&self, query: &str, limit: usize) -> Result<Vec<Note>, HarnessError>;

    /// Enumerate stored notes ordered by most recently updated
    async fn list_notes(&self, limit: usize, offset: usize) -> Result<Vec<Note>, HarnessError>;

    /// Retrieve full verbatim content of a note
    async fn read_note(&self, id: &str) -> Result<Note, HarnessError>;

    /// Update note content and re-distill pointer
    async fn update_note(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<Note, HarnessError>;

    /// Permanently delete a note. Returns `true` if a note with `id` existed and was removed,
    /// `false` if no such note was found (which is not an error).
    async fn delete_note(&self, id: &str) -> Result<bool, HarnessError>;
}

/// Receives a tool's output while it is still running, so a UI can show progress instead of
/// waiting for the call to finish.
///
/// Chunks are raw and may split lines (or even a UTF-8 character, for a caller that forwards
/// bytes): consumers must treat the stream as text to sanitize and buffer, not as whole lines.
pub type ToolProgress = Arc<dyn Fn(&str) + Send + Sync>;

#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name of the tool
    fn name(&self) -> &str;

    /// Description presented to LLM
    fn description(&self) -> &str;

    /// JSON schema for tool arguments
    fn parameters(&self) -> serde_json::Value;

    /// Execute the tool with given arguments
    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError>;

    /// Execute the tool, reporting output as it arrives.
    ///
    /// The default runs [`Tool::execute`] and reports nothing: only a tool that produces a stream
    /// worth watching (a command, a long build) needs to override it. The returned result must
    /// still be the complete output, so a caller that ignores progress sees exactly what it did
    /// before.
    async fn execute_with_progress(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        cancel_token: &CancellationToken,
        _progress: Option<&ToolProgress>,
    ) -> Result<ToolResult, HarnessError> {
        self.execute(call_id, arguments, cancel_token).await
    }

    /// Whether running this tool can change the machine or the repository — write a file, run a
    /// command, apply a patch, mutate stored state. Read-only tools must not override this.
    /// The default is `false` so a read tool needs no thought; **a mutating tool that forgets to
    /// override this runs ungated**, which is why the review question for a new tool is this one.
    fn is_mutating(&self) -> bool {
        false
    }

    /// Whether this specific call mutates given its arguments.
    /// Defaults to `self.is_mutating()`.
    fn is_mutating_call(&self, _arguments: &serde_json::Value) -> bool {
        self.is_mutating()
    }

    /// What a human needs to see before this call runs: the path that will be written or the exact
    /// command that will run, and for a patch-shaped call a unified diff. The default adds nothing,
    /// and the call is still gated — the prompt then shows the tool name and the arguments on
    /// request.
    async fn preview(&self, _arguments: &serde_json::Value) -> Option<ToolPreview> {
        None
    }

    /// Generate tool definition schema
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }
}

#[async_trait]
pub trait ToolMiddleware: Send + Sync {
    /// Hook executed before a tool runs. Can inspect/modify `call` or abort execution with an error.
    async fn on_pre_execute(&self, _call: &mut ToolCall) -> Result<(), HarnessError> {
        Ok(())
    }

    /// Hook executed after a tool completes. Can inspect/modify `result` or abort with an error.
    async fn on_post_execute(
        &self,
        _call: &ToolCall,
        _result: &mut ToolResult,
    ) -> Result<(), HarnessError> {
        Ok(())
    }
}

#[async_trait]
pub trait ToolRegistry: Send + Sync {
    /// Register a new tool
    async fn register(&self, tool: Arc<dyn Tool>) -> Result<(), HarnessError>;

    /// Unregister a tool by name. Returns true if the tool was present and removed.
    async fn unregister(&self, _name: &str) -> Result<bool, HarnessError> {
        Ok(false)
    }

    /// List all registered tool definitions
    async fn list_tools(&self) -> Result<Vec<ToolDefinition>, HarnessError>;

    /// Whether the named tool mutates. Deliberately has no default: a registry that silently
    /// answered "read-only" would fail open on the one check the safety gate depends on.
    async fn is_mutating(&self, name: &str) -> Result<bool, HarnessError>;

    /// Whether this specific call mutates given its arguments.
    async fn is_mutating_call(&self, call: &ToolCall) -> Result<bool, HarnessError> {
        self.is_mutating(&call.name).await
    }

    /// [`Tool::preview`] for a call; `None` when the tool has nothing to add to the prompt.
    async fn preview(&self, _call: &ToolCall) -> Option<ToolPreview> {
        None
    }

    /// Dispatch a tool call
    async fn execute(
        &self,
        call: &ToolCall,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError>;

    /// Dispatch a tool call, forwarding output as it arrives.
    ///
    /// A registry that does not forward progress still runs the call; the default is exactly
    /// [`ToolRegistry::execute`]. Registries that hold real tools should override this so the
    /// stream reaches the observer.
    async fn execute_with_progress(
        &self,
        call: &ToolCall,
        cancel_token: &CancellationToken,
        _progress: Option<&ToolProgress>,
    ) -> Result<ToolResult, HarnessError> {
        self.execute(call, cancel_token).await
    }
}
