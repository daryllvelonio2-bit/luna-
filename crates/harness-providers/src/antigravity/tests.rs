// Everything the tests exercise is reachable from the facade, which re-exports the sibling
// modules' items and binds the two payload translations outright.
use super::*;

use harness_core::{CompletionOptions, Message, ReasoningEffort, Role, ToolCall, ToolResult};
use serde_json::json;

#[test]
fn test_resolve_antigravity_model_mappings() {
    assert_eq!(
        resolve_antigravity_model("gemini-3.8-flash", Some(&ReasoningEffort::High)),
        "gemini-3.8-flash-tiered"
    );
    assert_eq!(
        resolve_antigravity_model("gemini-3.8-flash", None),
        "gemini-3.8-flash-tiered"
    );
    assert_eq!(
        resolve_antigravity_model("antigravity/gemini-3.8-flash", Some(&ReasoningEffort::Low)),
        "gemini-3.8-flash-tiered"
    );
    assert_eq!(
        resolve_antigravity_model("gpt-oss-120b", None),
        "gpt-oss-120b-medium"
    );
    assert_eq!(
        resolve_antigravity_model("claude-sonnet-4-6", None),
        "claude-sonnet-4-6"
    );
    assert_eq!(
        resolve_antigravity_model("claude-opus-4-6", None),
        "claude-opus-4-6-thinking"
    );
}

#[test]
fn test_build_antigravity_payload_merges_tool_responses_and_sets_thought_signature() {
    let messages = vec![
        Message::user("run two tools"),
        Message {
            role: Role::Assistant,
            content: None,
            reasoning: None,
            tool_calls: vec![
                ToolCall {
                    id: "c1".to_string(),
                    name: "read_file".to_string(),
                    arguments: json!({ "path": "test.txt" }),
                    signature: Some("sig-for-read".to_string()),
                },
                ToolCall {
                    id: "c2".to_string(),
                    name: "terminal".to_string(),
                    arguments: json!({ "command": "cargo test" }),
                    signature: Some("sig-for-terminal".to_string()),
                },
            ],
            tool_result: None,
            usage: None,
        },
        Message::tool(harness_core::ToolResult::success("c1", "content")),
        Message::tool(harness_core::ToolResult::success("c2", "test ok")),
    ];

    let payload = build_antigravity_payload(
        &messages,
        &[],
        &CompletionOptions::default(),
        "gemini-3.8-flash",
    );

    let contents = payload["request"]["contents"]
        .as_array()
        .expect("contents array");
    // Must be exactly 3 turns: user, model, user (with both function responses merged)
    assert_eq!(contents.len(), 3);
    assert_eq!(contents[0]["role"], "user");
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[2]["role"], "user");

    // Model turn must carry the model's own signature on each functionCall — the value the
    // provider issued, replayed verbatim.
    let model_parts = contents[1]["parts"].as_array().expect("model parts");
    assert_eq!(model_parts.len(), 2);
    assert_eq!(model_parts[0]["thoughtSignature"], json!("sig-for-read"));
    assert_eq!(
        model_parts[1]["thoughtSignature"],
        json!("sig-for-terminal")
    );

    // User turn must carry both functionResponses in the single turn
    let user_parts = contents[2]["parts"].as_array().expect("user parts");
    assert_eq!(user_parts.len(), 2);
    assert_eq!(user_parts[0]["functionResponse"]["name"], "read_file");
    assert_eq!(user_parts[1]["functionResponse"]["name"], "terminal");
}

/// A call recorded without a signature (another provider wrote the rows, or the session
/// predates the field) is replayed with Google's documented dummy on a model that validates
/// signatures: omitting the field is what 400s the request, and the dummy makes the validator
/// skip the check instead.
#[test]
fn a_signatureless_call_carries_the_documented_dummy_on_a_strict_model() {
    let messages = signatureless_call_conversation();

    let payload = build_antigravity_payload(
        &messages,
        &[],
        &CompletionOptions::default(),
        "gemini-3.8-flash",
    );
    let contents = payload["request"]["contents"].as_array().expect("contents");
    let model_parts = contents[1]["parts"].as_array().expect("model parts");
    assert_eq!(
        model_parts[0]["thoughtSignature"],
        json!("skip_thought_signature_validator"),
        "a strict model rejects a function call replayed without a signature"
    );
}

