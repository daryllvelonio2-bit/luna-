//! Model-facing profile management tools:
//! `profile_list`, `profile_view`, `profile_create`, `profile_edit`, and `profile_delete`.
//!
//! Enables Luna agents (especially during `/automation` planning or self-tuning) to inspect,
//! create, and customize specialized Luna profiles (`~/.luna/profiles/<name>/profile.md` +
//! optional `config.yaml`) with distinct personas, mindsets, working rules, and model overrides.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::HarnessError;
use crate::profiles::{
    self, default_profile, load as load_profile, profile_dir, render as render_profile,
    validate_name, Profile, ProfileText, DEFAULT_PROFILE, PROFILE_FILE,
};
use crate::traits::{Tool, ToolRegistry};
use crate::types::ToolResult;

#[derive(Debug, Clone)]
pub struct ProfileToolContext {
    pub profiles_dir: PathBuf,
    pub credentials_path: PathBuf,
    pub allow_mutation: bool,
}

impl ProfileToolContext {
    pub fn from_config(config: &Config, allow_mutation: bool) -> Self {
        Self {
            profiles_dir: config.paths.profiles_dir.clone(),
            credentials_path: config.paths.credentials.clone(),
            allow_mutation,
        }
    }
}

/// Register `profile_list`, `profile_view`, `profile_create`, `profile_edit`, and `profile_delete`.
pub async fn register_profile_tools(
    registry: &Arc<dyn ToolRegistry>,
    config: &Config,
    allow_mutation: bool,
) -> Result<(), HarnessError> {
    let ctx = ProfileToolContext::from_config(config, allow_mutation);
    registry
        .register(Arc::new(ProfileListTool { ctx: ctx.clone() }))
        .await?;
    registry
        .register(Arc::new(ProfileViewTool { ctx: ctx.clone() }))
        .await?;
    registry
        .register(Arc::new(ProfileCreateTool { ctx: ctx.clone() }))
        .await?;
    registry
        .register(Arc::new(ProfileEditTool { ctx: ctx.clone() }))
        .await?;
    registry
        .register(Arc::new(ProfileDeleteTool { ctx }))
        .await?;
    Ok(())
}

#[derive(Debug, Default)]
struct ProfileYamlInfo {
    provider: Option<String>,
    model: Option<String>,
    max_consecutive_failures: Option<usize>,
    raw_yaml: Option<String>,
}

