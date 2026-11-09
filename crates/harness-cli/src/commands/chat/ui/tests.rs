#![cfg(unix)]

use super::test_support::capture_stdout;
use super::*;
use harness_providers::QuotaWindow;

fn a_snapshot() -> TodoSnapshot {
    TodoSnapshot {
        items: vec![harness_core::tools::todo::TodoItem {
            id: 0,
            text: "a task".to_string(),
            status: harness_core::tools::todo::TodoStatus::Pending,
        }],
    }
}

#[test]
fn a_running_command_keeps_only_its_newest_lines() {
    let ui = ChatUi::new(true);
    ui.start_activity("$ cargo build", "terminal", &serde_json::json!({}));
    for i in 0..20 {
        ui.push_tool_output(&format!("line {i}\n"));
    }

    let live = ui.live.lock().unwrap();
    let rows = live.tool_output.rows(80, TOOL_TAIL_LINES);
    assert_eq!(rows.len(), TOOL_TAIL_LINES, "the tail is bounded: {rows:?}");
    assert!(rows.last().unwrap().contains("line 19"));
    assert!(
        !rows.iter().any(|row| row.contains("line 11")),
        "the oldest lines fall off the tail: {rows:?}"
    );
    assert!(
        rows[0].contains("earlier line"),
        "the tail says how much it is hiding: {rows:?}"
    );
}

#[test]
fn the_tool_tail_drops_ansi_and_keeps_the_newest_progress_update() {
    let ui = ChatUi::new(true);
    ui.start_activity("$ make", "terminal", &serde_json::json!({}));
    ui.push_tool_output("\x1b[31mbuilding\x1b[0m\n");
    ui.push_tool_output("10%\r50%\r");

    let live = ui.live.lock().unwrap();
    let plain: Vec<String> = live
        .tool_output
        .rows(80, TOOL_TAIL_LINES)
        .iter()
        .map(|row| dialoguer::console::strip_ansi_codes(row).to_string())
        .collect();
    assert!(plain.iter().any(|row| row.contains("building")));
    assert!(
        plain.iter().any(|row| row.contains("50%")),
        "a `\\r` redraw leaves a line the tail can show: {plain:?}"
    );
    assert!(
        !plain.iter().any(|row| row.contains("10%")),
        "a `\\r` redraw overwrites the previous update: {plain:?}"
    );
    assert!(
        !plain
            .iter()
            .any(|row| row.contains("[31m") || row.contains("[0m")),
        "escape sequences are not text: {plain:?}"
    );
}

