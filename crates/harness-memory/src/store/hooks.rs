//! Lifecycle hooks and instructions for the memory store.
//! Implements semantic hooks: session_start, stop, session_end, and precompact,
//! plus instructions and shared brain agent rules.

use std::fs;
use std::path::{Path, PathBuf};

use harness_core::HarnessError;
use serde_json::{json, Value};
use tracing::warn;

use super::MemoryStore;

pub const DEFAULT_SAVE_INTERVAL: usize = 10;

/// Serialize a hook reply. The hook contract is JSON on stdout, so a serialization failure is a
/// hook failure rather than an empty reply the caller could mistake for success.
fn json_reply(value: Value) -> Result<String, HarnessError> {
    serde_json::to_string_pretty(&value)
        .map_err(|error| HarnessError::SerializationError(error.to_string()))
}

pub fn hook_state_dir(root: &Path) -> PathBuf {
    root.join("hooks_state")
}

pub fn run_hook(
    hook_name: &str,
    harness: &str,
    input_json: &str,
    store: &MemoryStore,
) -> Result<String, HarnessError> {
    let input: Value = serde_json::from_str(input_json).unwrap_or(json!({}));
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("default-session")
        .to_string();
    let transcript_path = input
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(PathBuf::from);

    let state_dir = hook_state_dir(store.root());
    fs::create_dir_all(&state_dir).map_err(|e| HarnessError::Io(e.to_string()))?;

    let normalized = hook_name.replace('_', "-");
    if !store.config.hooks_auto_save
        && matches!(normalized.as_str(), "stop" | "session-end" | "precompact")
    {
        let res = json!({
            "decision": "disabled",
            "reason": "memory.store.hooks_auto_save is false",
            "session_id": session_id,
            "success": true
        });
        return json_reply(res);
    }
    match normalized.as_str() {
        "session-start" => {
            let active = store.active_records()?;
            let context_block = store.context_stack().wake_up(&active, None);

            let res = json!({
                "session_id": session_id,
                "harness": harness,
                "system_prompt_addition": context_block,
                "success": true
            });
            json_reply(res)
        }
        "stop" => {
            let last_save_file = state_dir.join(format!("{session_id}_last_save"));
            let last_save: usize = fs::read_to_string(&last_save_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);

            let current_count = if let Some(path) = transcript_path.as_deref() {
                count_messages_in_file(path)
            } else {
                input
                    .get("exchange_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize
            };

            let since = current_count.saturating_sub(last_save);
            let interval = store.config.hooks_save_interval.max(1);
            if since >= interval && current_count > 0 {
                match save_transcript(store, &session_id, transcript_path.as_deref())? {
                    Some(report) => {
                        fs::write(&last_save_file, format!("{current_count}\n"))
                            .map_err(|e| HarnessError::Io(e.to_string()))?;
                        let res = json!({
                            "decision": "saved",
                            "session_id": session_id,
                            "exchange_count": current_count,
                            "messages_since_save": since,
                            "files_mined": report.files,
                            "records_written": report.records,
                            "success": true
                        });
                        json_reply(res)
                    }
                    None => {
                        let res = json!({
                            "decision": "pass",
                            "reason": "no readable transcript",
                            "session_id": session_id,
                            "exchange_count": current_count,
                            "messages_since_save": since,
                            "success": true
                        });
                        json_reply(res)
                    }
                }
            } else {
                let res = json!({
                    "decision": "pass",
                    "session_id": session_id,
                    "exchange_count": current_count,
                    "messages_since_save": since,
                    "success": true
                });
                json_reply(res)
            }
        }
        "session-end" => {
            let saved = save_transcript(store, &session_id, transcript_path.as_deref())?;
            if let Err(error) = store.journal_write(
                "luna",
                &format!("Session {session_id} ended cleanly."),
                None,
            ) {
                warn!("could not write the session-end journal entry: {error}");
            }
            let last_save_file = state_dir.join(format!("{session_id}_last_save"));
            // Best effort: the marker may already be gone, and its absence is the desired state.
            if let Err(error) = fs::remove_file(last_save_file) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    warn!("could not clear the session save marker: {error}");
                }
            }

            let res = json!({
                "decision": if saved.is_some() { "session_saved" } else { "session_ended" },
                "session_id": session_id,
                "records_written": saved.as_ref().map(|r| r.records).unwrap_or(0),
                "success": true
            });
            json_reply(res)
        }
        "precompact" => {
            let saved = save_transcript(store, &session_id, transcript_path.as_deref())?;
            if let Err(error) = store.reindex() {
                warn!("precompact reindex failed: {error}");
            }
            let res = json!({
                "decision": if saved.is_some() { "precompact_mined" } else { "precompact_reindexed" },
                "session_id": session_id,
                "records_written": saved.as_ref().map(|r| r.records).unwrap_or(0),
                "success": true
            });
            json_reply(res)
        }
        other => Err(HarnessError::MemoryError(format!("unknown hook: {other}"))),
    }
}

/// Mine a session transcript into the store. Returns `None` when there is no
/// readable transcript or it produced no records, so the hook never reports a
/// save that did not happen.
fn save_transcript(
    store: &MemoryStore,
    session_id: &str,
    transcript_path: Option<&Path>,
) -> Result<Option<super::miner::MineReport>, HarnessError> {
    let Some(path) = transcript_path else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let topic = format!("session-{}", super::ids::slugify(session_id));
    let report = super::convo::mine_convos(store, path, Some("conversations"), Some(&topic))?;
    if report.records == 0 {
        Ok(None)
    } else {
        Ok(Some(report))
    }
}

