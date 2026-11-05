//! The desktop tools: the agent's hands on the desktop.
//!
//! Each desktop command is one tool, so the model can call it by name and the
//! approval gate sees exactly which command is about to run.

pub mod params;
pub mod run;

use std::sync::Arc;

use async_trait::async_trait;
use harness_core::{HarnessError, Tool, ToolPreview, ToolRegistry, ToolResult};
use harness_desktop::DesktopEngine;
use tokio_util::sync::CancellationToken;

pub use params::schema;
pub use run::DesktopCommand;

/// One desktop command, as a tool.
pub struct DesktopTool {
    name: &'static str,
    description: &'static str,
    command: DesktopCommand,
    engine: Arc<DesktopEngine>,
}

impl DesktopTool {
    pub fn new(engine: Arc<DesktopEngine>, command: DesktopCommand) -> Self {
        Self {
            name: command.tool_name(),
            description: command.description(),
            command,
            engine,
        }
    }
}

#[async_trait]
impl Tool for DesktopTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters(&self) -> serde_json::Value {
        schema(&self.command)
    }

    /// Every command that changes the screen goes through the approval gate.
    fn is_mutating(&self) -> bool {
        self.command.is_mutating()
    }

    async fn preview(&self, arguments: &serde_json::Value) -> Option<ToolPreview> {
        let target = self.command.preview_target(arguments)?;
        Some(ToolPreview {
            target: Some(target),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let engine = self.engine.clone();
        let command = self.command;
        let arguments = arguments.clone();
        // The bus calls block, and a blocking call on the runtime's own thread
        // would stall every other task in the session.
        let response = tokio::task::spawn_blocking(move || {
            run::execute(&engine, &command, &arguments)
        })
        .await
        .map_err(|error| {
            HarnessError::ToolExecutionFailed {
                tool: command.tool_name().to_string(),
                reason: format!("the desktop task did not finish: {error}"),
            }
        })?;

        if response.ok {
            Ok(ToolResult::success(call_id, response.to_json()))
        } else {
            let error = response.error.expect("an error envelope");
            Ok(ToolResult::error(
                call_id,
                format!("{}: {}", error.code, error.message),
            ))
        }
    }
}

/// Register every desktop tool with the registry.
pub async fn register(registry: &dyn ToolRegistry) -> Result<(), HarnessError> {
    let engine = super::engine::engine();
    for command in DesktopCommand::all() {
        registry
            .register(Arc::new(DesktopTool::new(engine.clone(), command)))
            .await?;
    }
    Ok(())
}
