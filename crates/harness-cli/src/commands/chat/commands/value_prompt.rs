//! The one-line value prompt the numeric slash commands share.
//!
//! `/auto-compact` asks for a number and redraws the same box while the user types. The box
//! lives here rather than beside its caller so there is one answer to how wide it is and how a
//! rejected value reads.

use colored::Colorize;
use tokio::sync::mpsc;

use crate::commands::chat::keys::AppKey;
use crate::commands::chat::modal::{paint_centred, ModalScreen};
use crate::commands::chat::ui::{get_terminal_width, ChatUi};

/// What one value box of the `/auto-compact` wizard asks for.
pub(super) struct ValuePrompt {
    /// Box title.
    title: &'static str,
    /// The sentence above the input panel: what the value does.
    question: &'static str,
    /// The worked example and the accepted range, shown under the panel. The user should never
    /// have to guess the bounds, which is the thing the old typed argument got wrong.
    example: &'static str,
}

pub(super) const THRESHOLD_PROMPT: ValuePrompt = ValuePrompt {
    title: "Auto-Compact — Threshold",
    question: "Fold the oldest turns into a summary once the transcript reaches this many tokens.",
    example: "Example: 150k — or 150000. Range: 100k to 700k.",
};

pub(super) const PERCENT_PROMPT: ValuePrompt = ValuePrompt {
    title: "Auto-Compact — Percentage",
    question: "Percentage of the transcript context to fold away into a summary when threshold is reached.",
    example: "Example: 50 — Range: 1% to 90% (e.g. 50 folds half, keeping half).",
};

/// Ask for one auto-compact value in a box, re-asking in place while it does not parse.
///
/// Returns `None` when the user backs out (Esc / Ctrl+C). A blank line is a real answer: the parse
/// functions read it as the default, and the box says so by showing the current value as the
/// placeholder the answer falls back to.
pub(super) async fn prompt_value<F, A>(
    ui: &ChatUi,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    spec: &ValuePrompt,
    current: usize,
    parse: F,
    advisory: A,
) -> Option<usize>
where
    F: Fn(&str) -> Result<usize, String>,
    A: Fn(&str, usize) -> Option<String>,
{
    // Anything typed before the box opened belongs to the composer, not to this question.
    while key_rx.try_recv().is_ok() {}

    let placeholder = current.to_string();
    let mut input = String::new();
    let mut error: Option<String> = None;

    let compute_advisory = |text: &str| -> Option<String> {
        let val_str = if text.is_empty() {
            placeholder.as_str()
        } else {
            text
        };
        parse(val_str).ok().and_then(|val| advisory(val_str, val))
    };
    let mut adv = compute_advisory(&input);

    // The question takes the screen, the way the approval gate and the clarify box do: the whole
    // frame is repainted on every keystroke, so a box that changes height (an error row, a longer
    // value) can never walk the cursor up the screen.
    ui.set_modal_active(true);
    ui.wait_for_paints().await;
    let screen = ModalScreen::enter();
    paint_centred(&render_value_box(
        spec,
        &input,
        &placeholder,
        error.as_deref(),
        adv.as_deref(),
        get_terminal_width().saturating_sub(2),
    ));

    let answer = loop {
        let key = match key_rx.recv().await {
            Some(key) => key,
            None => break None,
        };
        match key {
            AppKey::Enter => match parse(&input) {
                Ok(value) => break Some(value),
                Err(message) => {
                    error = Some(message);
                    adv = None;
                }
            },
            AppKey::Backspace => {
                input.pop();
                error = None;
                adv = compute_advisory(&input);
            }
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => break None,
            AppKey::Char(c) if !c.is_control() => {
                input.push(c);
                error = None;
                adv = compute_advisory(&input);
            }
            _ => continue,
        }

        paint_centred(&render_value_box(
            spec,
            &input,
            &placeholder,
            error.as_deref(),
            adv.as_deref(),
            get_terminal_width().saturating_sub(2),
        ));
    };

    // Back on the transcript: the composer was never painted over, so there is nothing to restore.
    drop(screen);
    ui.set_modal_active(false);
    ui.redraw_box();
    answer
}

/// Render one value box: a rounded frame, the question, an inset panel holding what has been typed
/// (or the current value, dimmed, while the line is empty), the example, and the key hint.
///
/// Pure, and deliberately fixed-height: every row is present whatever the state, because the
/// callers repaint the whole frame — a box that grew when an error appeared would shift the block
/// off centre per keystroke.
fn render_value_box(
    spec: &ValuePrompt,
    input: &str,
    placeholder: &str,
    error: Option<&str>,
    advisory: Option<&str>,
    width: usize,
) -> String {
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

    let title = spec.title;
    let dashes = total.saturating_sub(title.chars().count() + 5);
    let mut lines = Vec::new();
    lines.push(format!("╭─ {} {}╮", title.bold(), "─".repeat(dashes)));
    lines.push(blank.clone());

    for line in wrap_words(spec.question, total.saturating_sub(6)) {
        lines.push(row(&format!(" {}", line)));
    }
    lines.push(blank.clone());

    lines.push(inner_top("Value"));
    let shown = if input.is_empty() {
        placeholder.dimmed().to_string()
    } else {
        input.bold().to_string()
    };
    lines.push(inner_row(&format!("{} {}", "❯".cyan().bold(), shown)));
    lines.push(inner_bottom());

    lines.push(blank.clone());
    lines.push(row(&format!(" {}", spec.example.dimmed())));
    if let Some(message) = error {
        lines.push(row(&format!(
            " {} {}",
            "✖".red().bold(),
            cut(message, total.saturating_sub(8)).red()
        )));
    } else if let Some(message) = advisory {
        lines.push(row(&format!(" {}", cut(message, total.saturating_sub(6)))));
    } else {
        // Kept as an empty row rather than dropped: the height must not depend on the state.
        lines.push(blank.clone());
    }
    lines.push(blank.clone());
    lines.push(row(&format!(
        " {}",
        "Enter confirms • blank keeps the current one • Esc cancels".dimmed()
    )));
    lines.push(format!("╰{}╯", "─".repeat(total.saturating_sub(2))));

    // The frame's height is a property of the text (`lines().count()`), so no caller is handed a
    // second copy of it to keep in step.
    lines.join("\n") + "\n"
}

