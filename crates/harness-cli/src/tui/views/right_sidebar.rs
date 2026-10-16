use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use crate::tui::app::{AppState, PanelFocus};

pub fn render_right_sidebar(f: &mut Frame, app: &mut AppState, area: Rect) {
    app.rect_right = area;
    let is_focused = app.focus == PanelFocus::RightSidebar;

    let border_style = if is_focused {
        app.theme.border_focused
    } else {
        app.theme.border
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(app.theme.border_type)
        .border_style(border_style)
        .title(Span::styled(" Telemetry & Context ", app.theme.title));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8), // Metrics table
            Constraint::Length(4), // Context gauge
            Constraint::Min(6),    // Active Background Tasks
        ])
        .split(inner);

    // 1. Metric rows
    let metric_lines = vec![
        make_metric_line("Subagents", app.subagents_count, app.theme.text_muted, app.theme.text),
        make_metric_line("Files Changed", app.files_changed_count, app.theme.text_muted, app.theme.accent_amber),
        make_metric_line("Artifacts", app.artifacts_count, app.theme.text_muted, app.theme.text),
        make_metric_line("Uploads", app.uploads_count, app.theme.text_muted, app.theme.text),
        make_metric_line("Background Tasks", app.background_tasks_count, app.theme.text_muted, app.theme.accent_cyan),
        make_metric_line("Terminals", app.terminals_count, app.theme.text_muted, app.theme.text),
    ];
    let metrics_para = Paragraph::new(metric_lines);
    f.render_widget(metrics_para, chunks[0]);

    // 2. Context Window Meter
    let percent = if app.context_max > 0 {
        ((app.context_used as f64 / app.context_max as f64) * 100.0).min(100.0) as u16
    } else {
        0
    };

    let gauge_title = format!("Context: {}/{} ({:.1}%)", app.context_used, app.context_max, percent as f64);
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::TOP).border_style(app.theme.border).title(Span::styled(gauge_title, app.theme.text_muted)))
        .gauge_style(app.theme.accent_purple)
        .percent(percent);
    f.render_widget(gauge, chunks[1]);

    // 3. Active Background Task / Evolver Card
    let bg_title = Span::styled("Active Evolver", app.theme.title);
    let bg_block = Block::default()
        .borders(Borders::TOP)
        .border_style(app.theme.border)
        .title(bg_title);

    let bg_lines = if let Some(ref title) = app.background_job_title {
        vec![
            Line::from(vec![
                Span::styled("⚙ ", app.theme.accent_cyan.add_modifier(Modifier::BOLD)),
                Span::styled("evolver-harness", app.theme.accent_cyan),
            ]),
            Line::from(vec![
                Span::styled("Task: ", app.theme.text_muted),
                Span::styled(title, app.theme.text),
            ]),
            Line::from(vec![
                Span::styled("[Ctrl+B] Open Viewer", app.theme.accent_purple),
            ]),
        ]
    } else {
        vec![
            Line::from(Span::styled("No active background tasks.", app.theme.text_muted)),
            Line::from(Span::styled("Runs asynchronously when invoked.", app.theme.text_muted)),
        ]
    };

    let bg_para = Paragraph::new(bg_lines).block(bg_block);
    f.render_widget(bg_para, chunks[2]);
}

fn make_metric_line(
    label: &str,
    val: usize,
    label_style: ratatui::style::Style,
    val_style: ratatui::style::Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<18}", label), label_style),
        Span::styled(val.to_string(), val_style),
    ])
}