#[test]
fn expanding_the_tail_reveals_the_buffered_lines_until_the_call_ends() {
    let ui = ChatUi::new(true);
    ui.start_activity("$ cargo build", "terminal", &serde_json::json!({}));
    for i in 0..20 {
        ui.push_tool_output(&format!("line {i}\n"));
    }

    assert!(ui.toggle_live_expanded());
    {
        let live = ui.live.lock().unwrap();
        let rows = live.tool_output.rows(80, tool_tail_limit(&live));
        assert_eq!(
            rows.len(),
            20,
            "expanded tail shows the whole buffer: {rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("line 0")));
    }

    ui.stop_activity();
    assert!(
        !ui.live.lock().unwrap().tool_output_expanded,
        "the next call starts collapsed"
    );
}

#[test]
fn a_new_tool_call_starts_with_an_empty_tail() {
    let ui = ChatUi::new(true);
    ui.start_activity("$ one", "terminal", &serde_json::json!({}));
    ui.push_tool_output("old output\n");

    ui.start_activity("$ two", "terminal", &serde_json::json!({}));
    assert!(
        ui.live
            .lock()
            .unwrap()
            .tool_output
            .rows(80, TOOL_TAIL_LINES)
            .is_empty(),
        "the previous call's output does not leak into the next one"
    );
}

#[test]
fn a_command_capture_keeps_what_the_command_printed() {
    let ui = ChatUi::new(false);
    ui.begin_command_capture();
    ui.emit_above("first line");
    ui.emit_above("second line");

    assert_eq!(
        ui.end_command_capture(),
        Some(vec!["first line".to_string(), "second line".to_string()])
    );
    assert_eq!(
        ui.end_command_capture(),
        None,
        "a finished capture is taken, not read twice"
    );
}

#[test]
fn the_todo_panel_is_drawn_above_the_composer() {
    let ui = ChatUi::new(true);
    ui.set_todo_panel(Some(a_snapshot()));

    let rows = ui.composer_rows(80, "hello");
    let panel_at = rows
        .iter()
        .position(|r| r.contains("Todo"))
        .expect("the panel is drawn");
    let prompt_at = rows
        .iter()
        .position(|r| r.contains("hello"))
        .expect("the composer is drawn");
    assert!(
        panel_at < prompt_at,
        "the panel belongs above the composer: {:?}",
        rows
    );
    // The panel is the first thing in the composer: nothing is drawn over the
    // top of the terminal, so the list stays in front of the user.
    assert_eq!(panel_at, 0, "the panel is at the top: {:?}", rows);
}

#[test]
fn the_panel_rows_are_counted_with_the_box() {
    let ui = ChatUi::new(true);
    ui.set_todo_panel(Some(a_snapshot()));
    // A header, one task and a footer.
    assert_eq!(ui.box_row_count(), 4 + 3);

    let boxed = ui.clone();
    capture_stdout(move || boxed.draw_box(80, ""));
    let metrics = ui.drawn_metrics.lock().unwrap();
    let drawn = metrics.as_ref().expect("the box was drawn");
    assert_eq!(drawn.panel_rows_lens.len(), 3);
}

#[test]
fn the_landing_keeps_showering_after_the_composer_is_drawn() {
    let ui = ChatUi::new(true);
    let drawn = ui.clone();
    capture_stdout(move || {
        drawn.draw_landing();
        drawn.draw_initial_box();
    });

    let frame = ui
        .landing
        .lock()
        .unwrap()
        .as_ref()
        .expect("the mark starts its shower")
        .frame;

    let ticked = ui.clone();
    capture_stdout(move || ticked.animate_landing());

    assert_eq!(
        ui.landing
            .lock()
            .unwrap()
            .as_ref()
            .map(|landing| landing.frame),
        Some(frame + 1),
        "the composer appearing must not settle the shower"
    );
}

#[test]
fn a_growing_composer_does_not_settle_the_landing() {
    let ui = ChatUi::new(true);
    let drawn = ui.clone();
    capture_stdout(move || {
        drawn.draw_landing();
        drawn.draw_initial_box();
    });

    // A queued message adds a composer row. The repaint can no longer reuse the old row
    // positions, so it redraws the box — but the mark itself is still on screen and must keep
    // animating instead of freezing at the frame it happened to be on.
    ui.set_queue_depth(1);
    let ticked = ui.clone();
    capture_stdout(move || ticked.animate_landing());

    assert!(
        ui.landing.lock().unwrap().is_some(),
        "a taller composer settles the shower"
    );
}

#[test]
fn a_fresh_list_replaces_the_old_one() {
    let ui = ChatUi::new(true);
    ui.set_todo_panel(Some(a_snapshot()));
    assert_eq!(ui.todo_panel_rows(80).len(), 3);

    let mut second = a_snapshot();
    second.items.push(harness_core::tools::todo::TodoItem {
        id: 1,
        text: "another task".to_string(),
        status: harness_core::tools::todo::TodoStatus::Done,
    });
    ui.set_todo_panel(Some(second));
    assert_eq!(ui.todo_panel_rows(80).len(), 4);

    // An empty list has no panel at all.
    ui.set_todo_panel(None);
    assert!(ui.todo_panel_rows(80).is_empty());
    assert_eq!(ui.box_row_count(), 4);
}

#[test]
fn the_panel_folds_and_unfolds() {
    let ui = ChatUi::new(true);
    assert!(!ui.todo_collapsed(), "the panel starts open");
    assert!(
        ui.toggle_todo_collapsed(),
        "folding says the panel is folded"
    );
    assert!(ui.todo_collapsed());
    assert!(!ui.toggle_todo_collapsed(), "unfolding says it is open");
    assert!(!ui.todo_collapsed());
}

#[test]
fn queued_messages_get_a_line_and_lose_it_when_the_queue_drains() {
    let ui = ChatUi::new(true);
    assert!(
        ui.format_queue_line(80).is_none(),
        "nothing queued, no line"
    );

    ui.set_queue_depth(2);
    assert_eq!(ui.queue_depth(), 2);
    let line = ui
        .format_queue_line(80)
        .expect("queued messages are announced");
    let plain = dialoguer::console::strip_ansi_codes(&line).to_string();
    assert!(plain.contains('2'), "the count is shown: {plain}");
    assert!(plain.contains("queued"), "the state is named: {plain}");

    // Drained: the line goes away again, and the row count with it.
    let with_line = ui.box_row_count();
    ui.set_queue_depth(0);
    assert!(ui.format_queue_line(80).is_none());
    assert_eq!(ui.box_row_count(), with_line - 1);
}

#[test]
fn delegated_children_stay_visible_and_report_progress() {
    let ui = ChatUi::new(true);
    ui.start_delegate_task("sub_20261029_175052_1", "Audit the delegate timeout");

    let rows = ui.delegate_task_rows(80);
    assert_eq!(rows.len(), 1, "the running child is visible");
    let plain = dialoguer::console::strip_ansi_codes(&rows[0]).to_string();
    assert!(plain.contains("sub_20261029_175052_1"), "{plain}");
    assert!(plain.contains("Audit the delegate timeout"), "{plain}");
    assert!(plain.contains("starting"), "{plain}");
    assert!(visible_width(&rows[0]) <= 80, "{plain}");

    ui.update_delegate_task("sub_20261029_175052_1", "read_file running");
    let updated = dialoguer::console::strip_ansi_codes(&ui.delegate_task_rows(80)[0]).to_string();
    assert!(updated.contains("read_file running"), "{updated}");
    assert_eq!(ui.box_row_count(), 5, "the child adds one composer row");

    let composer = ui.composer_rows(80, "");
    assert!(
        composer
            .iter()
            .any(|row| row.contains("sub_20261029_175052_1")),
        "the child is drawn above the prompt: {composer:?}"
    );

    let boxed = ui.clone();
    capture_stdout(move || boxed.draw_box(80, ""));
    let metrics = ui.drawn_metrics.lock().unwrap();
    let drawn = metrics.as_ref().expect("the box was drawn");
    assert_eq!(drawn.delegate_rows_lens.len(), 1);
    drop(metrics);

    ui.clear_live_state();
    assert_eq!(
        ui.delegate_task_rows(80).len(),
        1,
        "a background child survives the end of the main turn"
    );

    ui.finish_delegate_task("sub_20261029_175052_1");
    assert!(
        ui.delegate_task_rows(80).is_empty(),
        "the row leaves on finish"
    );
    assert_eq!(ui.box_row_count(), 4);

    // Multiple subagents still render exactly one row with (+N more)
    ui.start_delegate_task("sub_1", "First task");
    ui.start_delegate_task("sub_2", "Second task");
    let multi_rows = ui.delegate_task_rows(80);
    assert_eq!(multi_rows.len(), 1, "only one delegate row is shown in cli");
    let multi_plain = dialoguer::console::strip_ansi_codes(&multi_rows[0]).to_string();
    assert!(multi_plain.contains("(+1 more)"), "{multi_plain}");
    assert!(multi_plain.contains("sub_2"), "{multi_plain}");
}

/// A queued line sits in the composer like the background-task line it imitates, and the erase
/// metrics record it: a row the erase does not know about is left behind on the next redraw.
#[test]
fn the_queued_line_is_part_of_the_drawn_box() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 0, 1_000);
    ui.set_queue_depth(1);

    let rows = ui.composer_rows(80, "");
    let queue_at = rows
        .iter()
        .position(|r| r.contains("queued"))
        .expect("the queue line is drawn");
    let status_at = rows
        .iter()
        .position(|r| r.contains("ctx"))
        .expect("the status line is drawn");
    assert!(
        queue_at < status_at,
        "the queue line sits above the status line: {:?}",
        rows
    );

    let boxed = ui.clone();
    capture_stdout(move || boxed.draw_box(80, ""));
    let metrics = ui.drawn_metrics.lock().unwrap();
    let drawn = metrics.as_ref().expect("the box was drawn");
    assert!(drawn.queued_line_len > 0, "the drawn row is measured");
}

