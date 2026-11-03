//! The status line's provider-specific half: who the account is, and what is left of the quota.
use std::sync::Arc;

use super::ui::ChatUi;
use harness_providers::{CredentialManager, QuotaLine, QuotaWindow};

/// How long a quota snapshot is believed.
///
/// The numbers move slowly — a bucket is a share of a five-hour or a weekly window — and each
/// refresh is two authenticated POSTs to the account's own endpoint, so one snapshot is worth
/// minutes. This is the interval the provider's own CLI samples its quota on.
pub(crate) const QUOTA_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// The account's quota windows as the status bar shows them:
/// `5h 82% (4h 45m) · w 100% (3d 15h)`.
///
/// The countdown is rendered here, against the clock at paint time, rather than stored when the
/// quota was fetched: the status bar repaints as a turn streams and again whenever a minute of a
/// countdown passes, and a number computed at fetch time would sit frozen between the two.
pub(crate) fn format_quota_lines(lines: &[QuotaLine]) -> String {
    let now = chrono::Utc::now();
    lines
        .iter()
        .map(|line| {
            let window = match line.window {
                QuotaWindow::FiveHour => "5h",
                QuotaWindow::Weekly => "w",
            };
            let percent = format!(
                "{:.0}%",
                (line.remaining_fraction.clamp(0.0, 1.0) * 100.0).round()
            );
            match line
                .reset_time
                .as_deref()
                .map(|reset| format_reset_time_remaining(reset, now))
                .filter(|remaining| !remaining.is_empty())
            {
                Some(remaining) => format!("{} {} {}", window, percent, remaining),
                None => format!("{} {}", window, percent),
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A reset time as the time left until it: `(4h 59m)`, `(3d 15h)`, `(0m)`.
fn format_reset_time_remaining(reset_time_str: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let Ok(reset) = chrono::DateTime::parse_from_rfc3339(reset_time_str) else {
        return String::new();
    };
    let total_secs = (reset.with_timezone(&chrono::Utc) - now).num_seconds();
    if total_secs <= 0 {
        return "(0m)".to_string();
    }
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let minutes = (total_secs % 3_600) / 60;
    if days > 0 {
        format!("({}d {}h)", days, hours)
    } else if hours > 0 {
        format!("({}h {}m)", hours, minutes)
    } else {
        format!("({}m)", minutes.max(1))
    }
}

/// Resolve the account and its quota, and show both.
///
/// The call sites are the ones that establish which provider a session is spending: start-up,
/// `/model`, and `/connect`. A running session is not one of them — see
/// [`refresh_antigravity_quota_owned`].
pub(crate) async fn refresh_antigravity_status_if_needed(
    ui: &ChatUi,
    current_provider: &str,
    current_model: &str,
    cred_manager: &Arc<CredentialManager>,
) {
    if !is_antigravity(current_provider) {
        ui.set_account_name(None);
        ui.set_quota_lines(current_model, Vec::new());
        return;
    }

    match primary_credential(cred_manager).await {
        Some((client, api_key, label)) => {
            // The account name follows the credential actually in use, so a label written down
            // once cannot outlive the key it described.
            let strict_email =
                harness_providers::resolve_antigravity_email_strict(&client, &api_key).await;
            let account_name = strict_email.unwrap_or_else(|| {
                if !label.is_empty() {
                    label
                } else {
                    "antigravity".to_string()
                }
            });
            ui.set_account_name(Some(account_name));
            fetch_quota_lines(ui, &client, &api_key, current_model, cred_manager).await;
        }
        None => {
            ui.set_account_name(None);
            ui.set_quota_lines(current_model, Vec::new());
        }
    }
}

/// The quota half on its own, owned so a task can run it.
///
/// The account name is a fact about the credential and does not change under a running session;
/// the numbers do, and a session can run for hours. The chat loop's tick spawns this, so a slow
/// quota endpoint is never something the composer waits behind.
pub(crate) async fn refresh_antigravity_quota_owned(
    ui: Arc<ChatUi>,
    provider: String,
    model: String,
    cred_manager: Arc<CredentialManager>,
) {
    if !is_antigravity(&provider) {
        return;
    }
    if let Some((client, api_key, _label)) = primary_credential(&cred_manager).await {
        fetch_quota_lines(&ui, &client, &api_key, &model, &cred_manager).await;
    }
}

fn is_antigravity(provider: &str) -> bool {
    provider.eq_ignore_ascii_case("antigravity") || provider.eq_ignore_ascii_case("agy")
}

/// The credential the account's requests are served by, a client to spend it on, and its label.
async fn primary_credential(
    cred_manager: &Arc<CredentialManager>,
) -> Option<(reqwest::Client, String, String)> {
    let creds = cred_manager.get_credentials("antigravity").await;
    let primary = creds
        .iter()
        .find(|c| c.is_priority && !c.is_temporarily_disabled)
        .or_else(|| creds.iter().find(|c| !c.is_temporarily_disabled))?;
    let client = harness_providers::build_http_client(30).ok()?;
    Some((client, primary.api_key.clone(), primary.label.clone()))
}

async fn fetch_quota_lines(
    ui: &ChatUi,
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    cred_manager: &Arc<CredentialManager>,
) {
    if let Ok(summary) =
        harness_providers::retrieve_antigravity_quota(client, api_key, Some(cred_manager)).await
    {
        ui.set_quota_lines(model, summary.quota_lines(model).unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(window: QuotaWindow, fraction: f64, secs_until_reset: Option<i64>) -> QuotaLine {
        QuotaLine {
            window,
            remaining_fraction: fraction,
            reset_time: secs_until_reset.map(|secs| {
                (chrono::Utc::now() + chrono::Duration::seconds(secs))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            }),
        }
    }

    /// The countdown is a function of when it is rendered, not of when the quota was fetched: the
    /// status bar repaints as the clock moves, and a readout rendered once and then held still is
    /// the bug that made the numbers look frozen until the session was restarted.
    #[test]
    fn the_quota_countdown_is_rendered_against_the_clock() {
        let now = chrono::Utc::now();
        let window = "2026-11-02T22:57:14Z";

        assert_eq!(
            format_reset_time_remaining(
                window,
                chrono::DateTime::parse_from_rfc3339("2026-11-02T18:00:00Z")
                    .expect("timestamp")
                    .with_timezone(&chrono::Utc)
            ),
            "(4h 57m)"
        );
        assert_eq!(
            format_reset_time_remaining(
                window,
                chrono::DateTime::parse_from_rfc3339("2026-11-02T20:00:00Z")
                    .expect("timestamp")
                    .with_timezone(&chrono::Utc)
            ),
            "(2h 57m)"
        );
        assert_eq!(format_reset_time_remaining("not a timestamp", now), "");
    }

    /// Off the minute boundary, so the rendered countdown is stable long enough to assert on.
    #[test]
    fn both_windows_are_labelled_and_separated() {
        let lines = vec![
            line(QuotaWindow::FiveHour, 0.815, Some(4 * 3600 + 30 * 60 + 30)),
            line(QuotaWindow::Weekly, 0.46, Some(167 * 3600 + 30)),
        ];
        assert_eq!(
            format_quota_lines(&lines),
            "5h 82% (4h 30m) · w 46% (6d 23h)"
        );
    }

    /// A provider that reports no reset time gets no brackets rather than an empty pair of them.
    #[test]
    fn a_window_without_a_reset_time_has_no_countdown() {
        let lines = vec![line(QuotaWindow::Weekly, 1.0, None)];
        assert_eq!(format_quota_lines(&lines), "w 100%");
    }
}
