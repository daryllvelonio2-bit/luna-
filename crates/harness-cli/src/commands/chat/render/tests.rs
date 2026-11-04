use super::*;
use serde_json::json;

#[test]
fn shorten_path_keeps_at_most_three_segments() {
    assert_eq!(
        shorten_path("~/Downloads/rust-agent-harness/crates/harness-core/src/tools/todo.rs"),
        "src/tools/todo.rs"
    );
    assert_eq!(
        shorten_path("downloads/rust-agent-harness/agents.md"),
        "downloads/rust-agent-harness/agents.md"
    );
    assert_eq!(
        shorten_path("/home/janelle/Downloads/rust-agent-harness/crates/harness-core/prompts.md"),
        "crates/harness-core/prompts.md"
    );
    assert_eq!(shorten_path("agents.md"), "agents.md");
    assert_eq!(shorten_path("."), ".");
}

#[test]
fn shorten_path_keeps_a_windows_drive_letter() {
    assert_eq!(
        shorten_path(r"C:\Users\me\projects\harness\src\main.rs"),
        "C:/src/main.rs"
    );
    assert_eq!(
        shorten_path(r"\\server\share\team\project\src\lib.rs"),
        "project/src/lib.rs"
    );
}

fn call(name: &str, arguments: serde_json::Value) -> String {
    format_tool_call(name, &arguments)
}

#[test]
fn terminal_call_shows_the_command_and_directory() {
    let line = call(
        "terminal",
        json!({ "command": "cargo test\n--all", "workdir": "/repo" }),
    );
    assert!(line.contains("cargo test"), "got: {}", line);
    assert!(line.contains("in /repo"), "got: {}", line);
    assert!(
        !line.contains("--all"),
        "only the first command line is shown"
    );
}

#[test]
fn read_call_shows_the_line_range() {
    let ranged = call(
        "read_file",
        json!({ "path": "src/lib.rs", "offset": 10, "limit": 20 }),
    );
    assert!(ranged.contains("L10-29"), "got: {}", ranged);
    assert!(call("read_file", json!({ "path": "src/lib.rs" })).contains("src/lib.rs"));
}

#[test]
fn write_call_shows_the_payload_size() {
    let line = call(
        "write_file",
        json!({ "path": "a.rs", "content": "one\ntwo\nthree" }),
    );
    assert!(line.contains("3 lines"), "got: {}", line);
}

#[test]
fn patch_call_flags_replace_all() {
    let one = call(
        "patch",
        json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
    );
    assert!(!one.contains("all occurrences"), "got: {}", one);

    let all = call(
        "patch",
        json!({ "path": "a.rs", "old_string": "x", "new_string": "y", "replace_all": true }),
    );
    assert!(all.contains("all occurrences"), "got: {}", all);
}

/// The reader must be able to tell which search mode ran, and empty optionals must not
/// render as blank gaps.
#[test]
fn search_call_names_the_mode_and_defaults_the_path() {
    let content = call(
        "search_files",
        json!({ "pattern": "needle", "path": "", "file_glob": "*" }),
    );
    assert!(content.contains("\"needle\""), "got: {}", content);
    assert!(
        content.contains("in ."),
        "an empty path renders as the workspace root"
    );
    assert!(
        !content.contains("matching"),
        "a `*` glob filters nothing and is hidden"
    );
    assert!(
        !content.contains("(content)"),
        "the default mode carries no tag: {}",
        content
    );

    let files = call(
        "search_files",
        json!({ "pattern": "needle", "output_mode": "files_only" }),
    );
    assert!(files.contains("(files_only)"), "got: {}", files);

    let scoped = call(
        "search_files",
        json!({ "pattern": "needle", "file_glob": "*.rs" }),
    );
    assert!(scoped.contains("matching *.rs"), "got: {}", scoped);
}

#[test]
fn memory_and_note_calls_show_their_subject() {
    let memory = call(
        "save_memory",
        json!({ "subject": "user", "predicate": "editor", "object": "neovim" }),
    );
    assert!(memory.contains("memory save"), "got: {}", memory);
    assert!(memory.contains("user.editor"), "got: {}", memory);
    assert!(memory.contains("neovim"), "got: {}", memory);

    let update = call(
        "update_memory",
        json!({ "memory_id": "mem_123", "subject": "user", "predicate": "editor", "object": "helix" }),
    );
    assert!(update.contains("memory update"), "got: {}", update);
    assert!(update.contains("user.editor"), "got: {}", update);

    assert!(call("search_notes", json!({ "query": "budget", "limit": 5 })).contains("limit 5"));
    assert!(call("read_note", json!({ "id": "note_123" })).contains("note_123"));
}

#[test]
fn unknown_and_mcp_tools_fall_back_to_named_arguments() {
    let mcp = call(
        "mcp__filesystem__read",
        json!({ "path": "/etc/hosts", "missing": null }),
    );
    assert!(mcp.contains("filesystem/read"), "got: {}", mcp);
    assert!(mcp.contains("path=\"/etc/hosts\""), "got: {}", mcp);
    assert!(!mcp.contains("missing"), "null arguments are hidden");
}

