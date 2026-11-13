//! What a keypress does when the composer owns the screen.

use colored::Colorize;
use harness_core::HarnessError;

use super::super::clear_on_exit;
use super::super::commands::{command_entries, select_slash_command, Feature};
use super::super::helpers::{key_keeps_landing, redraw_transcript};
use super::super::keys::AppKey;
use super::super::render::right_aligned_user_message;
use super::super::turn::cancel_running_turn;
use super::super::ui::get_terminal_width;
use super::super::viewer::show_background_task_viewer;
use super::command;
use super::enter;
use super::state::{Env, Flow, KeyRx, ReplState};
use crate::commands::context_window::context_window;

/// Handle one key. `Enter` is the whole input path and lives in [`super::enter`].
pub(super) async fn handle(
    env: &mut Env<'_>,
    st: &mut ReplState,
    key_rx: &mut KeyRx,
    key: AppKey,
) -> Result<Flow, HarnessError> {
    // Editing the prompt does not move the mark, so the shower keeps running while
    // the user types. Only a key that can take over the screen, change the composer's
    // height or start a turn settles it.
    let prompt_empty = env
        .ui
        .input_buf
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty();
    if !key_keeps_landing(&key, prompt_empty) {
        env.ui.stop_landing();
    }

    dispatch_key(env, st, key_rx, key).await
}

async fn dispatch_key(
    env: &mut Env<'_>,
    st: &mut ReplState,
    key_rx: &mut KeyRx,
    key: AppKey,
) -> Result<Flow, HarnessError> {
    let input_empty = env
        .ui
        .input_buf
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty();
    match key {
        AppKey::Ctrl('o') => handle_ctrl_o(env, st),
        AppKey::CtrlShiftO | AppKey::Ctrl('t') => {
            // Ctrl+T or Ctrl+Shift+O: fold or unfold the todo panel.
            env.ui.toggle_todo_collapsed();
            Ok(Flow::Continue)
        }
        AppKey::CtrlShiftB | AppKey::Ctrl('b') => {
            // Ctrl+Shift+B (or Ctrl+B) opens the live background-task output viewer.
            handle_task_viewer(env, st, key_rx).await;
            Ok(Flow::Continue)
        }
        AppKey::Ctrl('v') => {
            // Ctrl+V: attach a screenshot from the clipboard to the next prompt
            paste_clipboard(env, st);
            Ok(Flow::Continue)
        }
        AppKey::Tab => {
            // Tab: toggle plan mode
            toggle_plan_mode(env);
            Ok(Flow::Continue)
        }
        AppKey::BackTab
            if !env
                .allow_all_session
                .load(std::sync::atomic::Ordering::SeqCst) =>
        {
            // Shift+Tab: yolo — mutating tools run without asking for the rest of
            // the chat. Pressing it again while yolo is on does nothing: the same
            // key must not silently flip a safety gate back off.
            enable_yolo(env);
            Ok(Flow::Continue)
        }
        AppKey::Char('/') if input_empty => handle_slash(env, st, key_rx).await,
        AppKey::Up => {
            history_up(env, st);
            Ok(Flow::Continue)
        }
        AppKey::Down => {
            history_down(env, st);
            Ok(Flow::Continue)
        }
        AppKey::Char(c) => {
            env.ui
                .input_buf
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(c);
            env.ui.redraw_input_line();
            Ok(Flow::Continue)
        }
        AppKey::Backspace => {
            env.ui
                .input_buf
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop();
            env.ui.redraw_input_line();
            Ok(Flow::Continue)
        }
        AppKey::Enter => enter::handle(env, st, key_rx).await,
        // Ctrl+C, and the ctrl+c a terminal may still report as a letter:
        // a chat you cannot leave is worse than a stray key.
        AppKey::CtrlC | AppKey::Ctrl('c') => handle_ctrl_c(env, st).await,
        AppKey::Escape => {
            handle_escape(env, st).await;
            Ok(Flow::Continue)
        }
        AppKey::Resize => {
            handle_resize(env, st);
            Ok(Flow::Continue)
        }
        _ => Ok(Flow::Continue),
    }
}

