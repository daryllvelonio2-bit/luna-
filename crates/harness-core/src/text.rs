//! Text hygiene for model output that arrives as the model's *private* markup.
//!
//! Some OpenAI-compatible gateways (DeepSeek most visibly) stream a tool call twice: once in the
//! real `tool_calls` field, and once as the model's own special-token scaffolding in the text
//! channel. The scaffolding is not content, but without this filter it is stored as the assistant's
//! message text — so a resumed session replays pages of `…DSML… calls>` noise that never appeared
//! during the live turn.
//!
//! The filter is deliberately narrow: it drops a single `<…>` run only when the run is recognisably
//! DeepSeek's scaffolding (it spells `DSML`, or it pairs a full-width bar `｜` with a tool-call
//! keyword). Ordinary prose that happens to use angle brackets survives untouched.

/// Keywords that, together with a full-width bar `｜`, mark a token as DSML scaffolding.
const DSML_KEYWORDS: [&str; 4] = ["calls", "invoke", "parameter", "tool"];

/// True when the inside of a `<…>` run is DeepSeek's DSML scaffolding rather than text.
fn is_dsml_scaffolding(inner: &str) -> bool {
    let lowered = inner.to_lowercase();
    if lowered.contains("dsml") {
        return true;
    }
    let has_full_width_bar = inner.contains('｜');
    if !has_full_width_bar {
        return false;
    }
    DSML_KEYWORDS
        .iter()
        .any(|keyword| lowered.contains(keyword))
}

/// Strip DeepSeek DSML tool-call scaffolding out of `text`.
///
/// Text that was nothing but scaffolding comes back as whitespace, so callers keep their existing
/// `trim().is_empty()` test to decide the message had no text at all.
pub fn strip_dsml_artifacts(text: &str) -> String {
    if !text.contains('<') {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;

    while index < chars.len() {
        if chars[index] == '<' {
            // A scaffolding tag never spans a line: stop the scan at the first `>` or newline so a
            // stray `<` in prose cannot swallow the rest of the message.
            let mut end = index + 1;
            while end < chars.len() && chars[end] != '>' && chars[end] != '\n' {
                end += 1;
            }
            if end < chars.len() && chars[end] == '>' {
                let inner: String = chars[index + 1..end].iter().collect();
                if is_dsml_scaffolding(&inner) {
                    index = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact artifact observed in stored transcripts: one tag naming the tool-call block, whose
    /// two inner bars are U+FF5C (written as an escape here so it cannot be mistaken for `|`).
    const OBSERVED_ARTIFACT: &str = "\n\n<\u{FF5C}DSML\u{FF5C} calls>\n\n";

    #[test]
    fn the_observed_dsml_artifact_is_dropped_entirely() {
        let stripped = strip_dsml_artifacts(OBSERVED_ARTIFACT);
        assert!(
            stripped.trim().is_empty(),
            "the scaffolding is not content: {stripped:?}"
        );
    }

    #[test]
    fn a_well_formed_dsml_wrapper_is_dropped() {
        let text = "<\u{FF5C}｜DSML｜\u{FF5C}tool_calls>\n<\u{FF5C}｜DSML｜\u{FF5C}invoke name=\"terminal\">\n</\u{FF5C} invoke>\n</\u{FF5C}｜DSML｜\u{FF5C}tool_calls>";
        let stripped = strip_dsml_artifacts(text);
        assert!(stripped.trim().is_empty(), "left over: {stripped:?}");
    }

    #[test]
    fn prose_around_the_artifact_survives() {
        let text =
            "Installing the CLI now.\n\n<\u{FF5C}DSML\u{FF5C} calls>\n\nThen I will run the tests.";
        let stripped = strip_dsml_artifacts(text);
        assert!(stripped.contains("Installing the CLI now."));
        assert!(stripped.contains("Then I will run the tests."));
        assert!(!stripped.contains("DSML") && !stripped.contains('\u{FF5C}'));
    }

    #[test]
    fn ordinary_text_is_untouched() {
        for text in [
            "Use Vec<String> when you need owned strings.",
            "if a < b && b > c { }",
            "no angle brackets at all",
            "a comparison: 3 < 5",
        ] {
            assert_eq!(strip_dsml_artifacts(text), text, "mangled {text:?}");
        }
    }

    #[test]
    fn a_generic_special_token_is_left_alone() {
        // Only DSML-shaped runs are scaffolding; other vendors' markers are not this filter's job.
        let text = "<|im_start|>system";
        assert_eq!(strip_dsml_artifacts(text), text);
    }
}
