//! DeepSeek-style plugin manager: discovery, TS compilation, hot-load/unload,
//! automatic sidebar agent synthesis, and `Tool` bridge into `harness-core`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, Tool, ToolResult};

use crate::builder::{generate_plugin_files, PluginBuilderSpec};
use crate::compiler::compile_typescript;
use crate::manifest::{
    PluginAgentEntry, PluginCommandSpec, PluginManifest, PluginScheduleDecl, PluginSummary,
    PluginToolDescriptor, PluginUiPanel,
};
use crate::runtime::{PluginRuntimeHandle, RegisteredPluginTool};
use crate::storage::{load_settings, save_settings};

struct LoadedPlugin {
    manifest: PluginManifest,
    dir: PathBuf,
    runtime: Option<Arc<PluginRuntimeHandle>>,
    last_error: Option<String>,
}

/// Manages all user-created and built-in DeepSeek-style TypeScript plugins under `plugins_dir`.
pub struct PluginManager {
    plugins_dir: PathBuf,
    workspace_plugins_dir: Option<PathBuf>,
    workspace_root: Option<PathBuf>,
    plugins: RwLock<BTreeMap<String, LoadedPlugin>>,
    generation: AtomicU64,
}

impl PluginManager {
    pub fn new(plugins_dir: PathBuf, workspace_plugins_dir: Option<PathBuf>) -> Self {
        let workspace_root = workspace_plugins_dir
            .as_ref()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(Path::to_path_buf);
        Self {
            plugins_dir,
            workspace_plugins_dir,
            workspace_root,
            plugins: RwLock::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
        }
    }

    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// Scan `plugins_dir` (and optional workspace `.luna/plugins`), compile stale TS, and load enabled plugins.
    pub fn discover_and_load(&self) -> Result<(), String> {
        let _ = std::fs::create_dir_all(&self.plugins_dir);

        let mut dirs_to_scan = vec![self.plugins_dir.clone()];
        if let Some(ws) = &self.workspace_plugins_dir {
            if ws.exists() {
                dirs_to_scan.push(ws.clone());
            }
        }

        let mut discovered: BTreeMap<String, (PluginManifest, PathBuf)> = BTreeMap::new();
        for root in dirs_to_scan {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let manifest_path = path.join("plugin.json");
                if !manifest_path.is_file() {
                    continue;
                }
                match std::fs::read_to_string(&manifest_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<PluginManifest>(&s).ok())
                {
                    Some(manifest) => {
                        discovered.insert(manifest.id.clone(), (manifest, path));
                    }
                    None => {
                        tracing::warn!(
                            "ignoring malformed plugin manifest at {}",
                            manifest_path.display()
                        );
                    }
                }
            }
        }

        // Shutdown existing runtimes before replacing
        {
            let mut guard = self.plugins.write().unwrap_or_else(|e| e.into_inner());
            for old in guard.values_mut() {
                if let Some(rt) = old.runtime.take() {
                    rt.shutdown();
                }
            }
            guard.clear();
        }

