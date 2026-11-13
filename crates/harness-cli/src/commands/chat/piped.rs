//! The non-interactive loop: print a prompt, read a line, answer it.
//!
//! Used whenever stdout is not an attended terminal. There is no composer, so the commands that
//! need one (the pickers, the key channel) are answered with plain text here instead.

use std::io::{self, Write};

use colored::Colorize;
use tokio_util::sync::CancellationToken;

use harness_core::HarnessError;

use super::commands::{
    command_entries, handle_auto_compact_command, handle_compact_command,
    handle_compact_model_switch, handle_default_model_switch, handle_delete_command,
    handle_diff_command, handle_feature_toggle, handle_fork_command, handle_model_switch,
    handle_name_command, handle_new_command, handle_reasoning_switch, handle_rewind_command,
    handle_sessions_switch, handle_settings_command, handle_summarize_and_new_command,
    handle_thinking_switch, handle_usage_command, is_bare_feature_command, looks_like_path,
    Feature,
};
use super::render::{reply_for_display, TurnRecord, LUNA_LABEL, PROMPT};
use super::setup::ChatRuntime;
use super::todo;
use super::viewer;

/// Run the piping loop until stdin closes or the user asks to leave.
pub(super) async fn run(rt: &mut ChatRuntime) -> Result<(), HarnessError> {
    let cancel_token = CancellationToken::new();
    let mut turns: Vec<TurnRecord> = Vec::new();
    let mut process_expanded = false;
    while let Some(query) = read_query()? {
        if query.is_empty() {
            continue;
        }
        if handle_model_commands(rt, &query).await? {
            continue;
        }
        if handle_session_commands(rt, &query, &mut turns, &mut process_expanded).await? {
            continue;
        }
        let query = normalize_bare_feature(query);
        if handle_help_and_feature_commands(rt, &query).await? {
            continue;
        }
        if handle_plan_and_fork_commands(rt, &query).await? {
            continue;
        }
        if handle_tasks_command(rt, &query).await {
            continue;
        }
        if handle_heartbeat_command(rt, &query).await {
            continue;
        }
        if handle_reasoning_commands(rt, &query).await? {
            continue;
        }
        if handle_name_and_summarize_commands(rt, &query).await? {
            continue;
        }
        if handle_maintenance_commands(rt, &query, &mut process_expanded).await? {
            continue;
        }
        if handle_session_reset_commands(rt, &query).await? {
            continue;
        }
        if handle_goal_command_piped(rt, &query, &cancel_token).await? {
            continue;
        }
        run_piped_turn(rt, query, &cancel_token).await?;
    }
    Ok(())
}

/// Print the prompt and read one line. `None` means EOF or an explicit exit.
fn read_query() -> Result<Option<String>, HarnessError> {
    print!("{}", PROMPT.bold().yellow());
    io::stdout()
        .flush()
        .map_err(|e| HarnessError::Io(e.to_string()))?;

    let mut input = String::new();
    let read = io::stdin()
        .read_line(&mut input)
        .map_err(|e| HarnessError::Io(e.to_string()))?;
    if read == 0 {
        return Ok(None);
    }
    let query = input.trim();
    if query.eq_ignore_ascii_case("exit") || query.eq_ignore_ascii_case("quit") {
        return Ok(None);
    }
    Ok(Some(query.to_string()))
}

fn normalize_bare_feature(query: String) -> String {
    if is_bare_feature_command(&query) && !query.starts_with('/') {
        format!("/{}", query.trim())
    } else {
        query
    }
}