fn handle_ctrl_o(env: &mut Env<'_>, st: &mut ReplState) -> Result<Flow, HarnessError> {
    // While a tool is running, Ctrl+O is about that call: expand its live tail
    // so a long build's earlier output is visible. Idle Ctrl+O keeps folding
    // the transcript's process blocks.
    if env.ui.has_live_activity() {
        env.ui.toggle_live_expanded();
        return Ok(Flow::Continue);
    }
    // Ctrl+O: toggle process expansion if transcript turns exist.
    // In standard terminals where Ctrl+Shift+O arrives as 0x0F (Ctrl+O),
    // fall back to toggling the todo panel when no process turns exist.
    if st.active_turn.is_none() && !st.turns.is_empty() {
        st.process_expanded = !st.process_expanded;
        if env.ui.is_attended {
            crate::commands::clear_screen();
            redraw_transcript(&st.turns, st.process_expanded);
            if let Some(ref err) = env.ui.transient_error() {
                println!();
                println!("{}", err);
            }
            env.ui.draw_initial_box();
        }
    } else if env.ui.has_todo_panel() {
        env.ui.toggle_todo_collapsed();
    }
    Ok(Flow::Continue)
}

async fn handle_task_viewer(env: &mut Env<'_>, st: &mut ReplState, key_rx: &mut KeyRx) {
    show_background_task_viewer(
        key_rx,
        Some(env.bg_queue.as_ref()),
        Some(env.delegate_tool.as_ref()),
        env.store.as_ref(),
        &env.ui,
        &st.turns,
        st.process_expanded,
    )
    .await;
}

fn paste_clipboard(env: &mut Env<'_>, st: &mut ReplState) {
    match crate::commands::clipboard_image::read_clipboard_image() {
        Ok(Some(img)) => {
            let data_url = img.data_url();
            let marker = format!("[image: {}x{}]", img.width, img.height);
            {
                let mut b = env.ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
                if !b.is_empty() && !b.ends_with(' ') {
                    b.push(' ');
                }
                b.push_str(&marker);
                b.push(' ');
            }
            let mut attachments = st.pending_images.lock().unwrap_or_else(|e| e.into_inner());
            attachments.push(data_url);
            env.ui.redraw_input_line();
        }
        Ok(None) => {
            env.ui.emit_above(&format!(
                "{} Clipboard holds no image (nothing to paste)",
                "•".dimmed()
            ));
        }
        Err(e) => {
            env.ui.emit_above(&format!(
                "{} Screenshot paste failed: {}",
                "✖".red().bold(),
                e
            ));
        }
    }
}

