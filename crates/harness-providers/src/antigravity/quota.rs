//! What the account has left, and which models it can spend it on.
//!
//! The quota summary is assembled here, together with the model table the route publishes: the
//! numbers a status line shows and the list a picker offers both come from the same endpoints.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityQuotaBucket {
    pub bucket_id: Option<String>,
    pub display_name: Option<String>,
    pub window: Option<String>,
    pub reset_time: Option<String>,
    pub remaining_fraction: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityQuotaGroup {
    pub display_name: Option<String>,
    #[serde(default)]
    pub buckets: Vec<AntigravityQuotaBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityModelQuotaInfo {
    pub remaining_fraction: Option<f64>,
    pub reset_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AntigravityQuotaSummary {
    #[serde(default)]
    pub groups: Vec<AntigravityQuotaGroup>,
    #[serde(default)]
    pub models_quota: std::collections::HashMap<String, AntigravityModelQuotaInfo>,
}

/// Which of the account's metered windows a quota line describes.
///
/// The two are not interchangeable: one is a rolling five-hour window and the other the weekly
/// limit, and they reset on different schedules. Naming the window rather than handing back a
/// rendered label is what keeps the status bar's wording out of this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWindow {
    /// The rolling five-hour window.
    FiveHour,
    /// The weekly limit.
    Weekly,
}

/// One window of the account's quota, as the provider reported it.
///
/// The reset time is carried as the RFC3339 string the provider sent rather than as a rendered
/// countdown: the caller repaints its status line as the clock moves, and a `(4h 59m)` computed
/// when the quota was fetched is a number that stops being true the moment it is written.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaLine {
    /// Which window this is.
    pub window: QuotaWindow,
    /// Share of the window still available, `0.0..=1.0`.
    pub remaining_fraction: f64,
    /// When the window refills, as the provider reported it.
    pub reset_time: Option<String>,
}

/// The `window` a bucket declares, lowercased.
fn bucket_window(bucket: &AntigravityQuotaBucket) -> String {
    bucket.window.as_deref().unwrap_or("").to_lowercase()
}

/// A bucket's id, lowercased.
fn bucket_id(bucket: &AntigravityQuotaBucket) -> String {
    bucket.bucket_id.as_deref().unwrap_or("").to_lowercase()
}

/// Whether a bucket meters a 5-hour window.
fn is_five_hour_bucket(bucket: &AntigravityQuotaBucket) -> bool {
    bucket_window(bucket) == "5h" || bucket_id(bucket).contains("5h")
}

/// Whether a bucket meters a weekly window.
fn is_weekly_bucket(bucket: &AntigravityQuotaBucket) -> bool {
    let window = bucket_window(bucket);
    window == "weekly" || window == "w" || bucket_id(bucket).contains("weekly")
}

/// Whether a bucket belongs to the Gemini models rather than to the third-party (Claude/GPT) ones.
///
/// Gemini is metered separately, and the summary says so itself: "within each group, models share a
/// weekly limit and a 5-hour limit". So a bucket's own identity — its id, or the name it is
/// labelled with — is what decides which model family may read it, not the name of the group it
/// happens to sit in.
fn is_gemini_bucket(bucket: &AntigravityQuotaBucket) -> bool {
    let id = bucket_id(bucket);
    let name = bucket.display_name.as_deref().unwrap_or("").to_lowercase();
    id.contains("gemini") || name.contains("gemini")
}

/// Whether a reset is far enough out to be a weekly window rather than the rolling 5-hour one.
///
/// Some accounts publish their weekly limit only as the per-model `quotaInfo` reset time; a day is
/// the separator, because a 5-hour window can never be more than five hours away.
fn reset_is_weekly(reset_time: Option<&str>) -> bool {
    reset_time
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|dt| (dt.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_days() > 1)
        .unwrap_or(false)
}

/// One line, with a fraction the provider left out read as "all of it".
fn quota_line(window: QuotaWindow, fraction: Option<f64>, reset_time: Option<String>) -> QuotaLine {
    QuotaLine {
        window,
        remaining_fraction: fraction.unwrap_or(1.0),
        reset_time,
    }
}

