//! Schedule data model, recurrence parsing, active working-time windows,
//! execution modes, and delivery targets.

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike, Utc, Weekday};
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

/// How an automation runs across cycles.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Fires on each scheduled tick (`once:`, `every:`, `daily:`, `daily-local:`, cron).
    #[default]
    Scheduled,
    /// Never-stop persistent worker mode: runs continuously (using the `sleep` tool inside
    /// turns and automatically resuming after a short cooldown if a turn yields), subject to
    /// `active_window`, `trigger_command`, and `paused`.
    Continuous,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionMode::Scheduled => "scheduled",
            ExecutionMode::Continuous => "continuous",
        }
    }

    pub fn from_str_lossy(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "scheduled" | "schedule" | "cron" => Ok(ExecutionMode::Scheduled),
            "continuous" | "never_stop" | "never-stop" | "loop" | "daemon" => {
                Ok(ExecutionMode::Continuous)
            }
            other => Err(format!(
                "invalid execution_mode '{other}': expected 'scheduled' or 'continuous'"
            )),
        }
    }
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
    /// - `"daily-local:<HH:MM>"` (host local wall-clock daily)
    /// - `"<min> <hour> <dom> <mon> <dow>"` (5-field UTC cron)
    /// - `"local-cron:<min> <hour> <dom> <mon> <dow>"` (5-field local wall-clock cron)
    /// - `"continuous"` or `"continuous:<cooldown_secs>"` (never-stop loop cooldown)
    pub rule: String,
    pub delivery_target: DeliveryTarget,
    #[serde(default)]
    pub plugin_id: Option<String>,
    /// Whether tool calls during scheduled runs ask for interactive permission.
    /// Defaults to `false` so automations run autonomously without blocking.
    #[serde(default)]
    pub require_approval: bool,
    /// Optional Luna profile name (`~/.luna/profiles/<name>/`) governing persona,
    /// mindset/behavior, and model overrides for this automation.
    #[serde(default)]
    pub profile: Option<String>,
    /// Scheduled vs Continuous never-stop execution mode.
    #[serde(default)]
    pub execution_mode: ExecutionMode,
    /// Optional targeted working-time window (e.g. `"Mon-Fri 09:00-17:00"`, `"22:00-06:00"`,
    /// `"weekdays 08:30-18:00"`, `"UTC 09:00-17:00"`). Local device time unless `UTC` is specified.
    #[serde(default)]
    pub active_window: Option<String>,
    /// Optional lightweight shell command pre-check (`sh -c`). Exit 0 triggers the LLM turn;
    /// non-zero skips this tick without burning tokens.
    #[serde(default)]
    pub trigger_command: Option<String>,
    /// Optional human/agent-readable trigger, escalation, and self-stopping logic description.
    #[serde(default)]
    pub trigger_logic: Option<String>,
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
    if trimmed.eq_ignore_ascii_case("continuous") {
        return Ok(Some(after + Duration::seconds(10)));
    }
    if let Some(cooldown_raw) = trimmed.strip_prefix("continuous:") {
        let secs: u64 = cooldown_raw
            .trim()
            .parse()
            .map_err(|e| format!("invalid continuous cooldown seconds '{cooldown_raw}': {e}"))?;
        let bounded = secs.clamp(1, 86_400);
        return Ok(Some(after + Duration::seconds(bounded as i64)));
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
    if let Some(hm) = trimmed.strip_prefix("daily-local:") {
        let time = NaiveTime::parse_from_str(hm.trim(), "%H:%M")
            .map_err(|e| format!("invalid daily-local HH:MM '{hm}': {e}"))?;
        let local_after = after.with_timezone(&Local);
        let mut day = local_after.date_naive();
        for _ in 0..3 {
            if let Some(dt) = resolve_local_datetime(day, time) {
                if dt > after {
                    return Ok(Some(dt));
                }
            }
            day = day
                .succ_opt()
                .ok_or_else(|| "date overflow computing daily-local".to_string())?;
        }
        return Err(format!(
            "could not resolve next daily-local time for '{hm}'"
        ));
    }
    if let Some(cron_raw) = trimmed.strip_prefix("local-cron:") {
        let parts: Vec<&str> = cron_raw.split_whitespace().collect();
        if parts.len() == 5 {
            return next_local_cron_match(&parts, after).map(Some);
        }
        return Err(format!(
            "invalid local-cron expression '{cron_raw}': expected 5 fields (min hour dom mon dow)"
        ));
    }

    // 5-field UTC cron: min hour dom mon dow
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() == 5 {
        return next_cron_match(&parts, after).map(Some);
    }

    Err(format!(
        "unsupported schedule rule '{trimmed}'. Use 'once:<RFC3339>', 'every:<secs>', 'daily:<HH:MM>', 'daily-local:<HH:MM>', 'continuous[:<secs>]', 'local-cron:<5-field>', or 5-field UTC cron."
    ))
}

