//! State structures for the full-screen OpenCode-style TUI.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use harness_core::{
    ApprovalDecision, ApprovalRequest, HarnessError, InterruptHandle, ReasoningEffort,
    SessionRecord, TurnOutcome,
};
use ratatui::style::Color;
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

use super::theme::BG_SURFACE;

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
    SettingsThinking,
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

/// A half-block thumbnail decoded from a pasted image.
///
/// Each cell carries the colour of the pixel above it and the pixel below it, so the renderer can
/// paint two image rows with one terminal row (`▀` with a foreground and background colour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePreview {
    pub width: u16,
    pub height: u16,
    /// Row-major `(top, bottom)` colours, `width * height` entries.
    pub cells: Vec<(Color, Color)>,
}

impl ImagePreview {
    pub fn cell(&self, x: u16, y: u16) -> Option<(Color, Color)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells
            .get(y as usize * self.width as usize + x as usize)
            .copied()
    }

    /// Average the stored cells covered by destination cell `(x, y)` of a `dest_w × dest_h`
    /// thumbnail.
    ///
    /// The preview is decoded once at its maximum size and painted at whatever width the composer
    /// has, so a narrow card downsamples instead of cropping the right-hand edge off the image.
    pub fn sampled_cell(&self, x: u16, y: u16, dest_w: u16, dest_h: u16) -> Option<(Color, Color)> {
        if dest_w == 0 || dest_h == 0 || x >= dest_w || y >= dest_h {
            return None;
        }
        let (src_w, src_h) = (self.width as u32, self.height as u32);
        let (dest_w, dest_h) = (dest_w as u32, dest_h as u32);
        let x0 = (x as u32 * src_w / dest_w).min(src_w.saturating_sub(1));
        let x1 = (((x as u32 + 1) * src_w / dest_w).max(x0 + 1)).min(src_w);
        let y0 = (y as u32 * src_h / dest_h).min(src_h.saturating_sub(1));
        let y1 = (((y as u32 + 1) * src_h / dest_h).max(y0 + 1)).min(src_h);

        let mut top_sum = (0u32, 0u32, 0u32);
        let mut bottom_sum = (0u32, 0u32, 0u32);
        let mut count = 0u32;
        for sy in y0..y1 {
            for sx in x0..x1 {
                let Some((top, bottom)) = self.cell(sx as u16, sy as u16) else {
                    continue;
                };
                let top = rgb_of(top);
                let bottom = rgb_of(bottom);
                top_sum = (top_sum.0 + top.0, top_sum.1 + top.1, top_sum.2 + top.2);
                bottom_sum = (
                    bottom_sum.0 + bottom.0,
                    bottom_sum.1 + bottom.1,
                    bottom_sum.2 + bottom.2,
                );
                count += 1;
            }
        }
        if count == 0 {
            return None;
        }
        let average = |sum: (u32, u32, u32)| {
            Color::Rgb(
                (sum.0 / count) as u8,
                (sum.1 / count) as u8,
                (sum.2 / count) as u8,
            )
        };
        Some((average(top_sum), average(bottom_sum)))
    }
}

fn rgb_of(color: Color) -> (u32, u32, u32) {
    match color {
        Color::Rgb(r, g, b) => (r as u32, g as u32, b as u32),
        _ => (30, 30, 32),
    }
}

/// An attached image or collapsed multi-line paste represented by a compact token
/// (`[image: WxH]` or `[pasted ~N lines]`) in the composer input box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerAttachment {
    pub token: String,
    pub kind: AttachmentKind,
    /// Decoded thumbnail for image attachments, so the composer can show the image itself.
    pub preview: Option<ImagePreview>,
}

/// Thumbnail size cap, in terminal cells. Cells are roughly twice as tall as they are wide, so
/// twenty rows cover forty image rows. The preview is decoded once at this size and downsampled
/// when the composer is narrower, which keeps small pasted screenshots legible.
const PREVIEW_MAX_COLS: u32 = 72;
const PREVIEW_MAX_ROWS: u32 = 20;
/// Never blow a tiny image up more than this, so a 16×16 icon stays an icon.
const PREVIEW_MAX_UPSCALE: f64 = 4.0;

