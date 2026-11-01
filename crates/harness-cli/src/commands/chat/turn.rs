//! The turn itself: what a submitted line does, and how a turn is started, queued and stopped.
//!
//! The Enter path, the queue drain and the queue pump all call [`launch_turn`], so prompt history,
//! plan-mode decoration, attached images and the spawn cannot drift apart between them.
//!
//! `use super::*;` would be shorter, but a module that says what it leans on is the point of the
//! split.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use colored::Colorize;
use tokio_util::sync::CancellationToken;

use super::render::{right_aligned_user_message, OutputBlockKind, TurnProcessItem, TurnRecord};
use super::ui::{get_terminal_width, ChatUi};
use super::{redraw_transcript, ChatAgent};
use harness_core::{HarnessError, TurnOutcome};

/// What a submitted line does when the agent may not be free to take it.
///
/// The rule a user can hold onto: a plain message mid-turn steers what is running, and a message
/// typed while the transcript is being folded waits — a turn started mid-fold would be built on
/// a history that is about to be rewritten. (`/interrupt`, `Ctrl+C` or `Escape` explicitly cancel
/// the active turn.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Submission {
    /// Nothing is in flight: start a turn.
    Turn,
    /// Steer the running turn with this line.
    Steer,
    /// Stop the running turn and start a new one with this line.
    #[allow(dead_code)]
    Interrupt,
    /// Hold the line until the fold finishes; the queue pump sends it in order.
    Queue,
}

pub(super) fn submission(_line: &str, turn_active: bool, compacting: bool) -> Submission {
    if compacting {
        return Submission::Queue;
    }
    if turn_active {
        return Submission::Steer;
    }
    Submission::Turn
}

/// Echo a submitted message the way the current mode shows it, then start the turn that answers it.
///
/// One body for the Enter path, the queue drain and the queue pump, so all three send turns the
/// same way: prompt history, plan-mode decoration, attached images and the spawn itself cannot
/// drift apart between them.
#[allow(clippy::too_many_arguments)]
pub(super) fn launch_turn(
    query: &str,
    ui: &ChatUi,
    turns: &[TurnRecord],
    process_expanded: bool,
    pending_images: &Arc<Mutex<Vec<String>>>,
    plan_mode: &std::sync::atomic::AtomicBool,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    in_flight_tools: &Arc<Mutex<Vec<TurnProcessItem>>>,
    current_cancel: &mut CancellationToken,
    current_query: &mut String,
    turn_start: &mut std::time::Instant,
    active_turn: &mut Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>>,
) {
    // Attached screenshots become OpenAI-style data-URL blocks appended to the prompt text; the
    // provider layer expands them into content parts. Each URL is one paste, oldest first.
    // Image paths typed into the prompt are inlined the same way, so naming a file on disk sends
    // its pixels rather than a path the model would have to OCR through a shell tool.
    let images = std::mem::take(&mut *pending_images.lock().unwrap_or_else(|e| e.into_inner()));
    let inlined_query = crate::commands::prompt_images::inline_image_paths(query);
    let q = if plan_mode.load(std::sync::atomic::Ordering::SeqCst) {
        format!(
            "[PLAN MODE ACTIVE: Formulate a detailed, step-by-step plan/blueprint. Do not attempt to modify files or execute mutating commands until the plan is approved.]\n\n{}",
            inlined_query
        )
    } else {
        inlined_query
    };
    let q_with_images = if images.is_empty() {
        q
    } else {
        format!("{}\n\n{}", q, images.join("\n\n"))
    };

    ui.clear_patch_diff();
    // Luna is thinking from here: the composer's hint says so until the turn ends, and it is set
    // before the box is drawn so the busy line is what the user sees while it runs.
    ui.set_turn_active(true);

    let display_query = crate::commands::prompt_images::strip_image_saved_paths(query);

    if ui.is_attended {
        crate::commands::clear_screen();
        redraw_transcript(turns, process_expanded);
        println!();
        println!(
            "{}",
            right_aligned_user_message(&display_query, get_terminal_width())
        );
        ui.draw_initial_box();
    } else {
        ui.begin_block(OutputBlockKind::User);
        ui.emit_above(&right_aligned_user_message(
            &display_query,
            get_terminal_width(),
        ));
    }

    *current_cancel = CancellationToken::new();
    let cancel_child = current_cancel.child_token();
    let ag_ref = agent.clone();
    *current_query = display_query;
    *turn_start = std::time::Instant::now();
    in_flight_tools
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *active_turn = Some(tokio::spawn(async move {
        let ag = ag_ref.lock().await;
        ag.run_turn(&q_with_images, cancel_child).await
    }));
}

