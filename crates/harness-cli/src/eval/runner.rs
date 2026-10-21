//! The suite runner: loads cases, drives each one through the real agent loop with the selected
//! backend, evaluates the assertions and writes `eval-report.json`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use harness_core::{Agent, AgentConfig, HarnessError, ModelProvider, TokenUsage};

use super::scenario::Scenario;
use super::scripted::{OFFLINE_MODEL_ID, OFFLINE_PROVIDER_ID};
use super::stubs::{NoopEvolver, NoopMemory, NoopNotes};
use super::tools::{register_builtin, EvalSandbox, ObservedCall, RecordingRegistry};

/// How the case's model turns are produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Scripted provider: canned turns from the case file. No credential, no quota.
    Offline,
    /// The real provider.
    Live,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Offline => "offline",
            Backend::Live => "live",
        }
    }
}

pub struct RunOptions {
    pub suite: PathBuf,
    pub backend: Backend,
    /// Substring match on the case name; `None` runs every case.
    pub filter: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    pub report_path: PathBuf,
    /// Reason recorded for every case when the backend cannot run (live without a credential).
    pub skip_reason: String,
    /// Offline word-count accounting, shared with the scripted provider so the report can attribute
    /// a delta to each case. `None` for live, where the harness drops the API's usage field.
    pub usage: Option<Arc<Mutex<TokenUsage>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CaseStatus {
    Pass,
    Fail,
    Skip,
}

impl CaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CaseStatus::Pass => "pass",
            CaseStatus::Fail => "fail",
            CaseStatus::Skip => "skip",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CaseReport {
    pub case: String,
    pub status: CaseStatus,
    pub reason: String,
    pub tool_calls: Vec<ObservedCall>,
    pub tokens: Option<TokenUsage>,
    pub wall_time_ms: u64,
    pub provider: String,
    pub model: String,
    pub backend: String,
}

#[derive(Debug, Serialize)]
pub struct EvalReport {
    pub suite: String,
    pub backend: String,
    pub provider: String,
    pub model: String,
    /// Stated in the report because the numbers differ in kind between backends.
    pub token_accounting: String,
    pub started_at: DateTime<Utc>,
    pub wall_time_ms: u64,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cases: Vec<CaseReport>,
}

impl EvalReport {
    /// Skips are not failures: a live suite with no credential still exits 0.
    pub fn ok(&self) -> bool {
        self.failed == 0
    }
}

const TOKEN_ACCOUNTING: &str =
    "offline: whitespace word counts, exact and reproducible; live: null, \
     the non-streaming provider path drops the API's usage field";

/// Runs every selected case. `make_provider` returns the provider for a case, or `None` when the
/// backend cannot run at all (live without a credential) — those cases are skipped, not failed.
pub async fn run_suite<P, F>(
    opts: &RunOptions,
    make_provider: F,
) -> Result<EvalReport, HarnessError>
where
    P: ModelProvider + 'static,
    F: Fn(&Scenario) -> Option<Arc<P>>,
{
    let scenarios = Scenario::load_suite(&opts.suite)?;
    let selected: Vec<&Scenario> = match &opts.filter {
        Some(filter) => scenarios
            .iter()
            .filter(|s| s.case.contains(filter.as_str()))
            .collect(),
        None => scenarios.iter().collect(),
    };
    if selected.is_empty() {
        return Err(HarnessError::ConfigurationError(format!(
            "no case in '{}' matches --filter '{}'",
            opts.suite.display(),
            opts.filter.as_deref().unwrap_or_default()
        )));
    }

    let (provider_id, model_id) = identity(opts);
    let started_at = Utc::now();
    let suite_start = Instant::now();

    let mut cases = Vec::with_capacity(selected.len());
    for scenario in selected {
        let case_start = Instant::now();
        let before = usage_snapshot(opts);
        let mut case = match make_provider(scenario) {
            None => skipped_case(scenario, opts, &provider_id, &model_id),
            Some(provider) => run_case(scenario, provider, opts, &provider_id, &model_id).await,
        };
        case.wall_time_ms = elapsed_ms(case_start);
        case.tokens = usage_delta(opts, before);
        cases.push(case);
    }

    let report = EvalReport {
        suite: opts.suite.display().to_string(),
        backend: opts.backend.as_str().to_string(),
        provider: provider_id,
        model: model_id,
        token_accounting: TOKEN_ACCOUNTING.to_string(),
        started_at,
        wall_time_ms: elapsed_ms(suite_start),
        total: cases.len(),
        passed: cases
            .iter()
            .filter(|c| c.status == CaseStatus::Pass)
            .count(),
        failed: cases
            .iter()
            .filter(|c| c.status == CaseStatus::Fail)
            .count(),
        skipped: cases
            .iter()
            .filter(|c| c.status == CaseStatus::Skip)
            .count(),
        cases,
    };

    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
    // A `--report <dir>/file.json` path may name directories that do not exist yet;
    // create them rather than failing the whole run after every case already passed.
    if let Some(parent) = opts.report_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| HarnessError::Io(format!("{}: {}", parent.display(), e)))?;
        }
    }
    std::fs::write(&opts.report_path, json)
        .map_err(|e| HarnessError::Io(format!("{}: {}", opts.report_path.display(), e)))?;

    Ok(report)
}

