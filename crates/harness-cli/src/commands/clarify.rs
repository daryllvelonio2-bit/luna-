use std::io::{self, Write};

use async_trait::async_trait;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use harness_core::{error::HarnessError, traits::Tool, types::ToolResult};

use crate::commands::chat::keys::AppKey;
use crate::commands::chat::ui::{get_terminal_width, ChatUi};

/// A single clarification question with its choices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyItem {
    pub question: String,
    #[serde(default)]
    pub choices: Vec<String>,
}

/// An incoming clarify request passed to the UI handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyRequest {
    pub questions: Vec<ClarifyItem>,
}

/// The user's answer to a single question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyAnswer {
    pub question: String,
    pub answer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice_index: Option<usize>,
}

/// Response returned to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice_index: Option<usize>,
    pub answers: Vec<ClarifyAnswer>,
}

/// ClarifyTool enables the assistant to ask interactive multiple-choice
/// and free-text questions to the user during conversation turns.
pub struct ClarifyTool {
    tx: mpsc::UnboundedSender<(ClarifyRequest, oneshot::Sender<ClarifyResponse>)>,
}

impl ClarifyTool {
    pub fn new(
        tx: mpsc::UnboundedSender<(ClarifyRequest, oneshot::Sender<ClarifyResponse>)>,
    ) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Tool for ClarifyTool {
    fn name(&self) -> &str {
        "clarify"
    }

    fn description(&self) -> &str {
        "Ask the user one or more clarifying questions when you need a decision, confirmation, or preference. The user can pick from provided choices or type a custom answer."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "Question to ask the user (for single-question calls)."
                },
                "choices": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of choices for the user to choose from (a 'Type your answer' free-text option is added automatically)."
                },
                "questions": {
                    "type": "array",
                    "description": "Optional list of multiple questions to ask sequentially.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string" },
                            "choices": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["question"]
                    }
                }
            }
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let mut items = Vec::new();

        if let Some(questions_val) = arguments.get("questions").and_then(|v| v.as_array()) {
            for q_val in questions_val {
                if let Some(q_str) = q_val.get("question").and_then(|v| v.as_str()) {
                    let mut choices = Vec::new();
                    if let Some(ch_arr) = q_val.get("choices").and_then(|v| v.as_array()) {
                        for c in ch_arr {
                            if let Some(cs) = c.as_str() {
                                choices.push(cs.to_string());
                            }
                        }
                    }
                    items.push(ClarifyItem {
                        question: q_str.trim().to_string(),
                        choices,
                    });
                }
            }
        }

        if items.is_empty() {
            if let Some(q_str) = arguments.get("question").and_then(|v| v.as_str()) {
                let mut choices = Vec::new();
                if let Some(ch_arr) = arguments.get("choices").and_then(|v| v.as_array()) {
                    for c in ch_arr {
                        if let Some(cs) = c.as_str() {
                            choices.push(cs.to_string());
                        }
                    }
                }
                items.push(ClarifyItem {
                    question: q_str.trim().to_string(),
                    choices,
                });
            }
        }

        if items.is_empty() {
            return Ok(ToolResult::error(
                call_id,
                "clarify requires at least one 'question' or non-empty 'questions' list",
            ));
        }

        let request = ClarifyRequest { questions: items };
        let (response_tx, response_rx) = oneshot::channel();

        if self.tx.send((request, response_tx)).is_err() {
            return Ok(ToolResult::error(
                call_id,
                "interactive UI channel unavailable",
            ));
        }

        tokio::select! {
            _ = cancel_token.cancelled() => {
                Err(HarnessError::Cancelled)
            }
            res = response_rx => {
                match res {
                    Ok(resp) => {
                        let serialized = serde_json::to_string(&resp)
                            .unwrap_or_else(|_| "{}".to_string());
                        Ok(ToolResult::success(call_id, serialized))
                    }
                    Err(_) => {
                        Ok(ToolResult::error(call_id, "clarification request was closed without response"))
                    }
                }
            }
        }
    }
}