/// A model that does not validate signatures is sent no `thoughtSignature` at all: the dummy
/// is a documented last resort for strict validation, and Google notes it degrades model
/// performance, so it must not be volunteered to a model that never asks for one.
#[test]
fn a_signatureless_call_sends_no_field_to_a_non_validating_model() {
    let messages = signatureless_call_conversation();

    let payload = build_antigravity_payload(
        &messages,
        &[],
        &CompletionOptions::default(),
        "gemini-2.5-flash",
    );
    let contents = payload["request"]["contents"].as_array().expect("contents");
    let model_parts = contents[1]["parts"].as_array().expect("model parts");
    assert!(
        model_parts[0].get("thoughtSignature").is_none(),
        "no signature concept for this model: {:?}",
        model_parts[0]
    );
}

/// The other side of the same rule: a non-Gemini model on Code Assist (Claude, GPT) has no
/// signature concept either, even though the endpoint is Google's.
#[test]
fn a_signatureless_call_sends_no_field_to_a_non_gemini_model() {
    let messages = signatureless_call_conversation();

    let payload = build_antigravity_payload(
        &messages,
        &[],
        &CompletionOptions::default(),
        "claude-sonnet-4-6",
    );
    let contents = payload["request"]["contents"].as_array().expect("contents");
    let model_parts = contents[1]["parts"].as_array().expect("model parts");
    assert!(model_parts[0].get("thoughtSignature").is_none());
}

/// Every system message reaches the request, in order.
///
/// The vector carries two by design: the session's system prompt, then the synthetic
/// compaction summary. Keeping only the last one — the summary — drops the prompt from the
/// request, so the persona, the rules and the tool guidance vanish the moment a session
/// compacts, and the folded history is read as instructions.
#[test]
fn both_system_messages_reach_the_system_instruction() {
    let messages = vec![
        Message::system("You are Mahiru, and you never lie about a test"),
        Message::system("Summary of the earlier conversation: the user asked for X"),
        Message::user("continue"),
    ];

    let payload = build_antigravity_payload(
        &messages,
        &[],
        &CompletionOptions::default(),
        "gemini-2.5-flash",
    );
    let instruction = &payload["request"]["systemInstruction"];
    let text = instruction["parts"][0]["text"]
        .as_str()
        .expect("the system instruction carries text");
    assert!(
        text.contains("You are Mahiru"),
        "the system prompt survives a compaction summary: {text}"
    );
    assert!(
        text.contains("Summary of the earlier conversation"),
        "the summary survives beside it: {text}"
    );
    assert!(
        text.find("You are Mahiru") < text.find("Summary of the earlier"),
        "the prompt comes first: {text}"
    );
}

/// One user turn, one model turn that called a tool without a signature, and the result.
fn signatureless_call_conversation() -> Vec<Message> {
    vec![
        Message::user("run one tool"),
        Message {
            role: Role::Assistant,
            content: None,
            reasoning: None,
            tool_calls: vec![ToolCall {
                id: "c1".to_string(),
                name: "terminal".to_string(),
                arguments: json!({ "command": "ls" }),
                signature: None,
            }],
            tool_result: None,
            usage: None,
        },
        Message::tool(harness_core::ToolResult::success("c1", "ok")),
    ]
}

