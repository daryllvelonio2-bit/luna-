use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use harness_core::*;

mod common;
use common::*;

#[tokio::test]
async fn auto_compact_folds_in_the_background_and_the_next_request_carries_the_summary() {
    let (_dir, path, store) = seeded_store(6).await;
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED BY AUTO-COMPACT"));

    let mut cfg = agent_config();
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 10; // fires on the first turn
    cfg.auto_compact_turns = 2;

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
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
    assert_eq!(
        main.last_request().len(),
        14,
        "the turn that tripped the threshold did not wait for the fold"
    );

    // The fold reports itself when it is done, off the turn's path.
    folded.notified().await;
    assert_eq!(summariser.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "the folded turns left a summary behind"
    );
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 8);

    // And the next turn is the one that sends the bounded transcript.
    let second = agent
        .run_turn("second", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(second.reply, "done");
    let sent = main.last_request();
    assert!(
        sent.iter()
            .any(|message| message.content.as_deref() == Some("FOLDED BY AUTO-COMPACT")),
        "the next request carries the summary"
    );
    assert!(
        sent.len() < 14,
        "and it is smaller than the transcript it replaced: {} messages",
        sent.len()
    );
    assert_eq!(sent[sent.len() - 1].content.as_deref(), Some("second"));
}

/// Answers only after a delay, so a fold spawned at the start of a turn has certainly finished by
/// the time that turn's own checks run again.
struct SlowProvider {
    delay: std::time::Duration,
}

impl SlowProvider {
    fn new(delay_ms: u64) -> Self {
        Self {
            delay: std::time::Duration::from_millis(delay_ms),
        }
    }
}

#[async_trait]
impl ModelProvider for SlowProvider {
    fn provider_id(&self) -> &str {
        "slow"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["slow-model".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        tokio::time::sleep(self.delay).await;
        Ok(Message::assistant("done"))
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(Vec::new())
    }
}

/// A turn that crosses the bar on its own reply folds by itself, with no second user message.
///
/// The check at the top of a turn only runs when a turn starts, so a transcript that grew past the
/// threshold *during* a turn stayed unfolded until the user came back — which reads as an
/// auto-compaction that only works when you prompt it. The turn's own reply is weight enough to trip
/// the bar, so the turn that wrote it is the one that starts the fold.
#[tokio::test]
async fn auto_compact_folds_when_the_turn_itself_crosses_the_bar() {
    let (_dir, path, store) = seeded_store(3).await;
    let main = Arc::new(RecordingProvider::new(&"x".repeat(2_000_000)));
    let summariser = Arc::new(CannedProvider::new("FOLDED WITHOUT BEING ASKED"));

    let mut cfg = agent_config();
    // The long-standing bar stays out of the way, and the auto-compact bar sits between what the
    // session weighs before this turn and what it weighs after it.
    cfg.compact_at_tokens = 500_000;
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 100_000;
    cfg.auto_compact_turns = 1;

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
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

    // One turn, and only one: nothing else is sent to the agent.
    let outcome = agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome.reply.chars().count(), 2_000_000);
    assert_eq!(
        main.call_count(),
        1,
        "the turn ran on the transcript it had; the fold is not part of it"
    );

    tokio::time::timeout(std::time::Duration::from_secs(10), folded.notified())
        .await
        .expect("the turn must start the fold itself, without another user message");

    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "one summarising call"
    );
    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "the fold wrote its summary"
    );
    assert!(
        count_rows(&path, "compacted_at IS NOT NULL") > 0,
        "and it folded rows rather than only marking the session"
    );
}

