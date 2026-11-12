//! The behaviour of the Codex route that can be checked without the network: reading a login out
//! of a Codex `auth.json`, the two translations, and what the stream frames are read as.

use super::credentials::{detect_codex_credentials_in, parse_codex_secret, CodexCredentials};
use super::payload::{build_codex_payload, parse_codex_frame};
use super::*;

use harness_core::{CompletionOptions, Message, ReasoningEffort, ToolCall, ToolDefinition};
use serde_json::json;

fn options() -> CompletionOptions {
    CompletionOptions {
        model: None,
        temperature: None,
        max_tokens: None,
        top_p: None,
        stop: Vec::new(),
        reasoning_effort: None,
    }
}

/// An id_token with the claims a ChatGPT login carries, base64url-encoded as the real one is.
fn id_token(email: &str, account_id: &str) -> String {
    use base64::Engine as _;

    let claims = json!({
        "email": email,
        "https://api.openai.com/auth": { "chatgpt_account_id": account_id },
    });
    let encode = |value: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    };
    format!(
        "{}.{}.{}",
        encode(&json!({ "alg": "RS256" })),
        encode(&claims),
        "signature"
    )
}

fn write_auth_json(dir: &std::path::Path, contents: serde_json::Value) {
    let codex_dir = dir.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("auth.json"),
        serde_json::to_string(&contents).unwrap(),
    )
    .unwrap();
}

#[test]
fn a_codex_login_is_read_off_the_machine() {
    let home = tempfile::tempdir().unwrap();
    write_auth_json(
        home.path(),
        json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": id_token("someone@example.com", "acct-from-claim"),
                "access_token": "access-token-value",
                "refresh_token": "refresh-token-value",
                "account_id": "acct-from-file",
            },
        }),
    );

    let credentials = detect_codex_credentials_in(home.path(), None).expect("the login is found");
    assert_eq!(credentials.access_token, "access-token-value");
    assert_eq!(
        credentials.refresh_token.as_deref(),
        Some("refresh-token-value")
    );
    // The file's own field wins: it is what the CLI sends.
    assert_eq!(credentials.account_id.as_deref(), Some("acct-from-file"));
    assert_eq!(credentials.email.as_deref(), Some("someone@example.com"));
    assert_eq!(credentials.label(), "someone@example.com");

    // The stored form round-trips through the parser the route uses.
    let restored = parse_codex_secret(&credentials.to_secret());
    assert_eq!(restored.access_token, "access-token-value");
    assert_eq!(restored.account_id.as_deref(), Some("acct-from-file"));
}

#[test]
fn the_account_id_falls_back_to_the_id_token_claim() {
    let home = tempfile::tempdir().unwrap();
    write_auth_json(
        home.path(),
        json!({
            "tokens": {
                "id_token": id_token("someone@example.com", "acct-from-claim"),
                "access_token": "access-token-value",
            },
        }),
    );

    let credentials = detect_codex_credentials_in(home.path(), None).unwrap();
    assert_eq!(credentials.account_id.as_deref(), Some("acct-from-claim"));
    assert_eq!(credentials.refresh_token, None);
}

#[test]
fn codex_home_is_checked_before_the_default_location() {
    let home = tempfile::tempdir().unwrap();
    let codex_home = tempfile::tempdir().unwrap();
    write_auth_json(
        home.path(),
        json!({ "tokens": { "access_token": "from-home" } }),
    );
    std::fs::write(
        codex_home.path().join("auth.json"),
        json!({ "tokens": { "access_token": "from-codex-home" } }).to_string(),
    )
    .unwrap();

    let credentials =
        detect_codex_credentials_in(home.path(), Some(&codex_home.path().to_string_lossy()))
            .unwrap();
    assert_eq!(credentials.access_token, "from-codex-home");
}

#[test]
fn an_api_key_only_login_is_not_a_chatgpt_login() {
    let home = tempfile::tempdir().unwrap();
    write_auth_json(
        home.path(),
        json!({ "auth_mode": "apikey", "OPENAI_API_KEY": "sk-not-a-chatgpt-token" }),
    );

    let error = detect_codex_credentials_in(home.path(), None).expect_err("no tokens, no login");
    let message = error.to_string();
    // The message has to say where it looked and what to run: the alternative is a user staring
    // at "authentication failed" with a Codex CLI that is plainly logged in.
    assert!(message.contains("auth.json"), "{message}");
    assert!(message.contains("codex login"), "{message}");
}

