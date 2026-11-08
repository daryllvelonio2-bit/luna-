//! The launch-time build check.
//!
//! `luna` binaries are published by the `release-build` GitHub Actions workflow, so "is there a new
//! build?" is "does the newest successful run of that workflow point at a different commit than the
//! one this binary was built from?" The commit is baked in by `build.rs`.
//!
//! Two rules shape the rest:
//!
//! - A launch must not wait on the network. The check runs concurrently with the chat's setup, is
//!   awaited only once the screen is ready, and gives up after [`CHECK_TIMEOUT`].
//! - A failure is silent. No token, no repository access, no DNS, an offline laptop — all mean
//!   "no notice", never an error in front of the composer.
//!
//! The answer is cached next to the config file, so a launch usually costs nothing.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use colored::Colorize;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use harness_core::config::{self, Config};

/// The commit `build.rs` stamped into this binary, or `unknown` without git metadata.
pub const BUILD_SHA: &str = env!("LUNA_BUILD_SHA");

/// A launch check may not feel like a stall. Two seconds is longer than a healthy request and
/// shorter than the point where the user notices the screen has not settled.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Cache beside `config.yaml`.
const CACHE_FILE: &str = "update-check.json";
const USER_AGENT: &str = "luna-update-check/1.0";
/// GitHub renders a commit short-sha best at seven; the same cut is used here and in the notice.
const SHORT_SHA: usize = 8;

/// What a successful check found.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: String,
}

impl UpdateStatus {
    /// The one line the chat prints above the composer.
    pub fn notice(&self) -> String {
        format!(
            "{} New build available: {} → {}. Update with `scripts/auto_build_install.py`.",
            "↑".yellow().bold(),
            short(&self.current).dimmed(),
            short(&self.latest).cyan().bold(),
        )
    }
}

/// Start the check without waiting for it. `None` when checking is off or the build has no commit.
pub fn start(config: &Config) -> Option<JoinHandle<Option<UpdateStatus>>> {
    if !config.update.check || BUILD_SHA == "unknown" {
        return None;
    }
    let settings = config.update.clone();
    let cache = cache_path();
    Some(tokio::spawn(async move { check(&settings, &cache).await }))
}

/// Await a started check. The timeout is inside the request, so this is bounded.
pub async fn finish(handle: JoinHandle<Option<UpdateStatus>>) -> Option<UpdateStatus> {
    handle.await.ok().flatten()
}

async fn check(settings: &config::UpdateSettings, cache: &Path) -> Option<UpdateStatus> {
    let now = now_secs();
    let ttl = settings.check_interval_hours.saturating_mul(60 * 60);
    let cached = read_cache(cache).filter(|entry| now.saturating_sub(entry.checked_at) < ttl);

    let latest = match cached {
        Some(entry) => entry.latest,
        None => {
            let latest = fetch_latest(settings).await?;
            write_cache(cache, &latest, now);
            latest
        }
    };

    if latest == BUILD_SHA {
        return None;
    }
    Some(UpdateStatus {
        current: BUILD_SHA.to_string(),
        latest,
    })
}

/// The newest successful run of the release workflow, as a commit sha.
async fn fetch_latest(settings: &config::UpdateSettings) -> Option<String> {
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
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT);

    let response = tokio::time::timeout(CHECK_TIMEOUT, request.send())
        .await
        .ok()?
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: serde_json::Value = response.json().await.ok()?;
    body.get("workflow_runs")?
        .get(0)?
        .get("head_sha")?
        .as_str()
        .map(str::to_string)
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Unix seconds when GitHub answered. `u64` because a clock before the epoch is not a reason
    /// to fail a launch.
    checked_at: u64,
    latest: String,
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

fn write_cache(path: &Path, latest: &str, now: u64) {
    let entry = CacheEntry {
        checked_at: now,
        latest: latest.to_string(),
    };
    if let Ok(raw) = serde_json::to_string(&entry) {
        let _ = std::fs::write(path, raw);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn short(sha: &str) -> &str {
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

    #[test]
    fn notice_names_both_builds() {
        let status = UpdateStatus {
            current: "1111111111111111".to_string(),
            latest: "2222222222222222".to_string(),
        };
        let line = status.notice();
        assert!(line.contains("11111111"));
        assert!(line.contains("22222222"));
        assert!(line.contains("New build available"));
    }
}
