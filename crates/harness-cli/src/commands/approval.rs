//! The terminal side of the approval gate: the prompt a human answers, and the approver the CLI
//! injects into `Agent` from `tools.approval`.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use colored::Colorize;
use tokio::sync::{mpsc, oneshot};

use harness_core::{
    AllowAll, ApprovalDecision, ApprovalMode, ApprovalRequest, ApprovalSettings, Approver, Config,
    DenyAll, HarnessError,
};

/// The three answers the approval popup offers, in focus order.
pub const APPROVAL_ACTIONS: [&str; 3] = [
    "[y] Approve once",
    "[a] Approve all for this session",
    "[n] Deny execution",
];

/// What the interactive approval prompt resolved to, before it maps onto [`ApprovalDecision`].
pub enum ApprovalChoice {
    Once,
    AllSession,
    Deny,
}

/// Renders the approval popup: a rounded box that names the tool, shows the exact command or
/// path in an inset panel, and lists the three actions with the focused one marked.
pub fn render_approval_box(
    request: &ApprovalRequest,
    selected: usize, // index into APPROVAL_ACTIONS
    width: usize,
) -> (String, usize) {
    let total = width.clamp(32, 76);
    let visible_len = |s: &str| dialoguer::console::strip_ansi_codes(s).chars().count();
    let cut = |s: &str, room: usize| -> String {
        if visible_len(s) > room {
            format!(
                "{}…",
                dialoguer::console::strip_ansi_codes(s)
                    .chars()
                    .take(room.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            s.to_string()
        }
    };

    let row = |content: &str| -> String {
        let max_content = total.saturating_sub(4);
        let vlen = visible_len(content);
        if vlen > max_content {
            let truncated = cut(content, max_content);
            let pad = max_content.saturating_sub(visible_len(&truncated));
            format!("│ {}{} │", truncated, " ".repeat(pad))
        } else {
            let pad = max_content - vlen;
            format!("│ {}{} │", content, " ".repeat(pad))
        }
    };
    let blank = row("");

    let inner_row = |payload: &str| -> String {
        let max_inner = total.saturating_sub(10);
        let truncated = cut(payload, max_inner);
        let pad = max_inner.saturating_sub(visible_len(&truncated));
        row(&format!(" │ {}{} │ ", truncated, " ".repeat(pad)))
    };
    let inner_top = |label: &str| -> String {
        let dashes = total.saturating_sub(label.len() + 11);
        row(&format!(" ┌─ {} {}┐ ", label.cyan(), "─".repeat(dashes)))
    };
    let inner_bottom =
        || -> String { row(&format!(" └{}┘ ", "─".repeat(total.saturating_sub(8)))) };

    let mut lines = Vec::new();
    let title = if total >= 44 {
        "Tool Execution Approval"
    } else if total >= 34 {
        "Approval Required"
    } else {
        "Approval"
    };
    let dashes = total.saturating_sub(title.len() + 5);
    lines.push(format!("╭─ {} {}╮", title.bold(), "─".repeat(dashes)));

    lines.push(blank.clone());
    let tool_msg = if total >= 50 {
        format!(" The agent requested access to: {}", request.tool.bold())
    } else {
        format!(" Tool: {}", request.tool.bold())
    };
    lines.push(row(&tool_msg));
    lines.push(blank.clone());

    if let Some(diff) = &request.diff {
        lines.push(inner_top("Diff"));
        let diff_lines: Vec<&str> = diff.lines().collect();
        let max_diff = 8.min(diff_lines.len());
        for dl in &diff_lines[..max_diff] {
            let truncated = cut(dl, total.saturating_sub(12));
            let colored = if dl.starts_with('+') {
                truncated.green().to_string()
            } else if dl.starts_with('-') {
                truncated.red().to_string()
            } else if dl.starts_with('@') {
                truncated.cyan().to_string()
            } else {
                truncated.dimmed().to_string()
            };
            lines.push(inner_row(&format!("  {}", colored)));
        }
        if diff_lines.len() > max_diff {
            lines.push(inner_row(&format!(
                "  {} (+{} more)",
                "…".dimmed(),
                diff_lines.len() - max_diff
            )));
        }
        lines.push(inner_bottom());
    } else if let Some(target) = &request.target {
        let label = match request.tool.as_str() {
            "terminal" | "shell" | "bash" => "Command",
            "write_file" | "patch" => "File",
            "save_note" | "update_note" | "delete_note" => "Note",
            _ => "Target",
        };
        lines.push(inner_top(label));
        let is_command = matches!(request.tool.as_str(), "terminal" | "shell" | "bash");
        let room = total.saturating_sub(14); // interior minus indent and "$ "
        let plain = cut(target, room);
        let payload = if is_command {
            format!("{} {}", "$".cyan().bold(), plain.bold())
        } else {
            plain.cyan().to_string()
        };
        lines.push(inner_row(&payload));
        lines.push(inner_bottom());
    }

    lines.push(blank.clone());
    lines.push(row(" Action"));
    let actions = if total >= 44 {
        APPROVAL_ACTIONS
    } else {
        [
            "[y] Approve once",
            "[a] Approve all (session)",
            "[n] Deny execution",
        ]
    };
    for (idx, action) in actions.iter().enumerate() {
        if idx == selected {
            lines.push(row(&format!(" {} {}", "❯".cyan().bold(), action.bold())));
        } else {
            lines.push(row(&format!(" {} {}", " ", action.dimmed())));
        }
    }
    lines.push(blank.clone());
    let hint = if total >= 54 {
        "Navigate with ↑/↓ • Press Enter or key to confirm"
    } else {
        "↑/↓ • Enter or y/a/n to confirm"
    };
    lines.push(row(&format!(" {}", hint.dimmed())));
    lines.push(format!("╰{}╯", "─".repeat(total.saturating_sub(2))));

    let count = lines.len();
    (lines.join("\n") + "\n", count)
}

/// Approver for interactive Chat mode: routes approval requests through a channel to the UI loop.
pub struct ChatApprover {
    request_tx: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
    allow_all: Arc<AtomicBool>,
    plan_mode: Arc<AtomicBool>,
}

impl ChatApprover {
    pub fn new(
        request_tx: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
        allow_all: Arc<AtomicBool>,
        plan_mode: Arc<AtomicBool>,
    ) -> Self {
        Self {
            request_tx,
            allow_all,
            plan_mode,
        }
    }
}

#[async_trait]
impl Approver for ChatApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        if self.plan_mode.load(Ordering::SeqCst) {
            return Err(HarnessError::ApprovalRejected(
                "plan mode active: mutating tool calls are disabled (press Tab to toggle plan mode off)"
                    .to_string(),
            ));
        }
        if self.allow_all.load(Ordering::SeqCst) {
            return Ok(ApprovalDecision::Run);
        }
        let (decision_tx, decision_rx) = oneshot::channel();
        if self
            .request_tx
            .send((request.clone(), decision_tx))
            .is_err()
        {
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
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    let _ = reader.read_line(&mut line).await;
    line
}

pub fn build_approver(config: &Config) -> Arc<dyn Approver> {
    match config.tools.approval {
        ApprovalMode::Allow => Arc::new(AllowAll),
        ApprovalMode::Deny => Arc::new(DenyAll),
        ApprovalMode::Ask if io::stdin().is_terminal() => Arc::new(TerminalApprover),
        // Headless / non-terminal: YOLO mode, auto-approve mutating tools
        ApprovalMode::Ask => Arc::new(AllowAll),
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
    async fn ask_without_a_terminal_runs_in_yolo_mode() {
        let mut config = Config::default();
        config.tools.approval = ApprovalMode::Ask;

        let approver = build_approver(&config);
        let decision = approver
            .approve(&ApprovalRequest::default())
            .await
            .expect("headless ask runs in yolo mode");
        assert_eq!(decision, ApprovalDecision::Run);
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

    #[tokio::test]
    async fn chat_approver_respects_allow_all_without_deadlock() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let allow_all = Arc::new(AtomicBool::new(false));
        let plan_mode = Arc::new(AtomicBool::new(false));
        let approver = ChatApprover::new(tx, allow_all.clone(), plan_mode);

        // Initial request should ask
        let req = ApprovalRequest::default();
        tokio::spawn(async move {
            if let Some((_request, decision_tx)) = rx.recv().await {
                let _ = decision_tx.send(ApprovalDecision::Run);
            }
        });

        let decision = approver.approve(&req).await.unwrap();
        assert_eq!(decision, ApprovalDecision::Run);

        // Turn on allow_all: subsequent requests bypass the channel completely
        allow_all.store(true, Ordering::SeqCst);
        let decision2 = approver.approve(&req).await.unwrap();
        assert_eq!(decision2, ApprovalDecision::Run);
    }

    #[tokio::test]
    async fn chat_approver_denies_on_dropped_receiver() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let allow_all = Arc::new(AtomicBool::new(false));
        let plan_mode = Arc::new(AtomicBool::new(false));
        let approver = ChatApprover::new(tx, allow_all, plan_mode);

        let req = ApprovalRequest::default();
        tokio::spawn(async move {
            if let Some((_request, decision_tx)) = rx.recv().await {
                // Drop decision_tx without sending
                drop(decision_tx);
            }
        });

        let decision = approver.approve(&req).await.unwrap();
        assert_eq!(decision, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn chat_approver_gates_mutations_in_plan_mode() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let allow_all = Arc::new(AtomicBool::new(true)); // Even with allow_all on
        let plan_mode = Arc::new(AtomicBool::new(true)); // Plan mode takes precedence
        let approver = ChatApprover::new(tx, allow_all, plan_mode);

        let req = ApprovalRequest::default();
        let err = approver
            .approve(&req)
            .await
            .expect_err("must reject in plan mode");
        assert!(matches!(err, HarnessError::ApprovalRejected(_)));
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

#[cfg(test)]
mod render_tests {
    use super::*;

    #[test]
    fn command_popup_matches_the_design() {
        let request = ApprovalRequest {
            tool: "terminal".to_string(),
            call_id: "c1".to_string(),
            target: Some("ls -la && head -100 README.md agents.md 2>/dev/null".to_string()),
            diff: None,
            arguments: serde_json::json!({"command": "ls -la"}),
        };
        let (text, lines) = render_approval_box(&request, 0, 74);
        print!("\n{}", text);
        assert_eq!(lines, text.lines().count());
        assert!(text.contains("Tool Execution Approval"));
        assert!(text.contains("The agent requested access to: terminal"));
        assert!(text.contains("$ ls -la && head -100 README.md"));
        assert!(text.contains("[y] Approve once"));
        assert!(text.contains("[a] Approve all for this session"));
        assert!(text.contains("[n] Deny execution"));
        // Every visible row is the same display width: the box must not wobble.
        let widths: Vec<usize> = text
            .lines()
            .map(|l| dialoguer::console::strip_ansi_codes(l).chars().count())
            .collect();
        assert!(
            widths.iter().all(|&w| w == widths[0]),
            "widths: {:?}",
            widths
        );
    }

    #[test]
    fn focus_moves_mark_the_selected_action() {
        let request = ApprovalRequest {
            tool: "write_file".to_string(),
            call_id: "c2".to_string(),
            target: Some("/tmp/report.md".to_string()),
            diff: Some("--- a/report.md\n+++ b/report.md\n@@ -1 +1 @@\n-old\n+new\n".to_string()),
            arguments: serde_json::json!({}),
        };
        for selected in 0..3 {
            let (text, _) = render_approval_box(&request, selected, 74);
            let plain = dialoguer::console::strip_ansi_codes(&text).to_string();
            let focused = plain.lines().filter(|l| l.contains('❯')).count();
            assert_eq!(
                focused, 1,
                "selected={} must mark exactly one action",
                selected
            );
            assert!(plain.contains("Diff"), "diff panel must render");
            assert!(plain.contains("+new"));
        }
    }

    #[test]
    fn narrow_terminal_approval_box_does_not_overflow_or_wobble() {
        let request = ApprovalRequest {
            tool: "terminal".to_string(),
            call_id: "c3".to_string(),
            target: Some(
                "ls -la ~/Downloads/rust-agent-harness 2>/dev/null; echo '---'; ls".to_string(),
            ),
            diff: None,
            arguments: serde_json::json!({"command": "ls -la"}),
        };
        for width in [36, 42, 50, 60] {
            let (text, lines) = render_approval_box(&request, 0, width);
            assert_eq!(lines, text.lines().count());
            let widths: Vec<usize> = text
                .lines()
                .map(|l| dialoguer::console::strip_ansi_codes(l).chars().count())
                .collect();
            assert!(
                widths.iter().all(|&w| w == widths[0]),
                "widths at width={}: {:?}",
                width,
                widths
            );
            assert!(
                widths[0] <= width,
                "box width {} exceeds terminal width {}",
                widths[0],
                width
            );
        }
    }
}
