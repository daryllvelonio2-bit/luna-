use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use harness_core::*;

mod common;
use common::*;

#[tokio::test]
async fn compact_does_not_fold_when_only_summaries_would_be_folded() {
    let history = vec![
        // Prior summary row
        StoredMessage {
            id: 1,
            seq: 1,
            message: Message::system("prior summary of old turns"),
            is_summary: true,
        },
        // One active user turn with a large content that exceeds keep budget
        StoredMessage {
            id: 2,
            seq: 2,
            message: Message::user("current active user prompt"),
            is_summary: false,
        },
    ];

    let provider = CannedProvider::new("new summary");
    let outcome = compact(&provider, "canned-model", &history, Some(1))
        .await
        .unwrap();
    assert!(
        outcome.is_none(),
        "must return None when only summary rows precede the kept turn"
    );

    let percent_outcome = compact_percent(&provider, "canned-model", &history, 50)
        .await
        .unwrap();
    assert!(
        percent_outcome.is_none(),
        "compact_percent must return None when only summary rows precede the kept turn"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------- compaction

#[tokio::test]
async fn compact_folds_the_oldest_turns_and_bounds_the_vector() {
    let history = history(6);
    let provider = CannedProvider::new("four questions were answered");

    let outcome = compact(&provider, "canned-model", &history, Some(2))
        .await
        .unwrap()
        .expect("six turns with two kept is four folded");

    assert_eq!(outcome.folded_turns, 4);
    assert_eq!(outcome.folded_ids.len(), 8);
    assert_eq!(outcome.kept.len(), 4, "the last two turns stay verbatim");

    let bounded = outcome.bounded_messages("SYSTEM".to_string());
    assert_eq!(bounded.len(), 6, "system + summary + 4 kept turns");
    assert_eq!(bounded[0].role, Role::System);
    assert_eq!(bounded[0].content.as_deref(), Some("SYSTEM"));
    assert_eq!(bounded[1].role, Role::System);
    assert_eq!(
        bounded[1].content.as_deref(),
        Some("four questions were answered"),
        "the summary is the second message, right after the system prompt"
    );
    assert_eq!(bounded[2].content.as_deref(), Some("question 4"));
    assert!(bounded.len() < history.len(), "the sent vector is bounded");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn compact_folds_nothing_when_the_history_is_short_enough() {
    let history = history(2);
    let provider = CannedProvider::new("unused");

    assert_eq!(fold_boundary(&history, 2), 0);
    assert_eq!(fold_boundary(&history, 8), 0);
    let outcome = compact(&provider, "canned-model", &history, Some(2))
        .await
        .unwrap();
    assert!(outcome.is_none());
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "no foldable turn means no summarising call at all"
    );
}

#[tokio::test]
async fn compact_returns_an_error_when_the_provider_fails() {
    let history = history(6);
    let provider = FailingProvider::new();

    let result = compact(&provider, "unconfigured", &history, Some(2)).await;
    assert!(
        result.is_err(),
        "the caller decides what a failed summary means"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

/// A transcript that holds fewer turns than we keep is still foldable. A turn is not a
/// fixed size: one turn carrying a megabyte of inline image is as big as a hundred short
/// ones, so the turn rule can have nothing to fold while the transcript is far over
/// budget. Then the boundary moves to a message boundary — otherwise the user is left
/// with a context they cannot shrink, and `/compact` says there is nothing to compact.
#[tokio::test]
async fn a_big_transcript_with_few_turns_is_folded_at_a_message_boundary() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(500_000));
    let provider = RecordingProvider::new("FOLDED");

    let outcome = compact(&provider, "canned-model", &big, Some(8))
        .await
        .unwrap()
        .expect("a transcript over budget is foldable, whatever its turn count");

    assert_eq!(
        outcome.folded_ids,
        vec![big[0].id],
        "the huge message is what gets folded"
    );
    assert_eq!(
        outcome.kept.len(),
        3,
        "the most recent messages stay verbatim"
    );
    assert_eq!(
        outcome.folded_turns, 1,
        "the fold took the turn the huge message was in"
    );
    assert_eq!(outcome.folded_messages, 1);
}

/// The summarising call must not carry the megabytes. A provider rejects a prompt that
/// long, and the fold would fail for the very transcript that needed it.
#[tokio::test]
async fn the_summarising_call_carries_a_bounded_transcript() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(2_000_000));
    let provider = RecordingProvider::new("FOLDED");

    compact(&provider, "canned-model", &big, Some(8))
        .await
        .unwrap()
        .expect("foldable");

    let request = provider.last_request();
    let sent = request
        .iter()
        .map(|message| message.content.clone().unwrap_or_default())
        .collect::<String>();
    assert!(
        sent.len() < 100_000,
        "the transcript that goes to the summariser is bounded, got {} chars",
        sent.len()
    );
    assert!(
        sent.contains("more characters cut"),
        "the cut is marked, so the summary can still say what was cut"
    );
}