/// An automatic fold is a bound on the *next* request, not an announcement about the current turn.
/// Until the running turn adopts it, the turn still holds the transcript it replaced, so the fold
/// must not hand the gauge its shrunken size: the turn's own next report would restore the larger
/// figure, which is the drop-then-jump the gauge showed around a soft interrupt. Once the turn does
/// adopt the fold (covered by `auto_compact_does_not_repeat_and_updates_context_gauge_mid_turn`),
/// the measurement is honest and resets immediately.
#[tokio::test]
async fn an_unadopted_automatic_fold_does_not_reset_the_gauge_mid_turn() {
    let (_dir, _path, store) = seeded_store(3).await;
    let main = Arc::new(RecordingProvider::new(&"x".repeat(2_000_000)));
    let summariser = Arc::new(CannedProvider::new("FOLDED WITHOUT BEING ASKED"));

    let mut cfg = agent_config();
    cfg.compact_at_tokens = 500_000;
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 100_000;
    cfg.auto_compact_turns = 1;

    let mut agent = Agent::new(
        main,
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser,
        model: "canned-model".to_string(),
    });

    let folded = Arc::new(tokio::sync::Notify::new());
    let folded_obs = folded.clone();
    let updates: Arc<Mutex<Vec<(u32, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let updates_obs = updates.clone();
    agent.set_observer(Arc::new(move |event| match event {
        AgentEvent::CompactionFinished {
            folded: Some(_), ..
        } => folded_obs.notify_one(),
        AgentEvent::ContextTokensUpdated { tokens_used, reset } => {
            updates_obs.lock().unwrap().push((tokens_used, reset));
        }
        _ => {}
    }));

    agent
        .run_turn("hello", CancellationToken::new())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), folded.notified())
        .await
        .expect("the automatic fold must complete");

    let updates = updates.lock().unwrap().clone();
    assert!(!updates.is_empty(), "the turn reported its own size");
    assert!(
        updates.iter().all(|(_, reset)| !reset),
        "an automatic fold must not reset the gauge mid-turn: {updates:?}"
    );
}

/// One fold per turn — even though the turn's own request still measures the *unfolded* transcript.
///
/// A fold that lands while a turn runs rewrites the stored history, and the turn's remaining
/// model calls go on reporting their own (stale, larger) size. The re-armed check measures the
/// STORED transcript, so it folds again only when the store itself is still over the bar — never
/// because of a stale in-memory report, and never folding the summary it just wrote.
#[tokio::test]
async fn a_turn_folds_once_even_when_its_reports_still_describe_the_unfolded_transcript() {
    let (_dir, path, store) = seeded_store_with_reported_usage(6, 20_000).await;
    // Slow enough that the fold from the top of the turn has finished before the turn ends, which is
    // the state the check has to get right: nothing in flight, and a transcript already rewritten.
    let main = Arc::new(SlowProvider::new(300));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let mut cfg = agent_config();
    // The bar sits between the seeded transcript's REPORTED size (20k, crossed before the turn's
    // own work starts) and the folded store (a summary plus the 2 kept turns plus the system
    // prompt ≈ 3.3k). The re-armed check therefore finds the store under the bar and answers the
    // crossing exactly once.
    cfg.compact_at_tokens = 5_000;
    cfg.auto_compact_enabled = false;
    cfg.keep_recent_turns = 2;

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
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

    folded.notified().await;
    // Room for a second fold to land, were one going to.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert_eq!(
        summariser.calls.load(Ordering::SeqCst),
        1,
        "the crossing was folded once"
    );
    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "one summary, not a summary of the summary"
    );
}

/// The configured turn count is how many recent turns to *keep*, and it may exceed what the
/// session holds — that is the ordinary state of a young session on a low threshold. This is the
/// shape that used to take the whole transcript with it: no cutoff row was found, so every message
/// was marked folded, and the next replay had nothing to hand the model.
#[tokio::test]
async fn auto_compact_never_empties_a_session_that_holds_fewer_turns_than_it_keeps() {
    let (_dir, path, store) = temp_store();
    store.ensure_session("s_auto").await.unwrap();
    // Three turns, the first of them big enough that the transcript is over any budget.
    store
        .append(
            "s_auto",
            &Message::user(format!("opening question {}", "x".repeat(500_000))),
            false,
        )
        .await
        .unwrap();
    store
        .append("s_auto", &Message::assistant("first answer"), false)
        .await
        .unwrap();
    store
        .append("s_auto", &Message::user("second question"), false)
        .await
        .unwrap();
    store
        .append("s_auto", &Message::assistant("second answer"), false)
        .await
        .unwrap();

    let store = Arc::new(store);
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED"));

    let mut cfg = agent_config();
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 10;
    cfg.auto_compact_turns = 20; // more turns than this session has

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s_auto".to_string(),
        resume_max_messages: 200,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });
    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    agent
        .run_turn("new prompt", CancellationToken::new())
        .await
        .unwrap();
    folded.notified().await;

    let live = store.replay("s_auto", 100).await.unwrap();
    assert!(
        live.total > 0,
        "the session still holds the conversation: {} live messages",
        live.total
    );
    assert!(
        live.history.iter().any(|stored| stored.is_summary),
        "what was folded is represented by a summary"
    );
    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "exactly one fold happened, and it wrote its summary"
    );

    // The newest turns survived verbatim, and the model was sent them.
    let sent = main.last_request();
    assert!(
        sent.iter()
            .any(|message| message.content.as_deref() == Some("second question")),
        "the most recent turns are still in the request"
    );
}

