//! The permission-rule + human approval gate that decides what happens to one tool call.

use tracing::{debug, warn};

use super::Agent;
use crate::approval::{ApprovalDecision, ApprovalRequest};
use crate::error::HarnessError;
use crate::permission::RuleVerdict;
use crate::traits::*;
use crate::types::ToolCall;

/// What the gate decided for one call.
pub(super) enum GateOutcome {
    /// Run it — possibly with the arguments the human edited.
    Run(Option<serde_json::Value>),
    /// Do not run it. The string is the reason, which goes into the tool result and the log.
    Denied(String),
}

/// What the gate can decide **without asking anyone**: the standing rules and the mutability
/// default, and nothing else.
///
/// Split out of [`GateOutcome`] so the tool dispatcher can tell a call that will be approved
/// silently (and may therefore run beside another read-only call) from one that needs the human —
/// without a second, drifting copy of the rules. The split is exact: every `GateVerdict::Run`
/// becomes `GateOutcome::Run(None)` with no prompt, and every `NeedsHuman` goes to the approver.
pub(super) enum GateVerdict {
    /// Run it: a rule allowed it, or nothing asked for a human and the call is read-only.
    Run,
    /// The human has to be asked: a rule said `ask`, or the call mutates.
    NeedsHuman,
    /// Refused by a rule. The string is the reason, which goes into the tool result and the log.
    Denied(String),
}

impl<P, M, N, R> Agent<P, M, N, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    /// Whether this call must be put to the human: the flag is on *and* the tool mutates (or
    /// `tools.mutating_overrides` names it).
    pub(super) async fn needs_approval(&self, call: &ToolCall) -> bool {
        if !self.config.require_hitl_for_mutation {
            return false;
        }
        if self
            .approval
            .mutating_overrides
            .iter()
            .any(|tool| tool == &call.name)
        {
            return true;
        }
        match self.registry.is_mutating_call(call).await {
            Ok(mutating) => mutating,
            Err(error) => {
                // Fail closed: a registry that cannot answer is not permission to write.
                warn!(
                    "Cannot tell whether tool '{}' mutates ({}): treating it as mutating",
                    call.name, error
                );
                true
            }
        }
    }

    /// Decide what happens to a call, in one place: standing rules first, then the human gate.
    ///
    /// A rule can refuse (`deny`), force a prompt (`ask`), or skip the prompt (`allow`). Only a
    /// call no rule matched is put through the tool-mutability default — which is why the rules
    /// can quiet the forty `cargo test` prompts without being able to approve a `write_file` the
    /// gate would have asked about.
    pub(super) async fn gate_decision(&self, call: &ToolCall) -> GateOutcome {
        match self.gate_verdict(call).await {
            GateVerdict::Run => GateOutcome::Run(None),
            GateVerdict::Denied(reason) => GateOutcome::Denied(reason),
            GateVerdict::NeedsHuman => self.ask_the_human(call).await,
        }
    }

    /// The non-interactive half of [`Self::gate_decision`]: rules and mutability, no prompt.
    pub(super) async fn gate_verdict(&self, call: &ToolCall) -> GateVerdict {
        match self.permissions.evaluate(call) {
            RuleVerdict::Deny(reason) => {
                debug!(tool = %call.name, "denied by permission rule: {}", reason);
                return GateVerdict::Denied(reason);
            }
            RuleVerdict::Allow => {
                debug!(tool = %call.name, "allowed by permission rule");
                return GateVerdict::Run;
            }
            RuleVerdict::Ask => {}
            RuleVerdict::NoOpinion => {
                if !self.needs_approval(call).await {
                    return GateVerdict::Run;
                }
            }
        }

        GateVerdict::NeedsHuman
    }

    /// Put one call to the human and turn the answer into a decision.
    async fn ask_the_human(&self, call: &ToolCall) -> GateOutcome {
        let preview = self.registry.preview(call).await.unwrap_or_default();
        let request = ApprovalRequest {
            tool: call.name.clone(),
            call_id: call.id.clone(),
            target: preview.target,
            diff: preview.diff,
            arguments: call.arguments.clone(),
        };

        // Tool name and target only: never file contents, never the argument blob.
        debug!(
            tool = %request.tool,
            target = %request.target.as_deref().unwrap_or("-"),
            "mutating tool call needs approval"
        );

        let timeout = self.approval.timeout;
        match tokio::time::timeout(timeout, self.approver.approve(&request)).await {
            Ok(Ok(ApprovalDecision::Run)) => GateOutcome::Run(None),
            Ok(Ok(ApprovalDecision::RunWithArguments(arguments))) => {
                GateOutcome::Run(Some(arguments))
            }
            Ok(Ok(ApprovalDecision::Deny)) => {
                GateOutcome::Denied("denied at the prompt".to_string())
            }
            Ok(Err(HarnessError::ApprovalRejected(reason))) => GateOutcome::Denied(reason),
            Ok(Err(error)) => GateOutcome::Denied(error.to_string()),
            Err(_elapsed) => {
                GateOutcome::Denied(format!("no answer within {}s", timeout.as_secs()))
            }
        }
    }
}
