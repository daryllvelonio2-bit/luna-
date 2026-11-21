//! Native agent tools for creating, listing, reloading, and removing Luna plugins.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, Tool, ToolRegistry, ToolResult};

use crate::builder::slugify_plugin_id;
use crate::manager::PluginManager;
use crate::manifest::{AutoAgentSpec, PluginConfigField, PluginManifest};

pub struct PluginCreateTool {
    manager: Arc<PluginManager>,
}

impl PluginCreateTool {
    pub fn new(manager: Arc<PluginManager>) -> Self {
        Self { manager }
    }
}

#[derive(Deserialize)]
struct AutoAgentInput {
    name: Option<String>,
    icon: Option<String>,
    system_prompt: Option<String>,
    model: Option<String>,
    #[serde(default)]
    tools: Vec<String>,
}

#[derive(Deserialize)]
struct CreatePluginArgs {
    id: String,
    name: String,
    description: String,
    ts_source: String,
    version: Option<String>,
    scope: Option<String>,
    auto_agent: Option<AutoAgentInput>,
    #[serde(default)]
    config_schema: Vec<PluginConfigField>,
}

#[async_trait]
impl Tool for PluginCreateTool {
    fn name(&self) -> &str {
        "plugin_create"
    }

    fn description(&self) -> &str {
        "Create or update a Luna TypeScript plugin (`plugin.json` + `src/index.ts`), transpile it in-process, and load it into the embedded QuickJS runtime immediately."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Lowercase slug plugin ID (e.g. 'github-issues', 'git-cleanup')"
                },
                "name": {
                    "type": "string",
                    "description": "Human-readable plugin display name"
                },
                "description": {
                    "type": "string",
                    "description": "Concise 1-line summary of what the plugin provides"
                },
                "ts_source": {
                    "type": "string",
                    "description": "Complete TypeScript source for src/index.ts importing `type { Context } from '@luna/sdk'` and exporting `name` and `apply(ctx: Context)`"
                },
                "version": {
                    "type": "string",
                    "description": "Optional semver string (default '0.1.0')"
                },
                "scope": {
                    "type": "string",
                    "enum": ["global", "workspace"],
                    "description": "Where to install: 'global' (~/.luna/plugins, default) or 'workspace' (<workspace>/.luna/plugins)"
                },
                "auto_agent": {
                    "type": "object",
                    "description": "Optional sidebar agent metadata",
                    "properties": {
                        "name": { "type": "string" },
                        "icon": { "type": "string" },
                        "system_prompt": { "type": "string" },
                        "model": { "type": "string" },
                        "tools": { "type": "array", "items": { "type": "string" } }
                    }
                },
                "config_schema": {
                    "type": "array",
                    "description": "Optional settings schema fields for this plugin",
                    "items": {
                        "type": "object",
                        "properties": {
                            "key": { "type": "string" },
                            "label": { "type": "string" },
                            "field_type": { "type": "string" },
                            "required": { "type": "boolean" },
                            "default_value": {},
                            "description": { "type": "string" }
                        },
                        "required": ["key", "label"]
                    }
                }
            },
            "required": ["id", "name", "description", "ts_source"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let args: CreatePluginArgs = match serde_json::from_value(arguments.clone()) {
            Ok(a) => a,
            Err(e) => return Ok(ToolResult::error(call_id, format!("invalid arguments: {e}"))),
        };

        let id = slugify_plugin_id(&args.id);
        let name = args.name.trim().to_string();
        if name.is_empty() {
            return Ok(ToolResult::error(call_id, "plugin 'name' cannot be empty"));
        }
        if args.ts_source.trim().is_empty() {
            return Ok(ToolResult::error(
                call_id,
                "plugin 'ts_source' cannot be empty",
            ));
        }

        let workspace_scope = matches!(
            args.scope.as_deref().map(str::trim),
            Some(s) if s.eq_ignore_ascii_case("workspace")
        );

        let auto_agent = match args.auto_agent {
            Some(a) => AutoAgentSpec {
                name: a
                    .name
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| name.clone()),
                icon: a
                    .icon
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "sparkles".to_string()),
                system_prompt: a.system_prompt.unwrap_or_default(),
                model: a.model.filter(|s| !s.trim().is_empty()),
                tools: a.tools,
            },
            None => AutoAgentSpec {
                name: name.clone(),
                icon: "sparkles".to_string(),
                system_prompt: String::new(),
                model: None,
                tools: Vec::new(),
            },
        };

        let manifest = PluginManifest {
            id: id.clone(),
            name,
            version: args
                .version
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "0.1.0".to_string()),
            description: args.description.trim().to_string(),
            entry: "src/index.ts".to_string(),
            compiled: "dist/index.js".to_string(),
            enabled: true,
            builtin: false,
            auto_agent,
            config_schema: args.config_schema,
            builder_spec: None,
        };

        match self
            .manager
            .install_raw_in_scope(manifest, &args.ts_source, workspace_scope)
        {
            Ok(summary) => {
                if let Some(err) = &summary.last_error {
                    return Ok(ToolResult::error(
                        call_id,
                        format!(
                            "Plugin '{}' compiled and wrote files at {} but failed to load in QuickJS: {}",
                            summary.id,
                            summary.path.display(),
                            err
                        ),
                    ));
                }
                let out = serde_json::to_string_pretty(&summary)
                    .unwrap_or_else(|_| format!("Plugin '{}' created and loaded.", summary.id));
                Ok(ToolResult::success(call_id, out))
            }
            Err(err) => Ok(ToolResult::error(call_id, err)),
        }
    }
}

