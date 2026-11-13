//! Opening the attended screen: replay the session, seed the status line, start the key reader.

use colored::Colorize;
use dialoguer::console::Term;
use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, SessionStore, TurnOutcome};

use super::super::helpers::{redraw_transcript, IdleMemoryPass};
use super::super::history::{load_prompt_history, KittyKeyboardGuard};
use super::super::keys;
use super::super::keys::AppKey;
use super::super::render::{parse_turns_from_history, TurnRecord};
use super::super::status::refresh_antigravity_status_if_needed;
use super::super::todo;
use super::super::ui::RawTerminal;
use super::state::{Env, KeyRx, ReplState};
use crate::commands::context_window::context_window;

/// The screen and channels a seeded session hands to the event loop.
///
/// The two guards are held for exactly as long as the loop runs: dropping the raw-mode guard
/// restores the terminal on every exit path, and the kitty guard gives the encoding back.
pub(super) struct Startup {
    pub(super) st: ReplState,
    pub(super) key_rx: KeyRx,
    pub(super) _raw_terminal: Option<RawTerminal>,
    pub(super) _kitty_guard: Option<KittyKeyboardGuard>,
}

/// Replay the transcript, draw the first frame, and start the key reader thread.
/// Replay the transcript, draw the first frame, and start the key reader thread.
pub(super) async fn seed(env: &mut Env<'_>) -> Result<Startup, HarnessError> {
    // Start from a clean screen: launching the REPL shouldn't leave whatever the shell was
    // showing above the input box. Attended-only by construction — the non-attended path
    // returned above and `clear_screen` no-ops on a piped stdout anyway.
    crate::commands::clear_screen();

    let (turns, replayed_count) = replay_session(env).await?;

    // Warm the model catalog before the first status line is drawn.
    warm_model_catalog(env);

    seed_status(env).await;

    // Images pasted via Ctrl+V, waiting to ride the next prompt as data URLs.
    let pending_images: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    // A resumed session carries its list: the panel is drawn from the start, above
    // the composer, so the tasks are in front of the user before the first prompt.
    todo::refresh_panel(&env.ui, &env.todo_scope);

    // A brand-new session has no transcript, so the moon is the only thing anchoring the screen.
    // A resumed one already has its conversation; repeating the landing there would push it up
    // for no reason.
    if replayed_count == 0 {
        env.ui.draw_landing();
    }

    env.ui.draw_initial_box();

    let (key_tx, key_rx) = mpsc::unbounded_channel::<AppKey>();

    // Raw mode belongs to the session, not to each read — see `RawTerminal`. Held here so the
    // guard drops (and the terminal is restored) when this function returns on any exit path.
    let _raw_terminal = RawTerminal::acquire();

    let _kitty_guard = enable_kitty_keyboard_if_attended();

    install_key_readers(key_tx);

    let st = build_repl_state(env, turns, pending_images);

    Ok(Startup {
        st,
        key_rx,
        _raw_terminal,
        _kitty_guard,
    })
}

/// Print the stored transcript before the box so a resumed session reopens with its history
/// on screen — without it a continuation is indistinguishable from a fresh session until the
/// first reply lands.
async fn replay_session(env: &mut Env<'_>) -> Result<(Vec<TurnRecord>, usize), HarnessError> {
    let replayed = env
        .store
        .replay_transcript(
            env.session_id.as_str(),
            env.config.session.resume_max_messages,
        )
        .await?;
    let turns: Vec<TurnRecord> = parse_turns_from_history(&replayed.history);
    let process_expanded = false;

    if replayed.replayed > 0 {
        if replayed.truncated() {
            println!(
                "{}\n",
                format!(
                    "… {} earlier messages not shown ({} shown)",
                    replayed.total - replayed.replayed,
                    replayed.replayed
                )
                .dimmed()
            );
        }
        redraw_transcript(&turns, process_expanded);
        println!();
    }

    Ok((turns, replayed.replayed))
}

/// Warm the model catalog in the background before the first status line is drawn: the windows
/// the provider publishes live in it, and the gauge reads them from there on every turn. It is
/// detached on purpose — a slow or dead `/models` endpoint must never hold up a session, and
/// each answer it does give replaces the catalog's own guess with the provider's measurement.
fn warm_model_catalog(env: &Env<'_>) {
    let discovery = env.discovery.clone();
    let cred_manager = env.cred_manager.clone();
    let provider = env.current_provider.clone();
    tokio::spawn(async move {
        if let Err(error) = discovery.get_models(&provider, &cred_manager, false).await {
            tracing::debug!(
                "model catalog warm-up for '{}' did not run: {}",
                provider,
                error
            );
        }
    });
}

