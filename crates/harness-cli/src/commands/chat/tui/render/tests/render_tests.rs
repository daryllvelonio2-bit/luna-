use super::test_dummy_tui_state;
use ratatui::style::Color;

use super::*;
use std::collections::HashSet;
use std::time::Duration;

use crate::commands::chat::render::{TurnProcessItem, TurnRecord};

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>()
}

#[test]
fn highlight_line_selection_keeps_text_and_splits_spans() {
    let line = Line::from(vec![
        Span::styled("hello ", Style::default().fg(text_primary())),
        Span::styled("world", Style::default().fg(accent_coral())),
    ]);
    let highlighted = highlight_line_selection(line, 4, 8);
    assert_eq!(line_text(&highlighted), "hello world");

    // Columns 4..8 (`o wo`) are selected; neighbouring cells keep their own styling.
    let selected: String = highlighted
        .spans
        .iter()
        .filter(|s| s.style.bg == Some(bg_selection()))
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(selected, "o wo");
    assert!(highlighted
        .spans
        .iter()
        .any(|s| s.style.fg == Some(accent_coral()) && s.style.bg.is_none()));
}

#[test]
fn highlight_line_selection_includes_wide_glyphs_whole() {
    let line = Line::from("a中b");
    let highlighted = highlight_line_selection(line, 1, 2);
    let selected: String = highlighted
        .spans
        .iter()
        .filter(|s| s.style.bg == Some(bg_selection()))
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(selected, "中");
}

#[test]
fn active_turn_renders_new_thinking_after_completed_tools() {
    let in_flight = vec![
        TurnProcessItem::thinking_with_duration("round 1 reasoning", Some(1.4)),
        TurnProcessItem {
            tool_name: "list_files".to_string(),
            call_formatted: "list_files(path: \"\")".to_string(),
            result_formatted: "✔ listed 5 entries".to_string(),
            diff_lines: Vec::new(),
            output: String::new(),
            output_dropped: 0,
            arguments: Some(serde_json::json!({ "path": "." })),
            duration_secs: Some(0.2),
            is_error: false,
        },
        TurnProcessItem::thinking_with_duration("round 2 reasoning", Some(2.0)),
        TurnProcessItem {
            tool_name: "terminal".to_string(),
            call_formatted: "run ls -la".to_string(),
            result_formatted: "✔ exit 0".to_string(),
            diff_lines: Vec::new(),
            output: "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8".to_string(),
            output_dropped: 185,
            arguments: Some(serde_json::json!({ "command": "ls -la /home/janelle" })),
            duration_secs: Some(0.4),
            is_error: false,
        },
    ];
    let snap = TuiLiveSnapshot {
        reasoning: "round 3 active reasoning".to_string(),
        reasoning_secs: Some(0.8),
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut hits = Vec::new();
    let mut collapsed_steps = HashSet::new();
    collapsed_steps.insert(0);
    let mut expanded_tools = HashSet::new();
    expanded_tools.insert(3);

    let mut content_cache = crate::commands::chat::tui::state::LiveContentCache::default();
    let mut reasoning_cache = crate::commands::chat::tui::state::LiveReasoningCache::default();

    render_active_turn_process(
        &mut out,
        &mut hits,
        &mut content_cache,
        &mut reasoning_cache,
        &snap,
        &in_flight,
        false,
        false,
        None,
        &collapsed_steps,
        &expanded_tools,
        LiveThinkingView::Tail,
        0,
        4.2,
        84,
        80,
        false,
    );

    let rendered: Vec<String> = out.iter().map(line_text).collect();
    let joined = rendered.join("\n");

    let pos_t1 = joined.find("▸ Thought: 1.4s (1 line)").unwrap();
    let pos_tool1 = joined.find("→ List .").unwrap();
    let pos_t2 = joined.find("Thought: 2.0s").unwrap();
    let pos_t2_body = joined.find("round 2 reasoning").unwrap();
    let pos_tool2_cmd = joined.find("$ ls -la /home/janelle").unwrap();
    let pos_tool2_hidden = joined.find("(185 more lines)").unwrap();
    let pos_t3_hdr = joined.find("Thought: 0.8s").unwrap();
    let pos_t3_body = joined.find("round 3 active reasoning").unwrap();

    assert!(
        pos_t1 < pos_tool1
            && pos_tool1 < pos_t2
            && pos_t2 < pos_t2_body
            && pos_t2_body < pos_tool2_cmd
            && pos_tool2_cmd < pos_tool2_hidden
            && pos_tool2_hidden < pos_t3_hdr
            && pos_t3_hdr < pos_t3_body,
        "chronological order violated:\n{joined}"
    );
    // Verify shell card uses bg_surface()
    let card_line = out
        .iter()
        .find(|l| line_text(l).contains("$ ls -la /home/janelle"))
        .unwrap();
    assert!(card_line
        .spans
        .iter()
        .all(|s| s.style.bg == Some(bg_surface())));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveThinkingStep(0))));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveToolStep(1))));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveThinkingStep(2))));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveToolStep(3))));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveThinking)));
}

#[test]
fn active_tool_collapses_previous_thinking_only_when_auto_hide_is_on() {
    let in_flight = vec![
        TurnProcessItem::thinking_with_duration("plan the work", Some(1.2)),
        TurnProcessItem {
            tool_name: "grep".to_string(),
            call_formatted: "grep(pattern: \"todo\")".to_string(),
            result_formatted: String::new(),
            diff_lines: Vec::new(),
            output: String::new(),
            output_dropped: 0,
            arguments: Some(serde_json::json!({ "pattern": "todo" })),
            duration_secs: None,
            is_error: false,
        },
    ];
    let snap = TuiLiveSnapshot::default();

    let render = |auto_hide: bool| {
        let mut out = Vec::new();
        let mut hits = Vec::new();
        let mut content_cache = crate::commands::chat::tui::state::LiveContentCache::default();
        let mut reasoning_cache = crate::commands::chat::tui::state::LiveReasoningCache::default();
        render_active_turn_process(
            &mut out,
            &mut hits,
            &mut content_cache,
            &mut reasoning_cache,
            &snap,
            &in_flight,
            false,
            auto_hide,
            None,
            &HashSet::new(),
            &HashSet::new(),
            LiveThinkingView::Tail,
            0,
            3.0,
            84,
            80,
            false,
        );
        out.iter().map(line_text).collect::<Vec<_>>().join("\n")
    };

    let hidden = render(true);
    assert!(hidden.contains("▸ Thought: 1.2s (1 line)"), "{hidden}");
    assert!(!hidden.contains("plan the work"), "{hidden}");

    let visible = render(false);
    assert!(visible.contains("plan the work"), "{visible}");
}

#[test]
fn a_preview_is_scaled_down_to_the_card_instead_of_clipped() {
    let preview = ImagePreview {
        width: 72,
        height: 20,
        cells: vec![(bg_surface(), bg_surface()); 72 * 20],
    };

    let (width, height) = preview_display_size(&preview, 72);
    assert_eq!((width, height), (72, 20));

    let (width, height) = preview_display_size(&preview, 36);
    assert_eq!(width, 36);
    assert_eq!(height, 10, "aspect ratio kept when the card is narrow");
}

#[test]
fn the_preview_close_button_only_arms_while_hovered() {
    assert!(!pointer_inside(None, 10, 5, 20, 6));

    // Inside the thumbnail.
    assert!(pointer_inside(Some((10, 5)), 10, 5, 20, 6));
    assert!(pointer_inside(Some((29, 10)), 10, 5, 20, 6));

    // Just outside: left, right, above, below.
    assert!(!pointer_inside(Some((9, 5)), 10, 5, 20, 6));
    assert!(!pointer_inside(Some((30, 5)), 10, 5, 20, 6));
    assert!(!pointer_inside(Some((10, 4)), 10, 5, 20, 6));
    assert!(!pointer_inside(Some((10, 11)), 10, 5, 20, 6));
}

