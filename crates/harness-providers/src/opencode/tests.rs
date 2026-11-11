use super::cli::AttachmentFiles;
use super::form::{form_value_for_answer, parse_opencode_form_to_clarify, FormFieldKind};
use super::native::session_permissions;
use super::prompt::format_opencode_prompt;
use super::usage::{add_usage, session_total_usage, step_context_usage, usage_from_tokens};
use harness_core::types::Message;
use harness_core::ClarifyAnswer;

/// Code Mode is never offered to a Luna session: the deny rule must come after the blanket
/// allow, because OpenCode applies the last matching rule.
#[test]
fn code_mode_is_denied_after_the_blanket_allow() {
    for yolo in [false, true] {
        let rules = session_permissions(yolo)
            .as_array()
            .expect("an array of rules")
            .clone();
        let last = rules.last().expect("at least one rule");
        assert_eq!(last["action"], "execute");
        assert_eq!(last["effect"], "deny");
        assert_eq!(rules[0]["effect"], "allow");
    }
}

/// The live wire puts `input` and `cache.read` side by side; the ledger's contract is that the
/// cache count is part of the prompt. Row 456 of a real ledger had 56 input and 7,680 cached.
#[test]
fn a_cache_read_beside_the_input_is_folded_into_the_prompt() {
    let tokens = serde_json::json!({
        "input": 56,
        "output": 18,
        "reasoning": 3,
        "cache": { "read": 7_680 },
        "total": 77
    });
    let usage = usage_from_tokens(&tokens);
    assert_eq!(usage.prompt_tokens, 7_736, "input + cache.read");
    assert_eq!(usage.completion_tokens, 21, "reasoning is billed as output");
    assert_eq!(usage.cached_tokens, 7_680);
    assert_eq!(usage.total_tokens, 7_757);
}

/// The gauge reads `session.step.ended`: one step's prompt is the context. The same numbers on
/// `session.usage.updated` are the running total for every step the inner loop ran, so that
/// event must never be mistaken for a context size.
#[test]
fn only_a_step_report_is_a_context_size() {
    let step = serde_json::json!({
        "tokens": {
            "input": 929,
            "output": 2,
            "reasoning": 0,
            "cache": { "read": 6_144 }
        }
    });
    let context = step_context_usage("session.step.ended", &step).expect("a step reports usage");
    assert_eq!(context.prompt_tokens, 7_073, "input + cache.read");
    assert_eq!(context.completion_tokens, 2);
    assert!(
        step_context_usage("session.usage.updated", &step).is_none(),
        "the cumulative event is not a context size"
    );
}

/// The native route bills from `session.usage.updated`, which already covers every step of the
/// completion; the session is created per call, so its newest value is the call's total.
#[test]
fn the_cumulative_report_is_the_ledger_total() {
    let cumulative = serde_json::json!({
        "tokens": {
            "input": 1_015,
            "output": 6,
            "reasoning": 81,
            "cache": { "read": 6_528 }
        }
    });
    let usage =
        session_total_usage("session.usage.updated", &cumulative).expect("a cumulative total");
    assert_eq!(usage.prompt_tokens, 7_543);
    assert_eq!(usage.completion_tokens, 87, "output + reasoning");
    assert!(
        session_total_usage("session.step.ended", &cumulative).is_none(),
        "a single step is not a cumulative total"
    );
}

/// Headless emits one `step_finish` per step and no cumulative event, so the call total is the
/// sum of the steps while the gauge keeps whichever step arrived last.
#[test]
fn headless_steps_are_summed_for_the_ledger() {
    let first = usage_from_tokens(&serde_json::json!({
        "input": 14, "output": 41, "reasoning": 21, "cache": { "read": 6_016 }
    }));
    let second = usage_from_tokens(&serde_json::json!({
        "input": 20, "output": 5, "reasoning": 0, "cache": { "read": 7_000 }
    }));
    let mut total = None;
    add_usage(&mut total, &first);
    add_usage(&mut total, &second);
    let total = total.expect("two steps make a total");
    assert_eq!(
        total.prompt_tokens,
        first.prompt_tokens + second.prompt_tokens
    );
    assert_eq!(
        total.completion_tokens,
        first.completion_tokens + second.completion_tokens
    );
    assert_eq!(
        total.cached_tokens,
        first.cached_tokens + second.cached_tokens
    );
    assert_eq!(total.total_tokens, first.total_tokens + second.total_tokens);
    assert_eq!(
        second.prompt_tokens, 7_020,
        "each step still holds its own prompt"
    );
}