/// The clarify box prints the question on screen itself, so the call line must not repeat it:
/// the fallback rendered `question="…"` (the same paragraph twice), and a multi-question call
/// dumped the whole `questions` array as raw JSON next to the box.
#[test]
fn a_clarify_call_reports_how_many_questions_not_the_questions() {
    let single = call(
        "clarify",
        json!({
            "question": "Which database do you prefer?",
            "choices": ["PostgreSQL", "SQLite"]
        }),
    );
    assert!(single.contains("ask"), "got: {}", single);
    assert!(single.contains("1 question"), "got: {}", single);
    assert!(
        !single.contains("database"),
        "the question belongs to the box, not the transcript: {}",
        single
    );
    assert!(
        !single.contains("PostgreSQL"),
        "the choices belong to the box too: {}",
        single
    );

    let many = call(
        "clarify",
        json!({
            "questions": [
                { "question": "Language?", "choices": ["Rust", "Go"] },
                { "question": "Framework?" }
            ]
        }),
    );
    assert!(many.contains("2 questions"), "got: {}", many);
    assert!(
        !many.contains("Language") && !many.contains("Framework"),
        "no question text may leak into the call line: {}",
        many
    );
    assert!(
        many.chars().count() < 40,
        "the line stays one short line: {}",
        many
    );
}

#[test]
fn long_arguments_are_clipped_to_one_line() {
    let line = call("terminal", json!({ "command": "x".repeat(500) }));
    assert!(
        line.chars().count() < 500,
        "a long command must not wrap the transcript: {} chars",
        line.chars().count()
    );
    assert!(line.contains('…'));
}

