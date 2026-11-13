//! The slash command that inspects or changes the active workspace directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use colored::Colorize;
use tokio::sync::mpsc;

use super::*;
use crate::commands::chat::keys::AppKey;
use crate::commands::chat::ui::ChatUi;
use crate::commands::chat::ChatAgent;
use harness_core::config::Config;
use harness_core::{HarnessError, ToolRegistry};

pub(crate) async fn handle_dir_command(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    config: &mut Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let trimmed_arg = arg.map(|s| s.trim()).filter(|s| !s.is_empty());

    // Check for reset: `/dir reset`, `/dir --reset`, `/dir -r`
    if let Some(a) = trimmed_arg {
        if a.eq_ignore_ascii_case("reset")
            || a.eq_ignore_ascii_case("--reset")
            || a.eq_ignore_ascii_case("-r")
        {
            return reset_workspace(ui, config, agent).await;
        }
    }

    // Parse potential path and flags from arg (e.g. `/dir /path --global` or `/dir --session /path`)
    let (path_str_opt, explicit_scope) = parse_dir_args(trimmed_arg);

    let Some(target_dir) = resolve_target_dir(key_rx, ui, config, path_str_opt).await? else {
        return Ok(());
    };

    let Some(is_global) = choose_dir_scope(key_rx, ui, explicit_scope).await else {
        return Ok(());
    };

    if is_global {
        persist_global_workspace(ui, &target_dir)?;
    }

    apply_workspace_change(config, agent, target_dir.clone()).await?;

    report_workspace_switch(ui, &target_dir, is_global);

    Ok(())
}

/// Apply a workspace path and scope chosen in a TUI modal.
///
/// The interactive CLI path resolves the directory through a picker before reaching
/// `apply_workspace_change`; the TUI asks for the path in its own input modal, so this helper
/// starts from the typed string instead.
pub(crate) async fn apply_dir_choice(
    ui: &ChatUi,
    config: &mut Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    path_str: &str,
    is_global: bool,
) -> Result<(), HarnessError> {
    let expanded = harness_core::config::expand_tilde(path_str.trim());
    let path = PathBuf::from(expanded);
    if !path.is_dir() {
        ui.emit_above(&format!(
            "{} Directory not found: {}",
            "✖".red().bold(),
            path_str
        ));
        return Ok(());
    }
    let target_dir = harness_core::fsutil::canonicalize(&path).unwrap_or(path);

    if is_global {
        persist_global_workspace(ui, &target_dir)?;
    }

    apply_workspace_change(config, agent, target_dir.clone()).await?;
    report_workspace_switch(ui, &target_dir, is_global);
    Ok(())
}

/// Clear the workspace root and fall back to the current directory.
pub(crate) async fn reset_workspace(
    ui: &ChatUi,
    config: &mut Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let cleared = harness_core::config::unset_in_file(&layers, "paths.workspace_root")?;

    let default_dir = harness_core::fsutil::canonicalize(
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    )
    .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    apply_workspace_change(config, agent, default_dir.clone()).await?;

    if cleared.is_some() {
        ui.emit_above(&format!(
            "{} Workspace reset to default: {} (global config cleared)",
            "✔".green().bold(),
            default_dir.display().to_string().bold()
        ));
    } else {
        ui.emit_above(&format!(
            "{} Workspace reset to default: {}",
            "✔".green().bold(),
            default_dir.display().to_string().bold()
        ));
    }
    Ok(())
}

/// Split a `/dir` argument into an optional path and an explicit global/session scope.
pub(crate) fn parse_dir_args(trimmed_arg: Option<&str>) -> (Option<String>, Option<bool>) {
    match trimmed_arg {
        Some(a) => {
            let parts: Vec<&str> = a.split_whitespace().collect();
            let mut scope = None;
            let mut path_tokens = Vec::new();
            for part in parts {
                if part.eq_ignore_ascii_case("--global")
                    || part.eq_ignore_ascii_case("-g")
                    || part.eq_ignore_ascii_case("global")
                {
                    scope = Some(true);
                } else if part.eq_ignore_ascii_case("--session")
                    || part.eq_ignore_ascii_case("-s")
                    || part.eq_ignore_ascii_case("session")
                {
                    scope = Some(false);
                } else {
                    path_tokens.push(part);
                }
            }
            let p_opt = if path_tokens.is_empty() {
                None
            } else {
                Some(path_tokens.join(" "))
            };
            (p_opt, scope)
        }
        None => (None, None),
    }
}

/// Resolve the directory from the argument or the picker. `None` stops the command.
async fn resolve_target_dir(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    config: &Config,
    path_str_opt: Option<String>,
) -> Result<Option<PathBuf>, HarnessError> {
    match path_str_opt {
        Some(p_str) => {
            let expanded = harness_core::config::expand_tilde(&p_str);
            let p = PathBuf::from(expanded);
            if !p.is_dir() {
                ui.emit_above(&format!(
                    "{} Directory not found: {}",
                    "✖".red().bold(),
                    p_str
                ));
                return Ok(None);
            }
            Ok(Some(harness_core::fsutil::canonicalize(&p).unwrap_or(p)))
        }
        None => {
            ui.redraw_input_line();
            picker::pick_workspace_directory(key_rx, ui, &config.paths.workspace_root).await
        }
    }
}

