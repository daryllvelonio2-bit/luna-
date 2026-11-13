//! Everything the event loop does on a tick or a channel message.

use colored::Colorize;
use harness_core::SessionStore;
use tokio::sync::oneshot;

use super::super::helpers::redraw_transcript;
use super::super::prompts::prompt_approval_choice;
use super::super::prompts::prompt_sudo_password_dialog;
use super::super::render::{
    background_job_resume_prompt, format_approval_verdict, format_elapsed_secs,
    format_job_notification, format_job_system_message, format_tool_call,
    right_aligned_user_message, TurnProcessItem,
};
use super::super::status::{refresh_antigravity_quota_owned, QUOTA_REFRESH_INTERVAL};
use super::super::turn::{can_auto_resume, drain_queue, launch_internal_turn};
use super::super::ui::get_terminal_width;
use super::state::{Env, KeyRx, ReplState};
use crate::commands::approval::ApprovalChoice;

/// Repaint every row at the new width: transcript, live turn rows, transient error, composer.
pub(super) fn reflow(env: &Env<'_>, st: &ReplState) {
    let Env {
        ui,
        in_flight_tools,
        ..
    } = env;
    let ReplState {
        turns,
        process_expanded,
        active_turn,
        current_query,
        ..
    } = st;
    crate::commands::clear_screen();
    redraw_transcript(turns, *process_expanded);
    if active_turn.is_some() {
        if !current_query.trim().is_empty() {
            println!();
            println!(
                "{}",
                right_aligned_user_message(current_query, get_terminal_width())
            );
        }
        let tools = in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for item in &tools {
            println!("{}", item.call_formatted);
            if !item.result_formatted.is_empty() {
                println!("{}", item.result_formatted);
            }
            for diff in &item.diff_lines {
                println!("{}", diff);
            }
        }
    }
    if let Some(ref err) = ui.transient_error() {
        println!();
        println!("{}", err);
    }
    ui.handle_resize();
}

/// The animation tick: settle the live region, refresh the quota, drain the queue, and run the
/// idle memory pass.
pub(super) fn animation(env: &mut Env<'_>, st: &mut ReplState) {
    let Env {
        ui,
        cred_manager,
        agent,
        plan_mode,
        compacting,
        in_flight_tools,
        config,
        session_id,
        current_provider,
        current_model,
        ..
    } = env;
    let ReplState {
        turns,
        process_expanded,
        queue,
        current_cancel,
        active_turn,
        active_goal,
        current_query,
        turn_start,
        quota_refreshed_at,
        idle_pass,
        pending_job_turn,
        pending_images,
        ..
    } = st;
    ui.animate_tick();
    // The countdown is rendered from the bucket's reset time, so an idle composer — one
    // with no live rows to animate — paints nothing and would show a frozen clock.
    ui.refresh_quota_clock();
    // The same tick re-reads the account's quota once its snapshot has aged out. It is
    // spawned rather than awaited: a metered endpoint that is slow or down must never
    // be something the composer stalls behind.
    if quota_refreshed_at.elapsed() >= QUOTA_REFRESH_INTERVAL {
        *quota_refreshed_at = std::time::Instant::now();
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
            queue,
            ui,
            turns,
            *process_expanded,
            pending_images,
            plan_mode,
            agent,
            in_flight_tools,
            current_cancel,
            current_query,
            turn_start,
            active_turn,
        );
    }

    // A completion that arrived while the composer was busy waits for this tick.
    // A turn that started after the notification already replayed the stored
    // system message, so it covers the job and the flag is cleared rather than
    // starting a second, redundant turn.
    if let Some((pending_session, at)) = pending_job_turn.take() {
        if pending_session != session_id.as_str() {
            // The completion lives in the session it arrived in; a switch means this
            // loop's next turn would not replay it.
        } else if active_turn.is_some() && *turn_start > at {
            // A turn that started after the notification already replayed it.
        } else {
            let draft = ui
                .input_buf
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let images = pending_images
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len();
            if active_goal.is_none()
                && can_auto_resume(
                    active_turn.is_some(),
                    compacting.load(std::sync::atomic::Ordering::SeqCst),
                    queue.len(),
                    &draft,
                    images,
                    ui.modal_active(),
                    plan_mode.load(std::sync::atomic::Ordering::SeqCst),
                )
            {
                launch_internal_turn(
                    &background_job_resume_prompt(),
                    ui,
                    agent,
                    in_flight_tools,
                    current_cancel,
                    current_query,
                    turn_start,
                    active_turn,
                );
            } else {
                // Still busy: put it back unchanged so the next tick asks again.
                *pending_job_turn = Some((pending_session, at));
            }
        }
    }

    // The idle memory pass. A short session can end before the every-N-turns interval
    // fires, so once the conversation has been quiet for a while the tick runs one
    // pass over what is already there.
    let busy = active_turn.is_some()
        || compacting.load(std::sync::atomic::Ordering::SeqCst)
        || !queue.is_empty();
    if idle_pass.tick(
        turns.len(),
        busy,
        config.memory.background_idle_secs,
        std::time::Instant::now(),
    ) {
        let agent = agent.clone();
        tokio::spawn(async move {
            let agent = agent.lock().await;
            agent.spawn_memory_pass_now().await;
        });
    }
}

