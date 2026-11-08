//! The skill that documents the desktop tools.
//!
//! The text is a file, not a Rust literal, so it reads as prose and is edited
//! as prose. The file is compiled into the binary, which is what makes the
//! skill *built in*: there is nothing to install, and the guidance cannot
//! drift away from the build that ships the tools.

/// The skill's markdown, frontmatter included.
pub const SKILL_MARKDOWN: &str = include_str!("../skills/agent-desktop/SKILL.md");

/// The name the harness lists the skill under.
pub const SKILL_NAME: &str = "agent-desktop";

/// The one-line summary the harness shows in its skill index.
pub const SKILL_DESCRIPTION: &str = "Operate desktop applications through native accessibility trees: read the UI, click, type, fill fields, toggle controls and verify the result. Includes OS-level window focus, real pointer clicks, key chords, clipboard paste and screenshots for controls the accessibility tree cannot drive. Use for any desktop GUI task, including a web page already open in a logged-in browser.";

/// The skill's name.
pub fn skill_name() -> &'static str {
    SKILL_NAME
}

/// The skill's summary.
pub fn skill_description() -> &'static str {
    SKILL_DESCRIPTION
}

/// The skill's body, without the frontmatter.
///
/// The frontmatter is harness metadata; the body is what an agent reads, so it
/// is what `skill_view` hands over.
pub fn skill_instructions() -> &'static str {
    let body = SKILL_MARKDOWN.strip_prefix("---\n").unwrap_or(SKILL_MARKDOWN);
    match body.split_once("\n---\n") {
        Some((_, rest)) => rest.trim(),
        None => body.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frontmatter_names_the_skill_the_harness_looks_for() {
        assert!(
            SKILL_MARKDOWN.starts_with("---\n"),
            "the skill must open with frontmatter"
        );
        assert!(
            SKILL_MARKDOWN.contains(&format!("name: {SKILL_NAME}")),
            "the frontmatter name must match SKILL_NAME"
        );
    }

    #[test]
    fn the_frontmatter_carries_the_description_the_harness_shows() {
        assert!(
            SKILL_MARKDOWN.contains(SKILL_DESCRIPTION),
            "the frontmatter must carry SKILL_DESCRIPTION"
        );
    }

    #[test]
    fn the_instructions_are_the_body_without_the_frontmatter() {
        let instructions = skill_instructions();
        assert!(!instructions.starts_with("---"));
        assert!(!instructions.contains("name: agent-desktop"));
        assert!(instructions.contains("desktop_snapshot"));
        assert!(instructions.contains("desktop_click"));
    }

    #[test]
    fn the_skill_names_the_tools_the_harness_registers() {
        let instructions = skill_instructions();
        for tool in [
            "desktop_status",
            "desktop_list_apps",
            "desktop_list_windows",
            "desktop_snapshot",
            "desktop_find",
            "desktop_get",
            "desktop_is",
            "desktop_click",
            "desktop_type",
            "desktop_set_value",
            "desktop_toggle",
            "desktop_focus",
            "desktop_key",
            "desktop_paste",
            "desktop_click_at",
            "desktop_screenshot",
        ] {
            assert!(instructions.contains(tool), "{tool} is not documented");
        }
    }
}