pub struct PluginListTool {
    manager: Arc<PluginManager>,
}

impl PluginListTool {
    pub fn new(manager: Arc<PluginManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for PluginListTool {
    fn name(&self) -> &str {
        "plugin_list"
    }

    fn description(&self) -> &str {
        "List all installed Luna plugins along with their load status, tools, commands, sidebar agent, and errors."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        _arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let summaries = self.manager.list_summaries();
        let out = serde_json::to_string_pretty(&summaries).unwrap_or_else(|_| "[]".to_string());
        Ok(ToolResult::success(call_id, out))
    }
}

pub struct PluginReloadTool {
    manager: Arc<PluginManager>,
}

impl PluginReloadTool {
    pub fn new(manager: Arc<PluginManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for PluginReloadTool {
    fn name(&self) -> &str {
        "plugin_reload"
    }

    fn description(&self) -> &str {
        "Recompile and reload a plugin by ID (or rescan all plugins if id is omitted)."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Optional plugin ID to reload; omit to rediscover and reload all plugins"
                }
            }
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let id = arguments
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        match id {
            Some(plugin_id) => match self.manager.reload(plugin_id) {
                Ok(summary) => {
                    let out = serde_json::to_string_pretty(&summary)
                        .unwrap_or_else(|_| format!("Reloaded '{plugin_id}'"));
                    Ok(ToolResult::success(call_id, out))
                }
                Err(e) => Ok(ToolResult::error(call_id, e)),
            },
            None => match self.manager.discover_and_load() {
                Ok(()) => {
                    let summaries = self.manager.list_summaries();
                    let out = serde_json::to_string_pretty(&summaries)
                        .unwrap_or_else(|_| "[]".to_string());
                    Ok(ToolResult::success(call_id, out))
                }
                Err(e) => Ok(ToolResult::error(call_id, e)),
            },
        }
    }
}

pub struct PluginRemoveTool {
    manager: Arc<PluginManager>,
}

impl PluginRemoveTool {
    pub fn new(manager: Arc<PluginManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for PluginRemoveTool {
    fn name(&self) -> &str {
        "plugin_remove"
    }

    fn description(&self) -> &str {
        "Uninstall and delete a user-created plugin by ID (cannot remove built-in plugins)."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Plugin ID to remove"
                }
            },
            "required": ["id"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let Some(id) = arguments
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(ToolResult::error(call_id, "plugin 'id' is required"));
        };

        if let Some(summary) = self.manager.get_summary(id) {
            if summary.builtin {
                return Ok(ToolResult::error(
                    call_id,
                    format!("cannot remove built-in plugin '{id}'"),
                ));
            }
        }

        match self.manager.remove(id) {
            Ok(()) => Ok(ToolResult::success(
                call_id,
                format!("Removed plugin '{id}'."),
            )),
            Err(e) => Ok(ToolResult::error(call_id, e)),
        }
    }
}

/// Register the built-in plugin creation & management tools (`plugin_create`,
/// `plugin_list`, `plugin_reload`, `plugin_remove`) onto `registry`.
pub async fn register_plugin_tools(
    registry: &Arc<dyn ToolRegistry>,
    manager: Arc<PluginManager>,
) -> Result<(), HarnessError> {
    registry
        .register(Arc::new(PluginCreateTool::new(manager.clone())))
        .await?;
    registry
        .register(Arc::new(PluginListTool::new(manager.clone())))
        .await?;
    registry
        .register(Arc::new(PluginReloadTool::new(manager.clone())))
        .await?;
    registry
        .register(Arc::new(PluginRemoveTool::new(manager)))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginSummary;

    #[tokio::test]
    async fn plugin_create_tool_compiles_loads_and_lists_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_tmp = tempfile::tempdir().unwrap();
        let manager = Arc::new(PluginManager::new(
            tmp.path().to_path_buf(),
            Some(ws_tmp.path().to_path_buf()),
        ));

        let create = PluginCreateTool::new(manager.clone());
        let list = PluginListTool::new(manager.clone());
        let cancel = CancellationToken::new();

        let ts = r#"
import type { Context } from '@luna/sdk';

export const name = 'greeter';

export function apply(ctx: Context): void {
  ctx.tools.register(
    {
      name: 'greet_user',
      description: 'Greet a person by name',
      mutating: false,
      parameters: {
        type: 'object',
        properties: { person: { type: 'string' } },
        required: ['person']
      }
    },
    (args: Record<string, unknown>) => {
      return `Hello, ${String(args.person || 'world')}!`;
    }
  );
  ctx.agents.register({
    id: 'greeter',
    name: 'Greeter',
    description: 'Friendly greeter agent',
    icon: 'sparkles',
    systemPrompt: 'Always use greet_user.',
    tools: ['greet_user']
  });
}
"#;

        let res = create
            .execute(
                "c1",
                &json!({
                    "id": "Greeter Plugin",
                    "name": "Greeter",
                    "description": "Friendly greeter",
                    "ts_source": ts
                }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(!res.is_error, "unexpected error: {}", res.content);
        let summary: PluginSummary = serde_json::from_str(&res.content).unwrap();
        assert_eq!(summary.id, "greeter-plugin");
        assert!(summary.loaded);
        assert!(summary.last_error.is_none());
        assert_eq!(summary.tools, vec!["greet_user".to_string()]);

        let list_res = list.execute("c2", &json!({}), &cancel).await.unwrap();
        assert!(!list_res.is_error);
        assert!(list_res.content.contains("greeter-plugin"));

        let tools = manager.registered_tools();
        assert_eq!(tools.len(), 1);
        let call = tools[0]
            .execute("c3", &json!({ "person": "Janelle" }), &cancel)
            .await
            .unwrap();
        assert!(!call.is_error);
        assert_eq!(call.content, "Hello, Janelle!");
    }

    #[tokio::test]
    async fn plugin_create_tool_reports_ts_compile_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = Arc::new(PluginManager::new(tmp.path().to_path_buf(), None));
        let create = PluginCreateTool::new(manager);
        let cancel = CancellationToken::new();

        let res = create
            .execute(
                "c1",
                &json!({
                    "id": "broken",
                    "name": "Broken",
                    "description": "Bad syntax",
                    "ts_source": "export function apply(ctx: { broken"
                }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(res.is_error);
        assert!(res.content.contains("syntax error"));
    }
}