#[test]
fn compaction_animates_between_start_and_finish() {
    let ui = ChatUi::new(true);
    assert!(!ui.is_compacting(), "no fold is running");

    ui.start_compaction(false);
    assert!(ui.is_compacting(), "the fold is marked as running");

    ui.start_compaction(true);
    assert!(
        ui.is_compacting(),
        "a second start replaces, not duplicates"
    );

    ui.finish_compaction();
    assert!(!ui.is_compacting(), "the fold is over");

    // Finishing with nothing running is a no-op, not a panic.
    ui.finish_compaction();
    assert!(!ui.is_compacting());
}

/// Replays the shape of a turn: the composer is drawn, the agent answers, the
/// agent opens a task list, and the answer comes again.
///
/// Ignored by default: it is a scenario to render in a terminal, not an
/// assertion. Run it with `--ignored --nocapture` and read the screen.
#[test]
#[ignore]
fn the_panel_stays_pinned_while_the_conversation_scrolls() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 1200, 128_000);
    ui.draw_initial_box();
    ui.emit_above("Luna: working on it");
    ui.set_todo_panel(Some(TodoSnapshot {
        items: vec![
            harness_core::tools::todo::TodoItem {
                id: 0,
                text: "read the panel code".to_string(),
                status: harness_core::tools::todo::TodoStatus::Done,
            },
            harness_core::tools::todo::TodoItem {
                id: 1,
                text: "pin it above the composer".to_string(),
                status: harness_core::tools::todo::TodoStatus::Pending,
            },
        ],
    }));
    ui.emit_above("Luna: the panel is pinned above the composer");
    std::thread::sleep(std::time::Duration::from_millis(50));
}

/// Prints a composer and one in-place repaint, so the escape sequence can be replayed in a
/// terminal and read back. Ignored by default: it is a scenario to watch, not an assertion.
#[test]
#[ignore]
fn demo_the_in_place_repaint() {
    let ui = ChatUi::new(true);
    ui.set_status("minimaxai/minimax-m3.1-flash", 194_000, 246_000);
    let width = get_terminal_width();
    let status = ui.format_status_line(width).expect("status line");
    let divider = "─".repeat(width).dimmed().to_string();
    let prompt = ui.prompt_row_drawn("").0;

    {
        let mut metrics = ui.drawn_metrics.lock().unwrap();
        *metrics = Some(BoxDrawnMetrics {
            width,
            queued_line_len: 0,
            status_line_len: visible_width(&status),
            divider_len: width,
            prompt_len: visible_width(&prompt),
            live_rows_lens: vec![visible_width("⠋ reading")],
            panel_rows_lens: Vec::new(),
            delegate_rows_lens: Vec::new(),
            memory_notice_rows_lens: Vec::new(),
            block_rows: vec!["⠋ reading".to_string(), status.clone(), divider.clone()],
            prompt_drawn: prompt.clone(),
        });
    }
    ui.box_drawn.store(true, Ordering::SeqCst);
    {
        let mut live = ui.live.lock().unwrap();
        live.rows = vec!["⠋ reading".to_string()];
    }

    println!("\x01DEMO_START");
    // The screen as it stands: transcript, live row, composer, prompt.
    println!("a transcript line above");
    println!("⠋ reading");
    println!("{}", status);
    println!("{}", divider);
    println!();
    println!("{}", divider);
    print!("\x1b[2A\r{}", prompt);

    {
        let mut live = ui.live.lock().unwrap();
        live.rows = vec!["⠙ reading".to_string()];
    }
    let sequence = ui
        .repaint_block_sequence(width, "")
        .expect("the same height repaints in place");
    print!("{sequence}");
    println!("\x01DEMO_END");
    let _ = io::stdout().flush();
    std::thread::sleep(std::time::Duration::from_secs(2));
}