/// The 5-hour line and the weekly line, in that order, with an absent one left out.
///
/// The order is the contract: every caller paints them as the account's two windows, shortest
/// first.
fn join_quota_lines(hourly: Option<QuotaLine>, weekly: Option<QuotaLine>) -> Vec<QuotaLine> {
    [hourly, weekly].into_iter().flatten().collect()
}

impl AntigravityQuotaSummary {
    /// The quota windows the status bar shows for `model`, the 5-hour window first.
    ///
    /// Gemini models and the third-party ones (Claude, GPT-OSS) are metered in separate buckets,
    /// and for one account the two are not published in the same place. `fetchAvailableModels`
    /// carries the 5-hour window per Gemini model (`gemini-3.8-flash-tiered` -> 82%) and nothing
    /// else; `retrieveUserQuotaSummary` carries the groups, and the only group many accounts get
    /// back is the third-party one, "Claude and GPT models", holding the account's one weekly
    /// bucket. So the 5-hour window of a Gemini model is read from that model's own `quotaInfo`,
    /// and a weekly window is read from whichever bucket declares one — the Gemini bucket first
    /// when the provider does publish one, the account's third-party bucket otherwise, because
    /// that bucket *is* the account's weekly limit.
    pub fn quota_lines(&self, model: &str) -> Option<Vec<QuotaLine>> {
        let resolved = resolve_antigravity_model(model, None);
        let lower = resolved.to_lowercase();
        let is_gemini = lower.contains("gemini");
        let is_claude_gpt = lower.contains("claude") || lower.contains("gpt");

        let account_weekly = self
            .groups
            .iter()
            .flat_map(|group| group.buckets.iter())
            .filter(|bucket| is_weekly_bucket(bucket))
            .min_by_key(|bucket| u8::from(!is_gemini_bucket(bucket)))
            .map(|bucket| {
                quota_line(
                    QuotaWindow::Weekly,
                    bucket.remaining_fraction,
                    bucket.reset_time.clone(),
                )
            });

        if is_gemini {
            let own = self.model_quota(model, &resolved);
            let own_is_weekly = own.is_some_and(|info| reset_is_weekly(info.reset_time.as_deref()));

            let hourly = own
                .filter(|_| !own_is_weekly)
                .map(|info| {
                    quota_line(
                        QuotaWindow::FiveHour,
                        info.remaining_fraction,
                        info.reset_time.clone(),
                    )
                })
                // No per-model window: a Gemini bucket in a group is the next best thing, and it
                // is still a Gemini number. The third-party buckets are never read for a Gemini
                // model — that 5-hour window belongs to Claude and GPT-OSS, and showing it here
                // would report another family's limit as this model's.
                .or_else(|| {
                    self.gemini_bucket(is_five_hour_bucket).map(|bucket| {
                        quota_line(
                            QuotaWindow::FiveHour,
                            bucket.remaining_fraction,
                            bucket.reset_time.clone(),
                        )
                    })
                });
            let weekly = account_weekly.or_else(|| {
                own.filter(|_| own_is_weekly).map(|info| {
                    quota_line(
                        QuotaWindow::Weekly,
                        info.remaining_fraction,
                        info.reset_time.clone(),
                    )
                })
            });

            let lines = join_quota_lines(hourly, weekly);
            return (!lines.is_empty()).then_some(lines);
        }

        // Claude, GPT-OSS, and anything else the account meters as a third party: the group that
        // names the family, or the account's only group when no name matches. A group name is a
        // label rather than data, so it decides a *preference* here and nothing more.
        let group = self
            .groups
            .iter()
            .find(|group| {
                let name = group.display_name.as_deref().unwrap_or("").to_lowercase();
                is_claude_gpt && (name.contains("claude") || name.contains("gpt"))
            })
            .or_else(|| self.groups.first())?;

        let pick = |matches: fn(&AntigravityQuotaBucket) -> bool| {
            group
                .buckets
                .iter()
                .filter(|bucket| matches(bucket))
                .min_by_key(|bucket| u8::from(is_gemini_bucket(bucket)))
        };
        let hourly = pick(is_five_hour_bucket).map(|bucket| {
            quota_line(
                QuotaWindow::FiveHour,
                bucket.remaining_fraction,
                bucket.reset_time.clone(),
            )
        });
        let weekly = pick(is_weekly_bucket)
            .map(|bucket| {
                quota_line(
                    QuotaWindow::Weekly,
                    bucket.remaining_fraction,
                    bucket.reset_time.clone(),
                )
            })
            .or(account_weekly);

        let lines = join_quota_lines(hourly, weekly);
        (!lines.is_empty()).then_some(lines)
    }

