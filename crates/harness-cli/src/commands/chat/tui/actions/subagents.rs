//! subagents

use super::*;

pub async fn collect_subagent_entries(env: &Env<'_>) -> Vec<SubagentUiEntry> {
    let mut entries = Vec::new();
    let tasks = crate::commands::chat::viewer::collect_tasks(
        Some(env.bg_queue.as_ref()),
        Some(env.delegate_tool.as_ref()),
    )
    .await;
    let sub_records: std::collections::HashMap<
        String,
        harness_core::tools::delegate::SubagentTaskRecord,
    > = env
        .delegate_tool
        .list_tasks()
        .await
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    let live_tasks: std::collections::HashMap<String, (String, String)> = env
        .ui
        .active_delegate_tasks()
        .into_iter()
        .map(|(id, goal, act)| (id, (goal, act)))
        .collect();

    for task in tasks {
        let is_worker = task.command.starts_with("worker:");
        let is_sub =
            task.id.starts_with("sub_") || is_worker || task.command.starts_with("subagent:");

        let agent = if is_worker {
            sub_records
                .get(&task.id)
                .and_then(|rec| worker_label_from_context(rec.context.as_deref()))
                .unwrap_or_else(|| "Worker".to_string())
        } else if let Some(stripped) = task.command.strip_prefix("subagent: ") {
            if let Some(pos) = stripped.find(':') {
                let cand = stripped[..pos].trim();
                if !cand.is_empty()
                    && cand
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                {
                    cand.to_string()
                } else {
                    "Subagent".to_string()
                }
            } else {
                "Subagent".to_string()
            }
        } else if task.id.starts_with("sub_") {
            "Subagent".to_string()
        } else {
            "Job".to_string()
        };

        let title = task
            .command
            .strip_prefix("worker: ")
            .or_else(|| task.command.strip_prefix("subagent: "))
            .unwrap_or(&task.command)
            .to_string();

        let live_act = live_tasks.get(&task.id).map(|(_, a)| a.clone());
        let activity = live_act.unwrap_or_else(|| {
            if task.status == "running" {
                "running".to_string()
            } else {
                task.status.clone()
            }
        });

        let mut turns = Vec::new();
        let mut log_tail = Vec::new();
        if is_sub {
            if let Ok(outcome) = env.store.replay_transcript(&task.id, usize::MAX).await {
                let parsed = parse_turns_from_history(&outcome.history);
                turns = if is_worker {
                    worker_turns_from_history(parsed, &title, &task.status)
                } else {
                    parsed
                };
            }
            if turns.is_empty() || task.status == "running" {
                log_tail = read_subagent_log_tail(&task.log_path, 80);
            }
        } else {
            let log_body = std::fs::read_to_string(&task.log_path).unwrap_or_default();
            turns.push(TurnRecord {
                user_query: task.command.clone(),
                reasoning: None,
                tools: Vec::new(),
                tool_items: Vec::new(),
                duration: std::time::Duration::from_secs(task.elapsed_secs),
                reply: if log_body.is_empty() {
                    "(no output yet)".to_string()
                } else {
                    log_body
                },
            });
        }

        entries.push(SubagentUiEntry {
            id: task.id,
            agent,
            title,
            status: task.status,
            activity,
            elapsed_secs: task.elapsed_secs,
            turns,
            log_tail,
        });
    }

    entries
}

pub async fn open_subagents_list(env: &Env<'_>, st: &mut TuiState) {
    let items = collect_subagent_entries(env).await;
    if items.is_empty() {
        env.ui
            .emit_above("No background tasks or subagents active or recorded");
        return;
    }
    let has_running = items.iter().any(|i| i.status == "running");
    st.modal = ActiveModal::SubagentsModal {
        items,
        selected: 0,
        active_only: has_running,
    };
}

pub async fn open_subagent_inspector(env: &Env<'_>, st: &mut TuiState, target_id: &str) {
    let items = collect_subagent_entries(env).await;
    if items.is_empty() {
        env.ui.emit_above("No subagents found");
        return;
    }
    let current_idx = items.iter().position(|e| e.id == target_id).unwrap_or(0);
    st.modal = ActiveModal::SubagentInspector {
        items,
        current_idx,
        scroll: 0,
    };
}

pub async fn interrupt_subagent(env: &Env<'_>, st: &mut TuiState, id: &str) {
    let killed = env.delegate_tool.kill(id).await;
    env.ui.finish_delegate_task(id);
    if killed {
        env.ui.emit_above(&format!("Stopped subagent [{id}]"));
    }
    match &mut st.modal {
        ActiveModal::SubagentsModal { items, .. } => {
            if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                item.status = "cancelled".to_string();
                item.activity = "cancelled by user".to_string();
            }
        }
        ActiveModal::SubagentInspector {
            items, current_idx, ..
        } => {
            if let Some(item) = items.get_mut(*current_idx) {
                if item.id == id {
                    item.status = "cancelled".to_string();
                    item.activity = "cancelled by user".to_string();
                }
            }
        }
        _ => {}
    }
}

