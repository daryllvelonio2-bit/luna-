use rusqlite::Connection;
use tempfile::TempDir;
use harness_core::*;

mod common;
use common::*;

#[tokio::test]
async fn append_assigns_sequence_and_replay_returns_it_in_order() {
    let (_dir, _path, store) = temp_store();
    store
        .ensure_session("s1")
        .await
        .expect("create the session");

    store
        .append("s1", &Message::user("first"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("second"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::user("third"), false)
        .await
        .unwrap();

    let replay = store.replay("s1", 200).await.unwrap();
    assert_eq!(replay.total, 3);
    assert_eq!(replay.replayed, 3);
    assert!(!replay.truncated());

    let seqs: Vec<i64> = replay.history.iter().map(|stored| stored.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    let bodies: Vec<Option<String>> = replay
        .history
        .iter()
        .map(|stored| stored.message.content.clone())
        .collect();
    assert_eq!(
        bodies,
        vec![
            Some("first".to_string()),
            Some("second".to_string()),
            Some("third".to_string())
        ]
    );
    assert_eq!(replay.history[0].message.role, Role::User);
}

/// A tool call's provider signature is part of the record, not scratch state: Gemini validates it
/// against the call when the history is replayed on the next request, so a store that dropped it
/// would break the conversation it just persisted.
#[tokio::test]
async fn a_tool_calls_signature_survives_the_round_trip_through_the_store() {
    let (_dir, _path, store) = temp_store();
    store
        .ensure_session("s1")
        .await
        .expect("create the session");

    let assistant = Message {
        role: Role::Assistant,
        content: None,
        reasoning: None,
        tool_calls: vec![ToolCall {
            id: "call_1".to_string(),
            name: "list_files".to_string(),
            arguments: serde_json::json!({ "path": "." }),
            signature: Some("sig-from-gemini".to_string()),
        }],
        tool_result: None,
        usage: None,
    };
    store.append("s1", &assistant, false).await.unwrap();

    let replay = store.replay("s1", 200).await.unwrap();
    let replayed = &replay.history[0].message.tool_calls[0];
    assert_eq!(replayed.name, "list_files");
    assert_eq!(
        replayed.signature.as_deref(),
        Some("sig-from-gemini"),
        "the signature must come back verbatim, or the next request 400s"
    );
}

#[tokio::test]
async fn replay_bound_keeps_the_newest_and_reports_the_total() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    for index in 0..10 {
        let message = Message::user(format!("line {}", index));
        store.append("s1", &message, false).await.unwrap();
    }

    let replay = store.replay("s1", 3).await.unwrap();
    assert_eq!(replay.total, 10);
    assert_eq!(replay.replayed, 3);
    assert!(
        replay.truncated(),
        "the bound dropped messages, so it must say so"
    );

    let bodies: Vec<Option<String>> = replay
        .history
        .iter()
        .map(|stored| stored.message.content.clone())
        .collect();
    assert_eq!(
        bodies,
        vec![
            Some("line 7".to_string()),
            Some("line 8".to_string()),
            Some("line 9".to_string())
        ]
    );
}

/// The bound counts rows, but a row is not a turn: one long turn can fill the whole window, and
/// a window that holds no user row replays as nothing at all — the console prints an empty
/// transcript and the model is handed tool results whose calls are missing. The window is pulled
/// back to the user message that anchors the turn it landed in.
#[tokio::test]
async fn replay_window_pulls_back_to_the_user_message_that_anchors_the_turn() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    store
        .append("s1", &Message::user("the first question"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("the first answer"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::user("the question"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("the answer"), false)
        .await
        .unwrap();

    // A bound of one row would take the answer alone and leave the question behind.
    let replay = store.replay("s1", 1).await.unwrap();
    let roles: Vec<Role> = replay
        .history
        .iter()
        .map(|stored| stored.message.role)
        .collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant],
        "the turn comes in whole, so it can be read and replayed"
    );
    assert_eq!(
        replay.history[0].message.content.as_deref(),
        Some("the question")
    );
    assert!(replay.truncated(), "the bound still dropped the rest");
}

#[tokio::test]
async fn replay_window_pulls_back_when_cut_turn_precedes_later_user_turns() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();

    // Turn 1: user message followed by tool call, tool result, and assistant reply
    store
        .append("s1", &Message::user("turn 1 prompt"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("calling tool"), false)
        .await
        .unwrap();
    store
        .append(
            "s1",
            &Message::tool(ToolResult::success("tool_1", "tool result output")),
            false,
        )
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("turn 1 answer"), false)
        .await
        .unwrap();

    // Turn 2: clean user turn
    store
        .append("s1", &Message::user("turn 2 prompt"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("turn 2 answer"), false)
        .await
        .unwrap();

    // Total rows: 6.
    // Bound limit = 3 falls on seq 4 ("turn 1 answer", assistant).
    // Must pull back to seq 1 ("turn 1 prompt") so turn 1 is preserved whole.
    let replay = store.replay("s1", 3).await.unwrap();
    assert_eq!(
        replay.history[0].message.role,
        Role::User,
        "history starts on a clean user turn"
    );
    assert_eq!(
        replay.history[0].message.content.as_deref(),
        Some("turn 1 prompt"),
        "pulled back to the user message anchoring the cut turn"
    );
    assert_eq!(replay.history.len(), 6, "all rows of both turns preserved");
}

#[tokio::test]
async fn record_compaction_writes_a_summary_and_keeps_the_folded_rows() {
    let (_dir, path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    store
        .append("s1", &Message::user("one"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("two"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::user("three"), false)
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("four"), false)
        .await
        .unwrap();

    let summary_id = store
        .record_compaction("s1", &[1, 2], "the user asked one")
        .await
        .unwrap();
    assert!(summary_id > 4, "the summary is a new row, not a rewrite");

    let replay = store.replay("s1", 200).await.unwrap();
    assert!(replay.history[0].is_summary);
    assert_eq!(
        replay.history[0].message.content.as_deref(),
        Some("the user asked one")
    );
    assert_eq!(replay.history[0].message.role, Role::System);
    assert_eq!(
        replay
            .history
            .iter()
            .filter(|stored| !stored.is_summary)
            .count(),
        2,
        "the two unfolded rows are what is left to replay"
    );

    // Nothing is destroyed: all four originals are still rows, the folded two are marked.
    assert_eq!(count_rows(&path, "1 = 1"), 5);
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 2);
    assert_eq!(count_rows(&path, "is_summary = 1"), 1);

    // The human conversation transcript keeps every ordinary message and drops synthetic summaries.
    let transcript = store.replay_transcript("s1", 200).await.unwrap();
    assert_eq!(
        transcript.total, 4,
        "all four conversation messages are counted"
    );
    assert_eq!(transcript.replayed, 4, "all four messages are replayed");
    assert!(
        transcript.history.iter().all(|s| !s.is_summary),
        "summary rows are not part of the transcript"
    );
    let contents: Vec<&str> = transcript
        .history
        .iter()
        .map(|s| s.message.content.as_deref().unwrap())
        .collect();
    assert_eq!(contents, vec!["one", "two", "three", "four"]);
}

#[tokio::test]
async fn list_reports_the_first_user_line_and_the_most_recent_session() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    store
        .append(
            "s1",
            &Message::user("  what is a compaction boundary?  "),
            false,
        )
        .await
        .unwrap();
    store
        .append("s1", &Message::assistant("a boundary"), false)
        .await
        .unwrap();

    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "s1");
    assert_eq!(sessions[0].title, "what is a compaction boundary?");
    assert_eq!(sessions[0].name, "", "a fresh session has no name");
    assert_eq!(sessions[0].message_count, 2);
    assert_eq!(store.most_recent().await.unwrap().as_deref(), Some("s1"));
}

#[tokio::test]
async fn delete_session_removes_the_session_and_its_messages() {
    let (_dir, path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    store.ensure_session("s2").await.unwrap();
    store
        .append("s1", &Message::user("keep me"), false)
        .await
        .unwrap();
    store
        .append("s2", &Message::user("delete me"), false)
        .await
        .unwrap();
    store
        .append("s2", &Message::assistant("gone"), false)
        .await
        .unwrap();
    assert_eq!(count_rows(&path, "1 = 1"), 3);

    assert!(store.delete_session("s2").await.unwrap());

    // The foreign key cascade takes the messages with the session.
    assert_eq!(count_rows(&path, "1 = 1"), 1);
    assert!(
        store.get("s2").await.is_err(),
        "the deleted session is gone"
    );

    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "s1");
    assert_eq!(store.most_recent().await.unwrap().as_deref(), Some("s1"));

    // Deleting what is already gone is not an error: it reports that nothing was removed.
    assert!(!store.delete_session("s2").await.unwrap());
}

#[tokio::test]
async fn delete_all_sessions_empties_the_store() {
    let (_dir, path, store) = temp_store();
    for id in ["s1", "s2", "s3"] {
        store.ensure_session(id).await.unwrap();
        store.append(id, &Message::user("hi"), false).await.unwrap();
        store
            .append(id, &Message::assistant("hello"), false)
            .await
            .unwrap();
    }
    assert_eq!(count_rows(&path, "1 = 1"), 6);

    assert_eq!(store.delete_all_sessions().await.unwrap(), 3);

    // The cascade takes every message with its session, in the same statement.
    assert_eq!(count_rows(&path, "1 = 1"), 0);
    assert!(store.list().await.unwrap().is_empty());
    assert_eq!(store.most_recent().await.unwrap(), None);

    // An empty store is not an error: it reports that nothing was removed.
    assert_eq!(store.delete_all_sessions().await.unwrap(), 0);
}

#[tokio::test]
async fn set_name_is_persisted_and_reported_by_list() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();

    store.set_name("s1", "  release triage  ").await.unwrap();
    assert_eq!(store.get("s1").await.unwrap().name, "release triage");

    let sessions = store.list().await.unwrap();
    assert_eq!(sessions[0].name, "release triage");

    // An empty name clears it again.
    store.set_name("s1", "").await.unwrap();
    assert_eq!(store.get("s1").await.unwrap().name, "");
}