#[test]
fn the_live_region_refreshes_in_place_instead_of_blanking_the_screen() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 68_000, 272_000);
    let width = get_terminal_width();
    let status = ui.format_status_line(width).expect("status line");
    let divider = "─".repeat(width).dimmed().to_string();

    // Stand in for a drawn composer: the repaint reads the measurements, not the screen.
    {
        let mut metrics = ui.drawn_metrics.lock().unwrap();
        *metrics = Some(BoxDrawnMetrics {
            width,
            queued_line_len: 0,
            status_line_len: visible_width(&status),
            divider_len: width,
            prompt_len: visible_width(&ui.prompt_row_drawn("").0),
            live_rows_lens: Vec::new(),
            panel_rows_lens: Vec::new(),
            delegate_rows_lens: Vec::new(),
            memory_notice_rows_lens: Vec::new(),
            block_rows: vec![status.clone(), divider.clone()],
            prompt_drawn: ui.prompt_row_drawn("").0,
        });
    }
    ui.box_drawn.store(true, Ordering::SeqCst);

    // A live row makes the block taller: the composer has to move, so there is no in-place
    // repaint and the caller falls back to the full redraw.
    {
        let mut live = ui.live.lock().unwrap();
        live.rows = vec!["⠋ reading".to_string()];
    }
    assert!(
        ui.repaint_block_sequence(width, "").is_none(),
        "a height change cannot be repainted in place"
    );

    // Same height, new frame: only the row that changed is rewritten.
    {
        let mut metrics = ui.drawn_metrics.lock().unwrap();
        if let Some(m) = metrics.as_mut() {
            m.block_rows = vec!["⠋ reading".to_string(), status.clone(), divider.clone()];
            m.live_rows_lens = vec![visible_width("⠋ reading")];
        }
    }
    {
        let mut live = ui.live.lock().unwrap();
        live.rows = vec!["⠙ reading".to_string()];
    }
    let sequence = ui
        .repaint_block_sequence(width, "")
        .expect("the same height repaints in place");
    assert!(
        !sequence.contains("\x1b[J"),
        "a same-height refresh must not blank the screen:\n{sequence}"
    );
    assert!(sequence.contains("⠙ reading"), "the new frame is written");
    assert!(
        !sequence.contains("test-model"),
        "the unchanged status line is left alone, not cleared and redrawn:\n{sequence}"
    );
    assert!(
        sequence.contains("\x1b[2K"),
        "the changed row is cleared before it is rewritten"
    );
}

#[test]
fn multiple_changed_rows_keep_cursor_in_place_and_walk_down_cleanly() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 10_000, 100_000);
    let width = get_terminal_width();
    let status = ui.format_status_line(width).expect("status line");
    let divider = "─".repeat(width).dimmed().to_string();

    {
        let mut metrics = ui.drawn_metrics.lock().unwrap();
        *metrics = Some(BoxDrawnMetrics {
            width,
            queued_line_len: 0,
            status_line_len: visible_width(&status),
            divider_len: width,
            prompt_len: visible_width(&ui.prompt_row_drawn("").0),
            live_rows_lens: vec![
                visible_width("thinking row 1"),
                visible_width("thinking row 2"),
            ],
            panel_rows_lens: Vec::new(),
            delegate_rows_lens: Vec::new(),
            memory_notice_rows_lens: Vec::new(),
            block_rows: vec![
                "thinking row 1".to_string(),
                "thinking row 2".to_string(),
                status.clone(),
                divider.clone(),
            ],
            prompt_drawn: ui.prompt_row_drawn("").0,
        });
    }
    ui.box_drawn.store(true, Ordering::SeqCst);

    // Both rows change at the same time:
    {
        let mut live = ui.live.lock().unwrap();
        live.rows = vec![
            "updated thinking 1".to_string(),
            "updated thinking 2".to_string(),
        ];
    }
    let sequence = ui
        .repaint_block_sequence(width, "")
        .expect("repaints in place");

    // The sequence should move up to the first row (offset 4), then move DOWN by 1 to the second row (offset 3),
    // and finally move DOWN by 3 back to the prompt line.
    assert!(
        sequence.contains("\x1b[4A"),
        "moves up to top row: {sequence}"
    );
    assert!(
        sequence.contains("\x1b[1B"),
        "steps down to next row: {sequence}"
    );
    assert!(
        sequence.contains("\x1b[3B"),
        "returns down to prompt row: {sequence}"
    );
    // It must NOT compound moves up (e.g. \x1b[4A followed by \x1b[3A) which would blast off the top of screen!
    assert!(
        !sequence.contains("\x1b[3A"),
        "must not compound upward movements: {sequence}"
    );
}