/// A reasoning-bearing assistant turn replays its thinking as a thought part before the
/// functionCall, so the replayed history says what the model actually produced.
#[test]
fn a_reasoning_turn_replays_a_thought_part_before_its_call() {
    let messages = vec![
        Message::user("run one tool"),
        Message {
            role: Role::Assistant,
            content: None,
            reasoning: Some("I should list the files first.".to_string()),
            tool_calls: vec![ToolCall {
                id: "c1".to_string(),
                name: "list_files".to_string(),
                arguments: json!({ "path": "." }),
                signature: Some("sig-1".to_string()),
            }],
            tool_result: None,
            usage: None,
        },
        Message::tool(harness_core::ToolResult::success("c1", "a\nb")),
    ];

    let payload = build_antigravity_payload(
        &messages,
        &[],
        &CompletionOptions::default(),
        "gemini-3.8-flash",
    );
    let contents = payload["request"]["contents"].as_array().expect("contents");
    let model_parts = contents[1]["parts"].as_array().expect("model parts");
    assert_eq!(model_parts.len(), 2);
    assert_eq!(model_parts[0]["thought"], json!(true));
    assert_eq!(
        model_parts[0]["text"],
        json!("I should list the files first.")
    );
    assert_eq!(model_parts[1]["thoughtSignature"], json!("sig-1"));
}

#[test]
fn format_quota_display_separates_gemini_and_claude_groups() {
    let mut models_quota = std::collections::HashMap::new();
    models_quota.insert(
        "gemini-3.8-flash-tiered".to_string(),
        AntigravityModelQuotaInfo {
            remaining_fraction: Some(0.0628),
            reset_time: None,
        },
    );

    let summary = AntigravityQuotaSummary {
        groups: vec![AntigravityQuotaGroup {
            display_name: Some("Claude and GPT models".to_string()),
            buckets: vec![
                AntigravityQuotaBucket {
                    bucket_id: Some("3p-5h".to_string()),
                    display_name: Some("Five Hour Limit Remaining".to_string()),
                    window: Some("5h".to_string()),
                    reset_time: None,
                    remaining_fraction: Some(1.0),
                },
                AntigravityQuotaBucket {
                    bucket_id: Some("3p-weekly".to_string()),
                    display_name: Some("Weekly Limit Remaining".to_string()),
                    window: Some("weekly".to_string()),
                    reset_time: None,
                    remaining_fraction: Some(0.46),
                },
            ],
        }],
        models_quota,
    };

    // When active model is gemini, it must select the 6% quotaInfo from models_quota as the 5h limit
    let gemini_fmt = summary
        .format_quota_display("gemini-3.8-flash")
        .expect("gemini quota");
    assert_eq!(gemini_fmt, "5h 6%");

    // When active model is claude, it must select the Claude and GPT models group (5h 100% · w 46%)
    let claude_fmt = summary
        .format_quota_display("claude-sonnet-4-6")
        .expect("claude quota");
    assert_eq!(claude_fmt, "5h 100% · w 46%");
}

#[test]
fn format_quota_display_gemini_with_both_5h_and_weekly() {
    let mut models_quota = std::collections::HashMap::new();
    models_quota.insert(
        "gemini-2.5-flash".to_string(),
        AntigravityModelQuotaInfo {
            remaining_fraction: Some(0.93),
            reset_time: Some("2026-10-24T17:41:46Z".to_string()),
        },
    );

    let summary = AntigravityQuotaSummary {
        groups: vec![AntigravityQuotaGroup {
            display_name: Some("Gemini models".to_string()),
            buckets: vec![AntigravityQuotaBucket {
                bucket_id: Some("gemini-weekly".to_string()),
                display_name: Some("Weekly Limit".to_string()),
                window: Some("weekly".to_string()),
                reset_time: None,
                remaining_fraction: Some(0.85),
            }],
        }],
        models_quota,
    };

    let gemini_fmt = summary
        .format_quota_display("gemini-2.5-flash")
        .expect("gemini quota");
    assert!(gemini_fmt.starts_with("5h 93%"));
    assert!(gemini_fmt.contains("· w 85%"));
}