fn identity(opts: &RunOptions) -> (String, String) {
    match opts.backend {
        Backend::Offline => (
            OFFLINE_PROVIDER_ID.to_string(),
            OFFLINE_MODEL_ID.to_string(),
        ),
        Backend::Live => (opts.provider_id.clone(), opts.model_id.clone()),
    }
}

fn skipped_case(
    scenario: &Scenario,
    opts: &RunOptions,
    provider_id: &str,
    model_id: &str,
) -> CaseReport {
    CaseReport {
        case: scenario.case.clone(),
        status: CaseStatus::Skip,
        reason: opts.skip_reason.clone(),
        tool_calls: vec![],
        tokens: None,
        wall_time_ms: 0,
        provider: provider_id.to_string(),
        model: model_id.to_string(),
        backend: opts.backend.as_str().to_string(),
    }
}

/// One case, through the real agent loop.
async fn run_case<P: ModelProvider + 'static>(
    scenario: &Scenario,
    provider: Arc<P>,
    opts: &RunOptions,
    provider_id: &str,
    model_id: &str,
) -> CaseReport {
    let outcome = drive(scenario, provider).await;
    let (status, reason, tool_calls) = match outcome {
        Ok((reason, calls)) => (CaseStatus::Pass, reason, calls),
        Err(failure) => (CaseStatus::Fail, failure.1, failure.0),
    };

    CaseReport {
        case: scenario.case.clone(),
        status,
        reason,
        tool_calls,
        tokens: None,
        wall_time_ms: 0,
        provider: provider_id.to_string(),
        model: model_id.to_string(),
        backend: opts.backend.as_str().to_string(),
    }
}

/// Returns the pass reason with the observed calls, or the failing assertion with what was observed.
async fn drive<P: ModelProvider + 'static>(
    scenario: &Scenario,
    provider: Arc<P>,
) -> Result<(String, Vec<ObservedCall>), (Vec<ObservedCall>, String)> {
    let sandbox = match EvalSandbox::new(&scenario.case) {
        Ok(s) => Arc::new(s),
        Err(e) => return Err((vec![], format!("sandbox: {}", e))),
    };

    let registry = Arc::new(RecordingRegistry::new(scenario.approval.deny.clone()));
    for name in &scenario.tools {
        if let Err(e) = register_builtin(&registry, name, sandbox.clone()).await {
            return Err((vec![], format!("register '{}': {}", name, e)));
        }
    }

    // The eval gate answers "allow" and lets the case's own `approval.deny` / `permissions`
    // decide, so a failing case names the mechanism that refused the call rather than the default.
    let permissions = match harness_core::PermissionEngine::validate(scenario.permissions.clone()) {
        Ok(engine) => engine,
        Err(error) => return Err((vec![], format!("permissions: {}", error))),
    };

    let agent = Agent::new(
        provider,
        Arc::new(NoopMemory),
        Arc::new(NoopNotes),
        Arc::new(NoopEvolver),
        registry.clone(),
        AgentConfig::default(),
    )
    .with_permissions(permissions)
    .with_approval(
        Arc::new(harness_core::AllowAll),
        harness_core::ApprovalSettings::default(),
    );

    let answer = match agent
        .run_task(&scenario.prompt, CancellationToken::new())
        .await
    {
        Ok(answer) => answer,
        Err(e) => return Err((registry.calls(), format!("turn did not complete: {}", e))),
    };

    let calls = registry.calls();
    match evaluate(scenario, &answer, &calls, &sandbox) {
        Ok(reason) => Ok((reason, calls)),
        Err(reason) => Err((calls, reason)),
    }
}

