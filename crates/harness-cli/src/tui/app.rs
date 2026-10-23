use super::theme::{Theme, ThemeMode};
use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelFocus {
    LeftSidebar,
    CenterTranscript,
    Composer,
    RightSidebar,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub author: String,
    pub content: String,
    pub timestamp: String,
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SessionSummaryItem {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub time: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragTarget {
    LeftDivider,
    RightDivider,
}

#[allow(dead_code)]
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

    // Responsive sidebar sizing & toggles
    pub left_width: u16,
    pub right_width: u16,
    pub left_collapsed: bool,
    pub right_collapsed: bool,
    pub active_drag: Option<DragTarget>,
    pub mouse_down_pos: Option<(u16, u16)>,

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
            workspace_name: String::new(),
            current_file: String::new(),
            sessions: Vec::new(),
            selected_session_idx: 0,
            workspace_files: Vec::new(),
            selected_file_idx: 0,
            transcript: Vec::new(),
            transcript_scroll: 0,
            composer_input: String::new(),
            composer_cursor: 0,
            model_name: String::new(),
            left_width: 28,
            right_width: 30,
            left_collapsed: false,
            right_collapsed: false,
            active_drag: None,
            mouse_down_pos: None,
            subagents_count: 0,
            files_changed_count: 0,
            artifacts_count: 0,
            uploads_count: 0,
            background_tasks_count: 0,
            terminals_count: 0,
            context_used: 0,
            context_max: 0,
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

    #[allow(dead_code)]
    pub fn set_theme_mode(&mut self, mode: ThemeMode) {
        self.theme = Theme::for_mode(mode);
    }

    pub fn toggle_left_sidebar(&mut self) {
        self.left_collapsed = !self.left_collapsed;
    }

    pub fn toggle_right_sidebar(&mut self) {
        self.right_collapsed = !self.right_collapsed;
    }

    pub fn resize_left(&mut self, delta: i16, max_width: u16) {
        let max = (max_width / 2).max(20) as i16;
        let new_w = (self.left_width as i16 + delta).clamp(16, max);
        self.left_width = new_w as u16;
        self.left_collapsed = false;
    }

    pub fn resize_right(&mut self, delta: i16, max_width: u16) {
        let max = (max_width / 2).max(20) as i16;
        let new_w = (self.right_width as i16 + delta).clamp(16, max);
        self.right_width = new_w as u16;
        self.right_collapsed = false;
    }

    /// Horizontal two-finger trackpad swipe or wheel swipe
    pub fn handle_horizontal_swipe(&mut self, left: bool, col: u16, total_width: u16) {
        if col < total_width / 2 {
            // Left sidebar swipe:
            if left {
                // Swiping left shrinks or collapses
                if self.left_width <= 18 {
                    self.left_collapsed = true;
                } else {
                    self.resize_left(-2, total_width);
                }
            } else {
                // Swiping right expands or restores
                self.left_collapsed = false;
                self.resize_left(2, total_width);
            }
        } else {
            // Right sidebar swipe:
            if left {
                // Swiping left expands or restores
                self.right_collapsed = false;
                self.resize_right(2, total_width);
            } else {
                // Swiping right shrinks or collapses
                if self.right_width <= 18 {
                    self.right_collapsed = true;
                } else {
                    self.resize_right(-2, total_width);
                }
            }
        }
    }

    pub fn handle_mouse_down(&mut self, col: u16, row: u16) {
        self.mouse_down_pos = Some((col, row));

        // Broad boundary detection (+/- 3 cells) so normal swiping/dragging catches easily
        if !self.left_collapsed && self.rect_left.width > 0 {
            let left_edge = self.rect_left.x + self.rect_left.width;
            if col >= left_edge.saturating_sub(3)
                && col <= left_edge + 3
                && row >= self.rect_left.y
                && row < self.rect_left.y + self.rect_left.height
            {
                self.active_drag = Some(DragTarget::LeftDivider);
                return;
            }
        }

        if !self.right_collapsed && self.rect_right.width > 0 {
            let right_edge = self.rect_right.x;
            if col >= right_edge.saturating_sub(3)
                && col <= right_edge + 3
                && row >= self.rect_right.y
                && row < self.rect_right.y + self.rect_right.height
            {
                self.active_drag = Some(DragTarget::RightDivider);
                return;
            }
        }

        // Edge swipe-in detection when collapsed
        if self.left_collapsed && col <= 3 {
            self.active_drag = Some(DragTarget::LeftDivider);
            return;
        }

        if self.right_collapsed && self.rect_center.width > 0 {
            let right_edge = self.rect_center.x + self.rect_center.width;
            if col >= right_edge.saturating_sub(3) {
                self.active_drag = Some(DragTarget::RightDivider);
                return;
            }
        }

        self.handle_mouse_click(col, row);
    }

    pub fn handle_mouse_drag(&mut self, col: u16, total_width: u16) {
        // Detect swipe initiation from mouse down starting point
        if self.active_drag.is_none() {
            if let Some((start_x, _)) = self.mouse_down_pos {
                let dx = (col as i32 - start_x as i32).abs();
                if dx >= 3 {
                    if start_x <= self.rect_left.x + self.rect_left.width + 2 {
                        self.active_drag = Some(DragTarget::LeftDivider);
                    } else if start_x >= self.rect_right.x.saturating_sub(2)
                        && self.rect_right.width > 0
                    {
                        self.active_drag = Some(DragTarget::RightDivider);
                    }
                }
            }
        }

        let max = (total_width / 2).max(20);
        match self.active_drag {
            Some(DragTarget::LeftDivider) => {
                if col < 10 {
                    // Swiped all the way left: collapse
                    self.left_collapsed = true;
                } else {
                    self.left_collapsed = false;
                    self.left_width = col.clamp(16, max);
                }
            }
            Some(DragTarget::RightDivider) => {
                if col > total_width.saturating_sub(10) {
                    // Swiped all the way right: collapse
                    self.right_collapsed = true;
                } else {
                    self.right_collapsed = false;
                    let w = total_width.saturating_sub(col);
                    self.right_width = w.clamp(16, max);
                }
            }
            None => {}
        }
    }

    pub fn handle_mouse_up(&mut self) {
        self.active_drag = None;
        self.mouse_down_pos = None;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sidebar_toggles() {
        let mut app = AppState::default();
        assert!(!app.left_collapsed);
        assert!(!app.right_collapsed);

        app.toggle_left_sidebar();
        assert!(app.left_collapsed);

        app.toggle_right_sidebar();
        assert!(app.right_collapsed);
    }

    #[test]
    fn test_sidebar_resizing() {
        let mut app = AppState::default();
        let initial_left = app.left_width;

        app.resize_left(4, 120);
        assert_eq!(app.left_width, initial_left + 4);

        app.resize_left(-100, 120);
        assert_eq!(app.left_width, 16); // Bounded at minimum 16

        app.active_drag = Some(DragTarget::LeftDivider);
        app.handle_mouse_drag(35, 120);
        assert_eq!(app.left_width, 35);
        app.handle_mouse_up();
        assert_eq!(app.active_drag, None);
    }

    #[test]
    fn test_horizontal_swipe() {
        let mut app = AppState::default();
        let initial_left = app.left_width;

        // Swiping right on left side expands left sidebar
        app.handle_horizontal_swipe(false, 10, 120);
        assert_eq!(app.left_width, initial_left + 2);

        // Swiping left shrinks left sidebar
        app.handle_horizontal_swipe(true, 10, 120);
        assert_eq!(app.left_width, initial_left);

        // Swiping left repeatedly collapses it
        app.left_width = 18;
        app.handle_horizontal_swipe(true, 10, 120);
        assert!(app.left_collapsed);

        // Swiping right uncollapses it
        app.handle_horizontal_swipe(false, 10, 120);
        assert!(!app.left_collapsed);
    }
}
