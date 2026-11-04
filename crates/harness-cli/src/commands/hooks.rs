//! `luna hooks`: show which lifecycle hooks are actually loaded.
//!
//! This reads the *runner*, not the files. A hook listed in `~/.luna/hooks.json` is not armed if
//! `hooks.enabled` is false, if the event name is a typo, if the event is in `hooks.disabled`, or
//! if it lives in a repository file and `hooks.trust_workspace` is unset. Printing the file would
//! answer the wrong question; printing the runner answers the one the operator is asking.

use std::path::Path;

use colored::Colorize;
use harness_core::hooks::{HookRunner, PROJECT_HOOKS_RELATIVE, USER_HOOKS_RELATIVE};
use harness_core::HarnessError;

pub struct HooksOptions {
    pub json: bool,
}

pub fn run_hooks(config: harness_core::Config, options: HooksOptions) -> Result<(), HarnessError> {
    let workspace = &config.paths.workspace_root;
    let runner = HookRunner::load(workspace, &config.hooks)?;
    let bound = runner.bound();

    let home = harness_core::config::home_dir();
    let user_file = (home != Path::new(".")).then(|| home.join(USER_HOOKS_RELATIVE));
    let project_file = workspace.join(PROJECT_HOOKS_RELATIVE);

    if options.json {
        let hooks: Vec<serde_json::Value> = bound
            .iter()
            .map(|(event, spec)| {
                serde_json::json!({
                    "event": event.as_str(),
                    "matcher": spec.matcher,
                    "command": spec.command,
                    "timeout_secs": spec.timeout_secs.unwrap_or(config.hooks.timeout_secs),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "enabled": config.hooks.enabled,
                "trust_workspace": config.hooks.trust_workspace,
                "timeout_secs": runner.timeout().as_secs(),
                "disabled_events": config.hooks.disabled,
                "user_file": user_file.as_ref().map(|path| path.display().to_string()),
                "user_file_exists": user_file.as_ref().map(|path| path.is_file()),
                "project_file": project_file.display().to_string(),
                "project_file_exists": project_file.is_file(),
                "hooks": hooks,
            }))
            .unwrap_or_default()
        );
        return Ok(());
    }

    if !config.hooks.enabled {
        println!(
            "{} hooks are {} (`hooks.enabled: false`). Nothing below will run.",
            "note:".yellow().bold(),
            "off".red().bold()
        );
    }

    println!("{}", "Sources".bold());
    print_source("user", user_file.as_deref(), config.hooks.enabled);
    print_source(
        "project",
        Some(&project_file),
        config.hooks.enabled && config.hooks.trust_workspace,
    );
    if config.hooks.enabled && !config.hooks.trust_workspace && project_file.is_file() {
        println!(
            "  {} repository hooks are present but not trusted (set hooks.trust_workspace)",
            "!".yellow()
        );
    }

    println!("{}", format!("Bound hooks ({})", bound.len()).bold());
    if bound.is_empty() {
        println!("  none");
        return Ok(());
    }
    for (event, spec) in bound {
        let matcher = spec
            .matcher
            .as_deref()
            .map(|matcher| format!(" [{matcher}]"))
            .unwrap_or_default();
        println!(
            "  {}{}  {}",
            event.as_str().cyan(),
            matcher,
            spec.command.dimmed()
        );
    }
    println!(
        "  {}",
        format!("timeout {}s", runner.timeout().as_secs()).dimmed()
    );
    Ok(())
}

fn print_source(label: &str, path: Option<&Path>, active: bool) {
    match path {
        Some(path) if path.is_file() => println!(
            "  {label:8} {} {}",
            path.display(),
            if active {
                "".to_string()
            } else {
                "(ignored)".yellow().to_string()
            }
        ),
        Some(path) => println!("  {label:8} {} (none)", path.display().to_string().dimmed()),
        None => {}
    }
}
