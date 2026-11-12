use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{SpecError, SpecLoadError};
use crate::validate;

/// Schema revision understood by this build. A renderer must reject any other value rather than
/// guess at fields it was not written for.
pub const SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

/// A single screen: the unit the agent generates, stores, and the app renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    /// Action ids this screen is allowed to dispatch. A node may only reference an action listed
    /// here; everything else is rejected before it reaches a handler.
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub root: Node,
}

impl Screen {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        capabilities: Vec<String>,
        root: Node,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: id.into(),
            title: title.into(),
            capabilities,
            root,
        }
    }

    pub fn validate(&self) -> Result<(), SpecError> {
        validate::validate(self)
    }

    pub fn from_json(raw: &str) -> Result<Self, SpecLoadError> {
        let screen: Self = serde_json::from_str(raw)?;
        screen.validate()?;
        Ok(screen)
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Node)) {
        self.root.walk(visit);
    }

    /// State keys written by inputs/selects/toggles/tables/repeats/conditionals, in document order.
    pub fn declared_binds(&self) -> Vec<&str> {
        let mut binds = Vec::new();
        self.walk(&mut |node| {
            if let Some(bind) = node.bind() {
                binds.push(bind);
            }
        });
        binds
    }
}

/// One node in the screen tree. `id` is stable across regenerations so the renderer can preserve
/// scroll position and focus when the agent edits a screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(flatten)]
    pub kind: NodeKind,
}

impl Node {
    pub fn new(id: impl Into<String>, kind: NodeKind) -> Self {
        Self {
            id: id.into(),
            kind,
        }
    }

    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Node)) {
        visit(self);
        match &self.kind {
            NodeKind::Conditional {
                children,
                otherwise,
                ..
            } => {
                for child in children.iter().chain(otherwise.iter()) {
                    child.walk(visit);
                }
            }
            _ => {
                for child in self.children() {
                    child.walk(visit);
                }
            }
        }
    }

    pub fn children(&self) -> &[Node] {
        match &self.kind {
            NodeKind::Column { children }
            | NodeKind::Row { children }
            | NodeKind::Repeat { children, .. } => children,
            NodeKind::Conditional { children, .. } => children,
            _ => &[],
        }
    }

    /// State key this node reads or writes, if any.
    pub fn bind(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Input { bind, .. }
            | NodeKind::Select { bind, .. }
            | NodeKind::Toggle { bind, .. }
            | NodeKind::Table { bind, .. }
            | NodeKind::Repeat { bind, .. }
            | NodeKind::Conditional { bind, .. } => Some(bind),
            _ => None,
        }
    }

    /// Action id this node dispatches, if any.
    pub fn action(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Button { action, .. } => Some(action),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextRole {
    #[default]
    Body,
    Heading,
    Caption,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    Column {
        #[serde(default)]
        children: Vec<Node>,
    },
    Row {
        #[serde(default)]
        children: Vec<Node>,
    },
    Text {
        text: String,
        #[serde(default)]
        role: TextRole,
    },
    Input {
        bind: String,
        label: String,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        value: Option<String>,
    },
    Select {
        bind: String,
        label: String,
        options: Vec<SelectOption>,
        #[serde(default)]
        value: Option<String>,
    },
    Toggle {
        bind: String,
        label: String,
        #[serde(default)]
        value: bool,
    },
    Button {
        label: String,
        action: String,
    },
    Table {
        bind: String,
        columns: Vec<Column>,
        #[serde(default)]
        empty_text: Option<String>,
    },
    Image {
        src: String,
        #[serde(default)]
        alt: Option<String>,
    },
    Repeat {
        bind: String,
        #[serde(default)]
        children: Vec<Node>,
    },
    Conditional {
        bind: String,
        equals: Value,
        #[serde(default)]
        children: Vec<Node>,
        #[serde(default)]
        otherwise: Vec<Node>,
    },
    Spacer {
        #[serde(default)]
        size: Option<u16>,
    },
}