/// The folded region is budgeted from its newest end.
///
/// The turns immediately before the kept tail are the working state the next turn continues
/// from. A head-first cut — render the oldest messages and stop at the budget — replaces
/// exactly those with a truncation marker, so the summary describes the opening of the session
/// and says nothing about what was happening when the fold ran. That reads as the model
/// forgetting its own next step right after a compaction.
#[tokio::test]
async fn the_summarising_call_keeps_the_newest_folded_turns() {
    let mut transcript = history(40);
    for stored in transcript.iter_mut() {
        if let Some(content) = stored.message.content.as_mut() {
            content.push_str(&"f".repeat(4_000));
        }
    }
    // The compressed head of an earlier fold: a handful of lines, never the thing to cut.
    let mut prior = stored(Role::System, "PRIOR SUMMARY LAYER".to_string());
    prior.is_summary = true;
    transcript.insert(0, prior);
    // Turn 37 is the last folded turn (turns 38 and 39 are kept). Its user line is the state
    // the summary must carry forward.
    transcript[75].message.content =
        Some(format!("THE MOST RECENT FOLDED TURN {}", "g".repeat(4_000)));

    let provider = RecordingProvider::new("FOLDED");
    compact(&provider, "canned-model", &transcript, Some(2))
        .await
        .unwrap()
        .expect("foldable");

    let request = provider.last_request();
    let sent = request
        .iter()
        .map(|message| message.content.clone().unwrap_or_default())
        .collect::<String>();
    assert!(
        sent.contains("THE MOST RECENT FOLDED TURN"),
        "the newest folded turn is in the summariser's transcript"
    );
    assert!(
        sent.contains("PRIOR SUMMARY LAYER"),
        "a previous summary survives the cut: it is the only trace of the oldest layers"
    );
    assert!(
        sent.contains("older messages cut"),
        "the cut is marked, and it is the older messages that were cut"
    );
    assert!(
        sent.len() < 100_000,
        "the transcript that goes to the summariser is still bounded, got {} chars",
        sent.len()
    );
    assert!(
        !sent.contains("transcript truncated"),
        "the tail of the folded region is never what gets thrown away"
    );
}

/// A tool result may not stand alone: the assistant message that called it has to be in
/// the kept tail as well, or the provider rejects the vector. So a boundary that lands
/// on a tool result moves forward.
#[tokio::test]
async fn the_kept_tail_never_starts_with_a_tool_result() {
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "a.rs"}),
        signature: None,
    };
    let mut transcript = vec![
        stored(Role::User, "look at this".to_string()),
        stored_with(Role::Assistant, String::new(), vec![call], None),
        stored_with(
            Role::Tool,
            String::new(),
            vec![],
            Some(ToolResult::success("call-1", "y".repeat(400_000))),
        ),
        stored(Role::Assistant, "here it is".to_string()),
    ];
    transcript[0].message.content = Some("x".repeat(500_000));

    let provider = RecordingProvider::new("FOLDED");
    let outcome = compact(&provider, "canned-model", &transcript, Some(8))
        .await
        .unwrap()
        .expect("foldable");

    assert!(
        outcome.kept.iter().all(|m| m.role != Role::Tool),
        "no orphan tool result survives in the kept tail"
    );
    assert_eq!(
        outcome.kept.first().map(|m| m.role),
        Some(Role::Assistant),
        "the kept tail opens on a message that can start a conversation"
    );
    assert!(
        outcome.folded_messages >= 2,
        "the huge message and the huge tool result are folded, got {}",
        outcome.folded_messages
    );
}

