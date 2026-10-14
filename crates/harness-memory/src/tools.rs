use std::sync::Arc;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, MemoryCategory, MemoryEngine, MemoryRecord, Tool, ToolResult};

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
        "Save a durable fact, rule, user preference, or pattern into persistent long-term memory. Use only when information is valuable across future sessions."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "subject": { "type": "string", "description": "Entity or subject (e.g. 'user', 'project', 'rust_compiler')" },
                "predicate": { "type": "string", "description": "Relationship or property (e.g. 'preferred_language', 'coding_style')" },
                "object": { "type": "string", "description": "Target value or statement (e.g. 'Rust', 'prefers explicit error handling')" },
                "category": {
                    "type": "string",
                    "enum": ["user_profile", "code_pattern", "workflow_rule", "fact", "harness_improvement"],
                    "description": "Category of memory"
                },
                "confidence": { "type": "number", "description": "Confidence score from 0.0 to 1.0 (default 1.0)" }
            },
            "required": ["subject", "predicate", "object", "category"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let subject = args.get("subject").and_then(|s| s.as_str()).unwrap_or_default();
        let predicate = args.get("predicate").and_then(|p| p.as_str()).unwrap_or_default();
        let object = args.get("object").and_then(|o| o.as_str()).unwrap_or_default();
        let cat_str = args.get("category").and_then(|c| c.as_str()).unwrap_or("fact");
        let confidence = args.get("confidence").and_then(|c| c.as_f64()).unwrap_or(1.0) as f32;

        let category = match cat_str {
            "user_profile" => MemoryCategory::UserProfile,
            "code_pattern" => MemoryCategory::CodePattern,
            "workflow_rule" => MemoryCategory::WorkflowRule,
            "harness_improvement" => MemoryCategory::HarnessImprovement,
            _ => MemoryCategory::Fact,
        };

        let id = format!("mem_{}", uuid::Uuid::new_v4());
        let raw_content = format!("{} {} {}", subject, predicate, object);

        let record = MemoryRecord {
            id: id.clone(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
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
        "Update or correct an existing memory record, superseding and invalidating the prior record."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "memory_id": { "type": "string", "description": "ID of memory record being superseded" },
                "subject": { "type": "string", "description": "Updated entity or subject" },
                "predicate": { "type": "string", "description": "Updated property or relationship" },
                "object": { "type": "string", "description": "Updated value or statement" },
                "category": {
                    "type": "string",
                    "enum": ["user_profile", "code_pattern", "workflow_rule", "fact", "harness_improvement"]
                }
            },
            "required": ["memory_id", "subject", "predicate", "object", "category"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let old_id = args.get("memory_id").and_then(|id| id.as_str()).unwrap_or_default();
        let subject = args.get("subject").and_then(|s| s.as_str()).unwrap_or_default();
        let predicate = args.get("predicate").and_then(|p| p.as_str()).unwrap_or_default();
        let object = args.get("object").and_then(|o| o.as_str()).unwrap_or_default();
        let cat_str = args.get("category").and_then(|c| c.as_str()).unwrap_or("fact");

        let category = match cat_str {
            "user_profile" => MemoryCategory::UserProfile,
            "code_pattern" => MemoryCategory::CodePattern,
            "workflow_rule" => MemoryCategory::WorkflowRule,
            "harness_improvement" => MemoryCategory::HarnessImprovement,
            _ => MemoryCategory::Fact,
        };

        let new_id = format!("mem_{}", uuid::Uuid::new_v4());
        let raw_content = format!("{} {} {}", subject, predicate, object);

        let record = MemoryRecord {
            id: new_id.clone(),
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            raw_content,
            category,
            confidence: 1.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        };

        self.engine.update(old_id, &record).await?;
        Ok(ToolResult::success(
            call_id,
            json!({
                "status": "updated",
                "old_memory_id": old_id,
                "new_memory_id": new_id,
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
        "Deactivate an obsolete memory record when it is no longer true or desired."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "memory_id": { "type": "string", "description": "ID of memory record to deactivate" }
            },
            "required": ["memory_id"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let memory_id = args.get("memory_id").and_then(|id| id.as_str()).unwrap_or_default();
        self.engine.forget(memory_id).await?;
        Ok(ToolResult::success(
            call_id,
            json!({
                "status": "deactivated",
                "memory_id": memory_id,
            })
            .to_string(),
        ))
    }
}
