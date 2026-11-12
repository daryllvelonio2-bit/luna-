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

/// What the account's own endpoint publishes when it cannot be reached.
///
/// The endpoint is the authority for this list — `antigravityModelSorts` carries the rows the CLI
/// shows, in its own order, with a display name each — so this table is only the fallback for an
/// outage, and the seed for name→key resolution before anything has been fetched. It is written
/// against a live account (31 models, 17 recommended rows) rather than the retired five-row family
/// list this used to be: `gemini-4-argon`, `gemini-4-hydrogen` and the tiered `gemini-3.8-flash`
/// rows are all real keys of the current table.
pub const ANTIGRAVITY_MODELS: &[&str] = &[
    "Gemini 4 Argon (Medium)",
    "Gemini 4 Argon (High)",
    "Gemini 4 Argon (Low)",
    "Gemini 4 Hydrogen (High)",
    "Gemini 4 Hydrogen (XHigh)",
    "Gemini 4 Hydrogen (Medium)",
    "Gemini 4 Hydrogen (Low)",
    "Gemini 3.8 Flash (High)",
    "Gemini 3.8 Flash (Medium)",
    "Gemini 3.8 Flash (Low)",
    "Claude Opus 5.5 (Low)",
    "Claude Opus 5.5 (Medium)",
    "Claude Opus 5.5 (High)",
    "Claude Sonnet 5.5 (Low)",
    "Claude Sonnet 5.5 (Medium)",
    "Claude Sonnet 5.5 (High)",
    "GPT-OSS 120B (Medium)",
];

/// `Gemini 4 Argon (Medium)` → `gemini-4-argon-medium`.
///
/// Every row the endpoint publishes is its key with the spaces dashed and the tier folded in, so
/// the display name alone is enough to name the model the backend expects. `GPT-OSS 120B (Medium)`
/// → `gpt-oss-120b-medium` holds the same way.
fn key_from_display_name(name: &str) -> String {
    let dashed = name
        .replace(['(', ')'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    // A generation is written with a dash in a Claude key (`claude-opus-5-5-low`) and with its dot
    // in a Gemini one (`gemini-3.8-flash-medium`), and the display name spells both the same way.
    // The family is what decides.
    if dashed.starts_with("claude-") {
        dashed.replace("5.5", "5-5").replace("4.6", "4-6")
    } else {
        dashed
    }
}

/// Which backend key each row of the live listing (or [`ANTIGRAVITY_MODELS`]) names.
///
/// Resolution has to answer for a *name* — the picker, the status line and a stored session all
/// hand over what the user chose — and only the endpoint knows which key is behind the row it
/// published. So the listing is remembered process-wide as it is read, and a name that has been
/// seen resolves to its own key rather than to a guess. The static table seeds it first, so a
/// session restored from cache (the listing is cached for a day) still resolves exactly.
#[derive(Default)]
struct ModelRegistry {
    /// Display name, lowercased, to the backend key it names.
    by_name: std::collections::HashMap<String, String>,
    /// Backend key to whether the endpoint says it takes a thinking config.
    thinking: std::collections::HashMap<String, bool>,
}

static MODEL_REGISTRY: std::sync::OnceLock<std::sync::RwLock<ModelRegistry>> =
    std::sync::OnceLock::new();

fn registry() -> &'static std::sync::RwLock<ModelRegistry> {
    MODEL_REGISTRY.get_or_init(|| {
        let mut registry = ModelRegistry::default();
        for name in ANTIGRAVITY_MODELS {
            registry.by_name.insert(
                name.to_lowercase(),
                key_from_display_name(&name.to_lowercase()),
            );
        }
        std::sync::RwLock::new(registry)
    })
}

/// The backend key a display name was published for, if the listing (or the static table) named it.
fn registered_key(name: &str) -> Option<String> {
    registry().read().ok()?.by_name.get(name).cloned()
}

/// Whether the endpoint says this backend key accepts a thinking config.
///
/// `gemini-3.1-flash-image` answers HTTP 400 — "Thinking level is not supported for this model" —
/// when one is sent, so the answer is worth having before the request is built. The listing's own
/// `supportsThinking` is the authority once it has been read; until then a model that names a
/// media output (an image, speech or music model) is the one case this route knows about, and every
/// other model takes a thinking config.
pub fn antigravity_model_supports_thinking(model: &str) -> bool {
    let lower = model.to_lowercase();
    if let Some(known) = registry()
        .read()
        .ok()
        .and_then(|registry| registry.thinking.get(&lower).copied())
    {
        return known;
    }
    !names_a_media_output(&lower)
}

/// Whether a model key names something other than text: an image, speech or music model has no
/// thinking to configure.
fn names_a_media_output(key: &str) -> bool {
    ["image", "imagen", "tts", "audio", "lyria"]
        .iter()
        .any(|marker| key.contains(marker))
}

/// The tier suffix a name may already carry, and the tier it names.
///
/// The CLI writes a tier two ways: a backend key suffixes it with a dash
/// (`claude-opus-5-5-high`), a display row with parentheses (`Gemini 4 Hydrogen (XHigh)`). Both
/// spellings mean the same thing, and a name that already carries one keeps it rather than being
/// re-tiered by the session setting.
const TIER_SUFFIXES: &[(&str, &str)] = &[
    ("-low", "low"),
    ("(low)", "low"),
    ("-medium", "medium"),
    ("(medium)", "medium"),
    ("-high", "high"),
    ("(high)", "high"),
    ("-xhigh", "xhigh"),
    ("(xhigh)", "xhigh"),
    ("-max", "max"),
    ("(max)", "max"),
];

/// The tier a name already names, if any.
fn explicit_tier(lower: &str) -> Option<&'static str> {
    TIER_SUFFIXES
        .iter()
        .find(|(suffix, _)| lower.ends_with(suffix))
        .map(|(_, tier)| *tier)
}