/// An assistant message issuing tool calls cannot open the kept tail: providers like Gemini
/// require a user turn before any function call turn, and tool calls without the user prompt
/// are orphaned. The boundary must advance forward past the tool loop.
#[tokio::test]
async fn the_kept_tail_never_starts_with_an_assistant_tool_call() {
    let call1 = ToolCall {
        id: "call-1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "a.rs"}),
        signature: None,
    };
    let call2 = ToolCall {
        id: "call-2".to_string(),
        name: "search_files".to_string(),
        arguments: serde_json::json!({"pattern": "foo"}),
        signature: None,
    };
    let mut transcript = vec![
        stored(Role::User, "initial prompt".to_string()),
        stored_with(Role::Assistant, String::new(), vec![call1], None),
        stored_with(
            Role::Tool,
            String::new(),
            vec![],
            Some(ToolResult::success("call-1", "file content")),
        ),
        stored_with(Role::Assistant, String::new(), vec![call2], None),
        stored_with(
            Role::Tool,
            String::new(),
            vec![],
            Some(ToolResult::success("call-2", "y".repeat(400_000))),
        ),
        stored(Role::User, "next user task".to_string()),
        stored(Role::Assistant, "completed".to_string()),
    ];
    transcript[0].message.content = Some("x".repeat(500_000));

    let provider = RecordingProvider::new("FOLDED");
    let outcome = compact(&provider, "canned-model", &transcript, Some(8))
        .await
        .unwrap()
        .expect("foldable");

    let first = outcome.kept.first().expect("kept tail not empty");
    assert!(
        !(first.role == Role::Assistant && !first.tool_calls.is_empty()),
        "first kept message cannot be an assistant issuing tool calls"
    );
    assert_ne!(
        first.role,
        Role::Tool,
        "first kept message cannot be a tool result"
    );
    assert_eq!(
        first.role,
        Role::User,
        "kept tail advances to the clean user turn"
    );
    assert_eq!(first.content.as_deref(), Some("next user task"));
}

/// A transcript that fits the budget has nothing to fold, and must not spend a call.
#[tokio::test]
async fn a_small_transcript_with_few_turns_is_left_alone() {
    let history = history(2);
    let provider = CannedProvider::new("unused");

    assert_eq!(fold_boundary(&history, 8), 0);
    let outcome = compact(&provider, "canned-model", &history, Some(8))
        .await
        .unwrap();
    assert!(outcome.is_none());
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "a transcript under the budget needs no summarising call at all"
    );
}

/// One stored message with its own row id, so a fold can be checked against it.
fn numbered(
    id: i64,
    role: Role,
    content: Option<String>,
    tool_calls: Vec<ToolCall>,
) -> StoredMessage {
    StoredMessage {
        id,
        seq: id,
        is_summary: false,
        message: Message {
            role,
            content,
            reasoning: None,
            tool_calls,
            tool_result: None,
            usage: None,
        },
    }
}

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({ "path": "a.rs" }),
        signature: None,
    }
}

/// A fold may not cut a tool round in half.
///
/// The answers of the last call arrive while the summariser is still thinking: the fold is
/// planned on the transcript as it was when the fold started. Folding that call would leave
/// its answers in the live tail with nothing to answer, and a provider rejects such a request
/// outright — NVIDIA answers `HTTP 400 {"message":"Invalid request"}`.
///
/// So the call stays with the answers that are still on their way.
#[tokio::test]
async fn a_fold_never_folds_a_call_whose_answers_have_not_arrived() {
    // One huge user turn, and an assistant whose call has not been answered yet.
    let transcript = vec![
        numbered(1, Role::User, Some("x".repeat(500_000)), vec![]),
        numbered(2, Role::Assistant, None, vec![call("call-1")]),
    ];
    let provider = RecordingProvider::new("FOLDED");

    let outcome = compact(&provider, "canned-model", &transcript, Some(8))
        .await
        .unwrap()
        .expect("a transcript over budget is foldable");

    assert!(
        !outcome.folded_ids.contains(&2),
        "the call stays with the answers that are still on their way, folded: {:?}",
        outcome.folded_ids
    );
    assert_eq!(
        outcome.kept.len(),
        1,
        "the unanswered call is the tail, so its answers have somewhere to land"
    );
    assert_eq!(outcome.kept[0].tool_calls.len(), 1);
}

