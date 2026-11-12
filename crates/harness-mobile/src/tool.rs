use std::sync::Arc;

use async_trait::async_trait;
use harness_core::{Tool, ToolResult};
use harness_ui_spec::Screen;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::event::EngineEvent;
use crate::screens::UiSpecStore;
use crate::MobileError;

/// Native tool the model calls to create or replace a screen. Invalid specs come back as a tool
/// error rather than a turn failure, so the model can see what it got wrong and retry.
pub struct CreateUiTool {
    screens: Arc<UiSpecStore>,
    events: broadcast::Sender<EngineEvent>,
}

impl CreateUiTool {
    pub fn new(screens: Arc<UiSpecStore>, events: broadcast::Sender<EngineEvent>) -> Self {
        Self { screens, events }
    }
}

#[async_trait]
impl Tool for CreateUiTool {
    fn name(&self) -> &str {
        "create_ui"
    }

    fn description(&self) -> &str {
        "Create or replace a native app screen from a JSON screen specification. Every interactive \
         node must reference an action listed in the screen's capabilities."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "description": "A complete screen specification. See the system prompt for the node catalog.",
            "properties": {
                "schema_version": { "type": "integer", "description": "Optional; defaults to the current schema version." },
                "id": { "type": "string", "description": "Stable screen id. Reuse it to revise the same screen." },
                "title": { "type": "string" },
                "capabilities": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Action ids this screen may dispatch."
                },
                "root": { "type": "object", "description": "Root node; every node has an id and a type." }
            },
            "required": ["id", "title", "root"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, harness_core::HarnessError> {
        let candidate = arguments
            .get("screen")
            .filter(|_| arguments.get("root").is_none())
            .cloned()
            .unwrap_or_else(|| arguments.clone());

        let screen: Screen = match serde_json::from_value(candidate) {
            Ok(screen) => screen,
            Err(error) => {
                return Ok(ToolResult::error(
                    call_id,
                    format!("invalid screen JSON: {error}"),
                ));
            }
        };

        match save_screen(&self.screens, &self.events, screen) {
            Ok(saved) => Ok(ToolResult::success(
                call_id,
                json!({
                    "status": "saved",
                    "screen_id": saved.id,
                    "title": saved.title,
                })
                .to_string(),
            )),
            Err(error) => Ok(ToolResult::error(call_id, error.to_string())),
        }
    }
}

pub(crate) fn save_screen(
    screens: &UiSpecStore,
    events: &broadcast::Sender<EngineEvent>,
    screen: Screen,
) -> Result<Screen, MobileError> {
    let saved = screens.upsert(screen)?;
    if events
        .send(EngineEvent::ScreenSaved(saved.clone()))
        .is_err()
    {
        tracing::trace!(screen = %saved.id, "no engine event subscribers");
    }
    Ok(saved)
}
