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
//!   running binary. The running process keeps its old inode; the next launch starts the new build.
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

/// What the user chose at launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchAction {
    Update,
    Remind,
    Later,
}

/// What a successful check found.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: String,
    /// The workflow run that produced `latest`; `None` when it came from an older cache format.
    pub run_id: Option<u64>,
}

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

    let (latest, run_id, skipped) = match fresh {
        Some(entry) => (entry.latest.clone(), entry.run_id, entry.skipped.clone()),
        None => {
            let build = fetch_latest(&config.update).await?;
            let skipped = previous.as_ref().and_then(|entry| entry.skipped.clone());
            write_cache(
                &cache,
                &CacheEntry {
                    checked_at: now,
                    latest: build.sha.clone(),
                    run_id: Some(build.run_id),
                    skipped: skipped.clone(),
                },
            );
            (build.sha, Some(build.run_id), skipped)
        }
    };

    if latest == BUILD_SHA || skipped.as_deref() == Some(latest.as_str()) {
        return None;
    }
    Some(UpdateStatus {
        current: BUILD_SHA.to_string(),
        latest,
        run_id,
    })
}

/// Remember that this build is not wanted; the popup returns only when an even newer one appears.
pub fn remind(status: &UpdateStatus) {
    let path = cache_path();
    let mut entry = read_cache(&path).unwrap_or_default();
    entry.checked_at = now_secs();
    entry.latest = status.latest.clone();
    entry.run_id = status.run_id.or(entry.run_id);
    entry.skipped = Some(status.latest.clone());
    write_cache(&path, &entry);
}

// ---------------------------------------------------------------- popup

/// Draw the update question centred, read one answer, and give the screen back.
///
/// Runs before the chat takes the terminal. Keys are read directly (there is no chat key reader
/// yet); raw mode is scoped to this popup so Ctrl-C arrives as a key and the alternate screen is
/// always given back.
pub fn prompt(status: &UpdateStatus) -> LaunchAction {
    let Some(_raw) = PopupTerminal::enter() else {
        return LaunchAction::Later;
    };
    let mut selected = DEFAULT_ACTION;
    let mut width = get_terminal_width().saturating_sub(2);
    let screen = ModalScreen::enter();
    paint_centred(&render_update_box(status, selected, width));

    let action = loop {
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
            AppKey::Char('u') | AppKey::Char('U') | AppKey::Char('1') => Some(LaunchAction::Update),
            AppKey::Char('r') | AppKey::Char('R') | AppKey::Char('2') => Some(LaunchAction::Remind),
            AppKey::Char('l') | AppKey::Char('L') | AppKey::Char('3') => Some(LaunchAction::Later),
            AppKey::Enter | AppKey::Char(' ') => Some(match selected {
                0 => LaunchAction::Update,
                1 => LaunchAction::Remind,
                _ => LaunchAction::Later,
            }),
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => Some(LaunchAction::Later),
            AppKey::Resize => {
                width = get_terminal_width().saturating_sub(2);
                paint_centred(&render_update_box(status, selected, width));
                continue;
            }
            _ => continue,
        };
        if let Some(action) = chosen {
            break action;
        }
        paint_centred(&render_update_box(status, selected, width));
    };

    drop(screen);
    action
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
        short(&status.current).bold(),
        short(&status.latest).cyan().bold()
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

// ---------------------------------------------------------------- install

/// Download the artifact of the run the check found and replace the running binary.
///
/// The write is staged beside the target and renamed into place, so a failed download or a full
/// disk leaves the current binary untouched. The running process keeps the old inode and the next
/// launch gets the new one.
pub async fn install(status: &UpdateStatus, config: &Config) -> Result<PathBuf, String> {
    let token = github_token()?;
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
    let bytes = response
        .error_for_status()
        .map_err(|error| format!("GitHub refused the artifact download: {error}"))?
        .bytes()
        .await
        .map_err(|error| format!("could not read the artifact: {error}"))?;

    let binary = extract_binary(&bytes)?;
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheEntry {
    /// Unix seconds when GitHub answered. `u64` because a clock before the epoch is not a reason
    /// to fail a launch.
    #[serde(default)]
    checked_at: u64,
    #[serde(default)]
    latest: String,
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
            latest: "2222222222222222".to_string(),
            run_id: Some(7),
        }
    }

    #[test]
    fn the_popup_names_both_builds_and_every_action() {
        let frame = render_update_box(&status(), DEFAULT_ACTION, 76);
        assert!(frame.contains("Update available"));
        assert!(frame.contains("11111111"));
        assert!(frame.contains("22222222"));
        for action in ACTIONS {
            assert!(frame.contains(action), "missing {action}: {frame}");
        }
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
}