        for (id, (manifest, dir)) in discovered {
            let loaded = Self::load_one(manifest, dir, self.workspace_root.clone());
            self.plugins
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id, loaded);
        }
        self.bump_generation();

        Ok(())
    }

    fn load_one(
        manifest: PluginManifest,
        dir: PathBuf,
        workspace_root: Option<PathBuf>,
    ) -> LoadedPlugin {
        if !manifest.enabled {
            return LoadedPlugin {
                manifest,
                dir,
                runtime: None,
                last_error: None,
            };
        }

        let js_result = Self::ensure_compiled(&manifest, &dir);
        match js_result {
            Ok(js_code) => match PluginRuntimeHandle::spawn_with_workspace(
                manifest.clone(),
                dir.clone(),
                workspace_root,
                js_code,
            ) {
                Ok(rt) => LoadedPlugin {
                    manifest,
                    dir,
                    runtime: Some(Arc::new(rt)),
                    last_error: None,
                },
                Err(err) => LoadedPlugin {
                    manifest,
                    dir,
                    runtime: None,
                    last_error: Some(err),
                },
            },
            Err(err) => LoadedPlugin {
                manifest,
                dir,
                runtime: None,
                last_error: Some(err),
            },
        }
    }

    fn ensure_compiled(manifest: &PluginManifest, dir: &Path) -> Result<String, String> {
        let entry_path = dir.join(&manifest.entry);
        let compiled_path = dir.join(&manifest.compiled);

        if entry_path.is_file() {
            let ts_source = std::fs::read_to_string(&entry_path)
                .map_err(|e| format!("failed to read {}: {e}", entry_path.display()))?;
            let js_code =
                compile_typescript(&manifest.entry, &ts_source).map_err(|e| e.to_string())?;
            if let Some(parent) = compiled_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&compiled_path, &js_code);
            return Ok(js_code);
        }

        if compiled_path.is_file() {
            return std::fs::read_to_string(&compiled_path)
                .map_err(|e| format!("failed to read {}: {e}", compiled_path.display()));
        }

        Err(format!(
            "plugin '{}' missing entrypoint '{}' and compiled bundle '{}'",
            manifest.id, manifest.entry, manifest.compiled
        ))
    }

    /// Install or update a plugin from a Guided Builder spec, compile it, and load it immediately.
    pub fn install_from_builder(&self, spec: &PluginBuilderSpec) -> Result<PluginSummary, String> {
        let generated = generate_plugin_files(spec)?;
        self.install_raw(generated.manifest, &generated.ts_source)
    }

    /// Install or update a plugin from raw manifest + TypeScript source, compile it, and load it.
    pub fn install_raw(
        &self,
        manifest: PluginManifest,
        ts_source: &str,
    ) -> Result<PluginSummary, String> {
        self.install_raw_in_scope(manifest, ts_source, false)
    }

    /// Install or update a plugin into either the global plugins directory or the workspace `.luna/plugins` directory.
    pub fn install_raw_in_scope(
        &self,
        manifest: PluginManifest,
        ts_source: &str,
        workspace_scope: bool,
    ) -> Result<PluginSummary, String> {
        self.install_package_in_scope(manifest, ts_source, None, workspace_scope)
    }

    /// Install or update a full plugin package (manifest + TypeScript source + optional `ui/` files)
    /// into either the global plugins directory or the workspace `.luna/plugins` directory.
    pub fn install_package_in_scope(
        &self,
        mut manifest: PluginManifest,
        ts_source: &str,
        ui_files: Option<&BTreeMap<String, String>>,
        workspace_scope: bool,
    ) -> Result<PluginSummary, String> {
        let js_code = compile_typescript(&manifest.entry, ts_source).map_err(|e| e.to_string())?;

        let base_dir = if workspace_scope {
            self.workspace_plugins_dir
                .as_ref()
                .ok_or_else(|| "no workspace plugins directory configured".to_string())?
        } else {
            &self.plugins_dir
        };
        let dir = base_dir.join(&manifest.id);
        let entry_path = dir.join(&manifest.entry);
        let compiled_path = dir.join(&manifest.compiled);

        if let Some(p) = entry_path.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        if let Some(p) = compiled_path.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }

        if let Some(files) = ui_files {
            let ui_root = dir.join("ui");
            std::fs::create_dir_all(&ui_root).map_err(|e| e.to_string())?;
            for (rel_path, content) in files {
                let safe_rel = sanitize_relative_ui_path(rel_path)?;
                let target = ui_root.join(&safe_rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&target, content).map_err(|e| e.to_string())?;
            }
        }

        if manifest.ui_entry.is_none() && dir.join("ui/index.html").is_file() {
            manifest.ui_entry = Some("ui/index.html".to_string());
        }

        std::fs::write(&entry_path, ts_source).map_err(|e| e.to_string())?;
        std::fs::write(&compiled_path, &js_code).map_err(|e| e.to_string())?;
        let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("plugin.json"), manifest_json).map_err(|e| e.to_string())?;

        // Unload previous version if loaded
        {
            let mut guard = self.plugins.write().unwrap_or_else(|e| e.into_inner());
            if let Some(mut old) = guard.remove(&manifest.id) {
                if let Some(rt) = old.runtime.take() {
                    rt.shutdown();
                }
            }
            let loaded = Self::load_one(manifest.clone(), dir, self.workspace_root.clone());
            guard.insert(manifest.id.clone(), loaded);
        }
        self.bump_generation();

        self.get_summary(&manifest.id)
            .ok_or_else(|| format!("plugin '{}' failed to register", manifest.id))
    }

    /// Install a built-in plugin only if it does not already exist on disk.
    pub fn ensure_builtin_plugin(
        &self,
        manifest: PluginManifest,
        ts_source: &str,
    ) -> Result<(), String> {
        let dir = self.plugins_dir.join(&manifest.id);
        if dir.join("plugin.json").is_file() {
            return Ok(());
        }
        let _ = self.install_raw(manifest, ts_source)?;
        Ok(())
    }

    pub fn set_enabled(&self, plugin_id: &str, enabled: bool) -> Result<PluginSummary, String> {
        let (manifest, dir) = {
            let mut guard = self.plugins.write().unwrap_or_else(|e| e.into_inner());
            let entry = guard
                .get_mut(plugin_id)
                .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
            if let Some(rt) = entry.runtime.take() {
                rt.shutdown();
            }
            entry.manifest.enabled = enabled;
            let manifest_json =
                serde_json::to_string_pretty(&entry.manifest).map_err(|e| e.to_string())?;
            std::fs::write(entry.dir.join("plugin.json"), manifest_json)
                .map_err(|e| e.to_string())?;
            (entry.manifest.clone(), entry.dir.clone())
        };

        let reloaded = Self::load_one(manifest, dir, self.workspace_root.clone());
        self.plugins
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plugin_id.to_string(), reloaded);
        self.bump_generation();

        self.get_summary(plugin_id)
            .ok_or_else(|| format!("plugin '{plugin_id}' missing after reload"))
    }

    pub fn reload(&self, plugin_id: &str) -> Result<PluginSummary, String> {
        let (manifest, dir) = {
            let mut guard = self.plugins.write().unwrap_or_else(|e| e.into_inner());
            let entry = guard
                .get_mut(plugin_id)
                .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
            if let Some(rt) = entry.runtime.take() {
                rt.shutdown();
            }
            let fresh_manifest: PluginManifest =
                std::fs::read_to_string(entry.dir.join("plugin.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| entry.manifest.clone());
            (fresh_manifest, entry.dir.clone())
        };

        let reloaded = Self::load_one(manifest, dir, self.workspace_root.clone());
        self.plugins
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plugin_id.to_string(), reloaded);
        self.bump_generation();

        self.get_summary(plugin_id)
            .ok_or_else(|| format!("plugin '{plugin_id}' missing after reload"))
    }

    pub fn remove(&self, plugin_id: &str) -> Result<(), String> {
        let dir = {
            let mut guard = self.plugins.write().unwrap_or_else(|e| e.into_inner());
            let mut old = guard
                .remove(plugin_id)
                .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
            if let Some(rt) = old.runtime.take() {
                rt.shutdown();
            }
            old.dir
        };
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        self.bump_generation();
        Ok(())
    }

    pub fn update_settings(
        &self,
        plugin_id: &str,
        settings: BTreeMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        let dir = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard
                .get(plugin_id)
                .map(|p| p.dir.clone())
                .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?
        };
        save_settings(&dir, &settings)
    }

    /// Merge settings keys into `settings.json`, notify the running plugin via
    /// `settings_changed`, and return the updated `PluginSummary`.
    pub async fn set_settings_patch(
        &self,
        plugin_id: &str,
        patch: BTreeMap<String, serde_json::Value>,
    ) -> Result<PluginSummary, String> {
        let (dir, rt) = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            let p = guard
                .get(plugin_id)
                .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
            (p.dir.clone(), p.runtime.clone())
        };
        let mut merged = load_settings(&dir);
        for (k, v) in patch {
            merged.insert(k, v);
        }
        save_settings(&dir, &merged)?;
        if let Some(runtime) = rt {
            let payload = serde_json::to_value(&merged).unwrap_or_else(|_| serde_json::json!({}));
            let _ = runtime.emit_event("settings_changed", &payload).await;
        }
        self.get_summary(plugin_id)
            .ok_or_else(|| format!("plugin '{plugin_id}' missing after settings update"))
    }

    pub fn read_source(&self, plugin_id: &str) -> Result<String, String> {
        let (dir, entry) = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            let p = guard
                .get(plugin_id)
                .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
            (p.dir.clone(), p.manifest.entry.clone())
        };
        std::fs::read_to_string(dir.join(entry)).map_err(|e| e.to_string())
    }

    pub fn read_package_snapshot(&self, plugin_id: &str) -> Result<serde_json::Value, String> {
        let (manifest, dir) = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            let p = guard
                .get(plugin_id)
                .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
            (p.manifest.clone(), p.dir.clone())
        };
        let ts_source = std::fs::read_to_string(dir.join(&manifest.entry)).unwrap_or_default();
        let mut ui_files: BTreeMap<String, String> = BTreeMap::new();
        let ui_dir = dir.join("ui");
        if ui_dir.is_dir() {
            for entry in walkdir::WalkDir::new(&ui_dir).into_iter().flatten() {
                if entry.file_type().is_file() {
                    if let Ok(rel) = entry.path().strip_prefix(&ui_dir) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        if let Ok(text) = std::fs::read_to_string(entry.path()) {
                            ui_files.insert(rel_str, text);
                        }
                    }
                }
            }
        }
        Ok(serde_json::json!({
            "manifest": manifest,
            "ts_source": ts_source,
            "ui_files": ui_files,
        }))
    }

    pub fn list_summaries(&self) -> Vec<PluginSummary> {
        let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        guard.values().map(|p| self.summarize(p)).collect()
    }

    pub fn get_summary(&self, plugin_id: &str) -> Option<PluginSummary> {
        let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        guard.get(plugin_id).map(|p| self.summarize(p))
    }

    /// Return one automatic sidebar agent per enabled & loaded plugin.
    pub fn sidebar_agents(&self) -> Vec<PluginAgentEntry> {
        self.list_summaries()
            .into_iter()
            .filter(|p| p.enabled && p.loaded)
            .filter_map(|p| p.agent)
            .collect()
    }

    pub fn ui_panels(&self) -> Vec<PluginUiPanel> {
        self.list_summaries()
            .into_iter()
            .filter(|p| p.enabled && p.loaded)
            .flat_map(|p| p.panels)
            .collect()
    }

    pub fn schedule_declarations(&self) -> Vec<PluginScheduleDecl> {
        self.list_summaries()
            .into_iter()
            .filter(|p| p.enabled && p.loaded)
            .flat_map(|p| p.schedules)
            .collect()
    }

    /// Build `Tool` adapters for every tool registered by enabled plugins.
    pub fn registered_tools(&self) -> Vec<Arc<dyn Tool>> {
        let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        for p in guard.values() {
            if !p.manifest.enabled {
                continue;
            }
            let Some(rt) = &p.runtime else {
                continue;
            };
            for tool_meta in &rt.registrations.tools {
                out.push(Arc::new(PluginToolBridge {
                    meta: tool_meta.clone(),
                    runtime: rt.clone(),
                }));
            }
        }
        out
    }

    pub async fn invoke_command(
        &self,
        plugin_id: &str,
        command: &str,
        args: serde_json::Value,
    ) -> Result<String, String> {
        let rt = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard
                .get(plugin_id)
                .and_then(|p| p.runtime.clone())
                .ok_or_else(|| format!("plugin '{plugin_id}' is not loaded"))?
        };
        rt.call_command(command, &args).await
    }

    pub async fn invoke_tool(
        &self,
        plugin_id: &str,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<String, String> {
        let rt = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard
                .get(plugin_id)
                .and_then(|p| p.runtime.clone())
                .ok_or_else(|| format!("plugin '{plugin_id}' is not loaded"))?
        };
        rt.call_tool(tool_name, &args).await
    }

    /// Read a custom UI file for `plugin_id` (`ui/` directory or `ctx.ui.setCustomHtml`),
    /// returning `(bytes, content_type)`.
    pub fn read_ui_asset(
        &self,
        plugin_id: &str,
        rel_path: &str,
    ) -> Result<(Vec<u8>, String), String> {
        let (dir, ui_entry, custom_html) = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            let p = guard
                .get(plugin_id)
                .ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
            let html = p.runtime.as_ref().and_then(|r| r.live_custom_html());
            (p.dir.clone(), p.manifest.ui_entry.clone(), html)
        };

        let trimmed = rel_path.trim().trim_start_matches('/');
        let requested = if trimmed.is_empty() {
            "index.html"
        } else {
            trimmed
        };
        let safe_rel = sanitize_relative_ui_path(requested)?;

        let ui_dir = dir.join("ui");
        let candidate = ui_dir.join(&safe_rel);
        if candidate.is_file() {
            let bytes = std::fs::read(&candidate).map_err(|e| e.to_string())?;
            let mime = guess_ui_mime(&safe_rel);
            return Ok((bytes, mime));
        }

        if requested == "index.html" {
            if let Some(entry) = ui_entry {
                let safe_entry = sanitize_relative_ui_path(&entry)?;
                let entry_path = dir.join(&safe_entry);
                if entry_path.is_file() {
                    let bytes = std::fs::read(&entry_path).map_err(|e| e.to_string())?;
                    let mime = guess_ui_mime(&safe_entry);
                    return Ok((bytes, mime));
                }
            }
            if let Some(html) = custom_html {
                return Ok((html.into_bytes(), "text/html; charset=utf-8".to_string()));
            }
        }

        Err(format!(
            "UI asset '{}' not found for plugin '{}'",
            requested, plugin_id
        ))
    }

    pub async fn dispatch_event(&self, event: &str, payload: serde_json::Value) {
        let runtimes: Vec<Arc<PluginRuntimeHandle>> = {
            let guard = self.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard.values().filter_map(|p| p.runtime.clone()).collect()
        };
        for rt in runtimes {
            let _ = rt.emit_event(event, &payload).await;
        }
    }

    fn summarize(&self, p: &LoadedPlugin) -> PluginSummary {
        let scope = if p.manifest.builtin {
            "builtin".to_string()
        } else if self
            .workspace_plugins_dir
            .as_ref()
            .is_some_and(|ws| p.dir.starts_with(ws))
        {
            "workspace".to_string()
        } else {
            "global".to_string()
        };
        let settings = load_settings(&p.dir);
        let (tools, tool_schemas, commands, panels, schedules, explicit_agent, has_runtime_html) =
            match &p.runtime {
                Some(rt) => (
                    rt.registrations
                        .tools
                        .iter()
                        .map(|t| t.name.clone())
                        .collect::<Vec<_>>(),
                    rt.registrations
                        .tools
                        .iter()
                        .map(|t| PluginToolDescriptor {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: t.parameters.clone(),
                            mutating: t.is_mutating,
                        })
                        .collect::<Vec<_>>(),
                    rt.registrations.commands.clone(),
                    rt.live_panels(),
                    rt.registrations.schedules.clone(),
                    rt.registrations.agents.first().cloned(),
                    rt.live_custom_html().is_some(),
                ),
                None => (
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    false,
                ),
            };

        let has_disk_ui = p.dir.join("ui/index.html").is_file()
            || p.manifest
                .ui_entry
                .as_ref()
                .is_some_and(|u| p.dir.join(u).is_file());
        let has_custom_ui = has_runtime_html || has_disk_ui;

        // Guarantee every enabled & loaded plugin gets one automatic sidebar agent.
        let agent = if p.manifest.enabled && p.runtime.is_some() {
            Some(explicit_agent.unwrap_or_else(|| {
                let agent_name = if p.manifest.auto_agent.name.trim().is_empty() {
                    p.manifest.name.clone()
                } else {
                    p.manifest.auto_agent.name.clone()
                };
                let system_prompt = if p.manifest.auto_agent.system_prompt.trim().is_empty() {
                    format!(
                        "You are the {} plugin agent ({}). {}",
                        agent_name, p.manifest.id, p.manifest.description
                    )
                } else {
                    p.manifest.auto_agent.system_prompt.clone()
                };
                PluginAgentEntry {
                    plugin_id: p.manifest.id.clone(),
                    id: p.manifest.id.clone(),
                    name: agent_name,
                    description: p.manifest.description.clone(),
                    icon: p.manifest.auto_agent.icon.clone(),
                    system_prompt,
                    model: p.manifest.auto_agent.model.clone(),
                    registered_tools: tools.clone(),
                    panel_ids: panels.iter().map(|pn| pn.id.clone()).collect(),
                }
            }))
        } else {
            None
        };

        PluginSummary {
            id: p.manifest.id.clone(),
            name: p.manifest.name.clone(),
            version: p.manifest.version.clone(),
            description: p.manifest.description.clone(),
            enabled: p.manifest.enabled,
            loaded: p.runtime.is_some(),
            builtin: p.manifest.builtin,
            scope,
            path: p.dir.clone(),
            last_error: p.last_error.clone(),
            has_custom_ui,
            ui_entry: p.manifest.ui_entry.clone(),
            tools,
            tool_schemas,
            commands,
            agent,
            panels,
            schedules,
            settings,
            config_schema: p.manifest.config_schema.clone(),
            builder_spec: p.manifest.builder_spec.clone(),
        }
    }
}

