//! What a finished turn leaves behind: the transcript record, the redraw and the queue pump.

use colored::Colorize;
use harness_core::HarnessError;

use super::super::helpers::redraw_transcript;
use super::super::render::{reply_for_display, LUNA_LABEL};
use super::super::todo;
use super::super::turn::{drain_queue, launch_internal_turn, turn_records};
use super::state::{Env, ReplState};
use crate::commands::context_window::context_window;
use harness_core::{GoalRunState, GoalVerdict};

/// Record the turn, repaint, and pump whatever was queued while it ran.
pub(super) async fn handle(
    env: &Env<'_>,
    st: &mut ReplState,
    turn_res: Result<
        Result<harness_core::TurnOutcome, harness_core::HarnessError>,
        tokio::task::JoinError,
    >,
) {
    st.active_turn = None;
    // If the turn that just ended started after a deferred completion, it replayed
    // that completion's stored message; the deferred auto-turn is no longer needed.
    if st
        .pending_job_turn
        .as_ref()
        .is_some_and(|(pending_session, at)| {
            pending_session == env.session_id.as_str() && st.turn_start > *at
        })
    {
        st.pending_job_turn = None;
    }
    // Nothing is in flight any more, whatever the turn did: the hint goes back to
    // the idle line before the transcript below is redrawn.
    env.ui.set_turn_active(false);
    // One snapshot for every outcome: a turn that stopped early still produced the
    // tool rows and mid-turn messages above, and each branch records them the same way.
    let current_tools = std::mem::take(
        &mut *env
            .in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );
    let interruptions =
        std::mem::take(&mut *env.interruptions.lock().unwrap_or_else(|e| e.into_inner()));
    let turn_dur = st.turn_start.elapsed();
    let goal_reply = match turn_res {
        Ok(Ok(outcome)) => {
            let reply = outcome.reply.clone();
            handle_success(env, st, outcome, interruptions, current_tools, turn_dur).await;
            Some(reply)
        }
        Ok(Err(HarnessError::Cancelled)) => {
            handle_cancelled(env, st, interruptions, current_tools).await;
            None
        }
        Ok(Err(e)) => {
            handle_failure(
                env,
                st,
                interruptions,
                current_tools,
                turn_dur,
                "Error",
                e.to_string(),
            )
            .await;
            None
        }
        Err(join_err) => {
            handle_failure(
                env,
                st,
                interruptions,
                current_tools,
                turn_dur,
                "Task join error",
                join_err.to_string(),
            )
            .await;
            None
        }
    };

    // Whatever the turn did — finish, fail, get cancelled — a line the user typed
    // while it ran may still be held by the interrupt channel. It was never part of
    // the turn's answer, so it goes back in the queue the pump below drains.
    let held = env.interrupt_handle.take_leftover();
    if !held.is_empty() {
        st.queue.extend(held);
        env.ui.set_queue_depth(st.queue.len());
    }

    if let Some(reply) = goal_reply {
        advance_goal_if_active(env, st, &reply);
    }

    drain_queue(
        &mut st.queue,
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
}

async fn handle_success(
    env: &Env<'_>,
    st: &mut ReplState,
    outcome: harness_core::TurnOutcome,
    interruptions: Vec<super::super::turn::InterruptNotice>,
    current_tools: Vec<super::super::render::TurnProcessItem>,
    turn_dur: std::time::Duration,
) {
    env.ui.stop_activity();
    env.ui.clear_live_state();
    // Accounting happens before rendering: a UI panic must not lose the row.
    env.usage
        .record_chat(
            env.current_provider,
            env.current_model,
            Some(env.session_id.as_str()),
            &outcome,
        )
        .await;

    st.turns.extend(turn_records(
        &st.current_query,
        interruptions,
        st.turn_start,
        turn_dur,
        outcome.reasoning.clone(),
        outcome.tool_calls.clone(),
        current_tools,
        outcome.reply.clone(),
    ));

    if env.config.memory.mode == harness_core::MemoryMode::Background
        && env.config.memory.background_every_turns > 0
        && st
            .turns
            .len()
            .is_multiple_of(env.config.memory.background_every_turns)
    {
        st.idle_pass.mark_passed(st.turns.len());
    }

    if env.config.display.auto_collapse {
        st.process_expanded = false;
    }

    if env.ui.is_attended {
        repaint_attended(env, st);
    } else if !outcome.reply.is_empty() {
        println!(
            "{}\n{}",
            LUNA_LABEL.bold().green(),
            reply_for_display(&outcome.reply)
        );
    }

    update_status_bar(env).await;
}

async fn handle_cancelled(
    env: &Env<'_>,
    st: &mut ReplState,
    interruptions: Vec<super::super::turn::InterruptNotice>,
    current_tools: Vec<super::super::render::TurnProcessItem>,
) {
    env.ui.stop_activity();
    env.ui.clear_live_state();
    let turn_dur = st.turn_start.elapsed();

    st.turns.extend(turn_records(
        &st.current_query,
        interruptions,
        st.turn_start,
        turn_dur,
        None,
        Vec::new(),
        current_tools,
        "✖ Cancelled".to_string(),
    ));

    if env.ui.is_attended {
        repaint_attended(env, st);
    } else {
        println!("{}", "✖ Cancelled".red());
    }

    if st.active_goal.take().is_some() {
        env.ui.set_goal_status(None);
        env.ui
            .emit_above(&format!("{} Goal loop stopped.", "•".dimmed()));
    }

    update_status_bar(env).await;
}

async fn handle_failure(
    env: &Env<'_>,
    st: &mut ReplState,
    interruptions: Vec<super::super::turn::InterruptNotice>,
    current_tools: Vec<super::super::render::TurnProcessItem>,
    turn_dur: std::time::Duration,
    label: &str,
    detail: String,
) {
    env.ui.stop_activity();
    env.ui.clear_live_state();
    let goal_was_active = st.active_goal.take().is_some();
    if goal_was_active {
        env.ui.set_goal_status(None);
    }
    let had_records = !current_tools.is_empty() || !interruptions.is_empty();

    if had_records {
        st.turns.extend(turn_records(
            &st.current_query,
            interruptions,
            st.turn_start,
            turn_dur,
            None,
            Vec::new(),
            current_tools,
            format!("✖ {label}: {detail}"),
        ));
        *env.ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = "continue".to_string();

        if env.ui.is_attended {
            repaint_attended(env, st);
        } else {
            println!("{} {label}: {}", "✖".red().bold(), detail);
        }
    } else {
        if !st.current_query.is_empty() {
            *env.ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = st.current_query.clone();
        }

        let err_msg = format!("{} {label}: {}", "✖".red().bold(), detail);
        env.ui.set_transient_error(Some(err_msg.clone()));

        if env.ui.is_attended {
            crate::commands::clear_screen();
            redraw_transcript(&st.turns, st.process_expanded);
            println!();
            println!("{}", err_msg);
            env.ui.draw_initial_box();
        } else {
            println!("{}", err_msg);
        }
    }

    if goal_was_active {
        env.ui.emit_above(&format!(
            "{} Goal loop stopped due to turn error.",
            "✖".red().bold()
        ));
    }

    update_status_bar(env).await;
}

fn advance_goal_if_active(env: &Env<'_>, st: &mut ReplState, reply: &str) {
    let Some(mut goal) = st.active_goal.take() else {
        return;
    };

    match GoalRunState::evaluate_reply(reply) {
        GoalVerdict::Complete(_) => {
            env.ui.set_goal_status(None);
            env.ui.emit_above(&format!(
                "{} Goal achieved in {} iteration(s).",
                "✔".green().bold(),
                goal.iteration
            ));
        }
        GoalVerdict::Blocked(reason) => {
            env.ui.set_goal_status(None);
            env.ui.emit_above(&format!(
                "{} Goal blocked (iteration {}/{}): {}",
                "✖".red().bold(),
                goal.iteration,
                goal.max_iterations,
                reason
            ));
        }
        GoalVerdict::Continue => {
            if !st.queue.is_empty() {
                env.ui.set_goal_status(None);
                env.ui.emit_above(&format!(
                    "{} Goal loop paused for queued input after iteration {}/{}.",
                    "⏸".yellow().bold(),
                    goal.iteration,
                    goal.max_iterations
                ));
                return;
            }
            if env.plan_mode.load(std::sync::atomic::Ordering::SeqCst) {
                env.ui.set_goal_status(None);
                env.ui.emit_above(&format!(
                    "{} Goal loop stopped because plan mode is active.",
                    "⏸".yellow().bold()
                ));
                return;
            }
            if !goal.advance() {
                env.ui.set_goal_status(None);
                env.ui.emit_above(&format!(
                    "{} Goal stopped after reaching max iterations ({}/{}).",
                    "⏸".yellow().bold(),
                    goal.iteration,
                    goal.max_iterations
                ));
                return;
            }
            let next_prompt = match goal.prompt_for_current_iteration() {
                Ok(prompt) => prompt,
                Err(error) => {
                    env.ui.set_goal_status(None);
                    env.ui.emit_above(&format!(
                        "{} Failed to build next goal prompt: {}",
                        "✖".red().bold(),
                        error
                    ));
                    return;
                }
            };
            env.ui
                .set_goal_status(Some((goal.iteration, goal.max_iterations)));
            env.ui.emit_above(&format!(
                "{} Goal iteration {}/{} — continuing...",
                "↻".cyan().bold(),
                goal.iteration,
                goal.max_iterations
            ));
            st.active_goal = Some(goal);
            launch_internal_turn(
                &next_prompt,
                &env.ui,
                &env.agent,
                &env.in_flight_tools,
                &mut st.current_cancel,
                &mut st.current_query,
                &mut st.turn_start,
                &mut st.active_turn,
            );
        }
    }
}

fn repaint_attended(env: &Env<'_>, st: &ReplState) {
    todo::refresh_panel(&env.ui, &env.todo_scope);
    crate::commands::clear_screen();
    redraw_transcript(&st.turns, st.process_expanded);
    env.ui.draw_initial_box();
}

async fn update_status_bar(env: &Env<'_>) {
    // Update status bar with current context usage
    let context_used = env
        .agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    let context_max = context_window(
        env.config,
        &env.discovery,
        env.current_provider,
        env.current_model,
    );
    env.ui.update_status(context_used, context_max);
}
