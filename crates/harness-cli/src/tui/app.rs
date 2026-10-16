use ratatui::layout::Rect;
use super::theme::{Theme, ThemeMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelFocus {
    LeftSidebar,
    CenterTranscript,
    Composer,
    RightSidebar,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub author: String,
    pub content: String,
    pub timestamp: String,
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Debug, Clone)]
pub struct SessionSummaryItem {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub time: String,
}

pub struct AppState {
    pub theme: Theme,
    pub focus: PanelFocus,
    pub workspace_name: String,
    pub current_file: String,
    pub sessions: Vec<SessionSummaryItem>,
    pub selected_session_idx: usize,
    pub workspace_files: Vec<String>,
    pub selected_file_idx: usize,
    pub transcript: Vec<ChatMessage>,
    pub transcript_scroll: usize,
    pub composer_input: String,
    pub composer_cursor: usize,
    pub model_name: String,

    // Telemetry & context stats
    pub subagents_count: usize,
    pub files_changed_count: usize,
    pub artifacts_count: usize,
    pub uploads_count: usize,
    pub background_tasks_count: usize,
    pub terminals_count: usize,
    pub context_used: usize,
    pub context_max: usize,
    pub background_job_title: Option<String>,

    pub should_quit: bool,

    // Cached layout bounding boxes for mouse interaction
    pub rect_header: Rect,
    pub rect_left: Rect,
    pub rect_center: Rect,
    pub rect_composer: Rect,
    pub rect_right: Rect,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            focus: PanelFocus::Composer,
            workspace_name: "rust-agent-harness".to_string(),
            current_file: "crates/harness-cli/src/main.rs".to_string(),
            sessions: vec![
                SessionSummaryItem {
                    id: "luna".to_string(),
                    name: "Luna Main Session".to_string(),
                    active: true,
                    time: "just now".to_string(),
                },
                SessionSummaryItem {
                    id: "refactor".to_string(),
                    name: "TUI Architecture".to_string(),
                    active: false,
                    time: "10m ago".to_string(),
                },
                SessionSummaryItem {
                    id: "evolver-logs".to_string(),
                    name: "Evolver Hardening".to_string(),
                    active: false,
                    time: "1h ago".to_string(),
                },
            ],
            selected_session_idx: 0,
            workspace_files: vec![
                "crates/harness-cli/src/main.rs".to_string(),
                "crates/harness-cli/src/commands/chat.rs".to_string(),
                "crates/harness-core/src/agent.rs".to_string(),
                "crates/harness-evolver/src/engine.rs".to_string(),
                "agents.md".to_string(),
                "Cargo.toml".to_string(),
            ],
            selected_file_idx: 0,
            transcript: vec![
                ChatMessage {
                    role: MessageRole::Assistant,
                    author: "Luna".to_string(),
                    content: "Welcome to Luna. Workspace indexed and ready.\nAsk anything, / for actions, or @ for context references.".to_string(),
                    timestamp: "Now".to_string(),
                    tool_name: None,
                },
            ],
            transcript_scroll: 0,
            composer_input: String::new(),
            composer_cursor: 0,
            model_name: "Gemini 3.8 Flash High".to_string(),
            subagents_count: 0,
            files_changed_count: 3,
            artifacts_count: 0,
            uploads_count: 0,
            background_tasks_count: 0,
            terminals_count: 0,
            context_used: 3642,
            context_max: 128000,
            background_job_title: None,
            should_quit: false,
            rect_header: Rect::default(),
            rect_left: Rect::default(),
            rect_center: Rect::default(),
            rect_composer: Rect::default(),
            rect_right: Rect::default(),
        }
    }
}

impl AppState {
    pub fn new(model_name: String, workspace_name: String) -> Self {
        let mut app = Self::default();
        app.model_name = model_name;
        app.workspace_name = workspace_name;
        app
    }

    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            PanelFocus::LeftSidebar => PanelFocus::CenterTranscript,
            PanelFocus::CenterTranscript => PanelFocus::Composer,
            PanelFocus::Composer => PanelFocus::RightSidebar,
            PanelFocus::RightSidebar => PanelFocus::LeftSidebar,
        };
    }

    pub fn cycle_theme(&mut self) {
        let next_mode = self.theme.mode.next();
        self.theme = Theme::for_mode(next_mode);
    }

    pub fn set_theme_mode(&mut self, mode: ThemeMode) {
        self.theme = Theme::for_mode(mode);
    }

    pub fn handle_mouse_click(&mut self, col: u16, row: u16) {
        if is_inside_rect(col, row, self.rect_composer) {
            self.focus = PanelFocus::Composer;
        } else if is_inside_rect(col, row, self.rect_center) {
            self.focus = PanelFocus::CenterTranscript;
        } else if is_inside_rect(col, row, self.rect_left) {
            self.focus = PanelFocus::LeftSidebar;
            // Check if clicking in session list area (top half of left sidebar)
            let relative_y = row.saturating_sub(self.rect_left.y + 2) as usize;
            if relative_y < self.sessions.len() {
                self.selected_session_idx = relative_y;
                for (i, s) in self.sessions.iter_mut().enumerate() {
                    s.active = i == relative_y;
                }
            }
        } else if is_inside_rect(col, row, self.rect_right) {
            self.focus = PanelFocus::RightSidebar;
        }
    }

    pub fn handle_mouse_scroll(&mut self, up: bool, col: u16, row: u16) {
        if is_inside_rect(col, row, self.rect_center) {
            if up {
                self.transcript_scroll = self.transcript_scroll.saturating_add(2);
            } else {
                self.transcript_scroll = self.transcript_scroll.saturating_sub(2);
            }
        } else if is_inside_rect(col, row, self.rect_left) {
            if up && self.selected_session_idx > 0 {
                self.selected_session_idx -= 1;
            } else if !up && self.selected_session_idx + 1 < self.sessions.len() {
                self.selected_session_idx += 1;
            }
        }
    }

    pub fn submit_input(&mut self) {
        let text = self.composer_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.transcript.push(ChatMessage {
            role: MessageRole::User,
            author: "User".to_string(),
            content: text,
            timestamp: "Just now".to_string(),
            tool_name: None,
        });
        self.composer_input.clear();
        self.composer_cursor = 0;
        self.transcript_scroll = 0;
    }
}

fn is_inside_rect(x: u16, y: u16, r: Rect) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}
