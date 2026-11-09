use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::{
    HarnessError, MemoryCategory, MemoryEngine, MemoryRecord, NotesEngine, Tool, ToolPreview,
    ToolResult,
};

pub struct SaveMemoryTool {
    engine: Arc<dyn MemoryEngine>,
}

impl SaveMemoryTool {
    pub fn new(engine: Arc<dyn MemoryEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for SaveMemoryTool {
    fn name(&self) -> &str {
        "save_memory"
    }

    fn description(&self) -> &str {
        "Save a lean index card into persistent memory (name, path, short 2-5 word about). Detailed notes, architectures, specs, and instructions belong in notes, not memories. Exactly one active memory record is kept per topic."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "subject": { "type": "string", "description": "Entity topic or subject (e.g. 'project.astra', 'user.profile', 'user.editor')" },
                "object": { "type": "string", "description": "Short description or value of what to remember (also accepts content/value)" },
                "predicate": { "type": "string", "description": "Optional property or qualifier (default: 'profile')" },
                "name": { "type": "string", "description": "Optional friendly name (e.g. 'Astra IDE')" },
                "path": { "type": "string", "description": "Optional absolute project or file path" },
                "category": {
                    "type": "string",
                    "enum": ["user_profile", "code_pattern", "workflow_rule", "fact"],
                    "description": "Optional category (default: 'fact' or 'user_profile' for user.*)"
                },
                "confidence": { "type": "number", "description": "Confidence score from 0.0 to 1.0 (default 1.0)" }
            },
            "required": ["subject", "object"]
        })
    }

    /// Writes a durable memory record: gated by the approval prompt when
    /// `agent.require_hitl_for_mutation` is on.
    fn is_mutating(&self) -> bool {
        true
    }

    /// The subject/predicate the call would record.
    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        Some(ToolPreview {
            target: Some(format!(
                "{}: {}",
                args.get("subject").and_then(|s| s.as_str()).unwrap_or("?"),
                args.get("predicate")
                    .and_then(|p| p.as_str())
                    .unwrap_or("profile")
            )),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let subject = args
            .get("subject")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .trim();
        let predicate = args
            .get("predicate")
            .and_then(|p| p.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("profile");
        let object = args
            .get("object")
            .or_else(|| args.get("content"))
            .or_else(|| args.get("value"))
            .and_then(|o| o.as_str())
            .unwrap_or_default()
            .trim();
        let cat_str = args.get("category").and_then(|c| c.as_str()).unwrap_or(
            if subject.starts_with("user.") {
                "user_profile"
            } else {
                "fact"
            },
        );
        let confidence = args
            .get("confidence")
            .and_then(|c| c.as_f64())
            .unwrap_or(1.0) as f32;

        let name = args
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let path = args
            .get("path")
            .and_then(|p| p.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let category = match cat_str {
            "user_profile" => MemoryCategory::UserProfile,
            "code_pattern" => MemoryCategory::CodePattern,
            "workflow_rule" => MemoryCategory::WorkflowRule,
            _ => MemoryCategory::Fact,
        };

        let s_sub = subject
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        let s_pred = predicate
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        let id = if !s_sub.is_empty() && !s_pred.is_empty() {
            format!("{}.{}", s_sub, s_pred)
        } else if !s_sub.is_empty() {
            s_sub
        } else {
            format!("mem_{}", &uuid::Uuid::new_v4().to_string()[..8])
        };
        let raw_content = format!("{} {} {}", subject, predicate, object);

        let record = MemoryRecord {
            id: id.clone(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            name,
            path,
            raw_content,
            category,
            confidence,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        };

        self.engine.save(&record).await?;
        Ok(ToolResult::success(
            call_id,
            json!({
                "status": "saved",
                "memory_id": id,
                "subject": subject,
                "predicate": predicate,
                "object": object,
            })
            .to_string(),
        ))
    }
}

fn derive_memory_id(subject: &str, predicate: &str) -> String {
    let s_sub = subject
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    let s_pred = predicate
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if !s_sub.is_empty() && !s_pred.is_empty() {
        format!("{}.{}", s_sub, s_pred)
    } else if !s_sub.is_empty() {
        s_sub
    } else {
        format!("mem_{}", &uuid::Uuid::new_v4().to_string()[..8])
    }
}

pub struct UpdateMemoryTool {
    engine: Arc<dyn MemoryEngine>,
}

impl UpdateMemoryTool {
    pub fn new(engine: Arc<dyn MemoryEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for UpdateMemoryTool {
    fn name(&self) -> &str {
        "update_memory"
    }

    fn description(&self) -> &str {
        "Update or edit an existing memory record. Accepts memory_id (or topic/subject) and any fields to update (e.g. object/content, name, path). Omitted fields are automatically preserved from the existing record. If the record does not exist yet, it will be created."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "memory_id": {
                    "type": "string",
                    "description": "ID or topic of memory record to update (e.g. 'user.profile', 'project.luna')"
                },
                "object": {
                    "type": "string",
                    "description": "Updated short description, content, preference, or fact"
                },
                "name": {
                    "type": "string",
                    "description": "Optional updated display name"
                },
                "path": {
                    "type": "string",
                    "description": "Optional updated absolute project or file path"
                },
                "subject": {
                    "type": "string",
                    "description": "Optional entity topic (preserved from existing record if omitted)"
                },
                "predicate": {
                    "type": "string",
                    "description": "Optional property qualifier (preserved from existing record if omitted)"
                },
                "category": {
                    "type": "string",
                    "enum": ["user_profile", "code_pattern", "workflow_rule", "fact"],
                    "description": "Optional category (preserved from existing record if omitted)"
                }
            },
            "required": ["memory_id"]
        })
    }

    /// Supersedes an existing memory record: gated like `save_memory`.
    fn is_mutating(&self) -> bool {
        true
    }

    /// The id of the record the call would supersede.
    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        Some(ToolPreview {
            target: args
                .get("memory_id")
                .or_else(|| args.get("id"))
                .or_else(|| args.get("target"))
                .or_else(|| args.get("topic"))
                .or_else(|| args.get("subject"))
                .and_then(|i| i.as_str())
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
        let old_id = args
            .get("memory_id")
            .or_else(|| args.get("id"))
            .or_else(|| args.get("target"))
            .or_else(|| args.get("topic"))
            .or_else(|| args.get("subject"))
            .and_then(|id| id.as_str())
            .unwrap_or_default()
            .trim();

        if old_id.is_empty() {
            return Ok(ToolResult::error(
                call_id,
                "Missing required parameter 'memory_id'",
            ));
        }

        let existing = self.engine.get_record(old_id).await?;

        let subject = args
            .get("subject")
            .and_then(|s| s.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                existing
                    .as_ref()
                    .map(|r| r.subject.clone())
                    .unwrap_or_else(|| old_id.to_string())
            });

        let predicate = args
            .get("predicate")
            .and_then(|p| p.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                existing
                    .as_ref()
                    .map(|r| r.predicate.clone())
                    .unwrap_or_else(|| "profile".to_string())
            });

        let object = args
            .get("object")
            .or_else(|| args.get("content"))
            .or_else(|| args.get("value"))
            .or_else(|| args.get("text"))
            .or_else(|| args.get("description"))
            .and_then(|o| o.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                existing
                    .as_ref()
                    .map(|r| r.object.clone())
                    .unwrap_or_default()
            });

        let name = args
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| existing.as_ref().and_then(|r| r.name.clone()));

        let path = args
            .get("path")
            .and_then(|p| p.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| existing.as_ref().and_then(|r| r.path.clone()));

        let category = if let Some(cat_str) = args.get("category").and_then(|c| c.as_str()) {
            match cat_str {
                "user_profile" => MemoryCategory::UserProfile,
                "code_pattern" => MemoryCategory::CodePattern,
                "workflow_rule" => MemoryCategory::WorkflowRule,
                _ => MemoryCategory::Fact,
            }
        } else if let Some(ref ex) = existing {
            ex.category
        } else {
            MemoryCategory::Fact
        };

        let new_id = if let Some(ref ex) = existing {
            if args.get("subject").is_none() && args.get("predicate").is_none() {
                ex.id.clone()
            } else {
                derive_memory_id(&subject, &predicate)
            }
        } else {
            derive_memory_id(&subject, &predicate)
        };

        let raw_content = format!("{} {} {}", subject, predicate, object);

        let record = MemoryRecord {
            id: new_id.clone(),
            subject,
            predicate,
            object,
            name,
            path,
            raw_content,
            category,
            confidence: 1.0,
            created_at: existing
                .as_ref()
                .map(|r| r.created_at)
                .unwrap_or_else(Utc::now),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        };

        if let Some(ref ex) = existing {
            self.engine.update(&ex.id, &record).await?;
        } else {
            // No existing record: create it (the tool promises creation when missing).
            if record.object.is_empty() {
                return Ok(ToolResult::error(
                    call_id,
                    "Cannot create a new memory without 'object' content (no existing record to preserve)",
                ));
            }
            self.engine.save(&record).await?;
        }

        Ok(ToolResult::success(
            call_id,
            json!({
                "status": "updated",
                "old_memory_id": old_id,
                "new_memory_id": new_id,
                "subject": record.subject,
                "object": record.object,
            })
            .to_string(),
        ))
    }
}

