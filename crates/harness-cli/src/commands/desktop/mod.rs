//! The desktop tools, and the built-in skill that documents them.
//!
//! The skill ships inside the binary, so it is always present and always
//! matches the tools that are registered beside it. An agent that needs to
//! know what the desktop commands are reads the skill; the skill tells it to
//! call the tools.

pub mod engine;
pub mod tools;

use harness_plugins::Skill;

/// The skills the harness ships with.
///
/// The desktop skill is one of them. It is a built-in rather than a file in
/// `~/.luna/skills` because the tools it describes are part of the harness:
/// a skill that can be uninstalled while its tools stay is a skill that lies.
pub fn builtin_skills() -> Vec<Skill> {
    vec![Skill::builtin(
        harness_desktop::skill_name(),
        harness_desktop::skill_description(),
        harness_desktop::skill_instructions(),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::registry::DefaultToolRegistry;
    use harness_core::traits::{Tool, ToolRegistry};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn the_builtin_skill_is_the_desktop_skill() {
        let skills = builtin_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "agent-desktop");
        assert!(skills[0].description.contains("desktop applications"));
        assert!(
            skills[0].instructions.contains("desktop_snapshot"),
            "the skill must document the tools"
        );
    }

    #[tokio::test]
    async fn the_skill_is_readable_by_either_spelling() {
        let skills = Arc::new(builtin_skills());
        let view = harness_plugins::SkillViewTool::new(skills);
        for name in ["agent-desktop", "agent_desktop", "AGENT_DESKTOP"] {
            let result = view
                .execute(
                    "call-1",
                    &serde_json::json!({ "name": name }),
                    &CancellationToken::new(),
                )
                .await
                .expect("the skill is readable");
            assert!(!result.is_error, "{name} was not found: {}", result.content);
            assert!(result.content.contains("desktop_click"), "{name}");
        }
    }

    #[tokio::test]
    async fn every_desktop_tool_is_registered() {
        let registry = Arc::new(DefaultToolRegistry::new());
        tools::register(registry.as_ref())
            .await
            .expect("the desktop tools register");
        let definitions = registry.list_tools().await.expect("the tool list");
        let names: Vec<&str> = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        for expected in [
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
            assert!(names.contains(&expected), "{expected} is not registered");
        }
    }

    #[tokio::test]
    async fn only_the_tools_that_change_the_screen_are_gated() {
        let registry = Arc::new(DefaultToolRegistry::new());
        tools::register(registry.as_ref())
            .await
            .expect("the desktop tools register");
        for gated in [
            "desktop_click",
            "desktop_type",
            "desktop_set_value",
            "desktop_toggle",
            "desktop_focus",
            "desktop_key",
            "desktop_paste",
            "desktop_click_at",
        ] {
            assert!(
                registry.is_mutating(gated).await.expect("a verdict"),
                "{gated} must go through the approval gate"
            );
        }
        for read_only in [
            "desktop_status",
            "desktop_list_apps",
            "desktop_list_windows",
            "desktop_snapshot",
            "desktop_find",
            "desktop_get",
            "desktop_is",
            "desktop_screenshot",
        ] {
            assert!(
                !registry.is_mutating(read_only).await.expect("a verdict"),
                "{read_only} must not be gated"
            );
        }
    }

    #[tokio::test]
    async fn the_status_tool_answers_on_this_machine() {
        // The live path: the tool, the engine and the platform adapter, on the
        // machine the test runs on. Whether a desktop session is there or not,
        // the tool must answer with an envelope that says which it was.
        let registry = Arc::new(DefaultToolRegistry::new());
        tools::register(registry.as_ref())
            .await
            .expect("the desktop tools register");
        let call = harness_core::types::ToolCall {
            id: "call-1".to_string(),
            name: "desktop_status".to_string(),
            arguments: serde_json::json!({}),
            signature: None,
        };
        let result = registry
            .execute(&call, &CancellationToken::new())
            .await
            .expect("the tool answers");
        assert!(
            result.content.contains("platform") || result.content.contains("PLATFORM_NOT_SUPPORTED"),
            "the answer must say what the desktop can do: {}",
            result.content
        );
    }
}