fn read_profile_yaml(dir: &Path) -> ProfileYamlInfo {
    let path = dir.join("config.yaml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return ProfileYamlInfo::default();
    };
    let parsed: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Null);
    let provider = parsed
        .get("model")
        .and_then(|m| m.get("provider"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let model = parsed
        .get("model")
        .and_then(|m| m.get("default"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let max_consecutive_failures = parsed
        .get("agent")
        .and_then(|a| a.get("max_consecutive_failures"))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    ProfileYamlInfo {
        provider,
        model,
        max_consecutive_failures,
        raw_yaml: Some(raw),
    }
}

fn write_profile_yaml(
    dir: &Path,
    provider: Option<&str>,
    model: Option<&str>,
    max_consecutive_failures: Option<usize>,
    clear_model_override: bool,
) -> Result<(), String> {
    let path = dir.join("config.yaml");
    let mut root = if let Ok(raw) = std::fs::read_to_string(&path) {
        serde_yaml::from_str::<serde_yaml::Value>(&raw)
            .unwrap_or_else(|_| serde_yaml::Value::Mapping(Default::default()))
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };
    if !root.is_mapping() {
        root = serde_yaml::Value::Mapping(Default::default());
    }
    let map = root.as_mapping_mut().expect("mapping");

    if clear_model_override {
        map.remove(serde_yaml::Value::String("model".to_string()));
    } else if provider.is_some() || model.is_some() {
        let model_key = serde_yaml::Value::String("model".to_string());
        let entry = map
            .entry(model_key)
            .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
        if !entry.is_mapping() {
            *entry = serde_yaml::Value::Mapping(Default::default());
        }
        let m_map = entry.as_mapping_mut().expect("model mapping");
        if let Some(p) = provider {
            let trimmed = p.trim();
            if trimmed.is_empty() {
                m_map.remove(serde_yaml::Value::String("provider".to_string()));
            } else {
                m_map.insert(
                    serde_yaml::Value::String("provider".to_string()),
                    serde_yaml::Value::String(trimmed.to_string()),
                );
            }
        }
        if let Some(m) = model {
            let trimmed = m.trim();
            if trimmed.is_empty() {
                m_map.remove(serde_yaml::Value::String("default".to_string()));
            } else {
                m_map.insert(
                    serde_yaml::Value::String("default".to_string()),
                    serde_yaml::Value::String(trimmed.to_string()),
                );
            }
        }
    }

    if let Some(max_fail) = max_consecutive_failures {
        let agent_key = serde_yaml::Value::String("agent".to_string());
        let entry = map
            .entry(agent_key)
            .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
        if !entry.is_mapping() {
            *entry = serde_yaml::Value::Mapping(Default::default());
        }
        let a_map = entry.as_mapping_mut().expect("agent mapping");
        a_map.insert(
            serde_yaml::Value::String("max_consecutive_failures".to_string()),
            serde_yaml::Value::Number((max_fail.max(1) as u64).into()),
        );
    }

    let yaml = if map.is_empty() {
        "{}\n".to_string()
    } else {
        serde_yaml::to_string(&root).map_err(|e| format!("failed to serialize config.yaml: {e}"))?
    };
    std::fs::write(&path, yaml).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

fn copy_credentials(source: &Path, dest: &Path) -> Result<(), String> {
    if !source.exists() {
        return Ok(());
    }
    std::fs::copy(source, dest).map_err(|e| {
        format!(
            "failed to copy credentials from {} to {}: {e}",
            source.display(),
            dest.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub struct ProfileListTool {
    ctx: ProfileToolContext,
}

#[async_trait]
impl Tool for ProfileListTool {
    fn name(&self) -> &str {
        "profile_list"
    }

    fn description(&self) -> &str {
        "List all Luna profiles ('default' plus custom profiles on disk) with summaries and optional model/provider overrides."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn execute(
        &self,
        call_id: &str,
        _arguments: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let items: Vec<serde_json::Value> = profiles::catalog(&self.ctx.profiles_dir)
            .into_iter()
            .map(|summary| {
                let yaml = summary
                    .dir
                    .as_deref()
                    .map(read_profile_yaml)
                    .unwrap_or_default();
                json!({
                    "name": summary.name,
                    "summary": summary.summary,
                    "builtin": summary.builtin,
                    "dir": summary.dir.map(|d| d.display().to_string()),
                    "provider": yaml.provider,
                    "model": yaml.model,
                    "max_consecutive_failures": yaml.max_consecutive_failures,
                })
            })
            .collect();
        Ok(ToolResult::success(
            call_id,
            serde_json::to_string_pretty(&items).unwrap_or_default(),
        ))
    }
}

pub struct ProfileViewTool {
    ctx: ProfileToolContext,
}

#[async_trait]
impl Tool for ProfileViewTool {
    fn name(&self) -> &str {
        "profile_view"
    }

    fn description(&self) -> &str {
        "Inspect a Luna profile's full Summary, Persona, Behavior/mindset instructions, and config overrides."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Profile name (defaults to 'default')."
                }
            }
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let name = arguments["name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_PROFILE);
        match load_profile(name, &self.ctx.profiles_dir) {
            Ok(p) => {
                let yaml = p.dir.as_deref().map(read_profile_yaml).unwrap_or_default();
                let payload = json!({
                    "name": p.name,
                    "summary": p.summary,
                    "persona": p.persona,
                    "behavior": p.behavior,
                    "builtin": p.builtin,
                    "dir": p.dir.map(|d| d.display().to_string()),
                    "provider": yaml.provider,
                    "model": yaml.model,
                    "max_consecutive_failures": yaml.max_consecutive_failures,
                    "config_yaml": yaml.raw_yaml,
                });
                Ok(ToolResult::success(
                    call_id,
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                ))
            }
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

pub struct ProfileCreateTool {
    ctx: ProfileToolContext,
}

#[async_trait]
impl Tool for ProfileCreateTool {
    fn name(&self) -> &str {
        "profile_create"
    }

    fn description(&self) -> &str {
        "Create a new custom Luna profile with specialized persona, mindset, and behavior rules (e.g. for an automation job, research worker, night-shift coder, or continuous sleep-loop monitor)."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Lowercase slug name (1-32 chars, [a-z0-9-], e.g. 'night-coder' or 'market-watcher')."
                },
                "summary": {
                    "type": "string",
                    "description": "Concise 1-line description of this profile's role/job."
                },
                "persona": {
                    "type": "string",
                    "description": "The '# Persona' section: Role, Tone, Mindset, Style, Boundaries. Inherits default if omitted."
                },
                "behavior": {
                    "type": "string",
                    "description": "The '# Behavior' section: execution rules, working-hour/sleep-loop habits, verification rules, output style, stopping criteria. Inherits default if omitted."
                },
                "provider": {
                    "type": "string",
                    "description": "Optional default LLM provider override for this profile (written to config.yaml)."
                },
                "model": {
                    "type": "string",
                    "description": "Optional default LLM model override for this profile (written to config.yaml)."
                },
                "max_consecutive_failures": {
                    "type": "integer",
                    "description": "Optional max consecutive tool failures before halting a turn (written to config.yaml)."
                },
                "copy_credentials": {
                    "type": "boolean",
                    "description": "Copy active provider credentials into the new profile so it can authenticate immediately (default true)."
                }
            },
            "required": ["name"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if !self.ctx.allow_mutation {
            return Ok(ToolResult::error(
                call_id,
                "profile_create is disabled for subagents",
            ));
        }
        let Some(raw_name) = arguments["name"].as_str() else {
            return Ok(ToolResult::error(call_id, "Missing 'name'"));
        };
        let name = raw_name.trim().to_ascii_lowercase();
        if let Err(e) = validate_name(&name) {
            return Ok(ToolResult::error(call_id, e.to_string()));
        }

        if let Err(e) = std::fs::create_dir_all(&self.ctx.profiles_dir) {
            return Ok(ToolResult::error(
                call_id,
                format!("failed to create profiles dir: {e}"),
            ));
        }

        let dest = profile_dir(&self.ctx.profiles_dir, &name);
        if dest.exists() {
            return Ok(ToolResult::error(
                call_id,
                format!("Profile '{name}' already exists; use profile_edit to modify it."),
            ));
        }

        let def = default_profile()?;
        let summary = arguments["summary"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Custom automation profile")
            .to_string();
        let persona = arguments["persona"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&def.persona)
            .to_string();
        let behavior = arguments["behavior"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&def.behavior)
            .to_string();

        let staging = self
            .ctx
            .profiles_dir
            .join(format!(".staging-{name}-{}", std::process::id()));
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        let paths = profiles::paths_for(&staging);
        if let Err(e) = paths.ensure_dirs() {
            return Ok(ToolResult::error(call_id, e.to_string()));
        }

        let profile = Profile {
            name: name.clone(),
            summary,
            persona,
            behavior,
            builtin: false,
            dir: Some(staging.clone()),
        };
        let rendered = render_profile(&profile);
        if let Err(e) = ProfileText::parse(&rendered) {
            let _ = std::fs::remove_dir_all(&staging);
            return Ok(ToolResult::error(call_id, e.to_string()));
        }
        if let Err(e) = std::fs::write(staging.join(PROFILE_FILE), rendered) {
            let _ = std::fs::remove_dir_all(&staging);
            return Ok(ToolResult::error(
                call_id,
                format!("failed to write profile.md: {e}"),
            ));
        }

        let provider = arguments["provider"].as_str();
        let model = arguments["model"].as_str();
        let max_fail = arguments["max_consecutive_failures"]
            .as_u64()
            .map(|n| n as usize);
        if let Err(e) = write_profile_yaml(&staging, provider, model, max_fail, false) {
            let _ = std::fs::remove_dir_all(&staging);
            return Ok(ToolResult::error(call_id, e));
        }

        let copy_creds = arguments["copy_credentials"].as_bool().unwrap_or(true);
        if copy_creds {
            if let Err(e) = copy_credentials(&self.ctx.credentials_path, &paths.credentials) {
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(ToolResult::error(call_id, e));
            }
        }

        if let Err(e) = std::fs::rename(&staging, &dest) {
            let _ = std::fs::remove_dir_all(&staging);
            return Ok(ToolResult::error(
                call_id,
                format!("failed to finalize profile '{name}': {e}"),
            ));
        }

        let yaml = read_profile_yaml(&dest);
        let out = json!({
            "name": profile.name,
            "summary": profile.summary,
            "dir": dest.display().to_string(),
            "provider": yaml.provider,
            "model": yaml.model,
            "max_consecutive_failures": yaml.max_consecutive_failures,
        });
        Ok(ToolResult::success(
            call_id,
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        ))
    }
}

pub struct ProfileEditTool {
    ctx: ProfileToolContext,
}

#[async_trait]
impl Tool for ProfileEditTool {
    fn name(&self) -> &str {
        "profile_edit"
    }

    fn description(&self) -> &str {
        "Modify an existing custom Luna profile's summary, persona, mindset/behavior rules (replace or append), or model/provider overrides."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Existing custom profile name (cannot edit built-in 'default')."
                },
                "summary": { "type": "string", "description": "Replace '# Summary' text." },
                "persona": { "type": "string", "description": "Replace '# Persona' text." },
                "append_persona": { "type": "string", "description": "Append directives to '# Persona' without replacing existing text." },
                "behavior": { "type": "string", "description": "Replace '# Behavior' text." },
                "append_behavior": { "type": "string", "description": "Append directives (e.g. mindset shift, sleep loop rule, verification rule) to '# Behavior'." },
                "provider": { "type": "string", "description": "Set/update 'model.provider' in config.yaml (empty string clears)." },
                "model": { "type": "string", "description": "Set/update 'model.default' in config.yaml (empty string clears)." },
                "clear_model_override": { "type": "boolean", "description": "Remove model/provider override from config.yaml." },
                "max_consecutive_failures": { "type": "integer", "description": "Update agent.max_consecutive_failures in config.yaml." }
            },
            "required": ["name"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if !self.ctx.allow_mutation {
            return Ok(ToolResult::error(
                call_id,
                "profile_edit is disabled for subagents",
            ));
        }
        let Some(raw_name) = arguments["name"].as_str() else {
            return Ok(ToolResult::error(call_id, "Missing 'name'"));
        };
        let name = raw_name.trim().to_ascii_lowercase();
        if name == DEFAULT_PROFILE {
            return Ok(ToolResult::error(
                call_id,
                "Cannot edit the built-in 'default' profile. Create a custom profile with profile_create instead.",
            ));
        }
        let mut profile = match load_profile(&name, &self.ctx.profiles_dir) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e.to_string())),
        };
        let Some(dir) = profile.dir.clone() else {
            return Ok(ToolResult::error(
                call_id,
                "Profile has no directory on disk",
            ));
        };

        if let Some(s) = arguments["summary"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            profile.summary = s.to_string();
        }
        if let Some(p) = arguments["persona"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            profile.persona = p.to_string();
        }
        if let Some(ap) = arguments["append_persona"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            profile.persona = format!("{}\n{}", profile.persona.trim_end(), ap);
        }
        if let Some(b) = arguments["behavior"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            profile.behavior = b.to_string();
        }
        if let Some(ab) = arguments["append_behavior"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            profile.behavior = format!("{}\n\n{}", profile.behavior.trim_end(), ab);
        }

        let rendered = render_profile(&profile);
        if let Err(e) = ProfileText::parse(&rendered) {
            return Ok(ToolResult::error(call_id, e.to_string()));
        }
        if let Err(e) = std::fs::write(dir.join(PROFILE_FILE), rendered) {
            return Ok(ToolResult::error(
                call_id,
                format!("failed to write profile.md: {e}"),
            ));
        }

        let provider = arguments["provider"].as_str();
        let model = arguments["model"].as_str();
        let clear_model = arguments["clear_model_override"].as_bool().unwrap_or(false);
        let max_fail = arguments["max_consecutive_failures"]
            .as_u64()
            .map(|n| n as usize);
        if provider.is_some() || model.is_some() || clear_model || max_fail.is_some() {
            if let Err(e) = write_profile_yaml(&dir, provider, model, max_fail, clear_model) {
                return Ok(ToolResult::error(call_id, e));
            }
        }

        let yaml = read_profile_yaml(&dir);
        let out = json!({
            "name": profile.name,
            "summary": profile.summary,
            "persona": profile.persona,
            "behavior": profile.behavior,
            "provider": yaml.provider,
            "model": yaml.model,
            "max_consecutive_failures": yaml.max_consecutive_failures,
        });
        Ok(ToolResult::success(
            call_id,
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        ))
    }
}

pub struct ProfileDeleteTool {
    ctx: ProfileToolContext,
}

#[async_trait]
impl Tool for ProfileDeleteTool {
    fn name(&self) -> &str {
        "profile_delete"
    }

    fn description(&self) -> &str {
        "Delete a custom Luna profile and its directory (cannot delete 'default')."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Custom profile name to delete."
                }
            },
            "required": ["name"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if !self.ctx.allow_mutation {
            return Ok(ToolResult::error(
                call_id,
                "profile_delete is disabled for subagents",
            ));
        }
        let Some(raw_name) = arguments["name"].as_str() else {
            return Ok(ToolResult::error(call_id, "Missing 'name'"));
        };
        let name = raw_name.trim().to_ascii_lowercase();
        if name == DEFAULT_PROFILE {
            return Ok(ToolResult::error(
                call_id,
                "Cannot delete the built-in 'default' profile.",
            ));
        }
        let dir = profile_dir(&self.ctx.profiles_dir, &name);
        if !dir.exists() {
            return Ok(ToolResult::error(
                call_id,
                format!("Profile '{name}' does not exist."),
            ));
        }
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            return Ok(ToolResult::error(
                call_id,
                format!("failed to delete profile '{name}': {e}"),
            ));
        }
        Ok(ToolResult::success(
            call_id,
            format!("Deleted profile '{name}'."),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::DefaultToolRegistry;

    #[tokio::test]
    async fn profile_tools_crud_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let creds_path = dir.path().join("credentials.json");
        std::fs::write(&creds_path, "{}").unwrap();

        let mut cfg = Config::default();
        cfg.paths.profiles_dir = dir.path().join("profiles");
        cfg.paths.credentials = creds_path;

        let reg: Arc<dyn ToolRegistry> = Arc::new(DefaultToolRegistry::new());
        register_profile_tools(&reg, &cfg, true).await.unwrap();

        let cancel = CancellationToken::new();

        // Create
        let created = reg
            .execute(
                &crate::types::ToolCall {
                    id: "1".into(),
                    name: "profile_create".into(),
                    arguments: json!({
                        "name": "night-coder",
                        "summary": "Autonomous night shift engineer",
                        "persona": "Role: Tireless senior engineer.\nMindset: Ship clean tested fixes.",
                        "behavior": "Work continuously using sleep(seconds) between checks.",
                        "provider": "openai",
                        "model": "gpt-5"
                    }),
                    signature: None,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(!created.is_error, "{}", created.content);
        assert!(cfg
            .paths
            .profiles_dir
            .join("night-coder/credentials.json")
            .is_file());

        // Edit (append_behavior + model)
        let edited = reg
            .execute(
                &crate::types::ToolCall {
                    id: "2".into(),
                    name: "profile_edit".into(),
                    arguments: json!({
                        "name": "night-coder",
                        "append_behavior": "- Always write a handoff note before resting.",
                        "model": "gpt-5-mini"
                    }),
                    signature: None,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(!edited.is_error, "{}", edited.content);
        assert!(edited.content.contains("handoff note"));
        assert!(edited.content.contains("gpt-5-mini"));

        // View
        let viewed = reg
            .execute(
                &crate::types::ToolCall {
                    id: "3".into(),
                    name: "profile_view".into(),
                    arguments: json!({ "name": "night-coder" }),
                    signature: None,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(!viewed.is_error, "{}", viewed.content);
        assert!(viewed.content.contains("Tireless senior engineer"));

        // List
        let listed = reg
            .execute(
                &crate::types::ToolCall {
                    id: "4".into(),
                    name: "profile_list".into(),
                    arguments: json!({}),
                    signature: None,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(listed.content.contains("night-coder"));
        assert!(listed.content.contains("default"));

        // Delete
        let deleted = reg
            .execute(
                &crate::types::ToolCall {
                    id: "5".into(),
                    name: "profile_delete".into(),
                    arguments: json!({ "name": "night-coder" }),
                    signature: None,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert!(!deleted.is_error, "{}", deleted.content);
        assert!(!cfg.paths.profiles_dir.join("night-coder").exists());
    }
}