pub struct ForgetMemoryTool {
    engine: Arc<dyn MemoryEngine>,
}

impl ForgetMemoryTool {
    pub fn new(engine: Arc<dyn MemoryEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for ForgetMemoryTool {
    fn name(&self) -> &str {
        "forget_memory"
    }

    fn description(&self) -> &str {
        "Delete or deactivate memory records. Use when the user asks to delete, forget, or clear memory. Accepts 'all' (or '*') to delete all memories, a specific ID, a topic/subject, multiple IDs separated by commas or semicolons, or a wildcard ('user.*')."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "memory_id": {
                    "type": "string",
                    "description": "ID, topic/subject, comma-separated IDs, wildcard ('user.*'), or 'all' to delete all memories"
                }
            },
            "required": ["memory_id"]
        })
    }

    /// Deactivates an existing memory record: gated like `save_memory`.
    fn is_mutating(&self) -> bool {
        true
    }

    /// The id of the record the call would deactivate.
    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        Some(ToolPreview {
            target: args
                .get("memory_id")
                .or_else(|| args.get("id"))
                .or_else(|| args.get("target"))
                .or_else(|| args.get("query"))
                .or_else(|| args.get("subject"))
                .and_then(|i| i.as_str())
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
        let raw_id = args
            .get("memory_id")
            .or_else(|| args.get("id"))
            .or_else(|| args.get("target"))
            .or_else(|| args.get("query"))
            .or_else(|| args.get("subject"))
            .and_then(|id| id.as_str())
            .unwrap_or_default()
            .trim();

        if raw_id.is_empty() {
            return Ok(ToolResult::error(
                call_id,
                "Missing memory identifier to forget. Pass memory_id, or 'all' to delete all memories.",
            ));
        }

        self.engine.forget(raw_id).await?;
        Ok(ToolResult::success(
            call_id,
            json!({
                "status": "forgotten",
                "target": raw_id,
            })
            .to_string(),
        ))
    }
}