/// The effort tier a model request asks for, from an explicit suffix or the session's effort.
fn requested_tier(lower: &str, effort: Option<&ReasoningEffort>) -> &'static str {
    if let Some(tier) = explicit_tier(lower) {
        return tier;
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
/// The name a caller hands over is what the user picked: a row the endpoint published ("Gemini 4
/// Argon (Medium)"), a name written by an older Luna ("Gemini 4 Argon"), or a backend key copied
/// from `agy models` (`gemini-4-hydrogen-xhigh`). The published row and the key are answered
/// exactly — the row through the listing's own name→key table, the key as given — and only a name
/// nothing has published yet is resolved by the family rules below. Claude and GPT-OSS select a
/// tier-suffixed key, the Gemini 4 families carry their tier in the key itself, and the effort in
/// `thinkingConfig` follows the tier the name already chose.
pub fn resolve_antigravity_model(
    raw_model: &str,
    reasoning_effort: Option<&ReasoningEffort>,
) -> String {
    let cleaned = raw_model.trim();
    let cleaned = cleaned.strip_prefix("antigravity/").unwrap_or(cleaned);
    let cleaned = cleaned.strip_prefix("agy/").unwrap_or(cleaned);
    let lower = cleaned.to_lowercase();
    if lower.is_empty() {
        return cleaned.to_string();
    }

    // A row the listing published is the model it names; so is a key handed over as a key. This is
    // the path the picker and a restored session take, and it never guesses.
    if let Some(key) = registered_key(&lower) {
        return key;
    }

    let tier = requested_tier(&lower, reasoning_effort);
    // The name as a key would be written: spaces dashed and the parenthesised tier folded in. The
    // families below are decided on this form, so `Gemini 3.8 Flash (Low)` and
    // `gemini-3.8-flash-low` take the same path.
    let dashed = key_from_display_name(&lower);
    let key_shaped = !lower.contains(' ');

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
    } else if dashed.starts_with("gemini-4-argon") || dashed.starts_with("gemini-4-hydrogen") {
        // The Gemini 4 rows are keys of their own, one per tier — `gemini-4-argon-medium`, not a
        // single tiered key with the effort beside it. A name that already carries a tier keeps it;
        // a bare family name takes the tier the session asked for.
        if explicit_tier(&lower).is_some() {
            dashed
        } else {
            format!("{dashed}-{tier}")
        }
    } else if dashed.starts_with("gemini-3.8-flash") {
        // The 3.8 line is published as three tiered keys; `-tiered` is the retired single key and is
        // passed through untiered, so a session that stored it keeps the model it ran on.
        if lower.ends_with("-tiered") {
            cleaned.to_string()
        } else if explicit_tier(&lower).is_some() {
            dashed
        } else {
            format!("{dashed}-{tier}")
        }
    } else if key_shaped && lower.starts_with("gemini-") {
        // An explicit backend key from an older session (gemini-2.5-flash, gemini-3-flash,
        // gemini-3.1-flash-lite) is already what the endpoint expects. Mapping it to another key
        // would silently change which model answers, and would give a Gemini 3 validator a 2.5
        // request's signature rules.
        cleaned.to_string()
    } else if lower.contains("pro") && !lower.contains("argon") {
        "gemini-2.5-pro".to_string()
    } else if lower.contains(' ') {
        // A display row nothing has published yet: its own name is the key, dashed, with the tier
        // folded in — the shape every row on this route has.
        dashed
    } else {
        cleaned.to_string()
    }
}

