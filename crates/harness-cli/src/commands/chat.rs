use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use colored::Colorize;
use dialoguer::console::{Key, Term};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_core::{
    Agent, ApprovalRequest, Config, DefaultToolRegistry, HarnessError,
    ModelProvider, SessionBinding, SessionStore,
    ToolRegistry, TurnOutcome,
};
use harness_evolver::{
    apply_update_at_startup, CheckImprovementTool, CodebaseEvolver, DelegateImprovementTool,
    ListImprovementJobsTool,
};
use harness_memory::{
    ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool,
};
use harness_notes::{
    DeleteNoteTool, ListNotesTool, ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine,
    UpdateNoteTool,
};
use harness_providers::{
    get_context_window, CredentialManager, GenericProviderClient,
    ModelDiscovery,
};

use super::approval::{
    approval_settings, build_approver, render_approval_box, ApprovalChoice, ChatApprover,
    APPROVAL_ACTIONS,
};
use super::sessions::{open_store, resolve_session, SessionSelection};
use tokio::sync::oneshot;

pub mod commands;
pub(crate) mod render;
pub mod ui;
pub mod viewer;

use commands::{
    execute_slash_command, handle_compact_command, handle_compact_model_switch,
    handle_delete_command, handle_model_switch, handle_name_command, handle_new_command,
    handle_reasoning_switch, handle_summarize_and_new_command, select_slash_command,
    SLASH_COMMANDS,
};
use render::{
    format_approval_verdict, format_tool_call, format_tool_result, parse_turns_from_history,
    render_all_turns, right_aligned_user_message, OutputBlockKind, TurnProcessItem, TurnRecord,
    LUNA_LABEL, PROMPT,
};
use ui::{get_terminal_width, BackgroundTaskState, ChatUi, RawTerminal, ANIMATION_INTERVAL};
use viewer::show_background_task_viewer;

pub(crate) type ChatAgent = Agent<
    GenericProviderClient,
    SqliteMemoryEngine,
    SqliteNotesEngine,
    CodebaseEvolver,
    DefaultToolRegistry,
>;

/// Wipe the terminal on session exit, so only the shell prompt remains.
/// Cosmetic: a failure to write is ignored on purpose (see `commands::clear_screen`).
pub(crate) fn clear_on_exit() {
    crate::commands::clear_screen();
}

fn load_prompt_history(path: Option<&std::path::Path>, initial_turns: &[TurnRecord]) -> Vec<String> {
    let mut prompts = Vec::new();
    if let Some(p) = path {
        if let Ok(content) = std::fs::read_to_string(p) {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && prompts.last().map(|s| s != trimmed).unwrap_or(true) {
                    prompts.push(trimmed.to_string());
                }
            }
        }
    }
    for turn in initial_turns {
        let q = turn.user_query.trim();
        if !q.is_empty() && !q.starts_with('•') && prompts.last().map(|s| s != q).unwrap_or(true) {
            prompts.push(q.to_string());
        }
    }
    prompts
}

fn append_prompt_history(path: Option<&std::path::Path>, prompt: &str) {
    if let Some(p) = path {
        use std::io::Write;
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(file, "{}", prompt);
        }
    }
}