pub struct MemorySearchTool {
    engine: Arc<dyn MemoryEngine>,
    /// When attached, one query also searches note bodies. A note's pointer card is often only
    /// its title, so without this a note is effectively write-only unless the model guesses
    /// `search_notes`; recall should not depend on that guess.
    notes: Option<Arc<dyn NotesEngine>>,
}

impl MemorySearchTool {
    pub fn new(engine: Arc<dyn MemoryEngine>) -> Self {
        Self {
            engine,
            notes: None,
        }
    }

    pub fn with_notes(mut self, notes: Arc<dyn NotesEngine>) -> Self {
        self.notes = Some(notes);
        self
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search persistent memory and stored notes before answering about people, projects, prior decisions, past work, preferences, or anything that may already be stored. Returns verbatim record text plus full matching notes. Quote it; never paraphrase stored facts. Do not use for pure greenfield coding with no memory relevance."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Short keywords or a question; not a pasted conversation or system prompt"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum records to return (default 5, max 20)"
                }
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
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim();
        if query.is_empty() {
            return Ok(ToolResult::error(call_id, "Missing search query."));
        }
        let limit = args
            .get("limit")
            .and_then(|value| value.as_u64())
            .unwrap_or(5)
            .clamp(1, 20) as usize;
        let records = self.engine.retrieve(query, limit).await?;
        let results: Vec<serde_json::Value> = records
            .iter()
            .map(|record| {
                json!({
                    "id": record.id,
                    "category": record.category.to_string(),
                    "subject": record.subject,
                    "predicate": record.predicate,
                    "object": record.object,
                    "name": record.name,
                    "path": record.path,
                    "text": record.raw_content,
                    "confidence": record.confidence,
                    "updated_at": record.updated_at,
                })
            })
            .collect();

