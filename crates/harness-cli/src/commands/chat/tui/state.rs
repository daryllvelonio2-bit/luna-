//! State structures for the full-screen OpenCode-style TUI.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use harness_core::{
    ApprovalDecision, ApprovalRequest, HarnessError, ReasoningEffort, SessionRecord, TurnOutcome,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::super::commands::CommandEntry;
use super::super::helpers::IdleMemoryPass;
use super::super::render::TurnRecord;
use crate::commands::clarify::{ClarifyRequest, ClarifyResponse};

/// Agent execution mode toggled by `Shift+Tab` (shown in coral/accent on composer footer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentMode {
    #[default]
    Build,
    Auto,
    Plan,
}

impl AgentMode {
    pub fn label(self) -> &'static str {
        match self {
            AgentMode::Build => "Build",
            AgentMode::Auto => "Auto",
            AgentMode::Plan => "Plan",
        }
    }

    pub fn sub_label(self) -> &'static str {
        match self {
            AgentMode::Build => "default",
            AgentMode::Auto => "auto",
            AgentMode::Plan => "plan",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            AgentMode::Build => AgentMode::Auto,
            AgentMode::Auto => AgentMode::Plan,
            AgentMode::Plan => AgentMode::Build,
        }
    }
}

/// Multi-line text composer state with cursor & history navigation.
#[derive(Debug, Clone, Default)]
pub struct ComposerState {
    pub text: String,
    /// Byte index inside `text` (always on a char boundary).
    pub cursor: usize,
    pub prompt_history: Vec<String>,
    pub history_cursor: Option<usize>,
    pub draft_backup: String,
}

impl ComposerState {
    pub fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.history_cursor = None;
    }

    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.history_cursor = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.text.drain(prev..self.cursor);
        self.cursor = prev;
        self.history_cursor = None;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(off, _)| self.cursor + off)
            .unwrap_or(self.text.len());
        self.text.drain(self.cursor..next);
        self.history_cursor = None;
    }

    pub fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let before = &self.text[..self.cursor];
        let trimmed_len = before.trim_end().len();
        let cut = before[..trimmed_len]
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        self.text.drain(cut..self.cursor);
        self.cursor = cut;
        self.history_cursor = None;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_cursor = None;
    }

    pub fn set_text(&mut self, s: impl Into<String>) {
        self.text = s.into();
        self.cursor = self.text.len();
    }

    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
    }

    pub fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        self.cursor = self.text[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(off, _)| self.cursor + off)
            .unwrap_or(self.text.len());
    }

    pub fn move_home(&mut self) {
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.cursor = line_start;
    }

    pub fn move_end(&mut self) {
        let line_end = self.text[self.cursor..]
            .find('\n')
            .map(|off| self.cursor + off)
            .unwrap_or(self.text.len());
        self.cursor = line_end;
    }

    pub fn is_on_first_line(&self) -> bool {
        !self.text[..self.cursor].contains('\n')
    }

    pub fn is_on_last_line(&self) -> bool {
        !self.text[self.cursor..].contains('\n')
    }

    pub fn history_prev(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }
        match self.history_cursor {
            None => {
                self.draft_backup = self.text.clone();
                let idx = self.prompt_history.len() - 1;
                self.history_cursor = Some(idx);
                self.text = self.prompt_history[idx].clone();
                self.cursor = self.text.len();
            }
            Some(idx) if idx > 0 => {
                let next = idx - 1;
                self.history_cursor = Some(next);
                self.text = self.prompt_history[next].clone();
                self.cursor = self.text.len();
            }
            _ => {}
        }
    }

    pub fn history_next(&mut self) {
        let Some(idx) = self.history_cursor else {
            return;
        };
        if idx + 1 < self.prompt_history.len() {
            let next = idx + 1;
            self.history_cursor = Some(next);
            self.text = self.prompt_history[next].clone();
            self.cursor = self.text.len();
        } else {
            self.history_cursor = None;
            self.text = std::mem::take(&mut self.draft_backup);
            self.cursor = self.text.len();
        }
    }

    pub fn move_up_line(&mut self) -> bool {
        let before = &self.text[..self.cursor];
        let Some(cur_line_start) = before.rfind('\n').map(|i| i + 1) else {
            return false;
        };
        let col = before[cur_line_start..].chars().count();
        let prev_end = cur_line_start - 1;
        let prev_start = self.text[..prev_end]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let target_byte = self.text[prev_start..prev_end]
            .char_indices()
            .nth(col)
            .map(|(off, _)| prev_start + off)
            .unwrap_or(prev_end);
        self.cursor = target_byte;
        true
    }

    pub fn move_down_line(&mut self) -> bool {
        let Some(next_nl_off) = self.text[self.cursor..].find('\n') else {
            return false;
        };
        let cur_line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let col = self.text[cur_line_start..self.cursor].chars().count();
        let next_start = self.cursor + next_nl_off + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map(|off| next_start + off)
            .unwrap_or(self.text.len());
        let target_byte = self.text[next_start..next_end]
            .char_indices()
            .nth(col)
            .map(|(off, _)| next_start + off)
            .unwrap_or(next_end);
        self.cursor = target_byte;
        true
    }
}

