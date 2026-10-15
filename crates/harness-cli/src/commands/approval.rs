//! The terminal side of the approval gate: the prompt a human answers, and the approver the CLI
//! injects into `Agent` from `tools.approval`.

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use colored::Colorize;
use tokio::sync::{mpsc, oneshot};

use harness_core::{
    AllowAll, ApprovalDecision, ApprovalMode, ApprovalRequest, ApprovalSettings, Approver, Config,
    DenyAll, HarnessError, NonInteractive,
};

/// Formats the single clean approval box with [ Approve ] and [ Cancel ] choices.
pub fn render_approval_box(
    request: &ApprovalRequest,
    selected: usize, // 0 = Approve, 1 = Cancel
    width: usize,
) -> (String, usize) {
    let mut lines = Vec::new();
    let inner_width = width.saturating_sub(4).max(50);
    let title = " Approval Required ";
    let header_dashes = inner_width.saturating_sub(title.len() + 2);
    let top_border = format!("┌─{}{}{}┐", title.yellow().bold(), "─".repeat(header_dashes).yellow(), "─".repeat(2).yellow());
    lines.push(top_border);

    let make_line = |content: &str| -> String {
        let stripped = dialoguer::console::strip_ansi_codes(content);
        let pad = inner_width.saturating_sub(stripped.len());
        format!("│ {}{} │", content, " ".repeat(pad))
    };

    lines.push(make_line(&format!("{}:   {}", "Tool".bold(), request.tool.cyan().bold())));

    if let Some(target) = &request.target {
        let target_display = if target.len() > inner_width.saturating_sub(12) {
            format!("{}...", &target[..inner_width.saturating_sub(15)])
        } else {
            target.clone()
        };
        lines.push(make_line(&format!("{}: {}", "Target".bold(), target_display)));
    }

    if let Some(diff) = &request.diff {
        lines.push(make_line(&format!("{}:", "Diff".bold())));
        let diff_lines: Vec<&str> = diff.lines().collect();
        let max_diff = 12.min(diff_lines.len());
        for dl in &diff_lines[..max_diff] {
            let truncated = if dl.len() > inner_width.saturating_sub(4) {
                format!("{}...", &dl[..inner_width.saturating_sub(7)])
            } else {
                dl.to_string()
            };
            let colored_dl = if dl.starts_with('+') {
                truncated.green().to_string()
            } else if dl.starts_with('-') {
                truncated.red().to_string()
            } else if dl.starts_with('@') {
                truncated.cyan().to_string()
            } else {
                truncated.dimmed().to_string()
            };
            lines.push(make_line(&format!("  {}", colored_dl)));
        }
        if diff_lines.len() > max_diff {
            lines.push(make_line(&format!("  {} (+ {} more lines)", "...".dimmed(), diff_lines.len() - max_diff)));
        }
    }

    lines.push(make_line(""));

    let approve_btn = if selected == 0 {
        format!("❯ {}", "[ Approve ]".green().bold())
    } else {
        format!("  {}", "[ Approve ]".dimmed())
    };

    let cancel_btn = if selected == 1 {
        format!("❯ {}", "[ Cancel ]".red().bold())
    } else {
        format!("  {}", "[ Cancel ]".dimmed())
    };

    let btn_row = format!("      {}        {}", approve_btn, cancel_btn);
    lines.push(make_line(&btn_row));

    let bot_border = format!("└{}┘", "─".repeat(inner_width + 2).yellow());
    lines.push(bot_border);

    let count = lines.len();
    let full = lines.join("\n") + "\n";
    (full, count)
}

/// Approver for interactive Chat mode: routes approval requests through a channel to the UI loop.
pub struct ChatApprover {
    request_tx: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
}

impl ChatApprover {
    pub fn new(
        request_tx: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
    ) -> Self {
        Self { request_tx }
    }
}

#[async_trait]
impl Approver for ChatApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        let (decision_tx, decision_rx) = oneshot::channel();
        if self.request_tx.send((request.clone(), decision_tx)).is_err() {
            return Ok(ApprovalDecision::Deny);
        }
        match decision_rx.await {
            Ok(decision) => Ok(decision),
            Err(_) => Ok(ApprovalDecision::Deny),
        }
    }
}

/// Fallback terminal approver for non-chat sessions.
pub struct TerminalApprover;

#[async_trait]
impl Approver for TerminalApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        let (text, _) = render_approval_box(request, 0, 72);
        print!("{}", text);
        print!("Choose [y]es / [n]o: ");
        let _ = io::stdout().flush();

        match read_line().await.trim().chars().next() {
            Some('y') | Some('Y') | Some('a') | Some('A') => Ok(ApprovalDecision::Run),
            _ => Ok(ApprovalDecision::Deny),
        }
    }
}

async fn read_line() -> String {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
        line
    })
    .await
    .unwrap_or_default()
}

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

pub fn approval_settings(config: &Config) -> ApprovalSettings {
    ApprovalSettings {
        timeout: Duration::from_secs(config.tools.approval_timeout_secs),
        mutating_overrides: config.tools.mutating_overrides.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
