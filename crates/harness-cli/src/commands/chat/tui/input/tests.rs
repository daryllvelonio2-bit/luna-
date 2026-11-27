//! Input unit tests.

use super::super::state::TextSelection;
use super::mouse::{cursor_byte_from_coords, find_word_bounds, selection_text_from, slice_cells};

#[test]
fn slice_cells_uses_terminal_columns() {
    assert_eq!(slice_cells("abcdef", 1, 4), "bcd");
    assert_eq!(slice_cells("abcdef", 0, usize::MAX), "abcdef");
    assert_eq!(slice_cells("abcdef", 4, 4), "");
}

#[test]
fn slice_cells_keeps_wide_glyphs_whole() {
    // `中` occupies cells 1-2, so selecting only its first cell still copies the glyph.
    assert_eq!(slice_cells("a中b", 1, 2), "中");
    assert_eq!(slice_cells("a中b", 2, 3), "中");
    assert_eq!(slice_cells("a中b", 3, 4), "b");
}

fn selection(anchor: (usize, usize), cursor: (usize, usize)) -> TextSelection {
    TextSelection {
        anchor,
        cursor,
        dragging: true,
    }
}

#[test]
fn selection_text_includes_the_cell_under_the_cursor() {
    let lines = vec!["hello world".to_string()];
    assert_eq!(
        selection_text_from(&lines, 0, 20, selection((0, 0), (0, 4))),
        "hello"
    );
}

#[test]
fn selection_text_normalizes_a_backwards_drag() {
    let lines = vec!["hello world".to_string()];
    assert_eq!(
        selection_text_from(&lines, 0, 20, selection((0, 4), (0, 0))),
        "hello"
    );
}

#[test]
fn selection_text_joins_multiple_visible_lines() {
    let lines = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
    // Lines 10..12 are visible; start mid-line 11 and end mid-line 12.
    assert_eq!(
        selection_text_from(&lines, 10, 20, selection((11, 1), (12, 2))),
        "eta\ngam"
    );
}

#[test]
fn selection_text_skips_lines_outside_the_visible_snapshot() {
    let lines = vec!["only".to_string()];
    assert_eq!(
        selection_text_from(&lines, 5, 20, selection((4, 0), (5, 3))),
        "only"
    );
    assert_eq!(
        selection_text_from(&lines, 5, 0, selection((5, 0), (5, 3))),
        ""
    );
}

#[test]
fn find_word_bounds_identifies_words_and_flags() {
    let line = "cargo test --release /path/to/file.rs";
    assert_eq!(find_word_bounds(line, 7), (6, 10)); // "test"
    assert_eq!(find_word_bounds(line, 13), (11, 20)); // "--release"
    assert_eq!(find_word_bounds(line, 25), (21, 37)); // "/path/to/file.rs"
    assert_eq!(find_word_bounds(line, 5), (5, 6)); // whitespace
}

#[test]
fn cursor_byte_from_coords_maps_lines_and_columns() {
    let text = "hello\nworld";
    assert_eq!(cursor_byte_from_coords(text, 0, 0, 40), 0);
    assert_eq!(cursor_byte_from_coords(text, 0, 3, 40), 3);
    assert_eq!(cursor_byte_from_coords(text, 0, 10, 40), 5);
    assert_eq!(cursor_byte_from_coords(text, 1, 0, 40), 6);
    assert_eq!(cursor_byte_from_coords(text, 1, 2, 40), 8);
}

#[test]
fn approval_ask_and_suggest_take_different_routes() {
    use super::super::state::ApprovalInputMode;
    use super::modals::{
        apply_approval_submit, approval_suggestion_payload, ApprovalSubmitOutcome,
    };

    let req = harness_core::ApprovalRequest {
        tool: "terminal".to_string(),
        call_id: "c1".to_string(),
        target: Some("cargo test".to_string()),
        diff: None,
        arguments: serde_json::Value::Null,
    };

    // Whitespace is not a suggestion.
    assert_eq!(approval_suggestion_payload(&req, "   "), None);

    // A suggestion carries the tool and its target, so the model knows what and where.
    let payload = approval_suggestion_payload(&req, " use pytest ").expect("payload");
    assert!(payload.contains("terminal"), "{payload}");
    assert!(payload.contains("cargo test"), "{payload}");
    assert!(payload.ends_with("use pytest"), "{payload}");

    // Enter in ask mode runs the Q&A route and sends no decision.
    let mut asked: Vec<String> = Vec::new();
    let outcome = apply_approval_submit(ApprovalInputMode::Ask, &req, "why?", false, |q| {
        asked.push(q)
    });
    assert_eq!(outcome, ApprovalSubmitOutcome::Asked("why?".to_string()));
    assert_eq!(asked, vec!["why?".to_string()]);

    // Enter in suggest mode yields the denial payload instead of asking.
    match apply_approval_submit(
        ApprovalInputMode::Suggest,
        &req,
        "use pytest",
        false,
        |_| {},
    ) {
        ApprovalSubmitOutcome::Suggested(feedback) => {
            assert!(feedback.ends_with("use pytest"), "{feedback}")
        }
        other => panic!("expected a suggestion, got {other:?}"),
    }

    // Empty text in suggest mode asks for text and sends nothing.
    assert_eq!(
        apply_approval_submit(ApprovalInputMode::Suggest, &req, "  ", false, |_| {}),
        ApprovalSubmitOutcome::NeedText(ApprovalInputMode::Suggest)
    );

    // A second Enter while the model is still answering must not send again.
    assert_eq!(
        apply_approval_submit(ApprovalInputMode::Ask, &req, "again?", true, |_| {}),
        ApprovalSubmitOutcome::Pending
    );
}
