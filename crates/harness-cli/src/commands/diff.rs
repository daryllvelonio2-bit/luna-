//! `luna diff`: what the last turn (or a named one) actually changed on disk.
//!
//! `/rewind` puts files back; this answers the question that comes first — *what did it do?* The
//! checkpoints already hold the answer (each turn snapshots the files it is about to touch), and
//! this command diffs that snapshot against the workspace as it stands now. It reads only: asking
//! is always safe, including for a turn you are about to rewind.
//!
//! ```bash
//! luna diff                                  # the newest turn of the newest session
//! luna diff --turn 3                         # one turn
//! luna diff --session s1a2b --stat           # just the stat lines
//! luna diff --json | jq '.turns[0].summary'  # machine-readable
//! ```

use colored::Colorize;
use serde_json::json;

use harness_core::checkpoint::{CheckpointStore, TurnDiff};
use harness_core::{Config, HarnessError, SessionStore, SqliteSessionStore};

use super::sessions::find_by_name;

pub struct DiffOptions {
    /// Session id or name. `None` means the most recently updated session.
    pub session: Option<String>,
    /// Turn number. `None` means the newest checkpointed turn.
    pub turn: Option<u32>,
    /// One JSON object on stdout.
    pub json: bool,
    /// Counts only, no hunks.
    pub stat: bool,
    /// Every checkpointed turn of the session, not just one.
    pub all: bool,
}