pub async fn run_interactive_chat(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    selection: SessionSelection,
    explicit_model: bool,
) -> Result<(), HarnessError> {
    // Every turn is appended to this session; a resumed one replays its recent transcript.
    // Resolved before the provider client so the session's stored model choice can take effect.
    let store = Arc::new(open_store(&config)?);
    let mut session_id = resolve_session(store.as_ref(), selection).await?;

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

    // The compaction call is a plain model call: one provider, one model, resolved from
    // `agent.compaction_provider` / `agent.compaction_model` (both default to the session's).
    let compaction_provider_id = config
        .agent
        .compaction_provider
        .clone()
        .unwrap_or_else(|| current_provider.clone());
    let compaction_model = config
        .agent
        .compaction_model
        .clone()
        .unwrap_or_else(|| current_model.clone());
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

    let memory = Arc::new(SqliteMemoryEngine::new(Some(config.paths.memory_db.clone()))?);
    let notes = Arc::new(SqliteNotesEngine::new(Some(config.paths.notes_db.clone()))?);
    let evolver = Arc::new(
        CodebaseEvolver::new(
            config.paths.workspace_root.clone(),
            config.paths.jobs_dir.clone(),
        )
        .with_provider(provider.clone()),
    );

    let registry = Arc::new(DefaultToolRegistry::new());

    // Register native memory tools
    registry.register(Arc::new(SaveMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(UpdateMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(ForgetMemoryTool::new(memory.clone()))).await?;

    // Register native note tools
    registry.register(Arc::new(SaveNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(SearchNotesTool::new(notes.clone()))).await?;
    registry.register(Arc::new(ListNotesTool::new(notes.clone()))).await?;
    registry.register(Arc::new(ReadNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(UpdateNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(DeleteNoteTool::new(notes.clone()))).await?;

    // Register self-improvement evolver tools (delegated background engineer)
    registry.register(Arc::new(DelegateImprovementTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(CheckImprovementTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(ListImprovementJobsTool::new(evolver.clone()))).await?;

    // Load skills from skills_dir and workspace .harness/skills
    let mut all_skills = Vec::new();
    let _ = std::fs::create_dir_all(&config.paths.skills_dir);
    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir) {
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

    if !all_skills.is_empty() {
        let skills_arc = Arc::new(all_skills);
        registry
            .register(Arc::new(harness_plugins::SkillViewTool::new(skills_arc.clone())))
            .await?;
        registry
            .register(Arc::new(harness_plugins::SkillsListTool::new(skills_arc)))
            .await?;
    }

    let bg_queue = Arc::new(harness_core::tasks::BackgroundTaskQueue::new(
        config.paths.jobs_dir.clone(),
    ));
    registry
        .register(Arc::new(harness_core::tools::ProcessTool::new(bg_queue.clone())))
        .await?;

    let delegate_tool = harness_core::tools::DelegateTaskTool::new(
        provider.clone(),
        memory.clone(),
        notes.clone(),
        evolver.clone(),
        Some(store.clone()),
        config.clone(),
    );
    registry.register(Arc::new(delegate_tool)).await?;

    // Register core filesystem and shell tools (§2.2)
    let sudo_cache = Arc::new(std::sync::Mutex::new(std::env::var("SUDO_PASSWORD").ok()));
    let (sudo_tx, mut sudo_rx) =
        mpsc::unbounded_channel::<(String, oneshot::Sender<Option<String>>)>();
    let sudo_prompter = Arc::new(ChatSudoPrompter::new(sudo_tx));

    let read_cache = harness_core::tools::ReadCache::new();
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    harness_core::tools::register_core_tools_with_sudo(
        &reg_dyn,
        &config,
        read_cache,
        Some(sudo_cache),
        Some(sudo_prompter),
    )
    .await?;

    let is_attended = Term::stdout().features().is_attended();
    let ui = Arc::new(ChatUi::new(is_attended));
    let ui_obs = ui.clone();

    let (approval_tx, mut approval_rx) =
        mpsc::unbounded_channel::<(ApprovalRequest, oneshot::Sender<harness_core::ApprovalDecision>)>();

    let allow_all_session = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let plan_mode = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let approver: Arc<dyn harness_core::Approver> =
        if is_attended && config.tools.approval == harness_core::ApprovalMode::Ask {
            Arc::new(ChatApprover::new(
                approval_tx,
                allow_all_session.clone(),
                plan_mode.clone(),
            ))
        } else {
            build_approver(&config)
        };

    let mut agent_config = config.agent.clone();
    agent_config.skills_summary = skills_summary;

    let agent = Agent::new(
        provider,
        memory,
        notes,
        evolver.clone(),
        registry,
        agent_config.clone(),
    )
    .with_approval(approver, approval_settings(&config))
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: session_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>> = Arc::new(Mutex::new(Vec::new()));
    let in_flight_tools_obs = in_flight_tools.clone();

    let mut agent = agent.with_observer(Arc::new(move |event| match event {
        // Reasoning lands in the live region and is settled into the transcript by the animation
        // tick, so it is visible while it is still being produced.
        harness_core::AgentEvent::ReasoningDelta(text) => ui_obs.push_reasoning(&text),
        harness_core::AgentEvent::ContextTokensUpdated { tokens_used } => {
            ui_obs.update_context_used(tokens_used);
        }
        harness_core::AgentEvent::ToolExecuting { name, arguments } => {
            ui_obs.finish_reasoning();
            let call_fmt = format_tool_call(&name, &arguments);
            ui_obs.begin_block(OutputBlockKind::Tool);
            // The call goes into the live region the instant it is invoked, so a slow tool is
            // visible while it runs; `ToolExecuted` below finalizes the same line into the
            // transcript with its verdict.
            ui_obs.start_activity(&call_fmt, &name, &arguments);
            in_flight_tools_obs.lock().unwrap().push(TurnProcessItem {
                call_formatted: call_fmt,
                result_formatted: String::new(),
            });
        }
        harness_core::AgentEvent::ToolExecuted { is_error, output, .. } => {
            ui_obs.stop_activity();
            let status = if is_error {
                " (failed)".red().to_string()
            } else {
                " (success)".green().to_string()
            };
            let line = {
                let mut tools = in_flight_tools_obs.lock().unwrap();
                if let Some(last) = tools.last_mut() {
                    last.call_formatted.push_str(&status);
                    last.result_formatted.clear();
                    last.call_formatted.clone()
                } else {
                    format_tool_result(is_error, &output)
                }
            };
            ui_obs.emit_above(&line);
        }
        harness_core::AgentEvent::Steered { text } => {
            ui_obs.finish_reasoning();
            let line = format!("{} Delivered steer into turn: {}", "⏩".cyan().bold(), text);
            in_flight_tools_obs.lock().unwrap().push(TurnProcessItem {
                call_formatted: line.clone(),
                result_formatted: String::new(),
            });
            ui_obs.emit_above(&line);
        }
    }));
    if let Some(setup) = compaction {
        agent = agent.with_compaction(setup);
    }

    let steer_handle = agent.steer_handle();
    let agent = Arc::new(tokio::sync::Mutex::new(agent));

    let mut compact_provider: Option<String> = config.agent.compaction_provider.clone();
    let mut compact_model: Option<String> = config.agent.compaction_model.clone();

    if !is_attended {
        let cancel_token = CancellationToken::new();
        loop {
            print!("{}", PROMPT.bold().yellow());
            io::stdout().flush().map_err(|e| HarnessError::Io(e.to_string()))?;

            let mut input = String::new();
            let read = io::stdin().read_line(&mut input).map_err(|e| HarnessError::Io(e.to_string()))?;
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
                    &current_provider,
                    &current_model,
                    &mut compact_provider,
                    &mut compact_model,
                    arg_opt,
                )
                .await?;
                continue;
            }
            if query == "/" || query.eq_ignore_ascii_case("/help") {
                for cmd in SLASH_COMMANDS {
                    println!("  {:<12} {}", cmd.name.cyan(), cmd.details.dimmed());
                }
                continue;
            }
            if query == "/skills" || query.starts_with("/skills ") {
                if let Some(ref summary) = agent_config.skills_summary {
                    println!("\n{}\n{}\n", "Available skills:".bold(), summary);
                } else {
                    println!("\n{}\n", "No specialized skills installed.".dimmed());
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
                        "ON (read-only planning, mutating tools gated)".cyan().bold()
                    } else {
                        "OFF".dimmed()
                    }
                ));
                continue;
            }
            if query == "/fork" || query.starts_with("/fork ") {
                let title_opt = query.strip_prefix("/fork").map(str::trim).filter(|s| !s.is_empty());
                match store.fork_session(&session_id, title_opt).await {
                    Ok(new_id) => {
                        let old_id = session_id.clone();
                        session_id = new_id.clone();
                        ui.set_session_name(&format!("fork:{}", &new_id[..new_id.len().min(8)]));
                        ui.emit_above(&format!(
                            "{} Forked session {} -> {}",
                            "✔".green().bold(),
                            old_id.dimmed(),
                            session_id.cyan().bold()
                        ));
                    }
                    Err(e) => {
                        ui.emit_above(&format!("{} Fork failed: {}", "✖".red().bold(), e));
                    }
                }
                continue;
            }
            if query == "/jobs" || query.starts_with("/jobs ") {
                let jobs = bg_queue.list();
                if jobs.is_empty() {
                    println!("\n{}\n", "No background tasks.".dimmed());
                } else {
                    println!("\n{}", "Background tasks:".bold());
                    for j in jobs {
                        println!(
                            "  {} {} [{}] ({}s) {}",
                            j.id.cyan().bold(),
                            j.command,
                            j.status,
                            j.elapsed_secs,
                            j.log_path.dimmed()
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
                let arg_opt = if stripped.is_empty() { None } else { Some(stripped) };
                crate::commands::heartbeat::handle_chat_heartbeat(
                    arg_opt,
                    &ui,
                    &agent,
                    &session_id,
                    &current_provider,
                    &current_model,
                    &[],
                )
                .await;
                continue;
            }
            if query.starts_with("/reasoning") {
                let arg = query.strip_prefix("/reasoning").unwrap().trim();
                let arg_opt = if arg.is_empty() { None } else { Some(arg) };
                handle_reasoning_switch(None, &ui, &agent, arg_opt).await?;
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
                handle_summarize_and_new_command(
                    &ui,
                    &store,
                    &config,
                    &agent,
                    &mut session_id,
                    &current_provider,
                    &current_model,
                    arg_opt,
                )
                .await?;
                continue;
            }
            if query.starts_with("/compact") {
                handle_compact_command(&ui, &agent, &current_provider, &current_model).await?;
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
                let arg_opt = if remaining_arg.is_empty() { None } else { Some(remaining_arg) };
                if is_summarize {
                    handle_summarize_and_new_command(
                        &ui,
                        &store,
                        &config,
                        &agent,
                        &mut session_id,
                        &current_provider,
                        &current_model,
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
                        arg_opt,
                    )
                    .await?;
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
            let ag_ref = agent.clone();
            let q = query.to_string();
            let cancel_child = cancel_token.child_token();
            let res = {
                let ag = ag_ref.lock().await;
                ag.run_turn(&q, cancel_child).await
            };
            match res {
                Ok(outcome) => {
                    println!("\n{}\n{}\n", LUNA_LABEL.bold().green(), outcome.reply);
                }
                Err(e) => {
                    eprintln!("\n{} Error: {}\n", "✖".red().bold(), e);
                }
            }
        }
        return Ok(());
    }

    // Check for pending self-improvement update before clearing the screen
    if is_attended {
        if let Some(pending) = evolver.get_pending_update() {
            print!(
                "An improved version of Luna is ready (job {}: {}). Apply now? [Y/n] ",
                pending.job_id, pending.task
            );
            let _ = io::stdout().flush();
            let mut answer = String::new();
            if io::stdin().read_line(&mut answer).is_ok() {
                let trimmed = answer.trim();
                if trimmed.is_empty()
                    || trimmed.eq_ignore_ascii_case("y")
                    || trimmed.eq_ignore_ascii_case("yes")
                {
                    match apply_update_at_startup(
                        evolver.repo_root(),
                        &config.paths.jobs_dir,
                    ) {
                        Ok(Some(msg)) => {
                            println!("{}", msg.green());
                            std::thread::sleep(std::time::Duration::from_millis(800));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("{} Failed to apply update: {}", "✖".red(), e);
                            std::thread::sleep(std::time::Duration::from_millis(1200));
                        }
                    }
                } else {
                    println!("Update deferred.");
                }
            }
        }
    }

    // Start from a clean screen: launching the REPL shouldn't leave whatever the shell was
    // showing above the input box. Attended-only by construction — the non-attended path
    // returned above and `clear_screen` no-ops on a piped stdout anyway.
    crate::commands::clear_screen();

    // Print the stored transcript before the box so a resumed session reopens with its history
    // on screen — without it a continuation is indistinguishable from a fresh session until the
    // first reply lands.
    let replayed = store
        .replay(&session_id, config.session.resume_max_messages)
        .await?;
    let mut turns: Vec<TurnRecord> = parse_turns_from_history(&replayed.history);
    let mut process_expanded = false;

    if replayed.replayed > 0 {
        let width = get_terminal_width();
        if replayed.truncated() {
            println!(
                "{}\n",
                format!(
                    "… {} earlier messages not shown (last {} resumed)",
                    replayed.total - replayed.replayed,
                    replayed.replayed
                )
                .dimmed()
            );
        }
        render_all_turns(&turns, process_expanded, width);
        println!();
    }

    let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
    // Seed the bar from the real transcript, not 0: a resumed session already carries history, and
    // reporting "0 used" made the continuation look like a fresh start.
    let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
    ui.set_status(&current_model, context_used, context_max);
    // A resumed named session shows its name centered in the status bar from the first frame.
    let stored_name = store.get(&session_id).await.map(|s| s.name).unwrap_or_default();
    ui.set_session_name(&stored_name);

    // Images pasted via Ctrl+V, waiting to ride the next prompt as data URLs.
    let pending_images: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut evolver_rx = evolver.subscribe();
    if let Some(active) = evolver.active_job().await {
        let current_tool = active
            .logs
            .iter()
            .rev()
            .find(|l| l.starts_with("• Tool:"))
            .cloned()
            .unwrap_or_else(|| "Running in background".to_string());
        ui.set_background_task(Some(BackgroundTaskState {
            job_id: active.id,
            task: active.task,
            current_tool,
            started_at: std::time::Instant::now(),
            status: "running".to_string(),
        }));
    }

    ui.draw_initial_box();

    #[cfg(unix)]
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok();

    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<Key>();

    // Raw mode belongs to the session, not to each read — see `RawTerminal`. Held here so the
    // guard drops (and the terminal is restored) when this function returns on any exit path.
    let _raw_terminal = RawTerminal::acquire();

    // Ctrl-C stays a signal because raw mode is entered with ISIG left on. Left to its default
    // action it would kill the process before the quit path runs and the resume hint never
    // prints; funnelling the signal into the key channel gives it the same path as `exit`.
    {
        let key_tx = key_tx.clone();
        tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                if key_tx.send(Key::CtrlC).is_err() {
                    break;
                }
            }
        });
    }

    std::thread::spawn(move || {
        let term = Term::stdout();
        loop {
            match term.read_key() {
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

    let history_path = config.paths.sessions_db.parent().map(|p| p.join("history.txt"));
    let mut prompt_history = load_prompt_history(history_path.as_deref(), &turns);
    let mut history_cursor: Option<usize> = None;
    let mut draft_input = String::new();

    // Drives the live region: streamed reasoning and the running-tool spinner advance on this
    // tick rather than on every delta, so a fast token stream cannot outrun the redraw.
    let mut animation = tokio::time::interval(ANIMATION_INTERVAL);
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                ui.handle_resize();
            }

            _ = animation.tick() => {
                ui.animate_tick();
            }

            maybe_evolver = evolver_rx.recv() => {
                if let Ok(event) = maybe_evolver {
                    match event {
                        harness_evolver::EvolverEvent::JobStarted { job_id, task } => {
                            ui.set_background_task(Some(BackgroundTaskState {
                                job_id,
                                task,
                                current_tool: "git worktree setup".to_string(),
                                started_at: std::time::Instant::now(),
                                status: "running".to_string(),
                            }));
                        }
                        harness_evolver::EvolverEvent::JobProgress { current_tool, .. } => {
                            ui.update_background_task_tool(&current_tool);
                        }
                        harness_evolver::EvolverEvent::JobCompleted { job_id, status, summary, .. } => {
                            ui.set_background_task(None);
                            let is_ok = matches!(
                                status,
                                harness_evolver::JobStatus::ScheduledUpdate { .. }
                                    | harness_evolver::JobStatus::ReadyForReenactment { .. }
                                    | harness_evolver::JobStatus::Verified
                            );
                            let notice = format!(
                                "{} Background evolve task [{}] finished: {}",
                                if is_ok { "✔".green().bold() } else { "✖".red().bold() },
                                job_id.cyan(),
                                summary
                            );
                            ui.emit_above(&notice);
                            let _ = store
                                .append(&session_id, &harness_core::Message::system(&notice), false)
                                .await;

                            let steer_notice = format!(
                                "[BACKGROUND EVOLVER NOTIFICATION]\nBackground evolve task [{}] finished with status: {}.\nSummary:\n{}\n[/BACKGROUND EVOLVER NOTIFICATION]",
                                job_id,
                                if is_ok { "success" } else { "failed" },
                                summary
                            );
                            steer_handle.lock().unwrap().push_back(steer_notice);
                        }
                    }
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

                ui.clear_live_and_box();
                let choice = prompt_approval_choice(&mut key_rx, &request, &ui, &mut decision_tx).await;
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
                let verdict_str = format_approval_verdict(is_run, &request);
                ui.emit_above(&verdict_str);
                in_flight_tools.lock().unwrap().push(TurnProcessItem {
                    call_formatted: verdict_str,
                    result_formatted: String::new(),
                });
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

                ui.clear_live_and_box();
                let password = prompt_sudo_password_dialog(&mut key_rx, &command).await;
                let _ = decision_tx.send(password);
                if ui.is_attended {
                    ui.draw_initial_box();
                }
            }

            maybe_key = key_rx.recv() => {
                let key = match maybe_key {
                    Some(k) => k,
                    None => break,
                };

                match key {
                    Key::Char('\x0f') => {
                        // Ctrl+O: toggle process expansion
                        if active_turn.is_none() && !turns.is_empty() {
                            process_expanded = !process_expanded;
                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                if let Some(ref err) = ui.transient_error() {
                                    println!();
                                    println!("{}", err);
                                }
                                ui.draw_initial_box();
                            }
                        }
                        continue;
                    }
                    Key::Char('\x02') => {
                        // Ctrl+B: inspect background tasks
                        show_background_task_viewer(
                            &mut key_rx,
                            evolver.as_ref(),
                            Some(bg_queue.as_ref()),
                            &ui,
                            &turns,
                            process_expanded,
                        )
                        .await;
                        continue;
                    }
                    Key::Char('\x16') => {
                        // Ctrl+V: attach a screenshot from the clipboard to the next prompt
                        match crate::commands::clipboard_image::read_clipboard_image() {
                            Ok(Some(img)) => {
                                let data_url = img.data_url();
                                let marker = format!(
                                    "[image: {}x{} saved to {}]",
                                    img.width,
                                    img.height,
                                    img.saved_path.display()
                                );
                                {
                                    let mut b = ui.input_buf.lock().unwrap();
                                    if !b.is_empty() && !b.ends_with(' ') {
                                        b.push(' ');
                                    }
                                    b.push_str(&marker);
                                    b.push(' ');
                                }
                                let mut attachments = pending_images.lock().unwrap();
                                attachments.push(data_url);
                                ui.redraw_input_line();
                                ui.emit_above(&format!(
                                    "{} Screenshot attached ({}×{}) — press Enter to send",
                                    "📷".cyan().bold(),
                                    img.width,
                                    img.height
                                ));
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
                    Key::Tab => {
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
                    Key::BackTab if !allow_all_session.load(std::sync::atomic::Ordering::SeqCst) => {
                        // Shift+Tab: yolo — mutating tools run without asking for the rest of
                        // the chat. Pressing it again while yolo is on does nothing: the same
                        // key must not silently flip a safety gate back off.
                        allow_all_session.store(true, std::sync::atomic::Ordering::SeqCst);
                        ui.set_yolo(true);
                        continue;
                    }
                    Key::Char('/') if ui.input_buf.lock().unwrap().is_empty() => {
                        ui.input_buf.lock().unwrap().push('/');
                        ui.redraw_input_line();
                        if let Some(cmd) = select_slash_command(&mut key_rx, &ui).await {
                            ui.input_buf.lock().unwrap().clear();
                            ui.redraw_input_line();
                            if execute_slash_command(
                                &cmd,
                                &mut key_rx,
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
                            )
                            .await?
                            {
                                break;
                            }
                        }
                    }
                    Key::ArrowUp => {
                        if !prompt_history.is_empty() {
                            let next_idx = match history_cursor {
                                None => {
                                    draft_input = ui.input_buf.lock().unwrap().clone();
                                    prompt_history.len().saturating_sub(1)
                                }
                                Some(idx) => idx.saturating_sub(1),
                            };
                            history_cursor = Some(next_idx);
                            if let Some(prompt) = prompt_history.get(next_idx) {
                                *ui.input_buf.lock().unwrap() = prompt.clone();
                                ui.redraw_input_line();
                            }
                        }
                        continue;
                    }
                    Key::ArrowDown => {
                        if let Some(idx) = history_cursor {
                            if idx + 1 < prompt_history.len() {
                                let next_idx = idx + 1;
                                history_cursor = Some(next_idx);
                                if let Some(prompt) = prompt_history.get(next_idx) {
                                    *ui.input_buf.lock().unwrap() = prompt.clone();
                                    ui.redraw_input_line();
                                }
                            } else {
                                history_cursor = None;
                                *ui.input_buf.lock().unwrap() = draft_input.clone();
                                ui.redraw_input_line();
                            }
                        }
                        continue;
                    }
                    Key::Char(c) => {
                        ui.input_buf.lock().unwrap().push(c);
                        ui.redraw_input_line();
                    }
                    Key::Backspace => {
                        ui.input_buf.lock().unwrap().pop();
                        ui.redraw_input_line();
                    }
                    Key::Enter => {
                        history_cursor = None;
                        draft_input.clear();
                        let query = {
                            let mut b = ui.input_buf.lock().unwrap();
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

                        if query == "/" || query.eq_ignore_ascii_case("/help") {
                            if let Some(cmd) = select_slash_command(&mut key_rx, &ui).await {
                                ui.input_buf.lock().unwrap().clear();
                                ui.redraw_input_line();
                                if execute_slash_command(
                                    &cmd,
                                    &mut key_rx,
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
                                )
                                .await?
                                {
                                    break;
                                }
                            }
                            continue;
                        }

                        if query.starts_with('/')
                            || query.eq_ignore_ascii_case("exit")
                            || query.eq_ignore_ascii_case("quit")
                        {
                            if execute_slash_command(
                                &query,
                                &mut key_rx,
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
                            )
                            .await?
                            {
                                break;
                            }
                            continue;
                        }

                        ui.clear_transient_error();

                        if active_turn.is_some() {
                            let steer_text = if let Some(stripped) = query.strip_prefix("/steer ") {
                                stripped.trim().to_string()
                            } else {
                                query.clone()
                            };

                            steer_handle.lock().unwrap().push_back(steer_text.clone());

                            let steer_display = format!("{} Steered into active turn: {}", "⏩".cyan().bold(), steer_text);
                            in_flight_tools.lock().unwrap().push(TurnProcessItem {
                                call_formatted: steer_display.clone(),
                                result_formatted: String::new(),
                            });
                            ui.emit_above(&steer_display);
                        } else {
                            // Attached screenshots become OpenAI-style data-URL blocks appended
                            // to the prompt text; the provider layer expands them into
                            // content parts. Each URL is one paste, oldest first.
                            let images = std::mem::take(&mut *pending_images.lock().unwrap());
                            let q = if plan_mode.load(std::sync::atomic::Ordering::SeqCst) {
                                format!(
                                    "[PLAN MODE ACTIVE: Formulate a detailed, step-by-step plan/blueprint. Do not attempt to modify files or execute mutating commands until the plan is approved.]\n\n{}",
                                    query
                                )
                            } else {
                                query.clone()
                            };
                            let q_with_images = if images.is_empty() {
                                q
                            } else {
                                format!("{}\n\n{}", q, images.join("\n\n"))
                            };

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                println!();
                                println!("{}", right_aligned_user_message(&query, get_terminal_width()));
                                ui.draw_initial_box();
                            } else {
                                ui.begin_block(OutputBlockKind::User);
                                ui.emit_above(&right_aligned_user_message(&query, get_terminal_width()));
                            }

                            current_cancel = CancellationToken::new();
                            let cancel_child = current_cancel.child_token();
                            let ag_ref = agent.clone();
                            current_query = query.clone();
                            turn_start = std::time::Instant::now();
                            in_flight_tools.lock().unwrap().clear();
                            active_turn = Some(tokio::spawn(async move {
                                let ag = ag_ref.lock().await;
                                ag.run_turn(&q_with_images, cancel_child).await
                            }));
                        }
                    }
                    Key::CtrlC => {
                        if let Some(handle) = active_turn.take() {
                            current_cancel.cancel();
                            handle.abort();
                            ui.stop_activity();
                            ui.clear_live_state();
                            let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
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
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                ui.draw_initial_box();
                            } else {
                                println!("{}", "✖ Cancelled".red());
                            }

                            let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                            let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                            ui.update_status(context_used, context_max);
                        } else {
                            ui.clear_box_on_exit();
                            clear_on_exit();
                            break;
                        }
                    }
                    Key::Escape => {
                        if let Some(handle) = active_turn.take() {
                            current_cancel.cancel();
                            handle.abort();
                            ui.stop_activity();
                            ui.clear_live_state();
                            let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
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
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                ui.draw_initial_box();
                            } else {
                                println!("{}", "✖ Cancelled".red());
                            }

                            let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                            let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                            ui.update_status(context_used, context_max);
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
                let turn_succeeded = matches!(turn_res, Ok(Ok(_)));
                match turn_res {
                    Ok(Ok(outcome)) => {
                        ui.stop_activity();
                        ui.clear_live_state();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
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
                            crate::commands::clear_screen();
                            render_all_turns(&turns, process_expanded, get_terminal_width());
                            ui.draw_initial_box();
                        } else {
                            if !outcome.reply.is_empty() {
                                println!("{}\n{}", LUNA_LABEL.bold().green(), outcome.reply);
                            }
                        }

                        // Update status bar with current context usage
                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Ok(Err(HarnessError::Cancelled)) => {
                        ui.stop_activity();
                        ui.clear_live_state();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
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
                            crate::commands::clear_screen();
                            render_all_turns(&turns, process_expanded, get_terminal_width());
                            ui.draw_initial_box();
                        } else {
                            println!("{}", "✖ Cancelled".red());
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Ok(Err(e)) => {
                        ui.stop_activity();
                        ui.clear_live_state();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
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
                            *ui.input_buf.lock().unwrap() = "continue".to_string();

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                ui.draw_initial_box();
                            } else {
                                println!("{} Error: {}", "✖".red().bold(), e);
                            }
                        } else {
                            if !current_query.is_empty() {
                                *ui.input_buf.lock().unwrap() = current_query.clone();
                            }

                            let err_msg = format!("{} Error: {}", "✖".red().bold(), e);
                            ui.set_transient_error(Some(err_msg.clone()));

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                println!();
                                println!("{}", err_msg);
                                ui.draw_initial_box();
                            } else {
                                println!("{}", err_msg);
                            }
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                    Err(join_err) => {
                        ui.stop_activity();
                        ui.clear_live_state();
                        let current_tools = std::mem::take(&mut *in_flight_tools.lock().unwrap());
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
                            *ui.input_buf.lock().unwrap() = "continue".to_string();

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                ui.draw_initial_box();
                            } else {
                                println!("{} Task join error: {}", "✖".red().bold(), join_err);
                            }
                        } else {
                            if !current_query.is_empty() {
                                *ui.input_buf.lock().unwrap() = current_query.clone();
                            }

                            let err_msg = format!("{} Task join error: {}", "✖".red().bold(), join_err);
                            ui.set_transient_error(Some(err_msg.clone()));

                            if ui.is_attended {
                                crate::commands::clear_screen();
                                render_all_turns(&turns, process_expanded, get_terminal_width());
                                println!();
                                println!("{}", err_msg);
                                ui.draw_initial_box();
                            } else {
                                println!("{}", err_msg);
                            }
                        }

                        let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
                        let context_max = get_context_window(&current_provider, &current_model).unwrap_or(32_768);
                        ui.update_status(context_used, context_max);
                    }
                }

                if turn_succeeded {
                    if let Some(next_query) = queue.pop_front() {
                        if ui.is_attended {
                            crate::commands::clear_screen();
                            render_all_turns(&turns, process_expanded, get_terminal_width());
                            println!();
                            println!("{}", right_aligned_user_message(&next_query, get_terminal_width()));
                            ui.draw_initial_box();
                        } else {
                            ui.begin_block(OutputBlockKind::User);
                            ui.emit_above(&right_aligned_user_message(&next_query, get_terminal_width()));
                        }
                        current_cancel = CancellationToken::new();
                        let cancel_child = current_cancel.child_token();
                        let ag_ref = agent.clone();
                        current_query = next_query.clone();
                        turn_start = std::time::Instant::now();
                        in_flight_tools.lock().unwrap().clear();
                        active_turn = Some(tokio::spawn(async move {
                            let ag = ag_ref.lock().await;
                            ag.run_turn(&next_query, cancel_child).await
                        }));
                    }
                }
            }
        }
    }

    clear_on_exit();
    // The screen is wiped on exit, so leave behind just the line that gets the user back in.
    // Both id and name are shown: the name is what the user recognises, the id is the
    // unambiguous handle, and `--session` accepts either.
    let session_name = store.get(&session_id).await.map(|s| s.name).unwrap_or_default();
    if session_name.is_empty() {
        println!("Resume: luna --session {}", session_id);
    } else {
        println!("Resume: luna --session {}    (name: {})", session_id, session_name);
    }
    Ok(())
}

async fn prompt_approval_choice(
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    request: &ApprovalRequest,
    ui: &ChatUi,
    decision_tx: &mut oneshot::Sender<harness_core::ApprovalDecision>,
) -> ApprovalChoice {
    let mut width = get_terminal_width().saturating_sub(2);
    let mut selected = 0; // index into APPROVAL_ACTIONS

    // Drain any leftover keys in channel
    while key_rx.try_recv().is_ok() {}

    // The popup renders above the composer: erase box + live region, draw the popup, redraw
    // the composer beneath it once the answer lands.
    ui.clear_box_on_exit();

    let (initial_text, mut line_count) = render_approval_box(request, selected, width);
    print!("{}", initial_text);
    let _ = io::stdout().flush();

    loop {
        let key = tokio::select! {
            _ = decision_tx.closed() => {
                // The agent timed out waiting for approval or the turn was cancelled.
                // Cleanly erase the popup box and return Deny immediately so the UI loop does not hang!
                print!("\r\x1b[{}A", line_count);
                for _ in 0..line_count {
                    print!("\x1b[2K\x1b[1B");
                }
                print!("\r\x1b[{}A", line_count);
                let _ = io::stdout().flush();
                ui.draw_initial_box();
                return ApprovalChoice::Deny;
            }
            maybe_key = key_rx.recv() => {
                match maybe_key {
                    Some(k) => k,
                    None => break,
                }
            }
        };

        let chosen = match key {
            Key::ArrowUp | Key::ArrowLeft | Key::BackTab => {
                selected = (selected + APPROVAL_ACTIONS.len() - 1) % APPROVAL_ACTIONS.len();
                None
            }
            Key::ArrowDown | Key::ArrowRight | Key::Tab => {
                selected = (selected + 1) % APPROVAL_ACTIONS.len();
                None
            }
            Key::Char('y') | Key::Char('Y') | Key::Char('1') => Some(ApprovalChoice::Once),
            Key::Char('a') | Key::Char('A') | Key::Char('2') => Some(ApprovalChoice::AllSession),
            Key::Char('n') | Key::Char('N') | Key::Char('3') | Key::Char('c') | Key::Char('C') | Key::Char('d') | Key::Char('D') | Key::Escape | Key::CtrlC => {
                Some(ApprovalChoice::Deny)
            }
            Key::Enter => match selected {
                0 => Some(ApprovalChoice::Once),
                1 => Some(ApprovalChoice::AllSession),
                _ => Some(ApprovalChoice::Deny),
            },
            Key::Char(' ') => match selected {
                0 => Some(ApprovalChoice::Once),
                1 => Some(ApprovalChoice::AllSession),
                _ => Some(ApprovalChoice::Deny),
            },
            _ => continue,
        };

        if let Some(choice) = chosen {
            // Erase the popup and restore the composer before returning.
            print!("\r\x1b[{}A", line_count);
            for _ in 0..line_count {
                print!("\x1b[2K\x1b[1B");
            }
            print!("\r\x1b[{}A", line_count);
            let _ = io::stdout().flush();
            ui.draw_initial_box();
            return choice;
        }

        // Repaint in place: jump to the first popup row, clear lines, and overwrite cleanly.
        print!("\r\x1b[{}A", line_count);
        for _ in 0..line_count {
            print!("\x1b[2K\x1b[1B");
        }
        print!("\r\x1b[{}A", line_count);
        width = get_terminal_width().saturating_sub(2);
        let (new_text, new_count) = render_approval_box(request, selected, width);
        print!("{}", new_text);
        line_count = new_count;
        let _ = io::stdout().flush();
    }

    ui.draw_initial_box();
    ApprovalChoice::Deny
}

pub struct ChatSudoPrompter {
    request_tx: mpsc::UnboundedSender<(String, oneshot::Sender<Option<String>>)>,
}

impl ChatSudoPrompter {
    pub fn new(
        request_tx: mpsc::UnboundedSender<(String, oneshot::Sender<Option<String>>)>,
    ) -> Self {
        Self { request_tx }
    }
}

#[async_trait::async_trait]
impl harness_core::tools::SudoPrompter for ChatSudoPrompter {
    async fn prompt_sudo_password(&self, command: &str) -> Result<Option<String>, HarnessError> {
        let (tx, rx) = oneshot::channel();
        if self.request_tx.send((command.to_string(), tx)).is_err() {
            return Ok(None);
        }
        match rx.await {
            Ok(pwd) => Ok(pwd),
            Err(_) => Ok(None),
        }
    }
}

async fn prompt_sudo_password_dialog(
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    command: &str,
) -> Option<String> {
    while key_rx.try_recv().is_ok() {}

    println!();
    println!("  {} {}", "🔐".bold(), "Sudo password required for command:".bold());
    println!("     {}", command.cyan());
    println!("     {}", "(Enter password; press Enter to confirm, Esc to skip)".dimmed());
    print!("  Password: ");
    let _ = io::stdout().flush();

    let mut password = String::new();
    loop {
        let key = match key_rx.recv().await {
            Some(k) => k,
            None => break,
        };
        match key {
            Key::Enter => {
                println!();
                if password.is_empty() {
                    println!("  {} Skipped - continuing without sudo\n", "⏭".dimmed());
                    let _ = io::stdout().flush();
                    return None;
                } else {
                    println!("  {} Password received and cached for this session\n", "✔".green().bold());
                    let _ = io::stdout().flush();
                    return Some(password);
                }
            }
            Key::Escape | Key::CtrlC => {
                println!("\n  {} Cancelled - continuing without sudo\n", "⏭".dimmed());
                let _ = io::stdout().flush();
                return None;
            }
            Key::Backspace => {
                if !password.is_empty() {
                    password.pop();
                    print!("\x08 \x08");
                    let _ = io::stdout().flush();
                }
            }
            Key::Char(c) => {
                if c == '\n' || c == '\r' {
                    println!();
                    if password.is_empty() {
                        println!("  {} Skipped - continuing without sudo\n", "⏭".dimmed());
                        let _ = io::stdout().flush();
                        return None;
                    } else {
                        println!("  {} Password received and cached for this session\n", "✔".green().bold());
                        let _ = io::stdout().flush();
                        return Some(password);
                    }
                } else if !c.is_control() {
                    password.push(c);
                    print!("•");
                    let _ = io::stdout().flush();
                }
            }
            _ => {}
        }
    }
    None
}