fn sanitize_relative_ui_path(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim().trim_start_matches('/');
    if trimmed.is_empty() || trimmed.contains('\0') || trimmed.contains('\\') {
        return Err(format!("invalid plugin UI path: '{raw}'"));
    }
    let path = Path::new(trimmed);
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => clean.push(seg),
            _ => return Err(format!("unsafe plugin UI path: '{raw}'")),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(format!("empty plugin UI path: '{raw}'"));
    }
    Ok(clean)
}

fn guess_ui_mime(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Bridges a JS/TS plugin tool handler into `harness_core::Tool`.
pub struct PluginToolBridge {
    meta: RegisteredPluginTool,
    runtime: Arc<PluginRuntimeHandle>,
}

#[async_trait]
impl Tool for PluginToolBridge {
    fn name(&self) -> &str {
        &self.meta.name
    }

    fn description(&self) -> &str {
        &self.meta.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.meta.parameters.clone()
    }

    fn is_mutating(&self) -> bool {
        self.meta.is_mutating
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                Ok(ToolResult::error(call_id, "plugin tool execution cancelled"))
            }
            res = self.runtime.call_tool(&self.meta.name, arguments) => {
                match res {
                    Ok(out) => Ok(ToolResult::success(call_id, out)),
                    Err(err) => Ok(ToolResult::error(call_id, err)),
                }
            }
        }
    }
}

