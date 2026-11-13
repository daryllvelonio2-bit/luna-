//! State structures for the full-screen OpenCode-style TUI.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use harness_core::{
    ApprovalDecision, ApprovalRequest, HarnessError, InterruptHandle, ReasoningEffort,
    SessionRecord, TurnOutcome,
};
use ratatui::text::Line;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::super::commands::CommandEntry;
use super::super::helpers::IdleMemoryPass;
use super::super::render::{TurnProcessItem, TurnRecord};
use super::super::turn::InterruptNotice;
use super::super::ui::ChatUi;
use super::super::ChatAgent;
use crate::commands::clarify::{ClarifyRequest, ClarifyResponse};

/// Agent execution mode toggled by `Shift+Tab` (shown in coral/accent on composer footer).
/// Which model slot a provider → model picker is choosing for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelTarget {
    /// The live session model (`/model`).
    #[default]
    Session,
    /// The persisted default model (`/default_model`). `global` is the scope the caller already
    /// supplied; `None` means ask with a follow-up picker.
    Default { global: Option<bool> },
    /// The compaction model (`/compact-model`).
    Compact { global: Option<bool> },
    /// The background memory saver's model (`/bgsaver-model`).
    BgSaver,
    /// A named worker's model (`/worker <name>`).
    Worker(String),
}

/// A choice a generic TUI select modal hands back to the action layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectAction {
    SettingsMain,
    SettingsBlockLimit,
    SettingsCollapse,
    DirScope {
        path: String,
    },
    ModelScope {
        target: ModelTarget,
        provider: String,
        model: String,
    },
    ConnectMenu,
    ConnectAddProvider {
        ids: Vec<String>,
    },
    ConnectRemoveProvider {
        ids: Vec<String>,
    },
    ConnectRemoveCredential {
        provider: String,
        ids: Vec<String>,
    },
    ConnectPriorityProvider {
        ids: Vec<String>,
    },
    ConnectPriorityCredential {
        provider: String,
        ids: Vec<String>,
    },
    ConnectStrategyProvider {
        ids: Vec<String>,
    },
    ConnectStrategy {
        provider: String,
    },
    ConnectResetCooldowns {
        ids: Vec<String>,
    },
    ProfileDeleteConfirm {
        name: String,
    },
    ProfileCreateProviders {
        name: String,
        summary: String,
    },
    TaskList {
        tasks: Vec<(String, String)>,
    },
}

/// Text a generic TUI input modal hands back to the action layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    DirPath {
        /// `Some(true)` persists globally, `Some(false)` is session-only, and `None`
        /// asks for the scope in a follow-up picker.
        scope: Option<bool>,
    },
    ApiKeyLabel {
        provider: String,
    },
    ApiKeySecret {
        provider: String,
        label: String,
    },
    AutoCompactThreshold,
    AutoCompactPercent {
        threshold: usize,
    },
    ProfileCreateName,
    ProfileCreateSummary {
        name: String,
    },
}

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

/// Payload backed by an inline placeholder token inside [`ComposerState::text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentKind {
    ImageDataUrl(String),
    PastedText(String),
}

/// An attached image or collapsed multi-line paste represented by a compact token
/// (`[image: WxH]` or `[pasted ~N lines]`) in the composer input box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerAttachment {
    pub token: String,
    pub kind: AttachmentKind,
}

/// Result of taking the composer contents for submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedComposer {
    /// Text as shown in the composer / transcript card (keeps `[image: ...]` and `[pasted ~N lines]`).
    pub display_text: String,
    /// Full prompt sent to the model (`[pasted ~N lines]` expanded to full pasted text).
    pub expanded_text: String,
    /// Image data URLs to attach to the turn in order of appearance.
    pub image_data_urls: Vec<String>,
}

/// Multi-line text composer state with cursor & history navigation.
#[derive(Debug, Clone, Default)]
pub struct ComposerState {
    pub text: String,
    /// Byte index inside `text` (always on a char boundary).
    pub cursor: usize,
    pub attachments: Vec<ComposerAttachment>,
    pub prompt_history: Vec<String>,
    pub history_cursor: Option<usize>,
    pub draft_backup: String,
    pub draft_attachments_backup: Vec<ComposerAttachment>,
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

    /// Insert an inline image token `[image: WxH]` at the cursor and track its data URL.
    pub fn attach_image(&mut self, _width: usize, _height: usize, data_url: String) {
        let image_num = self.image_count() + 1;
        let base = format!("[Image {image_num}]");
        let token = self.unique_token(&base);
        self.insert_token_with_spacing(&token);
        self.attachments.push(ComposerAttachment {
            token,
            kind: AttachmentKind::ImageDataUrl(data_url),
        });
    }