/// Stop the turn in flight and record it as cancelled: the body Ctrl+C, Escape and an interrupting
/// message all share, so the three cannot drift apart. Reports whether there was a turn to stop.
#[allow(clippy::too_many_arguments)]
pub(super) async fn cancel_running_turn(
    active_turn: &mut Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>>,
    current_cancel: &CancellationToken,
    ui: &ChatUi,
    turns: &mut Vec<TurnRecord>,
    in_flight_tools: &Arc<Mutex<Vec<TurnProcessItem>>>,
    turn_start: std::time::Instant,
    current_query: &str,
    process_expanded: bool,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    context_max: u32,
) -> bool {
    let Some(handle) = active_turn.take() else {
        return false;
    };
    current_cancel.cancel();
    handle.abort();
    ui.set_turn_active(false);
    ui.stop_activity();
    ui.clear_live_state();
    let current_tools =
        std::mem::take(&mut *in_flight_tools.lock().unwrap_or_else(|e| e.into_inner()));

    let record = TurnRecord {
        user_query: current_query.to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: current_tools,
        duration: turn_start.elapsed(),
        reply: "✖ Cancelled".to_string(),
    };
    turns.push(record);

    if ui.is_attended {
        crate::commands::clear_screen();
        redraw_transcript(turns, process_expanded);
        ui.draw_initial_box();
    } else {
        println!("{}", "✖ Cancelled".red());
    }

    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.update_status(context_used, context_max);
    true
}