/// The kept tail may not open on a tool result: the assistant that asked for it would sit in
/// the folded part, and the provider would reject a tool message that answers no call. The
/// cut moves back so the call travels with its answers.
#[tokio::test]
async fn a_fold_never_leaves_the_kept_tail_opening_on_a_tool_result() {
    let transcript = vec![
        numbered(1, Role::User, Some("x".repeat(500_000)), vec![]),
        numbered(2, Role::Assistant, None, vec![call("call-1")]),
        StoredMessage {
            id: 3,
            seq: 3,
            is_summary: false,
            message: Message {
                role: Role::Tool,
                content: Some("y".repeat(400_000)),
                reasoning: None,
                tool_calls: vec![],
                tool_result: Some(ToolResult::success("call-1", "y".repeat(400_000))),
                usage: None,
            },
        },
    ];
    let provider = RecordingProvider::new("FOLDED");

    let outcome = compact(&provider, "canned-model", &transcript, Some(8))
        .await
        .unwrap()
        .expect("a transcript over budget is foldable");

    assert_eq!(
        orphan_tool_results(&outcome.kept),
        0,
        "the kept tail carries no tool result that answers no call: {:?}",
        outcome
            .kept
            .iter()
            .map(|message| message.role)
            .collect::<Vec<_>>()
    );
}

/// How many tool results in `messages` answer no call in `messages`.
///
/// A provider rejects the whole request over one of them: NVIDIA answers `HTTP 400
/// {"message":"Invalid request"}`.
fn orphan_tool_results(messages: &[Message]) -> usize {
    let mut announced: HashSet<String> = HashSet::new();
    let mut orphans = 0;
    for message in messages {
        if message.role == Role::Assistant {
            for call in &message.tool_calls {
                announced.insert(call.id.clone());
            }
        } else if message.role == Role::Tool {
            let call_id = message
                .tool_result
                .as_ref()
                .map(|result| result.call_id.clone())
                .unwrap_or_default();
            if !announced.contains(&call_id) {
                orphans += 1;
            }
        }
    }
    orphans
}

/// A tool result whose call was folded away answers nothing, and a provider rejects the whole
/// request over it — NVIDIA answers `HTTP 400 {"message":"Invalid request"}`. The transcript
/// the model is given must not carry one, even when the store still holds it: a session that
/// was folded badly stays folded, and the next turn must still work.
#[tokio::test]
async fn a_replayed_transcript_never_carries_a_tool_result_that_answers_no_call() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s_orphan").await.unwrap();
    store
        .append("s_orphan", &Message::user("read the file"), false)
        .await
        .unwrap();
    let call_row = store
        .append(
            "s_orphan",
            &Message::assistant_with_tools(vec![ToolCall {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({ "path": "a.rs" }),
                signature: None,
            }]),
            false,
        )
        .await
        .unwrap();
    store
        .append(
            "s_orphan",
            &Message::tool(ToolResult::success("call-1", "file content")),
            false,
        )
        .await
        .unwrap();
    store
        .append("s_orphan", &Message::assistant("done"), false)
        .await
        .unwrap();
    // A fold that cut the round in half: the call went into the summary, its answer stayed live.
    store
        .record_compaction("s_orphan", &[call_row], "SUMMARY")
        .await
        .unwrap();

    let store = Arc::new(store);
    let main = Arc::new(RecordingProvider::new("done"));
    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s_orphan".to_string(),
        resume_max_messages: 200,
    });

    agent
        .run_turn("next prompt", CancellationToken::new())
        .await
        .expect("the turn runs");

    let sent = main.last_request();
    assert_eq!(
        orphan_tool_results(&sent),
        0,
        "the request carries no tool result that answers no call: {:?}",
        sent.iter().map(|message| message.role).collect::<Vec<_>>()
    );
    assert!(
        sent.iter()
            .any(|message| message.content.as_deref() == Some("done")),
        "the rest of the transcript is still sent"
    );
}

