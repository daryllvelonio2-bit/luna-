//! Lifecycle hooks and instructions for the memory store.
//! Implements semantic hooks: session_start, stop, session_end, and precompact,
//! plus instructions and shared brain agent rules.

use std::fs;
use std::path::{Path, PathBuf};

use harness_core::HarnessError;
use serde_json::{json, Value};

use super::MemoryStore;

pub const DEFAULT_SAVE_INTERVAL: usize = 10;

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
            Ok(serde_json::to_string_pretty(&res).unwrap())
        }
        "stop" => {
            let last_save_file = state_dir.join(format!("{session_id}_last_save"));
            let last_save: usize = fs::read_to_string(&last_save_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);

            let current_count = if let Some(path) = transcript_path {
                count_messages_in_file(&path)
            } else {
                input.get("exchange_count").and_then(Value::as_u64).unwrap_or(0) as usize
            };

            let since = current_count.saturating_sub(last_save);
            if since >= DEFAULT_SAVE_INTERVAL && current_count > 0 {
                fs::write(&last_save_file, format!("{current_count}\n"))
                    .map_err(|e| HarnessError::Io(e.to_string()))?;

                let res = json!({
                    "decision": "save_triggered",
                    "session_id": session_id,
                    "exchange_count": current_count,
                    "messages_since_save": since,
                    "success": true
                });
                Ok(serde_json::to_string_pretty(&res).unwrap())
            } else {
                let res = json!({
                    "decision": "pass",
                    "session_id": session_id,
                    "exchange_count": current_count,
                    "messages_since_save": since,
                    "success": true
                });
                Ok(serde_json::to_string_pretty(&res).unwrap())
            }
        }
        "session-end" => {
            let _ = store.journal_write("luna", &format!("Session {session_id} ended cleanly."), None);
            let last_save_file = state_dir.join(format!("{session_id}_last_save"));
            let _ = fs::remove_file(last_save_file);

            let res = json!({
                "decision": "session_ended",
                "session_id": session_id,
                "success": true
            });
            Ok(serde_json::to_string_pretty(&res).unwrap())
        }
        "precompact" => {
            let _ = store.reindex();
            let res = json!({
                "decision": "precompact_mined",
                "session_id": session_id,
                "success": true
            });
            Ok(serde_json::to_string_pretty(&res).unwrap())
        }
        other => Err(HarnessError::MemoryError(format!("unknown hook: {other}"))),
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
3. Sets up SQLite daemon queue and permissions."#.to_string()),
        "search" => Ok(r#"# Luna Memory Search

Search memory records using hybrid BM25 lexical + MiniLM vector ranking.

1. Provide search query text.
2. Optionally specify --domain and --topic filters.
3. Set --limit (default 8) and --since timestamp if needed.
4. Results return structured scores, excerpts, and matched entity pointers."#.to_string()),
        "mine" => Ok(r#"# Luna Memory Mine

Extract durable facts, architecture, rules, and notes from codebase or logs.

Modes:
- projects: mines codefiles, docs, configs into domain records
- convos: mines Claude JSONL conversation logs
- sessions: mines sessions.db chat database into memory"#.to_string()),
        "status" => Ok(r#"# Luna Memory Status

Overview of current store state:
- Total records count and active records
- Domains and topics breakdown
- Active embedder model
- Search index freshness and knowledge graph stats"#.to_string()),
        "audit" => Ok(r#"# Luna Memory Audit

Verifies structural integrity of the memory store:
- Invalid front matter checks
- Unreadable files
- Empty body detection
- Dangling supersessions
- Duplicate active (subject, predicate) records"#.to_string()),
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
- luna memory rules"#.to_string()),
        other => Err(HarnessError::MemoryError(format!("unknown instructions topic: {other}"))),
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

Coordination (logstream):
- Chat sessions are declared-idle until a coordination loop starts.
  Do not arm a background watcher at session start. Focus on the user's
  request first; engage the logstream when collaborating, delegating,
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
