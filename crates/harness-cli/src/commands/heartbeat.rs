use colored::Colorize;
use harness_core::{CompletionOptions, Config, HarnessError, Message, ModelProvider, ToolRegistry};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Instant;

use super::chat::render::TurnRecord;
use super::chat::ui::ChatUi;
use super::chat::ChatAgent;
use super::context_window::context_window_for;

static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

pub fn uptime() -> std::time::Duration {
    PROCESS_START.elapsed()
}

pub fn format_duration(dur: std::time::Duration) -> String {
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub fn get_process_rss() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        let page_size = 4096;
        let mb = (pages * page_size) as f64 / (1024.0 * 1024.0);
        Some(format!("{:.1} MB", mb))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub fn get_git_status(path: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "-b"])
        .current_dir(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let branch_line = lines.next().unwrap_or("## unknown");
    let branch = branch_line
        .strip_prefix("## ")
        .and_then(|s| s.split("...").next())
        .unwrap_or(branch_line);

    let dirty_count = lines.count();
    let status = if dirty_count == 0 {
        format!("branch {}, clean", branch)
    } else {
        format!("branch {}, {} modified", branch, dirty_count)
    };

    Some(status)
}

pub async fn handle_chat_heartbeat(
    arg: Option<&str>,
    ui: &ChatUi,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    current_provider: &str,
    current_model: &str,
    turns: &[TurnRecord],
) {
    let pid = std::process::id();
    let up = format_duration(uptime());
    let rss = get_process_rss().unwrap_or_else(|| "n/a".to_string());

    let cwd = std::env::current_dir().unwrap_or_default();
    let dir_name = cwd.file_name().and_then(|n| n.to_str()).unwrap_or(".");
    let git = get_git_status(&cwd).unwrap_or_else(|| "non-git".to_string());

    // Check agent state safely without deadlocking active turns
    let (status_str, tools_str, evolver_str, context_str) = match agent.try_lock() {
        Ok(ag) => {
            let tools = ag.registry().list_tools().await.unwrap_or_default();
            let tool_names: Vec<String> = tools.into_iter().map(|t| t.name).collect();
            let tools_desc = format!(
                "{} registered ({})",
                tool_names.len(),
                tool_names.join(", ")
            );

            let evolver_desc = {
                let jobs = ag.evolver().list_jobs().await.unwrap_or_default();
                let active: Vec<_> = jobs
                    .iter()
                    .filter(|j| j.status == harness_evolver::JobStatus::Running)
                    .collect();
                if active.is_empty() {
                    format!("idle ({} completed jobs)", jobs.len())
                } else {
                    let j = active[0];
                    format!(
                        "{} active: #{} [tool: {}]",
                        active.len(),
                        j.id,
                        j.current_tool.as_deref().unwrap_or("running")
                    )
                }
            };

            let ctx = ag.estimate_session_tokens().await.unwrap_or(0);
            let max_ctx = context_window_for(config, current_provider, current_model);
            let pct = if max_ctx > 0 {
                (ctx as f64 / max_ctx as f64) * 100.0
            } else {
                0.0
            };
            let ctx_desc = format!("{} / {}k tokens ({:.1}%)", ctx, max_ctx / 1000, pct);

            ("idle", tools_desc, evolver_desc, ctx_desc)
        }
        Err(_) => (
            "active (turn in progress)",
            "n/a (agent busy)".to_string(),
            "n/a".to_string(),
            "n/a".to_string(),
        ),
    };

    let mut lines = Vec::new();
    lines.push(format!(
        "heartbeat: {} (pid {}, uptime {}, rss {})",
        "ok".green().bold(),
        pid,
        up,
        rss
    ));
    lines.push(format!(
        "  provider:   {} ({})",
        current_model.cyan(),
        current_provider
    ));
    lines.push(format!(
        "  session:    {} ({} turns · {})",
        session_id.dimmed(),
        turns.len(),
        context_str
    ));
    lines.push(format!("  status:     {}", status_str));
    lines.push(format!("  tools:      {}", tools_str));
    lines.push(format!("  evolver:    {}", evolver_str));
    lines.push(format!("  workspace:  {} ({})", dir_name, git));

    if arg == Some("ping") {
        if let Ok(ag) = agent.try_lock() {
            let provider = ag.provider().clone();
            let start = Instant::now();
            let msg = Message::user("ping");
            let opts = CompletionOptions {
                max_tokens: Some(1),
                model: Some(current_model.to_string()),
                ..Default::default()
            };
            match provider.complete(&[msg], &[], &opts).await {
                Ok(_) => {
                    let lat = start.elapsed().as_millis();
                    lines.push(format!(
                        "  ping:       {}ms ({} API ok)",
                        lat, current_provider
                    ));
                }
                Err(e) => {
                    lines.push(format!("  ping:       {} ({})", "failed".red(), e));
                }
            }
        } else {
            lines.push(format!("  ping:       {}", "skipped (agent busy)".dimmed()));
        }
    }

    for line in lines {
        ui.emit_above(&line);
    }
}

pub async fn run_cli_heartbeat(
    target: Option<&str>,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    let pid = std::process::id();
    let rss = get_process_rss().unwrap_or_else(|| "n/a".to_string());
    let cwd = std::env::current_dir().unwrap_or_default();
    let dir_name = cwd.file_name().and_then(|n| n.to_str()).unwrap_or(".");
    let git = get_git_status(&cwd).unwrap_or_else(|| "non-git".to_string());

    let provider_id = &config.model.provider;
    let model_id = &config.model.default;

    println!(
        "heartbeat: {} (pid {}, rss {})",
        "ok".green().bold(),
        pid,
        rss
    );
    println!("  provider:   {} ({})", model_id.cyan(), provider_id);
    println!("  workspace:  {} ({})", dir_name, git);
    println!("  approval:   {:?}", config.tools.approval);

    if target == Some("ping") {
        let print_provider = GenericProviderClient::new(
            provider_id,
            model_id,
            cred_manager.clone(),
            discovery.clone(),
            None,
        )?;
        let start = Instant::now();
        let msg = Message::user("ping");
        let opts = CompletionOptions {
            max_tokens: Some(1),
            model: Some(model_id.to_string()),
            ..Default::default()
        };
        match print_provider.complete(&[msg], &[], &opts).await {
            Ok(_) => {
                let lat = start.elapsed().as_millis();
                println!("  ping:       {}ms ({} API ok)", lat, provider_id);
            }
            Err(e) => {
                println!("  ping:       {} ({})", "failed".red(), e);
            }
        }
    }

    Ok(())
}