/// `/model`, `/models`, `/provider`, `/compact-model`, and `/default_model`.
async fn handle_model_commands(rt: &mut ChatRuntime, query: &str) -> Result<bool, HarnessError> {
    if query.eq_ignore_ascii_case("/model")
        || query.eq_ignore_ascii_case("/models")
        || query.eq_ignore_ascii_case("/provider")
    {
        handle_model_switch(
            None,
            rt.ui.as_ref(),
            &rt.config,
            &rt.cred_manager,
            &rt.discovery,
            &rt.agent,
            rt.store.as_ref(),
            &rt.session_id,
            &mut rt.current_provider,
            &mut rt.current_model,
            &rt.compact_model,
        )
        .await?;
        return Ok(true);
    }
    if query.starts_with("/compact-model") {
        let arg = query.strip_prefix("/compact-model").unwrap().trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        handle_compact_model_switch(
            None,
            rt.ui.as_ref(),
            &rt.config,
            &rt.cred_manager,
            &rt.discovery,
            &rt.agent,
            rt.store.as_ref(),
            &rt.session_id,
            &rt.current_provider,
            &rt.current_model,
            &mut rt.compact_provider,
            &mut rt.compact_model,
            arg_opt,
        )
        .await?;
        return Ok(true);
    }
    if query == "/default_model"
        || query.starts_with("/default_model ")
        || query == "/default-model"
        || query.starts_with("/default-model ")
    {
        let arg = query
            .strip_prefix("/default_model")
            .or_else(|| query.strip_prefix("/default-model"))
            .unwrap_or("")
            .trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        handle_default_model_switch(
            None,
            rt.ui.as_ref(),
            &mut rt.config,
            &rt.cred_manager,
            &rt.discovery,
            &rt.agent,
            rt.store.as_ref(),
            &rt.session_id,
            &mut rt.current_provider,
            &mut rt.current_model,
            &rt.compact_model,
            arg_opt,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

/// `/sessions` and `/session`: switch to another stored session.
async fn handle_session_commands(
    rt: &mut ChatRuntime,
    query: &str,
    turns: &mut Vec<TurnRecord>,
    process_expanded: &mut bool,
) -> Result<bool, HarnessError> {
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
            rt.ui.as_ref(),
            &rt.config,
            &rt.cred_manager,
            &rt.discovery,
            &rt.agent,
            &rt.store,
            &mut rt.session_id,
            &mut rt.current_provider,
            &mut rt.current_model,
            &mut rt.compact_provider,
            &mut rt.compact_model,
            turns,
            process_expanded,
            &rt.todo_scope,
            arg,
        )
        .await
        {
            eprintln!("{} Failed to switch session: {}", "✖".red().bold(), e);
        }
        return Ok(true);
    }
    Ok(false)
}

/// `/help`, `/skills`, `/memory`, `/notes`, and `/mcp`.
async fn handle_help_and_feature_commands(
    rt: &mut ChatRuntime,
    query: &str,
) -> Result<bool, HarnessError> {
    let skills: &[harness_plugins::Skill] =
        rt.skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
    if query == "/" || query.eq_ignore_ascii_case("/help") {
        for cmd in command_entries(
            &rt.config,
            skills,
            rt.feature_toggles.enabled(Feature::Skills),
        ) {
            println!("  {:<12} {}", cmd.name.cyan(), cmd.details.dimmed());
        }
        return Ok(true);
    }
    if query == "/skills" || query.starts_with("/skills ") {
        let arg = query.strip_prefix("/skills").unwrap_or("").trim();
        if !arg.is_empty() {
            handle_feature_toggle(
                rt.ui.as_ref(),
                &rt.feature_toggles,
                &rt.agent,
                Feature::Skills,
                Some(arg),
            )
            .await;
        } else if !rt.feature_toggles.enabled(Feature::Skills) {
            println!(
                "\n{}\n",
                "Skills are OFF for this session. Use /skills on to enable them.".dimmed()
            );
        } else if let Some(ref readout) = rt.skills_readout {
            println!("\n{}\n{}\n", "Available skills:".bold(), readout);
        } else {
            println!("\n{}\n", "No specialized skills installed.".dimmed());
        }
        return Ok(true);
    }
    if query == "/memory" || query.starts_with("/memory ") {
        let arg = query.strip_prefix("/memory").unwrap_or("").trim();
        let arg = if arg.is_empty() { None } else { Some(arg) };
        handle_feature_toggle(
            rt.ui.as_ref(),
            &rt.feature_toggles,
            &rt.agent,
            Feature::Memory,
            arg,
        )
        .await;
        return Ok(true);
    }
    if query == "/notes" || query.starts_with("/notes ") {
        let arg = query.strip_prefix("/notes").unwrap_or("").trim();
        let arg = if arg.is_empty() { None } else { Some(arg) };
        handle_feature_toggle(
            rt.ui.as_ref(),
            &rt.feature_toggles,
            &rt.agent,
            Feature::Notes,
            arg,
        )
        .await;
        return Ok(true);
    }
    if query == "/mcp" || query.starts_with("/mcp ") {
        handle_mcp_command(rt, query).await;
        return Ok(true);
    }
    Ok(false)
}

async fn handle_mcp_command(rt: &mut ChatRuntime, query: &str) {
    let sub = query.strip_prefix("/mcp").unwrap_or("").trim();
    if matches!(
        sub,
        "on" | "off" | "enable" | "disable" | "enabled" | "disabled" | "true" | "false"
    ) {
        handle_feature_toggle(
            rt.ui.as_ref(),
            &rt.feature_toggles,
            &rt.agent,
            Feature::Mcp,
            Some(sub),
        )
        .await;
    } else if !rt.feature_toggles.enabled(Feature::Mcp) {
        println!(
            "\n{}\n",
            "MCP is OFF for this session. Use /mcp on to enable it.".dimmed()
        );
    } else if sub == "list" {
        println!("{}", crate::commands::mcp::format_mcp_servers(&rt.config));
    } else if sub == "tools" {
        println!(
            "{}",
            crate::commands::mcp::format_mcp_tools(&rt.config).await
        );
    } else {
        println!("{}", crate::commands::mcp::format_mcp_servers(&rt.config));
        if rt.config.mcp.enabled && rt.config.mcp.servers.iter().any(|s| s.enabled) {
            println!(
                "\n{}",
                crate::commands::mcp::format_mcp_tools(&rt.config).await
            );
        }
    }
}

/// `/plan` and `/fork`.
async fn handle_plan_and_fork_commands(
    rt: &mut ChatRuntime,
    query: &str,
) -> Result<bool, HarnessError> {
    if query == "/plan" || query.starts_with("/plan ") {
        let current = rt.plan_mode.load(std::sync::atomic::Ordering::SeqCst);
        let next = !current;
        rt.plan_mode
            .store(next, std::sync::atomic::Ordering::SeqCst);
        rt.ui.set_plan_mode(next);
        rt.ui.emit_above(&format!(
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
        return Ok(true);
    }
    if query == "/fork" || query.starts_with("/fork ") {
        let title_opt = query
            .strip_prefix("/fork")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        handle_fork_command(
            rt.ui.as_ref(),
            &rt.store,
            &mut rt.session_id,
            &rt.todo_scope,
            &rt.config,
            title_opt,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

/// `/jobs`, `/tasks`, `/subagents`.
async fn handle_tasks_command(rt: &mut ChatRuntime, query: &str) -> bool {
    if !(query == "/jobs"
        || query.starts_with("/jobs ")
        || query == "/tasks"
        || query.starts_with("/tasks ")
        || query == "/subagents"
        || query.starts_with("/subagents "))
    {
        return false;
    }
    let tasks =
        viewer::collect_tasks(Some(rt.bg_queue.as_ref()), Some(rt.delegate_tool.as_ref())).await;
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
    true
}

/// `/heartbeat` and `/hb`.
async fn handle_heartbeat_command(rt: &mut ChatRuntime, query: &str) -> bool {
    if !(query.starts_with("/heartbeat") || query.starts_with("/hb")) {
        return false;
    }
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
        rt.ui.as_ref(),
        &rt.config,
        &rt.agent,
        &rt.session_id,
        &rt.current_provider,
        &rt.current_model,
        &[],
    )
    .await;
    true
}

/// `/reasoning`, `/thinking`, and `/think`.
async fn handle_reasoning_commands(
    rt: &mut ChatRuntime,
    query: &str,
) -> Result<bool, HarnessError> {
    if query == "/reasoning" || query.starts_with("/reasoning ") {
        let arg = query.strip_prefix("/reasoning").unwrap().trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        handle_reasoning_switch(None, rt.ui.as_ref(), &rt.agent, arg_opt).await?;
        return Ok(true);
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
        handle_thinking_switch(None, rt.ui.as_ref(), &rt.agent, arg_opt).await?;
        return Ok(true);
    }
    Ok(false)
}

/// `/name` and `/summarize`.
async fn handle_name_and_summarize_commands(
    rt: &mut ChatRuntime,
    query: &str,
) -> Result<bool, HarnessError> {
    if query.starts_with("/name") {
        let arg = query.strip_prefix("/name").unwrap().trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        handle_name_command(rt.ui.as_ref(), rt.store.as_ref(), &rt.session_id, arg_opt).await?;
        return Ok(true);
    }
    if query.starts_with("/summarize") {
        let arg = query.strip_prefix("/summarize").unwrap().trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        // The summarized conversation carries its work into a new session, so
        // the list comes with it.
        handle_summarize_and_new_command(
            rt.ui.as_ref(),
            &rt.store,
            &rt.config,
            &rt.agent,
            &mut rt.session_id,
            &rt.current_provider,
            &rt.current_model,
            &rt.todo_scope,
            arg_opt,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

/// `/diff`, `/rewind`, `/auto-compact`, `/compact`, and `/settings`.
async fn handle_maintenance_commands(
    rt: &mut ChatRuntime,
    query: &str,
    process_expanded: &mut bool,
) -> Result<bool, HarnessError> {
    if query.starts_with("/diff") {
        let arg = query.strip_prefix("/diff").unwrap().trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        handle_diff_command(rt.ui.as_ref(), &rt.agent, &rt.session_id, arg_opt).await?;
        rt.ui.draw_initial_box();
        return Ok(true);
    }
    if query.starts_with("/rewind") {
        let arg = query.strip_prefix("/rewind").unwrap().trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        handle_rewind_command(rt.ui.as_ref(), &rt.agent, &rt.session_id, arg_opt).await?;
        return Ok(true);
    }
    if query.starts_with("/auto-compact") {
        let arg = query.strip_prefix("/auto-compact").unwrap().trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        // Piped stdin has no composer, so there is no key channel to hand the setup box:
        // this path prints the settings and the two arguments it can act on.
        handle_auto_compact_command(rt.ui.as_ref(), &rt.agent, arg_opt, None).await?;
        return Ok(true);
    }
    if query.starts_with("/compact") {
        handle_compact_command(
            rt.ui.as_ref(),
            &rt.config,
            &rt.agent,
            &rt.current_provider,
            &rt.current_model,
        )
        .await?;
        return Ok(true);
    }
    if query.starts_with("/settings") {
        let arg = query.strip_prefix("/settings").unwrap().trim();
        let arg_opt = if arg.is_empty() { None } else { Some(arg) };
        handle_settings_command(
            None,
            rt.ui.as_ref(),
            &mut rt.config,
            &rt.display_settings,
            process_expanded,
            arg_opt,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

/// `/new`, `/delete`, and `/usage`.
async fn handle_session_reset_commands(
    rt: &mut ChatRuntime,
    query: &str,
) -> Result<bool, HarnessError> {
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
                rt.ui.as_ref(),
                &rt.store,
                &rt.config,
                &rt.agent,
                &mut rt.session_id,
                &rt.current_provider,
                &rt.current_model,
                &rt.todo_scope,
                arg_opt,
            )
            .await?;
        } else {
            handle_new_command(
                rt.ui.as_ref(),
                &rt.store,
                &rt.config,
                &rt.agent,
                &mut rt.session_id,
                &rt.current_provider,
                &rt.current_model,
                &rt.todo_scope,
                arg_opt,
            )
            .await?;
            // The new conversation starts with an empty list.
            rt.todo_scope.set_session(rt.session_id.as_str());
            todo::refresh_panel(rt.ui.as_ref(), &rt.todo_scope);
        }
        return Ok(true);
    }
    if query == "/delete" || query.starts_with("/delete ") {
        handle_delete_command(
            rt.ui.as_ref(),
            &rt.store,
            &rt.config,
            &rt.agent,
            &mut rt.session_id,
            &rt.current_provider,
            &rt.current_model,
        )
        .await?;
        return Ok(true);
    }
    if query == "/usage" || query.starts_with("/usage ") {
        handle_usage_command(
            rt.ui.as_ref(),
            &rt.config,
            &rt.agent,
            &rt.session_id,
            &rt.current_provider,
            &rt.current_model,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

/// `/goal [--max N] <objective>` in non-interactive chat.
async fn handle_goal_command_piped(
    rt: &mut ChatRuntime,
    query: &str,
    cancel_token: &CancellationToken,
) -> Result<bool, HarnessError> {
    let Some(arg) = query
        .strip_prefix("/goal")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    else {
        return Ok(false);
    };
    let raw = arg.trim();
    if raw.is_empty()
        || raw.eq_ignore_ascii_case("status")
        || raw.eq_ignore_ascii_case("stop")
        || raw.eq_ignore_ascii_case("cancel")
        || raw.eq_ignore_ascii_case("off")
    {
        println!("Usage: /goal [--max N] <objective>");
        return Ok(true);
    }
    if rt.plan_mode.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!(
            "{} Disable plan mode (/plan) before starting an autonomous /goal loop.",
            "✖".red().bold()
        );
        return Ok(true);
    }
    let (max_iterations, objective) =
        match harness_core::parse_goal_args(raw, rt.config.goal.max_iterations) {
            Ok(parsed) => parsed,
            Err(error) => {
                eprintln!("{} {}", "✖".red().bold(), error);
                return Ok(true);
            }
        };
    let mut goal = harness_core::GoalRunState::new(objective, max_iterations)?;
    loop {
        println!(
            "{} Goal iteration {}/{}",
            "↻".cyan().bold(),
            goal.iteration,
            goal.max_iterations
        );
        let prompt = goal.prompt_for_current_iteration()?;
        let ag_ref = rt.agent.clone();
        let cancel_child = cancel_token.child_token();
        let outcome = {
            let ag = ag_ref.lock().await;
            ag.run_turn(&prompt, cancel_child).await
        };
        match outcome {
            Ok(outcome) => {
                rt.usage
                    .record_chat(
                        &rt.current_provider,
                        &rt.current_model,
                        Some(rt.session_id.as_str()),
                        &outcome,
                    )
                    .await;
                println!(
                    "\n{}\n{}\n",
                    LUNA_LABEL.bold().green(),
                    reply_for_display(&outcome.reply)
                );
                match harness_core::GoalRunState::evaluate_reply(&outcome.reply) {
                    harness_core::GoalVerdict::Complete(_) => {
                        println!(
                            "{} Goal achieved in {} iteration(s).",
                            "✔".green().bold(),
                            goal.iteration
                        );
                        break;
                    }
                    harness_core::GoalVerdict::Blocked(reason) => {
                        eprintln!(
                            "{} Goal blocked (iteration {}/{}): {}",
                            "✖".red().bold(),
                            goal.iteration,
                            goal.max_iterations,
                            reason
                        );
                        break;
                    }
                    harness_core::GoalVerdict::Continue => {
                        if !goal.advance() {
                            println!(
                                "{} Goal stopped after reaching max iterations ({}/{}).",
                                "⏸".yellow().bold(),
                                goal.iteration,
                                goal.max_iterations
                            );
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("\n{} Error: {}\n", "✖".red().bold(), error);
                break;
            }
        }
    }
    Ok(true)
}

/// Expand a skill invocation, reject unknown commands, and run one agent turn.
async fn run_piped_turn(
    rt: &mut ChatRuntime,
    query: String,
    cancel_token: &CancellationToken,
) -> Result<(), HarnessError> {
    // A skill invocation is a prompt, not a command: expand it here and let the normal
    // turn path below run it, exactly as if the user had typed the instructions. With
    // `/skills off` skill names are no longer commands.
    let skills: &[harness_plugins::Skill] =
        rt.skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
    let skill_prompt = if rt.feature_toggles.enabled(Feature::Skills) {
        crate::commands::skills::invocation_prompt(&query, skills)
    } else {
        None
    };
    let query = skill_prompt.as_deref().unwrap_or(&query);
    // An absolute path is input, not a command: `/home/me/notes.txt` goes to the agent.
    if query.starts_with('/') && !looks_like_path(query) {
        eprintln!(
            "{} Unrecognized command '{}'. Type /help for available commands.",
            "✖".red().bold(),
            query
        );
        return Ok(());
    }
    let ag_ref = rt.agent.clone();
    let q = query.to_string();
    let cancel_child = cancel_token.child_token();
    let res = {
        let ag = ag_ref.lock().await;
        ag.run_turn(&q, cancel_child).await
    };
    match res {
        Ok(outcome) => {
            rt.usage
                .record_chat(
                    &rt.current_provider,
                    &rt.current_model,
                    Some(rt.session_id.as_str()),
                    &outcome,
                )
                .await;
            println!(
                "\n{}\n{}\n",
                LUNA_LABEL.bold().green(),
                reply_for_display(&outcome.reply)
            );
        }
        Err(e) => {
            eprintln!("\n{} Error: {}\n", "✖".red().bold(), e);
        }
    }
    Ok(())
}