/// Decode a pasted image into a half-block thumbnail for the composer.
///
/// A missing or unsupported image is not an error: the attachment still carries its data URL and
/// the `[Image N]` token, it just renders as a token instead of a preview.
fn decode_image_preview(data_url: &str) -> Option<ImagePreview> {
    use base64::Engine;

    let (_, payload) = data_url.split_once(',')?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (image_w, image_h) = decoded.dimensions();
    if image_w == 0 || image_h == 0 {
        return None;
    }

    let max_w = PREVIEW_MAX_COLS;
    let max_h = PREVIEW_MAX_ROWS * 2;
    let scale = (max_w as f64 / image_w as f64)
        .min(max_h as f64 / image_h as f64)
        .min(PREVIEW_MAX_UPSCALE);
    let width = ((image_w as f64 * scale).round() as u32).clamp(1, max_w);
    let height = ((image_h as f64 * scale).round() as u32).clamp(1, max_h);
    let thumb = image::imageops::resize(
        &decoded,
        width,
        height,
        image::imageops::FilterType::CatmullRom,
    );

    let rows = height.div_ceil(2) as u16;
    let mut cells = Vec::with_capacity(width as usize * rows as usize);
    for row in 0..rows {
        let top_y = row as u32 * 2;
        let bottom_y = (top_y + 1).min(height - 1);
        for x in 0..width {
            let top = blend_over_surface(thumb.get_pixel(x, top_y).0);
            let bottom = blend_over_surface(thumb.get_pixel(x, bottom_y).0);
            cells.push((top, bottom));
        }
    }

    Some(ImagePreview {
        width: width as u16,
        height: rows,
        cells,
    })
}

/// Composite one RGBA pixel over the composer surface, which is what the cell background will be.
fn blend_over_surface(pixel: [u8; 4]) -> Color {
    let (bg_r, bg_g, bg_b) = match BG_SURFACE {
        Color::Rgb(r, g, b) => (r as u32, g as u32, b as u32),
        _ => (30, 30, 32),
    };
    let alpha = pixel[3] as u32;
    let mix = |channel: u8, background: u32| {
        ((channel as u32 * alpha + background * (255 - alpha)) / 255) as u8
    };
    Color::Rgb(
        mix(pixel[0], bg_r),
        mix(pixel[1], bg_g),
        mix(pixel[2], bg_b),
    )
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
        let preview = decode_image_preview(&data_url);
        self.attachments.push(ComposerAttachment {
            token,
            kind: AttachmentKind::ImageDataUrl(data_url),
            preview,
        });
    }

    /// Drop one attachment by token: the close button on an image preview calls this.
    pub fn remove_attachment(&mut self, token: &str) {
        let Some(start) = self.text.find(token) else {
            self.attachments.retain(|att| att.token != token);
            return;
        };
        let mut end = start + token.len();
        // The token is inserted with surrounding spaces; take one with it so removing a chip does
        // not leave a double space behind.
        if end < self.text.len() && self.text.as_bytes().get(end) == Some(&b' ') {
            end += 1;
        } else if start > 0 && self.text.as_bytes().get(start - 1) == Some(&b' ') {
            // `start - 1` is ASCII whitespace, so it is a char boundary.
            self.text.remove(start - 1);
            end -= 1;
        }
        self.text.replace_range(start..end, "");
        self.cursor = if self.cursor >= end {
            self.cursor - (end - start)
        } else if self.cursor > start {
            start
        } else {
            self.cursor
        };
        self.attachments.retain(|att| att.token != token);
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
            preview: None,
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

/// Clickable control inside the composer card (currently the close button on an image preview).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerHitBox {
    pub y: u16,
    pub x_start: u16,
    pub x_end: u16,
    /// Token of the attachment this control removes.
    pub token: String,
}