#[test]
fn the_close_button_is_drawn_muted_and_only_on_hover() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let preview = ImagePreview {
        width: 8,
        height: 4,
        cells: vec![(bg_surface(), bg_surface()); 8 * 4],
    };
    let placement = PreviewPlacement {
        index: 0,
        x: 0,
        y: 0,
        width: 8,
        height: 4,
    };
    let rect = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };
    let draw = |pointer| {
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_preview_cell(
                    frame,
                    rect,
                    &placement,
                    "[Image 1]",
                    &preview,
                    pointer,
                    &mut hits,
                );
                assert!(area.width > 0);
            })
            .unwrap();
        (terminal.backend().buffer().clone(), hits)
    };

    let (resting, hits) = draw(None);
    assert!(hits.is_empty(), "no close button while the pointer is away");
    let resting_text: String = (0..resting.area.height)
        .map(|y| {
            (0..resting.area.width)
                .map(|x| resting[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    assert!(!resting_text.contains('✕'), "{resting_text}");

    let (hovered, hits) = draw(Some((4, 1)));
    assert_eq!(hits.len(), 1, "hovering arms exactly one close button");
    let close_x = 3 + placement.width - 1;
    let cell = &hovered[(close_x, 1)];
    assert_eq!(cell.symbol(), "✕");
    assert_eq!(
        cell.fg,
        text_muted(),
        "the glyph is muted, not a red warning"
    );
    assert_eq!(cell.bg, bg_surface(), "the glyph sits on the card surface");
    assert_eq!(hits[0].x_start, close_x);
    assert_eq!(hits[0].y, 1);
}

#[test]
fn collapsed_thinking_header_keeps_the_coral_accent() {
    let mut out = Vec::new();
    let mut hits = Vec::new();
    push_settled_thinking_block(
        &mut out,
        &mut hits,
        "one line of reasoning",
        Some(1.2),
        false,
        None,
        TranscriptClickTarget::JumpToLatest,
        80,
    );

    let header = out
        .iter()
        .find(|line| line_text(line).contains("▸ Thought: 1.2s"))
        .expect("collapsed thinking header");
    let label = header
        .spans
        .iter()
        .find(|span| span.content.contains("Thought"))
        .expect("label span");
    assert_eq!(label.style.fg, Some(accent_coral()));
}

#[test]
fn compaction_turn_renders_yellow_card_and_compacted_footer() {
    let turn = TurnRecord {
        user_query: "Compaction".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: Vec::new(),
        duration: std::time::Duration::from_millis(1500),
        reply: "## Objective\nSummarized context.".to_string(),
    };
    let mut out = Vec::new();
    let mut hits = Vec::new();
    let empty_set: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let empty_turns: std::collections::HashSet<usize> = std::collections::HashSet::new();
    render_single_settled_turn(
        &mut out,
        &mut hits,
        0,
        &turn,
        None,
        false,
        true,
        None,
        &empty_set,
        &empty_set,
        &empty_turns,
        &empty_turns,
        "Build",
        "gpt-4o",
        80,
        76,
    );
    let text: Vec<String> = out.iter().map(line_text).collect();
    let all_text = text.join("\n");
    assert!(
        all_text.contains("⚡ Context Compacted"),
        "expected banner in {all_text}"
    );
    assert!(
        all_text.contains("Compacted ·"),
        "expected footer in {all_text}"
    );
    assert!(
        all_text.contains("Summarized context"),
        "expected markdown summary in {all_text}"
    );
}

#[test]
fn completed_turn_renders_interleaved_thinking_prose_and_tool_cards() {
    let turn = TurnRecord {
        user_query: "what is this repo about".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: vec![
            TurnProcessItem::thinking_with_duration("first thought", Some(1.1)),
            TurnProcessItem {
                tool_name: "list_files".to_string(),
                call_formatted: "list_files(path: \"\")".to_string(),
                result_formatted: "✔ ok".to_string(),
                diff_lines: Vec::new(),
                output: String::new(),
                output_dropped: 0,
                arguments: Some(serde_json::json!({ "path": "." })),
                duration_secs: None,
                is_error: false,
            },
            TurnProcessItem::thinking_with_duration("second thought", Some(2.0)),
            TurnProcessItem::assistant_text("Found it. Let me inspect."),
            TurnProcessItem {
                tool_name: "read_file".to_string(),
                call_formatted: "read_file(path: \"README.md\")".to_string(),
                result_formatted: "✔ ok".to_string(),
                diff_lines: Vec::new(),
                output: String::new(),
                output_dropped: 0,
                arguments: Some(serde_json::json!({ "path": "README.md" })),
                duration_secs: None,
                is_error: false,
            },
            TurnProcessItem::thinking_with_duration("final thought", Some(0.5)),
        ],
        duration: Duration::from_secs(3),
        reply: "Done".to_string(),
    };

    let mut out = Vec::new();
    let mut hits = Vec::new();
    let mut expanded_thinking = HashSet::new();
    expanded_thinking.insert((0, 1));
    expanded_thinking.insert((0, 2));
    let expanded_tools = HashSet::new();
    let empty_set = HashSet::new();

    render_completed_turn_process(
        &mut out,
        &mut hits,
        0,
        &turn,
        true,
        true,
        None,
        &expanded_thinking,
        &expanded_tools,
        &empty_set,
        &empty_set,
        84,
        80,
    );

    let joined = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    let p_think0 = joined.find("▸ Thought: 1.1s (1 line)").unwrap();
    let p_tools0 = joined.find("→ List .").unwrap();
    let p_think1 = joined.find("second thought").unwrap();
    let p_prose = joined.find("Found it. Let me inspect.").unwrap();
    let p_tools1 = joined.find("→ Read README.md").unwrap();
    let p_think2 = joined[p_tools1..]
        .find("final thought")
        .map(|rel| p_tools1 + rel)
        .unwrap();

    assert!(
        p_think0 < p_tools0
            && p_tools0 < p_think1
            && p_think1 < p_prose
            && p_prose < p_tools1
            && p_tools1 < p_think2,
        "completed turn chronological order violated:\n{joined}"
    );
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnThinking(0, 0))));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnTools(0, 0))));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnThinking(0, 1))));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnTools(0, 1))));
    assert!(hits
        .iter()
        .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnThinking(0, 2))));
}

#[test]
fn completed_turn_thinking_collapses_when_process_collapsed() {
    let turn = TurnRecord {
        user_query: "hi luna".to_string(),
        reasoning: Some("hidden thought body".to_string()),
        tools: Vec::new(),
        tool_items: Vec::new(),
        duration: Duration::from_secs(2),
        reply: "Hello.".to_string(),
    };
    let mut out = Vec::new();
    let mut hits = Vec::new();
    let empty: HashSet<(usize, usize)> = HashSet::new();
    let empty_turns: HashSet<usize> = HashSet::new();

    // When process_expanded = false (auto-collapse on), the whole process collapses to 1 line
    render_completed_turn_process(
        &mut out,
        &mut hits,
        0,
        &turn,
        false,
        true,
        None,
        &empty,
        &empty,
        &empty_turns,
        &empty_turns,
        84,
        80,
    );
    let collapsed = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(collapsed.contains("▶ Process"), "{collapsed}");
    assert!(collapsed.contains("thought for 2.0s"), "{collapsed}");
    assert!(!collapsed.contains("hidden thought body"), "{collapsed}");

    // When process_expanded = true and auto_hide_thinking = true, thinking block is collapsed to ▸ Thought
    out.clear();
    hits.clear();
    render_completed_turn_process(
        &mut out,
        &mut hits,
        0,
        &turn,
        true,
        true,
        None,
        &empty,
        &empty,
        &empty_turns,
        &empty_turns,
        84,
        80,
    );
    let expanded = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(expanded.contains("▼ Process"), "{expanded}");
    assert!(expanded.contains("▸ Thought (1 line)"), "{expanded}");
    assert!(!expanded.contains("hidden thought body"), "{expanded}");

    // When process_expanded = true and auto_hide_thinking = false, thinking body is visible
    out.clear();
    hits.clear();
    render_completed_turn_process(
        &mut out,
        &mut hits,
        0,
        &turn,
        true,
        false,
        None,
        &empty,
        &empty,
        &empty_turns,
        &empty_turns,
        84,
        80,
    );
    let fully_open = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(fully_open.contains("hidden thought body"), "{fully_open}");
}

