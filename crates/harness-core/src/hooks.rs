//! Lifecycle hooks: shell commands the operator attaches to harness events.
//!
//! A hook is a command, not a plugin. It receives one JSON document on stdin describing the event
//! (`event`, `tool`, `arguments`, `session_id`, `cwd`, …), may write one JSON document to stdout,
//! and its exit code decides what happens next:
//!
//! | outcome | effect |
//! |---|---|
//! | exit `0`, no stdout | the event proceeds |
//! | exit `0`, `{"decision":"deny","reason":"…"}` | a gating event is refused, the reason goes to the model |
//! | exit `0`, `{"updatedInput":{…}}` | the tool runs with replacement arguments |
//! | exit `2` | refused; stderr (or stdout) is the reason |
//! | any other exit code | recorded as a hook error; the event proceeds |
//! | no exit within `hooks.timeout_secs` | a *gating* event is refused, an observability event is not |
//!
//! Two properties matter more than the schema. First, **hooks never widen permission**: a hook can
//! refuse a call and can rewrite its arguments, but it cannot approve a mutating call the human
//! gate would otherwise refuse — the gate still runs afterwards. Second, **load is fail-closed**:
//! hooks are off unless `hooks.enabled` is set, and repository-supplied hooks
//! (`<workspace>/.luna/hooks.json`) need `hooks.trust_workspace` as well, because a cloned repo
//! must not be able to configure code execution for itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::config::HooksSettings;
use crate::error::HarnessError;

/// Default for `hooks.timeout_secs`.
pub const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 30;
/// Exit code that refuses an event, the convention Codex and Claude Code share.
pub const HOOK_BLOCK_EXIT_CODE: i32 = 2;
/// Project-relative hooks file.
pub const PROJECT_HOOKS_RELATIVE: &str = ".luna/hooks.json";
/// User hooks file, relative to `$HOME`.
pub const USER_HOOKS_RELATIVE: &str = ".luna/hooks.json";
/// How much of a hook's output is kept; a runaway hook must not become a prompt.
pub const HOOK_OUTPUT_MAX_CHARS: usize = 4_000;

/// An event the harness emits. Names are the wire format: `pre_tool_use`, not `PreToolUse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionRequest,
    PreCompact,
    PostCompact,
    SubagentStart,
    SubagentStop,
    Stop,
    Notification,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::UserPromptSubmit => "user_prompt_submit",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::PostToolUseFailure => "post_tool_use_failure",
            Self::PermissionRequest => "permission_request",
            Self::PreCompact => "pre_compact",
            Self::PostCompact => "post_compact",
            Self::SubagentStart => "subagent_start",
            Self::SubagentStop => "subagent_stop",
            Self::Stop => "stop",
            Self::Notification => "notification",
        }
    }

    /// Every event, in the order the harness can emit them.
    pub fn all() -> &'static [HookEvent] {
        &[
            Self::SessionStart,
            Self::SessionEnd,
            Self::UserPromptSubmit,
            Self::PreToolUse,
            Self::PostToolUse,
            Self::PostToolUseFailure,
            Self::PermissionRequest,
            Self::PreCompact,
            Self::PostCompact,
            Self::SubagentStart,
            Self::SubagentStop,
            Self::Stop,
            Self::Notification,
        ]
    }

    /// Events whose answer can stop work: a timeout or a malformed answer is a refusal.
    pub fn is_gating(self) -> bool {
        matches!(self, Self::PreToolUse | Self::PermissionRequest)
    }

    pub fn parse(raw: &str) -> Result<Self, HarnessError> {
        Self::all()
            .iter()
            .copied()
            .find(|event| event.as_str() == raw)
            .ok_or_else(|| {
                let known = Self::all()
                    .iter()
                    .map(|event| event.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                HarnessError::ConfigurationError(format!(
                    "unknown hook event '{raw}': known events are {known}"
                ))
            })
    }
}

/// One configured hook.
///
/// `matcher` is a glob over the tool name (`*`, `terminal`, `write_*`) and is ignored by events
/// that have no tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpec {
    #[serde(default)]
    pub matcher: Option<String>,
    pub command: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// What a hook decided.
#[derive(Debug, Clone, PartialEq)]
pub enum HookVerdict {
    /// Nothing objected.
    Proceed,
    /// The event is refused; the string is shown to the model and logged.
    Deny(String),
    /// Proceed, but the tool runs with replacement arguments.
    ProceedWith(serde_json::Value),
}