/// A text selection in the transcript.
///
/// Positions are absolute transcript coordinates: `line` is the index into the full transcript
/// (settled turns + notices + active turn), `column` is a terminal cell offset from the left edge
/// of the transcript viewport. Absolute line indices keep the highlight attached to the text when
/// new output arrives and the viewport shifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: (usize, usize),
    pub cursor: (usize, usize),
    /// True while the primary button is held and the pointer is being dragged.
    pub dragging: bool,
}

impl TextSelection {
    /// Anchor/cursor ordered top-to-bottom, left-to-right.
    pub fn bounds(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// True when the pointer moved far enough to be a drag rather than a click.
    pub fn is_range(&self) -> bool {
        self.anchor != self.cursor
    }
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
    pub width: usize,
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
        self.width = 0;
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

/// Data a picker modal asked for on a background task and is waiting to receive.
///
/// The modals open instantly with whatever is cached and the event loop fills them in when one of
/// these lands, so `/model` never stalls the loop on a credential file read or a `/models` HTTP
/// call.
#[derive(Debug, Clone)]
pub enum PickerLoad {
    Providers(Vec<ProviderPickerItem>),
    Models {
        provider: String,
        items: Vec<ModelPickerItem>,
    },
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
    fn text_selection_bounds_order_a_backwards_drag() {
        let forward = TextSelection {
            anchor: (2, 3),
            cursor: (4, 1),
            dragging: true,
        };
        let backwards = TextSelection {
            anchor: (4, 1),
            cursor: (2, 3),
            dragging: true,
        };
        assert_eq!(forward.bounds(), ((2, 3), (4, 1)));
        assert_eq!(backwards.bounds(), ((2, 3), (4, 1)));
        assert!(forward.is_range());
        assert!(!TextSelection {
            anchor: (2, 3),
            cursor: (2, 3),
            dragging: true,
        }
        .is_range());
    }

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

    fn png_data_url(width: u32, height: u32) -> String {
        use base64::Engine;

        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x * 9) as u8, (y * 17) as u8, 120, 255])
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode png");
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
        )
    }

    #[test]
    fn attached_image_decodes_into_a_half_block_preview() {
        let mut c = ComposerState::default();
        c.attach_image(32, 18, png_data_url(32, 18));

        let preview = c.attachments[0].preview.as_ref().expect("preview");
        assert!(preview.width > 0 && preview.width <= PREVIEW_MAX_COLS as u16);
        assert!(preview.height > 0 && preview.height <= PREVIEW_MAX_ROWS as u16);
        assert_eq!(
            preview.cells.len(),
            preview.width as usize * preview.height as usize
        );
        assert!(preview.cell(0, 0).is_some());
        assert!(preview.cell(preview.width, 0).is_none());
    }

    #[test]
    fn a_narrow_composer_downsamples_the_preview_instead_of_cropping_it() {
        let mut c = ComposerState::default();
        c.attach_image(32, 18, png_data_url(32, 18));
        let preview = c.attachments[0].preview.clone().expect("preview");
        let (dest_w, dest_h) = (preview.width / 2, preview.height / 2);

        let sampled = preview
            .sampled_cell(dest_w - 1, dest_h - 1, dest_w, dest_h)
            .expect("sampled cell");
        // The last destination cell must come from the bottom-right of the image, not fall off
        // the end of the source grid.
        assert!(matches!(sampled.0, ratatui::style::Color::Rgb(..)));
        assert!(preview.sampled_cell(dest_w, 0, dest_w, dest_h).is_none());
    }

    #[test]
    fn an_unreadable_image_data_url_still_attaches_as_a_token() {
        let mut c = ComposerState::default();
        c.attach_image(293, 400, "data:image/png;base64,AAA".to_string());

        assert_eq!(c.text, "[Image 1] ");
        assert!(c.attachments[0].preview.is_none());
    }

    #[test]
    fn remove_attachment_drops_the_token_preview_and_extra_space() {
        let mut c = ComposerState::default();
        c.insert_str("before ");
        c.attach_image(8, 8, png_data_url(8, 8));
        c.insert_str("after");
        let token = c.attachments[0].token.clone();

        c.remove_attachment(&token);

        assert_eq!(c.text, "before after");
        assert!(c.attachments.is_empty());
        assert_eq!(c.cursor, c.text.len());
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
    /// Collapse a thinking block as soon as the next tool call starts (mirrors
    /// `display.auto_hide_thinking`; the settings modal keeps it in sync).
    pub auto_hide_thinking: bool,
    pub expanded_thinking: HashSet<(usize, usize)>,
    pub expanded_tools: HashSet<(usize, usize)>,
    pub expanded_live_thinking: HashSet<usize>,
    pub expanded_live_tools: HashSet<usize>,
    pub live_thinking_view: LiveThinkingView,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Model choice waiting on the thinking-level picker that `/models` chains after it.
    pub pending_model_choice: Option<(ModelTarget, String, String)>,
    pub mode: AgentMode,
    pub composer: ComposerState,
    pub tabs: Vec<SessionTab>,
    pub total_sessions_count: usize,
    pub hit_boxes: Vec<TabHitBox>,
    pub transcript_hit_boxes: Vec<TranscriptHitBox>,
    /// Close buttons on composer image previews, rebuilt each draw.
    pub composer_hit_boxes: Vec<ComposerHitBox>,
    /// Latest pointer position in screen cells, used for hover-only controls.
    pub pointer: Option<(u16, u16)>,
    /// Last click timestamp and position for double/triple click detection.
    pub last_click: Option<(std::time::Instant, u16, u16)>,
    /// Sequential click counter (1 = single, 2 = double, 3 = triple).
    pub click_count: usize,
    /// Bounding rectangle of the open modal, if any.
    pub modal_rect: Option<ratatui::layout::Rect>,
    /// Bounding rectangle of the composer card `(x, y, width, height)`.
    pub composer_area: (u16, u16, u16, u16),
    /// Bounding rectangle of the composer text input area `(x, y, width, height)`.
    pub composer_input_area: (u16, u16, u16, u16),
    /// Vertical scroll offset of composer lines.
    pub composer_scroll_off: usize,
    /// Viewport of the transcript as `(x, y, width, height)` in screen cells, refreshed each draw.
    pub transcript_area: (u16, u16, u16, u16),
    /// Absolute index of the first transcript line visible in the viewport.
    pub transcript_start: usize,
    /// Plain text of each visible transcript line, aligned with `transcript_start`.
    pub transcript_visible_text: Vec<String>,
    /// Active mouse text selection, if any.
    pub selection: Option<TextSelection>,
    /// Click target armed on mouse-down; fired on mouse-up only when no drag happened.
    pub pending_transcript_click: Option<TranscriptClickTarget>,
    /// Short confirmation shown in the chrome after a selection is copied.
    pub selection_notice: Option<(String, std::time::Instant)>,
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
    /// Provider list shown by the first step of `/models`. Refreshed off-loop so the modal never
    /// waits on credential disk I/O.
    pub provider_items: Vec<ProviderPickerItem>,
    /// Provider whose model list is still being fetched, if a `ModelPicker` is open.
    pub picker_loading: Option<String>,
    /// Sender handed to picker background tasks.
    pub picker_tx: tokio::sync::mpsc::UnboundedSender<PickerLoad>,
    /// Receiver polled by the event loop to fill picker modals in place.
    pub picker_rx: tokio::sync::mpsc::UnboundedReceiver<PickerLoad>,
    pub settled_cache: SettledTranscriptCache,
    pub live_content_cache: LiveContentCache,
    pub live_reasoning_cache: LiveReasoningCache,
    /// Unlocked clone-only agent template used to instantiate independent per-session agents
    /// without waiting on a running turn's `env.agent` lock.
    pub template_agent: Arc<ChatAgent>,
    /// Other sessions kept live in memory (including any running background turns).
    pub background_sessions: HashMap<String, BackgroundSession>,
}