#[test]
fn status_line_context_is_humanized() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 68_000, 272_000);

    let line = ui.format_status_line(80).expect("status line");
    let stripped = dialoguer::console::strip_ansi_codes(&line).to_string();
    assert!(stripped.contains("ctx: 25% (68k/272k)"), "{stripped}");
    assert!(!stripped.contains("68000"), "{stripped}");
}

#[test]
fn humanize_tokens_stays_exact_below_10k() {
    assert_eq!(humanize_tokens(0), "0");
    assert_eq!(humanize_tokens(999), "999");
    assert_eq!(humanize_tokens(9_999), "9999");
    assert_eq!(humanize_tokens(10_000), "10k");
    assert_eq!(humanize_tokens(12_345), "12.3k");
    assert_eq!(humanize_tokens(272_000), "272k");
    assert_eq!(humanize_tokens(1_000_000), "1M");
    assert_eq!(humanize_tokens(2_000_000), "2M");
}

/// A reset stamp `secs` seconds from now, in the shape the provider sends.
fn reset_in(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[test]
fn status_line_displays_antigravity_account_and_quota_format() {
    let ui = ChatUi::new(true);
    ui.set_status("gemini-3.8-flash", 0, 1_000_000);
    ui.set_account_name(Some("daryllvelonio9@gmail.com".to_string()));
    // Off the minute boundary, so the rendered countdown is stable long enough to assert on.
    ui.set_quota_lines(
        "gemini-3.8-flash",
        vec![
            QuotaLine {
                window: QuotaWindow::FiveHour,
                remaining_fraction: 1.0,
                reset_time: Some(reset_in(4 * 3600 + 30 * 60 + 30)),
            },
            QuotaLine {
                window: QuotaWindow::Weekly,
                remaining_fraction: 1.0,
                reset_time: Some(reset_in(167 * 3600 + 30)),
            },
        ],
    );

    let line = ui.format_status_line(120).expect("status line");
    let stripped = dialoguer::console::strip_ansi_codes(&line).to_string();
    assert!(
        stripped.contains("gemini-3.8-flash · daryllvelonio9@gmail.com"),
        "{stripped}"
    );
    assert!(
        stripped.contains("0/1M · 5h 100% (4h 30m) · w 100% (6d 23h)"),
        "{stripped}"
    );
}

#[test]
fn a_quota_refresh_for_another_model_does_not_paint() {
    let ui = ChatUi::new(true);
    ui.set_status("claude-sonnet-4-6", 0, 200_000);
    // What an in-flight refresh for the model the user just switched away from looks like.
    ui.set_quota_lines(
        "gemini-3.8-flash",
        vec![QuotaLine {
            window: QuotaWindow::FiveHour,
            remaining_fraction: 0.5,
            reset_time: None,
        }],
    );

    let line = ui.format_status_line(120).expect("status line");
    let stripped = dialoguer::console::strip_ansi_codes(&line).to_string();
    assert!(!stripped.contains("50%"), "{stripped}");
}

/// The quota text the status bar last drew.
fn quota_drawn(ui: &ChatUi) -> String {
    ui.status
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.quota_drawn.clone())
        .unwrap_or_default()
}

/// A quota readout that is drawn once and then holds still is the reported bug: the numbers were
/// refetched only by restarting the session. This is the tick's half — it re-renders the countdown
/// and takes the new text as soon as a minute of it has passed.
#[test]
fn the_tick_takes_a_quota_readout_that_has_moved_a_minute() {
    let ui = ChatUi::new(true);
    ui.set_status("gemini-3.8-flash", 0, 1_000_000);
    ui.set_quota_lines(
        "gemini-3.8-flash",
        vec![QuotaLine {
            window: QuotaWindow::FiveHour,
            remaining_fraction: 0.86,
            // Off the minute boundary, so the countdown is stable for a minute either way.
            reset_time: Some(reset_in(4 * 3600 + 30 * 60 + 30)),
        }],
    );

    let ticked = ui.clone();
    capture_stdout(move || ticked.refresh_quota_clock());
    assert_eq!(quota_drawn(&ui), "5h 86% (4h 30m)");

    // Seed the row with what the previous minute drew. A tick then has something to notice, which
    // is the comparison under test — the clock itself is what the renderer is asked for.
    {
        let mut status = ui.status.lock().unwrap();
        status.as_mut().unwrap().quota_drawn = "5h 86% (4h 31m)".to_string();
    }
    let ticked = ui.clone();
    capture_stdout(move || ticked.refresh_quota_clock());
    assert_eq!(quota_drawn(&ui), "5h 86% (4h 30m)");
}

#[test]
fn status_line_handles_zero_context_max() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 0, 0);

    let line = ui.format_status_line(80).expect("status line");
    let stripped = dialoguer::console::strip_ansi_codes(&line).to_string();
    assert!(stripped.contains("ctx: 0/0"), "{stripped}");
}

#[test]
fn physical_line_count_calculates_wrapped_rows() {
    assert_eq!(physical_line_count(0, 80), 1);
    assert_eq!(physical_line_count(80, 80), 1);
    assert_eq!(physical_line_count(81, 80), 2);
    assert_eq!(physical_line_count(100, 70), 2);
    assert_eq!(physical_line_count(140, 70), 2);
    assert_eq!(physical_line_count(141, 70), 3);
}