/// One row of the account's own model table: the key, the name it is shown under, and whether it
/// takes a thinking config.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ListedModel {
    pub key: String,
    pub name: String,
    pub supports_thinking: Option<bool>,
}

/// The rows the endpoint published, in the order the CLI shows them.
///
/// The response is read as it describes itself. `agentModelSorts` is the switcher's own list —
/// one group per recommendation level, ids in the order they are offered — and each id's
/// `displayName` is the row's label, so a model the account gains appears in the picker without a
/// release here. The full `models` map is then folded in after it, so a model that is usable but
/// not recommended is still offered.
///
/// Two entries are left out on purpose: an id the CLI itself marks disabled (`-disabled`, and the
/// Gemini 4 Carbon row is exactly that: it is in the recommended list with no quota behind it, and
/// a turn against it answers 404), and any entry the endpoint publishes without a display name —
/// the internal `chat_*`/`tab_*` helpers, and the retired `gemini-3.8-flash-tiered` alias. The
/// alias still resolves (see [`resolve_antigravity_model`]); it is simply not a row of its own.
pub(super) fn models_from_listing(value: &serde_json::Value) -> Vec<String> {
    let models = value.get("models").and_then(|models| models.as_object());
    let Some(models) = models else {
        return Vec::new();
    };

    let entry = |key: &str| {
        let e = models.get(key)?;
        let name = e.get("displayName").and_then(|name| name.as_str())?.trim();
        if name.is_empty() {
            return None;
        }
        Some(ListedModel {
            key: key.to_string(),
            name: name.to_string(),
            supports_thinking: e.get("supportsThinking").and_then(|s| s.as_bool()),
        })
    };

    let mut listed: Vec<ListedModel> = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();
    let mut seen_names = std::collections::HashSet::new();
    let mut push = |key: &str, listed: &mut Vec<ListedModel>| {
        let key = key.trim();
        if key.is_empty() || key.ends_with("-disabled") || !seen_keys.insert(key.to_lowercase()) {
            return;
        }
        if let Some(model) = entry(key) {
            // Four retired Gemini keys all claim the display name "Gemini 3.5 Flash Lite". Two
            // identical rows would be a choice the picker cannot express — the user sees one label
            // and gets whichever key resolution happened to keep — so the first row to claim a name
            // is the one the name means. It is the recommended order that decides that, and the
            // published `gemini-3.5-flash-lite` key is not among them.
            if !seen_names.insert(model.name.to_lowercase()) {
                return;
            }
            listed.push(model);
        }
    };

    for sort in value
        .get("agentModelSorts")
        .and_then(|sorts| sorts.as_array())
        .unwrap_or(&Vec::new())
    {
        for group in sort
            .get("groups")
            .and_then(|groups| groups.as_array())
            .unwrap_or(&Vec::new())
        {
            for id in group
                .get("modelIds")
                .and_then(|ids| ids.as_array())
                .unwrap_or(&Vec::new())
            {
                if let Some(id) = id.as_str() {
                    push(id, &mut listed);
                }
            }
        }
    }

    let mut rest: Vec<&String> = models.keys().collect();
    rest.sort();
    for key in rest {
        push(key, &mut listed);
    }

    if listed.is_empty() {
        return Vec::new();
    }

    {
        let mut registry = registry()
            .write()
            .expect("the model registry is not poisoned");
        for model in &listed {
            registry
                .by_name
                .insert(model.name.to_lowercase(), model.key.clone());
            if let Some(supports) = model.supports_thinking {
                registry.thinking.insert(model.key.to_lowercase(), supports);
            }
        }
    }

    listed.into_iter().map(|model| model.name).collect()
}

/// Fetches the model rows the Antigravity CLI shows.
///
/// The live `fetchAvailableModels` call is what decides which models the account can spend on, and
/// the rows come back in the CLI's own order with the CLI's own labels. An endpoint outage must not
/// empty the picker, so an unreachable or unrecognised response falls back to
/// [`ANTIGRAVITY_MODELS`], which is the same table as of the last probe.
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
