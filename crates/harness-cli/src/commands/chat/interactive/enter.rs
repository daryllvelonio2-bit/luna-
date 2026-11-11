//! The input path: what a submitted line sends, queues, or dispatches as a command.

use colored::Colorize;
use harness_core::{HarnessError, InterruptOutcome};

use super::super::commands::{
    command_entries, is_bare_feature_command, looks_like_path, select_slash_command, Feature,
};
use super::super::history::append_prompt_history;
use super::super::turn::{launch_turn, submission, Submission};
use super::super::ui::ChatUi;
use super::command;
use super::state::{Env, Flow, KeyRx, ReplState};

/// Submit the composer's line.
pub(super) async fn handle(
    env: &mut Env<'_>,
    st: &mut ReplState,
    key_rx: &mut KeyRx,
) -> Result<Flow, HarnessError> {
    st.history_cursor = None;
    st.draft_input.clear();
    let query = take_query(&env.ui);

    if query.is_empty() {
        env.ui.redraw_input_line();
        return Ok(Flow::Continue);
    }

    record_prompt_history(st, &query);

    // A screenshot pasted before the command leaves an `[image: ...]` marker at
    // the front of the line. It is an attachment, not command text, so command
    // detection looks past it; the full text is still what gets sent.
    let command_query = crate::commands::prompt_images::strip_leading_image_markers(&query);

    if command_query == "/" || command_query.eq_ignore_ascii_case("/help") {
        open_help_menu(env, st, key_rx).await?;
        return Ok(Flow::Continue);
    }

    if let Some(cmd_str) = command_line(command_query) {
        command::run_tail(env, st, key_rx, &cmd_str, command_query).await?;
        return Ok(Flow::Continue);
    }

    env.ui.clear_transient_error();

    if let Some(flow) = handle_submission(env, st, &query) {
        return Ok(flow);
    }

    launch_turn(
        &query,
        None,
        &env.ui,
        &st.turns,
        st.process_expanded,
        &st.pending_images,
        &env.plan_mode,
        &env.agent,
        &env.in_flight_tools,
        &mut st.current_cancel,
        &mut st.current_query,
        &mut st.turn_start,
        &mut st.active_turn,
    );
    Ok(Flow::Continue)
}

/// Take the typed line out of the composer, trimmed, leaving it empty.
fn take_query(ui: &ChatUi) -> String {
    let mut b = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
    let s = b.trim().to_string();
    b.clear();
    s
}

/// Remember the submitted line in prompt history unless it repeats the last entry.
fn record_prompt_history(st: &mut ReplState, query: &str) {
    if st.prompt_history.last().map(|s| s != query).unwrap_or(true) {
        append_prompt_history(st.history_path.as_deref(), query);
        st.prompt_history.push(query.to_string());
    }
}

/// Open the slash-command palette for `/` or `/help`.
async fn open_help_menu(
    env: &mut Env<'_>,
    st: &mut ReplState,
    key_rx: &mut KeyRx,
) -> Result<(), HarnessError> {
    let skills: &[harness_plugins::Skill] =
        env.skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
    if let Some(cmd) = select_slash_command(
        key_rx,
        &env.ui,
        &command_entries(
            &*env.config,
            skills,
            env.feature_toggles.enabled(Feature::Skills),
        ),
        &st.pending_images,
    )
    .await
    {
        env.ui
            .input_buf
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        env.ui.redraw_input_line();
        command::run_tail(env, st, key_rx, &cmd, &cmd).await?;
    }
    Ok(())
}

/// The command string to run when the line dispatches a slash command, bare feature, or exit.
fn command_line(command_query: &str) -> Option<String> {
    let is_bare_feature = is_bare_feature_command(command_query);
    if (command_query.starts_with('/') && !looks_like_path(command_query))
        || command_query.eq_ignore_ascii_case("exit")
        || command_query.eq_ignore_ascii_case("quit")
        || is_bare_feature
    {
        if is_bare_feature && !command_query.starts_with('/') {
            Some(format!("/{}", command_query.trim()))
        } else {
            Some(command_query.to_string())
        }
    } else {
        None
    }
}

/// One decision for the whole input path: replace the turn, wait out the fold, or just send.
fn handle_submission(env: &mut Env<'_>, st: &mut ReplState, query: &str) -> Option<Flow> {
    match submission(
        query,
        st.active_turn.is_some(),
        env.compacting.load(std::sync::atomic::Ordering::SeqCst),
    ) {
        Submission::Queue => {
            queue_line(&env.ui, st, query);
            Some(Flow::Continue)
        }
        Submission::Interrupt => {
            // The turn keeps running. The handle stops the model call that is
            // streaming and hands the line to the same turn, which keeps the
            // reasoning and partial reply it had produced. If a tool is running,
            // the line waits for that batch rather than killing it.
            match env.interrupt_handle.interrupt(query) {
                InterruptOutcome::Streaming => {
                    env.ui.emit_above(&format!(
                        "{} Redirecting — stopping this reply to take your message",
                        "↳".cyan().bold()
                    ));
                }
                InterruptOutcome::Queued => {
                    env.ui.emit_above(&format!(
                        "{} Held until the running tool finishes, then sent",
                        "↳".cyan().bold()
                    ));
                }
                InterruptOutcome::Idle => {
                    // The turn ended between the busy check and the handoff.
                    // Queue the line so the turn-end pump still sends it, and
                    // the turn that just finished is recorded as usual.
                    queue_line(&env.ui, st, query);
                }
            }
            Some(Flow::Continue)
        }
        Submission::Turn => None,
    }
}

/// Queue a line for the turn-end pump.
fn queue_line(ui: &ChatUi, st: &mut ReplState, query: &str) {
    st.queue.push_back(query.to_string());
    ui.set_queue_depth(st.queue.len());
    ui.emit_above(&format!(
        "{} Queued — sent when the agent is free",
        "⏸".yellow().bold()
    ));
}