#[test]
fn status_line_never_exceeds_terminal_width_even_when_narrow() {
    let ui = ChatUi::new(true);
    ui.set_status("z-ai/glm-5.3-flash", 3902, 1_000_000);
    ui.set_session_name("long-session-name-here");

    for width in [15, 20, 30, 45, 60, 80, 120] {
        let line = ui.format_status_line(width).expect("status line");
        let stripped = dialoguer::console::strip_ansi_codes(&line);
        assert!(
            stripped.chars().count() <= width,
            "width {width}: line length {} exceeds width: {:?}",
            stripped.chars().count(),
            stripped
        );
    }
}

/// The prompt is never bare when the line is empty: it says what to do next.
#[test]
fn the_empty_composer_shows_a_hint_and_a_typed_line_does_not() {
    let ui = ChatUi::new(true);
    let bare = visible_width(&strip(&ui.prompt_row("")));

    let (row, hint_width) = ui.prompt_row_drawn("");
    let plain = strip(&row);
    assert!(
        plain.contains("/help"),
        "the hint teaches the way in: {plain}"
    );
    assert_eq!(
        hint_width,
        visible_width(&plain) - bare,
        "the hint's own width comes back for the caret: {plain}"
    );
    assert!(
        hint_width > 0,
        "the caret has somewhere to walk back over: {plain}"
    );

    // Typing replaces the hint: the row carries the text and nothing else.
    let (row, hint_width) = ui.prompt_row_drawn("hello");
    let plain = strip(&row);
    assert_eq!(hint_width, 0, "text means no hint: {plain}");
    assert!(plain.contains("hello"), "{plain}");
    assert!(!plain.contains("/help"), "the hint is gone: {plain}");
}

/// The hint names the commands that only work while a turn is running.
#[test]
fn the_hint_says_what_luna_is_doing() {
    let ui = ChatUi::new(true);
    assert!(!ui.turn_active(), "a fresh ui is idle");
    let idle = strip(&ui.prompt_row_drawn("").0);
    assert!(idle.contains("inactive"), "{idle}");

    ui.set_turn_active(true);
    assert!(ui.turn_active());
    let busy = strip(&ui.prompt_row_drawn("").0);
    assert!(busy.contains("redirect"), "{busy}");
    assert!(busy.contains("/steer"), "{busy}");
    assert!(busy.contains("/queue"), "{busy}");
    assert!(!busy.contains("inactive"), "{busy}");

    // Idle again: the line goes back to teaching /help.
    ui.set_turn_active(false);
    assert!(strip(&ui.prompt_row_drawn("").0).contains("inactive"));
}

/// The hint shares the prompt row, so it must never push the row past the terminal width: a
/// wrapped prompt row desyncs every cursor move the in-place repaint makes.
#[test]
fn the_hint_never_makes_the_prompt_row_wrap() {
    let ui = ChatUi::new(true);
    for busy in [false, true] {
        ui.set_turn_active(busy);
        for width in [1, 8, 12, 15, 20, 40, 80, 200] {
            let Some(hint) = ui.hint_for(width, "") else {
                continue;
            };
            assert!(
                PROMPT.chars().count() + visible_width(&hint) < width,
                "width {width}: the prompt row with '{hint}' does not fit"
            );
        }
    }
}

/// A hint needs room before it is worth drawing: a clipped fragment teaches nothing.
#[test]
fn a_terminal_too_narrow_gets_no_hint() {
    let ui = ChatUi::new(true);
    assert!(ui.hint_for(HINT_MIN_ROOM - 1, "").is_none());
    assert!(ui.hint_for(1, "").is_none());
    assert!(ui.hint_for(200, "").is_some());
    // The composer's own text always wins over the hint, however wide the terminal is.
    assert!(ui.hint_for(200, "typing").is_none());
}

/// The erase arithmetic reads the drawn row's width, so a hint the metrics do not know about
/// would leave a row behind on the next redraw.
#[test]
fn the_hint_is_part_of_the_drawn_composer() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 0, 1_000);
    let bare = visible_width(&strip(&ui.prompt_row("")));

    let boxed = ui.clone();
    capture_stdout(move || boxed.draw_box(80, ""));
    let metrics = ui.drawn_metrics.lock().unwrap();
    let drawn = metrics.as_ref().expect("the box was drawn");
    assert!(
        drawn.prompt_len > bare,
        "the hint is measured with the row: {} vs {bare}",
        drawn.prompt_len
    );
    assert!(
        drawn.prompt_drawn.contains(IDLE_HINT),
        "the drawn row is the one with the hint: {:?}",
        drawn.prompt_drawn
    );
}

/// While the model is thinking the block is drawn whole: the live region is the only place the
/// reasoning is visible at all, so a fixed tail there is a fixed blind spot in it.
#[test]
fn the_whole_thought_block_streams_while_thinking() {
    let ui = ChatUi::new(true);
    ui.push_reasoning(&thought_lines(9));

    let ticked = ui.clone();
    capture_stdout(move || ticked.animate_tick());

    let rows = ui.live.lock().unwrap().rows.clone();
    assert_eq!(rows.len(), 9, "every row of the block is drawn: {rows:?}");
    assert!(strip(&rows[0]).contains("thought line 1"), "{rows:?}");
    assert!(strip(&rows[8]).contains("thought line 9"), "{rows:?}");
}