/// The result of running every hook bound to one event.
#[derive(Debug, Clone, PartialEq)]
pub struct HookOutcome {
    pub verdict: HookVerdict,
    /// One line per hook that ran, for the log and for `post_*` events' context.
    pub notes: Vec<String>,
}

impl HookOutcome {
    pub fn proceed() -> Self {
        Self {
            verdict: HookVerdict::Proceed,
            notes: Vec::new(),
        }
    }

    pub fn is_denied(&self) -> bool {
        matches!(self.verdict, HookVerdict::Deny(_))
    }

    /// The denial reason, when there is one.
    pub fn denial(&self) -> Option<&str> {
        match &self.verdict {
            HookVerdict::Deny(reason) => Some(reason.as_str()),
            _ => None,
        }
    }
}

/// The context a hook receives. Only fields that are meaningful for the event are set.
#[derive(Debug, Clone, Default)]
pub struct HookContext<'a> {
    pub session_id: Option<&'a str>,
    pub tool: Option<&'a str>,
    pub arguments: Option<&'a serde_json::Value>,
    /// The user's prompt (`user_prompt_submit`), the model's reply (`stop`), or a notification body.
    pub message: Option<&'a str>,
    /// Why a `post_tool_use_failure` failed, or why the harness is asking for permission.
    pub reason: Option<&'a str>,
}

/// Runs the hooks configured for the workspace and user.
pub struct HookRunner {
    workspace: PathBuf,
    timeout: Duration,
    specs: BTreeMap<HookEvent, Vec<HookSpec>>,
}

impl HookRunner {
    /// Load `~/.luna/hooks.json` and, when `hooks.trust_workspace` is set, the project file.
    ///
    /// A missing file is not an error — most installs have none. A *malformed* file is: silently
    /// skipping a guardrail because of a typo is the failure mode this rejects.
    pub fn load(workspace: &Path, settings: &HooksSettings) -> Result<Self, HarnessError> {
        Self::load_with_user_file(workspace, settings, Some(&user_hooks_path()))
    }

    /// [`Self::load`] with the user file named explicitly, so tests never depend on `$HOME`.
    pub fn load_with_user_file(
        workspace: &Path,
        settings: &HooksSettings,
        user_file: Option<&Path>,
    ) -> Result<Self, HarnessError> {
        let mut runner = Self {
            workspace: workspace.to_path_buf(),
            timeout: Duration::from_secs(if settings.timeout_secs == 0 {
                DEFAULT_HOOK_TIMEOUT_SECS
            } else {
                settings.timeout_secs
            }),
            specs: BTreeMap::new(),
        };

        if !settings.enabled {
            let project = workspace.join(PROJECT_HOOKS_RELATIVE);
            if project.is_file() {
                debug!(
                    path = %project.display(),
                    "hooks are configured for this workspace but `hooks.enabled` is false; nothing will run"
                );
            }
            return Ok(runner);
        }

        if let Some(user) = user_file {
            runner.merge_file(user)?;
        }
        if settings.trust_workspace {
            runner.merge_file(&workspace.join(PROJECT_HOOKS_RELATIVE))?;
        } else {
            let project = workspace.join(PROJECT_HOOKS_RELATIVE);
            if project.is_file() {
                debug!(
                    path = %project.display(),
                    "ignoring repository hooks: set `hooks.trust_workspace` to run them"
                );
            }
        }

        for name in &settings.disabled {
            let event = HookEvent::parse(name)?;
            runner.specs.remove(&event);
        }

        Ok(runner)
    }

