use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::error::HarnessError;
use crate::traits::Tool;
use crate::types::ToolResult;

/// Maximum duration a single `sleep` tool call may pause (24 hours).
/// Continuous agents should sleep in bounded chunks so context compaction,
/// checkpoints, and interrupts remain responsive.
pub const MAX_SLEEP_SECONDS: f64 = 86_400.0;
pub const MIN_SLEEP_SECONDS: f64 = 0.05;

/// Cancellation-aware pause tool for continuous/never-stopping automations,
/// rate-limit backoff, polling loops, and waiting between work cycles.
pub struct SleepTool;

#[async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep"
    }

    fn description(&self) -> &str {
        "Pause execution for a specified number of seconds without spinning. Cancellation-aware: wakes immediately if paused, cancelled, or interrupted. Ideal for continuous/never-stopping workflows, polling intervals, cooldowns, and waiting for working windows."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "seconds": {
                    "type": "number",
                    "description": "Duration to sleep in seconds (0.05 to 86400). Use bounded intervals (e.g. 30-1800s) for responsive continuous loops."
                },
                "reason": {
                    "type": "string",
                    "description": "Optional short note describing why the agent is sleeping (e.g. 'Waiting 5m before next check')."
                }
            },
            "required": ["seconds"]
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let Some(raw_secs) = arguments.get("seconds").and_then(|v| v.as_f64()) else {
            return Ok(ToolResult::error(
                call_id,
                "Missing or invalid numeric 'seconds' parameter",
            ));
        };
        if !raw_secs.is_finite() || raw_secs <= 0.0 {
            return Ok(ToolResult::error(
                call_id,
                "'seconds' must be a positive finite number",
            ));
        }
        if raw_secs > MAX_SLEEP_SECONDS {
            return Ok(ToolResult::error(
                call_id,
                format!(
                    "'seconds' ({raw_secs}) exceeds single-call maximum of {MAX_SLEEP_SECONDS}s (24h). Sleep in smaller chunks so the agent stays responsive."
                ),
            ));
        }
        let secs = raw_secs.max(MIN_SLEEP_SECONDS);
        let reason = arguments
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let started = Instant::now();
        let duration = Duration::from_secs_f64(secs);

        tokio::select! {
            _ = tokio::time::sleep(duration) => {
                let elapsed = started.elapsed().as_secs_f64();
                let msg = match reason {
                    Some(r) => format!("Slept for {elapsed:.2}s (reason: {r}). Ready to continue."),
                    None => format!("Slept for {elapsed:.2}s. Ready to continue."),
                };
                Ok(ToolResult::success(call_id, msg))
            }
            _ = cancel_token.cancelled() => {
                let elapsed = started.elapsed().as_secs_f64();
                let msg = match reason {
                    Some(r) => format!("Sleep cancelled after {elapsed:.2}s (reason: {r}). Stop or wrap up cleanly."),
                    None => format!("Sleep cancelled after {elapsed:.2}s. Stop or wrap up cleanly."),
                };
                Ok(ToolResult::error(call_id, msg))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sleep_completes_and_reports_reason() {
        let tool = SleepTool;
        let cancel = CancellationToken::new();
        let res = tool
            .execute(
                "c1",
                &json!({ "seconds": 0.06, "reason": "cooldown" }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(!res.is_error, "{}", res.content);
        assert!(res.content.contains("Slept for"));
        assert!(res.content.contains("cooldown"));
    }

    #[tokio::test]
    async fn sleep_wakes_immediately_on_cancellation() {
        let tool = SleepTool;
        let cancel = CancellationToken::new();
        let child = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            child.cancel();
        });
        let started = Instant::now();
        let res = tool
            .execute(
                "c2",
                &json!({ "seconds": 30, "reason": "polling" }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(res.is_error);
        assert!(res.content.contains("cancelled"));
    }

    #[tokio::test]
    async fn sleep_rejects_invalid_durations() {
        let tool = SleepTool;
        let cancel = CancellationToken::new();
        assert!(
            tool.execute("c3", &json!({ "seconds": -1 }), &cancel)
                .await
                .unwrap()
                .is_error
        );
        assert!(
            tool.execute("c4", &json!({ "seconds": 99999 }), &cancel)
                .await
                .unwrap()
                .is_error
        );
    }
}