/// Resolve plugin commands by name for quick UI dispatch.
pub fn command_index(summaries: &[PluginSummary]) -> HashMap<String, (String, PluginCommandSpec)> {
    let mut map = HashMap::new();
    for s in summaries {
        for c in &s.commands {
            map.insert(c.name.clone(), (s.id.clone(), c.clone()));
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::{BuilderParamDef, BuilderToolAction, BuilderToolDef};

    #[tokio::test]
    async fn loads_typescript_plugin_executes_tool_and_synthesizes_sidebar_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::new(tmp.path().to_path_buf(), None);

        let spec = PluginBuilderSpec {
            id: "kv-helper".to_string(),
            name: "KV Helper".to_string(),
            version: "0.1.0".to_string(),
            description: "Stores notes in plugin KV".to_string(),
            icon: "database".to_string(),
            system_prompt: "Help store and read KV items.".to_string(),
            model: None,
            tools: vec![
                BuilderToolDef {
                    name: "kv_put".to_string(),
                    description: "Store a value".to_string(),
                    parameters: vec![
                        BuilderParamDef {
                            name: "k".to_string(),
                            description: "key".to_string(),
                            param_type: "string".to_string(),
                            required: true,
                        },
                        BuilderParamDef {
                            name: "v".to_string(),
                            description: "val".to_string(),
                            param_type: "string".to_string(),
                            required: true,
                        },
                    ],
                    action: BuilderToolAction::StorageKv {
                        operation: "set".to_string(),
                        key_template: "{{k}}".to_string(),
                        value_template: Some("{{v}}".to_string()),
                    },
                },
                BuilderToolDef {
                    name: "kv_get".to_string(),
                    description: "Get a value".to_string(),
                    parameters: vec![BuilderParamDef {
                        name: "k".to_string(),
                        description: "key".to_string(),
                        param_type: "string".to_string(),
                        required: true,
                    }],
                    action: BuilderToolAction::StorageKv {
                        operation: "get".to_string(),
                        key_template: "{{k}}".to_string(),
                        value_template: None,
                    },
                },
            ],
            schedules: Vec::new(),
            widgets: Vec::new(),
            config_fields: Vec::new(),
        };

        let summary = mgr.install_from_builder(&spec).expect("install");
        assert!(summary.loaded, "error: {:?}", summary.last_error);
        assert_eq!(mgr.sidebar_agents().len(), 1);
        assert_eq!(mgr.sidebar_agents()[0].name, "KV Helper");

        let tools = mgr.registered_tools();
        let put = tools.iter().find(|t| t.name() == "kv_put").unwrap();
        let get = tools.iter().find(|t| t.name() == "kv_get").unwrap();

        let cancel = CancellationToken::new();
        let r1 = put
            .execute("c1", &serde_json::json!({"k": "alpha", "v": "42"}), &cancel)
            .await
            .unwrap();
        assert!(!r1.is_error, "{}", r1.content);

        let r2 = get
            .execute("c2", &serde_json::json!({"k": "alpha"}), &cancel)
            .await
            .unwrap();
        assert!(!r2.is_error, "{}", r2.content);
        assert!(r2.content.contains("42"), "got {}", r2.content);
    }

    #[tokio::test]
    async fn plugin_without_explicit_agent_call_still_gets_automatic_sidebar_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::new(tmp.path().to_path_buf(), None);

        let manifest = PluginManifest {
            id: "raw-tool".to_string(),
            name: "Raw Tool".to_string(),
            version: "0.1.0".to_string(),
            description: "Minimal TS plugin".to_string(),
            entry: "src/index.ts".to_string(),
            compiled: "dist/index.js".to_string(),
            ui_entry: None,
            enabled: true,
            builtin: false,
            auto_agent: Default::default(),
            config_schema: Vec::new(),
            builder_spec: None,
        };
        let ts = r#"
export const name = 'raw-tool';
export function apply(ctx: any) {
  ctx.tools.register({ name: 'ping_pong', description: 'Ping', parameters: { type: 'object', properties: {} } }, () => 'pong');
}
"#;
        let summary = mgr.install_raw(manifest, ts).expect("install raw");
        assert!(summary.loaded, "err: {:?}", summary.last_error);
        let agents = mgr.sidebar_agents();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].plugin_id, "raw-tool");
        assert_eq!(agents[0].registered_tools, vec!["ping_pong".to_string()]);
    }

    #[tokio::test]
    async fn supports_async_handlers_workspace_ctx_console_and_multifile_ts_imports() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_root = tmp.path().join("workspace");
        let plugins_dir = tmp.path().join("plugins");
        let helper_dir = plugins_dir.join("async-mod").join("src");
        std::fs::create_dir_all(&ws_root).unwrap();
        std::fs::create_dir_all(&helper_dir).unwrap();
        std::fs::write(
            helper_dir.join("helper.ts"),
            "export const prefix = (s: string): string => `ok:${s}`;\n",
        )
        .unwrap();

        let mgr = PluginManager::new(plugins_dir, None).with_workspace_root(ws_root.clone());
        let manifest = PluginManifest {
            id: "async-mod".to_string(),
            name: "Async Mod".to_string(),
            version: "0.2.0".to_string(),
            description: "Tests async apply/tool/command and relative TS imports".to_string(),
            entry: "src/index.ts".to_string(),
            compiled: "dist/index.js".to_string(),
            ui_entry: None,
            enabled: true,
            builtin: false,
            auto_agent: Default::default(),
            config_schema: Vec::new(),
            builder_spec: None,
        };
        let ts = r#"
