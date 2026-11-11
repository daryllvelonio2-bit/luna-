//! `/reasoning` and `/thinking`: the aliases that set the agent's reasoning effort.

use colored::Colorize;
use std::sync::Arc;
use tokio::sync::mpsc;

use harness_core::{HarnessError, ReasoningEffort};

use crate::commands::chat::keys::AppKey;
use crate::commands::chat::ui::ChatUi;
use crate::commands::chat::ChatAgent;

use super::picker::select_from_rx;

pub(crate) async fn handle_thinking_switch(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    handle_reasoning_or_thinking_switch(key_rx, ui, agent, arg, true).await
}

pub(crate) async fn handle_reasoning_switch(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    handle_reasoning_or_thinking_switch(key_rx, ui, agent, arg, false).await
}

async fn handle_reasoning_or_thinking_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
    is_thinking: bool,
) -> Result<(), HarnessError> {
    let concept_name = if is_thinking {
        "thinking level"
    } else {
        "reasoning effort"
    };
    let title = if is_thinking {
        "Select Thinking Level"
    } else {
        "Select Reasoning Effort"
    };

    let chosen_effort = if let Some(s) = arg {
        if s.eq_ignore_ascii_case("off")
            || s.eq_ignore_ascii_case("none")
            || s.eq_ignore_ascii_case("disabled")
            || s == "0"
            || s.eq_ignore_ascii_case("false")
        {
            None
        } else {
            match ReasoningEffort::from_str_loose(s) {
                Some(e) => Some(e),
                None => {
                    ui.draw_initial_box();
                    ui.emit_above(&format!(
                        "{} Unknown {} '{}'. Available: minimal, low, medium, high, xhigh, max, off",
                        "✖".red().bold(),
                        concept_name,
                        s
                    ));
                    return Ok(());
                }
            }
        }
    } else {
        let reasoning_options = [
            ("off / default", None),
            ("minimal", Some(ReasoningEffort::Minimal)),
            ("low", Some(ReasoningEffort::Low)),
            ("medium", Some(ReasoningEffort::Medium)),
            ("high", Some(ReasoningEffort::High)),
            ("xhigh", Some(ReasoningEffort::Xhigh)),
            ("max", Some(ReasoningEffort::Max)),
        ];
        let reasoning_items: Vec<String> = reasoning_options
            .iter()
            .map(|(label, _)| label.to_string())
            .collect();
        let current_effort = agent.lock().await.reasoning_effort();
        let default_idx = match current_effort {
            None => 0,
            Some(ReasoningEffort::Minimal) => 1,
            Some(ReasoningEffort::Low) => 2,
            Some(ReasoningEffort::Medium) => 3,
            Some(ReasoningEffort::High) => 4,
            Some(ReasoningEffort::Xhigh) => 5,
            Some(ReasoningEffort::Max) => 6,
        };

        if let Some(ref mut rx) = key_rx {
            while rx.try_recv().is_ok() {}
            match select_from_rx(rx, ui, title, &reasoning_items, default_idx, false, false).await {
                Some(i) => reasoning_options[i].1,
                None => {
                    return Ok(());
                }
            }
        } else {
            let cur = agent.lock().await.reasoning_effort();
            let cur_label = cur
                .map(|e| e.to_string())
                .unwrap_or_else(|| "off".to_string());
            ui.emit_above(&format!(
                "{} Current {} is '{}'",
                "ℹ".blue().bold(),
                concept_name,
                cur_label.yellow()
            ));
            return Ok(());
        }
    };

    agent.lock().await.set_reasoning_effort(chosen_effort);
    // Same as the model switch: the picker left a box behind, so erase it before redrawing.
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    let effort_label = chosen_effort
        .map(|e| e.to_string())
        .unwrap_or_else(|| "off".to_string());

    let label_capitalized = if is_thinking {
        "Thinking level"
    } else {
        "Reasoning effort"
    };

    ui.emit_above(&format!(
        "{} {} set to '{}'",
        "✔".green().bold(),
        label_capitalized,
        effort_label.yellow()
    ));

    Ok(())
}