/// Compute the next run timestamp factoring in an optional `active_window`.
pub fn compute_effective_next_run(
    rule: &str,
    active_window: Option<&str>,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, String> {
    let Some(candidate) = compute_next_run(rule, after)? else {
        return Ok(None);
    };
    let Some(win) = active_window.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Some(candidate));
    };
    validate_active_window(win)?;
    if is_within_active_window(win, candidate)? {
        return Ok(Some(candidate));
    }

    let trimmed = rule.trim();
    if trimmed.eq_ignore_ascii_case("continuous")
        || trimmed.starts_with("continuous:")
        || trimmed.starts_with("every:")
        || trimmed.starts_with("once:")
    {
        return next_active_window_start(win, candidate).map(Some);
    }

    // For daily / cron schedules, advance through subsequent scheduled fires (up to 60)
    // until one lands inside the active window; otherwise fall back to next window start.
    let mut probe = candidate;
    for _ in 0..60 {
        if let Some(next_try) = compute_next_run(rule, probe)? {
            if is_within_active_window(win, next_try)? {
                return Ok(Some(next_try));
            }
            probe = next_try;
        } else {
            break;
        }
    }
    next_active_window_start(win, candidate).map(Some)
}

pub fn validate_rule(rule: &str) -> Result<(), String> {
    let _ = compute_next_run(rule, Utc::now())?;
    Ok(())
}

fn resolve_local_datetime(day: chrono::NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let naive = day.and_time(time);
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earliest, _) => Some(earliest.with_timezone(&Utc)),
        chrono::LocalResult::None => {
            // Spring-forward DST gap: shift 1 hour forward so daily jobs still run that day.
            let shifted = naive + Duration::hours(1);
            Local
                .from_local_datetime(&shifted)
                .earliest()
                .map(|dt| dt.with_timezone(&Utc))
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveWindowSpec {
    /// Allowed days of week indexed as `Weekday::num_days_from_sunday()` (0=Sun .. 6=Sat).
    allowed_days: [bool; 7],
    start_min: u32,
    end_min: u32,
    use_utc: bool,
}

fn parse_weekday_token(tok: &str) -> Result<u32, String> {
    match tok.trim().to_ascii_lowercase().as_str() {
        "sun" | "sunday" | "0" | "7" => Ok(0),
        "mon" | "monday" | "1" => Ok(1),
        "tue" | "tues" | "tuesday" | "2" => Ok(2),
        "wed" | "wednesday" | "3" => Ok(3),
        "thu" | "thur" | "thurs" | "thursday" | "4" => Ok(4),
        "fri" | "friday" | "5" => Ok(5),
        "sat" | "saturday" | "6" => Ok(6),
        other => Err(format!("unknown weekday '{other}' in active_window")),
    }
}

fn parse_days_spec(spec: &str) -> Result<[bool; 7], String> {
    let mut days = [false; 7];
    let lower = spec.trim().to_ascii_lowercase();
    match lower.as_str() {
        "" | "daily" | "everyday" | "all" | "24/7" => return Ok([true; 7]),
        "weekdays" | "weekday" | "workdays" => {
            days[1..=5].fill(true);
            return Ok(days);
        }
        "weekends" | "weekend" => {
            days[0] = true;
            days[6] = true;
            return Ok(days);
        }
        _ => {}
    }

    for part in lower.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if let Some((a, b)) = p.split_once('-') {
            let start = parse_weekday_token(a)?;
            let end = parse_weekday_token(b)?;
            let mut cur = start;
            for _ in 0..7 {
                days[cur as usize] = true;
                if cur == end {
                    break;
                }
                cur = (cur + 1) % 7;
            }
        } else {
            let idx = parse_weekday_token(p)?;
            days[idx as usize] = true;
        }
    }

    if !days.iter().any(|&d| d) {
        return Err(format!("no valid days parsed from '{spec}'"));
    }
    Ok(days)
}