#[tokio::test]
async fn naming_tolerates_databases_written_before_the_column_existed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    // A pre-`/name` database: the sessions table has no `name` column yet.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id         TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 title      TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO sessions (id, created_at, updated_at, title)
             VALUES ('s1', '2020-01-01T00:00:00+00:00', '2020-01-01T00:00:00+00:00', 'old');",
        )
        .unwrap();
    }

    let store = SqliteSessionStore::new(Some(path)).expect("opening migrates the table");
    assert_eq!(
        store.get("s1").await.unwrap().name,
        "",
        "backfilled as empty"
    );
    store.set_name("s1", "migrated").await.unwrap();
    assert_eq!(store.get("s1").await.unwrap().name, "migrated");
}

#[tokio::test]
async fn set_model_is_persisted_and_reported_by_get_and_list() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();

    // A session that has not picked a model reports none, not a zero pair.
    assert_eq!(store.get("s1").await.unwrap().model, None);

    store.set_model("s1", "deepseek", "glm-4.6").await.unwrap();
    assert_eq!(
        store.get("s1").await.unwrap().model,
        Some(("deepseek".to_string(), "glm-4.6".to_string()))
    );

    let sessions = store.list().await.unwrap();
    assert_eq!(
        sessions[0].model,
        Some(("deepseek".to_string(), "glm-4.6".to_string()))
    );

    // A second switch overwrites: the row holds the latest pair.
    store.set_model("s1", "openai", "gpt-5").await.unwrap();
    assert_eq!(
        store.get("s1").await.unwrap().model,
        Some(("openai".to_string(), "gpt-5".to_string()))
    );

    // Setting on a missing session is an error, same contract as set_name.
    assert!(store.set_model("nope", "openai", "gpt-5").await.is_err());
}

