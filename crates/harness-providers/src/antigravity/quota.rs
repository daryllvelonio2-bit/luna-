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

impl AntigravityQuotaSummary {
    pub fn format_quota_display(&self, model: &str) -> Option<String> {
        let lower = model.to_lowercase();
        let is_gemini =
            lower.contains("gemini") || lower.contains("flash") || lower.contains("pro");
        let is_claude_gpt = lower.contains("claude")
            || lower.contains("opus")
            || lower.contains("sonnet")
            || lower.contains("gpt");

        let resolved = resolve_antigravity_model(model, None);

        // Check model-specific quotaInfo from fetchAvailableModels first if Gemini.
        // Google Antigravity exposes the 5-hour rolling limit for Gemini models via quotaInfo in
        // fetchAvailableModels (resetTime <= 5 hours), NOT in retrieveUserQuotaSummary groups.
        if is_gemini {
            if let Some(q_info) = self.models_quota.get(&resolved).or_else(|| {
                self.models_quota
                    .iter()
                    .find(|(k, _)| {
                        k.to_lowercase().contains("flash") || k.to_lowercase().contains("gemini")
                    })
                    .map(|(_, v)| v)
            }) {
                let pct_str = q_info
                    .remaining_fraction
                    .map(|frac| format!("{:.0}%", (frac.clamp(0.0, 1.0) * 100.0).round()))
                    .unwrap_or_else(|| "100%".to_string());
                let time_str = q_info
                    .reset_time
                    .as_ref()
                    .map(|t| format_reset_time_remaining(t))
                    .unwrap_or_default();

                let is_weekly_reset = q_info
                    .reset_time
                    .as_deref()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|dt| (dt.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_days() > 1)
                    .unwrap_or(false);

                let mut hourly_part = None;
                let mut weekly_part = None;

                if is_weekly_reset {
                    let part = if time_str.is_empty() {
                        format!("w {}", pct_str)
                    } else {
                        format!("w {} {}", pct_str, time_str)
                    };
                    weekly_part = Some(part);
                } else {
                    let part = if time_str.is_empty() {
                        format!("5h {}", pct_str)
                    } else {
                        format!("5h {} {}", pct_str, time_str)
                    };
                    hourly_part = Some(part);
                }

                // Check if any group has a dedicated weekly bucket for Gemini
                if weekly_part.is_none() {
                    if let Some(group) = self.groups.iter().find(|g| {
                        g.display_name
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains("gemini")
                    }) {
                        if let Some(b) = group.buckets.iter().find(|b| {
                            let w = b.window.as_deref().unwrap_or("").to_lowercase();
                            let b_id = b.bucket_id.as_deref().unwrap_or("").to_lowercase();
                            w == "weekly" || w == "w" || b_id.contains("weekly")
                        }) {
                            let w_pct = if let Some(frac) = b.remaining_fraction {
                                format!("{:.0}%", (frac.clamp(0.0, 1.0) * 100.0).round())
                            } else {
                                "100%".to_string()
                            };
                            let w_time = b
                                .reset_time
                                .as_ref()
                                .map(|t| format_reset_time_remaining(t))
                                .unwrap_or_default();
                            let part = if w_time.is_empty() {
                                format!("w {}", w_pct)
                            } else {
                                format!("w {} {}", w_pct, w_time)
                            };
                            weekly_part = Some(part);
                        }
                    }
                }

                return match (hourly_part, weekly_part) {
                    (Some(h), Some(w)) => Some(format!("{} · {}", h, w)),
                    (Some(h), None) => Some(h),
                    (None, Some(w)) => Some(w),
                    (None, None) => None,
                };
            }
        }

        // Pick target group based on active model:
        // Gemini models match groups mentioning "gemini"; Claude/GPT models match groups mentioning "claude" or "gpt".
        let target_group = self
            .groups
            .iter()
            .find(|g| {
                let name = g.display_name.as_deref().unwrap_or("").to_lowercase();
                (is_gemini && name.contains("gemini"))
                    || (is_claude_gpt && (name.contains("claude") || name.contains("gpt")))
            })
            .or_else(|| {
                if !is_gemini {
                    self.groups.first()
                } else {
                    None
                }
            });

        let group = target_group?;

        let mut hourly_part = None;
        let mut weekly_part = None;

        for b in &group.buckets {
            let w = b.window.as_deref().unwrap_or("").to_lowercase();
            let b_id = b.bucket_id.as_deref().unwrap_or("").to_lowercase();

            let is_5h = w == "5h" || b_id.contains("5h");
            let is_weekly = w == "weekly" || w == "w" || b_id.contains("weekly");

            if !is_5h && !is_weekly {
                continue;
            }

            let pct_str = if let Some(frac) = b.remaining_fraction {
                format!("{:.0}%", (frac.clamp(0.0, 1.0) * 100.0).round())
            } else {
                "100%".to_string()
            };

            let time_str = if let Some(ref r_time) = b.reset_time {
                format_reset_time_remaining(r_time)
            } else {
                String::new()
            };

            if is_5h && hourly_part.is_none() {
                let part = if time_str.is_empty() {
                    format!("5h {}", pct_str)
                } else {
                    format!("5h {} {}", pct_str, time_str)
                };
                hourly_part = Some(part);
            } else if is_weekly && weekly_part.is_none() {
                let part = if time_str.is_empty() {
                    format!("w {}", pct_str)
                } else {
                    format!("w {} {}", pct_str, time_str)
                };
                weekly_part = Some(part);
            }
        }

        match (hourly_part, weekly_part) {
            (Some(h), Some(w)) => Some(format!("{} · {}", h, w)),
            (Some(h), None) => Some(h),
            (None, Some(w)) => Some(w),
            (None, None) => None,
        }
    }
}

