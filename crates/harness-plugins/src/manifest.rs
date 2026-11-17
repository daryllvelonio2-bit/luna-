//! DeepSeek-Harness-compatible plugin manifests, UI panel schemas, and sidebar agent entries.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Spec for the automatic sidebar agent every plugin contributes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AutoAgentSpec {
    /// Display name in the sidebar (defaults to plugin name).
    pub name: String,
    /// Icon identifier or emoji for the sidebar.
    pub icon: String,
    /// System prompt injected when chatting with this plugin's sidebar agent.
    pub system_prompt: String,
    /// Optional model override (`provider/model` or model id).
    pub model: Option<String>,
    /// Optional explicit tool allowlist (empty = all plugin tools + core tools).
    pub tools: Vec<String>,
}

impl Default for AutoAgentSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            icon: "puzzle".to_string(),
            system_prompt: String::new(),
            model: None,
            tools: Vec::new(),
        }
    }
}

/// Declarative widget rendered inside a plugin's sidebar agent panel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginWidget {
    StatCard {
        id: String,
        label: String,
        value: String,
        subtitle: Option<String>,
    },
    Table {
        id: String,
        title: String,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    ActionButton {
        id: String,
        label: String,
        action: String,
        variant: Option<String>,
    },
    Markdown {
        id: String,
        content: String,
    },
    KeyValue {
        id: String,
        items: Vec<(String, String)>,
    },
}

/// A UI panel contributed by a plugin (`ctx.ui.registerPanel(...)`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginUiPanel {
    pub plugin_id: String,
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
    #[serde(default)]
    pub widgets: Vec<PluginWidget>,
}

/// Configuration field schema displayed in the plugin settings drawer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginConfigField {
    pub key: String,
    pub label: String,
    #[serde(default = "default_field_type")]
    pub field_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default_value: serde_json::Value,
}

fn default_field_type() -> String {
    "string".to_string()
}

/// A scheduled task declared by a plugin (`ctx.schedules.register(...)`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginScheduleDecl {
    pub plugin_id: String,
    pub id: String,
    pub name: String,
    pub rule: String,
    pub instruction: String,
    #[serde(default = "default_delivery_mode")]
    pub delivery_mode: String,
}

fn default_delivery_mode() -> String {
    "new_session_each_run".to_string()
}

/// A sidebar agent registered automatically (or explicitly via `ctx.agents.register`) for a plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginAgentEntry {
    pub plugin_id: String,
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub system_prompt: String,
    pub model: Option<String>,
    pub registered_tools: Vec<String>,
    pub panel_ids: Vec<String>,
}

/// A slash/palette command registered by a plugin (`ctx.commands.register(...)`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCommandSpec {
    pub plugin_id: String,
    pub name: String,
    pub description: String,
}

/// On-disk manifest (`~/.luna/plugins/<id>/plugin.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default = "default_compiled")]
    pub compiled: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub auto_agent: AutoAgentSpec,
    #[serde(default)]
    pub config_schema: Vec<PluginConfigField>,
    #[serde(default)]
    pub builder_spec: Option<serde_json::Value>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_entry() -> String {
    "src/index.ts".to_string()
}

fn default_compiled() -> String {
    "dist/index.js".to_string()
}

fn default_true() -> bool {
    true
}

/// Runtime summary of a discovered plugin returned to CLI/GUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub loaded: bool,
    pub builtin: bool,
    pub path: PathBuf,
    pub last_error: Option<String>,
    pub tools: Vec<String>,
    pub commands: Vec<PluginCommandSpec>,
    pub agent: Option<PluginAgentEntry>,
    pub panels: Vec<PluginUiPanel>,
    pub schedules: Vec<PluginScheduleDecl>,
    pub settings: BTreeMap<String, serde_json::Value>,
    pub builder_spec: Option<serde_json::Value>,
}