pub async fn refresh_subagents_if_open(env: &Env<'_>, st: &mut TuiState) {
    if !matches!(
        st.modal,
        ActiveModal::SubagentInspector { .. } | ActiveModal::SubagentsModal { .. }
    ) {
        return;
    }
    let sub_records: std::collections::HashMap<
        String,
        harness_core::tools::delegate::SubagentTaskRecord,
    > = env
        .delegate_tool
        .list_tasks()
        .await
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();
    let now = chrono::Utc::now();

    match &mut st.modal {
        ActiveModal::SubagentInspector {
            items, current_idx, ..
        } => {
            if let Some(item) = items.get_mut(*current_idx) {
                let is_worker = item.agent == "Worker" || item.agent.starts_with("worker:");
                if let Some(rec) = sub_records.get(&item.id) {
                    let (status_str, end_at) = match &rec.status {
                        harness_core::tools::delegate::SubagentStatus::Running => {
                            ("running".to_string(), now)
                        }
                        harness_core::tools::delegate::SubagentStatus::Completed {
                            finished_at,
                            ..
                        } => ("completed".to_string(), *finished_at),
                        harness_core::tools::delegate::SubagentStatus::Failed {
                            error,
                            finished_at,
                        } => (format!("failed: {error}"), *finished_at),
                    };
                    item.status = status_str;
                    item.elapsed_secs = (end_at - rec.started_at).num_seconds().max(0) as u64;
                    if let Some(ref lp) = rec.log_path {
                        if item.turns.is_empty() || item.status == "running" {
                            item.log_tail = read_subagent_log_tail(lp, 80);
                        }
                    }
                }
                if let Ok(outcome) = env.store.replay_transcript(&item.id, usize::MAX).await {
                    let parsed = parse_turns_from_history(&outcome.history);
                    let replayed = if is_worker {
                        worker_turns_from_history(parsed, &item.title, &item.status)
                    } else {
                        parsed
                    };
                    if !replayed.is_empty() {
                        item.turns = replayed;
                    }
                }
                let tasks = env.ui.active_delegate_tasks();
                if let Some((_, _, act)) = tasks.into_iter().find(|(id, _, _)| id == &item.id) {
                    item.activity = act;
                } else if item.status != "running" {
                    item.activity = item.status.clone();
                }
            }
        }
        ActiveModal::SubagentsModal { items, .. } => {
            let tasks = env.ui.active_delegate_tasks();
            for item in items.iter_mut() {
                if let Some(rec) = sub_records.get(&item.id) {
                    let (status_str, end_at) = match &rec.status {
                        harness_core::tools::delegate::SubagentStatus::Running => {
                            ("running".to_string(), now)
                        }
                        harness_core::tools::delegate::SubagentStatus::Completed {
                            finished_at,
                            ..
                        } => ("completed".to_string(), *finished_at),
                        harness_core::tools::delegate::SubagentStatus::Failed {
                            error,
                            finished_at,
                        } => (format!("failed: {error}"), *finished_at),
                    };
                    item.status = status_str;
                    item.elapsed_secs = (end_at - rec.started_at).num_seconds().max(0) as u64;
                }
                if item.status == "running" {
                    if let Some((_, _, act)) = tasks.iter().find(|(id, _, _)| id == &item.id) {
                        item.activity = act.clone();
                    }
                } else {
                    item.activity = item.status.clone();
                }
            }
        }
        _ => {}
    }
}

pub(crate) async fn open_task_list(env: &Env<'_>, st: &mut TuiState) {
    open_subagents_list(env, st).await;
}

pub(crate) async fn open_auto_compact_threshold(env: &Env<'_>, st: &mut TuiState) {
    let (_, threshold, _) = env.agent.lock().await.auto_compact_settings();
    st.modal = ActiveModal::InputModal {
        title: "Auto-compact — Threshold".to_string(),
        prompt: "Token threshold, e.g. 200k. Blank keeps the default".to_string(),
        value: threshold.to_string(),
        secret: false,
        action: InputAction::AutoCompactThreshold,
    };
}

pub(crate) fn open_auto_compact_percent(st: &mut TuiState, threshold: usize, current: usize) {
    st.modal = ActiveModal::InputModal {
        title: "Auto-compact — Percentage".to_string(),
        prompt: "Percent of context to compact, 1–100".to_string(),
        value: current.to_string(),
        secret: false,
        action: InputAction::AutoCompactPercent { threshold },
    };
}

pub(crate) fn open_profile_create_name(st: &mut TuiState) {
    st.modal = ActiveModal::InputModal {
        title: "Create Profile — Name".to_string(),
        prompt: "Lowercase letters, digits and '-'".to_string(),
        value: String::new(),
        secret: false,
        action: InputAction::ProfileCreateName,
    };
}

pub(crate) fn open_profile_create_summary(st: &mut TuiState, name: String) {
    st.modal = ActiveModal::InputModal {
        title: "Create Profile — Summary".to_string(),
        prompt: "One line describing what this profile is for".to_string(),
        value: "Custom profile".to_string(),
        secret: false,
        action: InputAction::ProfileCreateSummary { name },
    };
}

pub(crate) fn open_profile_create_providers(st: &mut TuiState, name: String, summary: String) {
    st.modal = ActiveModal::SelectModal {
        title: "Create Profile — Providers".to_string(),
        items: vec![
            "Yes, copy the default profile's credentials".to_string(),
            "No, start with none".to_string(),
        ],
        selected: 0,
        action: SelectAction::ProfileCreateProviders { name, summary },
    };
}

