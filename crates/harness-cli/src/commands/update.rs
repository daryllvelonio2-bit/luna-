//! The launch-time build check, its popup, and the in-place update.
//!
//! `luna` binaries are published by the `release-build` GitHub Actions workflow, so "is there a new
//! build?" is "does the newest successful run of that workflow point at a different commit than the
//! one this binary was built from?" The commit is baked in by `build.rs`.
//!
//! The answer is asked *before* the chat exists, as a centred modal: the first thing on screen is
//! the question, with the transcript untouched behind it. Three answers are offered:
//!
//! - **Update now** downloads the artifact that run already produced and swaps it in beside the
//!   running binary, keeping the modal up with staged progress and a byte-counted bar. The running
//!   process keeps its old inode; the next launch starts the new build.
//! - **Remind me on the next update** records this commit as skipped, so an even newer build asks
//!   again but this one does not.
//! - **Later** asks again on the next launch.
//!
//! Two rules shape the rest: a launch must not wait on the network (the check gives up after
//! [`CHECK_TIMEOUT`] and any failure means no popup), and an install must never corrupt the
//! running binary (the write goes to a sibling and is renamed into place).

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use colored::Colorize;
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use harness_core::config::{self, Config};

use super::chat::keys::{self, AppKey};
use super::chat::modal::{paint_centred, ModalScreen};
use super::chat::ui::get_terminal_width;

/// The commit `build.rs` stamped into this binary, or `unknown` without git metadata.
pub const BUILD_SHA: &str = env!("LUNA_BUILD_SHA");

/// A launch check may not feel like a stall. Two seconds is longer than a healthy request and
/// shorter than the point where the user notices the screen has not settled.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);
/// The artifact is tens of megabytes; the download must not inherit the launch timeout.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Cache beside `config.yaml`.
const CACHE_FILE: &str = "update-check.json";
const USER_AGENT: &str = "luna-update-check/1.0";
const GITHUB_ACCEPT: &str = "application/vnd.github+json";
/// GitHub renders a commit short-sha best at seven; the same cut is used here and in the popup.
const SHORT_SHA: usize = 8;

/// The three answers the popup offers, in focus order.
const ACTIONS: [&str; 3] = [
    "[u] Update now",
    "[r] Remind me on the next update",
    "[l] Later",
];
/// "Later" is focused first: a stray Enter must not start a download.
const DEFAULT_ACTION: usize = 2;

/// What the launch question produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// The new build is on disk. The caller prints the last line and exits.
    Installed(PathBuf),
    /// The user asked for an update and it failed; the modal already showed why.
    Failed(String),
    /// This build was recorded as skipped.
    Remind,
    /// Nothing was recorded; the question returns on the next launch.
    Later,
}

/// The three answers, kept separate from [`LaunchOutcome`] because two of them have no result
/// beyond what the popup already showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Update,
    Remind,
    Later,
}

/// What a successful check found.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateStatus {
    pub current: String,
    /// The running build in `luna --version` form, e.g. `0.1.4 (6d5107c4)`.
    pub current_version: String,
    pub latest: String,
    /// The package version of the newer commit, or this build's version if it could not be read.
    pub latest_version: String,
    /// The workflow run that produced `latest`; `None` when it came from an older cache format.
    pub run_id: Option<u64>,
}

impl UpdateStatus {
    /// What the build in hand calls itself.
    pub fn current_label(&self) -> &str {
        &self.current_version
    }

    /// What the newer build calls itself: its manifest version plus the commit that carries it.
    pub fn latest_label(&self) -> String {
        format!("{} ({})", self.latest_version, short(&self.latest))
    }
}

/// The running build as `luna --version` prints it.
const CURRENT_DISPLAY: &str = env!("LUNA_VERSION");
/// Used when the newer commit's manifest cannot be fetched; the package version changes rarely.
const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Reading one small file from raw.githubusercontent should never hold up the question.
const VERSION_TIMEOUT: Duration = Duration::from_millis(800);

// ---------------------------------------------------------------- launch check

