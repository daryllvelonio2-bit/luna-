//! Human-in-the-loop approval for mutating tool calls.
//!
//! The gate is a [`Approver`] trait object injected into [`crate::Agent`]: the agent loop asks it
//! *before* a mutating tool runs and turns any answer other than [`ApprovalDecision::Run`] into a
//! normal `ToolResult::error(call_id, "approval denied…")`, so the model can react to a denial
//! instead of the harness aborting.
//!
//! Two knobs, deliberately orthogonal (see `docs/TRENDS_2026.md` §2.2): *this* module decides
//! **whether** a mutating call runs (`agent.require_hitl_for_mutation`, `tools.approval`); the
//! sandbox owned by the shell card decides **what a call can reach once it runs**. Neither
//! substitutes for the other — a one-keypress prompt is not a boundary, and a boundary is not a
//! human decision.
//!
//! Fail closed everywhere: an unreachable human ([`NonInteractive`]) is an error, a registry that
//! cannot report mutability is treated as mutating, and no answer inside the timeout is a denial.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::HarnessError;

/// Default for `tools.approval_timeout_secs`; the single source of truth for that key's default.
pub const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 120;

/// What the human is shown, and what a hook or test approver gets to decide on.
///
/// `target` and `diff` come from [`crate::Tool::preview`] — never from the tool's argument blob, so
/// a tool decides what a human needs to see (a path, an exact command, a unified diff).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool: String,
    pub call_id: String,
    /// The path that will be written, or the exact command that will run.
    pub target: Option<String>,
    /// Unified diff for a patch-shaped call, rendered by [`unified_diff`].
    pub diff: Option<String>,
    /// The call's arguments, so an interactive approver can offer an edit. Never logged.
    pub arguments: serde_json::Value,
}

/// The human's answer.
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalDecision {
    /// Run the call as the model asked.
    Run,
    /// Run it with replacement arguments (the "edit-and-run" answer).
    RunWithArguments(serde_json::Value),
    /// Do not run it.
    Deny,
}

/// Decides whether a mutating call runs.
///
/// Returning `Err(HarnessError::ApprovalRejected(reason))` is the no-human-available answer and is
/// treated exactly like a denial, with `reason` carried into the tool result and the log.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError>;
}

/// Approves every mutating call: `tools.approval = allow`, or `--yes`.
pub struct AllowAll;

#[async_trait]
impl Approver for AllowAll {
    async fn approve(&self, _request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        Ok(ApprovalDecision::Run)
    }
}

/// Refuses every mutating call without asking: `tools.approval = deny`.
pub struct DenyAll;

#[async_trait]
impl Approver for DenyAll {
    async fn approve(&self, _request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        Ok(ApprovalDecision::Deny)
    }
}

/// No interactive human is reachable (stdin is not a terminal, cron/background turn, hook not
/// installed). The fail-closed answer, and the producer of
/// [`HarnessError::ApprovalRejected`].
pub struct NonInteractive {
    reason: String,
}

impl NonInteractive {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl Approver for NonInteractive {
    async fn approve(&self, _request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        Err(HarnessError::ApprovalRejected(self.reason.clone()))
    }
}

/// A synchronous closure as an approver — tests, and embedders that already have a policy function.
/// A closure cannot await, which is the point: a decision that needs IO should be its own
/// [`Approver`] impl.
pub struct FnApprover<F>(pub F);

#[async_trait]
impl<F> Approver for FnApprover<F>
where
    F: Fn(&ApprovalRequest) -> ApprovalDecision + Send + Sync,
{
    async fn approve(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        Ok((self.0)(request))
    }
}

/// `tools.approval`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Ask the human for every mutating call (default).
    #[default]
    Ask,
    /// Run mutating calls without asking. Only defensible behind a real sandbox boundary.
    Allow,
    /// Never run a mutating call, and do not ask.
    Deny,
}

/// The gate knobs the agent loop needs, resolved from `tools.approval_timeout_secs` and
/// `tools.mutating_overrides`.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalSettings {
    /// No answer within this window counts as a denial.
    pub timeout: Duration,
    /// Tool names the gate must treat as mutating even when the tool says otherwise.
    pub mutating_overrides: Vec<String>,
}

impl Default for ApprovalSettings {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
            mutating_overrides: Vec::new(),
        }
    }
}

/// What a tool contributes to the prompt before it runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolPreview {
    pub target: Option<String>,
    pub diff: Option<String>,
}

/// A unified diff of `old` → `new`, labelled with `path`, for a human to read before saying yes.
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    use similar::TextDiff;

    TextDiff::from_lines(old, new)
        .unified_diff()
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_diff_marks_the_changed_lines() {
        let diff = unified_diff("one\ntwo\n", "one\nthree\n", "notes.txt");

        assert!(diff.contains("--- a/notes.txt"), "{diff}");
        assert!(diff.contains("+++ b/notes.txt"), "{diff}");
        assert!(diff.contains("-two"), "{diff}");
        assert!(diff.contains("+three"), "{diff}");
        assert!(diff.contains(" one"), "{diff}");
    }

    #[test]
    fn unified_diff_of_identical_text_has_no_hunks() {
        let diff = unified_diff("same\n", "same\n", "notes.txt");

        assert!(!diff.contains("@@"), "{diff}");
    }

    #[test]
    fn approval_mode_parses_the_three_documented_values() {
        for (text, expected) in [
            ("ask", ApprovalMode::Ask),
            ("allow", ApprovalMode::Allow),
            ("deny", ApprovalMode::Deny),
        ] {
            let parsed: ApprovalMode = serde_yaml::from_str(text).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[tokio::test]
    async fn non_interactive_approver_produces_approval_rejected() {
        let approver = NonInteractive::new("stdin is not a terminal");
        let error = approver
            .approve(&ApprovalRequest::default())
            .await
            .unwrap_err();

        assert!(matches!(error, HarnessError::ApprovalRejected(_)), "{error:?}");
        assert!(error.to_string().contains("stdin is not a terminal"), "{error}");
    }
}
