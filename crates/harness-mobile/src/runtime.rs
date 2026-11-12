use std::collections::HashSet;

use harness_ui_spec::{Node, NodeKind, Screen, SpecError};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::action::{ActionCall, ActionError, ActionId, ActionRegistry, StatePatch};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    InvalidSpec(#[from] SpecError),
    #[error(transparent)]
    Action(#[from] ActionError),
    #[error("state key `{0}` is not declared by the screen")]
    UnknownBind(String),
}

/// A validated screen plus its live state. Every value the renderer displays comes from here, so
/// the native frontend never invents state the spec did not declare.
#[derive(Debug)]
pub struct ScreenRuntime {
    screen: Screen,
    state: Map<String, Value>,
    binds: HashSet<String>,
}

impl ScreenRuntime {
    pub fn new(screen: Screen) -> Result<Self, SpecError> {
        screen.validate()?;

        let mut binds = HashSet::new();
        let mut state = Map::new();
        screen.walk(&mut |node| {
            if let Some(bind) = node.bind() {
                binds.insert(bind.to_string());
            }
            if let Some((bind, value)) = default_value(node) {
                state.entry(bind.to_string()).or_insert(value);
            }
        });

        Ok(Self {
            screen,
            state,
            binds,
        })
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn state(&self) -> &Map<String, Value> {
        &self.state
    }

    pub fn state_value(&self, bind: &str) -> Option<&Value> {
        self.state.get(bind)
    }

    pub fn set_input(&mut self, bind: &str, value: Value) -> Result<(), RuntimeError> {
        if !self.binds.contains(bind) {
            return Err(RuntimeError::UnknownBind(bind.to_string()));
        }
        self.state.insert(bind.to_string(), value);
        Ok(())
    }

    pub async fn dispatch(
        &mut self,
        registry: &ActionRegistry,
        action: &str,
        args: Value,
    ) -> Result<StatePatch, RuntimeError> {
        let call = ActionCall::new(ActionId::new(action)?, args);
        let patch = registry.dispatch(&self.screen, &call, &self.state).await?;
        for (key, value) in patch.values() {
            self.state.insert(key.clone(), value.clone());
        }
        Ok(patch)
    }
}

fn default_value(node: &Node) -> Option<(&str, Value)> {
    match &node.kind {
        NodeKind::Input { bind, value, .. } | NodeKind::Select { bind, value, .. } => {
            Some((bind, value.clone().map_or(Value::Null, Value::String)))
        }
        NodeKind::Toggle { bind, value, .. } => Some((bind, Value::Bool(*value))),
        _ => None,
    }
}