/// Ask GitHub whether a newer build exists. `None` means "do not show anything": checking is off,
/// the binary has no commit, the request failed, or the user chose to skip this build.
pub async fn check_at_launch(config: &Config) -> Option<UpdateStatus> {
    if !config.update.check || BUILD_SHA == "unknown" {
        return None;
    }
    let cache = cache_path();
    let now = now_secs();
    let ttl = config.update.check_interval_hours.saturating_mul(60 * 60);
    let previous = read_cache(&cache);
    let fresh = previous
        .as_ref()
        .filter(|entry| !entry.latest.is_empty() && now.saturating_sub(entry.checked_at) < ttl);

    let (latest, latest_version, run_id, skipped) = match fresh {
        Some(entry) => (
            entry.latest.clone(),
            entry.latest_version.clone(),
            entry.run_id,
            entry.skipped.clone(),
        ),
        None => {
            let build = fetch_latest(&config.update).await?;
            let skipped = previous.as_ref().and_then(|entry| entry.skipped.clone());
            write_cache(
                &cache,
                &CacheEntry {
                    checked_at: now,
                    latest: build.sha.clone(),
                    // Filled in below, and only when the question is actually going to be asked.
                    latest_version: String::new(),
                    run_id: Some(build.run_id),
                    skipped: skipped.clone(),
                },
            );
            (build.sha, String::new(), Some(build.run_id), skipped)
        }
    };

    if latest == BUILD_SHA || skipped.as_deref() == Some(latest.as_str()) {
        return None;
    }

    // Only a launch that is about to show the question pays for this; a normal launch never does.
    // The answer is written back to the cache so repeated "Later"s do not refetch it.
    let latest_version = match latest_version.is_empty() {
        false => latest_version,
        true => {
            let fetched = fetch_remote_version(&config.update, &latest).await;
            if let Some(version) = &fetched {
                if let Some(mut entry) = read_cache(&cache).filter(|entry| entry.latest == latest) {
                    entry.latest_version = version.clone();
                    write_cache(&cache, &entry);
                }
            }
            fetched.unwrap_or_else(|| PACKAGE_VERSION.to_string())
        }
    };

    Some(UpdateStatus {
        current: BUILD_SHA.to_string(),
        current_version: CURRENT_DISPLAY.to_string(),
        latest,
        latest_version,
        run_id,
    })
}

/// Remember that this build is not wanted; the popup returns only when an even newer one appears.
pub fn remind(status: &UpdateStatus) {
    let path = cache_path();
    let mut entry = read_cache(&path).unwrap_or_default();
    entry.checked_at = now_secs();
    entry.latest = status.latest.clone();
    entry.latest_version = status.latest_version.clone();
    entry.run_id = status.run_id.or(entry.run_id);
    entry.skipped = Some(status.latest.clone());
    write_cache(&path, &entry);
}

// ---------------------------------------------------------------- popup

/// Ask the question and, if the answer is Update, keep the modal up while it runs.
///
/// The progress belongs on the same alternate screen as the question: the install is the only part
/// of a launch that can take tens of seconds, and giving the screen back first leaves the user
/// staring at a blank terminal with no evidence that anything is happening.
pub async fn ask_and_install(status: &UpdateStatus, config: &Config) -> LaunchOutcome {
    let Some(_raw) = PopupTerminal::enter() else {
        return LaunchOutcome::Later;
    };
    let _screen = ModalScreen::enter();

    match read_choice(status) {
        Choice::Later => LaunchOutcome::Later,
        Choice::Remind => {
            remind(status);
            LaunchOutcome::Remind
        }
        Choice::Update => {
            let progress = ProgressUi::start(status);
            let result = install(status, config, &progress).await;
            progress.finish();
            match result {
                Ok(path) => {
                    show_result(&ResultView::Installed(&path));
                    LaunchOutcome::Installed(path)
                }
                Err(error) => {
                    show_result(&ResultView::Failed(&error));
                    LaunchOutcome::Failed(error)
                }
            }
        }
    }
    // `_screen` drops before `_raw` (reverse declaration order): the alternate buffer is left while
    // still in raw mode, then raw mode is released, so a later `println!` is not translated by a
    // terminal that has not settled yet.
}

/// The question itself: focus, keys, repaint on resize.
fn read_choice(status: &UpdateStatus) -> Choice {
    let mut selected = DEFAULT_ACTION;
    let mut width = get_terminal_width().saturating_sub(2);
    paint_centred(&render_update_box(status, selected, width));

    loop {
        let key = match keys::read_key() {
            Ok(key) => key,
            // A pause or a signal interrupted the read; the popup owns the terminal here, so
            // simply ask again.
            Err(_) => continue,
        };
        let chosen = match key {
            AppKey::Up | AppKey::Left | AppKey::BackTab => {
                selected = (selected + ACTIONS.len() - 1) % ACTIONS.len();
                None
            }
            AppKey::Down | AppKey::Right | AppKey::Tab => {
                selected = (selected + 1) % ACTIONS.len();
                None
            }
            AppKey::Char('u') | AppKey::Char('U') | AppKey::Char('1') => Some(Choice::Update),
            AppKey::Char('r') | AppKey::Char('R') | AppKey::Char('2') => Some(Choice::Remind),
            AppKey::Char('l') | AppKey::Char('L') | AppKey::Char('3') => Some(Choice::Later),
            AppKey::Enter | AppKey::Char(' ') => Some(match selected {
                0 => Choice::Update,
                1 => Choice::Remind,
                _ => Choice::Later,
            }),
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => Some(Choice::Later),
            AppKey::Resize => {
                width = get_terminal_width().saturating_sub(2);
                paint_centred(&render_update_box(status, selected, width));
                continue;
            }
            _ => continue,
        };
        if let Some(choice) = chosen {
            return choice;
        }
        paint_centred(&render_update_box(status, selected, width));
    }
}

