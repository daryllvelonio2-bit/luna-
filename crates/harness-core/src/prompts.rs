//! Prompt text, loaded from `prompts.md` at the crate root.
//!
//! Every instruction the harness sends to a model is read from that one text file, so prompts are
//! edited as prose instead of as Rust string literals. No prompt text lives in Rust.

use crate::error::HarnessError;

/// The file that holds every prompt. `include_str!` keeps it a build input, so a prompt change is
/// an ordinary rebuild with no runtime file lookup to fail.
const PROMPTS_FILE: &str = include_str!("../prompts.md");

/// The section names available in `prompts.md`.
pub const SYSTEM_PROMPT: &str = "SYSTEM_PROMPT";
pub const COMPACTION_INSTRUCTION: &str = "COMPACTION_INSTRUCTION";
pub const ENGINEER_SYSTEM_PROMPT: &str = "ENGINEER_SYSTEM_PROMPT";
pub const MEMORY_SESSION_PROMPT: &str = "MEMORY_SESSION_PROMPT";
pub const MEMORY_BACKGROUND_PROMPT: &str = "MEMORY_BACKGROUND_PROMPT";

/// The body of the `name` section, with surrounding blank lines trimmed.
///
/// A missing or empty section is an error rather than an empty string: a silently blank system
/// prompt would degrade every turn without a symptom, so a typo in the section name has to
/// surface at the call site.
pub fn section(name: &str) -> Result<String, HarnessError> {
    let marker = format!("===== {} =====", name);
    let body = PROMPTS_FILE
        .split('\n')
        .skip_while(|line| line.trim() != marker)
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with("====="))
        .collect::<Vec<_>>()
        .join("\n");

    if body.trim().is_empty() {
        return Err(HarnessError::ConfigurationError(format!(
            "prompt section '{}' is missing or empty in crates/harness-core/prompts.md",
            name
        )));
    }
    Ok(body.trim_matches('\n').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_section_loads() {
        for name in [
            SYSTEM_PROMPT,
            COMPACTION_INSTRUCTION,
            ENGINEER_SYSTEM_PROMPT,
            MEMORY_SESSION_PROMPT,
            MEMORY_BACKGROUND_PROMPT,
        ] {
            let body = section(name).expect("declared section is present");
            assert!(!body.is_empty(), "{name} loaded empty");
        }
    }

    #[test]
    fn system_prompt_exposes_its_substitution_points() {
        let body = section(SYSTEM_PROMPT).expect("system prompt loads");
        assert!(
            body.contains("{{SYSTEM_PROMPT_PREFIX}}"),
            "prefix marker missing"
        );
        assert!(
            body.contains("{{ACTIVE_MEMORIES}}"),
            "memory marker missing"
        );
        assert!(
            body.contains("{{AVAILABLE_SKILLS}}"),
            "skills marker missing"
        );
        assert!(
            body.contains("{{MEMORY_AND_NOTE_PROTOCOL}}"),
            "memory and note protocol marker missing"
        );
    }

    #[test]
    fn a_misspelled_section_is_an_error_not_an_empty_prompt() {
        assert!(section("NO_SUCH_SECTION").is_err());
    }

    #[test]
    fn the_reader_preamble_is_not_part_of_any_prompt() {
        for name in [SYSTEM_PROMPT, COMPACTION_INSTRUCTION] {
            let body = section(name).expect("declared section is present");
            assert!(!body.contains("====="), "{name} leaked a section marker");
            assert!(
                !body.starts_with("# Luna prompts"),
                "{name} leaked the preamble"
            );
        }
    }
}
