//! The read path drops DeepSeek's DSML scaffolding, so a session that already stored the noise
//! replays clean.
//!
//! This is the half a unit test cannot cover: the filter is applied when rows come back out of the
//! store, which is what a resumed session renders and what the model receives as history.

use harness_core::session::{SessionStore, SqliteSessionStore, StoredMessage};
use harness_core::types::{Message, Role, ToolCall};
use serde_json::json;

/// The artifact as it was actually stored: one tag naming the block, wrapped in blank lines. The
/// inner bars are U+FF5C, the full-width bar DeepSeek's tokeniser uses.
const DSML_ARTIFACT: &str = "\n\n<\u{FF5C}DSML\u{FF5C} calls>\n\n";

fn text_of(stored: &[StoredMessage]) -> Vec<String> {
    stored
        .iter()
        .filter_map(|entry| entry.message.content.clone())
        .collect()
}

#[tokio::test]
async fn a_stored_dsml_artifact_never_comes_back_out_of_the_store() {
    let store = SqliteSessionStore::in_memory().expect("in-memory store");
    store.ensure_session("dsml").await.expect("session");

    let mut with_calls = Message::assistant(DSML_ARTIFACT);
    with_calls.tool_calls = vec![ToolCall {
        id: "call_1".into(),
        name: "terminal".into(),
        arguments: json!({ "command": "ls" }),
        signature: None,
    }];
    store
        .append("dsml", &with_calls, false)
        .await
        .expect("append");

    let prose = Message::assistant(
        "Installing the CLI now.\n\n<\u{FF5C}DSML\u{FF5C} calls>\n\nThen tests.",
    );
    store.append("dsml", &prose, false).await.expect("append");

    let replayed = store.replay("dsml", 50).await.expect("replay");
    let texts = text_of(&replayed.history);

    for text in &texts {
        assert!(
            !text.contains("DSML") && !text.contains('\u{FF5C}'),
            "scaffolding survived the store: {text:?}"
        );
    }

    // The call itself is untouched, and prose that merely sat next to the noise is kept.
    let call_bearing = replayed
        .history
        .iter()
        .find(|entry| !entry.message.tool_calls.is_empty())
        .expect("the tool call survives");
    assert_eq!(call_bearing.message.tool_calls[0].name, "terminal");
    assert!(
        texts.iter().any(|text| text.contains("Then tests.")),
        "real text must survive the filter: {texts:?}"
    );
}

#[tokio::test]
async fn ordinary_assistant_text_is_returned_verbatim() {
    let store = SqliteSessionStore::in_memory().expect("in-memory store");
    store.ensure_session("plain").await.expect("session");

    let wording = "Keep the generic under 10% and compare ids with a < b.";
    store
        .append("plain", &Message::assistant(wording), false)
        .await
        .expect("append");

    let replayed = store.replay("plain", 50).await.expect("replay");
    assert_eq!(text_of(&replayed.history), vec![wording.to_string()]);
    assert_eq!(replayed.history[0].message.role, Role::Assistant);
}