/// The screen bounds the block, not a fixed count — and when it cannot fit, the newest rows are
/// the ones kept, because the interesting end of a stream is its end.
#[test]
fn a_thought_block_taller_than_the_screen_keeps_its_newest_rows() {
    let ui = ChatUi::new(true);
    ui.push_reasoning(&thought_lines(200));

    let ticked = ui.clone();
    capture_stdout(move || ticked.animate_tick());

    let rows = ui.live.lock().unwrap().rows.clone();
    let height = get_terminal_height();
    assert!(rows.len() >= MIN_LIVE_REASONING_ROWS, "{rows:?}");
    assert!(
        rows.len() + ui.box_row_count() + 2 <= height,
        "the block still leaves the composer its rows: {} of {height}",
        rows.len()
    );
    assert!(
        strip(rows.last().unwrap()).contains("thought line 200"),
        "the newest row is kept: {rows:?}"
    );
    assert!(
        !strip(&rows[0]).ends_with("thought line 1"),
        "the rows that went are the oldest: {rows:?}"
    );
}

/// A thought block of `count` numbered rows. No trailing newline: the row a trailing one would
/// wrap to is a real blank row, and these tests count rows.
fn thought_lines(count: usize) -> String {
    (1..=count)
        .map(|n| format!("thought line {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Thinking over, the whole block goes into the transcript: the live region has been the only
/// place it was visible, so folding it away at the end would hide what the user watched arrive.
#[test]
fn a_finished_thought_block_is_printed_whole() {
    let rows = reasoning_rows(&thought_lines(12), 80);
    let text = finished_reasoning_text(&rows);

    assert_eq!(
        text.lines().count(),
        12,
        "every thought row is handed to the transcript: {text:?}"
    );
    assert!(
        text.contains("thought line 1"),
        "the opening row is kept: {text:?}"
    );
    assert!(
        text.contains("thought line 12"),
        "the newest row is kept: {text:?}"
    );
    assert!(
        !text.contains("earlier lines"),
        "nothing is folded behind a count: {text:?}"
    );
}

/// A finished block belongs to the transcript: the live region gives it up rather than drawing it
/// twice.
#[test]
fn finishing_a_thought_block_hands_it_to_the_transcript() {
    let ui = ChatUi::new(true);
    ui.push_reasoning(&thought_lines(3));
    ui.finish_reasoning();

    assert!(
        ui.live.lock().unwrap().reasoning.is_empty(),
        "the live block is taken"
    );
}

fn strip(s: &str) -> String {
    dialoguer::console::strip_ansi_codes(s).to_string()
}

/// A modal owns the alternate screen; the chat loop is blocked on the prompt while it is up, but
/// updates still arrive from other tasks (the provider's usage callback, a delegate finishing).
/// Painting the composer there would put it on the modal's screen, which is the OpenCode approval
/// box the user saw with the status line and composer left visible underneath it.
#[test]
fn a_modal_keeps_background_updates_off_the_screen() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 0, 1000);
    ui.draw_box(80, "");
    assert!(ui.box_drawn.load(Ordering::SeqCst));

    let before = ui
        .drawn_metrics
        .lock()
        .unwrap()
        .clone()
        .expect("the composer was drawn");

    ui.set_modal_active(true);
    ui.update_context_used(4096);

    let after = ui
        .drawn_metrics
        .lock()
        .unwrap()
        .clone()
        .expect("the composer metrics survive");
    assert_eq!(
        after.block_rows, before.block_rows,
        "a background status update must not repaint the composer over the modal"
    );
    assert_eq!(
        ui.status.lock().unwrap().as_ref().unwrap().context_used,
        4096,
        "the update is still recorded for when the screen comes back"
    );

    ui.emit_above("a line from a background task");
    assert_eq!(
        ui.pending_above.lock().unwrap().as_slice(),
        ["a line from a background task"],
        "the transcript line is held while the modal owns the screen"
    );

    ui.set_modal_active(false);
    assert!(
        ui.pending_above.lock().unwrap().is_empty(),
        "the held line is written once the screen is back"
    );
}

/// A context bar that falls while nothing left the transcript reads as a reset, not as a
/// measurement. Turn-boundary recomputes use a different source than the running turn (the
/// provider's count versus the character heuristic, or a stored report without its tail), so the
/// gauge holds its high-water mark; only a fold, which really removes history, may lower it.
#[test]
fn the_context_gauge_holds_its_high_water_mark_until_a_fold() {
    let ui = ChatUi::new(true);
    ui.set_status("test-model", 126_000, 1_000_000);

    ui.update_status(94_000, 1_000_000);
    ui.update_context_used(94_000);
    assert_eq!(
        ui.status.lock().unwrap().as_ref().unwrap().context_used,
        126_000,
        "a boundary estimate smaller than the gauge must not rewind it"
    );

    ui.update_context_used(130_000);
    assert_eq!(
        ui.status.lock().unwrap().as_ref().unwrap().context_used,
        130_000,
        "growth is still shown immediately"
    );

    // A fold lands mid-turn: the running turn is still on the transcript it replaced, so the
    // shrunken size waits for the boundary instead of flashing and jumping back.
    ui.defer_context_drop();
    ui.update_context_used(90_000);
    assert_eq!(
        ui.status.lock().unwrap().as_ref().unwrap().context_used,
        130_000,
        "a mid-turn fold does not paint its smaller size immediately"
    );
    ui.update_status(90_000, 1_000_000);
    assert_eq!(
        ui.status.lock().unwrap().as_ref().unwrap().context_used,
        90_000,
        "the boundary takes the folded size once"
    );
    ui.update_status(80_000, 1_000_000);
    assert_eq!(
        ui.status.lock().unwrap().as_ref().unwrap().context_used,
        90_000,
        "the deferred drop is consumed by one boundary"
    );

    ui.reset_context_used(20_000);
    assert_eq!(
        ui.status.lock().unwrap().as_ref().unwrap().context_used,
        20_000,
        "an explicit fold takes the smaller figure at once"
    );
}

/// The flag alone cannot stop a paint that had already passed its check when the modal went up.
/// Such a paint is counted as in flight, and the modal waits for the count to reach zero before it
/// takes the alternate screen.
#[test]
fn paints_are_counted_while_they_run() {
    let ui = ChatUi::new(true);
    {
        let _first = ui.paint_in_flight();
        let _second = ui.paint_in_flight();
        assert_eq!(ui.paints_in_flight.load(Ordering::SeqCst), 2);
    }
    assert_eq!(
        ui.paints_in_flight.load(Ordering::SeqCst),
        0,
        "each paint is given up when it ends"
    );
}

/// The background saver's report is a glance, not transcript: one row per memory, named and with
/// the status the saver chose.
#[test]
fn a_memory_notice_names_the_memory_and_its_status() {
    let ui = ChatUi::new(true);
    ui.push_memory_notice("Astra", "saved");

    let rows = ui.memory_notice_rows(80);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains("memory"), "{}", rows[0]);
    assert!(rows[0].contains("Astra"), "{}", rows[0]);
    assert!(rows[0].contains("saved"), "{}", rows[0]);
}

