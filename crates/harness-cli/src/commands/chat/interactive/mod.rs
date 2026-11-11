//! The attended REPL: one event loop over keys, agent events and background channels.
//!
//! The loop is the only place that owns the terminal. Everything it does is a call into a
//! focused handler in a sibling module, each of which takes the shared [`Env`] and the loop's
//! [`ReplState`].

mod command;
mod enter;
mod keys;
mod startup;
mod state;
mod tick;
mod turn_end;

use harness_core::{HarnessError, SessionStore};

use super::clear_on_exit;
use super::setup::ChatRuntime;
use super::ui::{ANIMATION_INTERVAL, LANDING_FRAME_INTERVAL};
use state::{Env, Flow};

/// Run the attended loop until the user leaves, the key channel closes, or a command asks to exit.
pub(super) async fn run(runtime: &mut ChatRuntime) -> Result<(), HarnessError> {
    let mut env = Env::new(runtime);
    let startup = startup::seed(&mut env).await?;
    let startup::Startup {
        st,
        key_rx,
        _raw_terminal,
        _kitty_guard,
    } = startup;
    let mut st = st;
    let mut key_rx = key_rx;

    #[cfg(unix)]
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok();

    // Drives the live region: streamed reasoning and the running-tool spinner advance on this
    // tick rather than on every delta, so a fast token stream cannot outrun the redraw.
    let mut animation = tokio::time::interval(ANIMATION_INTERVAL);
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // The landing shower moves faster than the spinner, so it gets its own tick. It costs a
    // timer that does nothing once the mark has settled, and in exchange the first screen is
    // alive while the user is reading it instead of ending before the prompt appears.
    let mut landing_animation = tokio::time::interval(LANDING_FRAME_INTERVAL);
    landing_animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = async {
                #[cfg(unix)]
                {
                    match sigwinch.as_mut() {
                        Some(sig) => { sig.recv().await; }
                        None => std::future::pending().await,
                    }
                }
                #[cfg(not(unix))]
                {
                    std::future::pending::<()>().await
                }
            } => {
                // A resize changes how every soft-wrapped line lays out. The live region and
                // input box are redrawn by handle_resize, but the transcript above them was
                // printed at the old width and stays frozen there. Clear and re-render the
                // whole screen so old messages reflow to the new width.
                tick::reflow(&env, &st);
            }

            _ = animation.tick() => {
                tick::animation(&mut env, &mut st);
            }

            _ = landing_animation.tick() => {
                env.ui.animate_landing();
            }

            maybe_approval = env.approval_rx.recv() => {
                tick::approval(&env, &mut key_rx, maybe_approval).await;
            }

            maybe_sudo = env.sudo_rx.recv() => {
                tick::sudo(&env, &mut key_rx, maybe_sudo).await;
            }

            maybe_clarify = env.clarify_rx.recv() => {
                tick::clarify(&env, &mut key_rx, maybe_clarify).await;
            }

            maybe_job = env.job_rx.recv() => {
                tick::job(&mut env, &mut st, maybe_job).await;
            }

            maybe_key = key_rx.recv() => {
                let key = match maybe_key {
                    Some(k) => k,
                    None => break,
                };
                if keys::handle(&mut env, &mut st, &mut key_rx, key).await? == Flow::Break {
                    break;
                }
            }

            turn_res = async {
                if let Some(ref mut handle) = st.active_turn {
                    handle.await
                } else {
                    std::future::pending().await
                }
            } => {
                turn_end::handle(&env, &mut st, turn_res).await;
            }
        }
    }

    {
        let ag = env.agent.lock().await;
        if let Err(error) = ag
            .emit_hook_event(harness_core::HookEvent::SessionEnd, None)
            .await
        {
            tracing::warn!("session_end hook failed: {}", error);
        }
    }

    clear_on_exit();
    // The screen is wiped on exit, so leave behind just the line that gets the user back in.
    // Both id and name are shown: the name is what the user recognises, the id is the
    // unambiguous handle, and `--session` accepts either.
    let session_name = env
        .store
        .get(env.session_id)
        .await
        .map(|s| s.name)
        .unwrap_or_default();
    if session_name.is_empty() {
        println!("Resume: luna --session {}", env.session_id);
    } else {
        println!(
            "Resume: luna --session {}    (name: {})",
            env.session_id, session_name
        );
    }
    Ok(())
}
