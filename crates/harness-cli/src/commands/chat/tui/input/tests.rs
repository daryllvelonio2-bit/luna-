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