    /// A runner with no hooks: every event is `Proceed`.
    pub fn empty(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            timeout: Duration::from_secs(DEFAULT_HOOK_TIMEOUT_SECS),
            specs: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Whether any hook is bound to `event` (after matchers, for tool events).
    pub fn has(&self, event: HookEvent, tool: Option<&str>) -> bool {
        self.specs
            .get(&event)
            .map(|specs| {
                specs
                    .iter()
                    .any(|spec| matcher_allows(spec.matcher.as_deref(), tool))
            })
            .unwrap_or(false)
    }

    /// Number of configured hooks, across every event.
    pub fn len(&self) -> usize {
        self.specs.values().map(Vec::len).sum()
    }

    /// Run every hook for `event`, in configuration order.
    ///
    /// The first denial wins and stops the remaining hooks: a guardrail that already said no does
    /// not need a second opinion, and the model needs the reason promptly. An argument rewrite is
    /// carried forward to later hooks so they see what will actually run.
    pub async fn run(&self, event: HookEvent, context: &HookContext<'_>) -> HookOutcome {
        let Some(specs) = self.specs.get(&event) else {
            return HookOutcome::proceed();
        };

        let mut outcome = HookOutcome::proceed();
        for spec in specs {
            if !matcher_allows(spec.matcher.as_deref(), context.tool) {
                continue;
            }
            match self.run_one(spec, event, context).await {
                Ok(Some(verdict)) => {
                    outcome.notes.push(format!("{}: {}", spec.command, describe(&verdict)));
                    match verdict {
                        HookVerdict::Deny(reason) => {
                            outcome.verdict = HookVerdict::Deny(reason);
                            return outcome;
                        }
                        HookVerdict::ProceedWith(arguments) => {
                            outcome.verdict = HookVerdict::ProceedWith(arguments);
                        }
                        HookVerdict::Proceed => {}
                    }
                }
                Ok(None) => {
                    outcome.notes.push(format!("{}: ok", spec.command));
                }
                Err(error) => {
                    // A hook that cannot run is an error, not an approval. Gating events fail
                    // closed; the rest are observability and must not break a turn.
                    warn!(
                        event = event.as_str(),
                        hook = %spec.command,
                        "hook failed: {error}"
                    );
                    outcome.notes.push(format!("{}: {error}", spec.command));
                    if event.is_gating() {
                        outcome.verdict = HookVerdict::Deny(format!("hook failed: {error}"));
                        return outcome;
                    }
                }
            }
        }
        outcome
    }

    /// Run one hook. `Ok(None)` means "no decision, proceed".
    async fn run_one(
        &self,
        spec: &HookSpec,
        event: HookEvent,
        context: &HookContext<'_>,
    ) -> Result<Option<HookVerdict>, HarnessError> {
        let payload = self.payload(event, context);
        let timeout = spec
            .timeout_secs
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(self.timeout);

        let mut child = Command::new(shell())
            .arg(shell_flag())
            .arg(&spec.command)
            .current_dir(&self.workspace)
            .env("LUNA_HOOK_EVENT", event.as_str())
            .env("LUNA_PROJECT_DIR", &self.workspace)
            .env(
                "LUNA_HOOK_TOOL",
                context.tool.unwrap_or_default(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A hook that outlives its window must not outlive the turn.
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                HarnessError::Io(format!("cannot spawn hook '{}': {error}", spec.command))
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            let body = serde_json::to_vec(&payload).unwrap_or_default();
            // A hook may exit without reading stdin; that is its right, and EPIPE is not an error.
            let _ = stdin.write_all(&body).await;
            let _ = stdin.shutdown().await;
        }

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|error| {
                HarnessError::Io(format!("hook '{}' failed: {error}", spec.command))
            })?,
            Err(_elapsed) => {
                let reason = format!("hook '{}' timed out after {}s", spec.command, timeout.as_secs());
                // `kill_on_drop` already reaped the process when the future was dropped.
                return if event.is_gating() {
                    Ok(Some(HookVerdict::Deny(reason)))
                } else {
                    Err(HarnessError::Io(reason))
                };
            }
        };

        let stdout = clip(&String::from_utf8_lossy(&output.stdout));
        let stderr = clip(&String::from_utf8_lossy(&output.stderr));
        let code = output.status.code().unwrap_or(-1);

        if code == HOOK_BLOCK_EXIT_CODE {
            let reason = first_non_empty(&[stderr.as_str(), stdout.as_str()])
                .unwrap_or("blocked by hook")
                .to_string();
            return Ok(Some(HookVerdict::Deny(reason)));
        }

        let parsed: Option<HookStdout> = serde_json::from_str(stdout.trim()).ok();
        if let Some(parsed) = parsed {
            if let Some(verdict) = parsed.into_verdict() {
                return Ok(Some(verdict));
            }
        }

        if code != 0 {
            let reason = first_non_empty(&[stderr.as_str(), stdout.as_str()])
                .unwrap_or("no output")
                .to_string();
            return Err(HarnessError::Io(format!(
                "hook '{}' exited with {code}: {reason}",
                spec.command
            )));
        }

        Ok(None)
    }

    fn payload(&self, event: HookEvent, context: &HookContext<'_>) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("event".into(), event.as_str().into());
        map.insert(
            "cwd".into(),
            self.workspace.to_string_lossy().to_string().into(),
        );
        map.insert(
            "project_dir".into(),
            self.workspace.to_string_lossy().to_string().into(),
        );
        if let Some(session_id) = context.session_id {
            map.insert("session_id".into(), session_id.into());
        }
        if let Some(tool) = context.tool {
            map.insert("tool".into(), tool.into());
        }
        if let Some(arguments) = context.arguments {
            map.insert("arguments".into(), arguments.clone());
        }
        if let Some(message) = context.message {
            map.insert("message".into(), message.into());
        }
        if let Some(reason) = context.reason {
            map.insert("reason".into(), reason.into());
        }
        serde_json::Value::Object(map)
    }