        // A note body is authoritative and verbatim; return the whole thing so one search can
        // answer "what is the build command?" without a second round trip.
        let notes = match &self.notes {
            Some(engine) => match engine.search_notes(query, limit).await {
                Ok(notes) => notes,
                Err(error) => {
                    tracing::warn!("memory_search could not search notes: {}", error);
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        let note_results: Vec<serde_json::Value> = notes
            .iter()
            .map(|note| {
                json!({
                    "id": note.id,
                    "title": note.title,
                    "summary_pointer": note.summary_pointer,
                    "tags": note.tags,
                    "text": note.content,
                    "updated_at": note.updated_at,
                })
            })
            .collect();

        Ok(ToolResult::success(
            call_id,
            json!({
                "query": query,
                "results": results,
                "notes": note_results,
                "count": results.len() + note_results.len(),
            })
            .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteMemoryEngine;
    use harness_notes::FileNotesEngine;

    #[tokio::test]
    async fn memory_search_returns_matching_note_bodies_when_notes_are_attached() {
        let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory"));
        let notes = Arc::new(FileNotesEngine::in_memory().expect("notes"));
        let note = notes
            .save_note(
                "pure-gpt repository brief",
                "Build with `cargo build --release`.\n\nRun the suite with `cargo test --all`.",
                &["pure-gpt".to_string()],
            )
            .await
            .expect("saves the note");
        memory
            .add_note_pointer(&note.id, "pure-gpt repository brief")
            .await
            .expect("saves the pointer");

        let tool = MemorySearchTool::new(memory).with_notes(notes);
        let output = tool
            .execute(
                "call-1",
                &json!({"query": "build command"}),
                &CancellationToken::new(),
            )
            .await
            .expect("searches");

        assert!(!output.is_error, "{}", output.content);
        let body: serde_json::Value = serde_json::from_str(&output.content).expect("json");
        let notes = body["notes"].as_array().expect("notes array");
        assert_eq!(notes.len(), 1, "{body}");
        assert_eq!(notes[0]["id"], note.id);
        assert!(
            notes[0]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("cargo test --all"),
            "the full verbatim note body must come back, not just a pointer: {body}"
        );
        assert_eq!(body["count"], 2, "pointer card plus note: {body}");
    }

    #[tokio::test]
    async fn memory_search_without_notes_does_not_silently_claim_note_results() {
        let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory"));
        let tool = MemorySearchTool::new(memory);
        let output = tool
            .execute(
                "call-1",
                &json!({"query": "anything"}),
                &CancellationToken::new(),
            )
            .await
            .expect("searches");
        let body: serde_json::Value = serde_json::from_str(&output.content).expect("json");
        assert!(body["notes"].as_array().expect("notes array").is_empty());
    }
}
