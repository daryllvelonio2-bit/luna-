pub mod app;
pub mod theme;
pub mod views;

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Terminal;

use self::app::{AppState, PanelFocus};
use self::views::center::render_center;
use self::views::header::render_header;
use self::views::left_sidebar::render_left_sidebar;
use self::views::right_sidebar::render_right_sidebar;

/// RAII Terminal Guard ensuring the terminal is restored to normal state on exit.
pub struct TuiTerminalGuard;

impl TuiTerminalGuard {
    pub fn acquire() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TuiTerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Run the interactive Antigravity-style TUI
pub async fn run_app_tui(mut app: AppState) -> io::Result<()> {
    let _guard = TuiTerminalGuard::acquire()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut event_stream = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        // Render UI
        terminal.draw(|f| {
            let full_area = f.area();
            if app.theme.bg != ratatui::style::Color::Reset {
                f.render_widget(
                    ratatui::widgets::Block::default()
                        .style(ratatui::style::Style::default().bg(app.theme.bg)),
                    full_area,
                );
            }

            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2), // Top Header Bar
                    Constraint::Min(10),   // 3-Column Antigravity Body
                ])
                .split(full_area);

            // 1. Top Header
            render_header(f, &mut app, main_chunks[0]);

            // 2. Responsive Multi-Column Body
            let body_area = main_chunks[1];
            let width = body_area.width;

            let show_left = !app.left_collapsed && width >= 80;
            let show_right = !app.right_collapsed && width >= 110;

            let left_w = if show_left {
                app.left_width.min(width / 3).max(16)
            } else {
                0
            };
            let right_w = if show_right {
                app.right_width.min(width / 3).max(16)
            } else {
                0
            };

            match (show_left, show_right) {
                (true, true) => {
                    let body_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Length(left_w),
                            Constraint::Min(35),
                            Constraint::Length(right_w),
                        ])
                        .split(body_area);
                    render_left_sidebar(f, &mut app, body_chunks[0]);
                    render_center(f, &mut app, body_chunks[1]);
                    render_right_sidebar(f, &mut app, body_chunks[2]);
                }
                (true, false) => {
                    app.rect_right = ratatui::layout::Rect::default();
                    let body_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Length(left_w), Constraint::Min(35)])
                        .split(body_area);
                    render_left_sidebar(f, &mut app, body_chunks[0]);
                    render_center(f, &mut app, body_chunks[1]);
                }
                (false, true) => {
                    app.rect_left = ratatui::layout::Rect::default();
                    let body_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Min(35), Constraint::Length(right_w)])
                        .split(body_area);
                    render_center(f, &mut app, body_chunks[0]);
                    render_right_sidebar(f, &mut app, body_chunks[1]);
                }
                (false, false) => {
                    app.rect_left = ratatui::layout::Rect::default();
                    app.rect_right = ratatui::layout::Rect::default();
                    render_center(f, &mut app, body_area);
                }
            }
        })?;

        if app.should_quit {
            break;
        }

        let term_width = terminal.size()?.width;

        tokio::select! {
            _ = tick_interval.tick() => {
                // Heartbeat / timer tick for animations
            }

            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        handle_event(&mut app, event, term_width);
                    }
                    Some(Err(_)) | None => break,
                }
            }
        }
    }

    Ok(())
}

fn handle_event(app: &mut AppState, event: Event, width: u16) {
    match event {
        Event::Key(key) => {
            if key.kind != KeyEventKind::Press {
                return;
            }

            // Global Shortcuts
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('c') | KeyCode::Char('q') => {
                        app.should_quit = true;
                        return;
                    }
                    KeyCode::Char('t') => {
                        app.cycle_theme();
                        return;
                    }
                    KeyCode::Char('s') => {
                        app.toggle_left_sidebar();
                        return;
                    }
                    KeyCode::Char('b') => {
                        app.toggle_right_sidebar();
                        return;
                    }
                    KeyCode::Char('[') => {
                        app.resize_left(-2, width);
                        return;
                    }
                    KeyCode::Char(']') => {
                        app.resize_left(2, width);
                        return;
                    }
                    KeyCode::Char('n') => {
                        app.sessions.insert(0, app::SessionSummaryItem {
                            id: format!("session_{}", app.sessions.len() + 1),
                            name: format!("New Chat {}", app.sessions.len() + 1),
                            active: true,
                            time: "just now".to_string(),
                        });
                        for s in app.sessions.iter_mut().skip(1) {
                            s.active = false;
                        }
                        app.selected_session_idx = 0;
                        app.transcript.clear();
                        return;
                    }
                    _ => {}
                }
            }

            // Alt navigation & resizing
            if key.modifiers.contains(KeyModifiers::ALT) {
                match key.code {
                    KeyCode::Left => {
                        app.resize_left(-2, width);
                        return;
                    }
                    KeyCode::Right => {
                        app.resize_left(2, width);
                        return;
                    }
                    _ => {}
                }
            }

            // Tab navigation between panels
            if key.code == KeyCode::Tab {
                app.cycle_focus();
                return;
            }

            // Panel-specific navigation & actions
            match app.focus {
                PanelFocus::Composer => match key.code {
                    KeyCode::Enter => {
                        app.submit_input();
                    }
                    KeyCode::Char(c) => {
                        app.composer_input.push(c);
                        app.composer_cursor += 1;
                    }
                    KeyCode::Backspace => {
                        if !app.composer_input.is_empty() {
                            app.composer_input.pop();
                            app.composer_cursor = app.composer_cursor.saturating_sub(1);
                        }
                    }
                    KeyCode::Esc => {
                        app.focus = PanelFocus::CenterTranscript;
                    }
                    _ => {}
                },
                PanelFocus::CenterTranscript => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.transcript_scroll = app.transcript_scroll.saturating_add(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.transcript_scroll = app.transcript_scroll.saturating_sub(1);
                    }
                    KeyCode::Char('i') | KeyCode::Enter => {
                        app.focus = PanelFocus::Composer;
                    }
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    _ => {}
                },
                PanelFocus::LeftSidebar => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if app.selected_session_idx > 0 {
                            app.selected_session_idx -= 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if app.selected_session_idx + 1 < app.sessions.len() {
                            app.selected_session_idx += 1;
                        }
                    }
                    KeyCode::Enter => {
                        for (i, s) in app.sessions.iter_mut().enumerate() {
                            s.active = i == app.selected_session_idx;
                        }
                        app.focus = PanelFocus::Composer;
                    }
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    _ => {}
                },
                PanelFocus::RightSidebar => match key.code {
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    KeyCode::Esc => {
                        app.focus = PanelFocus::Composer;
                    }
                    _ => {}
                },
            }
        }

        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                app.handle_mouse_down(mouse.column, mouse.row);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                app.handle_mouse_drag(mouse.column, width);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                app.handle_mouse_up();
            }
            MouseEventKind::ScrollUp => {
                app.handle_mouse_scroll(true, mouse.column, mouse.row);
            }
            MouseEventKind::ScrollDown => {
                app.handle_mouse_scroll(false, mouse.column, mouse.row);
            }
            _ => {}
        },

        Event::Resize(_, _) => {
            // Ratatui auto-handles resizing on next terminal.draw()
        }

        _ => {}
    }
}