/// Break `text` into lines of at most `room` characters, on word boundaries where it can.
pub(super) fn wrap_words(text: &str, room: usize) -> Vec<String> {
    let room = room.max(8);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{} {}", current, word)
        };
        if candidate.chars().count() > room && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod value_box_tests {
    use super::*;

    fn widths_of(text: &str) -> Vec<usize> {
        text.lines()
            .map(|line| dialoguer::console::strip_ansi_codes(line).chars().count())
            .collect()
    }

    /// A popup that changes width wobbles as it repaints. Every row, at every terminal width, and
    /// with and without an error to show.
    #[test]
    fn the_value_box_holds_one_width_in_every_state() {
        let long_error = "threshold must be between 100000 and 700000 (got 50000)";
        for width in [24, 32, 40, 60, 76, 140] {
            for error in [None, Some(long_error)] {
                for input in ["", "150k", "not a number at all"] {
                    let text =
                        render_value_box(&THRESHOLD_PROMPT, input, "150000", error, None, width);
                    let widths = widths_of(&text);
                    assert!(
                        widths.iter().all(|&w| w == widths[0]),
                        "widths at width={width}, error={error:?}, input={input:?}: {widths:?}"
                    );
                    assert!(
                        widths[0] <= width.clamp(32, 76),
                        "box wider than the frame: {} > {width}",
                        widths[0]
                    );
                }
            }
        }
    }

    /// The example and the bounds are the reason this is a box and not an argument: they have to be
    /// on screen while the value is typed.
    #[test]
    fn the_value_box_carries_the_example_and_the_bounds() {
        let text = render_value_box(&THRESHOLD_PROMPT, "", "150000", None, None, 76);
        let plain = dialoguer::console::strip_ansi_codes(&text).to_string();
        assert!(plain.contains("Auto-Compact"));
        assert!(plain.contains("150k"));
        assert!(plain.contains("100k to 700k"));
        assert!(
            plain.contains("150000"),
            "the current value is shown: {plain}"
        );
        assert!(plain.contains("Esc cancels"));
    }

    /// A rejected value is answered inside the box, and the box says why instead of forgetting it.
    #[test]
    fn a_rejected_value_is_shown_in_the_box() {
        let clean = render_value_box(&PERCENT_PROMPT, "0", "50", None, None, 76);
        let failed = render_value_box(
            &PERCENT_PROMPT,
            "0",
            "50",
            Some("percentage must be between 1 and 90 (got 0)"),
            None,
            76,
        );
        let plain = dialoguer::console::strip_ansi_codes(&failed).to_string();
        assert!(plain.contains("must be between 1 and 90"));
        // Same height either way, so repainting the frame cannot shift the block on screen.
        assert_eq!(clean.lines().count(), failed.lines().count());
    }

    /// An advisory message appears cleanly in the box without changing frame height.
    #[test]
    fn an_advisory_message_holds_frame_height() {
        let clean = render_value_box(&PERCENT_PROMPT, "10", "50", None, None, 76);
        let advised = render_value_box(
            &PERCENT_PROMPT,
            "10",
            "50",
            None,
            Some("⚠ Warning: ≤10% frees very little context"),
            76,
        );
        let plain = dialoguer::console::strip_ansi_codes(&advised).to_string();
        assert!(plain.contains("Warning: ≤10%"));
        assert_eq!(clean.lines().count(), advised.lines().count());
    }

    /// Blank is an answer, not a missing one: the parsers behind the two boxes read it as the
    /// default, which is what the hint row promises.
    #[test]
    fn a_blank_line_keeps_the_default() {
        assert_eq!(
            harness_onboarding::compact_step::parse_threshold(""),
            Ok(150_000)
        );
        assert_eq!(harness_onboarding::compact_step::parse_percent(""), Ok(50));
        assert_eq!(harness_onboarding::compact_step::parse_turns(""), Ok(20));
    }

    /// The words a prompt is made of have to land inside the frame, however narrow it is. A single
    /// word longer than the room is left whole — the wrapper never splits a word; the row that
    /// prints it trims it to the frame instead.
    #[test]
    fn wrapping_never_exceeds_the_room_it_was_given() {
        for room in [8, 12, 30, 70] {
            for line in wrap_words(THRESHOLD_PROMPT.question, room) {
                assert!(
                    line.chars().count() <= room || !line.contains(' '),
                    "line longer than {room}: {line:?}"
                );
            }
        }
    }
}
