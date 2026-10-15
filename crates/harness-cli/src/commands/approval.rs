//! The terminal side of the approval gate: the prompt a human answers, and the approver the CLI
//! injects into `Agent` from `tools.approval`.
//!
//! This is the *decision* half of the safety story (`docs/TRENDS_2026.md` §2.2): it says whether a
//! mutating call runs. The sandbox says what a call can reach once it runs. Neither replaces the
//! other — a one-keypress prompt is not a boundary, and an OS boundary is not a human decision.

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use colored::Colorize;

use harness_core::{
    AllowAll, ApprovalDecision, ApprovalMode, ApprovalRequest, ApprovalSettings, Approver, Config,
    DenyAll, HarnessError, NonInteractive,
};

/// Asks on the terminal: `y` runs the call, `e` lets the human replace its arguments, anything
/// else — including an empty answer — denies it.
///
/// A line-oriented prompt rather than `dialoguer`'s raw-key mode: this runs inside the REPL's own
/// line-based stdin loop, and mixing the two reads keystrokes from under each other.
pub struct TerminalApprover;

#[async_trait]
impl Approver for TerminalApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        render(request);

        match read_line().await.trim().chars().next() {
            Some('y') | Some('Y') => Ok(ApprovalDecision::Run),
            Some('e') | Some('E') => edit_arguments(request).await,
            _ => Ok(ApprovalDecision::Deny),
        }
    }
}

fn render(request: &ApprovalRequest) {
    println!("{}", "── approval required ─────────────────────────────".yellow());
    println!("tool:   {}", request.tool.bold());
    if let Some(target) = &request.target {
        println!("target: {}", target);
    }
    if let Some(diff) = &request.diff {
        println!("{}", diff.trim_end());
    }
    print!("Run this call? [y]es / [e]dit arguments / [n]o: ");
    // The flush only affects how promptly a cosmetic prompt appears; a failure here cannot change
    // the answer, and the read below would still work.
    let _ = io::stdout().flush();
}

/// `e`: print the arguments the model asked for and read replacements. An empty line keeps them;
/// anything that is not JSON denies the call rather than guessing what the human meant.
async fn edit_arguments(request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
    println!("arguments: {}", request.arguments);
    print!("replacement arguments (JSON on one line, empty keeps them): ");
    let _ = io::stdout().flush();

    let answer = read_line().await;
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(ApprovalDecision::Run);
    }

    match serde_json::from_str(answer) {
        Ok(value) => Ok(ApprovalDecision::RunWithArguments(value)),
        Err(error) => {
            println!("{} not valid JSON ({}), denying the call", "!".red(), error);
            Ok(ApprovalDecision::Deny)
        }
    }
}

/// One line from stdin, read on a blocking thread so the agent's timeout can still fire.
async fn read_line() -> String {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        // A read that fails (EOF, closed stdin) leaves the line empty, and an empty answer is a
        // denial — the same outcome as any other answer that is not `y`.
        let _ = io::stdin().read_line(&mut line);
        line
    })
    .await
    .unwrap_or_default()
}

/// The approver for this process, chosen from `tools.approval`.
///
/// `ask` with stdin not a terminal resolves to [`NonInteractive`] rather than to a silent allow: a
/// one-keypress prompt is meaningless in a piped, `cron` or background run, and the honest answer
/// there is "there is nobody to ask" — which reaches the model as `approval denied`.
pub fn build_approver(config: &Config) -> Arc<dyn Approver> {
    match config.tools.approval {
        ApprovalMode::Allow => Arc::new(AllowAll),
        ApprovalMode::Deny => Arc::new(DenyAll),
        ApprovalMode::Ask if io::stdin().is_terminal() => Arc::new(TerminalApprover),
        ApprovalMode::Ask => Arc::new(NonInteractive::new(
            "stdin is not a terminal, so nobody can approve this call \
             (pass --yes or set tools.approval=allow if that is intended)",
        )),
    }
}

/// `tools.approval_timeout_secs` and `tools.mutating_overrides` in the shape the agent loop wants.
pub fn approval_settings(config: &Config) -> ApprovalSettings {
    ApprovalSettings {
        timeout: Duration::from_secs(config.tools.approval_timeout_secs),
        mutating_overrides: config.tools.mutating_overrides.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test` does not give the test process a terminal, so this is the non-interactive
    /// half of the mapping: `ask` must fail closed, and the modes that do not ask must not.
    #[tokio::test]
    async fn ask_without_a_terminal_fails_closed() {
        let mut config = Config::default();
        config.tools.approval = ApprovalMode::Ask;

        let approver = build_approver(&config);
        let error = approver
            .approve(&ApprovalRequest::default())
            .await
            .expect_err("a non-interactive `ask` must not approve anything");
        assert!(matches!(error, HarnessError::ApprovalRejected(_)), "{error:?}");
    }

    #[tokio::test]
    async fn allow_and_deny_do_not_ask() {
        let mut config = Config::default();

        config.tools.approval = ApprovalMode::Allow;
        assert_eq!(
            build_approver(&config)
                .approve(&ApprovalRequest::default())
                .await
                .unwrap(),
            ApprovalDecision::Run
        );

        config.tools.approval = ApprovalMode::Deny;
        assert_eq!(
            build_approver(&config)
                .approve(&ApprovalRequest::default())
                .await
                .unwrap(),
            ApprovalDecision::Deny
        );
    }

    #[test]
    fn settings_come_from_the_config_keys() {
        let mut config = Config::default();
        config.tools.approval_timeout_secs = 7;
        config.tools.mutating_overrides = vec!["save_note".to_string()];

        let settings = approval_settings(&config);
        assert_eq!(settings.timeout, Duration::from_secs(7));
        assert_eq!(settings.mutating_overrides, vec!["save_note".to_string()]);
    }
}
