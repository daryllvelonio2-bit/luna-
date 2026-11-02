use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::{
    HarnessError, MemoryCategory, MemoryEngine, MemoryRecord, Tool, ToolPreview, ToolResult,
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