/// Hold the result on screen until a key is pressed.
///
/// Without this the alt screen is torn down the instant the rename finishes and the only evidence
/// of a successful install is one line in the scrollback the user never saw appear. Success and
/// failure both wait here, so neither path can drop the user back to a blank terminal.
fn show_result(view: &ResultView<'_>) {
    let mut width = get_terminal_width().saturating_sub(2);
    paint_centred(&render_result_box(view, width));
    loop {
        match keys::read_key() {
            Ok(AppKey::Resize) => {
                width = get_terminal_width().saturating_sub(2);
                paint_centred(&render_result_box(view, width));
            }
            Ok(_) => return,
            Err(_) => continue,
        }
    }
}

/// Raw mode for the popup alone. Unlike the chat's `RawTerminal`, this one lets crossterm disable
/// signals too, so Ctrl-C is a key the popup can answer with "Later" and clean up from.
struct PopupTerminal;

impl PopupTerminal {
    fn enter() -> Option<Self> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        crossterm::terminal::enable_raw_mode().ok().map(|_| Self)
    }
}

impl Drop for PopupTerminal {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn render_update_box(status: &UpdateStatus, selected: usize, width: usize) -> String {
    let total = width.clamp(32, 76);
    let visible_len = |s: &str| dialoguer::console::strip_ansi_codes(s).chars().count();
    let cut = |s: &str, room: usize| -> String {
        if visible_len(s) > room {
            format!(
                "{}…",
                dialoguer::console::strip_ansi_codes(s)
                    .chars()
                    .take(room.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            s.to_string()
        }
    };

    let row = |content: &str| -> String {
        let max_content = total.saturating_sub(4);
        let truncated = cut(content, max_content);
        let pad = max_content.saturating_sub(visible_len(&truncated));
        format!("│ {}{} │", truncated, " ".repeat(pad))
    };
    let blank = row("");
    let inner_row = |payload: &str| -> String {
        let max_inner = total.saturating_sub(10);
        let truncated = cut(payload, max_inner);
        let pad = max_inner.saturating_sub(visible_len(&truncated));
        row(&format!(" │ {}{} │ ", truncated, " ".repeat(pad)))
    };
    let inner_top = |label: &str| -> String {
        let dashes = total.saturating_sub(label.len() + 11);
        row(&format!(" ┌─ {} {}┐ ", label.cyan(), "─".repeat(dashes)))
    };
    let inner_bottom =
        || -> String { row(&format!(" └{}┘ ", "─".repeat(total.saturating_sub(8)))) };

    let mut lines = Vec::new();
    let title = "Update available";
    let dashes = total.saturating_sub(title.len() + 5);
    lines.push(format!("╭─ {} {}╮", title.bold(), "─".repeat(dashes)));

    lines.push(blank.clone());
    let lead = if total >= 48 {
        " A newer build of Luna is ready to install."
    } else {
        " New build ready."
    };
    lines.push(row(lead));
    lines.push(blank.clone());

    lines.push(inner_top("Build"));
    lines.push(inner_row(&format!(
        "{} {} → {}",
        "running".dimmed(),
        status.current_label().bold(),
        status.latest_label().cyan().bold()
    )));
    lines.push(inner_bottom());
    lines.push(blank.clone());

    lines.push(row(&format!(" {}", "Action".dimmed())));
    for (index, action) in ACTIONS.iter().enumerate() {
        if index == selected {
            lines.push(row(&format!(" {} {}", "❯".cyan().bold(), action.bold())));
        } else {
            lines.push(row(&format!("   {}", action.dimmed())));
        }
    }
    lines.push(blank.clone());
    lines.push(row(&format!(
        " {}",
        "↑/↓ selects • Enter confirms • Esc later".dimmed()
    )));
    lines.push(format!("╰{}╯", "─".repeat(total.saturating_sub(2))));

    lines.join("\n") + "\n"
}

// ---------------------------------------------------------------- progress

/// The install steps, in the order the modal reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Preparing,
    Locating,
    Downloading,
    Extracting,
    Installing,
    Done,
}

/// The stages shown as a checklist. `Done` is a terminal marker, not a row.
const STAGES: [Stage; 5] = [
    Stage::Preparing,
    Stage::Locating,
    Stage::Downloading,
    Stage::Extracting,
    Stage::Installing,
];

impl Stage {
    fn rank(self) -> usize {
        match self {
            Stage::Preparing => 0,
            Stage::Locating => 1,
            Stage::Downloading => 2,
            Stage::Extracting => 3,
            Stage::Installing => 4,
            Stage::Done => 5,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Stage::Preparing => "Checking credentials",
            Stage::Locating => "Finding the release build",
            Stage::Downloading => "Downloading the build",
            Stage::Extracting => "Extracting the binary",
            Stage::Installing => "Installing beside the running copy",
            Stage::Done => "Done",
        }
    }
}

#[derive(Debug, Clone)]
struct ProgressState {
    stage: Stage,
    downloaded: u64,
    total: Option<u64>,
    /// Advances once per painted frame; only the spinner and the indeterminate bar read it.
    tick: usize,
}

#[derive(Debug, Clone)]
struct ProgressHeader {
    current: String,
    latest: String,
}

impl ProgressHeader {
    fn new(status: &UpdateStatus) -> Self {
        Self {
            current: status.current_label().to_string(),
            latest: status.latest_label(),
        }
    }
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PAINT_INTERVAL: Duration = Duration::from_millis(90);

/// Repaints the progress box while an install runs.
///
/// Painting lives on its own thread so the spinner keeps moving through steps that are one blocking
/// call (zip extraction, the final rename). The state mutex is held across the whole
/// render-and-write, which keeps two painters from emitting halves of a box out of order.
struct ProgressUi {
    state: Arc<Mutex<ProgressState>>,
    stop: Arc<AtomicBool>,
    header: ProgressHeader,
    painter: Option<JoinHandle<()>>,
}

impl ProgressUi {
    fn start(status: &UpdateStatus) -> Self {
        let state = Arc::new(Mutex::new(ProgressState {
            stage: Stage::Preparing,
            downloaded: 0,
            total: None,
            tick: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let header = ProgressHeader::new(status);

        // The first frame goes out before the first await, so pressing `u` changes the screen at
        // once instead of after the token lookup.
        paint_progress(&header, &state);

        let painter = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            let header = header.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(PAINT_INTERVAL);
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut state = state.lock().unwrap();
                    state.tick = state.tick.wrapping_add(1);
                    paint_centred(&render_progress_box(
                        &header,
                        &state,
                        get_terminal_width().saturating_sub(2),
                    ));
                }
            })
        };

