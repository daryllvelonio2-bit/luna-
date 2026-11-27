use super::super::*;
use super::test_dummy_tui_state;
use crate::commands::chat::render::TurnRecord;

#[test]
fn approval_modal_renders_four_choices_input_and_reply() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut st = test_dummy_tui_state();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    st.modal = ActiveModal::Approval {
        req: harness_core::ApprovalRequest {
            tool: "patch".to_string(),
            call_id: "c99".to_string(),
            target: Some("src/lib.rs".to_string()),
            diff: Some("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n".to_string()),
            arguments: serde_json::json!({}),
        },
        resp_tx: tx,
        selected: 3,
        diff_scroll: 0,
        editing_input: true,
        input: "Why replace a with b?".to_string(),
        asking: false,
        reply: Some("Because b fixes the off-by-one.".to_string()),
        reply_scroll: 0,
        error: None,
    };
    let in_flight = Vec::new();
    let info = FooterInfo {
        provider: "openai",
        model: "gpt-4o",
        reasoning_effort: None,
        session_id: "s1",
        in_flight_tools: &in_flight,
        features: Default::default(),
    };
    let snap = TuiLiveSnapshot::default();
    let backend = TestBackend::new(100, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let buf = terminal.backend().buffer();
    let text: String = (0..32)
        .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("patch"), "{text}");
    assert!(text.contains("src/lib.rs"), "{text}");
    assert!(!text.contains("--- a/src/lib.rs"), "{text}");
    assert!(!text.contains("Approval model"), "{text}");
    assert!(text.contains("-a"), "{text}");
    assert!(text.contains("+b"), "{text}");
    assert!(text.contains("Allow once"), "{text}");
    assert!(text.contains("Ask a question"), "{text}");
    assert!(text.contains("Suggest an alternative"), "{text}");
    assert!(text.contains("Because b fixes the off-by-one."), "{text}");
    assert!(text.contains("Why replace a with b?"), "{text}");

    // Deletions must read red and additions green: an all-green diff hides what was removed.
    let (mut del_red, mut add_green) = (0, 0);
    for y in 0..32 {
        let row: String = (0..100).map(|x| buf[(x, y)].symbol()).collect();
        if row.contains("-a") {
            if let Some(x) = (0..100).find(|&x| buf[(x, y)].symbol() == "-") {
                del_red += usize::from(buf[(x, y)].fg == red_err());
            }
        }
        if row.contains("+b") {
            if let Some(x) = (0..100).find(|&x| buf[(x, y)].symbol() == "+") {
                add_green += usize::from(buf[(x, y)].fg == green_ok());
            }
        }
    }
    assert!(del_red > 0, "the removed line must be red: {text}");
    assert!(add_green > 0, "the added line must be green: {text}");
}