pub async fn run_diff(config: &Config, opts: DiffOptions) -> Result<(), HarnessError> {
    let store = CheckpointStore::new(
        config.paths.checkpoints_dir.clone(),
        config.paths.workspace_root.clone(),
    );

    let Some(session_id) = resolve_session_id(config, opts.session.clone()).await? else {
        println!(
            "{}",
            "No sessions yet — nothing to diff. Run `luna chat` or `luna exec` first.".dimmed()
        );
        return Ok(());
    };

    let checkpoints = store.list(&session_id)?;
    if checkpoints.is_empty() {
        if opts.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "session": session_id,
                    "turns": [],
                    "note": "no checkpoints for this session",
                }))?
            );
        } else {
            println!(
                "{} No checkpoints for session {} — {}",
                "•".cyan(),
                session_id.bold(),
                if config.checkpoints.enabled {
                    "no turn has written a file yet".dimmed()
                } else {
                    "checkpoints are off (checkpoints.enabled = false)".dimmed()
                }
            );
        }
        return Ok(());
    }

    let turns: Vec<TurnDiff> = if opts.all {
        store.diff_all(&session_id)?
    } else {
        let seq = match opts.turn {
            Some(seq) => seq,
            None => checkpoints
                .last()
                .map(|checkpoint| checkpoint.seq)
                .unwrap_or(1),
        };
        if !checkpoints.iter().any(|checkpoint| checkpoint.seq == seq) {
            return Err(HarnessError::ConfigurationError(format!(
                "no turn {seq} in session {session_id} (have turns {})",
                checkpoints
                    .iter()
                    .map(|checkpoint| checkpoint.seq.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        vec![store.diff(&session_id, seq)?]
    };

    if opts.json {
        // `summary` is derived (it is a function of `files`), so it is added here rather than
        // stored: a consumer that only wants counts should not have to re-derive them, and a
        // consumer that wants hunks gets the same object.
        let turns: Vec<serde_json::Value> = turns
            .iter()
            .map(|turn| {
                json!({
                    "seq": turn.seq,
                    "session_id": turn.session_id,
                    "created_at": turn.created_at,
                    "prompt": turn.prompt,
                    "summary": turn.summary(),
                    "files": turn.files,
                    "unrecorded": turn.unrecorded,
                })
            })
            .collect();
        let payload = json!({
            "session": session_id,
            "workspace": config.paths.workspace_root.display().to_string(),
            "turns": turns,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let mut any_change = false;
    for turn in &turns {
        if print_turn(turn, opts.stat)? {
            any_change = true;
        }
    }
    if !any_change {
        println!(
            "{}",
            "The workspace is byte-identical to the start of those turns — nothing changed."
                .dimmed()
        );
    }
    Ok(())
}

/// Print one turn; returns whether it had any changed file.
fn print_turn(turn: &TurnDiff, stat_only: bool) -> Result<bool, HarnessError> {
    let summary = turn.summary();
    let prompt = turn
        .prompt
        .as_deref()
        .map(|prompt| prompt.replace('\n', " "))
        .map(|prompt| format!(" — {}", prompt.dimmed()))
        .unwrap_or_default();

    println!(
        "{}  {}{}",
        format!("turn {}", turn.seq).bold(),
        summary.stat_line().cyan(),
        prompt
    );
    if let Some(created_at) = timestamps(&turn.created_at) {
        println!("  {}", created_at.dimmed());
    }

    let mut changed = false;
    for file in &turn.files {
        let kind = file.kind.as_str();
        let label = match file.kind {
            harness_core::checkpoint::ChangeKind::Created => kind.green(),
            harness_core::checkpoint::ChangeKind::Modified => kind.yellow(),
            harness_core::checkpoint::ChangeKind::Deleted => kind.red(),
            harness_core::checkpoint::ChangeKind::Unchanged => kind.dimmed(),
        };
        println!(
            "  {} {:<9} {} {}",
            file.kind.marker(),
            label,
            file.path.bold(),
            format!("+{} -{}", file.additions, file.deletions).dimmed()
        );
        if file.kind == harness_core::checkpoint::ChangeKind::Unchanged {
            continue;
        }
        changed = true;
        if let Some(note) = &file.note {
            println!("      {}", note.dimmed());
        }
        if !stat_only && !file.diff.is_empty() {
            for line in file.diff.trim_end().lines() {
                println!("      {}", paint_hunk(line));
            }
        }
    }

    if let Some(total) = totals_line(&summary) {
        println!("  {}", total.dimmed());
    }
    if !turn.unrecorded.is_empty() {
        // Named, never hidden: these are the files a rewind cannot put back, and a review that
        // stayed quiet about them would be worse than no review.
        changed = true;
        println!(
            "  {}",
            format!(
                "! {} file(s) changed by a shell command — no snapshot, `luna rewind` cannot undo these:",
                turn.unrecorded.len()
            )
            .yellow()
        );
        for file in &turn.unrecorded {
            println!("      {:<9} {}", file.kind.as_str(), file.path.dimmed());
        }
    }
    println!();
    Ok(changed)
}

/// Colour a unified-diff line by its role, leaving headers readable rather than loud.
fn paint_hunk(line: &str) -> colored::ColoredString {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
        line.cyan()
    } else if line.starts_with('+') {
        line.green()
    } else if line.starts_with('-') {
        line.red()
    } else {
        line.normal()
    }
}

fn totals_line(summary: &harness_core::checkpoint::DiffSummary) -> Option<String> {
    if summary.additions == 0 && summary.deletions == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if summary.additions > 0 {
        parts.push(format!("+{} lines", summary.additions));
    }
    if summary.deletions > 0 {
        parts.push(format!("-{} lines", summary.deletions));
    }
    Some(parts.join(", "))
}

/// `created_at` is RFC 3339; show the clock part rather than the whole stamp.
fn timestamps(created_at: &str) -> Option<String> {
    if created_at.is_empty() {
        return None;
    }
    created_at
        .split_once('T')
        .map(|(_, rest)| rest.trim_end_matches('Z').to_string())
}

async fn resolve_session_id(
    config: &Config,
    requested: Option<String>,
) -> Result<Option<String>, HarnessError> {
    let store = SqliteSessionStore::new(Some(config.paths.sessions_db.clone()))?;
    let Some(requested) = requested else {
        return store.most_recent().await;
    };
    if store.get(&requested).await.is_ok() {
        return Ok(Some(requested));
    }
    // A session may be named rather than id'd, which is how `--session luna` works elsewhere.
    if let Some(id) = find_by_name(&store, &requested).await? {
        return Ok(Some(id));
    }
    Err(HarnessError::ConfigurationError(format!(
        "no session '{requested}' (see `luna sessions list`)"
    )))
}