#[tokio::test]
async fn fold_boundary_tokens_lands_on_budget_in_large_transcript() {
    use harness_core::session::fold_boundary_tokens;

    let (_dir, _path, store) = temp_store();
    store.ensure_session("s1").await.unwrap();
    // 20 turns, each with ~10k chars (2.5k tokens) => total ~50k tokens
    for i in 0..20 {
        store
            .append(
                "s1",
                &Message::user(format!("user turn {i} {}", "a".repeat(10_000))),
                false,
            )
            .await
            .unwrap();
        store
            .append(
                "s1",
                &Message::assistant(format!("assistant turn {i} {}", "b".repeat(10_000))),
                false,
            )
            .await
            .unwrap();
    }

    let replay = store.replay("s1", 200).await.unwrap();
    let history = replay.history;

    // Compact to 20k tokens: should fold older turns and keep ~20k tokens
    let boundary = fold_boundary_tokens(&history, 20_000);
    assert!(boundary > 0);
    assert_eq!(history[boundary].message.role, Role::User);

    let kept_tokens: usize = history[boundary..]
        .iter()
        .map(|stored| heuristic_tokens(std::slice::from_ref(&stored.message)) as usize)
        .sum();
    assert!(
        kept_tokens <= 25_000,
        "kept {kept_tokens} tokens, expected around 20k"
    );
}

#[tokio::test]
async fn fold_boundary_percent_respects_target_percentage() {
    use harness_core::session::fold_boundary_percent;

    let (_dir, _path, store) = seeded_store(10).await;
    let replay = store.replay("s1", 200).await.unwrap();
    let history = replay.history;
    assert_eq!(history.len(), 20); // 10 user, 10 assistant

    // 50% target should cut at a clean user turn around the middle
    let boundary_50 = fold_boundary_percent(&history, 50);
    assert!(boundary_50 > 0 && boundary_50 < history.len());
    assert_eq!(history[boundary_50].message.role, Role::User);

    // 80% target should fold more messages (boundary further to the right)
    let boundary_80 = fold_boundary_percent(&history, 80);
    assert!(boundary_80 >= boundary_50);
    assert_eq!(history[boundary_80].message.role, Role::User);

    // 10% target should fold fewer messages (boundary further to the left)
    let boundary_10 = fold_boundary_percent(&history, 10);
    assert!(boundary_10 <= boundary_50);
    if boundary_10 > 0 {
        assert_eq!(history[boundary_10].message.role, Role::User);
    }
}

#[tokio::test]
async fn auto_compact_percentage_folds_target_portion_of_transcript() {
    let (_dir, path, store) = seeded_store(6).await; // 12 messages: 6 turns
    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED WITH PERCENT"));

    let mut cfg = agent_config();
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 10;
    cfg.auto_compact_percent = Some(50); // fold approximately 50%

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
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

    folded.notified().await;
    assert_eq!(summariser.calls.load(Ordering::SeqCst), 1);
    assert_eq!(count_rows(&path, "is_summary = 1"), 1);

    // 50% of 12 messages = 6 messages folded
    assert_eq!(count_rows(&path, "compacted_at IS NOT NULL"), 6);

    let second = agent
        .run_turn("second", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(second.reply, "done");
    let sent = main.last_request();
    assert!(
        sent.iter()
            .any(|m| m.content.as_deref() == Some("FOLDED WITH PERCENT")),
        "carries percentage-based summary"
    );
}

/// The configured percentage is a share of the transcript, and that is where the fold must land.
///
/// `auto_compact_percent: 50` means the next request carries about half of what the last one did. A
/// fold that takes more than it was asked to is not a smaller fold: it is the remembered
/// conversation, gone. Ten percent is what this looked like in the wild — a session that went from
/// 180k tokens to 11k on its next prompt.
#[tokio::test]
async fn auto_compact_leaves_the_share_it_was_configured_to_leave() {
    use harness_core::session::{heuristic_tokens, sendable_messages};

    let (_dir, path, raw_store) = temp_store();
    let store = Arc::new(raw_store);
    store.ensure_session("s1").await.unwrap();
    // Eight turns of ~12k tokens: 96k of transcript, so a 50% fold has real room to be exact in.
    for turn in 0..8 {
        store
            .append(
                "s1",
                &Message::user(format!("question {turn} {}", "q".repeat(24_000))),
                false,
            )
            .await
            .unwrap();
        store
            .append(
                "s1",
                &Message::assistant(format!("answer {turn} {}", "a".repeat(24_000))),
                false,
            )
            .await
            .unwrap();
    }
    let before = heuristic_tokens(&sendable_messages(
        &store.replay("s1", 500).await.unwrap().history,
    ));

    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED HALF"));

    let mut cfg = agent_config();
    cfg.auto_compact_enabled = true;
    cfg.auto_compact_threshold = 40_000;
    cfg.auto_compact_percent = Some(50);

    let mut agent = Agent::new(
        main.clone(),
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        cfg,
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 500,
    })
    .with_compaction(CompactionSetup {
        provider: summariser.clone(),
        model: "canned-model".to_string(),
    });

    let (observer, folded) = fold_finished();
    agent.set_observer(observer);

    agent
        .run_turn("the new question", CancellationToken::new())
        .await
        .unwrap();
    folded.notified().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert_eq!(
        count_rows(&path, "is_summary = 1"),
        1,
        "one crossing, one fold"
    );
    let after = heuristic_tokens(&sendable_messages(
        &store.replay("s1", 500).await.unwrap().history,
    ));
    let share = after as f64 / before as f64;
    assert!(
        (0.35..=0.65).contains(&share),
        "a 50% fold must leave about half the transcript: {after} of {before} ({:.0}%)",
        share * 100.0
    );
}