#[test]
fn long_results_collapse_to_a_line_count() {
    let output = (0..40)
        .map(|i| format!("line {}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let rendered = format_tool_result(false, &output);
    assert!(rendered.contains("line 0"), "got: {}", rendered);
    assert!(rendered.contains("(+39 lines)"), "got: {}", rendered);
}

#[test]
fn errors_report_the_first_line() {
    let rendered = format_tool_result(true, "boom: file not found\nmore detail");
    assert!(
        rendered.contains("boom: file not found"),
        "got: {}",
        rendered
    );
    assert!(!rendered.contains("more detail"), "got: {}", rendered);
}

#[test]
fn activity_line_animates_the_frame_and_reports_elapsed_time() {
    let first = activity_line(0, "", None, Duration::from_millis(1400), 80);
    let second = activity_line(1, "", None, Duration::from_millis(1500), 80);
    assert_ne!(first, second, "each frame must differ, or nothing animates");
    assert!(first.contains("1.4s"), "got: {}", first);

    let patch = activity_line(0, "patch a.rs", Some("−3 +1"), Duration::from_millis(0), 80);
    assert!(patch.contains("−3 +1"), "got: {}", patch);
}

/// The whole point of the live line: the call is on screen while it runs, not only after it
/// returns. A regression that drops the call from the frame puts the user back to
/// result-only rendering on a slow tool.
#[test]
fn activity_line_carries_the_call_while_it_is_still_running() {
    let line = activity_line(
        0,
        "terminal \"cargo test\"",
        None,
        Duration::from_millis(300),
        80,
    );
    assert!(line.contains("cargo test"), "got: {}", line);
    assert!(line.contains("0.3s"), "got: {}", line);

    let empty = activity_line(0, "", None, Duration::from_millis(300), 80);
    assert!(
        empty.contains("running"),
        "a call with no formatting still says something: {}",
        empty
    );
}

/// The live region moves the cursor by counting visible characters per row, so a call wider
/// than the terminal must be cut down to one physical row — never allowed to wrap.
#[test]
fn activity_line_never_exceeds_the_terminal_width() {
    let long = "terminal \"cargo test --workspace --all-features --release\"";
    for width in [24usize, 40, 80, 200] {
        let row = activity_line(0, long, Some("−3 +1"), Duration::from_millis(1200), width);
        let visible = dialoguer::console::strip_ansi_codes(&row).chars().count();
        assert!(
            visible <= width.max(LIVE_CALL_MIN_CHARS + 20),
            "width {width} produced {visible} visible chars: {row}"
        );
    }
}

#[test]
fn the_compaction_bar_keeps_its_length_and_its_lit_run() {
    for cells in [1usize, 5, COMPACTION_BAR_CELLS, 40] {
        for frame in 0..64 {
            let bar = compaction_bar(frame, cells);
            assert_eq!(
                bar.chars().count(),
                cells,
                "frame {frame} of {cells} cells: {bar}"
            );
            let lit = bar.chars().filter(|c| *c == '█').count();
            assert_eq!(
                lit,
                COMPACTION_BAR_SWEEP.min(cells),
                "frame {frame} of {cells} cells: {bar}"
            );
        }
    }
    assert_eq!(compaction_bar(0, 0), "", "no cells, no bar");
}

/// The sweep is the animation: a bar that does not move is a static row, and a user watching a
/// fold would have no way to tell a slow summary from a hung process.
#[test]
fn the_compaction_bar_sweeps_and_wraps() {
    let cells = COMPACTION_BAR_CELLS;
    let first = compaction_bar(0, cells);
    assert_ne!(first, compaction_bar(3, cells), "the head moved");
    assert_eq!(
        first,
        compaction_bar(COMPACTION_BAR_HOLD * cells, cells),
        "the sweep wraps back to where it started"
    );
    // Every position is visited: the head walks the whole track.
    let positions: std::collections::HashSet<String> = (0..cells)
        .map(|step| compaction_bar(step * COMPACTION_BAR_HOLD, cells))
        .collect();
    assert_eq!(positions.len(), cells, "one distinct frame per cell");
}

#[test]
fn the_compaction_line_animates_and_says_why_the_fold_is_happening() {
    let automatic = compaction_line(0, false, Duration::from_millis(4200), 100);
    assert!(automatic.contains("compacting context"), "got: {automatic}");
    assert!(automatic.contains("automatic"), "got: {automatic}");
    assert!(automatic.contains("4.2s"), "got: {automatic}");

    let requested = compaction_line(0, true, Duration::from_millis(0), 100);
    assert!(requested.contains("requested"), "got: {requested}");
    assert_ne!(
        compaction_line(0, true, Duration::from_millis(0), 100),
        compaction_line(COMPACTION_BAR_HOLD, true, Duration::from_millis(0), 100),
        "each frame must differ, or nothing animates"
    );
}

#[test]
fn the_compaction_line_never_exceeds_the_terminal_width() {
    for width in [24usize, 40, 80, 200] {
        for forced in [false, true] {
            for frame in [0usize, 5, 11] {
                let row = compaction_line(frame, forced, Duration::from_millis(900), width);
                let plain = dialoguer::console::strip_ansi_codes(&row).to_string();
                let visible = dialoguer::console::measure_text_width(&plain);
                assert!(
                    visible <= width,
                    "width {width} produced {visible} visible cells: {row}"
                );
            }
        }
    }
    // The bar is what gives way first, and the words survive the squeeze.
    let narrow = dialoguer::console::strip_ansi_codes(&compaction_line(
        0,
        true,
        Duration::from_millis(900),
        24,
    ))
    .to_string();
    assert!(!narrow.contains('█'), "no bar in 24 columns: {narrow}");
    assert!(
        narrow.contains("requested"),
        "the reason survives: {narrow}"
    );
}

/// A row is measured in terminal cells, not chars: `💻` is one char but two cells, so a
/// chars-based row printed wider than the terminal, soft-wrapped into a second physical row,
/// and the live region's erase left one orphan frame behind per animation tick — the
/// reported "running tool spams the screen with copies of itself". Cell width is the
/// contract here; a regression to char counting reproduces the stack.
#[test]
fn an_activity_row_never_exceeds_the_terminal_in_cells() {
    let long = "💻 run for f in dark light midnight; do magick assets/$f.gif -coalesce \
                -crop 1x1+0+0 -format \"%[pixel:p{0,0}]\" info:; done";
    for width in [40usize, 80, 120] {
        for frame in 0..SPINNER_FRAMES.len() {
            let row = activity_line(frame, long, None, Duration::from_millis(3700), width);
            let cells =
                dialoguer::console::measure_text_width(&dialoguer::console::strip_ansi_codes(&row));
            assert!(
                cells <= width,
                "width {width} frame {frame} rendered {cells} cells (> width, would \
                 soft-wrap and orphan): {row}"
            );
        }
    }
}

/// wrap_rows is the live region's other producer (reasoning rows): the same cell contract.
#[test]
fn wrapped_rows_never_exceed_their_width_in_cells() {
    let text = "💻 a very long reasoning line with 📖 emoji 📝 scattered through 🤖 it";
    for width in [10usize, 20, 40] {
        for row in wrap_rows(text, width) {
            let cells = dialoguer::console::measure_text_width(&row);
            assert!(
                cells <= width,
                "width {width} produced a {cells}-cell row: {row}"
            );
        }
    }
}

/// truncate_to_cells keeps the ellipsis within the cell budget too.
#[test]
fn truncation_to_cells_counts_the_ellipsis() {
    let out = truncate_to_cells("💻💻💻💻💻💻💻💻", 5);
    assert_eq!(
        dialoguer::console::measure_text_width(&out),
        5,
        "got: {out}"
    );
    assert!(out.ends_with('…'), "got: {out}");
    // Untouched when it already fits.
    assert_eq!(truncate_to_cells("💻 hi", 10), "💻 hi");
}

#[test]
fn a_patch_reports_the_size_of_its_edit() {
    let detail = tool_activity_detail(
        "patch",
        &json!({ "old_string": "a\nb\nc", "new_string": "d" }),
    );
    assert_eq!(detail.as_deref(), Some("−3 +1"));

    assert_eq!(
        tool_activity_detail("write_file", &json!({ "content": "x\ny" })).as_deref(),
        Some("+2")
    );
    assert_eq!(
        tool_activity_detail("terminal", &json!({ "command": "ls" })),
        None
    );
}

/// Every row must fit one physical terminal row, because the live region moves the cursor by
/// counting rows and a wrapped row would desynchronise it.
#[test]
fn wrapping_never_exceeds_the_width() {
    let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
    for row in wrap_rows(text, 20) {
        assert!(row.chars().count() <= 20, "row overflowed: {:?}", row);
    }

    let long_word = "x".repeat(50);
    for row in wrap_rows(&long_word, 12) {
        assert!(row.chars().count() <= 12);
    }

    assert_eq!(wrap_rows("line one\nline two", 40).len(), 2);
    assert!(
        wrap_rows("", 40).len() == 1,
        "an empty line still occupies a row"
    );
}

#[test]
fn separators_only_appear_between_different_blocks() {
    use OutputBlockKind::*;
    assert!(!needs_separator(None, User));
    assert!(needs_separator(User, Thinking));
    assert!(needs_separator(Thinking, Tool));
    assert!(
        !needs_separator(Tool, Tool),
        "consecutive tool calls stay adjacent"
    );
    assert!(
        needs_separator(Reply, User),
        "a new user message starts after a blank line"
    );
}

#[test]
fn approval_verdict_colors_approved_blue_and_file_cyan() {
    let req = ApprovalRequest {
        tool: "write_file".to_string(),
        call_id: "c1".to_string(),
        target: Some("/home/janelle/fts-verify/src/main.rs".to_string()),
        diff: None,
        arguments: json!({ "path": "/home/janelle/fts-verify/src/main.rs", "content": "fn main() {}\n" }),
    };
    let line = format_approval_verdict(true, &req);
    assert!(line.contains("Approved:"), "got: {}", line);
    assert!(
        line.contains("/home/janelle/fts-verify/src/main.rs"),
        "got: {}",
        line
    );
    assert!(line.contains("write"), "got: {}", line);
}

#[test]
fn approval_verdict_formats_terminal_unbloated() {
    let req = ApprovalRequest {
        tool: "terminal".to_string(),
        call_id: "c2".to_string(),
        target: Some("cargo run --quiet 2>&1 | tail -110".to_string()),
        diff: None,
        arguments: json!({ "command": "cargo run --quiet 2>&1 | tail -110" }),
    };
    let line = format_approval_verdict(true, &req);
    assert!(line.contains("$"), "got: {}", line);
    assert!(line.contains("cargo run --quiet"), "got: {}", line);
    assert!(
        !line.contains("terminal (cargo"),
        "must not be wrapped in bloated tool parentheses"
    );
}

#[test]
fn approval_verdict_formats_note_uuid_shortened() {
    let req = ApprovalRequest {
        tool: "update_note".to_string(),
        call_id: "c3".to_string(),
        target: Some("note_5d26a9f8-b905-43aa-8168-3f286a96ee3f".to_string()),
        diff: None,
        arguments: json!({ "id": "note_5d26a9f8-b905-43aa-8168-3f286a96ee3f" }),
    };
    let line = format_approval_verdict(true, &req);
    assert!(line.contains("note_5d26a9f8…"), "got: {}", line);
    assert!(
        !line.contains("note_5d26a9f8-b905-43aa-8168-3f286a96ee3f"),
        "raw UUID must be shortened"
    );
}

#[test]
fn process_header_formats_collapsed_with_tools_and_duration() {
    let tools = vec!["read_file".to_string(), "patch".to_string()];
    let header = format_process_header(false, 2, &tools, Some(Duration::from_millis(2400)), true);
    assert!(header.contains("Process"), "got: {}", header);
    assert!(
        header.contains("2 tools: read_file, patch"),
        "got: {}",
        header
    );
    assert!(header.contains("thought for 2.4s"), "got: {}", header);
    assert!(header.contains("Ctrl+O to expand"), "got: {}", header);
    assert!(header.contains("▶"), "got: {}", header);
}

#[test]
fn process_header_formats_expanded_header() {
    let tools = vec!["terminal".to_string()];
    let header = format_process_header(true, 1, &tools, Some(Duration::from_millis(500)), false);
    assert!(header.contains("▼"), "got: {}", header);
    assert!(header.contains("1 tool: terminal"), "got: {}", header);
    assert!(header.contains("Ctrl+O to collapse"), "got: {}", header);
}

#[test]
fn a_tool_call_is_stamped_with_its_verdict_once() {
    use harness_core::{Message, ToolResult};

    // The call is stamped when the tool returns, and the transcript is drawn again
    // afterwards. A coloured verdict ends in the reset escape, so comparing the
    // visible text is what keeps the second pass from stamping it twice.
    let history = vec![
        StoredMessage {
            id: 0,
            seq: 0,
            message: Message::user("list the todos"),
            is_summary: false,
        },
        StoredMessage {
            id: 1,
            seq: 1,
            message: Message {
                role: Role::Assistant,
                content: None,
                reasoning: None,
                tool_calls: vec![harness_core::ToolCall {
                    id: "c1".to_string(),
                    name: "todo_list".to_string(),
                    arguments: json!({ "action": "list" }),
                    signature: None,
                }],
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
        StoredMessage {
            id: 2,
            seq: 2,
            message: Message {
                role: Role::Tool,
                content: None,
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: Some(ToolResult::success("c1", "[0] pending a task")),
                usage: None,
            },
            is_summary: false,
        },
    ];

    let turns = parse_turns_from_history(&history);
    let call = plain(&turns[0].tool_items[0].call_formatted);
    assert_eq!(
        call.matches("(success)").count(),
        1,
        "the verdict appears twice: {}",
        call
    );
}

#[test]
fn multiple_tool_calls_in_one_turn_each_get_their_own_verdict_without_duplication() {
    use harness_core::{Message, ToolResult};
    let history = vec![
        StoredMessage {
            id: 0,
            seq: 0,
            message: Message::user("check things"),
            is_summary: false,
        },
        StoredMessage {
            id: 1,
            seq: 1,
            message: Message {
                role: Role::Assistant,
                content: None,
                reasoning: None,
                tool_calls: vec![
                    harness_core::ToolCall {
                        id: "c1".to_string(),
                        name: "read_file".to_string(),
                        arguments: json!({ "path": "src/a.rs" }),
                        signature: None,
                    },
                    harness_core::ToolCall {
                        id: "c2".to_string(),
                        name: "read_file".to_string(),
                        arguments: json!({ "path": "src/b.rs" }),
                        signature: None,
                    },
                    harness_core::ToolCall {
                        id: "c3".to_string(),
                        name: "read_file".to_string(),
                        arguments: json!({ "path": "src/c.rs" }),
                        signature: None,
                    },
                ],
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
        StoredMessage {
            id: 2,
            seq: 2,
            message: Message {
                role: Role::Tool,
                content: None,
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: Some(ToolResult::success("c1", "content a")),
                usage: None,
            },
            is_summary: false,
        },
        StoredMessage {
            id: 3,
            seq: 3,
            message: Message {
                role: Role::Tool,
                content: None,
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: Some(ToolResult::error("c2", "file b not found")),
                usage: None,
            },
            is_summary: false,
        },
        StoredMessage {
            id: 4,
            seq: 4,
            message: Message {
                role: Role::Tool,
                content: None,
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: Some(ToolResult::success("c3", "content c")),
                usage: None,
            },
            is_summary: false,
        },
    ];

    let turns = parse_turns_from_history(&history);
    assert_eq!(turns[0].tool_items.len(), 3);
    for (i, item) in turns[0].tool_items.iter().enumerate() {
        let call = plain(&item.call_formatted);
        assert_eq!(
            call.matches("(success)").count() + call.matches("(failed)").count(),
            1,
            "tool item {i} must have exactly one verdict: {call}"
        );
    }
    assert!(plain(&turns[0].tool_items[0].call_formatted).ends_with("(success)"));
    assert!(plain(&turns[0].tool_items[1].call_formatted).ends_with("(failed)"));
    assert!(plain(&turns[0].tool_items[2].call_formatted).ends_with("(success)"));
}

#[test]
fn a_coloured_verdict_counts_as_a_verdict() {
    assert!(has_verdict(&format!(
        "read src/lib.rs {}",
        " (success)".green()
    )));
    assert!(has_verdict(&format!(
        "read src/lib.rs {}",
        " (failed)".red()
    )));
    assert!(!has_verdict("read src/lib.rs"));
}

/// The todo panel is chrome, not transcript: it is drawn above the composer, so it
/// stays put while the conversation scrolls under it. `render_transcript` takes no
/// todo argument at all, which is what keeps it out of the transcript.
#[test]
fn the_todo_panel_is_a_box_of_its_own() {
    let snapshot = harness_cli_todo_snapshot();
    let panel = crate::commands::chat::todo::render_panel(&snapshot, false, 80);
    assert_eq!(panel.len(), 3, "a header, one task, a footer: {:?}", panel);
    let first = dialoguer::console::strip_ansi_codes(&panel[0]);
    assert!(first.starts_with('╭'), "got: {}", first);
    assert!(first.contains("Todo"), "got: {}", first);
}

fn harness_cli_todo_snapshot() -> harness_core::tools::todo::TodoSnapshot {
    harness_core::tools::todo::TodoSnapshot {
        items: vec![harness_core::tools::todo::TodoItem {
            id: 0,
            text: "write the parser".to_string(),
            status: harness_core::tools::todo::TodoStatus::Pending,
        }],
    }
}

#[test]
fn parse_turns_from_history_handles_multistep_and_empty_queries() {
    use harness_core::Message;

    let history = vec![
        StoredMessage {
            id: 1,
            seq: 1,
            message: Message::user("first question"),
            is_summary: false,
        },
        StoredMessage {
            id: 2,
            seq: 2,
            message: Message::assistant("first reply"),
            is_summary: false,
        },
        StoredMessage {
            id: 3,
            seq: 3,
            message: Message::user("second question"),
            is_summary: false,
        },
        StoredMessage {
            id: 4,
            seq: 4,
            message: Message {
                role: Role::Assistant,
                content: None,
                reasoning: Some("thinking...".to_string()),
                tool_calls: vec![harness_core::ToolCall {
                    id: "c1".to_string(),
                    name: "terminal".to_string(),
                    arguments: json!({ "command": "ls" }),
                    signature: None,
                }],
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
        StoredMessage {
            id: 5,
            seq: 5,
            message: Message {
                role: Role::Assistant,
                content: Some("final reply".to_string()),
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
    ];

    let turns = parse_turns_from_history(&history);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].user_query, "first question");
    assert_eq!(turns[0].reply, "first reply");
    assert_eq!(turns[1].user_query, "second question");
    assert_eq!(turns[1].reply, "final reply");
    assert_eq!(turns[1].tool_items.len(), 1);
    assert_eq!(turns[1].reasoning.as_deref(), Some("thinking..."));
}

/// A replay that opens inside a turn — the window landed past the user message that anchors it
/// — still holds Luna's words. Dropping them would leave the resumed session showing nothing.
#[test]
fn parse_turns_from_history_keeps_a_turn_that_has_no_user_line() {
    use harness_core::Message;

    let history = vec![StoredMessage {
        id: 1,
        seq: 1,
        message: Message::assistant("the answer"),
        is_summary: false,
    }];

    let turns = parse_turns_from_history(&history);
    assert_eq!(turns.len(), 1, "the reply is not thrown away");
    assert_eq!(turns[0].user_query, "", "there was no user line to show");
    assert_eq!(turns[0].reply, "the answer");
}

#[test]
fn parse_turns_from_history_preserves_steered_messages_without_turn_fragmentation() {
    use harness_core::{Message, Role, ToolCall};
    use serde_json::json;

    let steer_envelope = "[OUT-OF-BAND USER MESSAGE — a direct message from the user, delivered once at this position; not tool output and not a new delivery when replayed from conversation history]\nswitch to alternate plan\n[/OUT-OF-BAND USER MESSAGE]";

    let history = vec![
        StoredMessage {
            id: 1,
            seq: 1,
            message: Message::user("solve the bug"),
            is_summary: false,
        },
        StoredMessage {
            id: 2,
            seq: 2,
            message: Message {
                role: Role::Assistant,
                content: None,
                reasoning: Some("analyzing code".to_string()),
                tool_calls: vec![ToolCall {
                    id: "c1".to_string(),
                    name: "read_file".to_string(),
                    arguments: json!({ "path": "src/main.rs" }),
                    signature: None,
                }],
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
        StoredMessage {
            id: 3,
            seq: 3,
            message: Message::tool(harness_core::ToolResult::success("c1", "fn main() {}")),
            is_summary: false,
        },
        StoredMessage {
            id: 4,
            seq: 4,
            message: Message::user(steer_envelope),
            is_summary: false,
        },
        StoredMessage {
            id: 5,
            seq: 5,
            message: Message::assistant("switched plan and resolved"),
            is_summary: false,
        },
    ];

    let turns = parse_turns_from_history(&history);
    assert_eq!(
        turns.len(),
        1,
        "steered message must not fragment turn into phantom turns"
    );
    assert_eq!(turns[0].user_query, "solve the bug");
    assert_eq!(turns[0].reply, "switched plan and resolved");
    assert_eq!(
        turns[0].tool_items.len(),
        2,
        "must include tool call and steer item"
    );
    assert!(
        plain(&turns[0].tool_items[0].call_formatted).contains("src/main.rs"),
        "first item should be tool call: {}",
        turns[0].tool_items[0].call_formatted
    );
    assert!(
        plain(&turns[0].tool_items[1].call_formatted)
            .contains("Steered into active turn: switch to alternate plan"),
        "second item should be steer process item: {}",
        turns[0].tool_items[1].call_formatted
    );
}

#[test]
fn steer_process_items_are_not_counted_as_tools_in_header() {
    use harness_core::Message;

    let steer_envelope = "[OUT-OF-BAND USER MESSAGE]\nredirect focus\n[/OUT-OF-BAND USER MESSAGE]";
    let history = vec![
        StoredMessage {
            id: 1,
            seq: 1,
            message: Message::user("solve it"),
            is_summary: false,
        },
        StoredMessage {
            id: 2,
            seq: 2,
            message: Message::user(steer_envelope),
            is_summary: false,
        },
        StoredMessage {
            id: 3,
            seq: 3,
            message: Message::assistant("done"),
            is_summary: false,
        },
    ];

    let turns = parse_turns_from_history(&history);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].tools.len(), 0);
    assert_eq!(turns[0].tool_items.len(), 1);
    assert!(plain(&turns[0].tool_items[0].call_formatted)
        .contains("Steered into active turn: redirect focus"));
}

/// `colored` decides on escapes by whether stdout is a tty, which it never is under
/// `cargo test`, so colour has to be forced. The override is process-global and these tests
/// run in parallel — hence unset in a guard, and hence the geometry tests below reading
/// through `plain` so a concurrent override cannot perturb them.
struct ColorOverride;
impl ColorOverride {
    fn force_on() -> Self {
        colored::control::set_override(true);
        Self
    }
}
impl Drop for ColorOverride {
    fn drop(&mut self) {
        colored::control::unset_override();
    }
}

/// Every row of a user message — wrapped rows included — carries the blue foreground, so a
/// long message reads as one block on the user's side rather than alternating with plain text.
#[test]
fn every_row_of_a_user_message_is_blue() {
    let _color = ColorOverride::force_on();

    // `colored` merges the attributes it is given, so blue+bold arrives as one sequence
    // rather than two separate ones.
    const BLUE_FG: &str = "\u{1b}[1;34m";
    let msg = right_aligned_user_message("alpha beta gamma delta epsilon zeta eta theta", 60);
    let rows: Vec<&str> = msg.lines().collect();
    assert!(rows.len() > 1, "expected a wrap at width 60");
    for row in &rows {
        assert!(
            row.contains(BLUE_FG),
            "row is missing the blue foreground: {row:?}"
        );
    }
}

#[test]
fn the_empty_input_prompt_is_blue_too() {
    let _color = ColorOverride::force_on();

    let msg = right_aligned_user_message("   ", 80);
    assert!(msg.contains("\u{1b}[1;34m"), "got: {msg:?}");
    assert!(msg.ends_with("\u{1b}[0m"), "style is closed: {msg:?}");
}

/// `colored::control::set_override` is process-global and these tests run in parallel, so the
/// blue tests can flip it on while a width test is mid-assertion. Anything that measures
/// geometry must therefore strip escapes first — the same way `ui::visible_width` does in the
/// live region, which sees both colored and plain rows.
// The dimension of the live region is computed from visible characters, so a colour escape
// must not be counted. `strip_ansi_codes` is what the live activity row already relies on;
// reusing it keeps the test and the code looking at the same definition of "visible".
fn plain(s: &str) -> String {
    dialoguer::console::strip_ansi_codes(s).to_string()
}

#[test]
fn plain_strips_colored_escapes() {
    assert_eq!(plain("\u{1b}[1;34mfoo\u{1b}[0m"), "foo");
}

#[test]
fn right_aligned_user_message_handles_multiline() {
    let msg = right_aligned_user_message("hello\nworld", 80);
    let lines: Vec<&str> = msg.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(plain(lines[0]).ends_with("hello"));
    assert!(plain(lines[1]).ends_with(PROMPT.trim_end()));
}

/// Regression: a message wider than the terminal padded to zero, so it started at column 0 and
/// the terminal soft-wrapped its tail back to column 0 — the continuation then sat in the
/// same column as Luna's replies and read as assistant text.
#[test]
fn a_long_message_wraps_on_the_user_side_and_never_at_the_left_margin() {
    let width = 103; // the narrow terminal this was reported on
    let long = "oohhhhh also, i have an idea, in admin abuse, players will also get encantations \
                and mutations on a plant!";
    let msg = right_aligned_user_message(long, width);
    let rows: Vec<&str> = msg.lines().collect();

    // Printed under `--nocapture` so the layout can be eyeballed without launching the TUI.
    println!("--- user message at width {width} ---\n{msg}\n---");

    assert!(rows.len() > 1, "a message this long must wrap: {msg}");
    for row in &rows {
        let row = plain(row);
        let leading = row.chars().take_while(|c| *c == ' ').count();
        let visible = row.chars().count();
        assert!(leading > 0, "no row may start at the left margin: {row:?}");
        assert!(
            visible <= width,
            "row overflows the terminal: {visible} > {width}"
        );
    }
    assert!(
        plain(rows.last().unwrap()).ends_with(PROMPT.trim_end()),
        "the prompt glyph closes the last row: {msg}"
    );
}

/// The wrapped rows belong to one right-anchored block: every row ends at the same column.
#[test]
fn wrapped_rows_of_a_message_share_the_right_edge() {
    let width = 100;
    let msg = right_aligned_user_message(&"word ".repeat(60), width);
    let rows: Vec<&str> = msg.lines().collect();
    assert!(rows.len() > 2, "expected several rows, got {}", rows.len());
    for row in &rows {
        assert_eq!(
            plain(row).chars().count(),
            width - 1,
            "each row is padded to the right edge: {row:?}"
        );
    }
}

#[test]
fn a_short_message_stays_one_right_aligned_row() {
    let msg = right_aligned_user_message("hi", 80);
    let rows: Vec<&str> = msg.lines().collect();
    assert_eq!(rows.len(), 1, "short messages must not be wrapped");
    assert_eq!(plain(rows[0]).chars().count(), 79);
    assert!(plain(rows[0]).starts_with(' '), "still pushed to the right");
}

#[test]
fn patch_diff_formatting_colors_additions_and_deletions() {
    let old = "fn hello() {\n    println!(\"old\");\n}\n";
    let new = "fn hello() {\n    println!(\"new\");\n}\n";
    let diff = format_patch_diff(old, new);

    assert_eq!(diff.len(), 4);
    // Equal line
    assert!(diff[0].contains("fn hello()"));
    // Deleted line should have '-' and pure red background (200;0;0) with white text (255;255;255)
    assert!(diff[1].contains("-"));
    assert!(diff[1].contains("println!(\"old\")"));
    assert!(
        diff[1].contains("200;0;0"),
        "expected pure red background in deletion: {:?}",
        diff[1]
    );
    assert!(
        diff[1].contains("255;255;255"),
        "expected white text in deletion: {:?}",
        diff[1]
    );
    // Added line should have '+' and pure green background (0;160;0) with white text (255;255;255)
    assert!(diff[2].contains("+"));
    assert!(diff[2].contains("println!(\"new\")"));
    assert!(
        diff[2].contains("0;160;0"),
        "expected pure green background in addition: {:?}",
        diff[2]
    );
    assert!(
        diff[2].contains("255;255;255"),
        "expected white text in addition: {:?}",
        diff[2]
    );
    // Equal line
    assert!(diff[3].contains("}"));
}

#[test]
fn patch_diff_included_in_history_parsing() {
    let history = vec![
        StoredMessage {
            id: 1,
            seq: 1,
            message: harness_core::Message {
                role: Role::User,
                content: Some("fix it".to_string()),
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
        StoredMessage {
            id: 2,
            seq: 2,
            message: harness_core::Message {
                role: Role::Assistant,
                content: None,
                reasoning: None,
                tool_calls: vec![harness_core::ToolCall {
                    id: "call_1".to_string(),
                    name: "patch".to_string(),
                    arguments: serde_json::json!({
                        "path": "src/main.rs",
                        "old_string": "let a = 1;",
                        "new_string": "let a = 2;"
                    }),
                    signature: None,
                }],
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
    ];

    let turns = parse_turns_from_history(&history);
    assert_eq!(turns.len(), 1);
    let turn = &turns[0];
    assert_eq!(turn.tool_items.len(), 1);
    let tool = &turn.tool_items[0];
    assert_eq!(tool.diff_lines.len(), 2);
    assert!(tool.diff_lines[0].contains("-"));
    assert!(tool.diff_lines[0].contains("200;0;0"));
    assert!(tool.diff_lines[1].contains("+"));
    assert!(tool.diff_lines[1].contains("0;160;0"));
}

#[tokio::test]
async fn resumed_compacted_session_replays_all_prompts_and_replies() {
    use harness_core::{Message, SessionStore, SqliteSessionStore};

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("sessions.db");
    let store = SqliteSessionStore::new(Some(db_path)).unwrap();
    store.ensure_session("s_test").await.unwrap();

    // Turn 1
    store
        .append("s_test", &Message::user("first question"), false)
        .await
        .unwrap();
    store
        .append("s_test", &Message::assistant("first answer"), false)
        .await
        .unwrap();

    // Turn 2
    store
        .append("s_test", &Message::user("second question"), false)
        .await
        .unwrap();
    store
        .append("s_test", &Message::assistant("second answer"), false)
        .await
        .unwrap();

    // Compaction folds Turn 1 into a summary
    store
        .record_compaction("s_test", &[1, 2], "User asked first question")
        .await
        .unwrap();

    // Loading via replay_transcript (used by the CLI UI) preserves both turns with agent replies
    let replayed = store.replay_transcript("s_test", 200).await.unwrap();
    let turns = parse_turns_from_history(&replayed.history);

    assert_eq!(turns.len(), 2, "both turns are shown on resume");
    assert_eq!(turns[0].user_query, "first question");
    assert_eq!(turns[0].reply, "first answer");
    assert_eq!(turns[1].user_query, "second question");
    assert_eq!(turns[1].reply, "second answer");
}

#[test]
fn clean_user_query_for_transcript_strips_plan_mode_and_data_urls() {
    use super::clean_user_query_for_transcript;

    // Normal text unchanged
    assert_eq!(
        clean_user_query_for_transcript("just a regular question"),
        "just a regular question"
    );

    // Multiline normal text
    assert_eq!(
        clean_user_query_for_transcript("line 1\nline 2"),
        "line 1\nline 2"
    );

    // Plan mode banner stripped
    let plan_prompt = "[PLAN MODE ACTIVE: Formulate a detailed, step-by-step plan/blueprint. Do not attempt to modify files or execute mutating commands until the plan is approved.]\n\nactual question here";
    assert_eq!(
        clean_user_query_for_transcript(plan_prompt),
        "actual question here"
    );

    // Attached image with marker drops whole-line base64 data URL
    let attached_image = "[image: 948x1058] look at this UI bug\n\ndata:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAcAAAAPoCAYAAAC";
    assert_eq!(
        clean_user_query_for_transcript(attached_image),
        "[image: 948x1058] look at this UI bug"
    );

    // Attached image with saved-to path in marker
    let attached_saved = "[image: 948x1058 saved to /tmp/screenshot_1.png] look at this\n\ndata:image/png;base64,AAAA";
    assert_eq!(
        clean_user_query_for_transcript(attached_saved),
        "[image: 948x1058] look at this"
    );

    // Plan mode + attached image
    let plan_image = "[PLAN MODE ACTIVE: Formulate a blueprint.]\n\n[image: 800x600] what is broken?\n\ndata:image/jpeg;base64,BBBB";
    assert_eq!(
        clean_user_query_for_transcript(plan_image),
        "[image: 800x600] what is broken?"
    );

    // Mid-line inlined data URL without image marker
    let inline_url = "inspect data:image/png;base64,CCCC and report";
    assert_eq!(
        clean_user_query_for_transcript(inline_url),
        "inspect [image] and report"
    );

    // Standalone data URL without marker becomes [image]
    let bare_url = "data:image/png;base64,DDDD";
    assert_eq!(clean_user_query_for_transcript(bare_url), "[image]");
}

#[test]
fn parse_turns_from_history_cleans_historical_image_data_urls() {
    use harness_core::{Message, Role};

    let history = vec![
        StoredMessage {
            id: 1,
            seq: 1,
            message: Message {
                role: Role::User,
                content: Some(
                    "[image: 948x1058] investigate this issue\n\ndata:image/png;base64,hxbMY87NCWUEZa3j5+PtQ1H7VD+7J/pgu/00V6/PWdodoAtS4k+/4b5Bydg/lyuE9s+x\n6e7d8dno/ER7UFF4sQc9bkf9AdCPRL+N8BxVFY0sAv6IoAAAAASUVORK5CYII="
                        .to_string(),
                ),
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
        StoredMessage {
            id: 2,
            seq: 2,
            message: Message {
                role: Role::Assistant,
                content: Some("I fixed the issue.".to_string()),
                reasoning: None,
                tool_calls: Vec::new(),
                tool_result: None,
                usage: None,
            },
            is_summary: false,
        },
    ];

    let turns = parse_turns_from_history(&history);
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].user_query,
        "[image: 948x1058] investigate this issue"
    );
    assert_eq!(turns[0].reply, "I fixed the issue.");

    let rendered = right_aligned_user_message(&turns[0].user_query, 80);
    assert!(
        !rendered.contains("base64"),
        "rendered user turn must not contain base64 text"
    );
    assert!(
        !rendered.contains("hxbMY87NCWUE"),
        "rendered user turn must not contain base64 bytes"
    );
    assert!(
        rendered.contains("[image: 948x1058]"),
        "rendered user turn retains the image marker"
    );
}