/// Every assertion in the case, first failure wins.
fn evaluate(
    scenario: &Scenario,
    answer: &str,
    calls: &[ObservedCall],
    sandbox: &EvalSandbox,
) -> Result<String, String> {
    let expected = &scenario.expect.tool_calls;
    if calls.len() != expected.len() {
        return Err(format!(
            "expected {} tool call(s), observed {}: {}",
            expected.len(),
            calls.len(),
            describe(calls)
        ));
    }
    for (index, (want, got)) in expected.iter().zip(calls.iter()).enumerate() {
        if want.name != got.name {
            return Err(format!(
                "tool call #{}: expected '{}', observed '{}'",
                index + 1,
                want.name,
                got.name
            ));
        }
        for (key, value) in &want.arguments_contain {
            if got.arguments.get(key) != Some(value) {
                return Err(format!(
                    "tool call #{} '{}': argument '{}' expected {}, observed {}",
                    index + 1,
                    want.name,
                    key,
                    value,
                    got.arguments.get(key).unwrap_or(&serde_json::Value::Null)
                ));
            }
        }
        if let Some(want_error) = want.error {
            if want_error != got.error {
                return Err(format!(
                    "tool call #{} '{}': expected error={}, observed error={}",
                    index + 1,
                    want.name,
                    want_error,
                    got.error
                ));
            }
        }
    }

    for needle in &scenario.expect.final_answer_contains {
        if !answer.contains(needle.as_str()) {
            return Err(format!(
                "final answer is missing {:?}; got {:?}",
                needle,
                truncate(answer)
            ));
        }
    }

    for file in &scenario.expect.files {
        let exists = sandbox.exists(&file.path);
        if file.exists != exists {
            return Err(format!(
                "file '{}': expected exists={}, observed exists={}",
                file.path, file.exists, exists
            ));
        }
        if let Some(wanted) = &file.content_equals {
            let observed = sandbox
                .read_to_string(&file.path)
                .map_err(|e| format!("file '{}': {}", file.path, e))?;
            if &observed != wanted {
                return Err(format!(
                    "file '{}': expected {} bytes {:?}, observed {:?}",
                    file.path,
                    wanted.len(),
                    truncate(wanted),
                    truncate(&observed)
                ));
            }
        }
    }

    Ok(format!(
        "{} tool call(s), {} answer and {} file assertion(s) held",
        calls.len(),
        scenario.expect.final_answer_contains.len(),
        scenario.expect.files.len()
    ))
}

fn describe(calls: &[ObservedCall]) -> String {
    if calls.is_empty() {
        return "none".to_string();
    }
    calls
        .iter()
        .map(|c| {
            let suffix = if c.denied {
                " (denied)"
            } else if c.error {
                " (error)"
            } else {
                ""
            };
            format!("{}{}", c.name, suffix)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn truncate(text: &str) -> String {
    const LIMIT: usize = 160;
    let mut out: String = text.chars().take(LIMIT).collect();
    if text.chars().count() > LIMIT {
        out.push('…');
    }
    out
}

fn elapsed_ms(start: Instant) -> u64 {
    let elapsed: Duration = start.elapsed();
    elapsed.as_millis() as u64
}

fn usage_snapshot(opts: &RunOptions) -> Option<TokenUsage> {
    opts.usage
        .as_ref()
        .map(|u| u.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

fn usage_delta(opts: &RunOptions, before: Option<TokenUsage>) -> Option<TokenUsage> {
    let before = before?;
    let after = usage_snapshot(opts)?;
    Some(TokenUsage {
        prompt_tokens: after.prompt_tokens.saturating_sub(before.prompt_tokens),
        completion_tokens: after
            .completion_tokens
            .saturating_sub(before.completion_tokens),
        total_tokens: after.total_tokens.saturating_sub(before.total_tokens),
    })
}