#[test]
fn approval_modal_falls_back_to_content_preview_when_diff_is_none() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut st = test_dummy_tui_state();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    st.modal = ActiveModal::Approval {
        req: harness_core::ApprovalRequest {
            tool: "write_file".to_string(),
            call_id: "c100".to_string(),
            target: Some("test.txt".to_string()),
            diff: None,
            arguments: serde_json::json!({
                "path": "test.txt",
                "content": "line one\nline two"
            }),
        },
        resp_tx: tx,
        selected: 0,
        diff_scroll: 0,
        editing_input: false,
        input: String::new(),
        asking: false,
        reply: None,
        reply_scroll: 0,
        error: None,
    };
    let in_flight = Vec::new();
    let info = FooterInfo {
        provider: "openai",
        model: "gpt-4o",
        reasoning_effort: None,
        session_id: "s1",
        in_flight_tools: &in_flight,
        features: Default::default(),
    };
    let snap = TuiLiveSnapshot::default();
    let backend = TestBackend::new(100, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let buf = terminal.backend().buffer();
    let text: String = (0..32)
        .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("write_file"), "{text}");
    assert!(text.contains("test.txt"), "{text}");
    assert!(text.contains("+line one"), "{text}");
    assert!(text.contains("+line two"), "{text}");
    // Verify darker diff box background (`bg_code`) and semantic button colors.
    let mut saw_dark_box = false;
    let mut saw_allow_green = false;
    let mut saw_auto_yellow = false;
    let mut saw_deny_red = false;
    let mut saw_ask_white = false;
    for y in 0..32 {
        let row: String = (0..100).map(|x| buf[(x, y)].symbol()).collect();
        if row.contains("+line one") {
            if let Some(x) = (0..100).find(|&x| buf[(x, y)].symbol() == "+") {
                saw_dark_box = buf[(x, y)].bg == bg_code();
            }
        }
        if (0..100).any(|x| buf[(x, y)].symbol() == "y" && buf[(x, y)].fg == green_ok()) {
            saw_allow_green = true;
        }
        if (0..100).any(|x| buf[(x, y)].symbol() == "a" && buf[(x, y)].fg == yellow_warn()) {
            saw_auto_yellow = true;
        }
        if (0..100).any(|x| buf[(x, y)].symbol() == "n" && buf[(x, y)].fg == red_err()) {
            saw_deny_red = true;
        }
        if (0..100).any(|x| {
            buf[(x, y)].symbol() == "q"
                && (buf[(x, y)].fg == cyan_link() || buf[(x, y)].fg == text_primary())
        }) {
            saw_ask_white = true;
        }
    }
    assert!(saw_dark_box, "diff/preview box must use darker bg_code()");
    assert!(saw_allow_green, "Allow once must be green");
    assert!(saw_auto_yellow, "Auto mode must be yellow");
    assert!(saw_deny_red, "Deny must be red");
    assert!(saw_ask_white, "Ask question must be white");
}