#[tokio::test]
async fn clear_model_drops_the_session_pin_and_reports_none() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    store.set_model("s1", "deepseek", "glm-4.6").await.unwrap();

    store.clear_model("s1").await.unwrap();
    assert_eq!(
        store.get("s1").await.unwrap().model,
        None,
        "a cleared pin follows the configured default again"
    );

    // Clearing a missing session is an error, same contract as set_model.
    assert!(store.clear_model("nope").await.is_err());
}

#[tokio::test]
async fn model_persistence_survives_reopening_the_database() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    {
        let store = SqliteSessionStore::new(Some(path.clone())).unwrap();
        store.ensure_session("s1").await.unwrap();
        store.set_model("s1", "zhipu", "glm-4.6").await.unwrap();
    }

    // A new process opens the same file and reads the pair back — this is the resume path.
    let reopened = SqliteSessionStore::new(Some(path)).unwrap();
    assert_eq!(
        reopened.get("s1").await.unwrap().model,
        Some(("zhipu".to_string(), "glm-4.6".to_string()))
    );
}

#[tokio::test]
async fn model_tolerates_databases_written_before_the_column_existed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    // A pre-model-persistence database: the sessions table has neither `name` nor `model`.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id         TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 title      TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO sessions (id, created_at, updated_at, title)
             VALUES ('s1', '2020-01-01T00:00:00+00:00', '2020-01-01T00:00:00+00:00', 'old');",
        )
        .unwrap();
    }

    let store = SqliteSessionStore::new(Some(path)).expect("opening migrates the table");
    assert_eq!(
        store.get("s1").await.unwrap().model,
        None,
        "backfilled as unset"
    );
    store.set_model("s1", "deepseek", "glm-4.6").await.unwrap();
    assert_eq!(
        store.get("s1").await.unwrap().model,
        Some(("deepseek".to_string(), "glm-4.6".to_string()))
    );
}

