//! UI-agnostic interactive clarification tool backed by an mpsc/oneshot channel.

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use harness_core::{error::HarnessError, traits::Tool, types::ToolResult};

pub use harness_core::{ClarifyAnswer, ClarifyItem, ClarifyRequest, ClarifyResponse};

/// ClarifyTool enables the assistant to ask interactive multiple-choice
/// and free-text questions to the user during conversation turns.
pub struct ClarifyTool {
    tx: mpsc::UnboundedSender<(ClarifyRequest, oneshot::Sender<ClarifyResponse>)>,
}

impl ClarifyTool {
    pub fn new(
        tx: mpsc::UnboundedSender<(ClarifyRequest, oneshot::Sender<ClarifyResponse>)>,
    ) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Tool for ClarifyTool {
    fn name(&self) -> &str {
        "clarify"
    }

    fn description(&self) -> &str {
        "Ask the user one or more clarifying questions when you need a decision, confirmation, or preference. The user can pick from provided choices or type a custom answer."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "Question to ask the user (for single-question calls)."
                },
                "choices": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of choices for the user to choose from (a 'Type your answer' free-text option is added automatically)."
                },
                "questions": {
                    "type": "array",
                    "description": "Optional list of multiple questions to ask sequentially.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string" },
                            "choices": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["question"]
                    }
                }
            }
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let mut items = Vec::new();

        if let Some(questions_val) = arguments.get("questions").and_then(|v| v.as_array()) {
            for q_val in questions_val {
                if let Some(q_str) = q_val.get("question").and_then(|v| v.as_str()) {
                    let mut choices = Vec::new();
                    if let Some(ch_arr) = q_val.get("choices").and_then(|v| v.as_array()) {
                        for c in ch_arr {
                            if let Some(cs) = c.as_str() {
                                choices.push(cs.to_string());
                            }
                        }
                    }
                    items.push(ClarifyItem {
                        question: q_str.trim().to_string(),
                        choices,
                    });
                }
            }
        }

        if items.is_empty() {
            if let Some(q_str) = arguments.get("question").and_then(|v| v.as_str()) {
                let mut choices = Vec::new();
                if let Some(ch_arr) = arguments.get("choices").and_then(|v| v.as_array()) {
                    for c in ch_arr {
                        if let Some(cs) = c.as_str() {
                            choices.push(cs.to_string());
                        }
                    }
                }
                items.push(ClarifyItem {
                    question: q_str.trim().to_string(),
                    choices,
                });
            }
        }

        if items.is_empty() {
            return Ok(ToolResult::error(
                call_id,
                "clarify requires at least one 'question' or non-empty 'questions' list",
            ));
        }

        let request = ClarifyRequest { questions: items };
        let (response_tx, response_rx) = oneshot::channel();

        if self.tx.send((request, response_tx)).is_err() {
            return Ok(ToolResult::error(
                call_id,
                "interactive UI channel unavailable",
            ));
        }

        tokio::select! {
            _ = cancel_token.cancelled() => {
                Ok(ToolResult::error(
                    call_id,
                    "the question was cancelled before the user answered it",
                ))
            }
            res = response_rx => {
                match res {
                    Ok(resp) => {
                        let serialized = serde_json::to_string(&resp)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(ToolResult::success(call_id, serialized))
                    }
                    Err(_) => {
                        Ok(ToolResult::error(call_id, "clarification request was closed without response"))
                    }
                }
            }
        }
    }
}