#[test]
fn build_antigravity_payload_ensures_valid_turn_order_even_if_opened_with_assistant_tool_call() {
    let call = ToolCall {
        id: "call-xyz".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "src/main.rs"}),
        signature: None,
    };
    // Simulated history that opens with an assistant message issuing tool calls (e.g. from compacted history)
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: None,
            tool_calls: vec![call],
            tool_result: None,
            usage: None,
            reasoning: None,
        },
        Message {
            role: Role::Tool,
            content: None,
            tool_calls: vec![],
            tool_result: Some(ToolResult::success("call-xyz", "fn main() {}")),
            usage: None,
            reasoning: None,
        },
        Message {
            role: Role::User,
            content: Some("next task".to_string()),
            tool_calls: vec![],
            tool_result: None,
            usage: None,
            reasoning: None,
        },
    ];

    let payload = build_antigravity_payload(
        &messages,
        &[],
        &CompletionOptions::default(),
        "gemini-3.8-flash",
    );
    let contents = payload["request"]["contents"]
        .as_array()
        .expect("contents array");

    // 1. Must start with a user turn
    assert_eq!(contents[0]["role"], "user", "turn 0 must be user");

    // 2. Turn 1 is the model turn containing functionCall
    assert_eq!(contents[1]["role"], "model", "turn 1 is model");
    let has_fc = contents[1]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p.get("functionCall").is_some());
    assert!(has_fc, "turn 1 has functionCall");

    // 3. Turn 2 is user turn with functionResponse
    assert_eq!(contents[2]["role"], "user", "turn 2 is user");
    let has_fr = contents[2]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p.get("functionResponse").is_some());
    assert!(has_fr, "turn 2 has functionResponse");

    // 4. Turn 3 is the next user turn merged into turn 2 or strictly alternating
    for (i, turn) in contents.iter().enumerate() {
        if i > 0 {
            assert_ne!(
                contents[i - 1]["role"],
                turn["role"],
                "adjacent turns must strictly alternate roles at index {}",
                i
            );
        }
    }
}

#[test]
fn build_antigravity_payload_matches_most_recent_tool_call_on_id_collision() {
    // If a call ID is reused across turns (e.g. random 6-digit collision in a long session,
    // or synthetic defaults like call_0), the tool result must pair with the most recent
    // assistant call, not the first one from the beginning of the transcript.
    let call1 = ToolCall {
        id: "call_collision_123".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "file1.rs"}),
        signature: None,
    };
    let call2 = ToolCall {
        id: "call_collision_123".to_string(),
        name: "search_files".to_string(),
        arguments: serde_json::json!({"pattern": "foo"}),
        signature: None,
    };

    let messages = vec![
        Message {
            role: Role::User,
            content: Some("first question".to_string()),
            tool_calls: vec![],
            tool_result: None,
            usage: None,
            reasoning: None,
        },
        Message {
            role: Role::Assistant,
            content: None,
            tool_calls: vec![call1],
            tool_result: None,
            usage: None,
            reasoning: None,
        },
        Message {
            role: Role::Tool,
            content: None,
            tool_calls: vec![],
            tool_result: Some(ToolResult::success("call_collision_123", "content 1")),
            usage: None,
            reasoning: None,
        },
        Message {
            role: Role::Assistant,
            content: Some("done with first".to_string()),
            tool_calls: vec![],
            tool_result: None,
            usage: None,
            reasoning: None,
        },
        Message {
            role: Role::User,
            content: Some("second question".to_string()),
            tool_calls: vec![],
            tool_result: None,
            usage: None,
            reasoning: None,
        },
        Message {
            role: Role::Assistant,
            content: None,
            tool_calls: vec![call2],
            tool_result: None,
            usage: None,
            reasoning: None,
        },
        Message {
            role: Role::Tool,
            content: None,
            tool_calls: vec![],
            tool_result: Some(ToolResult::success("call_collision_123", "content 2")),
            usage: None,
            reasoning: None,
        },
    ];

    let payload = build_antigravity_payload(
        &messages,
        &[],
        &CompletionOptions::default(),
        "gemini-3.8-flash",
    );
    let contents = payload["request"]["contents"]
        .as_array()
        .expect("contents array");

    // Find the functionResponse parts in the generated payload
    let mut fr_names = Vec::new();
    for turn in contents {
        if let Some(parts) = turn["parts"].as_array() {
            for part in parts {
                if let Some(fr) = part.get("functionResponse") {
                    if let Some(name) = fr.get("name").and_then(|n| n.as_str()) {
                        fr_names.push(name.to_string());
                    }
                }
            }
        }
    }

    assert_eq!(
        fr_names,
        vec!["read_file", "search_files"],
        "Second tool result must be named 'search_files', not the older 'read_file'"
    );
}