/// A tool wants a verdict.
pub(super) async fn approval(
    env: &Env<'_>,
    key_rx: &mut KeyRx,
    maybe_approval: Option<(
        harness_core::ApprovalRequest,
        oneshot::Sender<harness_core::ApprovalDecision>,
    )>,
) {
    let Env {
        ui,
        allow_all_session,
        in_flight_tools,
        ..
    } = env;
    let (request, mut decision_tx) = match maybe_approval {
        Some(pair) => pair,
        None => return,
    };

    if decision_tx.is_closed() {
        return;
    }

    // The popup owns the screen while it is up and gives it back on the way out; the
    // verdict below is what lands in the transcript.
    let choice = prompt_approval_choice(key_rx, &request, &mut decision_tx, ui).await;
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
        in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(TurnProcessItem {
                tool_name: "approval".to_string(),
                call_formatted: verdict_str,
                result_formatted: String::new(),
                diff_lines: Vec::new(),
                output: String::new(),
                output_dropped: 0,
                arguments: None,
                duration_secs: None,
                is_error: false,
            });
    }
    if is_run {
        let args = request.arguments.clone();
        ui.start_activity(
            &format_tool_call(&request.tool, &args),
            &request.tool,
            &args,
        );
    }
    let _ = decision_tx.send(decision);
}

/// A tool wants the sudo password.
pub(super) async fn sudo(
    env: &Env<'_>,
    key_rx: &mut KeyRx,
    maybe_sudo: Option<(String, oneshot::Sender<Option<String>>)>,
) {
    let Env { ui, .. } = env;
    let (command, decision_tx) = match maybe_sudo {
        Some(pair) => pair,
        None => return,
    };

    // The dialog owns the screen while it is up (see `prompt_sudo_password_dialog`):
    // nothing to clear here, and it prints its own outcome on the way back.
    let password = prompt_sudo_password_dialog(key_rx, &command, ui).await;
    let _ = decision_tx.send(password);
}

/// The model wants a clarification.
pub(super) async fn clarify(
    env: &Env<'_>,
    key_rx: &mut KeyRx,
    maybe_clarify: Option<(
        crate::commands::clarify::ClarifyRequest,
        oneshot::Sender<crate::commands::clarify::ClarifyResponse>,
    )>,
) {
    let Env { ui, .. } = env;
    let (request, response_tx) = match maybe_clarify {
        Some(pair) => pair,
        None => return,
    };

    // Nothing is cleared and no box is drawn on the way out: the question is asked on
    // the alternate screen, which leaves the transcript and the composer untouched,
    // and the lines it records on the way back restore the composer themselves. A
    // second `draw_initial_box` here would draw a second box over the live one.
    let response = crate::commands::clarify::prompt_clarify_choice(key_rx, &request, ui).await;
    let _ = response_tx.send(response);
}

/// A background job finished: store it, show it, and start the turn that reacts to it.
pub(super) async fn job(
    env: &mut Env<'_>,
    st: &mut ReplState,
    maybe_job: Option<harness_core::tasks::JobCompletion>,
) {
    let Env {
        ui,
        store,
        agent,
        plan_mode,
        compacting,
        in_flight_tools,
        session_id,
        ..
    } = env;
    let ReplState {
        queue,
        current_cancel,
        active_turn,
        active_goal,
        current_query,
        turn_start,
        pending_job_turn,
        pending_images,
        ..
    } = st;
    let Some(completion) = maybe_job else {
        return;
    };

    let is_ok = completion.status.starts_with("completed");
    let glyph = if is_ok {
        "✔".green().bold().to_string()
    } else {
        "✖".red().bold().to_string()
    };
    let notice = format_job_notification(
        &glyph,
        &completion.id,
        &completion.command,
        &completion.status,
        &format_elapsed_secs(completion.elapsed_secs),
    );
    ui.emit_above(&notice);

    // Store the full result before any turn can replay it. A turn already running
    // loaded its history before this row existed, which is what the deferred
    // auto-resume below is for; the next turn will see it either way.
    let message = harness_core::Message::system(format_job_system_message(&completion));
    let stored = match store.append(session_id, &message, false).await {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!("failed to store background job completion: {}", error);
            false
        }
    };

    // Only a stored completion can drive a turn: the model reads it from the replay,
    // and a turn started without that row would have nothing to react to. If the
    // write failed, the notice above is still there and the job remains pollable.
    if stored {
        let draft = ui
            .input_buf
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let images = pending_images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        if active_goal.is_none()
            && can_auto_resume(
                active_turn.is_some(),
                compacting.load(std::sync::atomic::Ordering::SeqCst),
                queue.len(),
                &draft,
                images,
                ui.modal_active(),
                plan_mode.load(std::sync::atomic::Ordering::SeqCst),
            )
        {
            launch_internal_turn(
                &background_job_resume_prompt(),
                ui,
                agent,
                in_flight_tools,
                current_cancel,
                current_query,
                turn_start,
                active_turn,
            );
        } else {
            // Busy now: remember the moment and the session so the tick can act once
            // the composer is quiet, unless a turn started after it and replayed the
            // message.
            pending_job_turn.get_or_insert_with(|| (session_id.clone(), std::time::Instant::now()));
        }
    }
}