    /// The window the provider published for this model on its own.
    ///
    /// The key is the resolved backend name (`gemini-3.8-flash-tiered`), with the name the caller
    /// asked for as a second chance, and both are looked up exactly. There is no loose match on
    /// purpose: the same table holds the Claude and GPT-OSS entries, so "any key mentioning gemini"
    /// answers a question about a different model's window than the one being asked about.
    fn model_quota(&self, model: &str, resolved: &str) -> Option<&AntigravityModelQuotaInfo> {
        self.models_quota
            .get(resolved)
            .or_else(|| self.models_quota.get(model))
    }

    /// The Gemini bucket matching `matches`, from whichever group carries it.
    fn gemini_bucket(
        &self,
        matches: fn(&AntigravityQuotaBucket) -> bool,
    ) -> Option<&AntigravityQuotaBucket> {
        self.groups
            .iter()
            .flat_map(|group| group.buckets.iter())
            .find(|bucket| is_gemini_bucket(bucket) && matches(bucket))
    }
}

/// Retrieve Antigravity user quota summary (hourly and weekly buckets).
pub async fn retrieve_antigravity_quota(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&std::sync::Arc<crate::auth::CredentialManager>>,
) -> Result<AntigravityQuotaSummary, HarnessError> {
    let (mut bearer_token, _) = extract_bearer_token(raw_secret);

    let send = |endpoint: &'static str, token: String| {
        let client = http_client.clone();
        async move {
            client
                .post(endpoint)
                .header(AUTHORIZATION, format!("Bearer {}", token))
                .header(USER_AGENT, user_agent())
                .header(CONTENT_TYPE, "application/json")
                .json(&json!({}))
                .send()
                .await
        }
    };

    let endpoints = [USER_QUOTA_ENDPOINT, FALLBACK_USER_QUOTA_ENDPOINT];
    let mut resp_opt = None;

    for ep in endpoints {
        match send(ep, bearer_token.clone()).await {
            Ok(r) => {
                if r.status().as_u16() == 401 {
                    if let Ok(new_token) =
                        super::credentials::refresh_and_store_antigravity_credential(
                            http_client,
                            raw_secret,
                            cred_manager,
                        )
                        .await
                    {
                        bearer_token = new_token;
                        if let Ok(r2) = send(ep, bearer_token.clone()).await {
                            if r2.status().is_success() {
                                resp_opt = Some(r2);
                                break;
                            }
                        }
                    }
                } else if r.status().is_success() {
                    resp_opt = Some(r);
                    break;
                }
            }
            Err(_) => continue,
        }
    }

    let mut summary = if let Some(resp) = resp_opt {
        resp.json::<AntigravityQuotaSummary>()
            .await
            .unwrap_or_default()
    } else {
        AntigravityQuotaSummary::default()
    };

    // Also query fetchAvailableModels to obtain exact per-model quotaInfo
    let model_endpoints = [MODELS_ENDPOINT, FALLBACK_MODELS_ENDPOINT];
    for ep in model_endpoints {
        if let Ok(resp) = send(ep, bearer_token.clone()).await {
            if resp.status().is_success() {
                if let Ok(val) = resp.json::<serde_json::Value>().await {
                    if let Some(models_map) = val.get("models").and_then(|m| m.as_object()) {
                        for (model_name, m_val) in models_map {
                            if let Some(q_val) = m_val.get("quotaInfo") {
                                let frac = q_val.get("remainingFraction").and_then(|f| f.as_f64());
                                let r_time = q_val
                                    .get("resetTime")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string());
                                summary.models_quota.insert(
                                    model_name.clone(),
                                    AntigravityModelQuotaInfo {
                                        remaining_fraction: frac,
                                        reset_time: r_time,
                                    },
                                );
                            }
                        }
                    }
                    break;
                }
            }
        }
    }

    Ok(summary)
}