/// A reasoning model can spend its whole budget on reasoning and answer with nothing.
/// The provider layer then hands back a marker that says so, and a compaction that took
/// it for a summary would have replaced the session with a sentence about reasoning.
/// So the call is retried with more room.
#[tokio::test]
async fn an_empty_summary_is_retried_with_a_bigger_budget() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(500_000));
    let provider = ReasoningProvider::new();

    let outcome = compact(&provider, "reasoning-model", &big, Some(8))
        .await
        .unwrap()
        .expect("foldable");

    assert_eq!(
        outcome.summary, "the summary, with room to write it",
        "the retry's summary is the one that counts"
    );
    let budgets = provider.budgets.lock().unwrap().clone();
    assert_eq!(budgets.len(), 2, "one call, one retry");
    assert!(
        budgets[1] > budgets[0],
        "the retry asks for more room: {} then {}",
        budgets[0],
        budgets[1]
    );
}

/// A model that never writes a summary is a failure, not a summary: the fold must not
/// go through with the marker in place of one.
#[tokio::test]
async fn a_summary_that_never_arrives_is_an_error() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(500_000));
    let provider = SilentProvider::new();

    let result = compact(&provider, "silent-model", &big, Some(8)).await;

    assert!(
        result.is_err(),
        "a fold with no summary must not go through"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "one call, one retry"
    );
}

// ---------------------------------------------------------------- the agent loop

/// The explicit fold policy: `compact_at_tokens` and `keep_recent_turns`, which is what `/compact`
/// and the default turn loop use. Auto-compact is off here because it moves both numbers, and the
/// tests built on this helper assert *these* two.

#[tokio::test]
async fn run_turn_does_not_wait_for_the_fold_and_the_next_turn_reaps_it() {
    let (_dir, path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");

    // The request this turn sent is the transcript as it stood: the fold did not hold it back.
    let sent = main.last_request();
    assert_eq!(
        sent.len(),
        14,
        "system + the full 12-message transcript + the new user line"
    );
    assert_eq!(sent[0].role, Role::System);
    assert_eq!(sent[1].content.as_deref(), Some("question 0"));
    assert_eq!(sent[sent.len() - 1].content.as_deref(), Some("hello"));

    // The fold ran off that path, and it announced itself when it was done.
    folded.notified().await;
    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "one summarising call"
    );
    assert_eq!(main.call_count(), 1);

    // What it wrote is a bounded transcript: the kept turns plus the summary.
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 8);
    let replay = store.replay("s1", 200).await.unwrap();
    assert!(replay.history.iter().any(|stored| stored.is_summary));

    // The next turn is the one that sends the bounded transcript.
    let second = agent
        .run_turn("again", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(second.reply, "done");
    let sent = main.last_request();
    assert!(
        sent.iter()
            .any(|message| message.content.as_deref() == Some("FOLDED")),
        "the next request carries the summary"
    );
    assert_eq!(
        sent.len(),
        9,
        "system + 4 kept messages + the summary + the first turn's own exchange + the new user line"
    );
}

#[tokio::test]
async fn run_turn_survives_a_failing_compaction_provider_and_does_not_retry() {
    let (_dir, path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(FailingProvider::new());

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "unconfigured".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        outcome.reply, "done",
        "the turn proceeds on the full history"
    );

    // The failure happens off the turn's path, where it cannot reach the reply.
    folded.notified().await;
    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "a failed compaction is not retried"
    );

    let sent = main.last_request();
    assert_eq!(
        sent.len(),
        14,
        "system + the full 12-message transcript + the user line"
    );
    assert_eq!(sent[1].role, Role::User);
    assert_eq!(sent[1].content.as_deref(), Some("question 0"));

    // Every original message is still there, none folded, no summary written.
    assert_eq!(
        count_rows(&path, "1 = 1"),
        14,
        "12 replayed + the user line + the reply"
    );
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 0);
    assert_eq!(count_rows(&path, "is_summary = 1"), 0);
}

/// `/compact` is the user asking for a fold. When the summary call fails, that is the
/// answer — it must not be reported as "there was nothing to compact", which sends the
/// user looking for a transcript that is not there.
#[tokio::test]
async fn the_explicit_compact_reports_a_failed_summary_as_a_failure() {
    let (_dir, path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(FailingProvider::new());

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "unconfigured".to_string(),
    });

    let error = agent.compact_current_session().await.unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("no credential for this provider"),
        "the real reason is reported, got: {text}"
    );
    assert!(
        !text.contains("nothing to compact"),
        "a failed summary is not an empty transcript, got: {text}"
    );
    assert_eq!(
        count_rows(&path, "compacted_at IS NOT NULL"),
        0,
        "nothing was folded, so nothing is marked folded"
    );
}