/// The line that makes a content-free answer diagnosable: it has to say what arrived, and it must
/// never quote what the model said — a log line is not a transcript.
#[test]
fn a_frame_is_described_by_its_shape_and_never_by_its_text() {
    let text_frame = json!({
        "response": {
            "candidates": [{ "content": { "parts": [{ "text": "the secret answer" }] } }],
            "usageMetadata": { "promptTokenCount": 12345 }
        }
    });
    let described = describe_frame(&text_frame);
    assert_eq!(
        described,
        "candidates=1 finish=- block=- parts=[text] promptTokens=12345"
    );
    assert!(!described.contains("secret"), "{described}");

    let call_frame = json!({
        "candidates": [{
            "finishReason": "STOP",
            "content": { "parts": [{ "functionCall": { "name": "terminal" } }] }
        }]
    });
    assert_eq!(
        describe_frame(&call_frame),
        "candidates=1 finish=STOP block=- parts=[functionCall]"
    );

    // A reasoning part and an unrecognised part are both worth telling apart from text.
    let mixed = json!({
        "response": { "candidates": [{ "content": { "parts": [{ "thought": true }, { "inlineData": {} }] } }] }
    });
    assert_eq!(
        describe_frame(&mixed),
        "candidates=1 finish=- block=- parts=[thought,other]"
    );

    // The frame that explains an empty answer: nothing at all.
    assert_eq!(
        describe_frame(&json!({})),
        "candidates=0 finish=- block=- parts=[]"
    );
}

#[test]
fn a_refused_prompt_is_reported_as_refused() {
    let blocked = json!({ "response": { "promptFeedback": { "blockReason": "SAFETY" } } });
    assert_eq!(blocked_reason(&blocked), Some("SAFETY"));
    // Same reason, unwrapped, and the ordinary frame that must not be mistaken for one.
    assert_eq!(
        blocked_reason(&json!({ "promptFeedback": { "blockReason": "OTHER" } })),
        Some("OTHER")
    );
    assert_eq!(
        blocked_reason(&json!({ "response": { "candidates": [] } })),
        None
    );
}

/// The message is the fix: it has to name the request it is about and something the reader can do.
#[test]
fn an_empty_answer_says_what_was_asked_and_what_to_do() {
    let error = no_answer_error(std::time::Duration::from_secs(52), 1706, 21, Some(84_000));
    let text = error.to_string();
    assert!(text.contains("52s"), "{text}");
    assert!(text.contains("1706 messages"), "{text}");
    assert!(text.contains("21 tools"), "{text}");
    assert!(text.contains("84000 prompt tokens"), "{text}");
    assert!(
        text.contains("/compact"),
        "the reader needs a way out: {text}"
    );
    assert!(
        !text.contains("may be overloaded"),
        "no condition the caller cannot see: {text}"
    );

    // Without a usage report it still says the shape of the request rather than nothing.
    let bare = no_answer_error(std::time::Duration::from_secs(3), 2, 0, None).to_string();
    assert!(bare.contains("2 messages and 0 tools"), "{bare}");
    assert!(!bare.contains("prompt tokens"), "{bare}");
}
