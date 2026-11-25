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
    pub notes_dir: PathBuf,
    pub memory_db: PathBuf,
    /// The active Markdown memory store. `memory_db` is only the legacy migration source.
    pub memory_dir: PathBuf,
    pub allow_mutation: bool,
}

impl ProfileToolContext {
    pub fn from_config(config: &Config, allow_mutation: bool) -> Self {
        Self {
            profiles_dir: config.paths.profiles_dir.clone(),
            credentials_path: config.paths.credentials.clone(),
            notes_dir: config.paths.notes_dir.clone(),
            memory_db: config.paths.memory_db.clone(),
            memory_dir: config.paths.memory_dir.clone(),
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
                },
                "notes": {
                    "description": "Notes to copy into the new profile: 'same' or 'all' to copy all existing notes, 'fresh' or 'none' for an empty notebook (default: 'fresh'), or an array of specific note titles/filenames to copy."
                },
                "memories": {
                    "description": "Memories to copy into the new profile: 'same' or 'all' to copy all active memories, 'fresh' or 'none' for a fresh empty memory store (default: 'fresh'), or an array of specific memory IDs/topics to copy."
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

        let copies: Result<(usize, usize), HarnessError> = (|| {
            let notes = copy_markdown_store(
                &self.ctx.notes_dir,
                &paths.notes_dir,
                arguments.get("notes"),
            )?;
            // The active memory store is the Markdown tree; `memory.db` is the legacy source a
            // profile created before the migration still needs, so both are copied.
            let mut memories = copy_markdown_store(
                &self.ctx.memory_dir,
                &paths.memory_dir,
                arguments.get("memories"),
            )?;
            memories += copy_selected_memories(
                &self.ctx.memory_db,
                &paths.memory_db,
                arguments.get("memories"),
            )?;
            Ok((notes, memories))
        })();
        let (notes_copied, memories_copied) = match copies {
            Ok(counts) => counts,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(ToolResult::error(call_id, e.to_string()));
            }
        };

        if let Err(e) = std::fs::rename(&staging, &dest) {
            let _ = std::fs::remove_dir_all(&staging);
            return Ok(ToolResult::error(
                call_id,
                format!("failed to finalize profile '{name}': {e}"),
            ));
        }

        let launcher = crate::profiles::install_launcher(&name).ok();
        let yaml = read_profile_yaml(&dest);
        let out = json!({
            "name": profile.name,
            "summary": profile.summary,
            "dir": dest.display().to_string(),
            "provider": yaml.provider,
            "model": yaml.model,
            "max_consecutive_failures": yaml.max_consecutive_failures,
            "credentials_copied": copy_creds,
            "notes_copied": notes_copied,
            "memories_copied": memories_copied,
            "launcher": launcher.map(|p| p.display().to_string()),
            "message": format!(
                "Profile '{}' created successfully. You can launch it by typing '{}' directly in your terminal, or with 'luna --profile {}'.",
                profile.name, profile.name, profile.name
            )
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
        let dir = match crate::profiles::resolve_custom_profile_dir(&self.ctx.profiles_dir, &name) {
            Ok(dir) => dir,
            Err(e) => return Ok(ToolResult::error(call_id, e.to_string())),
        };
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
        let _ = crate::profiles::remove_launcher(&name);
        Ok(ToolResult::success(
            call_id,
            format!("Deleted profile '{name}' and removed its launcher."),
        ))
    }
}

/// Copy one Markdown store — the notes tree, or the active memory records — into a new profile.
///
/// Both stores nest files in project/domain subfolders, so a flat listing of the top directory
/// would report success while copying almost nothing. Each file keeps its relative path, and the
/// selection filter is applied while walking the whole tree.
fn copy_markdown_store(
    source_dir: &Path,
    dest_dir: &Path,
    selection: Option<&serde_json::Value>,
) -> Result<usize, HarnessError> {
    if !source_dir.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| HarnessError::Io(format!("failed to create {}: {}", dest_dir.display(), e)))?;

    let Some(arg) = selection else {
        return Ok(0);
    };

    // `None` means "everything"; `Some(items)` is the explicit allow-list.
    let wanted: Option<Vec<String>> = match arg {
        serde_json::Value::Bool(true) => None,
        serde_json::Value::Bool(false) => return Ok(0),
        serde_json::Value::String(s) => {
            let lower = s.trim().to_ascii_lowercase();
            match lower.as_str() {
                "all" | "same" => None,
                "none" | "fresh" => return Ok(0),
                _ => Some(vec![lower]),
            }
        }
        serde_json::Value::Array(items) => {
            let items: Vec<String> = items
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if items.is_empty() {
                return Ok(0);
            }
            Some(items)
        }
        _ => return Ok(0),
    };

    profiles::copy_markdown_files(source_dir, dest_dir, &|relative, stem| {
        let Some(wanted) = wanted.as_ref() else {
            return true;
        };
        let title = read_note_title(&source_dir.join(relative)).to_ascii_lowercase();
        let stem = stem.to_ascii_lowercase();
        let file_name = relative
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        wanted
            .iter()
            .any(|item| stem == *item || title.contains(item.as_str()) || file_name == *item)
    })
}

fn read_note_title(path: &Path) -> String {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    for line in raw.lines().take(40) {
        if let Some(value) = line.strip_prefix("title:") {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("untitled")
        .to_string()
}

fn copy_selected_memories(
    source_db: &Path,
    dest_db: &Path,
    memories_arg: Option<&serde_json::Value>,
) -> Result<usize, HarnessError> {
    if !source_db.exists() {
        return Ok(0);
    }
    let Some(arg) = memories_arg else {
        return Ok(0);
    };

    let copy_all = match arg {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => {
            let lower = s.trim().to_ascii_lowercase();
            if lower == "none" || lower == "fresh" {
                return Ok(0);
            }
            lower == "all" || lower == "same"
        }
        _ => false,
    };

    let specific_items: Vec<String> = match arg {
        serde_json::Value::String(s) if !copy_all => {
            vec![s.trim().to_ascii_lowercase()]
        }
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    };

    if !copy_all && specific_items.is_empty() {
        return Ok(0);
    }

    let source = rusqlite::Connection::open_with_flags(
        source_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| HarnessError::MemoryError(format!("open source memory.db: {e}")))?;

    if let Some(parent) = dest_db.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let dest = rusqlite::Connection::open(dest_db)
        .map_err(|e| HarnessError::MemoryError(format!("open dest memory.db: {e}")))?;

    dest.execute_batch(
        "CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY,
            subject TEXT NOT NULL,
            predicate TEXT NOT NULL,
            object TEXT NOT NULL,
            raw_content TEXT NOT NULL,
            category TEXT NOT NULL CHECK(category IN ('user_profile', 'code_pattern', 'workflow_rule', 'fact', 'note_pointer', 'transcript')),
            confidence REAL NOT NULL DEFAULT 1.0,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            is_active INTEGER NOT NULL DEFAULT 1,
            superseded_by TEXT REFERENCES memories(id),
            name TEXT,
            path TEXT
        );
        CREATE TABLE IF NOT EXISTS memory_embeddings (
            memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
            embedding BLOB NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memories_subject_predicate ON memories(subject, predicate);
        CREATE INDEX IF NOT EXISTS idx_memories_active ON memories(is_active);",
    )
    .map_err(|e| HarnessError::MemoryError(format!("init dest memory schema: {e}")))?;

    let query = "SELECT id, subject, predicate, object, raw_content, category, confidence,
                        created_at, updated_at, is_active, superseded_by, name, path
                 FROM memories WHERE is_active = 1";
    let mut stmt = source
        .prepare(query)
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        })
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

    let mut count = 0;
    for row_res in rows {
        let row = row_res.map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        if !copy_all {
            let id_matches = specific_items.iter().any(|item| {
                row.0.to_ascii_lowercase() == *item
                    || row.1.to_ascii_lowercase() == *item
                    || row.1.to_ascii_lowercase().starts_with(item)
                    || row.2.to_ascii_lowercase() == *item
                    || row
                        .11
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(item)
            });
            if !id_matches {
                continue;
            }
        }

        dest.execute(
            "INSERT OR REPLACE INTO memories
             (id, subject, predicate, object, raw_content, category, confidence,
              created_at, updated_at, is_active, superseded_by, name, path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10,
                row.11, row.12,
            ],
        )
        .map_err(|e| HarnessError::MemoryError(format!("copy memory row: {e}")))?;

        let embedding: Option<Vec<u8>> = source
            .query_row(
                "SELECT embedding FROM memory_embeddings WHERE memory_id = ?1",
                rusqlite::params![&row.0],
                |r| r.get(0),
            )
            .ok();
        if let Some(emb) = embedding {
            let _ = dest.execute(
                "INSERT OR REPLACE INTO memory_embeddings (memory_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![&row.0, emb],
            );
        }
        count += 1;
    }

    Ok(count)
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

    #[tokio::test]
    async fn profile_create_copies_notes_and_memories() {
        let dir = tempfile::tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        std::fs::create_dir_all(&notes_dir).unwrap();
        std::fs::write(
            notes_dir.join("architecture.md"),
            "---\ntitle: Architecture\n---\nSystem architecture",
        )
        .unwrap();

        let mem_db = dir.path().join("memory.db");
        {
            let conn = rusqlite::Connection::open(&mem_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE memories (
                    id TEXT PRIMARY KEY,
                    subject TEXT NOT NULL,
                    predicate TEXT NOT NULL,
                    object TEXT NOT NULL,
                    raw_content TEXT NOT NULL,
                    category TEXT NOT NULL,
                    confidence REAL NOT NULL,
                    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    is_active INTEGER NOT NULL,
                    superseded_by TEXT,
                    name TEXT,
                    path TEXT
                );
                INSERT INTO memories (id, subject, predicate, object, raw_content, category, confidence, is_active)
                VALUES ('mem1', 'astra.engine', 'status', 'ready', 'Engine ready', 'fact', 1.0, 1);"
            ).unwrap();
        }

        let mut cfg = Config::default();
        cfg.paths.profiles_dir = dir.path().join("profiles");
        cfg.paths.credentials = dir.path().join("credentials.json");
        std::fs::write(&cfg.paths.credentials, "{}").unwrap();
        cfg.paths.notes_dir = notes_dir;
        cfg.paths.memory_db = mem_db;

        let reg: Arc<dyn ToolRegistry> = Arc::new(DefaultToolRegistry::new());
        register_profile_tools(&reg, &cfg, true).await.unwrap();

        let cancel = CancellationToken::new();

        let created = reg
            .execute(
                &crate::types::ToolCall {
                    id: "c1".into(),
                    name: "profile_create".into(),
                    arguments: json!({
                        "name": "astra",
                        "summary": "Astra dedicated profile",
                        "notes": "same",
                        "memories": "same"
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
            .join("astra/notes/architecture.md")
            .is_file());
        assert!(cfg.paths.profiles_dir.join("astra/memory.db").is_file());
        assert!(created.content.contains("notes_copied\": 1"));
        assert!(created.content.contains("memories_copied\": 1"));
    }

    /// The current defaults keep notes in project folders and memories as Markdown records, so a
    /// copy that only reads the top of each directory reports a count while copying almost nothing.
    #[tokio::test]
    async fn profile_create_copies_project_notes_and_markdown_memories() {
        let dir = tempfile::tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        std::fs::create_dir_all(notes_dir.join("project-alpha")).unwrap();
        std::fs::write(
            notes_dir.join("top-level.md"),
            "---\ntitle: Top Level\n---\nroot note",
        )
        .unwrap();
        std::fs::write(
            notes_dir.join("project-alpha").join("nested.md"),
            "---\ntitle: Nested Note\n---\nnested note",
        )
        .unwrap();

        let memory_dir = dir.path().join("memory");
        let record_dir = memory_dir.join("project-alpha").join("decisions");
        std::fs::create_dir_all(&record_dir).unwrap();
        std::fs::write(record_dir.join("mem-1.md"), "a memory record").unwrap();

        let ctx = ProfileToolContext {
            profiles_dir: dir.path().join("profiles"),
            credentials_path: dir.path().join("credentials.json"),
            notes_dir,
            memory_db: dir.path().join("memory.db"),
            memory_dir,
            allow_mutation: true,
        };
        let cancel = CancellationToken::new();
        let created = ProfileCreateTool { ctx }
            .execute(
                "call-1",
                &json!({
                    "name": "copied",
                    "summary": "Copy the active stores",
                    "notes": "same",
                    "memories": "same",
                    "copy_credentials": false
                }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(!created.is_error, "{}", created.content);

        let dest = profiles::paths_for(&dir.path().join("profiles").join("copied"));
        assert!(
            dest.notes_dir.join("top-level.md").is_file(),
            "the root note is copied"
        );
        assert!(
            dest.notes_dir
                .join("project-alpha")
                .join("nested.md")
                .is_file(),
            "the project note keeps its folder"
        );
        assert!(
            dest.memory_dir
                .join("project-alpha")
                .join("decisions")
                .join("mem-1.md")
                .is_file(),
            "the Markdown memory record is copied"
        );
        assert!(
            created.content.contains("notes_copied\": 2"),
            "the readout counts both notes: {}",
            created.content
        );
        assert!(
            created.content.contains("memories_copied\": 1"),
            "the readout counts the memory record: {}",
            created.content
        );
    }

    /// A failed copy must fail creation instead of reporting a successful, partial profile.
    #[cfg(unix)]
    #[tokio::test]
    async fn profile_create_reports_copy_errors() {
        let dir = tempfile::tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        std::fs::create_dir_all(&notes_dir).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("missing-target.md"),
            notes_dir.join("broken.md"),
        )
        .unwrap();

        let profiles_dir = dir.path().join("profiles");
        let ctx = ProfileToolContext {
            profiles_dir: profiles_dir.clone(),
            credentials_path: dir.path().join("credentials.json"),
            notes_dir,
            memory_db: dir.path().join("memory.db"),
            memory_dir: dir.path().join("memory"),
            allow_mutation: true,
        };
        let cancel = CancellationToken::new();
        let created = ProfileCreateTool { ctx }
            .execute(
                "call-1",
                &json!({
                    "name": "broken-copy",
                    "summary": "Must not create a partial profile",
                    "notes": "same",
                    "copy_credentials": false
                }),
                &cancel,
            )
            .await
            .unwrap();

        assert!(
            created.is_error,
            "a failed copy was reported as success: {}",
            created.content
        );
        assert!(!profiles_dir.join("broken-copy").exists());
        let staging_left: Vec<_> = std::fs::read_dir(&profiles_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".staging-"))
            .collect();
        assert!(
            staging_left.is_empty(),
            "the staging directory was left behind"
        );
    }
}