fn toggle_plan_mode(env: &mut Env<'_>) {
    let current = env.plan_mode.load(std::sync::atomic::Ordering::SeqCst);
    let next = !current;
    env.plan_mode
        .store(next, std::sync::atomic::Ordering::SeqCst);
    env.ui.set_plan_mode(next);
    env.ui.emit_above(&format!(
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
}

fn enable_yolo(env: &mut Env<'_>) {
    env.allow_all_session
        .store(true, std::sync::atomic::Ordering::SeqCst);
    env.ui.set_yolo(true);
}

async fn handle_slash(
    env: &mut Env<'_>,
    st: &mut ReplState,
    key_rx: &mut KeyRx,
) -> Result<Flow, HarnessError> {
    let skills: &[harness_plugins::Skill] =
        env.skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
    env.ui
        .input_buf
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push('/');
    env.ui.redraw_input_line();
    if let Some(cmd) = select_slash_command(
        key_rx,
        &env.ui,
        &command_entries(
            env.config,
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
        // The menu can name a composer command too — picking `/interrupt`
        // while Luna is thinking has to interrupt — so it goes through the
        // same handler the typed form does.
        command::run_tail(env, st, key_rx, &cmd, &cmd).await?;
    }
    Ok(Flow::Continue)
}

fn history_up(env: &mut Env<'_>, st: &mut ReplState) {
    if !st.prompt_history.is_empty() {
        let next_idx = match st.history_cursor {
            None => {
                st.draft_input = env
                    .ui
                    .input_buf
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                st.prompt_history.len().saturating_sub(1)
            }
            Some(idx) => idx.saturating_sub(1),
        };
        st.history_cursor = Some(next_idx);
        if let Some(prompt) = st.prompt_history.get(next_idx) {
            *env.ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = prompt.clone();
            env.ui.redraw_input_line();
        }
    }
}

fn history_down(env: &mut Env<'_>, st: &mut ReplState) {
    if let Some(idx) = st.history_cursor {
        if idx + 1 < st.prompt_history.len() {
            let next_idx = idx + 1;
            st.history_cursor = Some(next_idx);
            if let Some(prompt) = st.prompt_history.get(next_idx) {
                *env.ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = prompt.clone();
                env.ui.redraw_input_line();
            }
        } else {
            st.history_cursor = None;
            *env.ui.input_buf.lock().unwrap_or_else(|e| e.into_inner()) = st.draft_input.clone();
            env.ui.redraw_input_line();
        }
    }
}

async fn handle_ctrl_c(env: &mut Env<'_>, st: &mut ReplState) -> Result<Flow, HarnessError> {
    let cancelled = cancel_turn(env, st).await;
    if !cancelled {
        env.ui.clear_box_on_exit();
        clear_on_exit();
        return Ok(Flow::Break);
    }
    Ok(Flow::Continue)
}

async fn handle_escape(env: &mut Env<'_>, st: &mut ReplState) {
    let cancelled = cancel_turn(env, st).await;
    if !cancelled {
        // Nothing was running, so Esc is cancelling the draft. Attachments
        // belong to the draft; drop them and their markers together.
        let had_images = {
            let mut guard = st.pending_images.lock().unwrap_or_else(|e| e.into_inner());
            let had = !guard.is_empty();
            guard.clear();
            had
        };
        if had_images {
            let mut b = env.ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
            let cleaned = crate::commands::prompt_images::remove_image_markers(&b);
            *b = cleaned;
            drop(b);
            env.ui.redraw_input_line();
        }
    }
}

async fn cancel_turn(env: &mut Env<'_>, st: &mut ReplState) -> bool {
    let cancelled = cancel_running_turn(
        &mut st.active_turn,
        &st.current_cancel,
        &env.ui,
        &mut st.turns,
        &env.in_flight_tools,
        &env.interruptions,
        &env.interrupt_handle,
        &mut st.queue,
        st.turn_start,
        &st.current_query,
        st.process_expanded,
        &env.agent,
        context_window(
            env.config,
            &env.discovery,
            env.current_provider,
            env.current_model,
        ),
        true,
    )
    .await;
    if cancelled && st.active_goal.take().is_some() {
        env.ui.set_goal_status(None);
        env.ui
            .emit_above(&format!("{} Goal loop stopped.", "•".dimmed()));
    }
    cancelled
}

fn handle_resize(env: &mut Env<'_>, st: &mut ReplState) {
    // Windows reports a console resize as an event rather than SIGWINCH.
    // Reflow everything the same way the signal path above does.
    crate::commands::clear_screen();
    redraw_transcript(&st.turns, st.process_expanded);
    if st.active_turn.is_some() {
        if !st.current_query.trim().is_empty() {
            println!();
            println!(
                "{}",
                right_aligned_user_message(&st.current_query, get_terminal_width())
            );
        }
        let tools = env
            .in_flight_tools
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
    if let Some(ref err) = env.ui.transient_error() {
        println!();
        println!("{}", err);
    }
    env.ui.handle_resize();
}