fn parse_hhmm_minutes(s: &str) -> Result<u32, String> {
    let trimmed = s.trim();
    if trimmed == "24:00" {
        return Ok(24 * 60);
    }
    let t = NaiveTime::parse_from_str(trimmed, "%H:%M")
        .map_err(|e| format!("invalid time '{trimmed}' in active_window (expected HH:MM): {e}"))?;
    Ok(t.hour() * 60 + t.minute())
}

fn parse_active_window(raw: &str) -> Result<ActiveWindowSpec, String> {
    let mut tokens: Vec<&str> = raw.split_whitespace().collect();
    if tokens.is_empty() {
        return Err("active_window cannot be empty".to_string());
    }

    let mut use_utc = false;
    tokens.retain(|tok| {
        if tok.eq_ignore_ascii_case("UTC") {
            use_utc = true;
            false
        } else {
            !tok.eq_ignore_ascii_case("LOCAL")
        }
    });

    let range_idx = tokens
        .iter()
        .position(|t| t.contains('-') && t.contains(':'))
        .ok_or_else(|| {
            format!(
                "invalid active_window '{raw}': expected a time range like '09:00-17:00' or 'Mon-Fri 09:00-17:00'"
            )
        })?;
    let range_tok = tokens.remove(range_idx);
    let (start_str, end_str) = range_tok
        .split_once('-')
        .ok_or_else(|| format!("invalid time range '{range_tok}'"))?;
    let start_min = parse_hhmm_minutes(start_str)?;
    let end_min = parse_hhmm_minutes(end_str)?;
    if start_min == end_min {
        return Err(format!(
            "active_window start and end cannot be identical ('{range_tok}'); omit active_window for 24/7"
        ));
    }

    let days_spec = tokens.join(" ");
    let allowed_days = parse_days_spec(&days_spec)?;
    Ok(ActiveWindowSpec {
        allowed_days,
        start_min,
        end_min,
        use_utc,
    })
}

pub fn validate_active_window(spec: &str) -> Result<(), String> {
    let _ = parse_active_window(spec)?;
    Ok(())
}

fn eval_window_at(spec: &ActiveWindowSpec, at: DateTime<Utc>) -> bool {
    let (wd, minute_of_day) = if spec.use_utc {
        (at.weekday(), at.hour() * 60 + at.minute())
    } else {
        let local = at.with_timezone(&Local);
        (local.weekday(), local.hour() * 60 + local.minute())
    };
    let day_idx = wd.num_days_from_sunday() as usize;

    if spec.start_min < spec.end_min {
        spec.allowed_days[day_idx]
            && minute_of_day >= spec.start_min
            && minute_of_day < spec.end_min
    } else {
        // Overnight window (e.g. 22:00-06:00): after start on an allowed day, OR before end when
        // yesterday was an allowed day.
        if minute_of_day >= spec.start_min {
            spec.allowed_days[day_idx]
        } else if minute_of_day < spec.end_min {
            let prev_idx = (day_idx + 6) % 7;
            spec.allowed_days[prev_idx]
        } else {
            false
        }
    }
}