#[test]
fn block_limit_caps_thinking_blocks_while_tool_cards_stay_capped_at_ten() {
    let long_thought = (1..=15)
        .map(|i| format!("thought line {i}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let long_read = (1..=20)
        .map(|i| format!("file line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let turn = TurnRecord {
        user_query: "inspect".to_string(),
        reasoning: Some(long_thought.clone()),
        tools: Vec::new(),
        tool_items: vec![TurnProcessItem {
            tool_name: "read_file".to_string(),
            call_formatted: "read_file(path: \"src/lib.rs\")".to_string(),
            result_formatted: "✔ ok".to_string(),
            diff_lines: Vec::new(),
            output: long_read,
            output_dropped: 0,
            arguments: Some(serde_json::json!({ "path": "src/lib.rs" })),
            duration_secs: None,
            is_error: false,
        }],
        duration: Duration::from_secs(2),
        reply: "Done.".to_string(),
    };

    let empty: HashSet<(usize, usize)> = HashSet::new();
    let empty_turns: HashSet<usize> = HashSet::new();
    let mut out = Vec::new();
    let mut hits = Vec::new();
    // With block_limit = Some(3), process_expanded = true (auto-collapse off), auto_hide_thinking = false:
    // thinking is capped at 3 lines, while the tool card stays capped at 10 lines.
    render_completed_turn_process(
        &mut out,
        &mut hits,
        0,
        &turn,
        true,
        false,
        Some(3),
        &empty,
        &empty,
        &empty_turns,
        &empty_turns,
        84,
        80,
    );
    let capped = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(capped.contains("thought line 1"), "{capped}");
    assert!(!capped.contains("thought line 15"), "{capped}");
    assert!(capped.contains("more lines)"), "{capped}");

    // When process_expanded = false (auto-collapse on), the whole process collapses to 1 line!
    out.clear();
    hits.clear();
    render_completed_turn_process(
        &mut out,
        &mut hits,
        0,
        &turn,
        false,
        true,
        None,
        &empty,
        &empty,
        &empty_turns,
        &empty_turns,
        84,
        80,
    );
    let tool_collapsed = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(tool_collapsed.contains("▶ Process"));
    assert!(tool_collapsed.contains("1 tool: read_file"));
    assert!(tool_collapsed.contains("thought for 2.0s"));
    assert!(!tool_collapsed.contains("file line 1"));
    assert!(!tool_collapsed.contains("file line 10"));

    // When process_expanded = true, tool output is capped at 10 lines.
    out.clear();
    hits.clear();
    render_completed_turn_process(
        &mut out,
        &mut hits,
        0,
        &turn,
        true,
        true,
        None,
        &empty,
        &empty,
        &empty_turns,
        &empty_turns,
        84,
        80,
    );
    let tool_capped = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(tool_capped.contains("▼ Process"));
    assert!(tool_capped.contains("file line 10"), "{tool_capped}");
    assert!(!tool_capped.contains("file line 11"), "{tool_capped}");
    assert!(tool_capped.contains("(10 more lines)"), "{tool_capped}");

    // Live streaming thinking: uncapped (`None`) shows all 15 lines, `Some(4)` shows newest 4.
    let snap = TuiLiveSnapshot {
        reasoning: long_thought,
        reasoning_secs: Some(1.5),
        ..Default::default()
    };
    let mut c_cache = crate::commands::chat::tui::state::LiveContentCache::default();
    let mut r_cache = crate::commands::chat::tui::state::LiveReasoningCache::default();
    let empty_live: HashSet<usize> = HashSet::new();
    out.clear();
    render_active_turn_process(
        &mut out,
        &mut hits,
        &mut c_cache,
        &mut r_cache,
        &snap,
        &[],
        false,
        false,
        None,
        &empty_live,
        &empty_live,
        LiveThinkingView::Tail,
        0,
        1.5,
        84,
        80,
        false,
    );
    let live_uncapped = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(live_uncapped.contains("thought line 1"), "{live_uncapped}");
    assert!(live_uncapped.contains("thought line 15"), "{live_uncapped}");

    out.clear();
    render_active_turn_process(
        &mut out,
        &mut hits,
        &mut c_cache,
        &mut r_cache,
        &snap,
        &[],
        false,
        false,
        Some(4),
        &empty_live,
        &empty_live,
        LiveThinkingView::Tail,
        0,
        1.5,
        84,
        80,
        false,
    );
    let live_capped = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(!live_capped.contains("thought line 1\n"), "{live_capped}");
    assert!(live_capped.contains("thought line 15"), "{live_capped}");
}

#[test]
fn composer_line_highlights_image_and_pasted_tokens() {
    let spans = styled_composer_line_spans("fix [Image 1] and [Pasted ~12 lines] now");
    assert_eq!(spans.len(), 5);
    assert_eq!(spans[0].content, "fix ");
    assert_eq!(spans[1].content, "[Image 1]");
    assert_eq!(spans[1].style.fg, Some(accent_coral()));
    assert_eq!(spans[2].content, " and ");
    assert_eq!(spans[3].content, "[Pasted ~12 lines]");
    assert_eq!(spans[3].style.fg, Some(yellow_warn()));
    assert_eq!(spans[4].content, " now");
}

#[test]
fn ensure_settled_transcript_caches_and_appends_incrementally() {
    let (picker_tx, picker_rx) = tokio::sync::mpsc::unbounded_channel();
    let (approval_reply_tx, approval_reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (voice_tx, voice_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut st = TuiState {
        turns: vec![TurnRecord {
            user_query: "turn one".to_string(),
            reasoning: None,
            tools: Vec::new(),
            tool_items: Vec::new(),
            duration: Duration::from_secs(1),
            reply: "reply one".to_string(),
        }],
        turn_meta: vec![TurnMeta {
            mode: "Build".to_string(),
            model: "gpt-4".to_string(),
            output_tokens: Some(10),
        }],
        system_notices: Vec::new(),
        process_expanded: false,
        auto_hide_thinking: true,
        block_limit: None,
        expanded_thinking: HashSet::new(),
        expanded_tools: HashSet::new(),
        expanded_turns: HashSet::new(),
        collapsed_turns: HashSet::new(),
        expanded_live_thinking: HashSet::new(),
        expanded_live_tools: HashSet::new(),
        live_thinking_view: LiveThinkingView::Tail,
        reasoning_effort: None,
        pending_model_choice: None,
        mode: crate::commands::chat::tui::state::AgentMode::Build,
        composer: crate::commands::chat::tui::state::ComposerState::default(),
        tabs: Vec::new(),
        total_sessions_count: 1,
        hit_boxes: Vec::new(),
        transcript_hit_boxes: Vec::new(),
        composer_hit_boxes: Vec::new(),
        pointer: None,
        last_click: None,
        click_count: 0,
        modal_rect: None,
        composer_area: (0, 0, 0, 0),
        composer_input_area: (0, 0, 0, 0),
        composer_scroll_off: 0,
        transcript_area: (0, 0, 0, 0),
        transcript_start: 0,
        transcript_visible_text: Vec::new(),
        selection: None,
        pending_transcript_click: None,
        selection_notice: None,
        scroll_from_bottom: 0,
        scroll_anchor_top: None,
        max_scroll: 0,
        modal: ActiveModal::None,
        slash_popup: crate::commands::chat::tui::state::SlashPopupState::default(),
        queue: std::collections::VecDeque::new(),
        current_cancel: tokio_util::sync::CancellationToken::new(),
        active_turn: None,
        active_goal: None,
        current_query: String::new(),
        turn_start: std::time::Instant::now(),
        quota_refreshed_at: std::time::Instant::now(),
        idle_pass: crate::commands::chat::helpers::IdleMemoryPass::new(1),
        pending_job_turn: None,
        pending_images: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        history_path: None,
        ctrl_c_armed: None,
        esc_armed: None,
        spinner_tick: 0,
        cwd_label: "/workspace".to_string(),
        session_pinned_skills: Vec::new(),
        version: "0.1.8".to_string(),
        update_notice: None,
        last_rendered_rect: (100, 30),
        provider_items: Vec::new(),
        picker_loading: None,
        picker_tx,
        picker_rx,
        approval_reply_tx,
        approval_reply_rx,
        voice_active: false,
        voice_cancel: None,
        voice_speech_queue: crate::commands::voice::VoiceSpeechQueue::new(),
        voice_is_playing: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        voice_speaking_detected: false,
        voice_transcribing: false,
        voice_audio_level: 0.0,
        voice_auto_send: true,
        voice_pending_autosend_at: None,
        voice_stream_offset: 0,
        voice_stream_in_code: false,
        voice_spoken_any: false,
        voice_ack_sent: false,
        voice_reply_cancel: None,
        voice_tx,
        voice_rx,
        settled_cache: crate::commands::chat::tui::state::SettledTranscriptCache::default(),
        live_content_cache: crate::commands::chat::tui::state::LiveContentCache::default(),
        live_reasoning_cache: crate::commands::chat::tui::state::LiveReasoningCache::default(),
        template_agent: {
            let tmp = tempfile::tempdir().unwrap();
            let memory = std::sync::Arc::new(harness_memory::MemoryEngineKind::Sqlite(
                harness_memory::SqliteMemoryEngine::in_memory().unwrap(),
            ));
            let notes = std::sync::Arc::new(
                harness_notes::FileNotesEngine::new(tmp.path().join("notes"), None).unwrap(),
            );
            let registry = std::sync::Arc::new(harness_core::DefaultToolRegistry::new());
            let creds = harness_providers::CredentialManager::new(None).unwrap();
            let discovery = std::sync::Arc::new(harness_providers::ModelDiscovery::new(
                None,
                0,
                Default::default(),
            ));
            let provider = std::sync::Arc::new(
                harness_providers::GenericProviderClient::new(
                    "openai".to_string(),
                    "gpt-4o".to_string(),
                    creds,
                    discovery,
                    None,
                )
                .unwrap(),
            );
            std::sync::Arc::new(harness_core::Agent::new(
                provider,
                memory,
                notes,
                registry,
                harness_core::AgentConfig::default(),
            ))
        },
        background_sessions: std::collections::HashMap::new(),
        pending_modals: std::collections::VecDeque::new(),
        orchestrator_draft: None,
        active_orchestrator: None,
        memory_project_filter: None,
        compacted_added: 0,
        compacted_deleted: 0,
        session_max_added: 0,
        session_max_deleted: 0,
        diff_anim_from_added: 0,
        diff_anim_from_deleted: 0,
        diff_anim_target_added: 0,
        diff_anim_target_deleted: 0,
        diff_anim_start: None,
    };

    let in_flight = Vec::new();
    let info = FooterInfo {
        provider: "openai",
        model: "gpt-4",
        reasoning_effort: None,
        session_id: "s1",
        in_flight_tools: &in_flight,
        features: Default::default(),
    };

    ensure_settled_transcript(&mut st, &info, 84, 80);
    assert_eq!(st.settled_cache.turns_len, 1);
    let initial_lines_len = st.settled_cache.lines.len();
    assert!(initial_lines_len > 0);

    // A second call with identical state does not rebuild lines
    ensure_settled_transcript(&mut st, &info, 84, 80);
    assert_eq!(st.settled_cache.lines.len(), initial_lines_len);

    // Appending turn two incrementally adds only the second turn's lines
    st.turns.push(TurnRecord {
        user_query: "turn two".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: Vec::new(),
        duration: Duration::from_secs(2),
        reply: "reply two".to_string(),
    });
    st.turn_meta.push(TurnMeta {
        mode: "Build".to_string(),
        model: "gpt-4".to_string(),
        output_tokens: Some(20),
    });

    ensure_settled_transcript(&mut st, &info, 84, 80);
    assert_eq!(st.settled_cache.turns_len, 2);
    assert!(st.settled_cache.lines.len() > initial_lines_len);
}

fn pty_pane(title: &str, lines: &[&str]) -> TuiPtyPane {
    TuiPtyPane {
        title: title.to_string(),
        lines: lines.iter().map(|line| line.to_string()).collect(),
        running: true,
        idle_secs: 0,
    }
}

#[test]
fn pty_pane_hides_on_a_small_transcript_but_survives_half_screen() {
    let narrow = Rect {
        x: 0,
        y: 1,
        width: PTY_PANE_MIN_TRANSCRIPT_WIDTH - 1,
        height: 30,
    };
    assert!(pty_pane_rect(narrow).is_none());

    let short = Rect {
        x: 0,
        y: 1,
        width: 120,
        height: PTY_PANE_MIN_TRANSCRIPT_HEIGHT - 1,
    };
    assert!(pty_pane_rect(short).is_none());

    // Half of a roomy 176-column window is still wide enough for the overlay.
    let half = Rect {
        x: 0,
        y: 1,
        width: 84,
        height: 24,
    };
    let pane = pty_pane_rect(half).expect("half screen is still enough");
    assert_eq!(pane.width, PTY_PANE_MIN_WIDTH);
    assert_eq!(pane.height, PTY_PANE_ROWS);
    assert_eq!(pane.x + pane.width, half.width);
}

#[test]
fn pty_pane_draws_its_title_and_screen() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let backend = TestBackend::new(60, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    let pane = pty_pane(
        "start · chat",
        &[
            "Compiling harness-cli v0.1.8",
            "Finished `dev` profile [unoptimized]",
        ],
    );

    terminal
        .draw(|frame| {
            let area = frame.area();
            render_pty_pane(frame, area, &pane);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }

    assert!(
        text.contains("pty · start · chat"),
        "the pane names the session:\n{text}"
    );
    assert!(
        text.contains("Compiling harness-cli"),
        "the pane shows the reported screen:\n{text}"
    );
    assert!(
        text.contains("Finished `dev` profile"),
        "the newest lines are drawn:\n{text}"
    );
    assert!(
        text.contains("running"),
        "the pane says the session is still live:\n{text}"
    );
}

#[test]
fn strip_terminal_indent_removes_common_padding() {
    let lines = vec![
        "                       ┃".to_string(),
        "                       ┃  Ask anything…".to_string(),
        "                       ┃  Build auto".to_string(),
    ];
    let stripped = strip_terminal_indent(&lines);
    assert_eq!(stripped[0], "┃");
    assert_eq!(stripped[1], "┃  Ask anything…");
    assert_eq!(stripped[2], "┃  Build auto");
}

#[test]
fn pty_pane_caps_width_on_very_wide_windows() {
    let wide = Rect {
        x: 0,
        y: 1,
        width: 320,
        height: 40,
    };
    let pane = pty_pane_rect(wide).expect("pane");
    assert_eq!(
        pane.width, PTY_PANE_MAX_WIDTH,
        "a wide window caps the terminal width cleanly"
    );
    assert_eq!(pane.height, PTY_PANE_ROWS);
    assert_eq!(pane.x + pane.width, wide.width);
}

#[test]
fn selection_highlight_keeps_text_and_splits_spans() {
    let line = Line::from(vec![
        Span::styled("abc", Style::default().fg(Color::Red)),
        Span::styled("def", Style::default().fg(Color::Green)),
    ]);
    let highlighted = highlight_line_selection(line, 1, 5);

    assert_eq!(line_text(&highlighted), "abcdef");
    let selected: String = highlighted
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(bg_selection()))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(selected, "bcde");
    // Original foreground colours survive the split.
    assert!(highlighted
        .spans
        .iter()
        .any(|span| span.content.contains('a') && span.style.fg == Some(Color::Red)));
}

#[test]
fn selection_highlight_selects_wide_glyphs_whole() {
    let line = Line::from("a中b");
    let highlighted = highlight_line_selection(line, 1, 2);
    let selected: String = highlighted
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(bg_selection()))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(selected, "中");
}

#[test]
fn tool_cards_in_box_truncate_to_ten_lines_and_expand_on_click() {
    let lines_20 = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let item = TurnProcessItem {
        tool_name: "terminal".to_string(),
        call_formatted: "cargo check".to_string(),
        result_formatted: "✔ ok".to_string(),
        diff_lines: Vec::new(),
        output: lines_20,
        output_dropped: 0,
        arguments: Some(serde_json::json!({ "command": "cargo check" })),
        duration_secs: Some(1.0),
        is_error: false,
    };

    // When closed (open = false)
    let mut out_closed = Vec::new();
    let mut hits_closed = Vec::new();
    push_completed_tool_card(
        &mut out_closed,
        &mut hits_closed,
        &item,
        false,
        TranscriptClickTarget::ToggleTurnTools(0, 0),
        80,
        76,
    );
    let joined_closed = out_closed
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined_closed.contains("line 1"), "starts from head");
    assert!(joined_closed.contains("line 10"), "shows 10th line");
    assert!(!joined_closed.contains("line 11"), "line 11 is truncated");
    assert!(
        joined_closed.contains("(10 more lines)"),
        "shows bottom more lines indicator"
    );

    // When opened (open = true)
    let mut out_opened = Vec::new();
    let mut hits_opened = Vec::new();
    push_completed_tool_card(
        &mut out_opened,
        &mut hits_opened,
        &item,
        true,
        TranscriptClickTarget::ToggleTurnTools(0, 0),
        80,
        76,
    );
    let joined_opened = out_opened
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined_opened.contains("line 1"));
    assert!(
        joined_opened.contains("line 20"),
        "all 20 lines displayed when opened"
    );
    assert!(
        !joined_opened.contains("more lines"),
        "no remaining lines indicator"
    );
}

#[test]
fn push_card_row_clamps_content_to_width() {
    let mut out = Vec::new();
    let spans = vec![Span::raw(
        "\tlong_text_with_tabs\t\t\t\t\tthat_is_very_long",
    )];
    push_card_row(&mut out, None, None, Color::Blue, 40, spans);
    assert_eq!(out.len(), 1);
    let row_w = out[0]
        .spans
        .iter()
        .map(|s| s.content.width())
        .sum::<usize>();
    assert!(row_w <= 40, "row width {row_w} exceeded 40");
    assert!(
        !out[0].spans.iter().any(|s| s.content.contains('\t')),
        "row contains tab"
    );
}

#[test]
fn push_user_card_never_exceeds_width_with_tabs() {
    let mut out = Vec::new();
    let query = "Fix this:\n\t\tfoo\tbar\tbaz\tqux\tvery_long_variable_name_here";
    push_user_card(&mut out, query, 32, 28);
    for line in &out {
        let row_w = line.spans.iter().map(|s| s.content.width()).sum::<usize>();
        assert!(
            row_w <= 32,
            "user card row width {row_w} exceeded width 32: {line:?}"
        );
        assert!(
            !line.spans.iter().any(|s| s.content.contains('\t')),
            "user card row contains unexpanded tab"
        );
    }
}

#[test]
fn tool_output_with_tabs_and_diffs_never_exceeds_width() {
    let item = TurnProcessItem {
        tool_name: "terminal".to_string(),
        call_formatted: "gh run view\t23908298717\t--log".to_string(),
        result_formatted: "✔ ok\tcompleted".to_string(),
        diff_lines: vec![
            "+\treturn\t\ttrue;".to_string(),
            "-\treturn\t\tfalse;".to_string(),
        ],
        output: "\tJob\t123\tsuccess\n\tStep\tbuild\tpassed\r\n\tStep\ttest\tpassed".to_string(),
        output_dropped: 0,
        arguments: None,
        duration_secs: Some(0.5),
        is_error: false,
    };

    let mut out = Vec::new();
    let mut hits = Vec::new();
    push_completed_tool_card(
        &mut out,
        &mut hits,
        &item,
        true,
        TranscriptClickTarget::ToggleTurnTools(0, 0),
        60,
        56,
    );

    for line in &out {
        let row_w = line.spans.iter().map(|s| s.content.width()).sum::<usize>();
        assert!(
            row_w <= 60,
            "tool card row width {row_w} exceeded 60: {line:?}"
        );
        assert!(
            !line.spans.iter().any(|s| s.content.contains('\t')),
            "tool card line contains unexpanded tab"
        );
        assert!(
            !line.spans.iter().any(|s| s.content.contains('\r')),
            "tool card line contains carriage return"
        );
    }
}

#[test]
fn test_subagent_inspector_lines_formatting() {
    let entry = SubagentUiEntry {
        id: "sub_12345".to_string(),
        agent: "Explore".to_string(),
        title: "Inspect repository architecture".to_string(),
        status: "completed".to_string(),
        activity: "done".to_string(),
        elapsed_secs: 14,
        turns: vec![TurnRecord {
            user_query: "Map the system architecture".to_string(),
            reasoning: Some("Inspecting source tree and entrypoints".to_string()),
            tools: Vec::new(),
            tool_items: vec![TurnProcessItem {
                tool_name: "read_file".to_string(),
                call_formatted: "read_file(Cargo.toml)".to_string(),
                result_formatted: "done".to_string(),
                diff_lines: Vec::new(),
                output: "package details".to_string(),
                output_dropped: 0,
                arguments: None,
                duration_secs: Some(0.2),
                is_error: false,
            }],
            duration: std::time::Duration::from_secs(14),
            reply: "The repository contains a modular CLI harness.".to_string(),
        }],
        log_tail: Vec::new(),
    };

    let lines = build_subagent_inspector_lines(&entry, 80);
    let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Task Goal:"));
    assert!(text.contains("Map the system architecture"));
    assert!(text.contains("Subagent [Explore]"));
    assert!(text.contains("◈ Thinking"));
    assert!(text.contains("read_file(Cargo.toml)"));
    assert!(text.contains("The repository contains a modular CLI harness."));
}

#[test]
fn test_subagent_inspector_renders_live_worker_log_tail_and_worker_header() {
    let entry = SubagentUiEntry {
        id: "se999".to_string(),
        agent: "worker:shiina".to_string(),
        title: "Fix the failing tests".to_string(),
        status: "running".to_string(),
        activity: "terminal running".to_string(),
        elapsed_secs: 3,
        turns: Vec::new(),
        log_tail: vec![
            "▶ Tool Call: terminal {\"command\":\"cargo test\"}".to_string(),
            "✔ Tool Result [terminal] (SUCCESS):".to_string(),
        ],
    };
    let lines = build_subagent_inspector_lines(&entry, 80);
    let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Task Goal:"));
    assert!(text.contains("Fix the failing tests"));
    assert!(text.contains("▶ Tool Call: terminal"));
    assert!(text.contains("Subagent is running (terminal running)"));

    let (_prefix, header) = format_opencode_tool_header(
        "delegate_task",
        Some(&serde_json::json!({
            "worker": "shiina",
            "task_id": "se999",
            "tasks": [{ "goal": "Fix the failing tests" }]
        })),
        None,
    );
    assert_eq!(header, "Subagent [shiina]: Fix the failing tests");
}

#[test]
fn test_subagents_chrome_strip_and_hint_counter() {
    let st = test_dummy_tui_state();
    let snap = TuiLiveSnapshot {
        delegate_count: 2,
        delegate_last: Some((
            "Review pull request changes".to_string(),
            "search_files running".to_string(),
        )),
        ..Default::default()
    };

    let chrome = build_chrome_lines(&st, &snap, 80);
    let chrome_text = chrome.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(chrome_text.contains("Subagent (+1): Review pull request"));
    assert!(chrome_text.contains("(Ctrl+B / click to inspect)"));

    let mut right_spans = Vec::new();
    if snap.delegate_count > 0 {
        let label = format!("● {} subagents · ", snap.delegate_count);
        right_spans.push(Span::styled(label, Style::default().fg(cyan_link())));
    }
    let rendered_label: String = right_spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(rendered_label, "● 2 subagents · ");
}

#[test]
fn test_background_memory_saver_chrome_and_notices() {
    let st = test_dummy_tui_state();
    let snap = TuiLiveSnapshot {
        memory_distilling: Some(("claude-3-5-sonnet".to_string(), 1.4)),
        memory_notices: vec![
            (
                "memory".to_string(),
                "user.preference.theme".to_string(),
                Some("dark mode enabled".to_string()),
                "saved".to_string(),
            ),
            (
                "note".to_string(),
                "Architecture Decision".to_string(),
                None,
                "updated".to_string(),
            ),
        ],
        ..Default::default()
    };

    let chrome = build_chrome_lines(&st, &snap, 80);
    let chrome_text = chrome.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(chrome_text.contains("Distilling memory · claude-3-5-sonnet… (1.4s)"));
    assert!(chrome_text.contains("🧠 memory user.preference.theme saved: dark mode enabled"));
    assert!(chrome_text.contains("📝 note Architecture Decision updated"));
}

#[test]
fn landing_status_lines_show_features_and_saver_mode() {
    let features = LandingFeaturesStatus {
        mcp: FeatureLandingItem {
            enabled: true,
            disabled_days_left: None,
        },
        skills: FeatureLandingItem {
            enabled: false,
            disabled_days_left: Some(3),
        },
        memory: FeatureLandingItem {
            enabled: false,
            disabled_days_left: None,
        },
        notes: FeatureLandingItem {
            enabled: true,
            disabled_days_left: None,
        },
        memory_mode: harness_core::MemoryMode::Background,
        pinned_skills: Vec::new(),
    };

    let wide = build_landing_status_lines(&features, 100);
    let wide_text: Vec<String> = wide.iter().map(|(l, _)| line_text(l)).collect();
    assert_eq!(wide_text.len(), 2);
    assert_eq!(
        wide_text[0],
        "● mcp on · ○ skills off (3d) · ○ memory off · ● notes on"
    );
    assert_eq!(wide_text[1], "saver: background saver");

    let narrow = build_landing_status_lines(&features, 48);
    assert_eq!(narrow.len(), 3);
}

#[test]
fn antigravity_footer_renders_account_and_circular_quota_windows() {
    assert_eq!(quota_circle_glyph(1.0), "●");
    assert_eq!(quota_circle_glyph(0.75), "◕");
    assert_eq!(quota_circle_glyph(0.50), "◑");
    assert_eq!(quota_circle_glyph(0.25), "◔");
    assert_eq!(quota_circle_glyph(0.05), "○");

    assert_eq!(quota_circle_color(0.8), green_ok());
    assert_eq!(quota_circle_color(0.4), yellow_warn());
    assert_eq!(quota_circle_color(0.1), red_err());

    let now = chrono::Utc::now();
    let reset_5h = (now + chrono::Duration::seconds(4 * 3600 + 30 * 60 + 30))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let reset_weekly = (now + chrono::Duration::seconds(167 * 3600 + 30))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let ui = crate::commands::chat::ui::ChatUi::new(true);
    ui.set_status("claude-sonnet-4-6", 1000, 200_000);
    ui.set_account_name(Some("daryll@example.com".to_string()));
    ui.set_quota_lines(
        "claude-sonnet-4-6",
        vec![
            harness_providers::QuotaLine {
                window: harness_providers::QuotaWindow::FiveHour,
                remaining_fraction: 0.78,
                reset_time: Some(reset_5h),
            },
            harness_providers::QuotaLine {
                window: harness_providers::QuotaWindow::Weekly,
                remaining_fraction: 0.25,
                reset_time: Some(reset_weekly),
            },
        ],
    );

    let snap = ui.tui_snapshot(90);
    assert_eq!(snap.account_name.as_deref(), Some("daryll@example.com"));
    assert!(has_antigravity_footer(&snap));

    let line = build_antigravity_footer_line(&snap, accent_coral(), 80);
    let text = line_text(&line);
    assert!(text.contains("daryll@example.com"), "{text}");
    assert!(text.contains("◕ 5h 78% (4h 30m)"), "{text}");
    assert!(text.contains("◔ w 25% (6d 23h)"), "{text}");
}

#[test]
fn test_turn_footer_renders_additions_and_deletions() {
    let turn = TurnRecord {
        user_query: "apply patch".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: vec![TurnProcessItem {
            tool_name: "patch".to_string(),
            call_formatted: "patch(path: \"src/main.rs\")".to_string(),
            result_formatted: "✔ applied".to_string(),
            diff_lines: vec![
                "\x1b[48;2;200;0;0m\x1b[38;2;255;255;255;1m  - old_code();\x1b[0m".to_string(),
                "\x1b[48;2;0;160;0m\x1b[38;2;255;255;255;1m  + new_code_1();\x1b[0m".to_string(),
                "\x1b[48;2;0;160;0m\x1b[38;2;255;255;255;1m  + new_code_2();\x1b[0m".to_string(),
            ],
            output: String::new(),
            output_dropped: 0,
            arguments: None,
            duration_secs: Some(1.2),
            is_error: false,
        }],
        duration: std::time::Duration::from_millis(2200),
        reply: "Patch applied successfully.".to_string(),
    };
    let mut out = Vec::new();
    let mut hits = Vec::new();
    let empty_set: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let empty_turns: std::collections::HashSet<usize> = std::collections::HashSet::new();
    render_single_settled_turn(
        &mut out,
        &mut hits,
        0,
        &turn,
        None,
        false,
        true,
        None,
        &empty_set,
        &empty_set,
        &empty_turns,
        &empty_turns,
        "Build",
        "gemini-3.8-flash",
        80,
        76,
    );
    let text: Vec<String> = out.iter().map(line_text).collect();
    let all_text = text.join("\n");
    assert!(all_text.contains("+2 -1"), "expected +2 -1 in {all_text}");

    // Turn with no diffs should NOT render +0 -0
    let no_diff_turn = TurnRecord {
        user_query: "hello".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: Vec::new(),
        duration: std::time::Duration::from_millis(1500),
        reply: "Hello.".to_string(),
    };
    let mut out_empty = Vec::new();
    let mut hits_empty = Vec::new();
    render_single_settled_turn(
        &mut out_empty,
        &mut hits_empty,
        0,
        &no_diff_turn,
        None,
        false,
        true,
        None,
        &empty_set,
        &empty_set,
        &empty_turns,
        &empty_turns,
        "Build",
        "gemini-3.8-flash",
        80,
        76,
    );
    let empty_text = out_empty
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!empty_text.contains("+0"), "expected no +0 in {empty_text}");
    assert!(!empty_text.contains("-0"), "expected no -0 in {empty_text}");
}

#[test]
fn test_total_code_changes_across_sessions() {
    let mut st = test_dummy_tui_state();
    st.turns.push(TurnRecord {
        user_query: "turn 1".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: vec![TurnProcessItem {
            tool_name: "patch".to_string(),
            call_formatted: "patch()".to_string(),
            result_formatted: "".to_string(),
            diff_lines: vec![
                "  + line a".to_string(),
                "  + line b".to_string(),
                "  - line c".to_string(),
            ],
            output: String::new(),
            output_dropped: 0,
            arguments: None,
            duration_secs: None,
            is_error: false,
        }],
        duration: std::time::Duration::ZERO,
        reply: "".to_string(),
    });

    let in_flight = vec![TurnProcessItem {
        tool_name: "write_file".to_string(),
        call_formatted: "write_file()".to_string(),
        result_formatted: "".to_string(),
        diff_lines: vec!["  + line d".to_string()],
        output: String::new(),
        output_dropped: 0,
        arguments: None,
        duration_secs: None,
        is_error: false,
    }];

    let (added, deleted) = st.total_code_changes(Some(&in_flight));
    assert_eq!(added, 3);
    assert_eq!(deleted, 1);

    // Test animation lifecycle
    st.update_diff_counter(Some(&in_flight));
    assert!(st.is_diff_animating());
    assert_eq!(st.diff_anim_target_added, 3);
    assert_eq!(st.diff_anim_target_deleted, 1);

    let (disp_a, disp_d, anim) = st.current_animated_diff();
    assert!(anim);
    assert!(disp_a <= 3);
    assert!(disp_d <= 1);

    // The roll starts on the previous count and climbs to the new one.
    let (added_label, deleted_label) = st.animated_diff_labels();
    assert_eq!(added_label.as_deref(), Some("0"));
    assert_eq!(deleted_label.as_deref(), Some("0"));

    // Once the roll window has passed the labels settle on the real totals.
    st.diff_anim_start = Some(std::time::Instant::now() - std::time::Duration::from_millis(800));
    assert!(!st.is_diff_animating());
    let (added_label, deleted_label) = st.animated_diff_labels();
    assert_eq!(added_label.as_deref(), Some("3"));
    assert_eq!(deleted_label.as_deref(), Some("1"));
}

#[test]
fn read_only_turns_never_animate_the_diff_counter() {
    let mut st = test_dummy_tui_state();
    st.turns.push(TurnRecord {
        user_query: "read a file".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: vec![TurnProcessItem {
            tool_name: "read_file".to_string(),
            call_formatted: "read_file(path: \"src/lib.rs\")".to_string(),
            result_formatted: "✔ read".to_string(),
            diff_lines: Vec::new(),
            output: "+ not a diff\n- still not a diff".to_string(),
            output_dropped: 0,
            arguments: Some(serde_json::json!({ "path": "src/lib.rs" })),
            duration_secs: Some(0.3),
            is_error: false,
        }],
        duration: std::time::Duration::ZERO,
        reply: String::new(),
    });

    st.update_diff_counter(None);
    assert!(!st.is_diff_animating(), "a read-only turn must not roll");
    assert_eq!(st.animated_diff_labels(), (None, None));
}

#[test]
fn a_removed_counter_side_stays_hidden() {
    let mut st = test_dummy_tui_state();
    st.diff_anim_target_added = 5;
    st.diff_anim_target_deleted = 0;
    st.diff_anim_from_added = 5;
    st.diff_anim_from_deleted = 0;

    let (added, deleted) = st.animated_diff_labels();
    assert_eq!(added.as_deref(), Some("5"));
    assert_eq!(deleted, None, "zero deletions must not render -0");
}

#[test]
fn diff_counter_never_resets_down_to_low_value() {
    let mut st = test_dummy_tui_state();
    let in_flight = vec![TurnProcessItem {
        tool_name: "patch".to_string(),
        call_formatted: "patch(...)".to_string(),
        result_formatted: "✔".to_string(),
        diff_lines: vec![
            "  + line a".to_string(),
            "  + line b".to_string(),
            "  - line c".to_string(),
        ],
        output: String::new(),
        output_dropped: 0,
        arguments: None,
        duration_secs: None,
        is_error: false,
    }];

    st.update_diff_counter(Some(&in_flight));
    assert_eq!(st.diff_anim_target_added, 2);
    assert_eq!(st.diff_anim_target_deleted, 1);

    // Simulate in-flight tool being taken (empty in-flight during await before turns.extend)
    st.update_diff_counter(None);
    assert_eq!(
        st.diff_anim_target_added, 2,
        "counter must not reset to 0 during transition"
    );
    assert_eq!(
        st.diff_anim_target_deleted, 1,
        "counter must not reset to 0 during transition"
    );

    // Simulate turn appended
    st.turns.push(TurnRecord {
        user_query: "query".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: in_flight,
        duration: std::time::Duration::ZERO,
        reply: "done".to_string(),
    });
    st.update_diff_counter(None);
    assert_eq!(st.diff_anim_target_added, 2);
    assert_eq!(st.diff_anim_target_deleted, 1);

    // Simulate compaction where turns are cleared but preserved in compacted counters
    st.turns.clear();
    st.compacted_added = 2;
    st.compacted_deleted = 1;
    st.update_diff_counter(None);
    assert_eq!(
        st.diff_anim_target_added, 2,
        "compacted counters preserve diff"
    );
    assert_eq!(
        st.diff_anim_target_deleted, 1,
        "compacted counters preserve diff"
    );
}

#[test]
fn settled_turn_registers_copy_hit_target_on_code_block_header() {
    let turn = TurnRecord {
        user_query: "show me rust".to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: Vec::new(),
        duration: Duration::from_millis(500),
        reply: "Here is a snippet:\n\n```rust\nlet x = 42;\nprintln!(\"{x}\");\n```\n".to_string(),
    };
    let mut out = Vec::new();
    let mut hits = Vec::new();
    let empty_set: HashSet<(usize, usize)> = HashSet::new();
    let empty_turns: HashSet<usize> = HashSet::new();
    render_single_settled_turn(
        &mut out,
        &mut hits,
        0,
        &turn,
        None,
        false,
        true,
        None,
        &empty_set,
        &empty_set,
        &empty_turns,
        &empty_turns,
        "Build",
        "gpt-4o",
        80,
        76,
    );

    let copy_hit = hits.iter().find_map(|(row, target)| match target {
        TranscriptClickTarget::CopyCodeBlock(code) => Some((*row, code.as_str())),
        _ => None,
    });
    let (row, code) = copy_hit.expect("expected CopyCodeBlock hit target");
    assert_eq!(code, "let x = 42;\nprintln!(\"{x}\");\n");
    let header_row = line_text(&out[row]);
    assert!(
        header_row.contains("rust") && header_row.contains("copy"),
        "hit target row should be the code block header: {header_row:?}"
    );
}

fn make_bg_slot(
    _st: &TuiState,
    session_id: &str,
    pending_modals: std::collections::VecDeque<crate::commands::chat::tui::state::PendingModal>,
    has_unread_done: bool,
) -> crate::commands::chat::tui::state::BackgroundSession {
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    let tmp = tempfile::tempdir().unwrap();
    let memory = Arc::new(harness_memory::MemoryEngineKind::Sqlite(
        harness_memory::SqliteMemoryEngine::in_memory().unwrap(),
    ));
    let notes =
        Arc::new(harness_notes::FileNotesEngine::new(tmp.path().join("notes"), None).unwrap());
    let registry = Arc::new(harness_core::DefaultToolRegistry::new());
    let creds = harness_providers::CredentialManager::new(None).unwrap();
    let discovery = Arc::new(harness_providers::ModelDiscovery::new(
        None,
        0,
        Default::default(),
    ));
    let provider = Arc::new(
        harness_providers::GenericProviderClient::new(
            "openai".to_string(),
            "gpt-4o".to_string(),
            creds,
            discovery,
            None,
        )
        .unwrap(),
    );
    let agent = Arc::new(tokio::sync::Mutex::new(harness_core::Agent::new(
        provider,
        memory,
        notes,
        registry,
        harness_core::AgentConfig::default(),
    )));
    crate::commands::chat::tui::state::BackgroundSession {
        session_id: session_id.to_string(),
        agent,
        ui: Arc::new(crate::commands::chat::ui::ChatUi::new(false)),
        in_flight_tools: Arc::new(Mutex::new(Vec::new())),
        interruptions: Arc::new(Mutex::new(Vec::new())),
        steer_handle: Arc::new(Mutex::new(VecDeque::new())),
        interrupt_handle: Arc::new(harness_core::InterruptHandle::default()),
        compacting: Arc::new(AtomicBool::new(false)),
        provider: "openai".to_string(),
        model: "gpt-4o".to_string(),
        reasoning_effort: None,
        mode: Default::default(),
        turns: Vec::new(),
        turn_meta: Vec::new(),
        settled_cache: Default::default(),
        system_notices: Vec::new(),
        expanded_thinking: HashSet::new(),
        expanded_tools: HashSet::new(),
        expanded_turns: HashSet::new(),
        collapsed_turns: HashSet::new(),
        expanded_live_thinking: HashSet::new(),
        expanded_live_tools: HashSet::new(),
        live_thinking_view: Default::default(),
        process_expanded: false,
        scroll_from_bottom: 0,
        scroll_anchor_top: None,
        queue: VecDeque::new(),
        current_cancel: tokio_util::sync::CancellationToken::new(),
        active_turn: None,
        active_goal: None,
        current_query: String::new(),
        turn_start: std::time::Instant::now(),
        idle_pass: crate::commands::chat::helpers::IdleMemoryPass::new(0),
        pending_images: Arc::new(Mutex::new(Vec::new())),
        pending_job_turn: None,
        memory_project_filter: None,
        feature_toggles: (true, true, true, true),
        compacted_added: 0,
        compacted_deleted: 0,
        session_max_added: 0,
        session_max_deleted: 0,
        pending_modals,
        has_unread_done,
        active_orchestrator: None,
    }
}

#[test]
fn tab_bar_colors_background_pending_popup_yellow_done_green_and_inactive_plain() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::VecDeque;

    let mut st = test_dummy_tui_state();
    st.tabs = vec![
        crate::commands::chat::tui::state::SessionTab {
            id: "s_active".to_string(),
            title: "active".to_string(),
        },
        crate::commands::chat::tui::state::SessionTab {
            id: "s_ask".to_string(),
            title: "needs-ok".to_string(),
        },
        crate::commands::chat::tui::state::SessionTab {
            id: "s_done".to_string(),
            title: "finished".to_string(),
        },
        crate::commands::chat::tui::state::SessionTab {
            id: "s_idle".to_string(),
            title: "idle".to_string(),
        },
    ];

    let (tx, _rx) = tokio::sync::oneshot::channel();
    let mut ask_queue = VecDeque::new();
    ask_queue.push_back(crate::commands::chat::tui::state::PendingModal::Approval {
        req: harness_core::ApprovalRequest::default(),
        resp_tx: tx,
    });
    st.background_sessions.insert(
        "s_ask".to_string(),
        make_bg_slot(&st, "s_ask", ask_queue, false),
    );
    st.background_sessions.insert(
        "s_done".to_string(),
        make_bg_slot(&st, "s_done", VecDeque::new(), true),
    );
    st.background_sessions.insert(
        "s_idle".to_string(),
        make_bg_slot(&st, "s_idle", VecDeque::new(), false),
    );

    let backend = TestBackend::new(100, 3);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 100, 1);
            render_tab_bar(f, area, &mut st, "s_active");
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    let row_str: String = (0..100).map(|x| buf[(x, 0)].symbol()).collect();
    assert!(row_str.contains("needs-ok"), "tab bar: {row_str}");
    assert!(row_str.contains("finished"), "tab bar: {row_str}");
    assert!(row_str.contains("idle"), "tab bar: {row_str}");

    let find_col = |needle: &str| -> u16 {
        let byte_idx = row_str.find(needle).expect("tab present");
        row_str[..byte_idx].chars().count() as u16
    };
    let ask_x = find_col("needs-ok");
    let done_x = find_col("finished");
    let idle_x = find_col("idle");

    // Yellow indicator dot + yellow label for `needs-ok`.
    assert_eq!(buf[(ask_x - 2, 0)].symbol(), "●");
    assert_eq!(buf[(ask_x - 2, 0)].fg, yellow_warn());
    assert_eq!(buf[(ask_x, 0)].fg, yellow_warn());

    // Green dot for `finished`.
    assert_eq!(buf[(done_x - 2, 0)].symbol(), "●");
    assert_eq!(buf[(done_x - 2, 0)].fg, green_ok());

    // Inactive `idle` has no dot or spinner before its label.
    assert_eq!(buf[(idle_x - 1, 0)].symbol(), " ");
    assert_ne!(buf[(idle_x - 2, 0)].symbol(), "●");
}