        Self {
            state,
            stop,
            header,
            painter: Some(painter),
        }
    }

    fn set_stage(&self, stage: Stage) {
        let mut state = self.state.lock().unwrap();
        state.stage = stage;
        if stage == Stage::Downloading {
            state.downloaded = 0;
            state.total = None;
        }
        paint_centred(&render_progress_box(
            &self.header,
            &state,
            get_terminal_width().saturating_sub(2),
        ));
    }

    fn set_bytes(&self, downloaded: u64, total: Option<u64>) {
        let mut state = self.state.lock().unwrap();
        state.downloaded = downloaded;
        state.total = total;
    }

    /// Stop the animator and wait for its last frame, so no paint can race the result box.
    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(painter) = self.painter.take() {
            let _ = painter.join();
        }
    }
}

fn paint_progress(header: &ProgressHeader, state: &Mutex<ProgressState>) {
    let state = state.lock().unwrap();
    paint_centred(&render_progress_box(
        header,
        &state,
        get_terminal_width().saturating_sub(2),
    ));
}

fn render_progress_box(header: &ProgressHeader, state: &ProgressState, width: usize) -> String {
    let total = width.clamp(32, 76);
    let title = "Updating Luna".bold();
    let mut lines = Vec::new();
    lines.push(format!(
        "╭─ {} {}╮",
        title,
        "─".repeat(total.saturating_sub(visible_len(&title) + 5))
    ));

    lines.push(box_blank(total));
    lines.push(box_row(
        total,
        &format!(
            " {} → {}",
            header.current.bold(),
            header.latest.cyan().bold()
        ),
    ));
    lines.push(box_blank(total));

    let panel = match state.stage {
        Stage::Downloading => "Download",
        Stage::Done => "Complete",
        _ => "Progress",
    };
    lines.push(box_inner_top(total, panel));
    lines.push(box_inner_row(total, &progress_line(total, state)));
    lines.push(box_inner_bottom(total));
    lines.push(box_blank(total));

    for stage in STAGES {
        let done = state.stage == Stage::Done || stage.rank() < state.stage.rank();
        let current = stage == state.stage;
        let mark = if done {
            "✔".green().bold().to_string()
        } else if current {
            SPINNER[state.tick % SPINNER.len()]
                .cyan()
                .bold()
                .to_string()
        } else {
            "·".dimmed().to_string()
        };
        let label = if current && !done {
            stage.label().bold().to_string()
        } else if done {
            stage.label().to_string()
        } else {
            stage.label().dimmed().to_string()
        };
        lines.push(box_row(total, &format!(" {} {}", mark, label)));
    }

    lines.push(format!("╰{}╯", "─".repeat(total.saturating_sub(2))));
    lines.join("\n") + "\n"
}

/// The bar panel: a determinate bar while bytes are arriving, an indeterminate sweep otherwise.
fn progress_line(total: usize, state: &ProgressState) -> String {
    let inner = total.saturating_sub(10);
    let percent = state
        .total
        .filter(|total| *total > 0)
        .map(|total| ((state.downloaded.min(total) as u128 * 100) / total as u128) as usize);

    let meta = if state.stage == Stage::Downloading {
        match (percent, inner) {
            (Some(percent), width) if width >= 44 => match state.total {
                Some(total) => format!(
                    "{percent:>3}%  {} / {}",
                    human_bytes(state.downloaded),
                    human_bytes(total)
                ),
                None => format!("{percent:>3}%  {}", human_bytes(state.downloaded)),
            },
            (Some(percent), width) if width >= 28 => {
                format!("{percent:>3}%  {}", human_bytes(state.downloaded))
            }
            (Some(percent), _) => format!("{percent:>3}%"),
            (None, width) if width >= 28 => human_bytes(state.downloaded),
            _ => "…".to_string(),
        }
    } else {
        state.stage.label().to_string()
    };

    let gap = if meta.is_empty() { 0 } else { 2 };
    let bar_width = inner.saturating_sub(visible_len(&meta) + gap).max(6);
    let bar = render_bar(
        bar_width,
        (state.stage == Stage::Downloading)
            .then_some(percent)
            .flatten(),
        state.tick,
        state.stage == Stage::Done,
    );
    format!("{bar}{}{meta}", " ".repeat(gap))
}

fn render_bar(width: usize, percent: Option<usize>, tick: usize, complete: bool) -> String {
    let width = width.max(1);
    if complete {
        return "█".repeat(width).green().bold().to_string();
    }
    match percent {
        Some(percent) => {
            let filled = (width * percent.min(100)) / 100;
            format!(
                "{}{}",
                "█".repeat(filled).cyan().bold(),
                "░".repeat(width - filled).dimmed()
            )
        }
        None => {
            // A block that sweeps left to right, so a step without a byte count still moves.
            let span = (width / 3).max(3).min(width);
            let position = (tick * 2) % (width + span);
            let mut bar = String::with_capacity(width);
            for cell in 0..width {
                let filled = cell < position && cell + span >= position;
                bar.push(if filled { '█' } else { '░' });
            }
            bar.cyan().bold().to_string()
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------- result

#[derive(Clone, Copy)]
enum ResultView<'a> {
    Installed(&'a Path),
    Failed(&'a str),
}

fn render_result_box(view: &ResultView<'_>, width: usize) -> String {
    let total = width.clamp(32, 76);
    let (title, ok) = match view {
        ResultView::Installed(_) => ("Update installed".green().bold().to_string(), true),
        ResultView::Failed(_) => ("Update failed".red().bold().to_string(), false),
    };

    let mut lines = Vec::new();
    lines.push(format!(
        "╭─ {} {}╮",
        title,
        "─".repeat(total.saturating_sub(visible_len(&title) + 5))
    ));
    lines.push(box_blank(total));

    match view {
        ResultView::Installed(path) => {
            push_wrapped(
                &mut lines,
                total,
                &format!("{} ", "✔".green().bold()),
                &format!(
                    "The new build is installed at {}.",
                    path.display().to_string().bold()
                ),
            );
            lines.push(box_blank(total));
            push_wrapped(
                &mut lines,
                total,
                &format!("{} ", "→".cyan().bold()),
                "Luna exits now. The new build starts the next time you launch it.",
            );
        }
        ResultView::Failed(error) => {
            push_wrapped(&mut lines, total, &format!("{} ", "✖".red().bold()), error);
            lines.push(box_blank(total));
            push_wrapped(
                &mut lines,
                total,
                &format!("{} ", "•".dimmed()),
                "Nothing on disk was changed — the build you are running is still the current one.",
            );
        }
    }

    lines.push(box_blank(total));
    let hint = if ok {
        "Press any key to finish."
    } else {
        "Press any key to continue without updating."
    };
    lines.push(box_row(total, &format!(" {}", hint.bold())));
    lines.push(format!("╰{}╯", "─".repeat(total.saturating_sub(2))));
    lines.join("\n") + "\n"
}

// ---------------------------------------------------------------- box helpers

fn visible_len(text: &str) -> usize {
    dialoguer::console::strip_ansi_codes(text).chars().count()
}

fn truncate_visible(text: &str, room: usize) -> String {
    if visible_len(text) <= room {
        return text.to_string();
    }
    let plain = dialoguer::console::strip_ansi_codes(text);
    let cut: String = plain.chars().take(room.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn box_row(total: usize, content: &str) -> String {
    let max = total.saturating_sub(4);
    let truncated = truncate_visible(content, max);
    let pad = max.saturating_sub(visible_len(&truncated));
    format!("│ {}{} │", truncated, " ".repeat(pad))
}

fn box_blank(total: usize) -> String {
    box_row(total, "")
}

fn box_inner_top(total: usize, label: &str) -> String {
    let dashes = total.saturating_sub(visible_len(label) + 11);
    box_row(
        total,
        &format!(" ┌─ {} {}┐ ", label.cyan(), "─".repeat(dashes)),
    )
}

fn box_inner_row(total: usize, content: &str) -> String {
    let max_inner = total.saturating_sub(10);
    let truncated = truncate_visible(content, max_inner);
    let pad = max_inner.saturating_sub(visible_len(&truncated));
    box_row(total, &format!(" │ {}{} │ ", truncated, " ".repeat(pad)))
}

fn box_inner_bottom(total: usize) -> String {
    box_row(
        total,
        &format!(" └{}┘ ", "─".repeat(total.saturating_sub(8))),
    )
}

/// A paragraph as full box rows, wrapping long text and hanging continuation lines under it.
fn push_wrapped(lines: &mut Vec<String>, total: usize, prefix: &str, text: &str) {
    let prefix_width = visible_len(prefix);
    let text_width = total.saturating_sub(4).saturating_sub(prefix_width).max(8);
    for (index, line) in wrap_words(text, text_width).into_iter().enumerate() {
        let lead = if index == 0 {
            prefix.to_string()
        } else {
            " ".repeat(prefix_width)
        };
        lines.push(box_row(total, &format!("{lead}{line}")));
    }
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_len = visible_len(word);
        if current.is_empty() {
            if word_len > width {
                lines.push(truncate_visible(word, width));
            } else {
                current.push_str(word);
            }
        } else if visible_len(&current) + 1 + word_len <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            if word_len > width {
                lines.push(truncate_visible(word, width));
            } else {
                current.push_str(word);
            }
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

// ---------------------------------------------------------------- install

/// Download the artifact of the run the check found and replace the running binary.
///
/// The write is staged beside the target and renamed into place, so a failed download or a full
/// disk leaves the current binary untouched. The running process keeps the old inode and the next
/// launch gets the new one. Every step reports through `progress`, which owns the modal screen.
async fn install(
    status: &UpdateStatus,
    config: &Config,
    progress: &ProgressUi,
) -> Result<PathBuf, String> {
    progress.set_stage(Stage::Preparing);
    let token = github_token()?;
    progress.set_stage(Stage::Locating);
    let run_id = match status.run_id {
        Some(id) => id,
        None => fetch_latest(&config.update)
            .await
            .map(|build| build.run_id)
            .ok_or_else(|| "could not find the release workflow run".to_string())?,
    };

    // Redirects are followed by hand for the download: GitHub answers an artifact URL with a
    // pre-signed Blob URL, and forwarding the API token to that host makes it reject the request.
    let client = reqwest::Client::builder()
        .timeout(INSTALL_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("could not create the download client: {error}"))?;

    let artifacts_url = format!(
        "https://api.github.com/repos/{}/actions/runs/{run_id}/artifacts",
        config.update.repo
    );
    let artifacts: serde_json::Value = client
        .get(artifacts_url)
        .bearer_auth(&token)
        .header("Accept", GITHUB_ACCEPT)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|error| format!("could not list workflow artifacts: {error}"))?
        .error_for_status()
        .map_err(|error| format!("GitHub refused the artifact listing: {error}"))?
        .json()
        .await
        .map_err(|error| format!("could not read the artifact listing: {error}"))?;

    let download_url = artifacts
        .get("artifacts")
        .and_then(|value| value.as_array())
        .and_then(|artifacts| {
            artifacts.iter().find(|artifact| {
                artifact.get("name").and_then(|name| name.as_str())
                    == Some(config.update.artifact.as_str())
            })
        })
        .and_then(|artifact| artifact.get("archive_download_url"))
        .and_then(|url| url.as_str())
        .ok_or_else(|| format!("run {run_id} has no '{}' artifact", config.update.artifact))?;

    let response = client
        .get(download_url)
        .bearer_auth(&token)
        .header("Accept", GITHUB_ACCEPT)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|error| format!("could not download the artifact: {error}"))?;
    let response = match response.status().is_redirection() {
        true => {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "artifact download redirect had no location".to_string())?
                .to_string();
            client
                .get(location)
                .header("User-Agent", USER_AGENT)
                .send()
                .await
                .map_err(|error| format!("could not download the artifact: {error}"))?
        }
        false => response,
    };
    let mut response = response
        .error_for_status()
        .map_err(|error| format!("GitHub refused the artifact download: {error}"))?;

    // Read the body chunk by chunk rather than in one `bytes()` call: the byte count is the
    // difference between a progress bar that is accurate and one that only appears once the whole
    // artifact is already in memory. GitHub usually sends a `Content-Length`; when it does not, the
    // bar falls back to a sweep and the byte count still climbs.
    progress.set_stage(Stage::Downloading);
    let total = response.content_length();
    progress.set_bytes(0, total);
    let mut downloaded: u64 = 0;
    let mut bytes = Vec::with_capacity(total.unwrap_or(0) as usize);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("could not read the artifact: {error}"))?
    {
        downloaded += chunk.len() as u64;
        bytes.extend_from_slice(&chunk);
        progress.set_bytes(downloaded, total);
    }

    progress.set_stage(Stage::Extracting);
    let binary = extract_binary(&bytes)?;
    progress.set_stage(Stage::Installing);
    install_binary(&binary)
}

fn extract_binary(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|error| format!("artifact is not a zip: {error}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("could not read artifact entry: {error}"))?;
        let name = entry.name().to_string();
        if name == "luna" || name.ends_with("/luna") {
            let mut out = Vec::new();
            entry
                .read_to_end(&mut out)
                .map_err(|error| format!("could not read the luna binary: {error}"))?;
            return Ok(out);
        }
    }
    Err("the artifact did not contain a luna binary".to_string())
}

fn install_binary(binary: &[u8]) -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot find the running luna binary: {error}"))?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let name = exe
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "luna".to_string());
    let staging = exe.with_file_name(format!("{name}.new"));

    std::fs::write(&staging, binary)
        .map_err(|error| format!("could not write {}: {error}", staging.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("could not make the new build executable: {error}"))?;
    }
    replace_binary(&exe, &staging)?;
    Ok(exe)
}

#[cfg(unix)]
fn replace_binary(target: &Path, staging: &Path) -> Result<(), String> {
    std::fs::rename(staging, target)
        .map_err(|error| format!("could not replace {}: {error}", target.display()))
}

#[cfg(windows)]
fn replace_binary(target: &Path, staging: &Path) -> Result<(), String> {
    // A running executable cannot be overwritten on Windows, but it can be renamed aside.
    let old = target.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(target, &old)
        .map_err(|error| format!("could not move the old build aside: {error}"))?;
    std::fs::rename(staging, target)
        .map_err(|error| format!("could not install the new build: {error}"))?;
    let _ = std::fs::remove_file(&old);
    Ok(())
}

fn github_token() -> Result<String, String> {
    for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(value.to_string());
            }
        }
    }
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|_| "no GitHub token: set GITHUB_TOKEN or run `gh auth login`".to_string())?;
    if !output.status.success() {
        return Err("no GitHub token: set GITHUB_TOKEN or run `gh auth login`".to_string());
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err("no GitHub token: set GITHUB_TOKEN or run `gh auth login`".to_string());
    }
    Ok(token)
}

