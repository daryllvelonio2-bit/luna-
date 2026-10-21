//! Permission rules: policy that decides *before* the human is asked.
//!
//! The approval gate answers "may this run?" one call at a time. That is the right shape for a
//! human and the wrong shape for a rule: nobody wants to click "yes" forty times on `cargo test`,
//! and nobody wants a `git push --force` to reach a prompt where a tired person can approve it by
//! reflex. Rules are the standing answer, expressed once:
//!
//! ```yaml
//! tools:
//!   permissions:
//!     - action: allow
//!       tool: "read_*"
//!     - action: allow
//!       tool: terminal
//!       arguments: { command: "cargo *" }
//!     - action: deny
//!       tool: terminal
//!       arguments: { command: "*git push --force*" }
//!       reason: force-push is never run by the agent
//!     - action: ask
//!       tool: terminal
//! ```
//!
//! **Precedence is `deny` > `ask` > `allow`, regardless of order**, and a call matched by no rule
//! falls through to the gate's existing default. The asymmetry is deliberate: a guardrail that can
//! be switched off by a later, broader `allow` is not a guardrail. The cost is that an `allow` is
//! only useful where no `deny`/`ask` also matches — which is exactly what a rule author means.
//!
//! Rules decide **whether the human is asked**, never **what the sandbox permits**: an `allow` rule
//! skips the prompt, it does not grant filesystem reach the tool did not already have.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::HarnessError;
use crate::glob::matches_optional;
use crate::types::ToolCall;

/// What a rule says about a matching call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    /// Run it without asking.
    Allow,
    /// Ask the human, whatever the tool's own mutability says.
    Ask,
    /// Never run it; the model is told why.
    Deny,
}

/// One rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRule {
    pub action: RuleAction,
    /// Glob over the tool name (`*`, `terminal`, `write_*`).
    pub tool: String,
    /// Argument constraints, all of which must hold. A value is a glob over the argument's string
    /// form; a `!` prefix inverts the match (`{"command": "!git *"}` = "anything but git").
    #[serde(default)]
    pub arguments: BTreeMap<String, String>,
    /// Shown to the model and logged when this rule denies a call.
    #[serde(default)]
    pub reason: Option<String>,
}

impl PermissionRule {
    fn matches(&self, call: &ToolCall) -> bool {
        if !matches_optional(Some(self.tool.as_str()), &call.name) {
            return false;
        }
        self.arguments.iter().all(|(key, pattern)| {
            let Some(value) = call.arguments.get(key) else {
                return false;
            };
            argument_matches(pattern, value)
        })
    }
}

/// What the rules decided about a call.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleVerdict {
    /// No rule matched: the caller's default behaviour applies.
    NoOpinion,
    /// Run without prompting.
    Allow,
    /// Prompt.
    Ask,
    /// Refuse.
    Deny(String),
}

/// The configured rules, evaluated in one place.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PermissionEngine {
    rules: Vec<PermissionRule>,
}

impl PermissionEngine {
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        Self { rules }
    }

    /// Validate rules up front: an empty tool pattern or an empty argument pattern is almost always
    /// a typo, and a rule that cannot ever match silently does nothing — which is the failure this
    /// rejects rather than stores.
    pub fn validate(rules: Vec<PermissionRule>) -> Result<Self, HarnessError> {
        for rule in &rules {
            if rule.tool.trim().is_empty() {
                return Err(HarnessError::ConfigurationError(
                    "tools.permissions: a rule needs a non-empty 'tool' pattern".to_string(),
                ));
            }
            for (key, pattern) in &rule.arguments {
                if pattern.trim().is_empty() {
                    return Err(HarnessError::ConfigurationError(format!(
                        "tools.permissions: rule for '{}' has an empty pattern for argument '{key}'",
                        rule.tool
                    )));
                }
            }
        }
        Ok(Self { rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn rules(&self) -> &[PermissionRule] {
        &self.rules
    }

    /// Evaluate every rule and keep the most restrictive answer.
    ///
    /// Deny beats ask beats allow, whatever the file's order: see the module docs for why the
    /// ordering is not "first match wins".
    pub fn evaluate(&self, call: &ToolCall) -> RuleVerdict {
        let mut verdict = RuleVerdict::NoOpinion;
        for rule in &self.rules {
            if !rule.matches(call) {
                continue;
            }
            match rule.action {
                RuleAction::Deny => {
                    return RuleVerdict::Deny(
                        rule.reason
                            .clone()
                            .unwrap_or_else(|| format!("denied by rule for tool '{}'", rule.tool)),
                    );
                }
                RuleAction::Ask => {
                    if !matches!(verdict, RuleVerdict::Deny(_)) {
                        verdict = RuleVerdict::Ask;
                    }
                }
                RuleAction::Allow => {
                    if matches!(verdict, RuleVerdict::NoOpinion) {
                        verdict = RuleVerdict::Allow;
                    }
                }
            }
        }
        verdict
    }
}

/// A rule value is a glob against the argument's string form; `!pattern` inverts it.
fn argument_matches(pattern: &str, value: &serde_json::Value) -> bool {
    let text = argument_text(value);

    if let Some(inverted) = pattern.strip_prefix('!') {
        return !matches_optional(Some(inverted), &text);
    }
    matches_optional(Some(pattern), &text)
}

/// The string a rule matches against: a JSON string is used verbatim, anything else is rendered
/// as its compact JSON, so `{"command": ["a","b"]}` is matched as the text the model wrote.
fn argument_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