fn count_messages_in_file(path: &Path) -> usize {
    if !path.is_file() {
        return 0;
    }
    let Ok(content) = fs::read_to_string(path) else {
        return 0;
    };
    let mut count = 0;
    for line in content.lines() {
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            if let Some(msg) = val.get("message") {
                if msg.get("role").and_then(Value::as_str) == Some("user") {
                    count += 1;
                }
            } else if val.get("type").and_then(Value::as_str) == Some("event_msg") {
                if let Some(payload) = val.get("payload") {
                    if payload.get("type").and_then(Value::as_str) == Some("user_message") {
                        count += 1;
                    }
                }
            } else if val.get("role").and_then(Value::as_str) == Some("user") {
                count += 1;
            }
        }
    }
    count
}

pub fn run_instructions(name: &str) -> Result<String, HarnessError> {
    match name {
        "init" => Ok(r#"# Luna Memory Init

Initialize the local Markdown memory store layout.

1. Creates canonical directory hierarchy:
   - domains/
   - links/
   - crosslinks/
   - graph/entities/
   - graph/triples/
   - journal/
   - events/
   - artifacts/
   - summaries/
   - search_index/
2. Writes initial identity.md, store.md, and SUMMARY.md.
3. Sets up SQLite daemon queue and permissions."#
            .to_string()),
        "search" => Ok(r#"# Luna Memory Search

Search memory records using hybrid BM25 lexical + MiniLM vector ranking.

1. Provide search query text.
2. Optionally specify --domain and --topic filters.
3. Set --limit (default 8) and --since timestamp if needed.
4. Results return structured scores, excerpts, and matched entity pointers."#
            .to_string()),
        "mine" => Ok(r#"# Luna Memory Mine

Extract durable facts, architecture, rules, and notes from codebase or logs.

Modes:
- projects: mines codefiles, docs, configs into domain records
- convos: mines Claude JSONL conversation logs
- sessions: mines sessions.db chat database into memory"#
            .to_string()),
        "status" => Ok(r#"# Luna Memory Status

Overview of current store state:
- Total records count and active records
- Domains and topics breakdown
- Active embedder model
- Search index freshness and knowledge graph stats"#
            .to_string()),
        "audit" => Ok(r#"# Luna Memory Audit

Verifies structural integrity of the memory store:
- Invalid front matter checks
- Unreadable files
- Empty body detection
- Dangling supersessions
- Duplicate active (subject, predicate) records"#
            .to_string()),
        "help" => Ok(r#"# Luna Memory Help

Commands:
- luna memory init
- luna memory status
- luna memory mine <dir>
- luna memory search <query>
- luna memory daemon start|stop|status|jobs|wait
- luna memory mcp [--stdio|--http]
- luna memory serve [--host H --port P]
- luna memory hook run <event>
- luna memory instructions <name>
- luna memory rules"#
            .to_string()),
        other => Err(HarnessError::MemoryError(format!(
            "unknown instructions topic: {other}"
        ))),
    }
}

pub fn render_shared_brain_rules(
    host: &str,
    harness: &str,
    project: &str,
    mcp: &str,
) -> Result<String, HarnessError> {
    let host_clean = host.trim().to_ascii_lowercase();
    let harness_clean = harness.trim().to_ascii_lowercase();
    let project_clean = project.trim().to_ascii_lowercase();

    let raw = r#"## Luna shared brain

You share a Luna memory hub with other agents. Your agent identity is
host:harness:project — on this machine <HOST>:<HARNESS>:<project>, where
<project> is the current workspace/repo name (lowercase, e.g.
<HOST>:<HARNESS>:<PROJECT>). Use that composed identity as
from_agent/created_by in every Luna memory call. Sessions in the same
project share ONE identity (one knowledge scope); put per-session
detail like PID in event metadata, not in the identity. Never
impersonate another agent. Never mint a second harness suffix to split
windows — parallel lanes use topic, not a forged identity.

Memory (recall + writing):
- Before answering about past work, decisions, people, or projects,
  search memory (memory_search; memory_kg_query for
  relational/temporal facts). Quote results verbatim — never paraphrase
  stored content. If memory has nothing, say so; don't guess.
- File durable outcomes (decisions, conclusions, learned facts) with
  memory_add_record. New KG facts: memory_kg_add. When a
  single-valued fact changes: memory_kg_supersede. When a fact ended
  without replacement: memory_kg_invalidate. Don't file secrets or
  tokens.

Coordination (event stream):
- Chat sessions are declared-idle until a coordination loop starts.
  Do not arm a background watcher at session start. Focus on the user's
  request first; engage the event stream when collaborating, delegating,
  or when asked to listen.
- Inbox: when entering collaborative mode or before long tasks,
  memory_event_list with to_agent=<HOST>:<HARNESS>:<project>,
  since_event_id=<last event id you processed>.
- Arm watch when coordinating, ack with status=claimed.
"#;

    let substituted = raw
        .replace("<HOST>", &host_clean)
        .replace("<HARNESS>", &harness_clean)
        .replace("<PROJECT>", &project_clean);

    let output = if mcp == "light" {
        substituted
            .replace("memory_search", "memory_query FIND")
            .replace("memory_kg_query", "memory_query KG")
            .replace("memory_add_record", "memory_exec ADD")
            .replace("memory_kg_add", "memory_exec KG ADD")
            .replace("memory_kg_supersede", "memory_exec KG SUPERSEDE")
            .replace("memory_kg_invalidate", "memory_exec KG INVALIDATE")
            .replace("memory_event_list", "memory_coordinate EVENT LIST")
    } else {
        substituted
    };

    Ok(output)
}