#[test]
fn voice_detection_animates_composer_and_verify_badge() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut st = test_dummy_tui_state();
    st.voice_active = true;
    st.voice_speaking_detected = true;
    st.voice_audio_level = 0.75;
    st.spinner_tick = 3;
    let in_flight = Vec::new();
    let info = FooterInfo {
        provider: "antigravity",
        model: "gemini-3-flash",
        reasoning_effort: None,
        session_id: "s1",
        in_flight_tools: &in_flight,
        features: Default::default(),
    };
    let snap = TuiLiveSnapshot::default();
    let backend = TestBackend::new(100, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let buf = terminal.backend().buffer();
    let text: String = (0..32)
        .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Listening to your voice"), "{text}");
    assert!(text.contains("Listening"), "{text}");

    // Verify-before-send mode badge when idle
    st.voice_speaking_detected = false;
    st.voice_auto_send = false;
    terminal.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let buf2 = terminal.backend().buffer();
    let text2: String = (0..32)
        .map(|y| (0..100).map(|x| buf2[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text2.contains("Voice ON · verify"), "{text2}");
    assert!(text2.contains("verify before send"), "{text2}");
}

#[test]
fn clarify_modal_wraps_long_choices_without_truncation() {
    use harness_core::{ClarifyItem, ClarifyRequest};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut st = test_dummy_tui_state();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    st.modal = ActiveModal::Clarify {
            req: ClarifyRequest {
                questions: vec![ClarifyItem {
                    question: "What should it do?: Pick the capability the model must demonstrate in full detail.".to_string(),
                    choices: vec![
                        "Repeat / copy & pattern completion (Recommended) \u{2013} Char-level GPT that learns cyclic or mirrored sequences accurately".to_string(),
                        "Digit arithmetic mod 10 \u{2013} Transformer that outputs (a+b) mod 10 or (a*b) mod 10 cleanly".to_string(),
                    ],
                }],
            },
            resp_tx: tx,
            question_idx: 0,
            answers: Vec::new(),
            selected: 0,
            custom: "my very long custom answer that wraps cleanly across lines without horizontal clipping".to_string(),
            editing_custom: true,
            scroll: 0,
            follow_selection: true,
        };
    let in_flight = Vec::new();
    let info = FooterInfo {
        provider: "openai",
        model: "gpt-4o",
        reasoning_effort: None,
        session_id: "s1",
        in_flight_tools: &in_flight,
        features: Default::default(),
    };
    let snap = TuiLiveSnapshot::default();
    let backend = TestBackend::new(100, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let buf = terminal.backend().buffer();
    let text: String = (0..34)
        .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("mirrored sequences accurately"), "{text}");
    assert!(text.contains("(a*b) mod 10 cleanly"), "{text}");
    assert!(text.contains("without horizontal"), "{text}");
    assert!(text.contains("clipping"), "{text}");
    assert!(!text.contains("tha\u{2026}"), "{text}");
}

#[test]
fn clarify_modal_scrolls_to_follow_selection_on_short_screen() {
    use harness_core::{ClarifyItem, ClarifyRequest};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut st = test_dummy_tui_state();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let choices = (1..=8)
            .map(|i| {
                format!("Option {i} \u{2013} Detailed description number {i} explaining everything thoroughly for the user")
            })
            .collect();
    st.modal = ActiveModal::Clarify {
        req: ClarifyRequest {
            questions: vec![ClarifyItem {
                question: "Choose one option:".to_string(),
                choices,
            }],
        },
        resp_tx: tx,
        question_idx: 0,
        answers: Vec::new(),
        selected: 7,
        custom: String::new(),
        editing_custom: false,
        scroll: 0,
        follow_selection: true,
    };
    let in_flight = Vec::new();
    let info = FooterInfo {
        provider: "openai",
        model: "gpt-4o",
        reasoning_effort: None,
        session_id: "s1",
        in_flight_tools: &in_flight,
        features: Default::default(),
    };
    let snap = TuiLiveSnapshot::default();
    let backend = TestBackend::new(90, 18);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let buf = terminal.backend().buffer();
    let text: String = (0..18)
        .map(|y| (0..90).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Option 8"), "{text}");
    assert!(text.contains("thoroughly for"), "{text}");
    assert!(text.contains("the user"), "{text}");
}

#[test]
fn modals_adapt_responsively_to_small_terminal_sizes() {
    use harness_core::{ClarifyItem, ClarifyRequest};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let in_flight = Vec::new();
    let info = FooterInfo {
        provider: "openai",
        model: "gpt-4o",
        reasoning_effort: None,
        session_id: "s1",
        in_flight_tools: &in_flight,
        features: Default::default(),
    };
    let snap = TuiLiveSnapshot::default();

    // 1. Approval modal on a small 44x12 terminal must keep all 4 choices visible and centered inside bounds.
    let mut st = test_dummy_tui_state();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    st.modal = ActiveModal::Approval {
        req: harness_core::ApprovalRequest {
            tool: "patch".to_string(),
            call_id: "c1".to_string(),
            target: Some("src/main.rs".to_string()),
            diff: Some("--- a/src/main.rs\n+++ b/src/main.rs\n-old\n+new\n".to_string()),
            arguments: serde_json::json!({}),
        },
        resp_tx: tx,
        selected: 0,
        diff_scroll: 0,
        editing_input: false,
        input: String::new(),
        asking: false,
        reply: None,
        reply_scroll: 0,
        error: None,
    };
    let mut term = Terminal::new(TestBackend::new(44, 12)).unwrap();
    term.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let mr = st.modal_rect.expect("modal_rect set");
    assert!(mr.width <= 42 && mr.height <= 10, "modal_rect={mr:?}");
    let buf = term.backend().buffer();
    let text: String = (0..12)
        .map(|y| (0..44).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Allow once"), "{text}");
    assert!(text.contains("[n] Deny"), "{text}");
    assert!(text.contains("[q] Ask"), "{text}");

    // 2. Clarify modal shrinks cleanly onto a 38x11 screen without exceeding bounds.
    let (ctx, _crx) = tokio::sync::oneshot::channel();
    st.modal = ActiveModal::Clarify {
        req: ClarifyRequest {
            questions: vec![ClarifyItem {
                question: "Pick a target format for this build?".to_string(),
                choices: vec!["JSON output".to_string(), "Plain text".to_string()],
            }],
        },
        resp_tx: ctx,
        question_idx: 0,
        answers: Vec::new(),
        selected: 1,
        custom: String::new(),
        editing_custom: false,
        scroll: 0,
        follow_selection: true,
    };
    let mut term_small = Terminal::new(TestBackend::new(38, 11)).unwrap();
    term_small.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let cmr = st.modal_rect.expect("clarify modal_rect set");
    assert!(cmr.width <= 36 && cmr.height <= 9, "cmr={cmr:?}");
    let buf2 = term_small.backend().buffer();
    let text2: String = (0..11)
        .map(|y| (0..38).map(|x| buf2[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text2.contains("Plain text"), "{text2}");

    // 3. ThinkingPicker scrolls cleanly onto a short 30x8 terminal when "max" is selected.
    st.modal = ActiveModal::ThinkingPicker {
        current: None,
        selected: 6,
    };
    let mut term_tiny = Terminal::new(TestBackend::new(30, 8)).unwrap();
    term_tiny.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let tmr = st.modal_rect.expect("thinking modal_rect set");
    assert!(tmr.width <= 28 && tmr.height <= 6, "tmr={tmr:?}");
    let buf3 = term_tiny.backend().buffer();
    let text3: String = (0..8)
        .map(|y| (0..30).map(|x| buf3[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text3.contains("max"), "{text3}");
}

#[tokio::test]
async fn scrolled_up_viewport_stays_pinned_while_agent_streams_at_bottom() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut st = test_dummy_tui_state();
    st.active_turn = Some(tokio::spawn(std::future::pending()));
    st.current_query = "Live question".to_string();
    for i in 0..8 {
        st.turns.push(TurnRecord {
            user_query: format!("Question {i}"),
            reasoning: None,
            tools: Vec::new(),
            tool_items: Vec::new(),
            duration: std::time::Duration::from_secs(1),
            reply: format!("Answer paragraph {i}\nwith detail line {i}"),
            command_output: false,
        });
        st.turn_meta.push(TurnMeta {
            mode: "Build".to_string(),
            model: "gpt-4o".to_string(),
            output_tokens: None,
        });
    }
    let in_flight = Vec::new();
    let info = FooterInfo {
        provider: "openai",
        model: "gpt-4o",
        reasoning_effort: None,
        session_id: "s1",
        in_flight_tools: &in_flight,
        features: Default::default(),
    };
    let mut snap = TuiLiveSnapshot {
        content: "Initial streamed line".to_string(),
        ..Default::default()
    };
    let mut term = Terminal::new(TestBackend::new(90, 24)).unwrap();

    // First draw at bottom (`scroll_from_bottom == 0`): follows latest output.
    term.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let bottom_start = st.transcript_start;
    assert!(bottom_start > 0);
    assert!(st
        .transcript_visible_text
        .iter()
        .any(|l| l.contains("Initial streamed line")));

    // Scroll all the way to the top (`start == 0`).
    st.scroll_transcript_up(10_000);
    term.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    assert_eq!(st.transcript_start, 0);
    assert!(st.scroll_from_bottom > 0);
    let pinned_lines = st.transcript_visible_text.clone();

    // Agent streams many more lines AND shows a subagent strip that shrinks transcript height.
    snap.content = (0..40)
        .map(|n| format!("New streamed output line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    snap.delegate_count = 1;
    snap.delegate_last = Some(("worker".to_string(), "running".to_string()));
    term.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    assert_eq!(
        st.transcript_start, 0,
        "viewport must remain pinned at top while agent streams"
    );
    assert_eq!(
        st.transcript_visible_text[..5],
        pinned_lines[..5],
        "top visible lines must not shift while agent streams"
    );

    // Pin in the middle too, then stream more lines -> `transcript_start` must stay identical.
    st.scroll_transcript_down(10);
    term.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    let mid_start = st.transcript_start;
    let mid_lines = st.transcript_visible_text.clone();
    assert!(mid_start > 0);

    snap.content
        .push_str("\nEven more lines\nLine A\nLine B\nLine C");
    term.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    assert_eq!(st.transcript_start, mid_start);
    assert_eq!(st.transcript_visible_text[..5], mid_lines[..5]);

    // Jumping to latest re-enables bottom-follow mode.
    st.jump_to_latest();
    term.draw(|f| draw(f, &mut st, &snap, &info)).unwrap();
    assert_eq!(st.scroll_from_bottom, 0);
    assert!(st
        .transcript_visible_text
        .iter()
        .any(|l| l.contains("Line C")));
}