/// The narrowest a clarification box ever gets, and the widest.
///
/// The box follows the terminal between these bounds: narrower terminals get a narrower box
/// (content wraps instead of being cut), very wide ones stop growing so the text keeps a
/// readable measure.
pub const MIN_BOX_WIDTH: usize = 36;
pub const MAX_BOX_WIDTH: usize = 78;

/// Appended to the first choice so it reads as the suggested one.
const RECOMMENDED_SUFFIX: &str = " (Recommended)";

/// Word-wrap `text` to at most `room` visible columns per line.
///
/// Nothing is ever dropped: words longer than the room are hard-split, so the caller can rely on
/// every returned line fitting (and on the joined lines holding the whole input).
fn wrap_plain(text: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let mut current = String::new();
        for word in raw.split_whitespace() {
            let mut word: Vec<char> = word.chars().collect();
            // A word that cannot fit on a line of its own is split across lines.
            if word.len() > room {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                while word.len() > room {
                    lines.push(word.drain(..room).collect());
                }
            }
            if word.is_empty() {
                continue;
            }
            let needed = word.len() + usize::from(!current.is_empty());
            if current.chars().count() + needed > room {
                lines.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.extend(word);
        }
        // A blank source line stays a blank rendered line.
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// The tail of `text` that fits in `room` columns, marked with a leading ellipsis when it had to
/// scroll. Used for live free-text entry, where the cursor must stay visible.
fn tail_window(text: &str, room: usize) -> String {
    let room = room.max(1);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= room {
        return text.to_string();
    }
    let mut out = String::from("…");
    out.extend(chars[chars.len() - room.saturating_sub(1)..].iter());
    out
}

/// Render `text` into the box as one or more padded rows, wrapped rather than truncated.
///
/// `first_prefix` and `cont_prefix` are emitted before the first and subsequent wrapped lines, so
/// a wrapped list item keeps its marker on the first line and stays aligned underneath it.
fn push_wrapped(
    lines: &mut Vec<String>,
    text: &str,
    first_prefix: &str,
    cont_prefix: &str,
    body: usize,
    style: &dyn Fn(&str) -> String,
    row: &dyn Fn(&str) -> String,
) {
    let indent = first_prefix
        .chars()
        .count()
        .max(cont_prefix.chars().count());
    let room = body.saturating_sub(indent).max(1);
    for (i, segment) in wrap_plain(text, room).into_iter().enumerate() {
        let prefix = if i == 0 { first_prefix } else { cont_prefix };
        lines.push(row(&format!("{prefix}{}", style(&segment))));
    }
}

/// Renders the clarification box for a question, choices, and optional free-text typing state.
pub fn render_clarify_box(
    question: &str,
    choices: &[String],
    selected: usize,
    typing_buf: Option<&str>,
    width: usize,
) -> (String, usize) {
    let total = width.clamp(MIN_BOX_WIDTH, MAX_BOX_WIDTH);
    let body = total.saturating_sub(4);
    let visible_len = |s: &str| dialoguer::console::strip_ansi_codes(s).chars().count();

    // Padding only: everything reaching a row has already been wrapped to fit.
    let row = |content: &str| -> String {
        let vlen = visible_len(content);
        let pad = body.saturating_sub(vlen);
        format!("│ {}{} │", content, " ".repeat(pad))
    };

    let blank = format!("│{}│", " ".repeat(total.saturating_sub(2)));

    let mut lines = Vec::new();
    let title = "Clarification Needed";
    let dashes = total.saturating_sub(title.len() + 5);
    lines.push(format!(
        "╭─ {} {}╮",
        title.bold().cyan(),
        "─".repeat(dashes)
    ));
    lines.push(blank.clone());

    // Render Question, wrapped over as many rows as it needs.
    push_wrapped(
        &mut lines,
        question,
        " ",
        " ",
        body,
        &|s| s.bold().to_string(),
        &row,
    );
    lines.push(blank.clone());

    if let Some(buf) = typing_buf {
        // Free-text entry mode
        lines.push(row(&format!(" {}", "Your answer:".dimmed())));
        let inner_width = total.saturating_sub(8);
        lines.push(row(&format!(" ┌{}┐", "─".repeat(inner_width))));
        let cursor_char = "█".cyan();
        let display_text = tail_window(buf, inner_width.saturating_sub(4));
        lines.push(row(&format!(
            " │ > {}{} │",
            display_text.bold(),
            cursor_char
        )));
        lines.push(row(&format!(" └{}┘", "─".repeat(inner_width))));
        lines.push(blank.clone());
        push_wrapped(
            &mut lines,
            "Type answer • Enter to confirm • Esc to cancel",
            " ",
            " ",
            body,
            &|s| s.dimmed().to_string(),
            &row,
        );
    } else {
        // Choices menu mode
        lines.push(row(&format!(" {}", "Select an option:".dimmed())));

        let mut all_options: Vec<String> = Vec::with_capacity(choices.len() + 1);
        for (i, c) in choices.iter().enumerate() {
            let mut plain = format!("[{}] {}", i + 1, c);
            if i == 0 {
                plain.push_str(RECOMMENDED_SUFFIX);
            }
            all_options.push(plain);
        }
        let type_idx = choices.len() + 1;
        all_options.push(format!("[{}] Type your answer...", type_idx));

        // Style the body of an option per wrapped line, keeping "(Recommended)" dimmed.
        let style_choice = |segment: &str, selected: bool| -> String {
            let head = if selected {
                segment.bold().to_string()
            } else {
                segment.dimmed().to_string()
            };
            match segment.find(RECOMMENDED_SUFFIX) {
                Some(pos) => {
                    let (body, tail) = segment.split_at(pos);
                    let body = if selected {
                        body.bold().to_string()
                    } else {
                        body.dimmed().to_string()
                    };
                    format!("{body}{}", tail.dimmed())
                }
                None => head,
            }
        };

        for (idx, opt) in all_options.iter().enumerate() {
            if idx == selected {
                // The arrow keeps its colour; the body is what wraps.
                let marker = format!(" {} ", "❯".cyan().bold());
                push_wrapped(
                    &mut lines,
                    opt,
                    &marker,
                    "   ",
                    body,
                    &|s| style_choice(s, true),
                    &row,
                );
            } else {
                push_wrapped(
                    &mut lines,
                    opt,
                    "   ",
                    "   ",
                    body,
                    &|s| style_choice(s, false),
                    &row,
                );
            }
        }

        lines.push(blank.clone());
        push_wrapped(
            &mut lines,
            "Navigate with ↑/↓ • Enter or 1-9 to select • Esc to skip",
            " ",
            " ",
            body,
            &|s| s.dimmed().to_string(),
            &row,
        );
    }

    lines.push(format!("╰{}╯", "─".repeat(total.saturating_sub(2))));
    let count = lines.len();
    (lines.join("\n") + "\n", count)
}

/// Prompt the user for clarify choice using the session's key receiver.
pub async fn prompt_clarify_choice(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    request: &ClarifyRequest,
    ui: &ChatUi,
) -> ClarifyResponse {
    if !ui.is_attended {
        let mut answers = Vec::new();
        for q in &request.questions {
            let def = q
                .choices
                .first()
                .cloned()
                .unwrap_or_else(|| "Yes".to_string());
            answers.push(ClarifyAnswer {
                question: q.question.clone(),
                answer: def,
                choice_index: if q.choices.is_empty() { None } else { Some(0) },
            });
        }
        let first_ans = answers.first().map(|a| a.answer.clone());
        let first_idx = answers.first().and_then(|a| a.choice_index);
        return ClarifyResponse {
            answer: first_ans,
            choice_index: first_idx,
            answers,
        };
    }

    let mut answers = Vec::new();

    for item in &request.questions {
        while key_rx.try_recv().is_ok() {}
        ui.clear_box_on_exit();

        let mut selected = 0;
        let num_options = item.choices.len() + 1; // Predefined choices + "Type your answer..."
        let mut typing: Option<String> = None;
        let mut width = get_terminal_width().saturating_sub(2);

        let (initial_text, mut line_count) = render_clarify_box(
            &item.question,
            &item.choices,
            selected,
            typing.as_deref(),
            width,
        );
        print!("{}", initial_text);
        let _ = io::stdout().flush();

        let final_answer = loop {
            let key = match key_rx.recv().await {
                Some(k) => k,
                None => break None,
            };

            if let Some(ref mut buf) = typing {
                // In text entry mode
                match key {
                    AppKey::Enter => {
                        let trimmed = buf.trim().to_string();
                        if !trimmed.is_empty() {
                            break Some((trimmed, None));
                        }
                    }
                    AppKey::Escape => {
                        // Exit typing mode back to menu
                        typing = None;
                    }
                    AppKey::Backspace => {
                        buf.pop();
                    }
                    AppKey::Char(c) if !c.is_control() => {
                        buf.push(c);
                    }
                    AppKey::CtrlC => {
                        break None;
                    }
                    _ => {}
                }
            } else {
                // In menu navigation mode
                match key {
                    AppKey::Up | AppKey::Left | AppKey::BackTab => {
                        selected = (selected + num_options - 1) % num_options;
                    }
                    AppKey::Down | AppKey::Right | AppKey::Tab => {
                        selected = (selected + 1) % num_options;
                    }
                    AppKey::Char(c) if c.is_ascii_digit() && c != '0' => {
                        let digit = (c as u8 - b'1') as usize;
                        if digit < item.choices.len() {
                            break Some((item.choices[digit].clone(), Some(digit)));
                        } else if digit == item.choices.len() {
                            // "Type your answer" option
                            selected = digit;
                            typing = Some(String::new());
                        }
                    }
                    AppKey::Enter | AppKey::Char(' ') => {
                        if selected < item.choices.len() {
                            break Some((item.choices[selected].clone(), Some(selected)));
                        } else {
                            typing = Some(String::new());
                        }
                    }
                    AppKey::Escape | AppKey::CtrlC => {
                        break None;
                    }
                    _ => {}
                }
            }

            // Erase previous box and re-render
            print!("\r\x1b[{}A", line_count);
            for _ in 0..line_count {
                print!("\x1b[2K\x1b[1B");
            }
            print!("\r\x1b[{}A", line_count);

            width = get_terminal_width().saturating_sub(2);
            let (new_text, new_count) = render_clarify_box(
                &item.question,
                &item.choices,
                selected,
                typing.as_deref(),
                width,
            );
            print!("{}", new_text);
            line_count = new_count;
            let _ = io::stdout().flush();
        };

        // Erase box completely before continuing
        print!("\r\x1b[{}A", line_count);
        for _ in 0..line_count {
            print!("\x1b[2K\x1b[1B");
        }
        print!("\r\x1b[{}A", line_count);
        let _ = io::stdout().flush();

        let (chosen_answer, chosen_idx) = match final_answer {
            Some((ans, idx)) => (ans, idx),
            None => ("(skipped by user)".to_string(), None),
        };

        let summary = format!(
            "{} {}: {}",
            "✔".green().bold(),
            item.question.dimmed(),
            chosen_answer.cyan().bold()
        );
        ui.emit_above(&summary);

        answers.push(ClarifyAnswer {
            question: item.question.clone(),
            answer: chosen_answer,
            choice_index: chosen_idx,
        });
    }

    let first_ans = answers.first().map(|a| a.answer.clone());
    let first_idx = answers.first().and_then(|a| a.choice_index);

    ClarifyResponse {
        answer: first_ans,
        choice_index: first_idx,
        answers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(line: &str) -> String {
        dialoguer::console::strip_ansi_codes(line).to_string()
    }

    /// Visible column count of a rendered line, colour codes excluded.
    fn visible(line: &str) -> usize {
        strip(line).chars().count()
    }

    #[test]
    fn wrap_plain_never_loses_text_and_never_exceeds_the_room() {
        let text = "Debug pass: which memory writer should stay canonical after this test — session (agent writes) or background saver?";
        for room in 1..80usize {
            let wrapped = wrap_plain(text, room);
            for line in &wrapped {
                assert!(
                    line.chars().count() <= room,
                    "room {room} produced a {}-column line: {line}",
                    line.chars().count()
                );
            }
            let stripped = |s: &str| -> String {
                s.chars().filter(|c| !c.is_whitespace()).collect::<String>()
            };
            assert_eq!(
                stripped(&wrapped.join(" ")),
                stripped(text),
                "wrapping at {room} dropped or duplicated text"
            );
        }
    }

    #[test]
    fn wrap_plain_hard_splits_a_word_longer_than_the_line() {
        let wrapped = wrap_plain("supercalifragilisticexpialidocious", 10);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|l| l.chars().count() <= 10));
        assert_eq!(wrapped.concat(), "supercalifragilisticexpialidocious");
    }

    #[test]
    fn tail_window_keeps_the_end_of_a_long_buffer() {
        assert_eq!(tail_window("DuckDB", 10), "DuckDB");
        let shown = tail_window("0123456789abcdef", 6);
        assert_eq!(shown.chars().count(), 6);
        assert!(shown.ends_with("bcdef"));
    }

    #[test]
    fn a_long_question_wraps_instead_of_being_cut() {
        let question = "Debug pass: which memory writer should stay canonical after this test — session (agent writes) or background saver?";
        let choices = vec![
            "Session mode (agent writes memory mid-turn)".to_string(),
            "Background saver (independent writer after each turn)".to_string(),
        ];
        let terminal = 120;
        let (rendered, count) = render_clarify_box(question, &choices, 0, None, terminal);
        let total = terminal.clamp(MIN_BOX_WIDTH, MAX_BOX_WIDTH);

        assert!(
            !rendered.contains('…'),
            "nothing may be truncated now that the box wraps:\n{rendered}"
        );
        assert!(
            count > 5 + choices.len(),
            "the question should occupy extra rows"
        );

        // The box stays a rectangle: every row fills the same width, terminal included.
        for line in rendered.lines() {
            assert_eq!(visible(line), total, "ragged row: {line:?}");
        }

        // Every word of the question and of each choice survives the wrap.
        let plain = strip(&rendered);
        for word in question
            .split_whitespace()
            .chain(choices.iter().flat_map(|c| c.split_whitespace()))
        {
            assert!(plain.contains(word), "wrapped box lost {word:?}");
        }
    }

    #[test]
    fn the_box_follows_a_narrow_terminal() {
        let question =
            "A question long enough that a narrow terminal has to wrap it over several rows.";
        let choices = vec!["The only choice, also rather long for a narrow terminal".to_string()];
        let (rendered, _) = render_clarify_box(question, &choices, 0, None, 24);
        let total = 24usize.clamp(MIN_BOX_WIDTH, MAX_BOX_WIDTH);

        assert!(!rendered.contains('…'));
        for line in rendered.lines() {
            assert_eq!(visible(line), total);
        }
        assert!(strip(&rendered).contains("narrow"));
    }

    #[test]
    fn a_long_typed_answer_scrolls_to_keep_the_cursor_visible() {
        let question = "Which database do you prefer?";
        let choices = vec!["PostgreSQL".to_string()];
        let (rendered, _) = render_clarify_box(question, &choices, 0, Some(&"x".repeat(400)), 60);
        let total = 60usize.clamp(MIN_BOX_WIDTH, MAX_BOX_WIDTH);
        assert_eq!(
            rendered.matches('…').count(),
            1,
            "only the scrolled buffer marker may appear"
        );
        for line in rendered.lines() {
            assert_eq!(visible(line), total, "ragged row: {line:?}");
        }
        assert!(rendered.contains('█'), "the cursor must stay in the box");
    }

    #[test]
    fn test_render_clarify_box_menu() {
        let question = "Which database do you prefer?";
        let choices = vec!["PostgreSQL".to_string(), "SQLite".to_string()];
        let (rendered, count) = render_clarify_box(question, &choices, 0, None, 60);
        assert!(count > 5);
        assert!(rendered.contains("Clarification Needed"));
        assert!(rendered.contains("Which database do you prefer?"));
        assert!(rendered.contains("PostgreSQL"));
        assert!(rendered.contains("SQLite"));
        assert!(rendered.contains("Type your answer..."));
        assert!(rendered.contains("❯"));
    }

    #[test]
    fn test_render_clarify_box_typing() {
        let question = "Which database do you prefer?";
        let choices = vec!["PostgreSQL".to_string()];
        let (rendered, count) = render_clarify_box(question, &choices, 1, Some("DuckDB"), 60);
        assert!(count > 5);
        assert!(rendered.contains("Your answer:"));
        assert!(rendered.contains("DuckDB"));
        assert!(rendered.contains("Enter to confirm"));
    }

    #[tokio::test]
    async fn test_clarify_tool_single_question_flow() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tool = ClarifyTool::new(tx);

        tokio::spawn(async move {
            if let Some((req, resp_tx)) = rx.recv().await {
                assert_eq!(req.questions.len(), 1);
                assert_eq!(req.questions[0].question, "Favorite color?");
                assert_eq!(req.questions[0].choices, vec!["Blue", "Red"]);
                let resp = ClarifyResponse {
                    answer: Some("Blue".to_string()),
                    choice_index: Some(0),
                    answers: vec![ClarifyAnswer {
                        question: req.questions[0].question.clone(),
                        answer: "Blue".to_string(),
                        choice_index: Some(0),
                    }],
                };
                let _ = resp_tx.send(resp);
            }
        });

        let cancel = CancellationToken::new();
        let args = serde_json::json!({
            "question": "Favorite color?",
            "choices": ["Blue", "Red"]
        });

        let res = tool.execute("call_1", &args, &cancel).await.unwrap();
        assert!(!res.is_error);
        assert!(res.content.contains("Blue"));
    }

    #[tokio::test]
    async fn test_clarify_tool_multi_questions_flow() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tool = ClarifyTool::new(tx);

        tokio::spawn(async move {
            if let Some((req, resp_tx)) = rx.recv().await {
                assert_eq!(req.questions.len(), 2);
                let resp = ClarifyResponse {
                    answer: Some("Rust".to_string()),
                    choice_index: Some(0),
                    answers: vec![
                        ClarifyAnswer {
                            question: "Language?".to_string(),
                            answer: "Rust".to_string(),
                            choice_index: Some(0),
                        },
                        ClarifyAnswer {
                            question: "Framework?".to_string(),
                            answer: "Axum".to_string(),
                            choice_index: None,
                        },
                    ],
                };
                let _ = resp_tx.send(resp);
            }
        });

        let cancel = CancellationToken::new();
        let args = serde_json::json!({
            "questions": [
                { "question": "Language?", "choices": ["Rust", "Go"] },
                { "question": "Framework?", "choices": ["Actix"] }
            ]
        });

        let res = tool.execute("call_2", &args, &cancel).await.unwrap();
        assert!(!res.is_error);
        assert!(res.content.contains("Rust"));
        assert!(res.content.contains("Axum"));
    }
}