import { prefix } from './helper';

export async function apply(ctx: any): Promise<void> {
  console.log('booting', ctx.manifest.id, ctx.workspace.root);
  await Promise.resolve();
  ctx.storage.set('booted', true);

  ctx.tools.register(
    { name: 'async_echo', description: 'Async echo', parameters: { type: 'object', properties: {} } },
    async (args: Record<string, unknown>) => {
      await Promise.resolve();
      return JSON.stringify({
        val: prefix(String(args.text || 'hi')),
        ws: ctx.workspace.root,
        entries: ctx.storage.entries(),
      });
    }
  );

  ctx.commands.register('async_cmd', 'Async cmd', async () => {
    await Promise.resolve();
    ctx.storage.clear();
    return JSON.stringify({ keys: ctx.storage.list() });
  });
}
"#;
        let summary = mgr.install_raw(manifest, ts).expect("install async plugin");
        assert!(summary.loaded, "err: {:?}", summary.last_error);

        let tools = mgr.registered_tools();
        let echo = tools.iter().find(|t| t.name() == "async_echo").unwrap();
        let out = echo
            .execute(
                "c1",
                &serde_json::json!({ "text": "luna" }),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("ok:luna"), "{}", out.content);
        assert!(
            out.content.contains(&ws_root.display().to_string()),
            "{}",
            out.content
        );
        assert!(out.content.contains("\"booted\":true"), "{}", out.content);

        let cmd_out = mgr
            .invoke_command("async-mod", "async_cmd", serde_json::json!({}))
            .await
            .unwrap();
        assert!(cmd_out.contains("[]"), "{cmd_out}");
    }
}
