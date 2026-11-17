//! Schedule data model, recurrence parsing, and delivery targets.

use chrono::{DateTime, Datelike, Duration, NaiveTime, Timelike, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_DELIVERY_HISTORY: usize = 25;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryTarget {
    ExistingSession {
        session_id: String,
    },
    SingleNewSession {
        session_id: Option<String>,
    },
    #[default]
    NewSessionEachRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub delivered_at: DateTime<Utc>,
    pub session_id: String,
    pub status: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleTask {
    pub id: String,
    pub name: String,
    pub instruction: String,
    /// Rule expression:
    /// - `"once:<RFC3339>"` (one-shot)
    /// - `"every:<seconds>"` (fixed interval, minimum 10s)
    /// - `"daily:<HH:MM>"` (UTC daily)
    /// - `"<min> <hour> <dom> <mon> <dow>"` (5-field UTC cron)
    pub rule: String,
    pub delivery_target: DeliveryTarget,
    #[serde(default)]
    pub plugin_id: Option<String>,
    pub paused: bool,
    pub completed: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub history: Vec<DeliveryReceipt>,
}

/// Parse a schedule rule and compute the next run timestamp after `after`.
pub fn compute_next_run(rule: &str, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, String> {
    let trimmed = rule.trim();
    if let Some(ts) = trimmed.strip_prefix("once:") {
        let dt = DateTime::parse_from_rfc3339(ts.trim())
            .map_err(|e| format!("invalid once timestamp '{ts}': {e}"))?
            .with_timezone(&Utc);
        return if dt > after { Ok(Some(dt)) } else { Ok(None) };
    }
    if let Some(secs_raw) = trimmed.strip_prefix("every:") {
        let secs: u64 = secs_raw
            .trim()
            .parse()
            .map_err(|e| format!("invalid interval seconds '{secs_raw}': {e}"))?;
        let bounded = secs.max(10);
        return Ok(Some(after + Duration::seconds(bounded as i64)));
    }
    if let Some(hm) = trimmed.strip_prefix("daily:") {
        let time = NaiveTime::parse_from_str(hm.trim(), "%H:%M")
            .map_err(|e| format!("invalid daily HH:MM '{hm}': {e}"))?;
        let today_dt = after.date_naive().and_time(time).and_utc();
        let next = if today_dt > after {
            today_dt
        } else {
            today_dt + Duration::days(1)
        };
        return Ok(Some(next));
    }

    // 5-field cron: min hour dom mon dow
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() == 5 {
        return next_cron_match(&parts, after).map(Some);
    }

    Err(format!(
        "unsupported schedule rule '{trimmed}'. Use 'once:<RFC3339>', 'every:<secs>', 'daily:<HH:MM>', or 5-field cron."
    ))
}

pub fn validate_rule(rule: &str) -> Result<(), String> {
    let _ = compute_next_run(rule, Utc::now())?;
    Ok(())
}

fn next_cron_match(parts: &[&str], after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let min_set = parse_cron_field(parts[0], 0, 59)?;
    let hour_set = parse_cron_field(parts[1], 0, 23)?;
    let dom_set = parse_cron_field(parts[2], 1, 31)?;
    let mon_set = parse_cron_field(parts[3], 1, 12)?;
    let dow_set = parse_cron_field(parts[4], 0, 6)?;

    let mut cursor = (after + Duration::minutes(1))
        .with_second(0)
        .and_then(|d| d.with_nanosecond(0))
        .ok_or_else(|| "invalid cron start time".to_string())?;

    // Search up to 366 days ahead (527,040 minutes)
    for _ in 0..527_040 {
        let m = cursor.minute();
        let h = cursor.hour();
        let d = cursor.day();
        let mo = cursor.month();
        let dw = cursor.weekday().num_days_from_sunday();
        if min_set.contains(&m)
            && hour_set.contains(&h)
            && dom_set.contains(&d)
            && mon_set.contains(&mo)
            && dow_set.contains(&dw)
        {
            return Ok(cursor);
        }
        cursor += Duration::minutes(1);
    }
    Err("cron expression did not match within 366 days".to_string())
}

fn parse_cron_field(expr: &str, min: u32, max: u32) -> Result<Vec<u32>, String> {
    let mut vals = Vec::new();
    for segment in expr.split(',') {
        let seg = segment.trim();
        let (range_part, step) = if let Some((l, r)) = seg.split_once('/') {
            let s: u32 = r.parse().map_err(|_| format!("invalid cron step '{r}'"))?;
            if s == 0 {
                return Err("cron step cannot be 0".to_string());
            }
            (l, s)
        } else {
            (seg, 1)
        };

        let (start, end) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            let sa: u32 = a.parse().map_err(|_| format!("invalid cron value '{a}'"))?;
            let sb: u32 = b.parse().map_err(|_| format!("invalid cron value '{b}'"))?;
            (sa.max(min), sb.min(max))
        } else {
            let v: u32 = range_part
                .parse()
                .map_err(|_| format!("invalid cron value '{range_part}'"))?;
            if v < min || v > max {
                return Err(format!("cron value {v} out of range {min}..={max}"));
            }
            (v, v)
        };

        let mut v = start;
        while v <= end {
            if !vals.contains(&v) {
                vals.push(v);
            }
            v = v.saturating_add(step);
        }
    }
    if vals.is_empty() {
        return Err(format!("empty cron field '{expr}'"));
    }
    Ok(vals)
}