/// Determine scope: explicit from flag, or interactive prompt via select_from_rx. `None` means the
/// picker was cancelled and the box already restored.
async fn choose_dir_scope(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    explicit_scope: Option<bool>,
) -> Option<bool> {
    if let Some(global) = explicit_scope {
        return Some(global);
    }
    if !ui.is_attended {
        return Some(false);
    }
    let scope_options = [
        "This session only (active for current session)".to_string(),
        "Default global workspace (persisted in config for all sessions)".to_string(),
    ];
    while key_rx.try_recv().is_ok() {}
    let sel = picker::select_from_rx(
        key_rx,
        ui,
        "Workspace Scope",
        &scope_options,
        0,
        true,
        false,
    )
    .await;
    match sel {
        Some(1) => Some(true),
        Some(0) => Some(false),
        None => {
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            None
        }
        _ => Some(false),
    }
}

/// Persist the workspace root globally, warning when the write fails.
fn persist_global_workspace(ui: &ChatUi, target_dir: &Path) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    if let Err(e) = harness_core::config::set_in_file(
        &layers,
        "paths.workspace_root",
        &target_dir.display().to_string(),
    ) {
        ui.emit_above(&format!(
            "{} Failed to persist global workspace root: {}",
            "⚠".yellow(),
            e
        ));
    }
    Ok(())
}

/// Tell the user where the workspace now points.
fn report_workspace_switch(ui: &ChatUi, target_dir: &Path, is_global: bool) {
    if is_global {
        ui.emit_above(&format!(
            "{} Workspace switched globally to: {} (saved to config)",
            "✔".green().bold(),
            target_dir.display().to_string().bold()
        ));
    } else {
        ui.emit_above(&format!(
            "{} Workspace switched for this session: {}",
            "✔".green().bold(),
            target_dir.display().to_string().bold()
        ));
    }
}

async fn apply_workspace_change(
    config: &mut Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    target_dir: PathBuf,
) -> Result<(), HarnessError> {
    config.paths.workspace_root = target_dir.clone();

    let mut agent_guard = agent.lock().await;
    agent_guard.set_workspace_root(target_dir.clone());

    let read_cache = harness_core::tools::ReadCache::new();
    let ws = target_dir.clone();
    let extra = config.paths.extra_roots.clone();

    agent_guard
        .registry()
        .register(Arc::new(
            harness_core::tools::ReadFileTool::new(
                ws.clone(),
                config.tools.read_max_chars,
                read_cache.clone(),
            )
            .with_extra_roots(extra.clone()),
        ))
        .await?;
    agent_guard
        .registry()
        .register(Arc::new(
            harness_core::tools::WriteFileTool::new(ws.clone(), read_cache.clone())
                .with_extra_roots(extra.clone()),
        ))
        .await?;
    agent_guard
        .registry()
        .register(Arc::new(
            harness_core::tools::PatchTool::new(ws.clone(), read_cache.clone())
                .with_extra_roots(extra.clone()),
        ))
        .await?;
    agent_guard
        .registry()
        .register(Arc::new(
            harness_core::tools::SearchFilesTool::new(ws.clone()).with_extra_roots(extra.clone()),
        ))
        .await?;
    agent_guard
        .registry()
        .register(Arc::new(
            harness_core::tools::ListFilesTool::new(ws.clone()).with_extra_roots(extra.clone()),
        ))
        .await?;

    let term = harness_core::tools::TerminalTool::new(
        ws.clone(),
        config.tools.terminal_timeout_secs,
        config.tools.terminal_output_max_bytes,
    )
    .with_extra_roots(extra)
    .with_network_policy(config.tools.network)
    .with_prefix(config.tools.terminal_prefix.clone())
    .with_rtk_rewrite(config.tools.rtk_rewrite);
    agent_guard.registry().register(Arc::new(term)).await?;

    // Reload skills: global skills + workspace skills (.harness/skills)
    let mut all_skills = Vec::new();
    let _ = std::fs::create_dir_all(&config.paths.skills_dir);
    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir)
    {
        all_skills.extend(skills);
    }
    let workspace_skills = target_dir.join(".harness/skills");
    if workspace_skills.exists() {
        if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&workspace_skills) {
            all_skills.extend(skills);
        }
    }
    // The skills the harness ships with, including the desktop skill.
    all_skills.extend(crate::commands::desktop::builtin_skills());
    if !all_skills.is_empty() {
        let skills_arc = Arc::new(all_skills);
        agent_guard
            .registry()
            .register(Arc::new(harness_plugins::SkillViewTool::new(
                skills_arc.clone(),
            )))
            .await?;
        agent_guard
            .registry()
            .register(Arc::new(harness_plugins::SkillsListTool::new(skills_arc)))
            .await?;
    } else {
        let _ = agent_guard.registry().unregister("skill_view").await;
        let _ = agent_guard.registry().unregister("skills_list").await;
    }

    // Reload hooks from new workspace
    if let Ok(runner) = harness_core::hooks::HookRunner::load(&target_dir, &config.hooks) {
        agent_guard.set_hooks(Some(Arc::new(runner)));
    } else {
        agent_guard.set_hooks(None);
    }

    Ok(())
}
