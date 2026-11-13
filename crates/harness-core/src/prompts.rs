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
pub const MEMORY_SESSION_PROMPT: &str = "MEMORY_SESSION_PROMPT";
pub const NOTES_SESSION_PROMPT: &str = "NOTES_SESSION_PROMPT";
pub const MEMORY_BACKGROUND_PROMPT: &str = "MEMORY_BACKGROUND_PROMPT";
pub const GOAL_START_PROMPT: &str = "GOAL_START_PROMPT";
pub const GOAL_CONTINUE_PROMPT: &str = "GOAL_CONTINUE_PROMPT";

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
            MEMORY_SESSION_PROMPT,
            NOTES_SESSION_PROMPT,
            MEMORY_BACKGROUND_PROMPT,
            GOAL_START_PROMPT,
            GOAL_CONTINUE_PROMPT,
        ] {
            let body = section(name).expect("declared section is present");
            assert!(!body.is_empty(), "{name} loaded empty");
        }
    }

    #[test]
    fn goal_prompts_expose_their_substitution_points() {
        for name in [GOAL_START_PROMPT, GOAL_CONTINUE_PROMPT] {
            let body = section(name).expect("goal prompt loads");
            for marker in [
                "{{GOAL_OBJECTIVE}}",
                "{{ITERATION}}",
                "{{MAX_ITERATIONS}}",
                "{{GOAL_COMPLETE_TAG}}",
                "{{GOAL_BLOCKED_EXAMPLE}}",
            ] {
                assert!(body.contains(marker), "{name} missing marker {marker}");
            }
        }
    }

    #[test]
    fn system_prompt_exposes_its_substitution_points() {
        let body = section(SYSTEM_PROMPT).expect("system prompt loads");
        assert!(
            body.contains("{{PERSISTENT_MEMORY_TAG}}"),
            "persistent memory tag marker missing"
        );
        assert!(
            body.contains("{{SYSTEM_PROMPT_PREFIX}}"),
            "prefix marker missing"
        );
        assert!(
            body.contains("{{ACTIVE_MEMORIES_BLOCK}}"),
            "memory block marker missing"
        );
        assert!(
            body.contains("{{SKILLS_PROTOCOL}}"),
            "skills protocol marker missing"
        );
        assert!(
            body.contains("{{MCP_PROTOCOL}}"),
            "mcp protocol marker missing"
        );
        assert!(
            body.contains("{{MEMORY_AND_NOTE_PROTOCOL}}"),
            "memory and note protocol marker missing"
        );
        assert!(
            body.contains("{{PROFILE_PERSONA}}"),
            "profile persona marker missing"
        );
        assert!(
            body.contains("{{PROFILE_BEHAVIOR}}"),
            "profile behavior marker missing"
        );
    }

    #[test]
    fn the_compaction_prompt_asks_for_a_structured_checkpoint() {
        let body = section(COMPACTION_INSTRUCTION).expect("compaction prompt loads");

        // The template and the persisted shape are the same contract: a section the prompt stops
        // asking for would silently disappear from every later resume.
        for heading in crate::session::compaction::CHECKPOINT_SECTIONS {
            assert!(body.contains(heading), "{heading} missing from: {body}");
        }
        for state in ["### Completed", "### Active", "### Blocked"] {
            assert!(body.contains(state), "{state} missing from: {body}");
        }

        // The guarantees the old 3-5 line snapshot carried are upgraded, not dropped.
        assert!(body.contains("Merge, do not append"), "{body}");
        assert!(
            body.contains("Preserve exact paths, symbols, commands, flags, versions, exit codes"),
            "{body}"
        );
        assert!(body.contains("Never invent"), "{body}");
        assert!(
            !body.contains("exactly 3 to 5 lines"),
            "the old snapshot template is gone: {body}"
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

    #[test]
    fn the_memory_and_note_protocols_are_separate_sections() {
        let memory = section(MEMORY_SESSION_PROMPT).expect("memory prompt loads");
        assert!(memory.contains("save_memory"), "{memory}");
        assert!(memory.contains("single-line index cards"), "{memory}");
        assert!(
            memory.contains("single authoritative record per primary domain"),
            "{memory}"
        );
        assert!(memory.contains("forget_memory"), "{memory}");
        assert!(
            !memory.contains("Project Notes"),
            "notes moved to their own section: {memory}"
        );

        let notes = section(NOTES_SESSION_PROMPT).expect("notes prompt loads");
        assert!(notes.contains("Project Notes"), "{notes}");
        assert!(notes.contains("save_note"), "{notes}");
        assert!(notes.contains("markdown notes"), "{notes}");
        assert!(notes.contains("For repository audits"), "{notes}");

        // The old contract — a rich record per entity — is gone.
        assert!(!memory.contains("consolidated memory record"), "{memory}");
    }
}