#[test]
fn a_bare_token_still_parses_as_a_credential() {
    let credentials = parse_codex_secret("  plain-access-token  ");
    assert_eq!(credentials.access_token, "plain-access-token");
    assert_eq!(credentials.account_id, None);
}

#[test]
fn an_expired_token_names_the_one_keystroke_fix() {
    let error = expired_credential_error();
    let message = error.to_string();
    assert!(message.contains("401"), "{message}");
    assert!(message.contains("Import Codex"), "{message}");
}

fn echo_tool() -> ToolDefinition {
    ToolDefinition {
        name: "echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        }),
    }
}

#[test]
fn the_system_prompt_becomes_instructions_and_tools_stay_flat() {
    let messages = vec![
        Message::system("You are Luna."),
        Message::system("Folded summary."),
        Message::user("hello"),
    ];
    let mut options = options();
    options.reasoning_effort = Some(ReasoningEffort::Low);
    options.max_tokens = Some(1024);

    let body = build_codex_payload(&messages, &[echo_tool()], &options, "gpt-5.6-luna");

    // Both system messages survive, in order: the summary must not replace the persona.
    assert_eq!(body["instructions"], "You are Luna.\n\nFolded summary.");
    assert_eq!(body["model"], "gpt-5.6-luna");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(body["max_output_tokens"], 1024);
    // The Responses shape is flat — no nested `function` object.
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "echo");
    assert_eq!(body["tools"][0]["description"], "Echo text back");
    assert_eq!(body["tools"][0]["parameters"]["required"][0], "text");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][0]["content"][0]["text"], "hello");
    // The sampling parameters the reasoning models reject are never sent.
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
}

#[test]
fn a_tool_turn_replays_as_function_call_items() {
    let messages = vec![
        Message::user("call echo"),
        Message::assistant_with_tools(vec![ToolCall {
            id: "call_abc".to_string(),
            name: "echo".to_string(),
            arguments: json!({ "text": "hi" }),
            signature: None,
        }]),
        Message::tool(harness_core::ToolResult::success("call_abc", "hi")),
    ];

    let body = build_codex_payload(&messages, &[echo_tool()], &options(), "gpt-5.6-luna");

    assert_eq!(body["input"][1]["type"], "function_call");
    assert_eq!(body["input"][1]["call_id"], "call_abc");
    assert_eq!(body["input"][1]["name"], "echo");
    // Arguments are a JSON *string* on this wire, not an object.
    assert_eq!(body["input"][1]["arguments"], "{\"text\":\"hi\"}");
    assert_eq!(body["input"][2]["type"], "function_call_output");
    assert_eq!(body["input"][2]["call_id"], "call_abc");
    assert_eq!(body["input"][2]["output"], "hi");
}

#[test]
fn a_pasted_screenshot_becomes_an_image_part() {
    let messages = vec![Message::user(
        "look at this\n data:image/png;base64,AAAA \nthanks",
    )];
    let body = build_codex_payload(&messages, &[], &options(), "gpt-5.6-luna");

    let content = body["input"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "input_text");
    assert_eq!(content[0]["text"], "look at this");
    assert_eq!(content[1]["type"], "input_image");
    assert_eq!(content[1]["image_url"], "data:image/png;base64,AAAA");
    assert_eq!(content[2]["type"], "input_text");
    assert_eq!(content[2]["text"], "thanks");
}

#[test]
fn an_empty_transcript_still_sends_one_user_turn() {
    let body = build_codex_payload(&[Message::system("rules")], &[], &options(), "gpt-5.6-luna");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["instructions"], "rules");
}

#[test]
fn text_reasoning_and_calls_are_read_off_the_frames() {
    let text = parse_codex_frame(&json!({
        "type": "response.output_text.delta",
        "delta": "51",
    }));
    assert_eq!(text, vec![StreamEvent::ContentDelta("51".to_string())]);

    // A keep-alive-shaped frame appends nothing.
    assert!(parse_codex_frame(&json!({
        "type": "response.output_text.delta",
        "delta": "",
    }))
    .is_empty());

    let reasoning = parse_codex_frame(&json!({
        "type": "response.reasoning_summary_text.delta",
        "delta": "thinking",
    }));
    assert_eq!(
        reasoning,
        vec![StreamEvent::ReasoningDelta("thinking".to_string())]
    );

    // The bookkeeping frames the backend sends around a turn carry nothing.
    assert!(parse_codex_frame(&json!({ "type": "response.created" })).is_empty());
    assert!(parse_codex_frame(&json!({ "type": "response.in_progress" })).is_empty());

    let call = parse_codex_frame(&json!({
        "type": "response.output_item.done",
        "output_index": 1,
        "item": {
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_0eef",
            "name": "echo",
            "arguments": "{\"text\":\"hello\"}",
            "status": "completed",
        },
    }));
    assert_eq!(
        call,
        vec![StreamEvent::ToolCallDelta {
            index: 1,
            id: Some("call_0eef".to_string()),
            name: Some("echo".to_string()),
            arguments_delta: Some("{\"text\":\"hello\"}".to_string()),
            signature: None,
        }]
    );

    // A message item is not a call.
    assert!(parse_codex_frame(&json!({
        "type": "response.output_item.done",
        "item": { "type": "message", "role": "assistant" },
    }))
    .is_empty());
}

