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
        match self.permissions.evaluate(call) {
            RuleVerdict::Deny(reason) => {
                debug!(tool = %call.name, "denied by permission rule: {}", reason);
                return GateOutcome::Denied(reason);
            }
            RuleVerdict::Allow => {
                debug!(tool = %call.name, "allowed by permission rule");
                return GateOutcome::Run(None);
            }
            RuleVerdict::Ask => {}
            RuleVerdict::NoOpinion => {
                if !self.needs_approval(call).await {
                    return GateOutcome::Run(None);
                }
            }
        }

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
