//! The slash commands about the transcript rather than the conversation.
//!
//! `/diff` shows what the last turn changed; `/compact` and `/auto-compact` decide when the
//! transcript is folded. They are grouped because all three are read off the same history the
//! turn wrote.

use super::value_prompt::{prompt_value, PERCENT_PROMPT, THRESHOLD_PROMPT};
use super::*;

/// `/diff [turn|--all]`: what the workspace actually changed, read from the turn's checkpoint.
///
/// `/rewind` is a write. This is the read that belongs before it: the same snapshot, diffed against
/// the files as they are now, so a person can see what a turn did before deciding to undo it.
pub(crate) async fn handle_diff_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let controller = {
        let agent = agent.lock().await;
        agent.checkpoints()
    };
    let Some(controller) = controller.filter(|controller| controller.enabled()) else {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    };

    let store = controller.store();
    let checkpoints = store.list(session_id)?;
    if checkpoints.is_empty() {
        ui.emit_above(&format!(
            "{} No checkpoints yet — the next turn that writes a file creates one",
            "•".cyan()
        ));
        return Ok(());
    }

    let raw = arg.unwrap_or("").trim();
    let all = raw == "--all" || raw == "all";
    let seq = if all {
        None
    } else {
        match raw.split_whitespace().next().map(str::parse::<u32>) {
            Some(Ok(seq)) => Some(seq),
            _ => checkpoints.last().map(|checkpoint| checkpoint.seq),
        }
    };

    let turns = match seq {
        Some(seq) => vec![store.diff(session_id, seq)?],
        None => store.diff_all(session_id)?,
    };

    for turn in &turns {
        let summary = turn.summary();
        let prompt = turn
            .prompt
            .as_deref()
            .map(|prompt| prompt.replace('\n', " "))
            .map(|prompt| format!(" — {}", prompt.dimmed()))
            .unwrap_or_default();
        ui.emit_above(&format!(
            "{}  {}{}",
            format!("turn {}", turn.seq).bold(),
            summary.stat_line().cyan(),
            prompt
        ));

        if turn.is_empty() && turn.unrecorded.is_empty() {
            ui.emit_above(&format!(
                "    {}",
                "nothing changed (the workspace matches this turn's snapshot)".dimmed()
            ));
            continue;
        }

        for file in turn.changed() {
            ui.emit_above(&format!(
                "    {} {:<9} {} {}",
                file.kind.marker(),
                file.kind.as_str().yellow(),
                file.path.bold(),
                format!("+{} -{}", file.additions, file.deletions).dimmed()
            ));
            if let Some(note) = &file.note {
                ui.emit_above(&format!("        {}", note.dimmed()));
            }
            // Hunks are the point of a diff, but a chat pane is not a pager: cap them and say so
            // rather than letting one regenerated lockfile scroll the conversation away.
            let mut budget = DIFF_HUNK_BUDGET;
            for line in file.diff.trim_end().lines() {
                if budget == 0 {
                    ui.emit_above(&format!(
                        "        {}",
                        "… more hunks not shown in chat — pipe `luna diff` for all of them"
                            .dimmed()
                    ));
                    break;
                }
                budget -= 1;
                let painted =
                    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@")
                    {
                        line.cyan()
                    } else if line.starts_with('+') {
                        line.green()
                    } else if line.starts_with('-') {
                        line.red()
                    } else {
                        line.normal()
                    };
                ui.emit_above(&format!("        {}", painted));
            }
        }
        if !turn.unrecorded.is_empty() {
            ui.emit_above(&format!(
                "    {}",
                format!(
                    "! {} file(s) changed by a shell command — no snapshot, /rewind cannot undo these:",
                    turn.unrecorded.len()
                )
                .yellow()
            ));
            for file in &turn.unrecorded {
                ui.emit_above(&format!(
                    "      {:<9} {}",
                    file.kind.as_str(),
                    file.path.dimmed()
                ));
            }
        }
        ui.emit_above("");
    }

    Ok(())
}

/// How many diff lines a chat reply shows per turned file.
pub(super) const DIFF_HUNK_BUDGET: usize = 40;

/// `/compact`: fold the current session's oldest turns into a summary in place — the session
/// keeps its id and name, only the context shrinks.
///
/// The fold runs as its own task and reports through the agent's compaction events, so the
/// composer stays live and animates while the summary call is in flight. The agent lock is what
/// arbitrates: a turn holds it for its whole run, so a fold asked for mid-turn is refused instead
/// of being silently parked behind the turn.
pub(crate) async fn handle_compact_command(
    ui: &ChatUi,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    current_provider: &str,
    current_model: &str,
) -> Result<(), HarnessError> {
    let ui = ui.clone();
    let agent = agent.clone();
    // Resolved here, on the caller's thread: the fold runs in a task that has no config, and the
    // answer is the same before and after it (the window belongs to the model, not the fold).
    let context_max = context_window_for(config, current_provider, current_model);
    tokio::spawn(async move {
        let Ok(ag) = agent.try_lock() else {
            ui.emit_above(&format!(
                "{} Cannot compact while a turn is running: interrupt it (send a message or press Escape), then run /compact",
                "✖".red().bold()
            ));
            return;
        };
        // Everything that happens inside the fold — the animation, the report, the failure — is
        // emitted as compaction events, so this task only refreshes the gauge afterwards.
        if ag.compact_current_session().await.is_ok() {
            // The summary replaced the folded turns, so the gauge drops for real.
            let context_used = ag.estimate_session_tokens().await.unwrap_or(0);
            ui.update_status(context_used, context_max);
        }
    });

    Ok(())
}

