//! The `/goal` slash command: start an autonomous multi-turn loop or inspect its status.

use colored::Colorize;
use harness_core::{parse_goal_args, Config, GoalRunState};

use super::SlashOutcome;
use crate::commands::chat::ui::ChatUi;

/// Handle `/goal [--max N] <objective>` from the slash dispatcher.
///
/// `/goal stop` and `/goal status` are also intercepted by [`super::super::turn::composer_command`]
/// so they respond immediately even while an iteration is in flight.
pub(crate) fn handle_goal_command(
    ui: &ChatUi,
    config: &Config,
    plan_mode: bool,
    active_goal: Option<&GoalRunState>,
    arg: Option<&str>,
) -> SlashOutcome {
    let raw = arg.unwrap_or("").trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("status") {
        emit_goal_status(ui, active_goal);
        return SlashOutcome::Continue;
    }

    if raw.eq_ignore_ascii_case("stop")
        || raw.eq_ignore_ascii_case("cancel")
        || raw.eq_ignore_ascii_case("off")
    {
        ui.emit_above(&format!("{} No active goal loop.", "•".dimmed()));
        return SlashOutcome::Continue;
    }

    if plan_mode {
        ui.emit_above(&format!(
            "{} Disable plan mode (Tab or /plan) before starting an autonomous /goal loop.",
            "✖".red().bold()
        ));
        return SlashOutcome::Continue;
    }

    if let Some(goal) = active_goal {
        ui.emit_above(&format!(
            "{} A goal loop is already active ({}/{}): {}. Use {} first.",
            "✖".red().bold(),
            goal.iteration,
            goal.max_iterations,
            goal.objective,
            "/goal stop".cyan()
        ));
        return SlashOutcome::Continue;
    }

    let (max_iterations, objective) = match parse_goal_args(raw, config.goal.max_iterations) {
        Ok(parsed) => parsed,
        Err(error) => {
            ui.emit_above(&format!(
                "{} {} — usage: {}",
                "✖".red().bold(),
                error,
                "/goal [--max N] <objective> | /goal status | /goal stop".cyan()
            ));
            return SlashOutcome::Continue;
        }
    };

    let state = match GoalRunState::new(objective.clone(), max_iterations) {
        Ok(state) => state,
        Err(error) => {
            ui.emit_above(&format!("{} {}", "✖".red().bold(), error));
            return SlashOutcome::Continue;
        }
    };

    let prompt = match state.prompt_for_current_iteration() {
        Ok(prompt) => prompt,
        Err(error) => {
            ui.emit_above(&format!("{} {}", "✖".red().bold(), error));
            return SlashOutcome::Continue;
        }
    };

    let display = format!("/goal {}", raw);
    SlashOutcome::StartGoal {
        state,
        prompt,
        display,
    }
}

pub(crate) fn emit_goal_status(ui: &ChatUi, active_goal: Option<&GoalRunState>) {
    match active_goal {
        Some(goal) => ui.emit_above(&format!(
            "{} Active goal (iteration {}/{}): {} — use {} to cancel",
            "•".green().bold(),
            goal.iteration,
            goal.max_iterations,
            goal.objective.cyan(),
            "/goal stop".yellow()
        )),
        None => ui.emit_above(&format!(
            "{} Usage: {}",
            "•".cyan(),
            "/goal [--max N] <objective> | /goal status | /goal stop".yellow()
        )),
    }
}
