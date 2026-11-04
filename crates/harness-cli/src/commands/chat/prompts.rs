//! The prompts asked *during* a turn: approval for a tool call, and the sudo password.
//!
//! Both take the screen while they are up (see [`super::modal`]) and write their outcome into the
//! transcript on the way out. The clarify box is here too in spirit — it lives in
//! `commands::clarify` because it is also reachable outside the chat.
use std::io::{self, Write};

use colored::Colorize;
use tokio::sync::{mpsc, oneshot};

use super::keys::AppKey;
use super::modal::{paint_centred, ModalScreen};
use super::render;
use super::ui::{get_terminal_width, ChatUi};
use crate::commands::approval::{render_approval_box, ApprovalChoice, APPROVAL_ACTIONS};
use harness_core::{ApprovalRequest, HarnessError};

pub(super) async fn prompt_approval_choice(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    request: &ApprovalRequest,
    decision_tx: &mut oneshot::Sender<harness_core::ApprovalDecision>,
    ui: &ChatUi,
) -> ApprovalChoice {
    let mut selected = 0; // index into APPROVAL_ACTIONS

    // Drain any leftover keys in channel
    while key_rx.try_recv().is_ok() {}

    // The popup takes the screen: what is being approved is not something to be read out of the
    // transcript around it, and the composer is left untouched behind it — the caller writes the
    // verdict once this returns.
    ui.set_modal_active(true);
    // A paint that was already in flight when the flag went up must land before the alternate
    // buffer does, or its composer rows end up drawn around the question.
    ui.wait_for_paints().await;
    let screen = ModalScreen::enter();
    paint_centred(&render_approval_box(
        request,
        selected,
        get_terminal_width().saturating_sub(2),
    ));

    let choice = loop {
        let key = tokio::select! {
            _ = decision_tx.closed() => {
                // The agent timed out waiting for approval or the turn was cancelled: answer Deny
                // at once rather than leaving the UI loop waiting on a decision nobody will read.
                break ApprovalChoice::Deny;
            }
            maybe_key = key_rx.recv() => {
                match maybe_key {
                    Some(k) => k,
                    None => break ApprovalChoice::Deny,
                }
            }
        };

        let chosen = match key {
            AppKey::Up | AppKey::Left | AppKey::BackTab => {
                selected = (selected + APPROVAL_ACTIONS.len() - 1) % APPROVAL_ACTIONS.len();
                None
            }
            AppKey::Down | AppKey::Right | AppKey::Tab => {
                selected = (selected + 1) % APPROVAL_ACTIONS.len();
                None
            }
            AppKey::Char('y') | AppKey::Char('Y') | AppKey::Char('1') => Some(ApprovalChoice::Once),
            AppKey::Char('a') | AppKey::Char('A') | AppKey::Char('2') => {
                Some(ApprovalChoice::AllSession)
            }
            AppKey::Char('n')
            | AppKey::Char('N')
            | AppKey::Char('3')
            | AppKey::Char('c')
            | AppKey::Char('C')
            | AppKey::Char('d')
            | AppKey::Char('D')
            | AppKey::Escape
            | AppKey::CtrlC => Some(ApprovalChoice::Deny),
            AppKey::Enter => match selected {
                0 => Some(ApprovalChoice::Once),
                1 => Some(ApprovalChoice::AllSession),
                _ => Some(ApprovalChoice::Deny),
            },
            AppKey::Char(' ') => match selected {
                0 => Some(ApprovalChoice::Once),
                1 => Some(ApprovalChoice::AllSession),
                _ => Some(ApprovalChoice::Deny),
            },
            _ => continue,
        };

        if let Some(choice) = chosen {
            break choice;
        }

        // Repaint the whole frame: nothing on this screen has to survive it.
        paint_centred(&render_approval_box(
            request,
            selected,
            get_terminal_width().saturating_sub(2),
        ));
    };

    // Back on the transcript: the screen the popup was drawn on went with it.
    drop(screen);
    ui.set_modal_active(false);
    // Status updates (an OpenCode usage event, say) may have landed while the popup was up; show
    // them now that the composer is back.
    ui.redraw_box();
    choice
}

pub struct ChatSudoPrompter {
    request_tx: mpsc::UnboundedSender<(String, oneshot::Sender<Option<String>>)>,
}

impl ChatSudoPrompter {
    pub fn new(
        request_tx: mpsc::UnboundedSender<(String, oneshot::Sender<Option<String>>)>,
    ) -> Self {
        Self { request_tx }
    }
}

#[async_trait::async_trait]
impl harness_core::tools::SudoPrompter for ChatSudoPrompter {
    async fn prompt_sudo_password(&self, command: &str) -> Result<Option<String>, HarnessError> {
        let (tx, rx) = oneshot::channel();
        if self.request_tx.send((command.to_string(), tx)).is_err() {
            return Ok(None);
        }
        match rx.await {
            Ok(pwd) => Ok(pwd),
            Err(_) => Ok(None),
        }
    }
}

/// The sudo frame's widest fixed line. It sets the frame's width, so a command is wrapped to it
/// rather than allowed to become the widest line itself.
const SUDO_HINT: &str = "(Enter password; press Enter to confirm, Esc to skip)";

/// Ask for the sudo password, on a screen the prompt has to itself.
///
/// Not a box — a line of text and a masked field — but still asked mid-turn, so it takes the screen
/// the same way the other prompts do: the command being confirmed is not mixed in with the agent's
/// own output, and the transcript comes back untouched once the answer is in. The frame is written
/// once and the masked field grows in place after it, so typing does not repaint.
pub(super) async fn prompt_sudo_password_dialog(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    command: &str,
    ui: &ChatUi,
) -> Option<String> {
    while key_rx.try_recv().is_ok() {}

    // Wrapped to the width of the frame's own hint line: a longer command would be the widest line
    // on the screen, and a terminal narrower than it would re-flow the block and drag it off
    // centre. Capping at the hint keeps the frame the same shape whatever is being authorised.
    let command_lines = render::wrap_rows(command, SUDO_HINT.len());
    let frame = format!(
        "  {} {}\n     {}\n     {}\n\n  Password: ",
        "🔐".bold(),
        "Sudo password required for command:".bold(),
        command_lines.join("\n     ").cyan(),
        SUDO_HINT.dimmed()
    );

    ui.set_modal_active(true);
    ui.wait_for_paints().await;
    let screen = ModalScreen::enter();
    paint_centred(&frame);

    let mut password = String::new();
    let taken = loop {
        let key = match key_rx.recv().await {
            Some(k) => k,
            None => break None,
        };
        match key {
            AppKey::Enter => break Some(password),
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => break None,
            AppKey::Backspace => {
                if !password.is_empty() {
                    password.pop();
                    print!("\x08 \x08");
                    let _ = io::stdout().flush();
                }
            }
            AppKey::Char(c) => {
                if c == '\n' || c == '\r' {
                    break Some(password);
                } else if !c.is_control() {
                    password.push(c);
                    print!("•");
                    let _ = io::stdout().flush();
                }
            }
            _ => {}
        }
    };

    // Back on the transcript: the prompt went with the screen it was drawn on, and an empty entry
    // is a skip rather than a password.
    drop(screen);
    ui.set_modal_active(false);
    let password = taken.filter(|p| !p.is_empty());
    if password.is_some() {
        println!(
            "\n  {} Password received and cached for this session\n",
            "✔".green().bold()
        );
    } else {
        println!("\n  {} Skipped - continuing without sudo\n", "⏭".dimmed());
    }
    let _ = io::stdout().flush();
    password
}