/// A pasted screenshot is an attachment, not 300 KB of prose. The text keeps the instruction
/// (and the marker the CLI wrote) and the data URL moves to `files`, where OpenCode turns it
/// into an image part. Sending it in `text` is what produced a turn whose only content the
/// model could see was "pasted content" with no instruction.
#[test]
fn a_pasted_image_becomes_an_attachment_not_prompt_text() {
    let url = "data:image/png;base64,iVBORw0KGgo=";
    let messages = [
        Message::system("be brief"),
        Message::user(format!(
            "make the output coloured [image: 800x600]\n\n{}",
            url
        )),
    ];

    let prompt = format_opencode_prompt(&messages);

    assert!(
        !prompt.text.contains("base64"),
        "the pixels must not be replayed as text: {}",
        prompt.text
    );
    assert!(
        prompt.text.contains("make the output coloured"),
        "the instruction survives: {}",
        prompt.text
    );
    assert!(prompt.text.contains("[image: 800x600]"));
    assert_eq!(prompt.files.len(), 1);
    assert_eq!(prompt.files[0]["uri"], url);
    assert_eq!(prompt.files[0]["name"], "pasted-image-1.png");
}

/// A screenshot from an earlier turn is not uploaded again: the `[image: WxH]` marker stays as
/// the record of it, and only the newest user turn's pixels are attached.
#[test]
fn an_older_pasted_image_is_not_reuploaded() {
    let old = "data:image/jpeg;base64,QUJD";
    let messages = [
        Message::user(format!("look at this [image: 10x10]\n\n{}", old)),
        Message::assistant("That is a chart."),
        Message::user("and now?"),
    ];

    let prompt = format_opencode_prompt(&messages);

    assert!(!prompt.text.contains("base64"));
    assert!(prompt.files.is_empty(), "history is not re-attached");
    assert!(prompt.text.contains("[image: 10x10]"));
}

/// The headless route takes `--file <path>`, so the pixels have to reach a real file and that
/// file must not outlive the turn.
#[test]
fn attachment_files_are_written_for_the_cli_then_removed() {
    let files = [serde_json::json!({
        "uri": "data:image/png;base64,aGk=",
        "name": "pasted-image-1.png"
    })];

    let written = AttachmentFiles::write(&files).expect("the image decodes and writes");
    let path = written
        .paths()
        .next()
        .expect("one attachment")
        .to_path_buf();
    assert_eq!(std::fs::read(&path).expect("the file exists"), b"hi");
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));

    drop(written);
    assert!(!path.exists(), "the temp file is removed with the turn");
}

/// Text that merely mentions the format is prose: it must not be lifted out, so a prompt
/// explaining `data:image/png` still reaches the model intact.
#[test]
fn a_mention_of_a_data_url_is_not_an_attachment() {
    let messages = [Message::user("the spec says data:image/png is a URL")];

    let prompt = format_opencode_prompt(&messages);

    assert!(prompt.files.is_empty());
    assert!(prompt.text.contains("data:image/png"));
}

fn question_form(field_type: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "frm_test",
        "title": "Questions",
        "fields": [{
            "key": "q0",
            "title": "Next step",
            "description": "What should happen next?",
            "type": field_type,
            "custom": true,
            "options": [
                { "value": "restart", "label": "Restart everything" },
                { "value": "report", "label": "Just report" }
            ]
        }]
    })
}

fn answer(choice_index: Option<usize>, text: &str) -> ClarifyAnswer {
    ClarifyAnswer {
        question: "Next step".to_string(),
        answer: text.to_string(),
        choice_index,
    }
}

/// OpenCode types a question field as `string`, so its answer is a scalar. The bridge used to
/// wrap every answer in an array; the API rejects that with `FormInvalidAnswerError`, which
/// left the form pending and the turn loading forever.
#[test]
fn a_string_field_reply_is_scalar_and_uses_the_option_value() {
    let (request, mappings) = parse_opencode_form_to_clarify(&question_form("string"));

    assert_eq!(request.questions.len(), 1);
    assert_eq!(mappings[0].kind, FormFieldKind::Scalar);
    let value = form_value_for_answer(&mappings[0], &answer(Some(0), "Restart everything"))
        .expect("the chosen option is mapped");
    assert_eq!(value, serde_json::json!("restart"));
}

#[test]
fn a_custom_string_answer_is_sent_as_typed() {
    let (_, mappings) = parse_opencode_form_to_clarify(&question_form("string"));

    let value = form_value_for_answer(&mappings[0], &answer(None, "something else"))
        .expect("a typed answer is mapped");
    assert_eq!(value, serde_json::json!("something else"));
}

#[test]
fn a_multiselect_reply_is_an_array_of_values() {
    let (_, mappings) = parse_opencode_form_to_clarify(&question_form("multiselect"));

    assert_eq!(mappings[0].kind, FormFieldKind::Multi);
    let value = form_value_for_answer(&mappings[0], &answer(Some(1), "Just report"))
        .expect("the chosen option is mapped");
    assert_eq!(value, serde_json::json!(["report"]));
}

#[test]
fn a_boolean_field_offers_yes_no_and_replies_with_a_bool() {
    let form = serde_json::json!({
        "id": "frm_bool",
        "title": "Questions",
        "fields": [{ "key": "enabled", "title": "Enable it?", "type": "boolean" }]
    });
    let (request, mappings) = parse_opencode_form_to_clarify(&form);

    assert_eq!(request.questions[0].choices, vec!["Yes", "No"]);
    let value = form_value_for_answer(&mappings[0], &answer(Some(1), "No"))
        .expect("the chosen option is mapped");
    assert_eq!(value, serde_json::json!(false));
}
