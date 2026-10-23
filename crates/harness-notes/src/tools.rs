use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, NotesEngine, Tool, ToolPreview, ToolResult};

pub struct SaveNoteTool {
    engine: Arc<dyn NotesEngine>,
}

impl SaveNoteTool {
    pub fn new(engine: Arc<dyn NotesEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for SaveNoteTool {
    fn name(&self) -> &str {
        "save_note"
    }

    fn description(&self) -> &str {
        "Save large text, logs, specifications, transcripts, or documentation to persistent storage. Automatically generates a 1-5 word memory pointer so you retain awareness of the document."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Descriptive title of the note/document" },
                "content": { "type": "string", "description": "Full verbatim body of the note/document" },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of categorical tags"
                }
            },
            "required": ["title", "content"]
        })
    }

    /// Creates a persistent note: gated by the approval prompt when
    /// `agent.require_hitl_for_mutation` is on.
    fn is_mutating(&self) -> bool {
        true
    }

    /// The title of the note the call would create.
    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        Some(ToolPreview {
            target: args
                .get("title")
                .and_then(|t| t.as_str())
                .map(str::to_string),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let title = args
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or_default();
        let content = args
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or_default();
        let tags: Vec<String> = args
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let note = self.engine.save_note(title, content, &tags).await?;
        let output = serde_json::to_string_pretty(&note).unwrap_or_default();
        Ok(ToolResult::success(call_id, output))
    }
}

pub struct SearchNotesTool {
    engine: Arc<dyn NotesEngine>,
}

impl SearchNotesTool {
    pub fn new(engine: Arc<dyn NotesEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for SearchNotesTool {
    fn name(&self) -> &str {
        "search_notes"
    }

    fn description(&self) -> &str {
        "Perform full-text search (FTS5) across stored notes, titles, and documentation."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search keyword or phrase" },
                "limit": { "type": "integer", "description": "Max results to return (default 5)" }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .unwrap_or_default();
        let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(5) as usize;

        let results = self.engine.search_notes(query, limit).await?;
        let summaries: Vec<_> = results
            .into_iter()
            .map(|n| {
                json!({
                    "title": n.title,
                    "id": n.id,
                    "summary_pointer": n.summary_pointer,
                    "tags": n.tags,
                    "snippet": n.content.chars().take(200).collect::<String>(),
                })
            })
            .collect();

        Ok(ToolResult::success(
            call_id,
            serde_json::to_string_pretty(&summaries).unwrap_or_default(),
        ))
    }
}

pub struct ListNotesTool {
    engine: Arc<dyn NotesEngine>,
}

impl ListNotesTool {
    pub fn new(engine: Arc<dyn NotesEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for ListNotesTool {
    fn name(&self) -> &str {
        "list_notes"
    }

    fn description(&self) -> &str {
        "Enumerate stored notes in persistent storage ordered by most recently updated."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "Max results to return (default 20)" },
                "offset": { "type": "integer", "description": "Number of results to skip (default 0)" }
            }
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;

        let results = self.engine.list_notes(limit, offset).await?;
        let summaries: Vec<_> = results
            .into_iter()
            .map(|n| {
                json!({
                    "title": n.title,
                    "id": n.id,
                    "summary_pointer": n.summary_pointer,
                    "tags": n.tags,
                    "snippet": n.content.chars().take(200).collect::<String>(),
                    "updated_at": n.updated_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(ToolResult::success(
            call_id,
            serde_json::to_string_pretty(&summaries).unwrap_or_default(),
        ))
    }
}

pub struct ReadNoteTool {
    engine: Arc<dyn NotesEngine>,
}

impl ReadNoteTool {
    pub fn new(engine: Arc<dyn NotesEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for ReadNoteTool {
    fn name(&self) -> &str {
        "read_note"
    }

    fn description(&self) -> &str {
        "Retrieve the full verbatim content of a note by its title or note_id."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Note title or unique note identifier (e.g. 'Stripe Webhook Spec' or 'note_...')" }
            },
            "required": ["id"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let id = args.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        let note = self.engine.read_note(id).await?;
        Ok(ToolResult::success(
            call_id,
            serde_json::to_string_pretty(&note).unwrap_or_default(),
        ))
    }
}

pub struct UpdateNoteTool {
    engine: Arc<dyn NotesEngine>,
}

impl UpdateNoteTool {
    pub fn new(engine: Arc<dyn NotesEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for UpdateNoteTool {
    fn name(&self) -> &str {
        "update_note"
    }

    fn description(&self) -> &str {
        "Update the title or content of an existing note. Automatically refreshes the 1-5 word summary pointer."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Note title or unique note identifier" },
                "title": { "type": "string", "description": "New title (optional)" },
                "content": { "type": "string", "description": "New content body (optional)" }
            },
            "required": ["id"]
        })
    }

    /// Overwrites an existing note: gated like `save_note`.
    fn is_mutating(&self) -> bool {
        true
    }

    /// The id of the note the call would overwrite.
    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        Some(ToolPreview {
            target: args.get("id").and_then(|i| i.as_str()).map(str::to_string),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let id = args.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        let title = args.get("title").and_then(|t| t.as_str());
        let content = args.get("content").and_then(|c| c.as_str());

        let note = self.engine.update_note(id, title, content, None).await?;
        Ok(ToolResult::success(
            call_id,
            serde_json::to_string_pretty(&note).unwrap_or_default(),
        ))
    }
}

pub struct DeleteNoteTool {
    engine: Arc<dyn NotesEngine>,
}

impl DeleteNoteTool {
    pub fn new(engine: Arc<dyn NotesEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for DeleteNoteTool {
    fn name(&self) -> &str {
        "delete_note"
    }

    fn description(&self) -> &str {
        "Permanently delete a stored note by its note_id. Use this to remove notes that are obsolete or were saved in error; the note's full-text index entry is removed with it."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Note title or unique note identifier to delete" }
            },
            "required": ["id"]
        })
    }

    /// Permanently removes a note: gated like `save_note`.
    fn is_mutating(&self) -> bool {
        true
    }

    /// The id of the note the call would delete.
    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        Some(ToolPreview {
            target: args.get("id").and_then(|i| i.as_str()).map(str::to_string),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let id = args.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        let deleted = self.engine.delete_note(id).await?;

        Ok(ToolResult::success(
            call_id,
            json!({
                "status": if deleted { "deleted" } else { "not_found" },
                "note_id": id,
            })
            .to_string(),
        ))
    }
}
