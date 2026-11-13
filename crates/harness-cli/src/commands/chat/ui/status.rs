use colored::Colorize;
use harness_providers::QuotaLine;

use super::super::status::format_quota_lines;
use super::metrics::humanize_tokens;
use super::panels::ChatStatus;
use super::view::ChatUi;

impl ChatUi {
    pub fn set_status(&self, model: &str, context_used: u32, context_max: u32) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        let (yolo, plan_mode, goal_progress, session_name, account_name, quota, quota_drawn) =
            match status.as_ref() {
                Some(s) => (
                    s.yolo,
                    s.plan_mode,
                    s.goal_progress,
                    s.session_name.clone(),
                    s.account_name.clone(),
                    s.quota.clone(),
                    s.quota_drawn.clone(),
                ),
                None => (
                    false,
                    false,
                    None,
                    String::new(),
                    None,
                    Vec::new(),
                    String::new(),
                ),
            };
        *status = Some(ChatStatus {
            model: model.to_string(),
            account_name,
            quota,
            quota_drawn,
            context_used,
            context_max,
            allow_context_drop: false,
            yolo,
            plan_mode,
            goal_progress,
            session_name,
        });
    }
    pub fn set_account_name(&self, account: Option<String>) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                s.account_name = account;
            }
        }
        self.redraw_status_line();
    }
    /// Show the quota windows `model` is metered against.
    ///
    /// A payload for a model that is no longer the one in the bar is dropped: a refresh that was in
    /// flight when `/model` was used would otherwise paint the old model's numbers under the new
    /// model's name. An empty payload is always taken, because that is a caller saying there is
    /// nothing to show — a provider with no quota readout, or a credential that is gone.
    pub fn set_quota_lines(&self, model: &str, lines: Vec<QuotaLine>) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                if !lines.is_empty() && s.model != model {
                    return;
                }
                s.quota = lines;
                // Empty means "not drawn yet", so the tick notices the first real text.
                s.quota_drawn.clear();
            }
        }
        self.redraw_status_line();
    }
    /// Repaint the status line when the quota countdown has moved.
    ///
    /// The readout carries a reset countdown, and the countdown is only as fresh as the last paint
    /// — while a turn streams that is many times a second, but an idle composer paints nothing at
    /// all (the animation tick returns early with no live rows). This is the idle half: re-render
    /// the text and ask for a repaint only when a minute of it has actually passed.
    pub fn refresh_quota_clock(&self) {
        let changed = {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            match status.as_mut() {
                Some(s) if !s.quota.is_empty() => {
                    let rendered = format_quota_lines(&s.quota);
                    if rendered == s.quota_drawn {
                        false
                    } else {
                        s.quota_drawn = rendered;
                        true
                    }
                }
                _ => false,
            }
        };
        if changed {
            self.redraw_status_line();
        }
    }
    pub fn set_model(&self, model: &str, context_used: u32, context_max: u32) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut s) = *status {
            s.model = model.to_string();
            s.context_used = context_used;
            s.context_max = context_max;
            s.allow_context_drop = false;
        }
    }
    pub fn set_session_name(&self, name: &str) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                s.session_name = name.to_string();
            }
        }
        self.redraw_status_line();
    }
    /// Refresh the gauge at a turn boundary.
    ///
    /// The used figure only ever rises: a boundary recomputes from a different source than the
    /// running turn (the provider's count against the character heuristic, or a stored report
    /// without the tail), and letting that correction paint a smaller number made the context bar
    /// look like it had reset.
    ///
    /// A fold is the exception. It lands mid-turn, when the running turn is still on the transcript
    /// the fold replaced, so the drop is deferred: [`Self::defer_context_drop`] records it and the
    /// next boundary — by which time the turn has adopted the folded transcript — takes the smaller
    /// figure. The window always follows the model, so it is taken as given either way.
    pub fn update_status(&self, context_used: u32, context_max: u32) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                s.context_used = if s.allow_context_drop {
                    context_used
                } else {
                    s.context_used.max(context_used)
                };
                s.allow_context_drop = false;
                s.context_max = context_max;
            }
        }
        self.redraw_status_line();
    }
    /// Record a mid-turn estimate. This never lowers the gauge: a provider measures the prompt it
    /// was sent, and a running turn may still be on a transcript a fold has already replaced.
    pub fn update_context_used(&self, context_used: u32) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                s.context_used = s.context_used.max(context_used);
            }
        }
        self.redraw_status_line();
    }
    /// Allow the next turn boundary to take a figure lower than the gauge.
    ///
    /// Called when a fold lands: the summary really did retire history, but the running turn still
    /// holds the transcript it replaced. Waiting for the boundary keeps that round from painting a
    /// shrunken size and then jumping back to the one still in use.
    pub fn defer_context_drop(&self) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut s) = *status {
            s.allow_context_drop = true;
        }
    }
    /// Take a context figure that may be lower than the one on screen, right now.
    ///
    /// Only an explicit fold between turns (`/compact`) calls this: the value is the transcript
    /// that will be sent next, and no running turn can contradict it.
    pub fn reset_context_used(&self, context_used: u32) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                s.context_used = context_used;
                s.allow_context_drop = false;
            }
        }
        self.redraw_status_line();
    }
    pub fn set_yolo(&self, on: bool) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                s.yolo = on;
            }
        }
        self.redraw_status_line();
    }
    pub fn set_plan_mode(&self, on: bool) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                s.plan_mode = on;
            }
        }
        self.redraw_status_line();
    }
    pub fn set_goal_status(&self, progress: Option<(usize, usize)>) {
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut s) = *status {
                s.goal_progress = progress;
            }
        }
        self.redraw_status_line();
    }
}