pub fn format_reset_time_remaining(reset_time_str: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(reset_time_str) {
        let now = chrono::Utc::now();
        let diff = dt.with_timezone(&chrono::Utc) - now;
        let total_secs = diff.num_seconds();
        if total_secs <= 0 {
            "(0m)".to_string()
        } else {
            let days = total_secs / 86400;
            let hours = (total_secs % 86400) / 3600;
            let minutes = (total_secs % 3600) / 60;
            if days > 0 {
                format!("({}d {}h)", days, hours)
            } else if hours > 0 {
                format!("({}h {}m)", hours, minutes)
            } else {
                format!("({}m)", minutes.max(1))
            }
        }
    } else {
        String::new()
    }
}

/// Retrieve Antigravity user quota summary (hourly and weekly buckets).
pub async fn retrieve_antigravity_quota(
    http_client: &reqwest::Client,
    raw_secret: &str,
    cred_manager: Option<&std::sync::Arc<crate::auth::CredentialManager>>,
) -> Result<AntigravityQuotaSummary, HarnessError> {
    let (mut bearer_token, refresh_tok_opt) = extract_bearer_token(raw_secret);

    let send = |endpoint: &'static str, token: String| {
        let client = http_client.clone();
        async move {
            client
                .post(endpoint)
                .header(AUTHORIZATION, format!("Bearer {}", token))
                .header(USER_AGENT, DEFAULT_USER_AGENT)
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
                    if let Some(ref refresh_tok) = refresh_tok_opt {
                        if let Ok(new_token) =
                            refresh_antigravity_token(http_client, refresh_tok).await
                        {
                            bearer_token = new_token.clone();
                            if let Some(cm) = cred_manager {
                                let updated_secret = json!({
                                    "access_token": new_token,
                                    "refresh_token": refresh_tok,
                                });
                                let _ = cm
                                    .update_credential_key(
                                        "antigravity",
                                        raw_secret,
                                        &updated_secret.to_string(),
                                    )
                                    .await;
                            }
                            if let Ok(r2) = send(ep, bearer_token.clone()).await {
                                if r2.status().is_success() {
                                    resp_opt = Some(r2);
                                    break;
                                }
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

pub const ANTIGRAVITY_MODELS: &[&str] = &[
    "gemini-3.8-flash",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-3-flash-preview",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "gpt-oss-120b",
    "gpt-oss-20b",
];

/// Resolves user-facing clean model names and reasoning effort settings to backend Code Assist model names.
pub fn resolve_antigravity_model(
    raw_model: &str,
    _reasoning_effort: Option<&ReasoningEffort>,
) -> String {
    let cleaned = raw_model.trim();
    let cleaned = cleaned.strip_prefix("antigravity/").unwrap_or(cleaned);
    let cleaned = cleaned.strip_prefix("agy/").unwrap_or(cleaned);
    let lower = cleaned.to_lowercase();

    if lower.contains("opus") {
        "claude-opus-4-6-thinking".to_string()
    } else if lower.contains("sonnet") {
        "claude-sonnet-4-6".to_string()
    } else if lower.contains("gpt-oss") || lower.contains("oss") {
        "gpt-oss-120b-medium".to_string()
    } else if lower.contains("pro") {
        "gemini-2.5-pro".to_string()
    } else if lower.contains("gemini") || lower.contains("flash") {
        "gemini-3.8-flash-tiered".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Fetches supported Antigravity models, returning clean user-facing model names without reasoning effort suffixes.
pub async fn fetch_antigravity_models(
    _http_client: &reqwest::Client,
    _bearer_token: &str,
) -> Result<Vec<String>, HarnessError> {
    Ok(ANTIGRAVITY_MODELS.iter().map(|&m| m.to_string()).collect())
}