/// The rows the Antigravity CLI's model switcher shows, in its order.
///
/// These are the names the CLI puts in front of the user, not the backend keys: the switcher has
/// one row per model family and a separate effort control, and `gpt-oss` has only its medium tier
/// so its tier is part of the name. The endpoint does not publish `gemini-4-argon` at all (it
/// publishes the single tiered Gemini key), and it still lists retired models such as
/// `gemini-2.5-pro`; mirroring the live table verbatim is exactly how the picker drifted from the
/// CLI. This list is the fallback and the ordering the live listing is folded into.
pub const ANTIGRAVITY_MODELS: &[&str] = &[
    "Gemini 4 Argon",
    "Gemini 3.8 Flash",
    "Claude Opus 5.5",
    "Claude Sonnet 5.5",
    "GPT-OSS 120B (Medium)",
];

/// The effort tier a model request asks for, from an explicit suffix or the session's effort.
///
/// The CLI exposes low/medium/high beside the model, and a name that already carries a tier (an
/// old session, or an id copied from `agy models`) keeps that tier rather than being re-tiered by
/// the current session setting.
fn requested_tier(lower: &str, effort: Option<&ReasoningEffort>) -> &'static str {
    for (suffix, tier) in [
        ("-low", "low"),
        ("(low)", "low"),
        ("-medium", "medium"),
        ("(medium)", "medium"),
        ("-high", "high"),
        ("(high)", "high"),
        ("-max", "max"),
        ("(max)", "max"),
    ] {
        if lower.ends_with(suffix) {
            return tier;
        }
    }
    match effort {
        Some(ReasoningEffort::Minimal) | Some(ReasoningEffort::Low) => "low",
        Some(ReasoningEffort::High) | Some(ReasoningEffort::Xhigh) => "high",
        // The account publishes low/medium/high Claude tiers; max exists in the client registry
        // but is not among the available models, so the highest usable tier is high.
        Some(ReasoningEffort::Max) => "high",
        _ => "medium",
    }
}

/// Whether a Claude name refers to the 5.5 generation rather than the retired 4.6 one.
fn is_claude_55(lower: &str) -> bool {
    lower.contains("5-5") || lower.contains("5.5") || lower.contains("5 5")
}

/// Resolves user-facing model names and reasoning effort settings to backend Code Assist model names.
///
/// The Gemini rows share one backend key, `gemini-3.8-flash-tiered`: it is the tiered model the
/// endpoint publishes and the effort is carried by `thinkingConfig`, not by the key. Claude and
/// GPT-OSS select a tier-suffixed key instead. Names written by an older Luna (or copied from
/// `agy models`) still resolve, so a stored session keeps working after the picker is corrected.
pub fn resolve_antigravity_model(
    raw_model: &str,
    reasoning_effort: Option<&ReasoningEffort>,
) -> String {
    let cleaned = raw_model.trim();
    let cleaned = cleaned.strip_prefix("antigravity/").unwrap_or(cleaned);
    let cleaned = cleaned.strip_prefix("agy/").unwrap_or(cleaned);
    let lower = cleaned.to_lowercase();
    let tier = requested_tier(&lower, reasoning_effort);

    if lower.contains("opus") {
        if is_claude_55(&lower) {
            format!("claude-opus-5-5-{tier}")
        } else {
            "claude-opus-4-6-thinking".to_string()
        }
    } else if lower.contains("sonnet") {
        if is_claude_55(&lower) {
            format!("claude-sonnet-5-5-{tier}")
        } else {
            "claude-sonnet-4-6".to_string()
        }
    } else if lower.contains("gpt-oss") || lower.contains("oss") {
        "gpt-oss-120b-medium".to_string()
    } else if lower.starts_with("gemini-")
        && !lower.starts_with("gemini-3.8-flash")
        && !lower.starts_with("gemini-4-argon")
    {
        // An explicit backend key from an older session (gemini-2.5-flash, gemini-3-flash,
        // gemini-3.1-flash-lite) is already what the endpoint expects. Mapping it to the tiered
        // key would silently change which model answers, and would give a Gemini 3 validator a
        // 2.5 request's signature rules.
        cleaned.to_string()
    } else if lower.contains("pro") && !lower.contains("argon") {
        "gemini-2.5-pro".to_string()
    } else if lower.contains("argon") || lower.contains("gemini") || lower.contains("flash") {
        "gemini-3.8-flash-tiered".to_string()
    } else {
        cleaned.to_string()
    }
}