/// `/auto-compact`: open the setup box — the threshold, then the turns — and turn the feature on.
///
/// The two values used to be arguments (`/auto-compact set threshold 200k`). They are asked for in
/// a box instead, the way the approval gate asks for a verdict, so the bounds, an example and the
/// current value are in front of the user while they type. `on` and `off` stay as arguments: a
/// toggle has no value to choose, and a box for it would be a box for nothing.
///
/// `key_rx` is the composer's key channel, so the wizard reads the same keys the chat does. A host
/// with no composer (piped stdin) passes `None` and gets the settings printed instead of a prompt
/// nobody could type into.
pub(crate) async fn handle_auto_compact_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
) -> Result<(), HarnessError> {
    let arg = arg.unwrap_or("").trim();

    let layers = harness_core::config::process_layers(Config::default())?;

    if arg.eq_ignore_ascii_case("on") || arg.eq_ignore_ascii_case("enable") {
        {
            let mut ag = agent.lock().await;
            ag.set_auto_compact_enabled(true);
        }
        let _ = harness_core::config::set_in_file(&layers, "auto_compact_enabled", "true")?;
        let (threshold, percent) = {
            let ag = agent.lock().await;
            let (_, th, pct) = ag.auto_compact_settings();
            (th, pct)
        };
        ui.emit_above(&format!(
            "{} Auto-compact enabled (threshold: {} tokens, compact: {}%)",
            "✓".green().bold(),
            threshold,
            percent
        ));
        return Ok(());
    }

    if arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("disable") {
        {
            let mut ag = agent.lock().await;
            ag.set_auto_compact_enabled(false);
        }
        let _ = harness_core::config::set_in_file(&layers, "auto_compact_enabled", "false")?;
        ui.emit_above(&format!("{} Auto-compact disabled", "✓".green().bold()));
        return Ok(());
    }

    // No composer to type into: report the settings and the two arguments this host can use,
    // rather than opening a box that would read keys from a channel no one is writing to.
    let Some(key_rx) = key_rx else {
        let (enabled, threshold, percent) = {
            let ag = agent.lock().await;
            ag.auto_compact_settings()
        };
        let status = if enabled {
            "enabled".green().bold()
        } else {
            "disabled".dimmed()
        };
        ui.emit_above(&format!(
            "Auto-compact is {} (threshold: {} tokens, compact: {}%)",
            status, threshold, percent
        ));
        ui.emit_above(
            "Usage: /auto-compact opens the setup box in an interactive chat; here it also takes on | off",
        );
        return Ok(());
    };

    if !arg.is_empty() {
        ui.emit_above(&format!(
            "{} Unknown auto-compact option '{}' — {} opens the setup box, {} and {} toggle it",
            "✖".red().bold(),
            arg,
            "/auto-compact".bold(),
            "on".bold(),
            "off".bold()
        ));
        return Ok(());
    }

    let (_, current_threshold, current_percent) = {
        let ag = agent.lock().await;
        ag.auto_compact_settings()
    };

    // Two boxes, one value each. Esc in either leaves the config exactly as it was: a half
    // answered wizard must not write a threshold the user never confirmed.
    let Some(threshold) = prompt_value(
        key_rx,
        &THRESHOLD_PROMPT,
        current_threshold,
        harness_onboarding::compact_step::parse_threshold,
        |_text, _val| None,
    )
    .await
    else {
        ui.emit_above(&format!("{} Auto-compact unchanged", "•".dimmed()));
        return Ok(());
    };

    let Some(percent) = prompt_value(
        key_rx,
        &PERCENT_PROMPT,
        current_percent,
        harness_onboarding::compact_step::parse_percent,
        |_text, pct| {
            if pct <= 10 {
                Some(format!(
                    "{} ≤{}%: frees very little context; will quickly hit threshold again",
                    "⚠".yellow().bold(),
                    pct
                ))
            } else if pct <= 60 {
                Some(format!(
                    "{} {}%: good balance of history retention and token headroom",
                    "✓".green().bold(),
                    pct
                ))
            } else {
                Some(format!(
                    "{} {}%: aggressive compaction, clears majority of context",
                    "⚡".cyan().bold(),
                    pct
                ))
            }
        },
    )
    .await
    else {
        ui.emit_above(&format!("{} Auto-compact unchanged", "•".dimmed()));
        return Ok(());
    };

    {
        let mut ag = agent.lock().await;
        ag.set_auto_compact_threshold(threshold);
        ag.set_auto_compact_percent(percent);
        ag.set_auto_compact_enabled(true);
    }
    let _ = harness_core::config::set_in_file(&layers, "auto_compact_enabled", "true")?;
    let _ = harness_core::config::set_in_file(
        &layers,
        "auto_compact_threshold",
        &threshold.to_string(),
    )?;
    let _ =
        harness_core::config::set_in_file(&layers, "auto_compact_percent", &percent.to_string())?;

    ui.emit_above(&format!(
        "{} Auto-compact enabled (threshold: {} tokens, compact: {}%)",
        "✓".green().bold(),
        threshold,
        percent
    ));
    Ok(())
}
