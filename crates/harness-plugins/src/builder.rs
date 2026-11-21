//! Guided no-code plugin builder -> TypeScript DeepSeek-style plugin package generator.

use serde::{Deserialize, Serialize};

use crate::manifest::{AutoAgentSpec, PluginConfigField, PluginManifest};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuilderToolAction {
    /// Run a local shell command template (supports `{{arg}}` interpolation).
    ShellCommand { command_template: String },
    /// Perform an HTTP request using `ctx.net.fetch`.
    HttpRequest {
        method: String,
        url_template: String,
        body_template: Option<String>,
    },
    /// Read a file on disk using `ctx.fs.readText`.
    ReadFile { path_template: String },
    /// Write a file on disk using `ctx.fs.writeText`.
    WriteFile {
        path_template: String,
        content_template: String,
    },
    /// Store or fetch a value in the plugin's persistent KV store.
    StorageKv {
        operation: String, // "get" | "set" | "delete" | "list"
        key_template: String,
        value_template: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuilderParamDef {
    pub name: String,
    pub description: String,
    #[serde(default = "default_param_type")]
    pub param_type: String,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_param_type() -> String {
    "string".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuilderToolDef {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<BuilderParamDef>,
    pub action: BuilderToolAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuilderScheduleDef {
    pub id: String,
    pub name: String,
    pub rule: String,
    pub instruction: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuilderPanelWidgetDef {
    pub id: String,
    pub label: String,
    pub kind: String, // "stat" | "markdown" | "action"
    pub value_or_content: String,
    pub action_command: Option<String>,
}

/// Complete specification submitted by the GUI's Guided Plugin Builder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginBuilderSpec {
    pub id: String,
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    pub description: String,
    #[serde(default = "default_icon")]
    pub icon: String,
    pub system_prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tools: Vec<BuilderToolDef>,
    #[serde(default)]
    pub schedules: Vec<BuilderScheduleDef>,
    #[serde(default)]
    pub widgets: Vec<BuilderPanelWidgetDef>,
    #[serde(default)]
    pub config_fields: Vec<PluginConfigField>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_icon() -> String {
    "sparkles".to_string()
}

#[derive(Debug, Clone)]
pub struct GeneratedPluginFiles {
    pub manifest: PluginManifest,
    pub ts_source: String,
}

/// Sanitize a user-entered plugin name into a safe lowercase slug id.
pub fn slugify_plugin_id(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "custom-plugin".to_string()
    } else {
        out
    }
}

/// Generate a complete DeepSeek-style TypeScript plugin (`plugin.json` + `src/index.ts`)
/// from a Guided Builder form specification.
pub fn generate_plugin_files(spec: &PluginBuilderSpec) -> Result<GeneratedPluginFiles, String> {
    let id = slugify_plugin_id(&spec.id);
    if spec.name.trim().is_empty() {
        return Err("Plugin name must not be empty".to_string());
    }

    let manifest = PluginManifest {
        id: id.clone(),
        name: spec.name.trim().to_string(),
        version: if spec.version.trim().is_empty() {
            "0.1.0".to_string()
        } else {
            spec.version.trim().to_string()
        },
        description: spec.description.trim().to_string(),
        entry: "src/index.ts".to_string(),
        compiled: "dist/index.js".to_string(),
        ui_entry: None,
        enabled: true,
        builtin: false,
        auto_agent: AutoAgentSpec {
            name: spec.name.trim().to_string(),
            icon: if spec.icon.trim().is_empty() {
                "sparkles".to_string()
            } else {
                spec.icon.trim().to_string()
            },
            system_prompt: spec.system_prompt.trim().to_string(),
            model: spec.model.clone().filter(|m| !m.trim().is_empty()),
            tools: spec.tools.iter().map(|t| t.name.clone()).collect(),
        },
        config_schema: spec.config_fields.clone(),
        builder_spec: serde_json::to_value(spec).ok(),
    };

    let mut ts = String::new();
    ts.push_str("// Generated by Luna Guided Plugin Builder\n");
    ts.push_str("import type { Context } from '@luna/sdk';\n\n");
    ts.push_str(&format!(
        "export const name = {};\n\n",
        serde_json::to_string(&id).unwrap()
    ));
    ts.push_str("function interpolate(tpl: string, args: Record<string, unknown>): string {\n");
    ts.push_str("  return tpl.replace(/\\{\\{\\s*([a-zA-Z0-9_]+)\\s*\\}\\}/g, (_, k) => {\n");
    ts.push_str("    const v = args[k];\n");
    ts.push_str("    return v === undefined || v === null ? '' : String(v);\n");
    ts.push_str("  });\n");
    ts.push_str("}\n\n");
    ts.push_str("export function apply(ctx: Context): void {\n");

    // Register automatic sidebar agent
    let tool_names: Vec<String> = spec.tools.iter().map(|t| t.name.clone()).collect();
    ts.push_str(&format!(
        "  ctx.agents.register({{\n    id: {},\n    name: {},\n    description: {},\n    icon: {},\n    systemPrompt: {},\n    model: {},\n    tools: {},\n    panelId: {}\n  }});\n\n",
        serde_json::to_string(&id).unwrap(),
        serde_json::to_string(&manifest.auto_agent.name).unwrap(),
        serde_json::to_string(&manifest.description).unwrap(),
        serde_json::to_string(&manifest.auto_agent.icon).unwrap(),
        serde_json::to_string(&manifest.auto_agent.system_prompt).unwrap(),
        serde_json::to_string(&manifest.auto_agent.model).unwrap(),
        serde_json::to_string(&tool_names).unwrap(),
        if spec.widgets.is_empty() {
            "null".to_string()
        } else {
            serde_json::to_string(&format!("{id}-panel")).unwrap()
        }
    ));

    // Register tools
    for tool in &spec.tools {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for p in &tool.parameters {
            props.insert(
                p.name.clone(),
                serde_json::json!({
                    "type": p.param_type,
                    "description": p.description,
                }),
            );
            if p.required {
                required.push(serde_json::Value::String(p.name.clone()));
            }
        }
        let schema = serde_json::json!({
            "type": "object",
            "properties": props,
            "required": required,
        });

        ts.push_str(&format!(
            "  ctx.tools.register({{\n    name: {},\n    description: {},\n    parameters: {}\n  }}, (args: Record<string, unknown>) => {{\n",
            serde_json::to_string(&tool.name).unwrap(),
            serde_json::to_string(&tool.description).unwrap(),
            serde_json::to_string(&schema).unwrap(),
        ));

        match &tool.action {
            BuilderToolAction::ShellCommand { command_template } => {
                ts.push_str(&format!(
                    "    const cmd = interpolate({}, args);\n",
                    serde_json::to_string(command_template).unwrap()
                ));
                ts.push_str("    const res = ctx.process.exec('sh', ['-c', cmd]);\n");
                ts.push_str("    return JSON.stringify(res);\n");
            }
            BuilderToolAction::HttpRequest {
                method,
                url_template,
                body_template,
            } => {
                ts.push_str(&format!(
                    "    const url = interpolate({}, args);\n",
                    serde_json::to_string(url_template).unwrap()
                ));
                let body_expr = match body_template {
                    Some(b) => format!("interpolate({}, args)", serde_json::to_string(b).unwrap()),
                    None => "undefined".to_string(),
                };
                ts.push_str(&format!(
                    "    const res = ctx.net.fetch(url, {{ method: {}, body: {} }});\n",
                    serde_json::to_string(method).unwrap(),
                    body_expr
                ));
                ts.push_str("    return JSON.stringify(res);\n");
            }
            BuilderToolAction::ReadFile { path_template } => {
                ts.push_str(&format!(
                    "    const p = interpolate({}, args);\n",
                    serde_json::to_string(path_template).unwrap()
                ));
                ts.push_str("    return ctx.fs.readText(p);\n");
            }
            BuilderToolAction::WriteFile {
                path_template,
                content_template,
            } => {
                ts.push_str(&format!(
                    "    const p = interpolate({}, args);\n",
                    serde_json::to_string(path_template).unwrap()
                ));
                ts.push_str(&format!(
                    "    const c = interpolate({}, args);\n",
                    serde_json::to_string(content_template).unwrap()
                ));
                ts.push_str("    ctx.fs.writeText(p, c);\n");
                ts.push_str("    return 'Written to ' + p;\n");
            }
            BuilderToolAction::StorageKv {
                operation,
                key_template,
                value_template,
            } => {
                ts.push_str(&format!(
                    "    const k = interpolate({}, args);\n",
                    serde_json::to_string(key_template).unwrap()
                ));
                match operation.as_str() {
                    "set" => {
                        let v_expr = match value_template {
                            Some(v) => {
                                format!("interpolate({}, args)", serde_json::to_string(v).unwrap())
                            }
                            None => "JSON.stringify(args)".to_string(),
                        };
                        ts.push_str(&format!("    ctx.storage.set(k, {v_expr});\n"));
                        ts.push_str("    return 'Saved ' + k;\n");
                    }
                    "delete" => {
                        ts.push_str("    return String(ctx.storage.delete(k));\n");
                    }
                    "list" => {
                        ts.push_str("    return JSON.stringify(ctx.storage.list());\n");
                    }
                    _ => {
                        ts.push_str("    return JSON.stringify(ctx.storage.get(k));\n");
                    }
                }
            }
        }
        ts.push_str("  });\n\n");
    }

    // Register schedules
    for sched in &spec.schedules {
        ts.push_str(&format!(
            "  ctx.schedules.register({{\n    id: {},\n    name: {},\n    rule: {},\n    instruction: {}\n  }});\n\n",
            serde_json::to_string(&sched.id).unwrap(),
            serde_json::to_string(&sched.name).unwrap(),
            serde_json::to_string(&sched.rule).unwrap(),
            serde_json::to_string(&sched.instruction).unwrap(),
        ));
    }

    // Register UI panel if widgets exist
    if !spec.widgets.is_empty() {
        let widgets_json: Vec<serde_json::Value> = spec
            .widgets
            .iter()
            .map(|w| match w.kind.as_str() {
                "stat" => serde_json::json!({
                    "kind": "stat_card",
                    "id": w.id,
                    "label": w.label,
                    "value": w.value_or_content,
                    "subtitle": null
                }),
                "action" => serde_json::json!({
                    "kind": "action_button",
                    "id": w.id,
                    "label": w.label,
                    "action": w.action_command.clone().unwrap_or_else(|| w.id.clone()),
                    "variant": "primary"
                }),
                _ => serde_json::json!({
                    "kind": "markdown",
                    "id": w.id,
                    "content": w.value_or_content
                }),
            })
            .collect();
        ts.push_str(&format!(
            "  ctx.ui.registerPanel({{\n    id: {},\n    title: {},\n    icon: {},\n    widgets: {}\n  }});\n",
            serde_json::to_string(&format!("{id}-panel")).unwrap(),
            serde_json::to_string(&manifest.name).unwrap(),
            serde_json::to_string(&manifest.auto_agent.icon).unwrap(),
            serde_json::to_string(&widgets_json).unwrap(),
        ));
    }

    ts.push_str("}\n");

    Ok(GeneratedPluginFiles {
        manifest,
        ts_source: ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_generates_valid_compilable_typescript() {
        let spec = PluginBuilderSpec {
            id: "My Daily Weather".to_string(),
            name: "Daily Weather".to_string(),
            version: "0.1.0".to_string(),
            description: "Checks local weather".to_string(),
            icon: "cloud".to_string(),
            system_prompt: "You help check weather.".to_string(),
            model: None,
            tools: vec![BuilderToolDef {
                name: "fetch_weather".to_string(),
                description: "Fetch weather for a city".to_string(),
                parameters: vec![BuilderParamDef {
                    name: "city".to_string(),
                    description: "City name".to_string(),
                    param_type: "string".to_string(),
                    required: true,
                }],
                action: BuilderToolAction::HttpRequest {
                    method: "GET".to_string(),
                    url_template: "https://wttr.in/{{city}}?format=3".to_string(),
                    body_template: None,
                },
            }],
            schedules: vec![BuilderScheduleDef {
                id: "morning-weather".to_string(),
                name: "Morning Weather".to_string(),
                rule: "every:86400".to_string(),
                instruction: "Fetch weather for Manila and summarize.".to_string(),
            }],
            widgets: vec![BuilderPanelWidgetDef {
                id: "status".to_string(),
                label: "Status".to_string(),
                kind: "stat".to_string(),
                value_or_content: "Ready".to_string(),
                action_command: None,
            }],
            config_fields: Vec::new(),
        };

        let gen = generate_plugin_files(&spec).expect("generate");
        assert_eq!(gen.manifest.id, "my-daily-weather");
        let js = crate::compiler::compile_typescript("src/index.ts", &gen.ts_source)
            .expect("generated TS must compile cleanly");
        assert!(js.contains("export function apply(ctx)"));
    }
}
