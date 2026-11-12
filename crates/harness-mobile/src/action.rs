use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use harness_ui_spec::Screen;
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActionId(String);

impl ActionId {
    pub fn new(raw: impl Into<String>) -> Result<Self, ActionError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(ActionError::EmptyActionId);
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionCall {
    pub id: ActionId,
    pub args: Value,
}

impl ActionCall {
    pub fn new(id: ActionId, args: Value) -> Self {
        Self { id, args }
    }
}

/// State changes an action reports back to the screen. Keys are opaque state names; the renderer
/// merges them into the screen state after the handler returns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatePatch {
    values: Map<String, Value>,
}

impl StatePatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) -> &mut Self {
        self.values.insert(key.into(), value);
        self
    }

    pub fn values(&self) -> &Map<String, Value> {
        &self.values
    }

    pub fn into_values(self) -> Map<String, Value> {
        self.values
    }
}

#[derive(Debug, Clone)]
pub struct ActionContext {
    pub screen_id: String,
    pub state: Map<String, Value>,
}

#[async_trait]
pub trait ActionHandler: Send + Sync {
    async fn invoke(
        &self,
        call: &ActionCall,
        context: &ActionContext,
    ) -> Result<StatePatch, ActionError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ActionError {
    #[error("action id must not be empty")]
    EmptyActionId,
    #[error("action `{0}` is already registered")]
    DuplicateAction(String),
    #[error("screen `{screen}` does not grant action `{action}`")]
    UnauthorizedAction { screen: String, action: String },
    #[error("action `{0}` has no registered handler")]
    UnknownAction(String),
    #[error("action `{action}` failed: {message}")]
    Failed { action: String, message: String },
}

impl ActionError {
    pub fn failed(action: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Failed {
            action: action.into(),
            message: message.into(),
        }
    }
}

#[derive(Default)]
pub struct ActionRegistry {
    handlers: HashMap<String, Arc<dyn ActionHandler>>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        id: ActionId,
        handler: Arc<dyn ActionHandler>,
    ) -> Result<(), ActionError> {
        if self.handlers.contains_key(id.as_str()) {
            return Err(ActionError::DuplicateAction(id.as_str().to_string()));
        }
        self.handlers.insert(id.as_str().to_string(), handler);
        Ok(())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.handlers.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Enforce the screen's allowlist, then the host's registrations, then run the handler.
    pub async fn dispatch(
        &self,
        screen: &Screen,
        call: &ActionCall,
        state: &Map<String, Value>,
    ) -> Result<StatePatch, ActionError> {
        if !screen
            .capabilities
            .iter()
            .any(|capability| capability == call.id.as_str())
        {
            return Err(ActionError::UnauthorizedAction {
                screen: screen.id.clone(),
                action: call.id.as_str().to_string(),
            });
        }

        let handler = self
            .handlers
            .get(call.id.as_str())
            .ok_or_else(|| ActionError::UnknownAction(call.id.as_str().to_string()))?;

        let context = ActionContext {
            screen_id: screen.id.clone(),
            state: state.clone(),
        };
        handler.invoke(call, &context).await
    }
}