/// The CLI switcher row a backend model key belongs to.
///
/// The endpoint's `displayName` is the authority when it carries one, but it has gaps and lies:
/// `gemini-3.8-flash-tiered` has no name, while three retired Gemini keys all claim
/// "Gemini 3.5 Flash Lite". Only the families in [`ANTIGRAVITY_MODELS`] are recognised, so those
/// stale keys and the internal `chat_*`/`tab_*` entries never reach the picker.
fn cli_model_family(key: &str, display_name: Option<&str>) -> Option<&'static str> {
    let key = key.to_ascii_lowercase();
    let display = display_name.unwrap_or("").to_ascii_lowercase();

    // The tiered key is the current Gemini model even though its display name is absent. It is
    // what the CLI presents as Gemini 4 Argon; Gemini 3.8 Flash is the client-side alias beside it.
    if key.contains("gemini-3.8-flash-tiered") {
        return Some("Gemini 4 Argon");
    }
    if key.contains("gemini-4-argon") || display.starts_with("gemini 4 argon") {
        return Some("Gemini 4 Argon");
    }
    if key.contains("gemini-3.8-flash") || display.starts_with("gemini 3.8 flash") {
        return Some("Gemini 3.8 Flash");
    }
    if key.contains("claude-opus-5-5") || display.starts_with("claude opus 5.5") {
        return Some("Claude Opus 5.5");
    }
    if key.contains("claude-sonnet-5-5") || display.starts_with("claude sonnet 5.5") {
        return Some("Claude Sonnet 5.5");
    }
    if key.contains("gpt-oss-120b") || display.starts_with("gpt-oss 120b") {
        return Some("GPT-OSS 120B (Medium)");
    }
    None
}

/// The switcher rows present in a `fetchAvailableModels` body.
///
/// An empty body, or one with no recognised family, yields an empty list so the caller can fall
/// back to [`ANTIGRAVITY_MODELS`] rather than showing a partial picker. The two Gemini rows are
/// client aliases and are always present: the endpoint publishes only the tiered key for them.
pub(super) fn models_from_listing(value: &serde_json::Value) -> Vec<String> {
    let Some(models) = value.get("models").and_then(|models| models.as_object()) else {
        return Vec::new();
    };

    let mut seen = std::collections::HashSet::new();
    for (key, entry) in models {
        let display = entry.get("displayName").and_then(|name| name.as_str());
        if let Some(family) = cli_model_family(key, display) {
            seen.insert(family);
        }
    }
    if seen.is_empty() {
        return Vec::new();
    }
    seen.insert("Gemini 4 Argon");
    seen.insert("Gemini 3.8 Flash");

    ANTIGRAVITY_MODELS
        .iter()
        .filter(|name| seen.contains(**name))
        .map(|name| (*name).to_string())
        .collect()
}

/// Fetches the model rows the Antigravity CLI shows.
///
/// The live `fetchAvailableModels` call decides which families the account can actually spend on;
/// the result is folded into the CLI's own order and display names. A retired key in the response
/// is not evidence that the CLI still offers it, and an endpoint outage must not empty the picker,
/// so an unreachable or unrecognised response falls back to [`ANTIGRAVITY_MODELS`].
pub async fn fetch_antigravity_models(
    http_client: &reqwest::Client,
    bearer_token: &str,
) -> Result<Vec<String>, HarnessError> {
    let mut last_error = None;

    for endpoint in [MODELS_ENDPOINT, FALLBACK_MODELS_ENDPOINT] {
        match http_client
            .post(endpoint)
            .header(AUTHORIZATION, format!("Bearer {}", bearer_token))
            .header(USER_AGENT, user_agent())
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({}))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match response.json::<serde_json::Value>().await {
                    Ok(body) => {
                        let models = models_from_listing(&body);
                        if !models.is_empty() {
                            return Ok(models);
                        }
                        last_error = Some("the response named no known model".to_string());
                    }
                    Err(error) => last_error = Some(format!("invalid JSON: {error}")),
                }
            }
            Ok(response) => last_error = Some(format!("HTTP {}", response.status())),
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    tracing::debug!(
        target: "harness_providers::antigravity",
        "live model discovery unavailable ({}); using the CLI's current model list",
        last_error.as_deref().unwrap_or("no endpoint answered")
    );
    Ok(ANTIGRAVITY_MODELS.iter().map(|&m| m.to_string()).collect())
}
