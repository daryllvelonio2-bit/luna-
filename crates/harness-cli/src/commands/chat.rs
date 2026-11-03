use colored::Colorize;
use dialoguer::console::Term;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_core::BackgroundSaver;
use harness_core::{
    Agent, ApprovalRequest, Config, DefaultToolRegistry, HarnessError, ModelProvider,
    SessionBinding, SessionStore, ToolRegistry, TurnOutcome,
};
use harness_mcp::{McpStdioClient, McpToolAdapter};
use harness_memory::{ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool};
use harness_notes::{
    DeleteNoteTool, FileNotesEngine, ListNotesTool, ReadNoteTool, SaveNoteTool, SearchNotesTool,
    UpdateNoteTool,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use super::approval::{approval_settings, build_approver, ApprovalChoice, ChatApprover};
use super::context_window::context_window;
use super::sessions::{open_store, resolve_session, SessionSelection};
use tokio::sync::oneshot;

pub mod commands;
pub(crate) mod connect;
pub(crate) mod history;
pub(crate) mod keys;
pub(crate) mod memory_writer;
pub(crate) mod modal;
pub(crate) mod prompts;
pub(crate) mod render;
pub(crate) mod status;
pub mod todo;
pub(crate) mod turn;
pub mod ui;
pub mod viewer;

#[cfg(test)]
mod tests;

use commands::{
    command_entries, execute_slash_command, handle_auto_compact_command, handle_compact_command,
    handle_compact_model_switch, handle_delete_command, handle_diff_command, handle_fork_command,
    handle_model_switch, handle_name_command, handle_new_command, handle_reasoning_switch,
    handle_rewind_command, handle_sessions_switch, handle_summarize_and_new_command,
    handle_thinking_switch, handle_usage_command, select_slash_command, SlashOutcome,
};
use keys::AppKey;
use render::{
    compaction_report, format_approval_verdict, format_delegate_duration,
    format_delegate_notification, format_delegate_title, format_patch_diff, format_tool_call,
    format_tool_result, has_verdict, is_non_tool_event, parse_turns_from_history,
    render_transcript, right_aligned_user_message, OutputBlockKind, TurnProcessItem, TurnRecord,
    LUNA_LABEL, PROMPT,
};
use ui::{get_terminal_width, ChatUi, RawTerminal, ANIMATION_INTERVAL};
use viewer::show_background_task_viewer;

pub(crate) type ChatAgent =
    Agent<GenericProviderClient, SqliteMemoryEngine, FileNotesEngine, DefaultToolRegistry>;

/// Draw the transcript.
///
/// The todo panel is not drawn here: it is chrome, drawn above the composer by
/// [`super::ui::ChatUi`], so it stays put while the conversation scrolls under it.
fn redraw_transcript(turns: &[TurnRecord], expanded: bool) {
    render_transcript(turns, expanded, get_terminal_width());
}

pub(crate) use history::clear_on_exit;
use history::{append_prompt_history, load_prompt_history, KittyKeyboardGuard};
use prompts::{prompt_approval_choice, prompt_sudo_password_dialog, ChatSudoPrompter};
pub(crate) use status::{refresh_antigravity_quota_owned, refresh_antigravity_status_if_needed};
use turn::handle_composer_command;
use turn::{cancel_running_turn, drain_queue, launch_turn, submission, Submission};

/// Register MCP servers from configuration and add their tools to the registry.
async fn register_mcp_servers(
    registry: &Arc<DefaultToolRegistry>,
    config: &Config,
) -> Result<(), HarnessError> {
    if !config.mcp.enabled || config.mcp.servers.is_empty() {
        return Ok(());
    }

    for server_config in &config.mcp.servers {
        if !server_config.enabled {
            continue;
        }

        // Spawn the MCP stdio server
        let args: Vec<&str> = server_config.args.iter().map(|s| s.as_str()).collect();
        // `~` is expanded here rather than in the config type: the config keeps
        // what the user wrote, and only the spawn needs a real path.
        let cwd = server_config
            .cwd
            .as_ref()
            .map(|dir| PathBuf::from(harness_core::config::expand_tilde(&dir.to_string_lossy())));
        let client = match McpStdioClient::spawn(
            &server_config.command,
            &args,
            &server_config.env,
            cwd.as_deref(),
            config.mcp.startup_timeout_secs,
            config.mcp.call_timeout_secs,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "warning: failed to start MCP server '{}': {}",
                    server_config.name, e
                );
                continue;
            }
        };

        // List tools from the server
        let tools = match client.list_tools().await {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "warning: failed to list tools from MCP server '{}': {}",
                    server_config.name, e
                );
                continue;
            }
        };

        // Filter tools if an allowlist is configured
        let allowed: std::collections::HashSet<_> = server_config.tools.iter().cloned().collect();
        let filtered_tools: Vec<_> = tools
            .into_iter()
            .filter(|t| allowed.is_empty() || allowed.contains(&t.name))
            .collect();

        // Register each tool under the namespace, dispatching to the server's own
        // name: the namespace is local to this process and means nothing to the
        // server, which would reject `mcp__<server>__<tool>` as an unknown tool.
        for tool_info in &filtered_tools {
            let namespaced_name = format!("mcp__{}__{}", server_config.name, tool_info.name);
            let adapter = McpToolAdapter::namespaced(
                namespaced_name.clone(),
                tool_info.clone(),
                client.clone(),
            );
            if let Err(e) = registry.register(Arc::new(adapter)).await {
                eprintln!(
                    "warning: failed to register MCP tool '{}': {}",
                    namespaced_name, e
                );
            }
        }

        if !server_config.tools.is_empty() {
            eprintln!(
                "info: MCP server '{}' registered {} tool(s)",
                server_config.name,
                filtered_tools.len()
            );
        }
    }

    Ok(())
}