/// One tab shown in the top session bar.
#[derive(Debug, Clone)]
pub struct SessionTab {
    pub id: String,
    pub title: String,
}

/// Hit-test region for mouse clicks in the top bar.
#[derive(Debug, Clone)]
pub enum TabHitTarget {
    OverflowButton,
    Session(String),
    NewSession,
}

#[derive(Debug, Clone)]
pub struct TabHitBox {
    pub x_start: u16,
    pub x_end: u16,
    pub target: TabHitTarget,
}

/// Per-turn metadata stored alongside `TurnRecord` for rendering OpenCode-style footers
/// (`Build · Model · 2.2s · 5.6 tok/s`).
#[derive(Debug, Clone)]
pub struct TurnMeta {
    pub mode: String,
    pub model: String,
    pub output_tokens: Option<u32>,
}

/// Actions offered inside the `ctrl+p` Command Palette.
#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub title: String,
    pub shortcut: String,
    pub category: String,
    pub action: PaletteAction,
}

#[derive(Debug, Clone)]
pub enum PaletteAction {
    NewSession,
    SwitchSession,
    SwitchModel,
    CycleAgentMode,
    SelectThinking,
    ToggleProcessDetails,
    ToggleTodoPanel,
    CompactContext,
    ShowDiff,
    ShowUsage,
    ShowBackgroundJobs,
    ShowHelp,
    SlashCommand(String),
    Quit,
}

/// Provider entry shown in the first step of `/models` / `/model`.
#[derive(Debug, Clone)]
pub struct ProviderPickerItem {
    pub id: String,
    pub display_name: String,
    pub tag: String,
    pub is_current: bool,
}

/// Model entry shown in the Model Picker modal.
#[derive(Debug, Clone)]
pub struct ModelPickerItem {
    pub provider: String,
    pub model: String,
    pub display_name: String,
    pub is_current: bool,
}

/// Modal overlay currently owning keyboard input.
pub enum ActiveModal {
    None,
    CommandPalette {
        query: String,
        selected: usize,
    },
    SessionPicker {
        query: String,
        sessions: Vec<SessionRecord>,
        selected: usize,
    },
    ProviderPicker {
        query: String,
        items: Vec<ProviderPickerItem>,
        selected: usize,
    },
    ModelPicker {
        provider: String,
        provider_display: String,
        query: String,
        items: Vec<ModelPickerItem>,
        selected: usize,
    },
    ThinkingPicker {
        current: Option<ReasoningEffort>,
        selected: usize,
    },
    Approval {
        req: ApprovalRequest,
        resp_tx: oneshot::Sender<ApprovalDecision>,
        selected: usize,
    },
    Clarify {
        req: ClarifyRequest,
        resp_tx: oneshot::Sender<ClarifyResponse>,
        question_idx: usize,
        answers: Vec<crate::commands::clarify::ClarifyAnswer>,
        selected: usize,
        custom: String,
        editing_custom: bool,
    },
    Sudo {
        prompt: String,
        resp_tx: oneshot::Sender<Option<String>>,
        input: String,
    },
    Help {
        scroll: usize,
    },
}

/// Autocomplete popup when typing `/` at the start of the composer.
#[derive(Debug, Clone, Default)]
pub struct SlashPopupState {
    pub dismissed: bool,
    pub selected: usize,
    pub matches: Vec<CommandEntry>,
}

/// All UI & session loop state for the TUI.
pub struct TuiState {
    pub turns: Vec<TurnRecord>,
    pub turn_meta: Vec<TurnMeta>,
    pub system_notices: Vec<String>,
    pub process_expanded: bool,
    pub mode: AgentMode,
    pub composer: ComposerState,
    pub tabs: Vec<SessionTab>,
    pub total_sessions_count: usize,
    pub hit_boxes: Vec<TabHitBox>,
    pub scroll_from_bottom: usize,
    pub max_scroll: usize,
    pub modal: ActiveModal,
    pub slash_popup: SlashPopupState,
    pub queue: VecDeque<String>,
    pub current_cancel: CancellationToken,
    pub active_turn: Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>>,
    pub active_goal: Option<harness_core::GoalRunState>,
    pub current_query: String,
    pub turn_start: std::time::Instant,
    pub quota_refreshed_at: std::time::Instant,
    pub idle_pass: IdleMemoryPass,
    pub pending_job_turn: Option<(String, std::time::Instant)>,
    pub pending_images: Arc<Mutex<Vec<String>>>,
    pub history_path: Option<std::path::PathBuf>,
    pub ctrl_c_armed: Option<std::time::Instant>,
    pub spinner_tick: usize,
    pub cwd_label: String,
    pub version: String,
    pub update_notice: Option<String>,
    pub last_rendered_rect: (u16, u16),
}