#[test]
fn usage_is_read_off_the_completed_frame() {
    let events = parse_codex_frame(&json!({
        "type": "response.completed",
        "response": {
            "status": "completed",
            "usage": {
                "input_tokens": 58,
                "output_tokens": 17,
                "total_tokens": 75,
                "input_tokens_details": { "cached_tokens": 32 },
            },
        },
    }));

    assert_eq!(
        events,
        vec![StreamEvent::Completed {
            finish_reason: None,
            usage: Some(harness_core::TokenUsage {
                prompt_tokens: 58,
                completion_tokens: 17,
                total_tokens: 75,
                cached_tokens: 32,
            }),
        }]
    );
}

#[test]
fn a_turn_cut_off_by_the_output_cap_reports_max_tokens() {
    let events = parse_codex_frame(&json!({
        "type": "response.incomplete",
        "response": {
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
        },
    }));

    match events.as_slice() {
        [StreamEvent::Completed { finish_reason, .. }] => {
            assert_eq!(finish_reason.as_deref(), Some("max_tokens"));
        }
        other => panic!("expected a completed event, got {other:?}"),
    }
}

#[test]
fn a_failed_frame_ends_the_turn_with_the_providers_own_sentence() {
    let events = frame_events(&json!({
        "type": "response.failed",
        "response": { "error": { "message": "usage limit reached" } },
    }));

    match events.as_slice() {
        [Err(error)] => {
            let message = error.to_string();
            assert!(message.contains("usage limit reached"), "{message}");
        }
        other => panic!("expected one error, got {other:?}"),
    }
}

#[test]
fn the_route_claims_the_codex_client_identity() {
    let credentials = CodexCredentials {
        access_token: "token".to_string(),
        account_id: Some("acct".to_string()),
        ..Default::default()
    };
    let headers = codex_headers(&credentials).unwrap();

    assert_eq!(headers.get("originator").unwrap(), CODEX_ORIGINATOR);
    assert_eq!(headers.get("chatgpt-account-id").unwrap(), "acct");
    assert_eq!(
        headers.get("OpenAI-Beta").unwrap(),
        "responses=experimental"
    );
    assert_eq!(headers.get("authorization").unwrap(), "Bearer token");
    assert!(headers
        .get("user-agent")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("codex_cli_rs/"));
    // Every request gets its own session id: the backend groups a turn's frames by it.
    assert!(!headers.get("session_id").unwrap().is_empty());

    // A credential with no account id still authenticates; the header is simply absent.
    let headers = codex_headers(&CodexCredentials {
        access_token: "token".to_string(),
        ..Default::default()
    })
    .unwrap();
    assert!(headers.get("chatgpt-account-id").is_none());
}

#[test]
fn a_role_that_is_not_a_call_is_left_out_of_the_transcript() {
    // The system turn is instructions, not an input item: sending it twice would bill the rules
    // twice on every request.
    let messages = vec![Message::system("rules"), Message::user("hi")];
    let body = build_codex_payload(&messages, &[], &options(), "gpt-5.6-luna");
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], "user");
    assert_eq!(body["instructions"], "rules");
}

#[test]
fn the_default_options_send_no_reasoning_block() {
    let body = build_codex_payload(&[Message::user("hi")], &[], &options(), "gpt-5.6-luna");
    assert!(body.get("reasoning").is_none());
    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
}

#[test]
fn the_installed_version_is_only_accepted_when_it_looks_like_one() {
    assert!(is_version_like("0.167.2"));
    assert!(!is_version_like(""));
    assert!(!is_version_like("v0.167.2"));
    assert!(!is_version_like("codex-cli 0.167.2"));
    assert_eq!(
        user_agent(),
        format!("codex_cli_rs/{}", codex_client_version())
    );
}