#[test]
fn promote_pending_modal_skips_closed_and_waits_while_modal_open() {
    let mut st = test_dummy_tui_state();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    drop(closed_rx);
    let (open_tx, _open_rx) = tokio::sync::oneshot::channel();

    st.pending_modals
        .push_back(crate::commands::chat::tui::state::PendingModal::Sudo {
            prompt: "ls".to_string(),
            resp_tx: closed_tx,
        });
    st.pending_modals
        .push_back(crate::commands::chat::tui::state::PendingModal::Approval {
            req: harness_core::ApprovalRequest::default(),
            resp_tx: open_tx,
        });

    assert!(st.promote_pending_modal());
    assert!(matches!(st.modal, ActiveModal::Approval { .. }));
    assert!(st.pending_modals.is_empty());
}

#[test]
fn orchestrator_editor_renders_main_subagents_and_effort_slider() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut st = test_dummy_tui_state();
    st.modal = ActiveModal::OrchestratorEditor {
        name: "coder".to_string(),
        draft: harness_core::OrchestratorConfig {
            orchestrator: harness_core::OrchestratorModelSpec {
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
                thinking: Some(harness_core::ReasoningEffort::Medium),
            },
            subagents: vec![harness_core::OrchestratorModelSpec {
                provider: "anthropic".to_string(),
                model: "claude-sonnet-4".to_string(),
                thinking: Some(harness_core::ReasoningEffort::Low),
            }],
        },
        selected: 0,
        return_to_list: false,
    };

    let backend = TestBackend::new(90, 28);
    let mut terminal = Terminal::new(backend).unwrap();
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
    terminal
        .draw(|f| {
            draw(f, &mut st, &snap, &info);
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..28 {
        for x in 0..90 {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(text.contains("Orchestrator · coder"), "header: {text}");
    assert!(text.contains("Main: "), "main row: {text}");
    assert!(text.contains("Subagent #1"), "subagent row: {text}");
    assert!(text.contains("+ Add subagent"), "add row: {text}");
    assert!(
        text.contains("off") && text.contains("medium") && text.contains("xhigh"),
        "effort slider: {text}"
    );
    assert!(text.contains("[ Save Orchestrator ]"), "save row: {text}");
}

#[tokio::test]
async fn build_provider_items_filters_unconfigured_catalog_overrides_and_sorts_by_usage() {
    use harness_core::config::ProviderOverride;
    use harness_core::usage::{PriceTable, UsageLedger, UsageRecord};
    use std::collections::BTreeMap;

    let tmp = tempfile::tempdir().unwrap();
    let cred_path = tmp.path().join("creds.json");
    let creds = harness_providers::CredentialManager::new(Some(cred_path)).unwrap();
    creds
        .add_credential("openai", "k1", "sk-1", false)
        .await
        .unwrap();
    creds
        .add_credential("anthropic", "k2", "sk-2", false)
        .await
        .unwrap();

    // Simulate config.yaml containing unconfigured catalog entries (like `gemini` with empty
    // override) plus a genuine custom provider with a base_url.
    let mut cfg_providers = BTreeMap::new();
    cfg_providers.insert("gemini".to_string(), ProviderOverride::default());
    cfg_providers.insert(
        "my-local".to_string(),
        ProviderOverride {
            base_url: Some("http://localhost:1234/v1".to_string()),
            ..Default::default()
        },
    );

    let usage_db = tmp.path().join("usage.db");
    let ledger = UsageLedger::open(Some(usage_db.clone())).unwrap();
    let prices = PriceTable::default();
    let tok = harness_core::TokenUsage::default();
    for _ in 0..3 {
        ledger
            .record(&UsageRecord::for_turn(
                "anthropic",
                "claude-sonnet-4",
                &tok,
                100,
                &prices,
            ))
            .await
            .unwrap();
    }
    ledger
        .record(&UsageRecord::for_turn(
            "openai", "gpt-4o", &tok, 100, &prices,
        ))
        .await
        .unwrap();

    let items = crate::commands::chat::tui::actions::pickers::build_provider_items_with_usage(
        creds,
        Some(&cfg_providers),
        "openai",
        Some(&usage_db),
    )
    .await;

    let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    assert!(
        !ids.contains(&"gemini"),
        "unconfigured catalog override must not appear: {ids:?}"
    );
    assert!(
        ids.contains(&"my-local"),
        "custom provider with base_url must appear: {ids:?}"
    );
    let anth_pos = ids.iter().position(|&id| id == "anthropic").unwrap();
    let openai_pos = ids.iter().position(|&id| id == "openai").unwrap();
    assert!(
        anth_pos < openai_pos,
        "most-used provider (anthropic=3) must sort above openai=1: {ids:?}"
    );
}

#[test]
fn slash_popup_follows_composer_width_and_aligns_descriptions() {
    use crate::commands::chat::commands::CommandEntry;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut st = test_dummy_tui_state();
    st.slash_popup.matches = vec![
        CommandEntry {
            name: "/model".into(),
            details: "Switch provider and model".into(),
        },
        CommandEntry {
            name: "/default_model".into(),
            details: "Choose the default provider".into(),
        },
    ];
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    let screen = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 20,
    };
    let composer_rect = Rect {
        x: 0,
        y: 17,
        width: 120,
        height: 3,
    };

    terminal
        .draw(|frame| render_slash_popup(frame, screen, composer_rect, &st))
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    };

    // Two entries plus borders, drawn directly above the composer.
    let top = 13;
    assert_eq!(row(top).chars().next(), Some('┌'));
    assert_eq!(row(top).chars().last(), Some('┐'));

    // The description column is shared, so both lines start at the same cell.
    let first = row(top + 1);
    let second = row(top + 2);
    let first_desc = first.find("Switch provider").expect("first description");
    let second_desc = second
        .find("Choose the default")
        .expect("second description");
    assert_eq!(
        first_desc, second_desc,
        "descriptions must align:\n{first}\n{second}"
    );
}

#[test]
fn composer_card_footer_renders_orchestrator_badge_when_active() {
    let mut st = test_dummy_tui_state();
    st.active_orchestrator = Some("try".to_string());

    let backend = CrosstermBackend::new(Vec::new());
    let mut terminal = Terminal::new(backend).unwrap();
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 6,
    };

    let info = RenderInfo {
        session_id: "s123",
        provider: "nvidia",
        model: "nemotron-3-ultra-550b-a55b",
        reasoning_effort: Some(harness_core::ReasoningEffort::XHigh),
    };
    let snap = harness_core::usage::SessionSnapshot::default();
    let input = crate::commands::chat::tui::render::composer::wrap_composer_text("", 90);

    terminal
        .draw(|frame| {
            crate::commands::chat::tui::render::composer::render_composer_card(
                frame, area, &st, info, snap, &input, false,
            );
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let footer: String = (0..buffer.area.width)
        .map(|x| buffer[(x, area.height - 1)].symbol())
        .collect();

    assert!(
        footer.contains("Orchestrator: try"),
        "footer should show active orchestrator name, got:\n{footer}"
    );
    assert!(
        !footer.contains("Nvidia"),
        "footer should not show bare provider when orchestrator is active, got:\n{footer}"
    );
}

#[test]
fn orchestrator_active_strips_active_tag_from_underlying_providers() {
    let mut items = vec![
        crate::commands::chat::tui::state::ProviderPickerItem {
            id: "nvidia".to_string(),
            display_name: "NVIDIA NIM".to_string(),
            tag: "nvidia · active · configured".to_string(),
            is_current: true,
        },
        crate::commands::chat::tui::state::ProviderPickerItem {
            id: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
            tag: "configured".to_string(),
            is_current: false,
        },
    ];

    let orch_active = true;
    if orch_active {
        for item in &mut items {
            item.is_current = false;
            let tags: Vec<&str> = item.tag.split(" · ").filter(|t| *t != "active").collect();
            item.tag = tags.join(" · ");
        }
    }

    assert!(!items[0].is_current);
    assert_eq!(items[0].tag, "nvidia · configured");
    assert!(!items[1].is_current);
    assert_eq!(items[1].tag, "configured");
}