/// `/compact` without a compaction provider says so, instead of claiming the transcript
/// has nothing to fold.
#[tokio::test]
async fn the_explicit_compact_says_when_no_compaction_provider_is_set_up() {
    let (_dir, _path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    });

    let error = agent.compact_current_session().await.unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("compaction is unavailable"),
        "the missing setup is named, got: {text}"
    );
}

#[tokio::test]
async fn summarize_session_produces_compact_summary() {
    let (_dir, _path, store) = seeded_store(3).await;
    let replayed = store.replay("s1", usize::MAX).await.unwrap();
    let provider = CannedProvider::new("The user asked 3 questions and got 3 answers.");

    let summary = summarize_session(&provider, "test-model", &replayed.history)
        .await
        .unwrap();

    assert_eq!(summary, "The user asked 3 questions and got 3 answers.");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn agent_summarize_current_session_works_and_rejects_empty() {
    let (_dir, _path, store) = seeded_store(2).await;
    let provider = Arc::new(CannedProvider::new("Session summary content"));

    let agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    });

    let summary = agent.summarize_current_session("test-model").await.unwrap();
    assert_eq!(summary, "Session summary content");

    // Fresh empty session
    let empty_agent = Agent::new(
        provider.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "empty_sess".to_string(),
        resume_max_messages: 200,
    });

    assert!(empty_agent
        .summarize_current_session("test-model")
        .await
        .is_err());
}

#[tokio::test]
async fn fork_session_copies_all_messages_and_metadata() {
    let (_tmp, _path, store) = seeded_store(5).await;

    // Set model on s1
    store
        .set_model("s1", "nvidia", "deepseek-v4.1")
        .await
        .unwrap();
    store.set_name("s1", "feature-work").await.unwrap();

    let forked_id = store.fork_session("s1", Some("My Fork")).await.unwrap();

    // Verify record
    let forked_rec = store.get(&forked_id).await.unwrap();
    assert_eq!(forked_rec.title, "My Fork");
    assert_eq!(forked_rec.name, "feature-work");
    assert_eq!(
        forked_rec.model,
        Some(("nvidia".to_string(), "deepseek-v4.1".to_string()))
    );

    // Verify messages
    let orig_replay = store.replay("s1", 100).await.unwrap();
    let forked_replay = store.replay(&forked_id, 100).await.unwrap();

    assert_eq!(orig_replay.total, forked_replay.total);
    assert_eq!(orig_replay.history.len(), forked_replay.history.len());
    for (orig, forked) in orig_replay.history.iter().zip(forked_replay.history.iter()) {
        assert_eq!(orig.message.role, forked.message.role);
        assert_eq!(orig.message.content, forked.message.content);
        assert_eq!(orig.seq, forked.seq);
    }
}

// ---------------------------------------------------------------- the gauge across a fold
/// A fold has to announce the shrunken context itself: `/compact` runs between turns, so nothing
/// recomputes the figure until the next request reports usage, and until then the readout keeps
/// describing the history that was just folded away.
#[tokio::test]
async fn a_fold_announces_the_shrunken_context_without_waiting_for_a_model_call() {
    let (_dir, _path, store) = seeded_store_with_reported_usage(6, 90_000).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = events.clone();

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    })
    .with_observer(Arc::new(move |event| {
        let mut log = recorder.lock().unwrap();
        match event {
            AgentEvent::CompactionStarted { .. } => log.push("started".to_string()),
            AgentEvent::CompactionFinished { folded, error } => log.push(format!(
                "finished folded={} error={}",
                folded.is_some(),
                error.is_some()
            )),
            AgentEvent::ContextTokensUpdated { tokens_used, .. } => {
                log.push(format!("ctx {tokens_used}"))
            }
            _ => {}
        }
    }));

    agent.compact_current_session().await.unwrap();

    let log = events.lock().unwrap().clone();
    assert!(
        log.iter()
            .any(|line| line == "finished folded=true error=false"),
        "the fold itself is reported: {log:?}"
    );
    let finished_at = log
        .iter()
        .position(|line| line.starts_with("finished"))
        .expect("the fold is announced");
    let announced = log[finished_at + 1..]
        .iter()
        .find_map(|line| line.strip_prefix("ctx "))
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(|| panic!("the fold must refresh the gauge, log: {log:?}"));

    // The bound is an order of magnitude, not a number: the bounded transcript is a summary plus two
    // kept turns, and only the pre-fold report was 90k.
    assert!(
        announced < 30_000,
        "the announced figure describes the bounded transcript, got {announced}"
    );
}