impl ChatUi {
    /// The line that says messages are waiting, drawn above the status line.
    ///
    /// A message the user submitted while the agent could not take it is acknowledged here instead
    /// of the transcript: it has not been sent, so it does not belong in the conversation yet, but
    /// silently swallowing it would look like the keystroke was lost.
    pub fn format_queue_line(&self, width: usize) -> Option<String> {
        let queued = self.queue_depth();
        if queued == 0 {
            return None;
        }
        let left = format!(
            "{} {} queued — sent when the agent is free",
            "⏸".yellow().bold(),
            queued
        );
        let left_visible = dialoguer::console::strip_ansi_codes(&left).to_string();
        if dialoguer::console::measure_text_width(&left_visible) <= width {
            return Some(left);
        }
        // Narrower than the sentence: keep the count, drop the explanation.
        let short_visible = format!("⏸ {} queued", queued);
        if dialoguer::console::measure_text_width(&short_visible) <= width {
            return Some(short_visible.yellow().to_string());
        }
        None
    }

    pub fn format_status_line(&self, width: usize) -> Option<String> {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        let s = status.as_ref()?;

        let left = status_left(s);
        let center = status_center(s);
        let right = status_right(s);

        Some(fit_status_row(&left, &center, &right, width))
    }
}

/// The left of the status line: the model, followed by the account when the provider named one.
fn status_left(s: &ChatStatus) -> String {
    if let Some(ref account) = s.account_name {
        if !account.is_empty() {
            format!("{} · {}", s.model, account)
        } else {
            s.model.clone()
        }
    } else {
        s.model.clone()
    }
}

/// The centred session name, bracketed, or empty when the session is unnamed.
fn status_center(s: &ChatStatus) -> String {
    if s.session_name.is_empty() {
        String::new()
    } else {
        format!("[{}]", s.session_name)
    }
}

/// The right of the status line: the quota readout or the context gauge, with mode badges.
///
/// If a provider quota display is available, format as:
/// "<used>/<max> · 5h <pct>% (<time>) · w <pct>% (<time>)"
/// Otherwise, fall back to the standard context percentage and humanized tokens.
///
/// The quota text is rendered here, at paint time, rather than stored as a string: it
/// carries a reset countdown, and a countdown rendered once at fetch time is a number that
/// stops moving.
fn status_right(s: &ChatStatus) -> String {
    let quota = format_quota_lines(&s.quota);
    let mut right = if !quota.is_empty() {
        if s.context_max > 0 {
            format!(
                "{}/{} · {}",
                humanize_tokens(s.context_used),
                humanize_tokens(s.context_max),
                quota
            )
        } else {
            format!("{} · {}", s.context_used, quota)
        }
    } else if s.context_max > 0 {
        let percent = (s.context_used as u64) * 100 / s.context_max as u64;
        format!(
            "ctx: {}% ({}/{})",
            percent,
            humanize_tokens(s.context_used),
            humanize_tokens(s.context_max)
        )
    } else {
        format!("ctx: {}/{}", s.context_used, s.context_max)
    };
    if let Some((iter, max)) = s.goal_progress {
        right = format!("{} {}", format!("goal {iter}/{max}").green().bold(), right);
    }
    if s.plan_mode {
        right = format!("{} {}", "plan".cyan().bold(), right);
    }
    if s.yolo {
        right = format!("{} {}", "yolo".yellow().bold(), right);
    }
    right
}

/// Lay the three parts out on one row: the centre is dropped before anything is allowed to wrap,
/// and the left is truncated when even that is not enough.
fn fit_status_row(left: &str, center: &str, right: &str, width: usize) -> String {
    let right_visible = dialoguer::console::strip_ansi_codes(right).to_string();
    let right_len = right_visible.chars().count();
    let left_len = left.chars().count();
    let center_len = center.chars().count();

    // If right is already wider than available width, shorten it so it never wraps.
    let (right_str, right_len) = if right_len > width {
        let cut = width.saturating_sub(1);
        let truncated = format!("{}…", right_visible.chars().take(cut).collect::<String>());
        let t_len = truncated.chars().count();
        (truncated.dimmed().to_string(), t_len)
    } else {
        (right.dimmed().to_string(), right_len)
    };

    if left_len + right_len + 4 > width {
        let available = width.saturating_sub(right_len + 2);
        let left_truncated = if left_len > available {
            if available == 0 {
                String::new()
            } else {
                let cut = available.saturating_sub(1);
                format!("{}…", left.chars().take(cut).collect::<String>())
            }
        } else {
            left.to_string()
        };
        let left_len = left_truncated.chars().count();
        let spaces = width.saturating_sub(left_len + right_len);
        format!(
            "{}{}{}",
            left_truncated.dimmed(),
            " ".repeat(spaces),
            right_str
        )
    } else if left_len + right_len + center_len + 8 > width {
        let spaces = width.saturating_sub(left_len + right_len);
        format!("{}{}{}", left.dimmed(), " ".repeat(spaces), right_str)
    } else {
        let free = width.saturating_sub(left_len + right_len);
        let left_gap = ((free.saturating_sub(center_len)) / 2).max(1);
        let right_gap = free.saturating_sub(center_len + left_gap);
        format!(
            "{}{}{}{}{}",
            left.dimmed(),
            " ".repeat(left_gap),
            center.dimmed().bold(),
            " ".repeat(right_gap),
            right_str
        )
    }
}