pub async fn run_interactive_chat(
    mut config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    selection: SessionSelection,
    explicit_model: bool,
) -> Result<(), HarnessError> {
    // Every turn is appended to this session; a resumed one replays its recent transcript.
    // Resolved before the provider client so the session's stored model choice can take effect.
    let store = Arc::new(open_store(&config)?);
    // One recorder for the session: the ledger handle is process-wide, and the prices are resolved
    // from the config once rather than per turn.
    let usage = std::sync::Arc::new(super::usage::UsageRecorder::from_config(&config));
    let mut session_id = resolve_session(store.as_ref(), selection).await?;
    // The todo list belongs to the session, not to the workspace: a new session starts
    // with an empty list and never sees another session's tasks. The scope is a shared
    // handle, so the tools follow the session when it forks or starts over.
    let todo_scope =
        harness_core::tools::TodoScope::new(config.paths.todos_dir.clone(), &session_id);

    let mut current_provider = config.model.provider.clone();
    let mut current_model = config.model.default.clone();

    // A session carries its own model choice: `/model` stamps it on the session row, so a
    // resumed session returns to the model that conversation was switched to even when the
    // config default has moved on. An explicit CLI `--provider`/`--model` outranks the stored
    // choice for the run but does not overwrite it.
    let record = store.get(&session_id).await?;
    if !explicit_model {
        if let Some((provider, model)) = record.model.clone() {
            current_provider = provider;
            current_model = model;
        }
    }

    if current_provider.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "No LLM provider configured or loaded with credentials. Run 'luna auth' to register a credential or specify '--provider <name>'.".to_string(),
        ));
    }

    // Stamp the model this session starts on (a fresh session, or a row written before model
    // persistence existed) so the choice stays durable even after the config default changes.
    if record.model.is_none() {
        store
            .set_model(&session_id, &current_provider, &current_model)
            .await?;
    }

    let provider = Arc::new(GenericProviderClient::new(
        current_provider.clone(),
        current_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&current_provider),
    )?);

    // Compaction model: priority is session-specific override -> config setting -> active model
    let (compaction_provider_id, compaction_model) = match &record.compact_model {
        Some((p, m)) => (p.clone(), m.clone()),
        None => {
            let p = config
                .agent
                .compaction_provider
                .clone()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| current_provider.clone());
            let m = config
                .agent
                .compaction_model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| current_model.clone());
            (p, m)
        }
    };
    let compaction = match GenericProviderClient::new(
        compaction_provider_id.clone(),
        compaction_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&compaction_provider_id),
    ) {
        Ok(client) => {
            let compaction_provider: Arc<dyn ModelProvider> = Arc::new(client);
            Some(harness_core::CompactionSetup {
                provider: compaction_provider,
                model: compaction_model,
            })
        }
        // An unusable compaction provider is a warning, not a startup failure: the session still
        // runs, and the turn that cannot compact says so.
        Err(error) => {
            eprintln!(
                "warning: context compaction is unavailable ({}): {}",
                compaction_provider_id, error
            );
            None
        }
    };

    let memory = Arc::new(SqliteMemoryEngine::new(Some(
        config.paths.memory_db.clone(),
    ))?);
    let notes = Arc::new(FileNotesEngine::new(
        config.paths.notes_dir.clone(),
        Some(config.paths.notes_db.clone()),
    )?);

    // Who writes durable memory. `session` (the default) keeps the original behaviour: the agent
    // decides mid-turn and calls `save_memory` / `save_note`. `background` hands the pen to an
    // independent saver that reads each finished turn and writes the same records through the same
    // tool, so the write tools are withheld from the session to keep exactly one writer.
    let background_mode = config.memory.mode.is_background();
    let memory_saver = memory_writer::build_saver(
        &config,
        &memory,
        notes.clone(),
        &current_provider,
        &current_model,
        cred_manager.clone(),
        discovery.clone(),
    );
    let registry = Arc::new(DefaultToolRegistry::new());

    // Register native memory tools.
    // In background saver mode, all memory/note modification tools are withheld from the main agent
    // because the background saver owns memory creation, updating, and persistence completely.
    // In session writer mode, the main agent is the primary writer and receives the full toolset.
    if !background_mode {
        registry
            .register(Arc::new(SaveMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(UpdateMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(ForgetMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(SaveNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(UpdateNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(DeleteNoteTool::new(notes.clone())))
            .await?;
    }

    // Read-only note tools are always available so the agent can reference existing knowledge:
    registry
        .register(Arc::new(SearchNotesTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(ListNotesTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(ReadNoteTool::new(notes.clone())))
        .await?;

    // One line of notice, because a mode that changes *where* records come from is worth stating.
    if memory_saver.is_some() {
        if let Some(line) = memory_writer::notice(&config) {
            eprintln!("{line}");
        }
    }

    // Load skills from skills_dir and workspace .harness/skills
    let mut all_skills = Vec::new();
    let _ = std::fs::create_dir_all(&config.paths.skills_dir);
    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir)
    {
        all_skills.extend(skills);
    }
    let workspace_skills = config.paths.workspace_root.join(".harness/skills");
    if workspace_skills.exists() {
        if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&workspace_skills) {
            all_skills.extend(skills);
        }
    }

    let skills_summary = if all_skills.is_empty() {
        None
    } else {
        let lines: Vec<String> = all_skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect();
        Some(lines.join("\n"))
    };
    // The model's index above keeps the full descriptions — choosing between skills is done from
    // them. `/skills` prints this readout instead: a paragraph per skill is a wall of text on a
    // terminal, and the procedure itself is `skill_view`'s job.
    let skills_readout = crate::commands::skills::readout(&all_skills);

    // Skills are shared with the `skill_view` / `skills_list` tools and borrowed as a slice for
    // the command surface, so `/<skill-name>` can dispatch from the slash menu.
    let skills_arc = if all_skills.is_empty() {
        None
    } else {
        Some(Arc::new(all_skills))
    };
    if let Some(skills_arc) = &skills_arc {
        registry
            .register(Arc::new(harness_plugins::SkillViewTool::new(
                skills_arc.clone(),
            )))
            .await?;
        registry
            .register(Arc::new(harness_plugins::SkillsListTool::new(
                skills_arc.clone(),
            )))
            .await?;
    }
    let skills: &[harness_plugins::Skill] = skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);

    let bg_queue = Arc::new(
        harness_core::tasks::BackgroundTaskQueue::new(config.paths.jobs_dir.clone())
            .with_network_policy(config.tools.network),
    );
    let delegate_tool = Arc::new(harness_core::tools::DelegateTaskTool::new(
        provider.clone(),
        memory.clone(),
        notes.clone(),
        Some(store.clone()),
        config.clone(),
    ));
    registry.register(delegate_tool.clone()).await?;

    // Register core filesystem and shell tools (§2.2)
    let sudo_cache = Arc::new(std::sync::Mutex::new(std::env::var("SUDO_PASSWORD").ok()));
    let (sudo_tx, mut sudo_rx) =
        mpsc::unbounded_channel::<(String, oneshot::Sender<Option<String>>)>();
    let sudo_prompter = Arc::new(ChatSudoPrompter::new(sudo_tx));

    let read_cache = harness_core::tools::ReadCache::new();
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    // `terminal` and `process` must observe the same background-job registry as `/jobs` and the
    // task viewer — hand the registration the queue this host already holds instead of letting it
    // create a second one that only the tools would see.
    harness_core::tools::register_core_tools_shared_queue(
        &reg_dyn,
        &config,
        read_cache,
        Some(sudo_cache),
        Some(sudo_prompter),
        bg_queue.clone(),
        todo_scope.clone(),
    )
    .await?;

    // Register MCP servers from config
    register_mcp_servers(&registry, &config).await?;

    // Per-turn file snapshots: registered as registry middleware so the copy happens immediately
    // before the write, not somewhere near it.
    let checkpoints = Arc::new(harness_core::CheckpointController::new(
        config.paths.checkpoints_dir.clone(),
        config.paths.workspace_root.clone(),
        &config.checkpoints,
    ));
    registry
        .register_middleware(Arc::new(harness_core::CheckpointMiddleware::new(
            checkpoints.clone(),
        )))
        .await;

    let in_kitty = std::env::var("KITTY_WINDOW_ID").is_ok();
    let is_attended = Term::stdout().features().is_attended() || in_kitty;
    let ui = Arc::new(ChatUi::new(is_attended));
    let ui_obs = ui.clone();

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<(
        ApprovalRequest,
        oneshot::Sender<harness_core::ApprovalDecision>,
    )>();

    let allow_all_session = Arc::new(std::sync::atomic::AtomicBool::new(
        config.tools.approval == harness_core::ApprovalMode::Allow,
    ));
    let plan_mode = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let approver: Arc<dyn harness_core::Approver> =
        if is_attended && config.tools.approval == harness_core::ApprovalMode::Ask {
            Arc::new(ChatApprover::new(
                approval_tx.clone(),
                allow_all_session.clone(),
                plan_mode.clone(),
            ))
        } else {
            build_approver(&config)
        };

    let (clarify_tx, mut clarify_rx) = mpsc::unbounded_channel::<(
        crate::commands::clarify::ClarifyRequest,
        oneshot::Sender<crate::commands::clarify::ClarifyResponse>,
    )>();
    registry
        .register(Arc::new(crate::commands::clarify::ClarifyTool::new(
            clarify_tx.clone(),
        )))
        .await?;

    harness_providers::opencode::set_ui_bridge(harness_providers::opencode::OpencodeUiBridge {
        clarify_tx: clarify_tx.clone(),
        approval_tx: approval_tx.clone(),
        allow_all: allow_all_session.clone(),
    });


    let mut agent_config = config.agent.clone();
    agent_config.skills_summary = skills_summary.clone();
    agent_config.workspace_root = Some(config.paths.workspace_root.clone());
    agent_config.memory_mode = config.memory.mode;

    let agent = Agent::new(provider, memory, notes, registry, agent_config.clone())
        .with_permissions(harness_core::PermissionEngine::validate(
            config.tools.permissions.clone(),
        )?)
        .with_approval(approver.clone(), approval_settings(&config))
        .with_session(SessionBinding {
            store: store.clone(),
            session_id: session_id.clone(),
            resume_max_messages: config.session.resume_max_messages,
        });
    let mut agent = agent;
    // Workers run in the background but must ask the same human as the main turn: hand the chat
    // approver to the delegate tool so `/<name>` edits prompt (and honor Shift+Tab yolo) exactly
    // like a main-model edit.
    delegate_tool.set_worker_approver(approver.clone());
    // Attach the independent writer when the config asked for it; `None` leaves the session the
    // only writer, which is the original behaviour.
    agent.set_background_saver(memory_saver.map(|saver| saver as Arc<dyn BackgroundSaver>));

    let in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight_tools_obs = in_flight_tools.clone();
    // The observer reads the session's list back, so the transcript can show the tasks.
    let todo_scope_obs = todo_scope.clone();
    // True only while a fold the *user* asked for is rewriting the session in place. The input
    // path reads it to decide between queueing a message and interrupting a turn: a turn started
    // then would be built on history that is about to change under it.
    //
    // An automatic fold no longer sets it. Its summary lands while the turn that tripped it runs,
    // and blocking the composer for the length of a summarising call is what made auto-compact
    // feel like a stall — the agent has a transcript to work from either way.
    let compacting = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let compacting_obs = compacting.clone();

    // Lifecycle hooks: opt-in, loaded from the user file and (only when trusted) the project
    // file. Loading is fail-closed, so a missing file means an empty runner, not an error.
    let hooks = Arc::new(harness_core::HookRunner::load(
        &config.paths.workspace_root,
        &config.hooks,
    )?);
    let mut agent = agent
        .with_hooks(hooks.clone())
        .with_checkpoints(checkpoints.clone())
        .with_observer(Arc::new(move |event| match event {
            // Reasoning lands in the live region and is settled into the transcript by the animation
            // tick, so it is visible while it is still being produced.
            harness_core::AgentEvent::ReasoningDelta(text) => {
                ui_obs.clear_patch_diff();
                ui_obs.push_reasoning(&text);
            }
            harness_core::AgentEvent::ContextTokensUpdated { tokens_used } => {
                ui_obs.update_context_used(tokens_used);
            }
            harness_core::AgentEvent::ToolExecuting { name, arguments } => {
                ui_obs.clear_patch_diff();
                ui_obs.finish_reasoning();
                // A todo call reads as the task it acts on, not as the arguments it was sent.
                let call_fmt = if name == "todo_list" {
                    todo::format_call(&arguments, &todo_scope_obs)
                } else {
                    format_tool_call(&name, &arguments)
                };
                let diff_lines = if name == "patch" {
                    let old_str = arguments
                        .get("old_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let new_str = arguments
                        .get("new_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    format_patch_diff(old_str, new_str)
                } else {
                    Vec::new()
                };
                if name != "todo_list" {
                    ui_obs.begin_block(OutputBlockKind::Tool);
                    // The call goes into the live region the instant it is invoked, so a slow tool is
                    // visible while it runs; `ToolExecuted` below finalizes the same line into the
                    // transcript with its verdict.
                    ui_obs.start_activity(&call_fmt, &name, &arguments);
                    in_flight_tools_obs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(TurnProcessItem {
                            call_formatted: call_fmt,
                            result_formatted: String::new(),
                            diff_lines,
                        });
                }
            }
            harness_core::AgentEvent::ToolExecuted {
                name,
                is_error,
                output,
            } => {
                if name == "todo_list" {
                    ui_obs.stop_activity();
                    // The list changed: refresh before drawing so the panel displays the new state immediately
                    todo::refresh_panel(&ui_obs, &todo_scope_obs);
                    if is_error {
                        let line = format_tool_result(true, &output);
                        ui_obs.emit_above(&line);
                    }
                    return;
                }
                ui_obs.stop_activity();
                let status = if is_error {
                    " (failed)".red().to_string()
                } else {
                    " (success)".green().to_string()
                };
                let (line, diff_to_show) = {
                    let mut tools = in_flight_tools_obs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    // When multiple tool calls run in parallel, in_flight_tools holds all of them.
                    // Match the first in-flight tool call that hasn't received a verdict yet,
                    // preserving call order. Using tools.last_mut() repeatedly updated the last
                    // tool in the batch, appending duplicate verdicts and printing it multiple times.
                    let target = tools.iter_mut().find(|it| {
                        !is_non_tool_event(&it.call_formatted) && !has_verdict(&it.call_formatted)
                    });
                    if let Some(item) = target {
                        item.call_formatted.push_str(&status);
                        item.result_formatted.clear();
                        let diff = if !is_error && name == "patch" {
                            item.diff_lines.clone()
                        } else {
                            Vec::new()
                        };
                        (item.call_formatted.clone(), diff)
                    } else {
                        (format_tool_result(is_error, &output), Vec::new())
                    }
                };
                ui_obs.emit_above(&line);
                if !diff_to_show.is_empty() {
                    ui_obs.show_patch_diff(diff_to_show);
                }
            }
            harness_core::AgentEvent::Steered { text } => {
                ui_obs.clear_patch_diff();
                ui_obs.finish_reasoning();
                let line = format!("{} Delivered steer into turn: {}", "⏩".cyan().bold(), text);
                in_flight_tools_obs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(TurnProcessItem {
                        call_formatted: line.clone(),
                        result_formatted: String::new(),
                        diff_lines: Vec::new(),
                    });
                ui_obs.emit_above(&line);
            }
            harness_core::AgentEvent::CompactionStarted { forced } => {
                // Only a fold that rewrites what the composer would send holds the composer.
                compacting_obs.store(forced, std::sync::atomic::Ordering::SeqCst);
                ui_obs.start_compaction(forced);
            }
            harness_core::AgentEvent::CompactionFinished { folded, error } => {
                ui_obs.finish_compaction();
                compacting_obs.store(false, std::sync::atomic::Ordering::SeqCst);
                if let Some(err) = error {
                    ui_obs.emit_above(&format!("{} Compaction failed: {}", "✖".red(), err));
                } else if let Some(notice) = folded {
                    ui_obs.emit_above(&compaction_report(&notice));
                }
            }
        }));
    if let Some(setup) = compaction {
        agent = agent.with_compaction(setup);
    }

    let steer_handle = agent.steer_handle();
    let agent = Arc::new(tokio::sync::Mutex::new(agent));

    delegate_tool.set_steer_handle(steer_handle.clone());
    let delegate_trackers: Arc<
        std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, String)>>,
    > = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let trackers_events = delegate_trackers.clone();
    let ui_delegate_events = ui.clone();
    delegate_tool.set_event_notifier(Arc::new(move |event| {
        use harness_core::tools::delegate::{SubagentActivity, SubagentTaskEvent};
        match event {
            SubagentTaskEvent::Started { id, goal } => {
                // The live row is the notification: `start_delegate_task` draws it above the
                // composer the moment the child starts, and every event after this one repaints
                // that same row. Printing a line here as well put the child's state on screen
                // twice from the first instant.
                trackers_events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        id.clone(),
                        (std::time::Instant::now(), "starting".to_string()),
                    );
                ui_delegate_events.start_delegate_task(&id, &goal);
            }
            SubagentTaskEvent::Progress { id, activity, .. } => {
                // Progress belongs to the live row and nowhere else. A transcript line per tool
                // event stacked one copy of the same notification per event — a long child buried
                // the conversation under its own progress. `update_delegate_task` repaints the
                // single running row in place instead, which is what the row is for.
                {
                    let mut trackers = trackers_events.lock().unwrap_or_else(|e| e.into_inner());
                    let entry = trackers
                        .entry(id.clone())
                        .or_insert_with(|| (std::time::Instant::now(), "starting".to_string()));
                    entry.1 = match &activity {
                        SubagentActivity::ToolStarted { name } => name.clone(),
                        SubagentActivity::ToolFinished { name, is_error } if *is_error => {
                            format!("{} (failed)", name)
                        }
                        SubagentActivity::ToolFinished { name, .. } => name.clone(),
                        SubagentActivity::ModelStreaming => "streaming".to_string(),
                    };
                }

                let label = match &activity {
                    SubagentActivity::ModelStreaming => "streaming".to_string(),
                    SubagentActivity::ToolStarted { name } => format!("{} running", name),
                    SubagentActivity::ToolFinished { name, is_error } => {
                        format!("{} {}", name, if *is_error { "failed" } else { "done" })
                    }
                };
                ui_delegate_events.update_delegate_task(&id, &label);
            }
            SubagentTaskEvent::Finished { record } => {
                ui_delegate_events.finish_delegate_task(&record.id);
            }
        }
    }));
    let trackers_finish = delegate_trackers.clone();
    let ui_delegate = ui.clone();
    let store_delegate = store.clone();
    let session_id_delegate = session_id.clone();
    delegate_tool.set_notifier(Arc::new(move |record| {
        // Safety net: the live row is normally removed by the Finished event. If that event was
        // missed for any reason, the completion callback still clears it, so a finished child
        // never leaves its last action sitting above the composer.
        ui_delegate.finish_delegate_task(&record.id);
        let is_ok = matches!(
            record.status,
            harness_core::tools::delegate::SubagentStatus::Completed { .. }
        );
        let summary = match &record.status {
            harness_core::tools::delegate::SubagentStatus::Completed { reply, .. } => {
                reply.lines().next().unwrap_or("completed").to_string()
            }
            harness_core::tools::delegate::SubagentStatus::Failed { error, .. } => error.clone(),
            _ => "finished".to_string(),
        };
        let (started_at, _last_tool) = trackers_finish
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&record.id)
            .unwrap_or_else(|| (std::time::Instant::now(), "done".to_string()));
        let total_time = format_delegate_duration(started_at.elapsed());
        let title = format_delegate_title(&record.goal);
        let glyph = if is_ok {
            "✔".green().bold().to_string()
        } else {
            "✖".red().bold().to_string()
        };
        let status = if is_ok { "finished" } else { "failed" };
        let notice = format_delegate_notification(&glyph, &title, status, &total_time);
        ui_delegate.emit_above(&notice);
        if !summary.is_empty() && summary != "completed" {
            ui_delegate.emit_above(&format!("   {}", summary.dimmed()));
        }
        let s_id = session_id_delegate.clone();
        let s_store = store_delegate.clone();
        tokio::spawn(async move {
            let _ = s_store
                .append(&s_id, &harness_core::Message::system(&notice), false)
                .await;
        });
    }));

    {
        let ag = agent.lock().await;
        if let Err(error) = ag
            .emit_hook_event(harness_core::HookEvent::SessionStart, None)
            .await
        {
            tracing::warn!("session_start hook refused the run: {}", error);
        }
    }

    let mut compact_provider: Option<String> = record
        .compact_model
        .as_ref()
        .map(|(p, _)| p.clone())
        .or_else(|| {
            config
                .agent
                .compaction_provider
                .clone()
                .filter(|p| !p.is_empty())
        });
    let mut compact_model: Option<String> = record
        .compact_model
        .as_ref()
        .map(|(_, m)| m.clone())
        .or_else(|| {
            config
                .agent
                .compaction_model
                .clone()
                .filter(|m| !m.is_empty())
        });

    if !is_attended {
        let cancel_token = CancellationToken::new();
        let mut turns: Vec<TurnRecord> = Vec::new();
        let mut process_expanded = false;
        loop {
            print!("{}", PROMPT.bold().yellow());
            io::stdout()
                .flush()
                .map_err(|e| HarnessError::Io(e.to_string()))?;

            let mut input = String::new();
            let read = io::stdin()
                .read_line(&mut input)
                .map_err(|e| HarnessError::Io(e.to_string()))?;
            if read == 0 {
                break;
            }
            let query = input.trim();
            if query.eq_ignore_ascii_case("exit") || query.eq_ignore_ascii_case("quit") {
                break;
            }
            if query.is_empty() {
                continue;
            }
            if query.eq_ignore_ascii_case("/model")
                || query.eq_ignore_ascii_case("/models")
                || query.eq_ignore_ascii_case("/provider")
            {
                handle_model_switch(
                    None,
                    &ui,
                    &config,
                    &cred_manager,
                    &discovery,
                    &agent,
                    store.as_ref(),
                    &session_id,
                    &mut current_provider,
                    &mut current_model,
                    &compact_model,
                )
                .await?;
                continue;
            }
            if query.starts_with("/compact-model") {
                let arg = query.strip_prefix("/compact-model").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_compact_model_switch(
                    None,
                    &ui,
                    &config,
                    &cred_manager,
                    &discovery,
                    &agent,
                    store.as_ref(),
                    &session_id,
                    &current_provider,
                    &current_model,
                    &mut compact_provider,
                    &mut compact_model,
                    arg_opt,
                )
                .await?;
                continue;
            }
            if query == "/sessions"
                || query == "/session"
                || query.starts_with("/sessions ")
                || query.starts_with("/session ")
            {
                let arg = query
                    .split_once(' ')
                    .map(|(_, a)| a.trim())
                    .filter(|a| !a.is_empty());
                if let Err(e) = handle_sessions_switch(
                    None,
                    &ui,
                    &config,
                    &cred_manager,
                    &discovery,
                    &agent,
                    &store,
                    &mut session_id,
                    &mut current_provider,
                    &mut current_model,
                    &mut compact_provider,
                    &mut compact_model,
                    &mut turns,
                    &mut process_expanded,
                    &todo_scope,
                    arg,
                )
                .await
                {
                    eprintln!("{} Failed to switch session: {}", "✖".red().bold(), e);
                }
                continue;
            }
            if query == "/" || query.eq_ignore_ascii_case("/help") {
                for cmd in command_entries(&config, skills) {
                    println!("  {:<12} {}", cmd.name.cyan(), cmd.details.dimmed());
                }
                continue;
            }
            if query == "/skills" || query.starts_with("/skills ") {
                if let Some(ref readout) = skills_readout {
                    println!("\n{}\n{}\n", "Available skills:".bold(), readout);
                } else {
                    println!("\n{}\n", "No specialized skills installed.".dimmed());
                }
                continue;
            }
            if query == "/mcp" || query.starts_with("/mcp ") {
                let sub = query.strip_prefix("/mcp").unwrap_or("").trim();
                if sub == "list" {
                    println!("{}", crate::commands::mcp::format_mcp_servers(&config));
                } else if sub == "tools" {
                    println!("{}", crate::commands::mcp::format_mcp_tools(&config).await);
                } else {
                    println!("{}", crate::commands::mcp::format_mcp_servers(&config));
                    if config.mcp.enabled && config.mcp.servers.iter().any(|s| s.enabled) {
                        println!(
                            "\n{}",
                            crate::commands::mcp::format_mcp_tools(&config).await
                        );
                    }
                }
                continue;
            }
            if query == "/plan" || query.starts_with("/plan ") {
                let current = plan_mode.load(std::sync::atomic::Ordering::SeqCst);
                let next = !current;
                plan_mode.store(next, std::sync::atomic::Ordering::SeqCst);
                ui.set_plan_mode(next);
                ui.emit_above(&format!(
                    "{} Plan mode {}",
                    "•".cyan(),
                    if next {
                        "ON (read-only planning, mutating tools gated)"
                            .cyan()
                            .bold()
                    } else {
                        "OFF".dimmed()
                    }
                ));
                continue;
            }
            if query == "/fork" || query.starts_with("/fork ") {
                let title_opt = query
                    .strip_prefix("/fork")
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                handle_fork_command(
                    &ui,
                    &store,
                    &mut session_id,
                    &todo_scope,
                    &config,
                    title_opt,
                )
                .await?;
                continue;
            }
            if query == "/jobs"
                || query.starts_with("/jobs ")
                || query == "/tasks"
                || query.starts_with("/tasks ")
                || query == "/subagents"
                || query.starts_with("/subagents ")
            {
                let tasks =
                    viewer::collect_tasks(Some(bg_queue.as_ref()), Some(delegate_tool.as_ref()))
                        .await;
                if tasks.is_empty() {
                    println!(
                        "\n{}\n",
                        "No background tasks or subagents active or recorded.".dimmed()
                    );
                } else {
                    println!("\n{}", "Background tasks and subagents:".bold());
                    for t in tasks {
                        println!(
                            "  {} [{}] ({}s) {}",
                            t.id.cyan().bold(),
                            t.status,
                            t.elapsed_secs,
                            t.command
                        );
                    }
                    println!();
                }
                continue;
            }
            if query.starts_with("/heartbeat") || query.starts_with("/hb") {
                let stripped = if let Some(s) = query.strip_prefix("/heartbeat") {
                    s.trim()
                } else {
                    query.strip_prefix("/hb").unwrap().trim()
                };
                let arg_opt = if stripped.is_empty() {
                    None
                } else {
                    Some(stripped)
                };
                crate::commands::heartbeat::handle_chat_heartbeat(
                    arg_opt,
                    &ui,
                    &config,
                    &agent,
                    &session_id,
                    &current_provider,
                    &current_model,
                    &[],
                )
                .await;
                continue;
            }
            if query == "/reasoning" || query.starts_with("/reasoning ") {
                let arg = query.strip_prefix("/reasoning").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_reasoning_switch(None, &ui, &agent, arg_opt).await?;
                continue;
            }
            if query == "/thinking"
                || query.starts_with("/thinking ")
                || query == "/think"
                || query.starts_with("/think ")
            {
                let stripped = if let Some(s) = query.strip_prefix("/thinking") {
                    s.trim()
                } else {
                    query.strip_prefix("/think").unwrap().trim()
                };
                let arg_opt = if stripped.is_empty() {
                    None
                } else {
                    Some(stripped)
                };
                handle_thinking_switch(None, &ui, &agent, arg_opt).await?;
                continue;
            }
            if query.starts_with("/name") {
                let arg = query.strip_prefix("/name").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_name_command(&ui, store.as_ref(), &session_id, arg_opt).await?;
                continue;
            }
            if query.starts_with("/summarize") {
                let arg = query.strip_prefix("/summarize").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                // The summarized conversation carries its work into a new session, so
                // the list comes with it.
                handle_summarize_and_new_command(
                    &ui,
                    &store,
                    &config,
                    &agent,
                    &mut session_id,
                    &current_provider,
                    &current_model,
                    &todo_scope,
                    arg_opt,
                )
                .await?;
                continue;
            }
            if query.starts_with("/diff") {
                let arg = query.strip_prefix("/diff").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_diff_command(&ui, &agent, &session_id, arg_opt).await?;
                ui.draw_initial_box();
                continue;
            }
            if query.starts_with("/rewind") {
                let arg = query.strip_prefix("/rewind").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_rewind_command(&ui, &agent, &session_id, arg_opt).await?;
                continue;
            }
            if query.starts_with("/auto-compact") {
                let arg = query.strip_prefix("/auto-compact").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                // Piped stdin has no composer, so there is no key channel to hand the setup box:
                // this path prints the settings and the two arguments it can act on.
                handle_auto_compact_command(&ui, &agent, arg_opt, None).await?;
                continue;
            }
            if query.starts_with("/compact") {
                handle_compact_command(&ui, &config, &agent, &current_provider, &current_model)
                    .await?;
                continue;
            }
            if query.starts_with("/new") {
                let arg = query.strip_prefix("/new").unwrap().trim();
                let (is_summarize, remaining_arg) = if arg.starts_with("-s ") || arg == "-s" {
                    (true, arg.strip_prefix("-s").unwrap().trim())
                } else if arg.starts_with("--summarize ") || arg == "--summarize" {
                    (true, arg.strip_prefix("--summarize").unwrap().trim())
                } else if arg.starts_with("--summary ") || arg == "--summary" {
                    (true, arg.strip_prefix("--summary").unwrap().trim())
                } else {
                    (false, arg)
                };
                let arg_opt = if remaining_arg.is_empty() {
                    None
                } else {
                    Some(remaining_arg)
                };
                if is_summarize {
                    handle_summarize_and_new_command(
                        &ui,
                        &store,
                        &config,
                        &agent,
                        &mut session_id,
                        &current_provider,
                        &current_model,
                        &todo_scope,
                        arg_opt,
                    )
                    .await?;
                } else {
                    handle_new_command(
                        &ui,
                        &store,
                        &config,
                        &agent,
                        &mut session_id,
                        &current_provider,
                        &current_model,
                        &todo_scope,
                        arg_opt,
                    )
                    .await?;
                    // The new conversation starts with an empty list.
                    todo_scope.set_session(&session_id);
                    todo::refresh_panel(&ui, &todo_scope);
                }
                continue;
            }
            if query == "/delete" || query.starts_with("/delete ") {
                handle_delete_command(
                    &ui,
                    &store,
                    &config,
                    &agent,
                    &mut session_id,
                    &current_provider,
                    &current_model,
                )
                .await?;
                continue;
            }
            if query == "/usage" || query.starts_with("/usage ") {
                handle_usage_command(
                    &ui,
                    &config,
                    &agent,
                    &session_id,
                    &current_provider,
                    &current_model,
                )
                .await?;
                continue;
            }
            // A skill invocation is a prompt, not a command: expand it here and let the normal
            // turn path below run it, exactly as if the user had typed the instructions.
            let skill_prompt = crate::commands::skills::invocation_prompt(query, skills);
            let query = skill_prompt.as_deref().unwrap_or(query);
            if query.starts_with('/') {
                eprintln!(
                    "{} Unrecognized command '{}'. Type /help for available commands.",
                    "✖".red().bold(),
                    query
                );
                continue;
            }
            let ag_ref = agent.clone();
            let q = query.to_string();
            let cancel_child = cancel_token.child_token();
            let res = {
                let ag = ag_ref.lock().await;
                ag.run_turn(&q, cancel_child).await
            };
            match res {
                Ok(outcome) => {
                    usage
                        .record_chat(
                            &current_provider,
                            &current_model,
                            Some(&session_id),
                            &outcome,
                        )
                        .await;
                    println!("\n{}\n{}\n", LUNA_LABEL.bold().green(), outcome.reply);
                }
                Err(e) => {
                    eprintln!("\n{} Error: {}\n", "✖".red().bold(), e);
                }
            }
        }
        return Ok(());
    }

    // Start from a clean screen: launching the REPL shouldn't leave whatever the shell was
    // showing above the input box. Attended-only by construction — the non-attended path
    // returned above and `clear_screen` no-ops on a piped stdout anyway.
    crate::commands::clear_screen();

    // Print the stored transcript before the box so a resumed session reopens with its history
    // on screen — without it a continuation is indistinguishable from a fresh session until the
    // first reply lands.
    let replayed = store
        .replay_transcript(&session_id, config.session.resume_max_messages)
        .await?;
    let mut turns: Vec<TurnRecord> = parse_turns_from_history(&replayed.history);
    let mut process_expanded = false;

    if replayed.replayed > 0 {
        if replayed.truncated() {
            println!(
                "{}\n",
                format!(
                    "… {} earlier messages not shown ({} shown)",
                    replayed.total - replayed.replayed,
                    replayed.replayed
                )
                .dimmed()
            );
        }
        redraw_transcript(&turns, process_expanded);
        println!();
    }

    // Warm the model catalog in the background before the first status line is drawn: the windows
    // the provider publishes live in it, and the gauge reads them from there on every turn. It is
    // detached on purpose — a slow or dead `/models` endpoint must never hold up a session, and
    // each answer it does give replaces the catalog's own guess with the provider's measurement.
    {
        let discovery = discovery.clone();
        let cred_manager = cred_manager.clone();
        let provider = current_provider.clone();
        tokio::spawn(async move {
            if let Err(error) = discovery.get_models(&provider, &cred_manager, false).await {
                tracing::debug!(
                    "model catalog warm-up for '{}' did not run: {}",
                    provider,
                    error
                );
            }
        });
    }

    let context_max = context_window(&config, &discovery, &current_provider, &current_model);
    // Seed the bar from the real transcript, not 0: a resumed session already carries history, and
    // reporting "0 used" made the continuation look like a fresh start.
    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.set_status(&current_model, context_used, context_max);
    refresh_antigravity_status_if_needed(&ui, &current_provider, &current_model, &cred_manager)
        .await;
    // A resumed named session shows its name centered in the status bar from the first frame.
    let stored_name = store
        .get(&session_id)
        .await
        .map(|s| s.name)
        .unwrap_or_default();
    ui.set_session_name(&stored_name);

    // Images pasted via Ctrl+V, waiting to ride the next prompt as data URLs.
    let pending_images: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    // A resumed session carries its list: the panel is drawn from the start, above
    // the composer, so the tasks are in front of the user before the first prompt.
    todo::refresh_panel(&ui, &todo_scope);

    ui.draw_initial_box();

    #[cfg(unix)]
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok();

    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<AppKey>();

    // Raw mode belongs to the session, not to each read — see `RawTerminal`. Held here so the
    // guard drops (and the terminal is restored) when this function returns on any exit path.
    let _raw_terminal = RawTerminal::acquire();

    // Ask the terminal for unambiguous key codes. Without it ctrl+shift+o arrives as the
    // same byte as ctrl+o, and the todo panel could never have a key of its own.
    //
    // We enable the protocol when either the console crate thinks we're attended,
    // or when KITTY_WINDOW_ID is set (a reliable indicator of running in kitty).
    // The latter handles cases where the tool is invoked with redirected stdio but
    // the controlling terminal is still kitty.
    let in_kitty = std::env::var("KITTY_WINDOW_ID").is_ok();
    let _kitty_guard = if Term::stdout().features().is_attended() || in_kitty {
        keys::enable_kitty_keyboard();
        Some(KittyKeyboardGuard)
    } else {
        None
    };

    // Ctrl-C stays a signal because raw mode is entered with ISIG left on. Left to its default
    // action it would kill the process before the quit path runs and the resume hint never
    // prints; funnelling the signal into the key channel gives it the same path as `exit`.
    {
        let key_tx = key_tx.clone();
        tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                if key_tx.send(AppKey::CtrlC).is_err() {
                    break;
                }
            }
        });
    }

    std::thread::spawn(move || {
        loop {
            match keys::read_key(libc::STDIN_FILENO) {
                Ok(key) => {
                    if key_tx.send(key).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    // SIGINT or other signal interrupted read syscall - do not terminate reader thread!
                    continue;
                }
                Err(_) => {
                    if key_tx.is_closed() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    });

    let mut queue: VecDeque<String> = VecDeque::new();
    let mut current_cancel = CancellationToken::new();
    let mut active_turn: Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>> = None;
    let mut current_query = String::new();
    let mut turn_start = std::time::Instant::now();

    let history_path = config
        .paths
        .sessions_db
        .parent()
        .map(|p| p.join("history.txt"));
    let mut prompt_history = load_prompt_history(history_path.as_deref(), &turns);
    let mut history_cursor: Option<usize> = None;
    let mut draft_input = String::new();

    // Drives the live region: streamed reasoning and the running-tool spinner advance on this
    // tick rather than on every delta, so a fast token stream cannot outrun the redraw.
    let mut animation = tokio::time::interval(ANIMATION_INTERVAL);
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // A quota readout fetched at start-up and never fetched again is a snapshot: its countdown
    // stops moving the moment it is drawn, and its percentages describe the account as it was when
    // the session opened. The tick is what keeps it honest — the clock half is repainted whenever a
    // minute of the countdown passes, and the numbers themselves are refetched on this TTL.
    let mut quota_refreshed_at = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = async {
                #[cfg(unix)]
                {
                    match sigwinch.as_mut() {
                        Some(sig) => { sig.recv().await; }
                        None => std::future::pending().await,
                    }
                }
                #[cfg(not(unix))]
                {
                    std::future::pending::<()>().await
                }
            } => {
                // A resize changes how every soft-wrapped line lays out. The live region and
                // input box are redrawn by handle_resize, but the transcript above them was
                // printed at the old width and stays frozen there. Clear and re-render the
                // whole screen so old messages reflow to the new width.
                crate::commands::clear_screen();
                redraw_transcript(&turns, process_expanded);
                if active_turn.is_some() {
                    if !current_query.trim().is_empty() {
                        println!();
                        println!(
                            "{}",
                            right_aligned_user_message(&current_query, get_terminal_width())
                        );
                    }
                    let tools = in_flight_tools.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    for item in &tools {
                        println!("{}", item.call_formatted);
                        for diff in &item.diff_lines {
                            println!("{}", diff);
                        }
                    }
                }
                ui.handle_resize();
            }

            _ = animation.tick() => {
                ui.animate_tick();
                // The countdown is rendered from the bucket's reset time, so an idle composer — one
                // with no live rows to animate — paints nothing and would show a frozen clock.
                ui.refresh_quota_clock();
                // The same tick re-reads the account's quota once its snapshot has aged out. It is
                // spawned rather than awaited: a metered endpoint that is slow or down must never
                // be something the composer stalls behind.
                if quota_refreshed_at.elapsed() >= status::QUOTA_REFRESH_INTERVAL {
                    quota_refreshed_at = std::time::Instant::now();
                    tokio::spawn(refresh_antigravity_quota_owned(
                        ui.clone(),
                        current_provider.clone(),
                        current_model.clone(),
                        cred_manager.clone(),
                    ));
                }
                // The queue's second drain path: a fold the user asked for ends without a turn-end
                // event, so the tick is what notices the agent is free and sends what was held.
                if !compacting.load(std::sync::atomic::Ordering::SeqCst) {
                    drain_queue(
                        &mut queue,
                        &ui,
                        &turns,
                        process_expanded,
                        &pending_images,
                        &plan_mode,
                        &agent,
                        &in_flight_tools,
                        &mut current_cancel,
                        &mut current_query,
                        &mut turn_start,
                        &mut active_turn,
                    );
                }
            }

            maybe_approval = approval_rx.recv() => {
                let (request, mut decision_tx) = match maybe_approval {
                    Some(pair) => pair,
                    None => continue,
                };

                if decision_tx.is_closed() {
                    continue;
                }

                // The popup owns the screen while it is up, so nothing is cleared here and no
                // composer is redrawn on the way out: the verdict below restores it.
                let choice = prompt_approval_choice(&mut key_rx, &request, &mut decision_tx).await;
                if matches!(choice, ApprovalChoice::AllSession) {
                    // Sticky "approve all": flip the gate once and it stays for this chat.
                    allow_all_session.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let is_run = matches!(choice, ApprovalChoice::Once | ApprovalChoice::AllSession);
                let decision = if is_run {
                    harness_core::ApprovalDecision::Run
                } else {
                    harness_core::ApprovalDecision::Deny
                };
                if !is_run {
                    let verdict_str = format_approval_verdict(false, &request);
                    ui.emit_above(&verdict_str);
                    in_flight_tools.lock().unwrap_or_else(|e| e.into_inner()).push(TurnProcessItem {
                        call_formatted: verdict_str,
                        result_formatted: String::new(),
                        diff_lines: Vec::new(),
                    });
                }
                if is_run {
                    let args = request.arguments.clone();
                    ui.start_activity(&format_tool_call(&request.tool, &args), &request.tool, &args);
                }
                let _ = decision_tx.send(decision);
            }

            maybe_sudo = sudo_rx.recv() => {
                let (command, decision_tx) = match maybe_sudo {
                    Some(pair) => pair,
                    None => continue,
                };

                // The dialog owns the screen while it is up (see `prompt_sudo_password_dialog`):
                // nothing to clear here, and it prints its own outcome on the way back.
                let password = prompt_sudo_password_dialog(&mut key_rx, &command).await;
                let _ = decision_tx.send(password);
            }

            maybe_clarify = clarify_rx.recv() => {
                let (request, response_tx) = match maybe_clarify {
                    Some(pair) => pair,
                    None => continue,
                };

                // Nothing is cleared and no box is drawn on the way out: the question is asked on
                // the alternate screen, which leaves the transcript and the composer untouched,
                // and the lines it records on the way back restore the composer themselves. A
                // second `draw_initial_box` here would draw a second box over the live one.
                let response = crate::commands::clarify::prompt_clarify_choice(&mut key_rx, &request, &ui).await;
                let _ = response_tx.send(response);
            }

            maybe_key = key_rx.recv() => {
                let key = match maybe_key {
                    Some(k) => k,
                    None => break,
                };

                match key {
                    AppKey::Ctrl('o') => {
                        // Ctrl+O: toggle process expansion if transcript turns exist.
                        // In standard terminals where Ctrl+Shift+O arrives as 0x0F (Ctrl+O),
                        // fall back to toggling the todo panel when no process turns exist.
                        if active_turn.is_none() && !turns.is_empty() {
                            process_expanded = !process_expanded;
                            if ui.is_attended {
                                crate::commands::clear_screen();
                                redraw_transcript(&turns, process_expanded);
                                if let Some(ref err) = ui.transient_error() {
                                    println!();
                                    println!("{}", err);
                                }
                                ui.draw_initial_box();
                            }
                        } else if ui.has_todo_panel() {
                            ui.toggle_todo_collapsed();
                        }
                        continue;
                    }
                    AppKey::CtrlShiftO | AppKey::Ctrl('t') => {
                        // Ctrl+T or Ctrl+Shift+O: fold or unfold the todo panel.
                        ui.toggle_todo_collapsed();
                        continue;
                    }
                    AppKey::CtrlShiftB | AppKey::Ctrl('b') => {
                        // Ctrl+Shift+B (or Ctrl+B) opens the live background-task output viewer.
                        show_background_task_viewer(
                            &mut key_rx,
                            Some(bg_queue.as_ref()),
                            Some(delegate_tool.as_ref()),
                            &ui,
                            &turns,
                            process_expanded,
                        )
                        .await;
                        continue;
                    }
                    AppKey::Ctrl('v') => {
                        // Ctrl+V: attach a screenshot from the clipboard to the next prompt
                        match crate::commands::clipboard_image::read_clipboard_image() {
                            Ok(Some(img)) => {
                                let data_url = img.data_url();
                                let marker = format!("[image: {}x{}]", img.width, img.height);
                                {
                                    let mut b = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
                                    if !b.is_empty() && !b.ends_with(' ') {
                                        b.push(' ');
                                    }
                                    b.push_str(&marker);
                                    b.push(' ');
                                }
                                let mut attachments = pending_images.lock().unwrap_or_else(|e| e.into_inner());
                                attachments.push(data_url);
                                ui.redraw_input_line();
                            }
                            Ok(None) => {
                                ui.emit_above(&format!(
                                    "{} Clipboard holds no image (nothing to paste)",
                                    "•".dimmed()
                                ));
                            }
                            Err(e) => {
                                ui.emit_above(&format!("{} Screenshot paste failed: {}", "✖".red().bold(), e));
                            }
                        }
                        continue;
                    }
                    AppKey::Tab => {
                        // Tab: toggle plan mode
                        let current = plan_mode.load(std::sync::atomic::Ordering::SeqCst);
                        let next = !current;
                        plan_mode.store(next, std::sync::atomic::Ordering::SeqCst);
                        ui.set_plan_mode(next);
                        ui.emit_above(&format!(
                            "{} Plan mode {}",
                            "•".cyan(),
                            if next {
                                "ON (read-only planning, mutating tools gated)".cyan().bold()
                            } else {
                                "OFF".dimmed()
                            }
                        ));
                        continue;
                    }
                    AppKey::BackTab if !allow_all_session.load(std::sync::atomic::Ordering::SeqCst) => {
                        // Shift+Tab: yolo — mutating tools run without asking for the rest of
                        // the chat. Pressing it again while yolo is on does nothing: the same
                        // key must not silently flip a safety gate back off.
                        allow_all_session.store(true, std::sync::atomic::Ordering::SeqCst);
                        ui.set_yolo(true);
                        continue;
                    }
                    AppKey::Char('/') if ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()).is_empty() => {
                        ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()).push('/');
                        ui.redraw_input_line();
                        if let Some(cmd) =
                            select_slash_command(&mut key_rx, &ui, &command_entries(&config, skills), &pending_images).await
                        {
                            ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()).clear();
                            ui.redraw_input_line();
                            // The menu can name a composer command too — picking `/interrupt`
                            // while Luna is thinking has to interrupt — so it goes through the
                            // same handler the typed form does.
                            if handle_composer_command(
                                &cmd,
                                &mut queue,
                                &steer_handle,
                                &ui,
                                &mut active_turn,
                                &current_cancel,
                                &mut turns,
                                &in_flight_tools,
                                turn_start,
                                &current_query,
                                process_expanded,
                                &agent,
                                context_window(
                                    &config,
                                    &discovery,
                                    &current_provider,
                                    &current_model,
                                ),
                            )
                            .await
                            {
                                continue;
                            }
                            if cmd == "/jobs" || cmd == "/tasks" || cmd == "/subagents" {
                                show_background_task_viewer(
                                    &mut key_rx,
                                    Some(bg_queue.as_ref()),
                                    Some(delegate_tool.as_ref()),
                                    &ui,
                                    &turns,
                                    process_expanded,
                                )
                                .await;
                                continue;
                            }
                            match execute_slash_command(
                                &cmd,
                                &mut key_rx,
                                &ui,
                                &mut config,
                                &cred_manager,
                                &discovery,
                                &agent,
                                &store,
                                &mut session_id,
                                &mut current_provider,
                                &mut current_model,
                                &mut compact_provider,
                                &mut compact_model,
                                &mut turns,
                                &mut process_expanded,
                                &todo_scope,
                                skills_readout.as_deref(),
                                skills,
                                &plan_mode,
                                Some(bg_queue.as_ref()),
                                Some(delegate_tool.as_ref()),
                                Some(&pending_images),
                            )
                            .await?
                            {
                                SlashOutcome::Exit => break,
                                SlashOutcome::Turn { prompt, display } => launch_turn(
                                    &prompt,
                                    Some(&display),
                                    &ui,
                                    &turns,
                                    process_expanded,
                                    &pending_images,
                                    &plan_mode,
                                    &agent,
                                    &in_flight_tools,
                                    &mut current_cancel,
                                    &mut current_query,
                                    &mut turn_start,
                                    &mut active_turn,
                                ),
                                SlashOutcome::Continue => {}
                            }
                        }
                    }
                    AppKey::Up => {
                        if !prompt_history.is_empty() {
                            let next_idx = match history_cursor {
                                None => {
                                    draft_input = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
                                    prompt_history.len().saturating_sub(1)
                                }
                                Some(idx) => idx.saturating_sub(1),
                            };
                            history_cursor = Some(next_idx);
                            if let Some(prompt) = prompt_history.get(next_idx) {
                                *ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = prompt.clone();
                                ui.redraw_input_line();
                            }
                        }
                        continue;
                    }
                    AppKey::Down => {
                        if let Some(idx) = history_cursor {
                            if idx + 1 < prompt_history.len() {
                                let next_idx = idx + 1;
                                history_cursor = Some(next_idx);
                                if let Some(prompt) = prompt_history.get(next_idx) {
                                    *ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = prompt.clone();
                                    ui.redraw_input_line();
                                }
                            } else {
                                history_cursor = None;
                                *ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = draft_input.clone();
                                ui.redraw_input_line();
                            }
                        }
                        continue;
                    }
                    AppKey::Char(c) => {
                        ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()).push(c);
                        ui.redraw_input_line();
                    }
                    AppKey::Backspace => {
                        ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()).pop();
                        ui.redraw_input_line();
                    }
                    AppKey::Enter => {
                        history_cursor = None;
                        draft_input.clear();
                        let query = {
                            let mut b = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
                            let s = b.trim().to_string();
                            b.clear();
                            s
                        };

                        if query.is_empty() {
                            ui.redraw_input_line();
                            continue;
                        }

                        if prompt_history.last().map(|s| s != &query).unwrap_or(true) {
                            append_prompt_history(history_path.as_deref(), &query);
                            prompt_history.push(query.clone());
                        }

                        // A screenshot pasted before the command leaves an `[image: ...]` marker at
                        // the front of the line. It is an attachment, not command text, so command
                        // detection looks past it; the full text is still what gets sent.
                        let command_query =
                            crate::commands::prompt_images::strip_leading_image_markers(&query);

                        if command_query == "/" || command_query.eq_ignore_ascii_case("/help") {
                            if let Some(cmd) =
                                select_slash_command(&mut key_rx, &ui, &command_entries(&config, skills), &pending_images)
                                    .await
                            {
                                ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()).clear();
                                ui.redraw_input_line();
                                if handle_composer_command(
                                    &cmd,
                                    &mut queue,
                                    &steer_handle,
                                    &ui,
                                    &mut active_turn,
                                    &current_cancel,
                                    &mut turns,
                                    &in_flight_tools,
                                    turn_start,
                                    &current_query,
                                    process_expanded,
                                    &agent,
                                    context_window(
                                        &config,
                                        &discovery,
                                        &current_provider,
                                        &current_model,
                                    ),
                                )
                                .await
                                {
                                    continue;
                                }
                                if cmd == "/jobs" || cmd == "/tasks" || cmd == "/subagents" {
                                    show_background_task_viewer(
                                        &mut key_rx,
                                        Some(bg_queue.as_ref()),
                                        Some(delegate_tool.as_ref()),
                                        &ui,
                                        &turns,
                                        process_expanded,
                                    )
                                    .await;
                                    continue;
                                }
                                match execute_slash_command(
                                    &cmd,
                                    &mut key_rx,
                                    &ui,
                                    &mut config,
                                    &cred_manager,
                                    &discovery,
                                    &agent,
                                    &store,
                                    &mut session_id,
                                    &mut current_provider,
                                    &mut current_model,
                                    &mut compact_provider,
                                    &mut compact_model,
                                    &mut turns,
                                    &mut process_expanded,
                                    &todo_scope,
                                    skills_readout.as_deref(),
                                    skills,
                                    &plan_mode,
                                    Some(bg_queue.as_ref()),
                                    Some(delegate_tool.as_ref()),
                                    Some(&pending_images),
                                )
                                .await?
                                {
                                    SlashOutcome::Exit => break,
                                    SlashOutcome::Turn { prompt, display } => launch_turn(
                                        &prompt,
                                        Some(&display),
                                        &ui,
                                        &turns,
                                        process_expanded,
                                        &pending_images,
                                        &plan_mode,
                                        &agent,
                                        &in_flight_tools,
                                        &mut current_cancel,
                                        &mut current_query,
                                        &mut turn_start,
                                        &mut active_turn,
                                    ),
                                    SlashOutcome::Continue => {}
                                }
                            }
                            continue;
                        }

                        // The three commands that act on this loop's own machinery — the pending
                        // queue, the steer handle, the turn in flight — are handled here, before
                        // the command menu sees them: the menu carries the bare name only, and
                        // these are only useful with their argument.
                        if handle_composer_command(
                            command_query,
                            &mut queue,
                            &steer_handle,
                            &ui,
                            &mut active_turn,
                            &current_cancel,
                            &mut turns,
                            &in_flight_tools,
                            turn_start,
                            &current_query,
                            process_expanded,
                            &agent,
                            context_window(&config, &discovery, &current_provider, &current_model),
                        )
                        .await
                        {
                            continue;
                        }

                        if command_query == "/jobs"
                            || command_query == "/tasks"
                            || command_query == "/subagents"
                        {
                            show_background_task_viewer(
                                &mut key_rx,
                                Some(bg_queue.as_ref()),
                                Some(delegate_tool.as_ref()),
                                &ui,
                                &turns,
                                process_expanded,
                            )
                            .await;
                            continue;
                        }

                        if command_query.starts_with('/')
                            || command_query.eq_ignore_ascii_case("exit")
                            || command_query.eq_ignore_ascii_case("quit")
                        {
                            match execute_slash_command(
                                command_query,
                                &mut key_rx,
                                &ui,
                                &mut config,
                                &cred_manager,
                                &discovery,
                                &agent,
                                &store,
                                &mut session_id,
                                &mut current_provider,
                                &mut current_model,
                                &mut compact_provider,
                                &mut compact_model,
                                &mut turns,
                                &mut process_expanded,
                                &todo_scope,
                                skills_readout.as_deref(),
                                skills,
                                &plan_mode,
                                Some(bg_queue.as_ref()),
                                Some(delegate_tool.as_ref()),
                                Some(&pending_images),
                            )
                            .await?
                            {
                                SlashOutcome::Exit => break,
                                SlashOutcome::Turn { prompt, display } => launch_turn(
                                    &prompt,
                                    Some(&display),
                                    &ui,
                                    &turns,
                                    process_expanded,
                                    &pending_images,
                                    &plan_mode,
                                    &agent,
                                    &in_flight_tools,
                                    &mut current_cancel,
                                    &mut current_query,
                                    &mut turn_start,
                                    &mut active_turn,
                                ),
                                SlashOutcome::Continue => {}
                            }
                            continue;
                        }

                        ui.clear_transient_error();

                        // One decision for the whole input path: replace the turn, wait out the
                        // fold, or just send. Anything queued is pumped by the animation tick once
                        // the agent is free again.
                        match submission(
                            &query,
                            active_turn.is_some(),
                            compacting.load(std::sync::atomic::Ordering::SeqCst),
                        ) {
                            Submission::Queue => {
                                queue.push_back(query.clone());
                                ui.set_queue_depth(queue.len());
                                ui.emit_above(&format!(
                                    "{} Queued — sent when the agent is free",
                                    "⏸".yellow().bold()
                                ));
                                continue;
                            }
                            Submission::Steer => {
                                steer_handle.lock().unwrap_or_else(|e| e.into_inner()).push_back(query.clone());
                                let line = format!(
                                    "{} Steered into active turn: {}",
                                    "⏩".cyan().bold(),
                                    query
                                );
                                in_flight_tools.lock().unwrap_or_else(|e| e.into_inner()).push(TurnProcessItem {
                                    call_formatted: line.clone(),
                                    result_formatted: String::new(),
                                    diff_lines: Vec::new(),
                                });
                                ui.emit_above(&line);
                                continue;
                            }
                            Submission::Interrupt => {
                                cancel_running_turn(
                                    &mut active_turn,
                                    &current_cancel,
                                    &ui,
                                    &mut turns,
                                    &in_flight_tools,
                                    turn_start,
                                    &current_query,
                                    process_expanded,
                                    &agent,
                                    context_window(
                                        &config,
                                        &discovery,
                                        &current_provider,
                                        &current_model,
                                    ),
                                )
                                .await;
                            }
                            Submission::Turn => {}
                        }

                        launch_turn(
                            &query,
                            None,
                            &ui,
                            &turns,
                            process_expanded,
                            &pending_images,
                            &plan_mode,
                            &agent,
                            &in_flight_tools,
                            &mut current_cancel,
                            &mut current_query,
                            &mut turn_start,
                            &mut active_turn,
                        );
                    }
                    // Ctrl+C, and the ctrl+c a terminal may still report as a letter:
                    // a chat you cannot leave is worse than a stray key.
                    AppKey::CtrlC | AppKey::Ctrl('c') => {
                        let cancelled = cancel_running_turn(
                            &mut active_turn,
                            &current_cancel,
                            &ui,
                            &mut turns,
                            &in_flight_tools,
                            turn_start,
                            &current_query,
                            process_expanded,
                            &agent,
                            context_window(
                                &config,
                                &discovery,
                                &current_provider,
                                &current_model,
                            ),
                        )
                        .await;
                        if !cancelled {
                            ui.clear_box_on_exit();
                            clear_on_exit();
                            break;
                        }
                    }
                    AppKey::Escape => {
                        let cancelled = cancel_running_turn(
                            &mut active_turn,
                            &current_cancel,
                            &ui,
                            &mut turns,
                            &in_flight_tools,
                            turn_start,
                            &current_query,
                            process_expanded,
                            &agent,
                            context_window(
                                &config,
                                &discovery,
                                &current_provider,
                                &current_model,
                            ),
                        )
                        .await;
                        if !cancelled {
                            // Nothing was running, so Esc is cancelling the draft. Attachments
                            // belong to the draft; drop them and their markers together.
                            let had_images = {
                                let mut guard = pending_images
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                let had = !guard.is_empty();
                                guard.clear();
                                had
                            };
                            if had_images {
                                let mut b = ui
                                    .input_buf
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                let cleaned =
                                    crate::commands::prompt_images::remove_image_markers(&b);
                                *b = cleaned;
                                drop(b);
                                ui.redraw_input_line();
                            }
                        }
                    }
                    _ => {}
                }
            }

            turn_res = async {
                if let Some(ref mut handle) = active_turn {
                    handle.await
                } else {
                    std::future::pending().await
                }
            } => {
                active_turn = None;
                // Nothing is in flight any more, whatever the turn did: the hint goes back to
                // the idle line before the transcript below is redrawn.
                ui.set_turn_active(false);
                match turn_res {
                    Ok(Ok(outcome)) => {
                        ui.stop_activity();
                        ui.clear_live_state();
                        // Accounting happens before rendering: a UI panic must not lose the row.
                        usage
                            .record_chat(&current_provider, &current_model, Some(&session_id), &outcome)
                            .await;
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap_or_else(|e| e.into_inner()));
                        let turn_dur = turn_start.elapsed();

                        let record = TurnRecord {
                            user_query: current_query.clone(),
                            reasoning: outcome.reasoning.clone(),
                            tools: outcome.tool_calls.clone(),
                            tool_items: current_tools,
                            duration: turn_dur,
                            reply: outcome.reply.clone(),
                        };
                        turns.push(record);

                        if ui.is_attended {
                            todo::refresh_panel(&ui, &todo_scope);
                            crate::commands::clear_screen();
                            redraw_transcript(&turns, process_expanded);
                            ui.draw_initial_box();
                        } else {
                            if !outcome.reply.is_empty() {
                                println!("{}\n{}", LUNA_LABEL.bold().green(), outcome.reply);
                            }
                        }

                        // Update status bar with current context usage
                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max =
                            context_window(&config, &discovery, &current_provider, &current_model);
                        ui.update_status(context_used, context_max);
                    }
                    Ok(Err(HarnessError::Cancelled)) => {
                        ui.stop_activity();
                        ui.clear_live_state();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap_or_else(|e| e.into_inner()));
                        let turn_dur = turn_start.elapsed();

                        let record = TurnRecord {
                            user_query: current_query.clone(),
                            reasoning: None,
                            tools: Vec::new(),
                            tool_items: current_tools,
                            duration: turn_dur,
                            reply: "✖ Cancelled".to_string(),
                        };
                        turns.push(record);

                        if ui.is_attended {
                            todo::refresh_panel(&ui, &todo_scope);
                            crate::commands::clear_screen();
                            redraw_transcript(&turns, process_expanded);
                            ui.draw_initial_box();
                        } else {
                            println!("{}", "✖ Cancelled".red());
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max =
                            context_window(&config, &discovery, &current_provider, &current_model);
                        ui.update_status(context_used, context_max);
                    }
                    Ok(Err(e)) => {
                        ui.stop_activity();
                        ui.clear_live_state();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap_or_else(|e| e.into_inner()));
                        let turn_dur = turn_start.elapsed();
                        let had_tools = !current_tools.is_empty();

                        if had_tools {
                            let record = TurnRecord {
                                user_query: current_query.clone(),
                                reasoning: None,
                                tools: Vec::new(),
                                tool_items: current_tools,
                                duration: turn_dur,
                                reply: format!("✖ Error: {}", e),
                            };
                            turns.push(record);
                            *ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = "continue".to_string();

                            if ui.is_attended {
                                todo::refresh_panel(&ui, &todo_scope);
                                crate::commands::clear_screen();
                                redraw_transcript(&turns, process_expanded);
                                ui.draw_initial_box();
                            } else {
                                println!("{} Error: {}", "✖".red().bold(), e);
                            }
                        } else {
                            if !current_query.is_empty() {
                                *ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = current_query.clone();
                            }

                            let err_msg = format!("{} Error: {}", "✖".red().bold(), e);
                            ui.set_transient_error(Some(err_msg.clone()));

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                redraw_transcript(&turns, process_expanded);
                                println!();
                                println!("{}", err_msg);
                                ui.draw_initial_box();
                            } else {
                                println!("{}", err_msg);
                            }
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max =
                            context_window(&config, &discovery, &current_provider, &current_model);
                        ui.update_status(context_used, context_max);
                    }
                    Err(join_err) => {
                        ui.stop_activity();
                        ui.clear_live_state();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap_or_else(|e| e.into_inner()));
                        let turn_dur = turn_start.elapsed();
                        let had_tools = !current_tools.is_empty();

                        if had_tools {
                            let record = TurnRecord {
                                user_query: current_query.clone(),
                                reasoning: None,
                                tools: Vec::new(),
                                tool_items: current_tools,
                                duration: turn_dur,
                                reply: format!("✖ Task join error: {}", join_err),
                            };
                            turns.push(record);
                            *ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = "continue".to_string();

                            if ui.is_attended {
                                todo::refresh_panel(&ui, &todo_scope);
                                crate::commands::clear_screen();
                                redraw_transcript(&turns, process_expanded);
                                ui.draw_initial_box();
                            } else {
                                println!("{} Task join error: {}", "✖".red().bold(), join_err);
                            }
                        } else {
                            if !current_query.is_empty() {
                                *ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = current_query.clone();
                            }

                            let err_msg = format!("{} Task join error: {}", "✖".red().bold(), join_err);
                            ui.set_transient_error(Some(err_msg.clone()));

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                redraw_transcript(&turns, process_expanded);
                                println!();
                                println!("{}", err_msg);
                                ui.draw_initial_box();
                            } else {
                                println!("{}", err_msg);
                            }
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max =
                            context_window(&config, &discovery, &current_provider, &current_model);
                        ui.update_status(context_used, context_max);
                    }
                }

                // Whatever the turn did — finish, fail, get cancelled — what the user typed while
                // it ran is still waiting, and the agent is free now.
                drain_queue(
                    &mut queue,
                    &ui,
                    &turns,
                    process_expanded,
                    &pending_images,
                    &plan_mode,
                    &agent,
                    &in_flight_tools,
                    &mut current_cancel,
                    &mut current_query,
                    &mut turn_start,
                    &mut active_turn,
                );
            }
        }
    }

    {
        let ag = agent.lock().await;
        if let Err(error) = ag
            .emit_hook_event(harness_core::HookEvent::SessionEnd, None)
            .await
        {
            tracing::warn!("session_end hook failed: {}", error);
        }
    }

    clear_on_exit();
    // The screen is wiped on exit, so leave behind just the line that gets the user back in.
    // Both id and name are shown: the name is what the user recognises, the id is the
    // unambiguous handle, and `--session` accepts either.
    let session_name = store
        .get(&session_id)
        .await
        .map(|s| s.name)
        .unwrap_or_default();
    if session_name.is_empty() {
        println!("Resume: luna --session {}", session_id);
    } else {
        println!(
            "Resume: luna --session {}    (name: {})",
            session_id, session_name
        );
    }
    Ok(())
}