/// `/compact` refreshes the gauge through `estimate_session_tokens` *after* the fold, and the kept
/// tail still carries the usage the provider reported for the pre-fold prompt. Preferring that
/// report blindly hands the gauge the size of the history the fold just removed, which is the
/// readout the user is left staring at until the next request.
#[tokio::test]
async fn estimate_after_a_fold_ignores_the_usage_of_the_pre_fold_transcript() {
    let (_dir, _path, store) = seeded_store_with_reported_usage(6, 90_000).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });

    assert_eq!(
        agent.estimate_session_tokens().await.unwrap(),
        90_020,
        "with no fold newer than it, the last reported usage is the size of the session"
    );

    agent.compact_current_session().await.unwrap();

    let after = agent.estimate_session_tokens().await.unwrap();
    assert!(
        after < 30_000,
        "the fold retires the pre-fold figure, got {after}"
    );

    // And it is the live transcript's own estimate, not a number borrowed from the folded rows.
    let replayed = store.replay("s1", 200).await.unwrap();
    let live: Vec<Message> = replayed
        .history
        .iter()
        .map(|stored| stored.message.clone())
        .collect();
    assert_eq!(after, heuristic_tokens(&live));
}

/// The turn loop measures its first request as "reported usage plus the heuristic tail"; the
/// boundary must hand back the same number. Returning only the newest report made the gauge shrink
/// the moment a turn ended, with nothing having left the transcript.
#[tokio::test]
async fn estimate_at_a_turn_boundary_adds_the_tail_after_the_newest_report() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();

    let mut reply = Message::assistant("an answer");
    reply.usage = Some(TokenUsage::estimated(1_000, 200));
    store.append("s1", &reply, false).await.unwrap();
    // A row appended after the report is sent with the next request, but no new report covers it
    // yet.
    store
        .append("s1", &Message::assistant("x".repeat(400)), false)
        .await
        .unwrap();

    let agent = Agent::new(
        Arc::new(RecordingProvider::new("done")),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: Arc::new(store),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    });

    assert_eq!(
        agent.estimate_session_tokens().await.unwrap(),
        1_200 + 100,
        "reported prompt+completion plus the heuristic tail"
    );
}

/// A fold boundary is never the end of the transcript.
///
/// The kept count is a *floor*: when the session holds fewer turns than that, the boundary moves
/// to a token budget and the newest messages stay. Asking to keep more turns than exist is an
/// ordinary request — it is what a freshly configured threshold does on a young session — and it
/// must not fold the conversation away.
#[tokio::test]
async fn a_keep_count_larger_than_the_transcript_still_keeps_the_newest_messages() {
    let mut big = history(3);
    big[0].message.content = Some("x".repeat(500_000));

    let boundary = fold_boundary(&big, Some(20));
    assert!(
        boundary < big.len(),
        "something is foldable in an oversized transcript"
    );
    assert!(
        boundary > 0,
        "but the newest messages are never the ones folded"
    );

    let provider = RecordingProvider::new("FOLDED");
    let outcome = compact(&provider, "canned-model", &big, Some(20))
        .await
        .unwrap()
        .expect("an oversized transcript is foldable whatever the keep count");

    assert!(!outcome.kept.is_empty(), "the kept tail is never empty");
    assert_eq!(
        outcome.summary, "FOLDED",
        "the folded turns are replaced by a summary, not by nothing"
    );
}

#[tokio::test]
async fn compact_accepts_optional_keep_recent_turns_none() {
    let mut big = history(2);
    big[0].message.content = Some("x".repeat(500_000));
    let provider = RecordingProvider::new("FOLDED WITH NONE");

    let outcome = compact(&provider, "canned-model", &big, None)
        .await
        .unwrap()
        .expect("none keep_recent_turns still folds budget-exceeding messages");

    assert_eq!(outcome.summary, "FOLDED WITH NONE");
}