/// Send the next queued message, if there is one and nothing is in the way.
///
/// Called when a turn ends and from the animation tick: the tick is what unblocks a queue built up
/// during a fold the user asked for, because that fold produces no turn-end event to drain it.
#[allow(clippy::too_many_arguments)]
pub(super) fn drain_queue(
    queue: &mut VecDeque<String>,
    ui: &ChatUi,
    turns: &[TurnRecord],
    process_expanded: bool,
    pending_images: &Arc<Mutex<Vec<String>>>,
    plan_mode: &std::sync::atomic::AtomicBool,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    in_flight_tools: &Arc<Mutex<Vec<TurnProcessItem>>>,
    current_cancel: &mut CancellationToken,
    current_query: &mut String,
    turn_start: &mut std::time::Instant,
    active_turn: &mut Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>>,
) {
    if active_turn.is_some() {
        return;
    }
    let Some(next_query) = queue.pop_front() else {
        return;
    };
    ui.set_queue_depth(queue.len());
    launch_turn(
        &next_query,
        ui,
        turns,
        process_expanded,
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

/// A command the composer itself acts on, as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ComposerCommand {
    /// `/queue <text>`: hold the text until the turn in flight is over.
    Queue(String),
    /// `/steer <text>`: insert the text right after the next model call.
    Steer(String),
    /// `/interrupt`: abort the model request in flight.
    Interrupt,
    /// `/queue` with no message: say how to use it.
    QueueUsage,
    /// `/steer` with no guidance: say how to use it.
    SteerUsage,
}

/// Parse a submitted line as one of the commands the composer acts on.
///
/// The name has to be the whole first word, so `/queued` and `/interrupts` stay ordinary prompts
/// instead of near-misses of these. Anything that is not one of them is `None` and travels the
/// normal path.
pub(super) fn composer_command(line: &str) -> Option<ComposerCommand> {
    let (name, arg) = match line.split_once(|c: char| c.is_whitespace()) {
        Some((name, arg)) => (name, arg.trim()),
        None => (line, ""),
    };
    match name {
        "/queue" => Some(if arg.is_empty() {
            ComposerCommand::QueueUsage
        } else {
            ComposerCommand::Queue(arg.to_string())
        }),
        "/steer" => Some(if arg.is_empty() {
            ComposerCommand::SteerUsage
        } else {
            ComposerCommand::Steer(arg.to_string())
        }),
        "/interrupt" => Some(ComposerCommand::Interrupt),
        _ => None,
    }
}

/// `/queue`, `/steer` and `/interrupt`: the commands that reach into the chat loop's own machinery.
///
/// They are handled here rather than in `execute_slash_command` because what they act on belongs to
/// this loop and nowhere else — the queue the turn-end pumps, the steer handle the agent drains
/// between model calls, and the cancellation token of the turn in flight. Returns true when the
/// line was one of them, so the caller can stop before the command menu sees it.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_composer_command(
    line: &str,
    queue: &mut VecDeque<String>,
    steer_handle: &Arc<Mutex<VecDeque<String>>>,
    ui: &ChatUi,
    active_turn: &mut Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>>,
    current_cancel: &CancellationToken,
    turns: &mut Vec<TurnRecord>,
    in_flight_tools: &Arc<Mutex<Vec<TurnProcessItem>>>,
    turn_start: std::time::Instant,
    current_query: &str,
    process_expanded: bool,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    context_max: u32,
) -> bool {
    let Some(command) = composer_command(line) else {
        return false;
    };

    match command {
        ComposerCommand::Queue(text) => {
            let thinking = active_turn.is_some();
            queue.push_back(text);
            ui.set_queue_depth(queue.len());
            ui.emit_above(&format!(
                "{} Queued ({} waiting) — {}",
                "⏸".yellow().bold(),
                queue.len(),
                if thinking {
                    "sent when the turn in flight finishes".to_string()
                } else {
                    "Luna is idle, so it goes out on the next turn".to_string()
                }
            ));
        }
        ComposerCommand::Steer(text) => {
            // Pushed straight onto the handle the agent drains. Taking the agent lock here would
            // block until the running turn released it, and the guidance would land after the turn
            // it was meant to steer — which is the one thing a steer must never do.
            steer_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(text.clone());
            if active_turn.is_some() {
                let line = format!("{} Steered into active turn: {}", "⏩".cyan().bold(), text);
                in_flight_tools
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(TurnProcessItem {
                        call_formatted: line.clone(),
                        result_formatted: String::new(),
                        diff_lines: Vec::new(),
                    });
                ui.emit_above(&line);
                if ui.is_attended {
                    ui.draw_initial_box();
                }
            } else {
                ui.emit_above(&format!(
                    "{} Steer queued — delivered right after the next turn's first model call: {}",
                    "⏩".cyan().bold(),
                    text.dimmed()
                ));
            }
        }
        ComposerCommand::Interrupt => {
            let cancelled = cancel_running_turn(
                active_turn,
                current_cancel,
                ui,
                turns,
                in_flight_tools,
                turn_start,
                current_query,
                process_expanded,
                agent,
                context_max,
            )
            .await;
            if !cancelled {
                ui.emit_above(&format!(
                    "{} Nothing to interrupt — Luna is not thinking",
                    "•".dimmed()
                ));
            }
        }
        ComposerCommand::QueueUsage => {
            ui.emit_above(&format!(
                "{} Usage: /queue <message> — held until the turn in flight finishes",
                "•".dimmed()
            ));
        }
        ComposerCommand::SteerUsage => {
            ui.emit_above(&format!(
                "{} Usage: /steer <guidance> — inserted right after the next model call",
                "•".dimmed()
            ));
        }
    }
    true
}