// ---------------------------------------------------------------- GitHub and cache

struct LatestBuild {
    sha: String,
    run_id: u64,
}

/// The newest successful run of the release workflow.
async fn fetch_latest(settings: &config::UpdateSettings) -> Option<LatestBuild> {
    let url = format!(
        "https://api.github.com/repos/{}/actions/workflows/{}/runs?branch={}&status=success&per_page=1",
        settings.repo, settings.workflow, settings.branch
    );
    let client = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .build()
        .ok()?;
    let request = client
        .get(url)
        .header("Accept", GITHUB_ACCEPT)
        .header("User-Agent", USER_AGENT);

    let response = tokio::time::timeout(CHECK_TIMEOUT, request.send())
        .await
        .ok()?
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: serde_json::Value = response.json().await.ok()?;
    let run = body.get("workflow_runs")?.get(0)?;
    Some(LatestBuild {
        sha: run.get("head_sha")?.as_str()?.to_string(),
        run_id: run.get("id")?.as_u64()?,
    })
}

/// Read the newer commit's package version from its workspace manifest.
///
/// This is best-effort and deliberately short: it only runs when the update question is about to be
/// shown, and a timeout simply falls back to this build's version. The commit is pinned, so the
/// answer is stable for that build.
async fn fetch_remote_version(settings: &config::UpdateSettings, sha: &str) -> Option<String> {
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/Cargo.toml",
        settings.repo, sha
    );
    let client = reqwest::Client::builder()
        .timeout(VERSION_TIMEOUT)
        .build()
        .ok()?;
    let response = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let manifest = response.text().await.ok()?;
    parse_workspace_version(&manifest)
}