pub fn is_within_active_window(spec: &str, at: DateTime<Utc>) -> Result<bool, String> {
    let parsed = parse_active_window(spec)?;
    Ok(eval_window_at(&parsed, at))
}

pub fn next_active_window_start(spec: &str, after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let parsed = parse_active_window(spec)?;
    if eval_window_at(&parsed, after) {
        return Ok(after);
    }
    let mut cursor = (after + Duration::minutes(1))
        .with_second(0)
        .and_then(|d| d.with_nanosecond(0))
        .ok_or_else(|| "invalid timestamp in active_window scan".to_string())?;

    // Scan up to 14 days (20,160 minutes)
    for _ in 0..20_160 {
        if eval_window_at(&parsed, cursor) {
            return Ok(cursor);
        }
        cursor += Duration::minutes(1);
    }
    Err(format!(
        "active_window '{spec}' did not open within 14 days"
    ))
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

    let mut cursor = (after + Duration::minutes(1))
        .with_second(0)
        .and_then(|d| d.with_nanosecond(0))
        .ok_or_else(|| "invalid local-cron start time".to_string())?;

    for _ in 0..527_040 {
        let local = cursor.with_timezone(&Local);
        let m = local.minute();
        let h = local.hour();
        let d = local.day();
        let mo = local.month();
        let dw = local.weekday().num_days_from_sunday();
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
    Err("local-cron expression did not match within 366 days".to_string())
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

pub fn weekday_name_hint(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parses_local_and_continuous_rules() {
        let now = Utc::now();
        assert!(compute_next_run("daily-local:09:30", now)
            .unwrap()
            .is_some());
        assert!(compute_next_run("local-cron:0 9 * * 1-5", now)
            .unwrap()
            .is_some());
        assert!(compute_next_run("continuous", now).unwrap().is_some());
        assert!(compute_next_run("continuous:5", now).unwrap().is_some());
    }

    #[test]
    fn evaluates_utc_and_overnight_active_windows() {
        validate_active_window("Mon-Fri 09:00-17:00 UTC").unwrap();
        validate_active_window("22:00-06:00 UTC").unwrap();
        validate_active_window("weekdays 08:30-18:00").unwrap();

        // 2026-11-16 is a Monday
        let mon_10 = Utc.with_ymd_and_hms(2026, 11, 16, 10, 0, 0).unwrap();
        let mon_20 = Utc.with_ymd_and_hms(2026, 11, 16, 20, 0, 0).unwrap();
        let sat_10 = Utc.with_ymd_and_hms(2026, 11, 21, 10, 0, 0).unwrap();

        assert!(is_within_active_window("Mon-Fri 09:00-17:00 UTC", mon_10).unwrap());
        assert!(!is_within_active_window("Mon-Fri 09:00-17:00 UTC", mon_20).unwrap());
        assert!(!is_within_active_window("Mon-Fri 09:00-17:00 UTC", sat_10).unwrap());

        // Overnight wrap: 23:00 and 05:00 are inside 22:00-06:00
        let mon_23 = Utc.with_ymd_and_hms(2026, 11, 16, 23, 0, 0).unwrap();
        let tue_05 = Utc.with_ymd_and_hms(2026, 11, 17, 5, 0, 0).unwrap();
        assert!(is_within_active_window("22:00-06:00 UTC", mon_23).unwrap());
        assert!(is_within_active_window("22:00-06:00 UTC", tue_05).unwrap());
        assert!(!is_within_active_window("22:00-06:00 UTC", mon_10).unwrap());

        let next_open = next_active_window_start("Mon-Fri 09:00-17:00 UTC", sat_10).unwrap();
        assert_eq!(next_open.weekday(), Weekday::Mon);
        assert_eq!(next_open.hour(), 9);
    }
}