    /// Paste text into the composer, collapsing multi-line or large copied blocks into
    /// `[pasted ~N lines]` while keeping short single-line pastes inline.
    pub fn paste_text(&mut self, raw: &str) {
        let cleaned = raw.replace("\r\n", "\n").replace('\r', "\n");
        let trimmed_nl = cleaned.trim_end_matches('\n');
        let newline_count = trimmed_nl.matches('\n').count();
        let is_block = newline_count >= 1 || cleaned.len() > 200;
        if !is_block {
            self.insert_str(&cleaned);
            return;
        }
        let line_count = (newline_count + 1).max(cleaned.len().div_ceil(100));
        let base = format!("[pasted ~{line_count} lines]");
        let token = self.unique_token(&base);
        self.insert_token_with_spacing(&token);
        self.attachments.push(ComposerAttachment {
            token,
            kind: AttachmentKind::PastedText(cleaned),
        });
    }

    pub fn image_count(&self) -> usize {
        self.attachments
            .iter()
            .filter(|a| matches!(a.kind, AttachmentKind::ImageDataUrl(_)))
            .count()
    }

    fn unique_token(&self, base: &str) -> String {
        if !self.text.contains(base) && !self.attachments.iter().any(|a| a.token == base) {
            return base.to_string();
        }
        let stem = base.strip_suffix(']').unwrap_or(base);
        let mut n = 2usize;
        loop {
            let cand = format!("{stem} #{n}]");
            if !self.text.contains(&cand) && !self.attachments.iter().any(|a| a.token == cand) {
                return cand;
            }
            n += 1;
        }
    }