/// The `version` under `[workspace.package]`, which is what `CARGO_PKG_VERSION` resolves to for the
/// crates that inherit it. Other `version` keys (dependencies, `[package]`) are ignored.
fn parse_workspace_version(manifest: &str) -> Option<String> {
    let mut in_workspace_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_workspace_package = line == "[workspace.package]";
            continue;
        }
        if !in_workspace_package {
            continue;
        }
        let Some(rest) = line.strip_prefix("version") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = rest.trim().trim_matches('"');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheEntry {
    /// Unix seconds when GitHub answered. `u64` because a clock before the epoch is not a reason
    /// to fail a launch.
    #[serde(default)]
    checked_at: u64,
    #[serde(default)]
    latest: String,
    /// The newer commit's package version, learned lazily when the question is shown.
    #[serde(default)]
    latest_version: String,
    /// The workflow run that produced `latest`, for the artifact download.
    #[serde(default)]
    run_id: Option<u64>,
    /// A build the user asked not to be reminded about again.
    #[serde(default)]
    skipped: Option<String>,
}

fn cache_path() -> PathBuf {
    config::config_file_path()
        .parent()
        .map(|dir| dir.join(CACHE_FILE))
        .unwrap_or_else(|| PathBuf::from(CACHE_FILE))
}

fn read_cache(path: &Path) -> Option<CacheEntry> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_cache(path: &Path, entry: &CacheEntry) {
    if let Ok(raw) = serde_json::to_string(entry) {
        let _ = std::fs::write(path, raw);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

pub(crate) fn short(sha: &str) -> &str {
    match sha.char_indices().nth(SHORT_SHA) {
        Some((index, _)) => &sha[..index],
        None => sha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sha_cuts_at_eight_characters() {
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
    }

    fn status() -> UpdateStatus {
        UpdateStatus {
            current: "1111111111111111".to_string(),
            current_version: "0.1.4 (11111111)".to_string(),
            latest: "2222222222222222".to_string(),
            latest_version: "0.1.4".to_string(),
            run_id: Some(7),
        }
    }

    #[test]
    fn the_popup_names_both_builds_and_every_action() {
        let frame = render_update_box(&status(), DEFAULT_ACTION, 76);
        assert!(frame.contains("Update available"));
        assert!(frame.contains("0.1.4 (11111111)"));
        assert!(frame.contains("0.1.4 (22222222)"));
        for action in ACTIONS {
            assert!(frame.contains(action), "missing {action}: {frame}");
        }
    }

    #[test]
    fn the_workspace_version_is_parsed_from_the_manifest() {
        assert_eq!(
            parse_workspace_version("[workspace.package]\nversion = \"1.2.3\"\n"),
            Some("1.2.3".to_string())
        );
        assert_eq!(
            parse_workspace_version(
                "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.9.0\"\n"
            ),
            Some("0.9.0".to_string())
        );
        assert_eq!(
            parse_workspace_version("[package]\nversion = \"9.9.9\"\n"),
            None
        );
        assert_eq!(parse_workspace_version(""), None);
    }

    #[test]
    fn every_popup_row_has_the_same_width() {
        for width in [20, 32, 44, 76, 120] {
            for selected in 0..ACTIONS.len() {
                let frame = render_update_box(&status(), selected, width);
                let widths: Vec<usize> = frame
                    .lines()
                    .map(|line| dialoguer::console::strip_ansi_codes(line).chars().count())
                    .collect();
                assert!(
                    widths.iter().all(|value| *value == widths[0]),
                    "widths at width={width}, selected={selected}: {widths:?}"
                );
                assert!(widths[0] <= width.clamp(32, 76));
            }
        }
    }

    #[test]
    fn later_is_the_default_focus() {
        let frame = render_update_box(&status(), DEFAULT_ACTION, 76);
        assert!(frame.contains("❯ [l] Later"), "{frame}");
    }

    fn progress(stage: Stage, downloaded: u64, total: Option<u64>) -> ProgressState {
        ProgressState {
            stage,
            downloaded,
            total,
            tick: 7,
        }
    }

    fn plain(frame: &str) -> String {
        dialoguer::console::strip_ansi_codes(frame).into_owned()
    }

    fn rows(frame: &str) -> Vec<usize> {
        frame
            .lines()
            .map(|line| dialoguer::console::strip_ansi_codes(line).chars().count())
            .collect()
    }

    #[test]
    fn the_progress_box_shows_percent_and_bytes_when_the_total_is_known() {
        let header = ProgressHeader::new(&status());
        let frame = render_progress_box(
            &header,
            &progress(Stage::Downloading, 1024 * 1024, Some(4 * 1024 * 1024)),
            76,
        );
        let frame = plain(&frame);

        assert!(frame.contains("25%"), "{frame}");
        assert!(frame.contains("1.0 MB"), "{frame}");
        assert!(frame.contains("4.0 MB"), "{frame}");
        for stage in STAGES {
            assert!(
                frame.contains(stage.label()),
                "missing {}: {frame}",
                stage.label()
            );
        }
    }

    #[test]
    fn the_progress_box_animates_stages_without_a_byte_count() {
        let header = ProgressHeader::new(&status());
        let first = render_progress_box(&header, &progress(Stage::Extracting, 0, None), 76);
        let second = render_progress_box(
            &header,
            &ProgressState {
                tick: 9,
                ..progress(Stage::Extracting, 0, None)
            },
            76,
        );
        assert!(plain(&first).contains("Extracting the binary"));
        assert_ne!(plain(&first), plain(&second), "the indeterminate bar moves");
    }

    #[test]
    fn every_progress_row_has_the_same_width() {
        let header = ProgressHeader::new(&status());
        for width in [20, 32, 44, 76, 120] {
            for state in [
                progress(Stage::Preparing, 0, None),
                progress(Stage::Downloading, 512 * 1024, Some(2 * 1024 * 1024)),
                progress(Stage::Downloading, 999, None),
                progress(Stage::Extracting, 0, None),
                progress(Stage::Done, 0, None),
            ] {
                let widths = rows(&render_progress_box(&header, &state, width));
                assert!(
                    widths.iter().all(|value| *value == widths[0]),
                    "widths at width={width}, stage={:?}: {widths:?}",
                    state.stage
                );
                assert!(widths[0] <= width.clamp(32, 76));
            }
        }
    }

    #[test]
    fn human_bytes_stays_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn result_boxes_hold_success_and_failure_without_overflow() {
        let path = Path::new("/home/u/.local/bin/luna");
        let failure = "could not download the artifact: connection reset by peer";
        for width in [20, 32, 44, 76, 120] {
            for view in [ResultView::Installed(path), ResultView::Failed(failure)] {
                let frame = render_result_box(&view, width);
                let widths = rows(&frame);
                assert!(
                    widths.iter().all(|value| *value == widths[0]),
                    "widths at width={width}: {widths:?}"
                );
                assert!(widths[0] <= width.clamp(32, 76));
                let text = plain(&frame);
                assert!(text.contains("Press any key"), "{text}");
            }
        }
        assert!(plain(&render_result_box(&ResultView::Installed(path), 76))
            .contains("Update installed"));
        assert!(
            plain(&render_result_box(&ResultView::Failed(failure), 76)).contains("Update failed")
        );
    }
}
