//! Scenario format for the `luna eval` regression gate.
//!
//! One YAML file per case, loaded from a suite directory (`tests/eval` by default). The format is
//! documented for case authors in `docs/EVALS.md`; this module only defines and validates it.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use harness_core::HarnessError;

/// Tools the eval runner can install into a scenario's registry. A scenario may only name these:
/// the fake provider replays tool calls, so the set has to be deterministic and side-effect free
/// outside the case sandbox.
pub const BUILTIN_TOOLS: [&str; 4] = [
    "eval_write_file",
    "eval_read_file",
    "eval_echo",
    "eval_fail",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Case name; also the name used by `--filter` and in the report.
    pub case: String,
    /// The user turn handed to the agent loop.
    pub prompt: String,
    /// Built-in eval tools to register for this case.
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub approval: ApprovalSpec,
    /// Canned model turns for the offline backend, in order. Ignored by `--live`.
    #[serde(default)]
    pub script: Vec<ScriptTurn>,
    #[serde(default)]
    pub expect: Expect,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalSpec {
    /// Tool calls the human would refuse. A denied call must not run, and its error must surface
    /// to the model as a tool result.
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptTurn {
    #[serde(default)]
    pub tool_calls: Vec<ScriptCall>,
    /// Text for this turn. On a tool-call turn it is the assistant preamble; on the last turn it is
    /// the final answer the loop returns.
    #[serde(default)]
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptCall {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Every substring must appear in the final answer.
    #[serde(default)]
    pub final_answer_contains: Vec<String>,
    /// The observed tool calls, in order, must match this list one for one.
    #[serde(default)]
    pub tool_calls: Vec<ExpectedCall>,
    /// Filesystem assertions, resolved inside the case sandbox.
    #[serde(default)]
    pub files: Vec<ExpectedFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedCall {
    pub name: String,
    /// Each entry must be present in the call arguments with exactly this JSON value.
    #[serde(default)]
    pub arguments_contain: BTreeMap<String, serde_json::Value>,
    /// When set, the call's error status must match.
    #[serde(default)]
    pub error: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFile {
    /// Path relative to the case sandbox.
    pub path: String,
    #[serde(default = "yes")]
    pub exists: bool,
    /// Exact bytes the file must contain (no trailing-newline guessing).
    #[serde(default)]
    pub content_equals: Option<String>,
}

fn yes() -> bool {
    true
}

impl Scenario {
    /// Loads every `*.yaml` / `*.yml` case in `dir`, sorted by file name, and validates each one.
    pub fn load_suite(dir: &Path) -> Result<Vec<Scenario>, HarnessError> {
        if !dir.is_dir() {
            return Err(HarnessError::ConfigurationError(format!(
                "eval suite directory '{}' does not exist",
                dir.display()
            )));
        }

        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| HarnessError::Io(format!("{}: {}", dir.display(), e)))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("yaml") | Some("yml")
                )
            })
            .collect();
        files.sort();

        if files.is_empty() {
            return Err(HarnessError::ConfigurationError(format!(
                "eval suite directory '{}' holds no *.yaml cases",
                dir.display()
            )));
        }

        let mut scenarios: Vec<Scenario> = Vec::with_capacity(files.len());
        for file in files {
            let raw = std::fs::read_to_string(&file)
                .map_err(|e| HarnessError::Io(format!("{}: {}", file.display(), e)))?;
            let scenario: Scenario = serde_yaml::from_str(&raw).map_err(|e| {
                HarnessError::ConfigurationError(format!("{}: {}", file.display(), e))
            })?;
            scenario.validate(&file)?;
            if let Some(duplicate) = scenarios.iter().find(|s| s.case == scenario.case) {
                return Err(HarnessError::ConfigurationError(format!(
                    "{}: case name '{}' is already used by another file",
                    file.display(),
                    duplicate.case
                )));
            }
            scenarios.push(scenario);
        }
        Ok(scenarios)
    }

    fn validate(&self, file: &Path) -> Result<(), HarnessError> {
        let at =
            |msg: String| HarnessError::ConfigurationError(format!("{}: {}", file.display(), msg));

        if self.case.trim().is_empty() {
            return Err(at("'case' must not be empty".to_string()));
        }
        // The case name becomes a sandbox directory name, so it may not be a path.
        if !self
            .case
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(at(format!(
                "case name '{}' must be ASCII letters, digits, '-', '_' or '.'",
                self.case
            )));
        }
        if self.prompt.trim().is_empty() {
            return Err(at(format!(
                "case '{}': 'prompt' must not be empty",
                self.case
            )));
        }
        for name in self.tools.iter() {
            if !BUILTIN_TOOLS.contains(&name.as_str()) {
                return Err(at(format!(
                    "case '{}': unknown tool '{}' (known: {})",
                    self.case,
                    name,
                    BUILTIN_TOOLS.join(", ")
                )));
            }
        }
        for call in self.script.iter().flat_map(|t| t.tool_calls.iter()) {
            if !BUILTIN_TOOLS.contains(&call.name.as_str()) {
                return Err(at(format!(
                    "case '{}': script calls unknown tool '{}'",
                    self.case, call.name
                )));
            }
        }
        for call in self.expect.tool_calls.iter() {
            if !BUILTIN_TOOLS.contains(&call.name.as_str()) {
                return Err(at(format!(
                    "case '{}': expectation names unknown tool '{}'",
                    self.case, call.name
                )));
            }
        }
        for name in self.approval.deny.iter() {
            if !BUILTIN_TOOLS.contains(&name.as_str()) {
                return Err(at(format!(
                    "case '{}': approval.deny names unknown tool '{}'",
                    self.case, name
                )));
            }
        }
        for file_expectation in self.expect.files.iter() {
            if !is_sandbox_relative(&file_expectation.path) {
                return Err(at(format!(
                    "case '{}': file path '{}' must be relative to the case sandbox",
                    self.case, file_expectation.path
                )));
            }
        }
        Ok(())
    }
}

/// Guard shared by the scenario loader, the sandbox and the write tool: no absolute paths and no
/// `..` components, so a case can never touch anything outside its own sandbox.
pub fn is_sandbox_relative(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return false;
    }
    candidate
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}