/// Regression test for the hydrogen `/compact` doubling bug: live `assistant`/`tool` rows left
/// after a mid-turn fold (with no `user` row before them) must never be hidden from replay or
/// compaction when a later `user` message exists, and must never resurface to inflate tokens
/// after `/compact` removes the remaining `user` rows.
#[tokio::test]
async fn compact_with_pre_user_live_rows_never_hides_or_doubles_context() {
    let (_dir, _path, store) = temp_store();
    let store = Arc::new(store);
    store.ensure_session("s1").await.unwrap();

    // Older turn that gets compacted into a summary.
    let old_user_id = store
        .append("s1", &Message::user("old prompt"), false)
        .await
        .unwrap();
    let old_asst_id = store
        .append("s1", &Message::assistant("old reply"), false)
        .await
        .unwrap();
    store
        .record_compaction("s1", &[old_user_id, old_asst_id], "prior summary")
        .await
        .unwrap();

    // Live post-compaction assistant/tool rounds whose initiating user prompt was already folded.
    // Give them substantial size (~120k chars = ~30k tokens) so hiding/resurfacing is dramatic.
    for i in 0..3 {
        let call_id = format!("pre_call_{i}");
        store
            .append(
                "s1",
                &Message::assistant_with_tools(vec![ToolCall {
                    id: call_id.clone(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({ "path": format!("file_{i}.rs") }),
                    signature: None,
                }]),
                false,
            )
            .await
            .unwrap();
        store
            .append(
                "s1",
                &Message::tool(ToolResult::success(
                    call_id,
                    format!("content_{i}: {}", "a".repeat(38_000)),
                )),
                false,
            )
            .await
            .unwrap();
    }

    // Later user turn (~80k chars = ~20k tokens) + kept tail (~40k chars = ~10k tokens).
    store
        .append("s1", &Message::user("later user turn"), false)
        .await
        .unwrap();
    for i in 0..2 {
        let call_id = format!("mid_call_{i}");
        store
            .append(
                "s1",
                &Message::assistant_with_tools(vec![ToolCall {
                    id: call_id.clone(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({ "path": format!("mid_{i}.rs") }),
                    signature: None,
                }]),
                false,
            )
            .await
            .unwrap();
        store
            .append(
                "s1",
                &Message::tool(ToolResult::success(
                    call_id,
                    format!("mid_{i}: {}", "b".repeat(40_000)),
                )),
                false,
            )
            .await
            .unwrap();
    }
    store
        .append(
            "s1",
            &Message::assistant(format!("tail reply {}", "c".repeat(40_000))),
            false,
        )
        .await
        .unwrap();

    // Replay within limit must include every live row, including the pre-user ones.
    let replayed = store.replay("s1", 200).await.unwrap();
    assert_eq!(replayed.replayed, replayed.total);
    assert!(
        replayed
            .history
            .iter()
            .any(|m| m.message.tool_calls.iter().any(|c| c.id == "pre_call_0")),
        "pre-user live rows must remain visible in replay"
    );

    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("FOLDED_ALL_OLD"));
    let make_agent = || {
        Agent::new(
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
    };

    let agent = make_agent();
    let before = agent.estimate_session_tokens().await.unwrap();
    agent.compact_current_session().await.unwrap();
    let after_live = agent.estimate_session_tokens().await.unwrap();

    // Restarting a fresh agent on the same DB must report the exact same shrunken token count.
    let restarted = make_agent();
    let after_restart = restarted.estimate_session_tokens().await.unwrap();

    assert!(
        after_live < before,
        "compaction must shrink token estimate, not double it: before={before}, after={after_live}"
    );
    assert_eq!(
        after_restart, after_live,
        "restarted session must match post-compaction token estimate"
    );

    let post_replay = store.replay("s1", 200).await.unwrap();
    assert!(
        !post_replay.history.iter().any(|m| m
            .message
            .tool_calls
            .iter()
            .any(|c| c.id == "pre_call_0")),
        "pre-user live rows must be folded by /compact rather than left stranded"
    );
}

/// `/compact` must compact the full live session history, even when the live row count exceeds
/// `resume_max_messages`.
#[tokio::test]
async fn compact_current_session_folds_beyond_resume_max_messages() {
    let (_dir, _path, store) = temp_store();
    let store = Arc::new(store);
    store.ensure_session("s1").await.unwrap();

    for turn in 0..8 {
        store
            .append(
                "s1",
                &Message::user(format!("turn {turn} {}", "x".repeat(40_000))),
                false,
            )
            .await
            .unwrap();
        store
            .append(
                "s1",
                &Message::assistant(format!("reply {turn} {}", "y".repeat(40_000))),
                false,
            )
            .await
            .unwrap();
    }

    let main = Arc::new(RecordingProvider::new("done"));
    let summariser = Arc::new(CannedProvider::new("COMPACTED_ALL"));
    let agent = Agent::new(
        main,
        Arc::new(MockMemory),
        Arc::new(MockNotes),
        Arc::new(DefaultToolRegistry::new()),
        agent_config(),
    )
    .with_session(SessionBinding {
        store: store.clone(),
        session_id: "s1".to_string(),
        resume_max_messages: 4,
    })
    .with_compaction(CompactionSetup {
        provider: summariser,
        model: "canned-model".to_string(),
    });

    agent.compact_current_session().await.unwrap();

    let all_live = store.replay("s1", usize::MAX).await.unwrap();
    assert!(
        !all_live.history.iter().any(|m| {
            m.message
                .content
                .as_deref()
                .is_some_and(|c| c.starts_with("turn 0 "))
        }),
        "oldest turn beyond resume_max_messages must be folded too"
    );
}

#[tokio::test]
async fn most_recent_ignores_empty_sessions() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s_empty").await.unwrap();
    assert_eq!(store.most_recent().await.unwrap(), None);

    store.ensure_session("s_convo").await.unwrap();
    store
        .append("s_convo", &Message::user("hello"), false)
        .await
        .unwrap();
    assert_eq!(
        store.most_recent().await.unwrap().as_deref(),
        Some("s_convo")
    );

    // Creating another empty session should NOT displace s_convo
    store.ensure_session("s_empty_2").await.unwrap();
    assert_eq!(
        store.most_recent().await.unwrap().as_deref(),
        Some("s_convo")
    );
}

#[tokio::test]
async fn prune_empty_sessions_removes_zero_message_sessions() {
    let (_dir, _path, store) = temp_store();
    store.ensure_session("s_empty1").await.unwrap();
    store.ensure_session("s_empty2").await.unwrap();
    store.ensure_session("s_kept").await.unwrap();
    store
        .append("s_kept", &Message::user("hi"), false)
        .await
        .unwrap();

    let pruned = store.prune_empty_sessions(Some("s_empty2")).await.unwrap();
    assert_eq!(pruned, 1);
    assert!(store.get("s_empty1").await.is_err());
    assert!(store.get("s_empty2").await.is_ok());
    assert!(store.get("s_kept").await.is_ok());

    let pruned_all = store.prune_empty_sessions(None).await.unwrap();
    assert_eq!(pruned_all, 1);
    assert!(store.get("s_empty2").await.is_err());
    assert!(store.get("s_kept").await.is_ok());
}