    fn merge_file(&mut self, path: &Path) -> Result<(), HarnessError> {
        if !path.is_file() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|error| HarnessError::Io(format!("cannot read {}: {error}", path.display())))?;
        let file: HookFile = serde_json::from_str(&raw).map_err(|error| {
            HarnessError::ConfigurationError(format!("{} is not valid hook config: {error}", path.display()))
        })?;
        for (name, specs) in file.into_events()? {
            self.specs.entry(name).or_default().extend(specs);
        }
        Ok(())
    }
}

/// The on-disk shape: either `{"hooks": {"pre_tool_use": [...]}}` or the flat form.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookFile {
    #[serde(default)]
    hooks: Option<BTreeMap<String, Vec<HookSpec>>>,
    #[serde(flatten)]
    events: BTreeMap<String, Vec<HookSpec>>,
}

impl HookFile {
    fn into_events(self) -> Result<Vec<(HookEvent, Vec<HookSpec>)>, HarnessError> {
        let mut merged: Vec<(HookEvent, Vec<HookSpec>)> = Vec::new();
        let nested = self.hooks.unwrap_or_default();
        for (name, specs) in nested.into_iter().chain(self.events) {
            let event = HookEvent::parse(&name)?;
            match merged.iter_mut().find(|(slot, _)| *slot == event) {
                Some((_, existing)) => existing.extend(specs),
                None => merged.push((event, specs)),
            }
        }
        Ok(merged)
    }
}

/// The stdout contract. Both `decision: deny` and `hookSpecificOutput.permissionDecision: deny`
/// are accepted, because both conventions are in the wild and a hook author should not have to
/// guess which one this harness speaks.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookStdout {
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    updated_input: Option<serde_json::Value>,
    #[serde(default)]
    hook_specific_output: Option<HookSpecificOutput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookSpecificOutput {
    #[serde(default)]
    permission_decision: Option<String>,
    #[serde(default)]
    permission_decision_reason: Option<String>,
    #[serde(default)]
    updated_input: Option<serde_json::Value>,
}

impl HookStdout {
    fn into_verdict(self) -> Option<HookVerdict> {
        let (decision, reason, updated_input) = match self.hook_specific_output {
            Some(specific) => (
                specific
                    .permission_decision
                    .or(self.decision)
                    .map(|value| value.to_ascii_lowercase()),
                specific.permission_decision_reason.or(self.reason),
                specific.updated_input.or(self.updated_input),
            ),
            None => (
                self.decision.map(|value| value.to_ascii_lowercase()),
                self.reason,
                self.updated_input,
            ),
        };

        match decision.as_deref() {
            Some("deny") | Some("block") | Some("forbid") => Some(HookVerdict::Deny(
                reason.unwrap_or_else(|| "blocked by hook".to_string()),
            )),
            _ => updated_input.map(HookVerdict::ProceedWith),
        }
    }
}

fn describe(verdict: &HookVerdict) -> String {
    match verdict {
        HookVerdict::Proceed => "ok".to_string(),
        HookVerdict::Deny(reason) => format!("denied: {reason}"),
        HookVerdict::ProceedWith(_) => "rewrote arguments".to_string(),
    }
}

fn first_non_empty<'a>(candidates: &[&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|text| text.trim())
        .find(|text| !text.is_empty())
}

fn clip(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= HOOK_OUTPUT_MAX_CHARS {
        return text.to_string();
    }
    text.chars().take(HOOK_OUTPUT_MAX_CHARS).collect::<String>() + "… (truncated)"
}

/// `matcher` is a glob over the tool name; absent or `*` matches everything.
fn matcher_allows(matcher: Option<&str>, tool: Option<&str>) -> bool {
    let Some(pattern) = matcher else {
        return true;
    };
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    let Some(tool) = tool else {
        // A tool-scoped hook is meaningless for an event with no tool.
        return false;
    };
    glob_match(pattern, tool)
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut resume) = (None::<usize>, 0usize);

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn user_hooks_path() -> PathBuf {
    let home = std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(USER_HOOKS_RELATIVE)
}

fn shell() -> &'static str {
    if cfg!(windows) {
        "cmd"
    } else {
        "sh"
    }
}

fn shell_flag() -> &'static str {
    if cfg!(windows) {
        "/C"
    } else {
        "-c"
    }
}
