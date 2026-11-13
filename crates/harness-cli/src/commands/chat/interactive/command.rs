//! One tail for every way a slash command can be chosen: the menu, the `/` help menu and the
//! typed line all execute it the same way.

use colored::Colorize;
use harness_core::HarnessError;

use super::super::commands::{execute_slash_command, SlashOutcome};
use super::super::helpers::finish_slash_command;
use super::super::turn::{handle_composer_command, launch_turn};
use super::super::viewer::show_background_task_viewer;
use super::state::{Env, Flow, KeyRx, ReplState};
use crate::commands::context_window::context_window;

/// Run `cmd` as a command, then act on its outcome.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_tail(
    env: &mut Env<'_>,
    st: &mut ReplState,
    key_rx: &mut KeyRx,
    cmd: &str,
    display: &str,
) -> Result<Flow, HarnessError> {
    let Env {
        ui,
        config,
        cred_manager,
        discovery,
        agent,
        store,
        session_id,
        current_provider,
        current_model,
        compact_provider,
        compact_model,
        todo_scope,
        skills_arc,
        skills_readout,
        feature_toggles,
        plan_mode,
        bg_queue,
        delegate_tool,
        steer_handle,
        interrupt_handle,
        in_flight_tools,
        interruptions,
        display_settings,
        ..
    } = env;
    let ReplState {
        turns,
        process_expanded,
        queue,
        active_turn,
        active_goal,
        current_cancel,
        current_query,
        turn_start,
        pending_images,
        ..
    } = st;
    let skills: &[harness_plugins::Skill] = skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);

    // The menu can name a composer command too — picking `/interrupt` while Luna is thinking has
    // to interrupt — so it goes through the same handler the typed form does.
    if handle_composer_command(
        cmd,
        queue,
        steer_handle,
        ui,
        active_turn,
        active_goal,
        current_cancel,
        interrupt_handle,
        turns,
        in_flight_tools,
        interruptions,
        *turn_start,
        current_query,
        *process_expanded,
        agent,
        context_window(config, discovery, current_provider, current_model),
    )
    .await
    {
        return Ok(Flow::Continue);
    }

    if (cmd == "/goal" || cmd.starts_with("/goal ")) && active_turn.is_some() {
        ui.emit_above(&format!(
            "{} A turn is currently running. Use {} or {} first.",
            "✖".red().bold(),
            "/goal stop".cyan(),
            "/interrupt".cyan()
        ));
        return Ok(Flow::Continue);
    }

    if cmd == "/jobs" || cmd == "/tasks" || cmd == "/subagents" {
        show_background_task_viewer(
            key_rx,
            Some(bg_queue.as_ref()),
            Some(delegate_tool.as_ref()),
            store.as_ref(),
            ui,
            turns,
            *process_expanded,
        )
        .await;
        return Ok(Flow::Continue);
    }

    ui.begin_command_capture();
    let outcome = execute_slash_command(
        cmd,
        key_rx,
        ui,
        config,
        cred_manager,
        discovery,
        agent,
        store,
        session_id,
        current_provider,
        current_model,
        compact_provider,
        compact_model,
        turns,
        process_expanded,
        todo_scope,
        skills_readout.as_deref(),
        skills,
        feature_toggles,
        plan_mode,
        active_goal.as_ref(),
        Some(bg_queue.as_ref()),
        Some(delegate_tool.as_ref()),
        Some(pending_images),
        display_settings,
    )
    .await?;
    finish_slash_command(ui, turns, display, *process_expanded, active_turn.is_none());
    match outcome {
        SlashOutcome::Exit => Ok(Flow::Break),
        SlashOutcome::Turn { prompt, display } => {
            launch_turn(
                &prompt,
                Some(&display),
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
            Ok(Flow::Continue)
        }
        SlashOutcome::StartGoal {
            state,
            prompt,
            display,
        } => {
            ui.set_goal_status(Some((state.iteration, state.max_iterations)));
            *active_goal = Some(state);
            launch_turn(
                &prompt,
                Some(&display),
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
            Ok(Flow::Continue)
        }
        SlashOutcome::Continue => Ok(Flow::Continue),
    }
}
