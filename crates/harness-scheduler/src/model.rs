//! Schedule data model, recurrence parsing, and delivery targets.

use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveTime, TimeZone, Timelike, Utc,
};
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
    /// - `"daily-local:<HH:MM>"` (device local daily)
    /// - `"daily:<HH:MM>"` (UTC daily)
    /// - `"local-cron:<min> <hour> <dom> <mon> <dow>"` (5-field device local cron)
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
    if let Some(hm) = trimmed.strip_prefix("daily-local:") {
        let time = NaiveTime::parse_from_str(hm.trim(), "%H:%M")
            .map_err(|e| format!("invalid daily-local HH:MM '{hm}': {e}"))?;
        return compute_daily_local(time, after).map(Some);
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
    if let Some(expr) = trimmed.strip_prefix("local-cron:") {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() == 5 {
            return next_local_cron_match(&parts, after).map(Some);
        }
        return Err(format!(
            "invalid local-cron rule '{trimmed}': expected 5 fields, got {}",
            parts.len()
        ));
    }

    // 5-field cron: min hour dom mon dow
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() == 5 {
        return next_cron_match(&parts, after).map(Some);
    }

    Err(format!(
        "unsupported schedule rule '{trimmed}'. Use 'once:<RFC3339>', 'every:<secs>', 'daily-local:<HH:MM>', 'daily:<HH:MM>', 'local-cron:<expr>', or 5-field cron."
    ))
}

fn compute_daily_local(
    target_time: NaiveTime,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let after_local = after.with_timezone(&Local);
    let today = after_local.date_naive();
    if let Some(dt) = resolve_local_time(today, target_time) {
        if dt.with_timezone(&Utc) > after {
            return Ok(dt.with_timezone(&Utc));
        }
    }
    for day_offset in 1..=3 {
        if let Some(day) = today.checked_add_signed(Duration::days(day_offset)) {
            if let Some(dt) = resolve_local_time(day, target_time) {
                if dt.with_timezone(&Utc) > after {
                    return Ok(dt.with_timezone(&Utc));
                }
            }
        }
    }
    Err("failed to resolve daily-local timestamp".to_string())
}

fn resolve_local_time(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    let naive_dt = date.and_time(time);
    match Local.from_local_datetime(&naive_dt) {
        LocalResult::Single(dt) => Some(dt),
        LocalResult::Ambiguous(earliest, _latest) => Some(earliest),
        LocalResult::None => {
            let mut probe = time;
            for _ in 0..120 {
                probe += Duration::minutes(1);
                let probe_dt = date.and_time(probe);
                match Local.from_local_datetime(&probe_dt) {
                    LocalResult::Single(dt) => return Some(dt),
                    LocalResult::Ambiguous(earliest, _) => return Some(earliest),
                    LocalResult::None => continue,
                }
            }
            None
        }
    }
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

fn next_local_cron_match(parts: &[&str], after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let min_set = parse_cron_field(parts[0], 0, 59)?;
    let hour_set = parse_cron_field(parts[1], 0, 23)?;
    let dom_set = parse_cron_field(parts[2], 1, 31)?;
    let mon_set = parse_cron_field(parts[3], 1, 12)?;
    let dow_set = parse_cron_field(parts[4], 0, 6)?;

    let after_local = after.with_timezone(&Local);
    let mut cursor = (after_local + Duration::minutes(1))
        .with_second(0)
        .and_then(|d| d.with_nanosecond(0))
        .ok_or_else(|| "invalid local cron start time".to_string())?;

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
            return Ok(cursor.with_timezone(&Utc));
        }
        cursor += Duration::minutes(1);
    }
    Err("local cron expression did not match within 366 days".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Local, NaiveTime, TimeZone, Timelike};

    #[test]
    fn test_daily_local_schedules_at_device_local_time() {
        let now_local = Local::now();
        let target_hour = (now_local.hour() + 2) % 24;
        let rule = format!("daily-local:{:02}:00", target_hour);
        let next_utc = compute_next_run(&rule, Utc::now())
            .expect("rule must be valid")
            .expect("should produce next run");
        let next_local = next_utc.with_timezone(&Local);
        assert_eq!(next_local.hour(), target_hour);
        assert_eq!(next_local.minute(), 0);
        assert_eq!(next_local.second(), 0);
        assert!(next_utc > Utc::now());
    }

    #[test]
    fn test_local_cron_schedules_at_device_local_time() {
        let now_local = Local::now();
        let target_hour = (now_local.hour() + 1) % 24;
        let rule = format!("local-cron:15 {:02} * * *", target_hour);
        let next_utc = compute_next_run(&rule, Utc::now())
            .expect("rule must be valid")
            .expect("should produce next run");
        let next_local = next_utc.with_timezone(&Local);
        assert_eq!(next_local.hour(), target_hour);
        assert_eq!(next_local.minute(), 15);
        assert_eq!(next_local.second(), 0);
        assert!(next_utc > Utc::now());
    }

    #[test]
    fn test_legacy_daily_remains_strictly_utc() {
        let now = Utc::now();
        let target_hour = (now.hour() + 3) % 24;
        let rule = format!("daily:{:02}:00", target_hour);
        let next_utc = compute_next_run(&rule, now)
            .expect("rule must be valid")
            .expect("should produce next run");
        assert_eq!(next_utc.hour(), target_hour);
        assert_eq!(next_utc.minute(), 0);
    }

    #[test]
    fn test_invalid_local_rules_are_rejected() {
        assert!(compute_next_run("daily-local:25:00", Utc::now()).is_err());
        assert!(compute_next_run("daily-local:bad", Utc::now()).is_err());
        assert!(compute_next_run("local-cron:* * *", Utc::now()).is_err());
    }
}