#[tokio::test]
async fn compact_model_persistence_and_clearing() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();

    // Fresh session has no compact model override
    assert_eq!(store.get("s1").await.unwrap().compact_model, None);

    store
        .set_compact_model("s1", "nvidia", "meta/llama-3.1-8b-instruct")
        .await
        .unwrap();
    assert_eq!(
        store.get("s1").await.unwrap().compact_model,
        Some((
            "nvidia".to_string(),
            "meta/llama-3.1-8b-instruct".to_string()
        ))
    );

    // Clearing reverts to None
    store.clear_compact_model("s1").await.unwrap();
    assert_eq!(store.get("s1").await.unwrap().compact_model, None);
}

#[tokio::test]
async fn compact_model_persistence_survives_reopening_the_database() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    {
        let store = SqliteSessionStore::new(Some(path.clone())).unwrap();
        store.ensure_session("s1").await.unwrap();
        store
            .set_compact_model("s1", "anthropic", "claude-3-haiku")
            .await
            .unwrap();
    }

    let reopened = SqliteSessionStore::new(Some(path)).unwrap();
    assert_eq!(
        reopened.get("s1").await.unwrap().compact_model,
        Some(("anthropic".to_string(), "claude-3-haiku".to_string()))
    );
}

// ---------------------------------------------------------------- estimator

/// Characters per token for the estimator's fallback. The provider-reported count for
/// `fixtures/token_sample.txt` was measured once, against the harness model
/// (`nvidia/deepseek-ai/deepseek-v4.1-flash`), with the sample as a single user message and no system
/// prompt; the command that prints it is `python3 scripts/measure-token-sample.py`. The tolerance
/// asserted below is [`HEURISTIC_TOLERANCE`].
const REPORTED_PROMPT_TOKENS: u32 = 397;

#[tokio::test]
async fn heuristic_and_reported_estimates_agree_on_a_fixed_sample() {
    let sample = include_str!("fixtures/token_sample.txt");
    let messages = vec![Message::user(sample)];

    let heuristic = heuristic_tokens(&messages);
    assert_eq!(heuristic as usize, sample.len().div_ceil(CHARS_PER_TOKEN));

    let reported = REPORTED_PROMPT_TOKENS as f64;
    let relative = (heuristic as f64 - reported).abs() / reported;
    assert!(
        relative <= HEURISTIC_TOLERANCE,
        "heuristic {} vs reported {} is {:.1}% off, tolerance is {:.0}%",
        heuristic,
        REPORTED_PROMPT_TOKENS,
        relative * 100.0,
        HEURISTIC_TOLERANCE * 100.0
    );
}

#[tokio::test]
async fn credential_priorities_persistence_survives_reopening_the_database() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    {
        let store = SqliteSessionStore::new(Some(path.clone())).unwrap();
        store.ensure_session("s1").await.unwrap();
        store
            .set_credential_priority("s1", "antigravity", "account-1")
            .await
            .unwrap();
    }

    let reopened = SqliteSessionStore::new(Some(path.clone())).unwrap();
    let rec = reopened.get("s1").await.unwrap();
    assert_eq!(
        rec.credential_priorities.get("antigravity"),
        Some(&"account-1".to_string())
    );

    // Clear credential priority
    reopened
        .clear_credential_priority("s1", "antigravity")
        .await
        .unwrap();
    let cleared_rec = reopened.get("s1").await.unwrap();
    assert_eq!(cleared_rec.credential_priorities.get("antigravity"), None);
}

#[tokio::test]
async fn estimate_prefers_reported_usage_and_adds_the_heuristic_tail() {
    let mut with_usage = Message::assistant("an answer");
    with_usage.usage = Some(TokenUsage::estimated(1_000, 200));
    let tail = Message::user("x".repeat(400));

    let estimate = estimate_tokens(&[with_usage, tail]);
    assert_eq!(estimate.source, EstimateSource::Reported);
    assert_eq!(
        estimate.tokens,
        1_200 + 100,
        "reported (prompt+completion) plus the 400-char tail"
    );

    let heuristic_only = estimate_tokens(&[Message::user("x".repeat(400))]);
    assert_eq!(heuristic_only.source, EstimateSource::Heuristic);
    assert_eq!(heuristic_only.tokens, 100);
    assert_eq!(estimate_tokens(&[]).tokens, 0);

    // An inline base64 image data URL must not be counted as hundreds of thousands of prose characters.
    let image_payload = format!(
        "inspect this header\n\ndata:image/png;base64,{}\n\nthanks",
        "A".repeat(600_000)
    );
    let image_estimate = estimate_tokens(&[Message::user(image_payload)]);
    assert_eq!(image_estimate.source, EstimateSource::Heuristic);
    assert!(
        image_estimate.tokens < 1_000,
        "inline image tokens must be estimated reasonably (~250 tokens), got {}",
        image_estimate.tokens
    );
}