/// Seed the status bar from the real transcript, not 0: a resumed session already carries history,
/// and reporting "0 used" made the continuation look like a fresh start.
async fn seed_status(env: &Env<'_>) {
    let context_max = context_window(
        env.config,
        env.discovery.as_ref(),
        env.current_provider.as_str(),
        env.current_model.as_str(),
    );
    let context_used = env
        .agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    env.ui
        .set_status(env.current_model.as_str(), context_used, context_max);
    refresh_antigravity_status_if_needed(
        &env.ui,
        env.current_provider.as_str(),
        env.current_model.as_str(),
        &env.cred_manager,
    )
    .await;
    // A resumed named session shows its name centered in the status bar from the first frame.
    let stored_name = env
        .store
        .get(env.session_id.as_str())
        .await
        .map(|s| s.name)
        .unwrap_or_default();
    env.ui.set_session_name(&stored_name);
}

/// Ask the terminal for unambiguous key codes. Without it ctrl+shift+o arrives as the
/// same byte as ctrl+o, and the todo panel could never have a key of its own.
///
/// We enable the protocol when either the console crate thinks we're attended,
/// or when KITTY_WINDOW_ID is set (a reliable indicator of running in kitty).
/// The latter handles cases where the tool is invoked with redirected stdio but
/// the controlling terminal is still kitty.
fn enable_kitty_keyboard_if_attended() -> Option<KittyKeyboardGuard> {
    let in_kitty = std::env::var("KITTY_WINDOW_ID").is_ok();
    if Term::stdout().features().is_attended() || in_kitty {
        keys::enable_kitty_keyboard();
        Some(KittyKeyboardGuard)
    } else {
        None
    }
}

/// Route ctrl-c and terminal keys into the key channel.
fn install_key_readers(key_tx: mpsc::UnboundedSender<AppKey>) {
    // Ctrl-C stays a signal because raw mode is entered with ISIG left on. Left to its default
    // action it would kill the process before the quit path runs and the resume hint never
    // prints; funnelling the signal into the key channel gives it the same path as `exit`.
    {
        let key_tx = key_tx.clone();
        tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                if key_tx.send(AppKey::CtrlC).is_err() {
                    break;
                }
            }
        });
    }

    std::thread::spawn(move || loop {
        match keys::read_key() {
            Ok(key) => {
                if key_tx.send(key).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // SIGINT or other signal interrupted read syscall - do not terminate reader
                // thread. A pause (another prompt owns stdin) also reports Interrupted, so
                // back off briefly instead of spinning while it holds the terminal.
                if keys::input_paused() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                continue;
            }
            Err(_) => {
                if key_tx.is_closed() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    });
}

/// Build the loop-local state a freshly seeded session starts with.
fn build_repl_state(
    env: &Env<'_>,
    turns: Vec<TurnRecord>,
    pending_images: Arc<std::sync::Mutex<Vec<String>>>,
) -> ReplState {
    let queue: VecDeque<String> = VecDeque::new();
    let current_cancel = CancellationToken::new();
    let active_turn: Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>> = None;
    let current_query = String::new();
    let turn_start = std::time::Instant::now();

    let history_path = env
        .config
        .paths
        .sessions_db
        .parent()
        .map(|p| p.join("history.txt"));
    let prompt_history = load_prompt_history(history_path.as_deref(), &turns);
    let history_cursor: Option<usize> = None;
    let draft_input = String::new();

    // A quota readout fetched at start-up and never fetched again is a snapshot: its countdown
    // stops moving the moment it is drawn, and its percentages describe the account as it was when
    // the session opened. The tick is what keeps it honest — the clock half is repainted whenever a
    // minute of the countdown passes, and the numbers themselves are refetched on this TTL.
    let quota_refreshed_at = std::time::Instant::now();

    // The idle memory pass: a short session may end before `background_every_turns` fires, so the
    // tick notices a quiet, finished conversation and runs one pass over it.
    let idle_pass = IdleMemoryPass::new(turns.len());

    // A background job that finished while the composer was busy. The tick starts the reacting
    // turn as soon as the composer is genuinely idle; a turn that started after this instant
    // already replayed the stored completion, so it clears the flag instead. The session id is
    // part of the key: the completion was stored in *that* session, and resuming after a switch
    // would start a turn with no job context.
    let pending_job_turn: Option<(String, std::time::Instant)> = None;

    ReplState {
        turns,
        process_expanded: false,
        queue,
        current_cancel,
        active_turn,
        active_goal: None,
        current_query,
        turn_start,
        prompt_history,
        history_cursor,
        draft_input,
        quota_refreshed_at,
        idle_pass,
        pending_job_turn,
        pending_images,
        history_path,
    }
}