    fn insert_token_with_spacing(&mut self, token: &str) {
        let need_leading_space = self.cursor > 0
            && self.text[..self.cursor]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace());
        let need_trailing_space = self.cursor == self.text.len()
            || self.text[self.cursor..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace());
        let mut snippet = String::with_capacity(token.len() + 2);
        if need_leading_space {
            snippet.push(' ');
        }
        snippet.push_str(token);
        if need_trailing_space {
            snippet.push(' ');
        }
        self.insert_str(&snippet);
    }

    fn attachment_range_for_backspace(&self) -> Option<(usize, usize)> {
        if self.cursor == 0 {
            return None;
        }
        for att in &self.attachments {
            if let Some(start) = self.text.find(&att.token) {
                let end = start + att.token.len();
                if self.cursor > start && self.cursor <= end {
                    return Some((start, end));
                }
            }
        }
        None
    }

    fn attachment_range_for_delete(&self) -> Option<(usize, usize)> {
        for att in &self.attachments {
            if let Some(start) = self.text.find(&att.token) {
                let end = start + att.token.len();
                if self.cursor >= start && self.cursor < end {
                    return Some((start, end));
                }
            }
        }
        None
    }

    fn prune_missing_attachments(&mut self) {
        self.attachments
            .retain(|att| self.text.contains(&att.token));
    }

    /// Take and expand the composer for submission, clearing the input state.
    pub fn take_for_submit(&mut self) -> SubmittedComposer {
        let raw = std::mem::take(&mut self.text);
        let attachments = std::mem::take(&mut self.attachments);
        self.clear();

        let display_text = raw.trim().to_string();
        let mut spans: Vec<(usize, usize, &ComposerAttachment)> = attachments
            .iter()
            .filter_map(|att| raw.find(&att.token).map(|s| (s, s + att.token.len(), att)))
            .collect();
        spans.sort_by_key(|(s, _, _)| *s);

        let mut expanded = String::with_capacity(raw.len());
        let mut image_data_urls = Vec::new();
        let mut pos = 0usize;
        for (start, end, att) in spans {
            if start < pos {
                continue;
            }
            expanded.push_str(&raw[pos..start]);
            match &att.kind {
                AttachmentKind::PastedText(full) => {
                    expanded.push_str(full);
                }
                AttachmentKind::ImageDataUrl(url) => {
                    expanded.push_str(&raw[start..end]);
                    image_data_urls.push(url.clone());
                }
            }
            pos = end;
        }
        expanded.push_str(&raw[pos..]);

        SubmittedComposer {
            display_text,
            expanded_text: expanded.trim().to_string(),
            image_data_urls,
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some((start, end)) = self.attachment_range_for_backspace() {
            self.text.drain(start..end);
            self.cursor = start;
            self.history_cursor = None;
            self.prune_missing_attachments();
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
        self.prune_missing_attachments();
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        if let Some((start, end)) = self.attachment_range_for_delete() {
            self.text.drain(start..end);
            self.cursor = start;
            self.history_cursor = None;
            self.prune_missing_attachments();
            return;
        }
        let next = self.text[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(off, _)| self.cursor + off)
            .unwrap_or(self.text.len());
        self.text.drain(self.cursor..next);
        self.history_cursor = None;
        self.prune_missing_attachments();
    }

    pub fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some((start, end)) = self.attachment_range_for_backspace() {
            self.text.drain(start..end);
            self.cursor = start;
            self.history_cursor = None;
            self.prune_missing_attachments();
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
        self.prune_missing_attachments();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.attachments.clear();
        self.history_cursor = None;
    }

    pub fn set_text(&mut self, s: impl Into<String>) {
        self.text = s.into();
        self.cursor = self.text.len();
        self.prune_missing_attachments();
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
                self.draft_attachments_backup = std::mem::take(&mut self.attachments);
                let idx = self.prompt_history.len() - 1;
                self.history_cursor = Some(idx);
                self.text = self.prompt_history[idx].clone();
                self.cursor = self.text.len();
            }
            Some(idx) if idx > 0 => {
                let next = idx - 1;
                self.history_cursor = Some(next);
                self.attachments.clear();
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
            self.attachments.clear();
            self.text = self.prompt_history[next].clone();
            self.cursor = self.text.len();
        } else {
            self.history_cursor = None;
            self.text = std::mem::take(&mut self.draft_backup);
            self.attachments = std::mem::take(&mut self.draft_attachments_backup);
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

/// Clickable target inside the rendered transcript viewport.
#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum TranscriptClickTarget {
    ToggleTurnThinking(usize, usize),
    ToggleTurnTools(usize, usize),
    ToggleLiveThinkingStep(usize),
    ToggleLiveToolStep(usize),
    ToggleLiveThinking,
    JumpToLatest,
}

#[derive(Debug, Clone)]
pub struct TranscriptHitBox {
    pub y: u16,
    pub x_start: u16,
    pub x_end: u16,
    pub target: TranscriptClickTarget,
}

/// Display mode for the live streaming thinking block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiveThinkingView {
    #[default]
    Tail,
    Expanded,
    Collapsed,
}

/// Cached rendered lines for settled turns in the transcript.
#[derive(Debug, Default, Clone)]
pub struct SettledTranscriptCache {
    pub session_id: String,
    pub card_inner_w: usize,
    pub turns_len: usize,
    pub process_expanded: bool,
    pub expanded_thinking: HashSet<(usize, usize)>,
    pub expanded_tools: HashSet<(usize, usize)>,
    pub lines: Vec<Line<'static>>,
    pub hit_targets: Vec<(usize, TranscriptClickTarget)>,
}

impl SettledTranscriptCache {
    pub fn clear(&mut self) {
        self.session_id.clear();
        self.card_inner_w = 0;
        self.turns_len = 0;
        self.lines.clear();
        self.hit_targets.clear();
    }
}

/// Cached rendered markdown lines for active streaming reply text.
#[derive(Debug, Default, Clone)]
pub struct LiveContentCache {
    pub content: String,
    pub card_inner_w: usize,
    pub lines: Vec<Line<'static>>,
}

/// Cached rendered markdown lines for active live reasoning.
#[derive(Debug, Default, Clone)]
pub struct LiveReasoningCache {
    pub reasoning: String,
    pub card_inner_w: usize,
    pub lines: Vec<Line<'static>>,
}

/// Parked state for a session that is not currently displayed (and may still be running a turn).
pub struct BackgroundSession {
    pub session_id: String,
    pub agent: Arc<tokio::sync::Mutex<ChatAgent>>,
    pub ui: Arc<ChatUi>,
    pub in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>>,
    pub interruptions: Arc<Mutex<Vec<InterruptNotice>>>,
    pub steer_handle: Arc<Mutex<VecDeque<String>>>,
    pub interrupt_handle: Arc<InterruptHandle>,
    pub compacting: Arc<AtomicBool>,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub mode: AgentMode,
    pub turns: Vec<TurnRecord>,
    pub turn_meta: Vec<TurnMeta>,
    pub settled_cache: SettledTranscriptCache,
    pub system_notices: Vec<String>,
    pub expanded_thinking: HashSet<(usize, usize)>,
    pub expanded_tools: HashSet<(usize, usize)>,
    pub expanded_live_thinking: HashSet<usize>,
    pub expanded_live_tools: HashSet<usize>,
    pub live_thinking_view: LiveThinkingView,
    pub process_expanded: bool,
    pub scroll_from_bottom: usize,
    pub queue: VecDeque<String>,
    pub current_cancel: CancellationToken,
    pub active_turn: Option<tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>>,
    pub active_goal: Option<harness_core::GoalRunState>,
    pub current_query: String,
    pub turn_start: std::time::Instant,
    pub idle_pass: IdleMemoryPass,
    pub pending_images: Arc<Mutex<Vec<String>>>,
    pub pending_job_turn: Option<(String, std::time::Instant)>,
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
        target: ModelTarget,
        query: String,
        items: Vec<ProviderPickerItem>,
        selected: usize,
    },
    ModelPicker {
        target: ModelTarget,
        provider: String,
        provider_display: String,
        query: String,
        items: Vec<ModelPickerItem>,
        selected: usize,
    },
    SelectModal {
        title: String,
        items: Vec<String>,
        selected: usize,
        action: SelectAction,
    },
    InputModal {
        title: String,
        prompt: String,
        value: String,
        secret: bool,
        action: InputAction,
    },
    TextModal {
        title: String,
        lines: Vec<String>,
        scroll: usize,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_image_inserts_marker_and_submits_data_url() {
        let mut c = ComposerState::default();
        c.insert_str("check");
        c.attach_image(293, 400, "data:image/png;base64,AAA".to_string());
        assert_eq!(c.text, "check [Image 1] ");
        assert_eq!(c.image_count(), 1);

        c.insert_str("please");
        let sub = c.take_for_submit();
        assert_eq!(sub.display_text, "check [Image 1] please");
        assert_eq!(sub.expanded_text, "check [Image 1] please");
        assert_eq!(sub.image_data_urls, vec!["data:image/png;base64,AAA"]);
        assert!(c.text.is_empty());
        assert_eq!(c.image_count(), 0);
    }

    #[test]
    fn paste_text_collapses_multiline_blocks_and_expands_on_submit() {
        let mut c = ComposerState::default();
        c.insert_str("explain ");
        c.paste_text("line 1\nline 2\nline 3\n");
        assert_eq!(c.text, "explain [pasted ~3 lines] ");

        let sub = c.take_for_submit();
        assert_eq!(sub.display_text, "explain [pasted ~3 lines]");
        assert_eq!(sub.expanded_text, "explain line 1\nline 2\nline 3");
        assert!(sub.image_data_urls.is_empty());
    }

    #[test]
    fn paste_text_keeps_short_single_line_inline() {
        let mut c = ComposerState::default();
        c.paste_text("cargo test -p harness-cli");
        assert_eq!(c.text, "cargo test -p harness-cli");
        assert!(c.attachments.is_empty());
    }

    #[test]
    fn backspace_and_delete_remove_attachment_tokens_atomically() {
        let mut c = ComposerState::default();
        c.attach_image(293, 400, "data:image/png;base64,AAA".to_string());
        assert_eq!(c.text, "[Image 1] ");
        // First backspace deletes trailing space, second deletes whole token.
        c.backspace();
        assert_eq!(c.text, "[Image 1]");
        assert_eq!(c.image_count(), 1);
        c.backspace();
        assert_eq!(c.text, "");
        assert_eq!(c.image_count(), 0);

        c.paste_text("a\nb\nc");
        c.move_home();
        c.delete();
        assert_eq!(c.text, " ");
        assert!(c.attachments.is_empty());
    }
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
    pub expanded_thinking: HashSet<(usize, usize)>,
    pub expanded_tools: HashSet<(usize, usize)>,
    pub expanded_live_thinking: HashSet<usize>,
    pub expanded_live_tools: HashSet<usize>,
    pub live_thinking_view: LiveThinkingView,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub mode: AgentMode,
    pub composer: ComposerState,
    pub tabs: Vec<SessionTab>,
    pub total_sessions_count: usize,
    pub hit_boxes: Vec<TabHitBox>,
    pub transcript_hit_boxes: Vec<TranscriptHitBox>,
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
    pub settled_cache: SettledTranscriptCache,
    pub live_content_cache: LiveContentCache,
    pub live_reasoning_cache: LiveReasoningCache,
    /// Unlocked clone-only agent template used to instantiate independent per-session agents
    /// without waiting on a running turn's `env.agent` lock.
    pub template_agent: Arc<ChatAgent>,
    /// Other sessions kept live in memory (including any running background turns).
    pub background_sessions: HashMap<String, BackgroundSession>,
}