/// A note write is announced with its own prefix, so a saved note is never mistaken for a memory.
#[test]
fn a_note_notice_names_the_note_and_its_status() {
    let ui = ChatUi::new(true);
    ui.push_note_notice("Memory Saver Plan", "updated");

    let rows = ui.memory_notice_rows(80);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains("note"), "{}", rows[0]);
    assert!(rows[0].contains("Memory Saver Plan"), "{}", rows[0]);
    assert!(rows[0].contains("updated"), "{}", rows[0]);
}

/// A generated name can be a paragraph; the CLI keeps the status on screen by trimming the name
/// to the row, with an ellipsis so the cut is visible.
#[test]
fn a_long_memory_name_is_trimmed_to_fit_the_notice() {
    let ui = ChatUi::new(true);
    ui.push_memory_notice(&"long memory name ".repeat(20), "updated");

    let rows = ui.memory_notice_rows(48);
    assert_eq!(rows.len(), 1);
    assert!(
        visible_width(&rows[0]) <= 48,
        "row is wider than the terminal: {} cells, {rows:?}",
        visible_width(&rows[0])
    );
    assert!(rows[0].contains('…'), "{}", rows[0]);
    assert!(rows[0].contains("updated"), "{}", rows[0]);
}

/// The notice is temporary: the next turn drops it, and a later pass for the same memory replaces
/// the status instead of stacking a stale row.
#[test]
fn memory_notices_are_temporary_and_keep_one_row_per_memory() {
    let ui = ChatUi::new(true);
    ui.push_memory_notice("Astra", "saved");
    ui.push_memory_notice("Astra", "updated");
    assert_eq!(ui.memory_notice_rows(80).len(), 1);
    assert!(ui.memory_notice_rows(80)[0].contains("updated"));

    ui.clear_memory_notices();
    assert!(ui.memory_notice_rows(80).is_empty());
}

#[test]
fn a_memory_notice_with_detail_shows_changes_properly() {
    let ui = ChatUi::new(true);
    ui.push_memory_notice_with_detail(
        "user.relationship.occasion",
        Some("\"girlfriend monthsary today\" → \"December 9 is our anniversary\""),
        "updated",
    );

    let rows = ui.memory_notice_rows(120);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains("memory"), "{}", rows[0]);
    assert!(rows[0].contains("user.relationship.occasion"), "{}", rows[0]);
    assert!(rows[0].contains("updated"), "{}", rows[0]);
    assert!(
        rows[0].contains("\"girlfriend monthsary today\" → \"December 9 is our anniversary\""),
        "{}",
        rows[0]
    );
}

#[test]
fn delegate_task_notification_format_contains_title_tool_and_running_time() {
    let title =
        crate::commands::chat::render::format_delegate_title("Extract all role credentials");
    let dur =
        crate::commands::chat::render::format_delegate_duration(std::time::Duration::from_secs(14));
    let notification = crate::commands::chat::render::format_delegate_notification(
        "🤖",
        &title,
        "read_file",
        &dur,
    );
    assert!(notification.contains("delegate:"), "{notification}");
    assert!(
        notification.contains("Extract all role credentials"),
        "{notification}"
    );
    assert!(notification.contains("> read_file (14s)"), "{notification}");
}
