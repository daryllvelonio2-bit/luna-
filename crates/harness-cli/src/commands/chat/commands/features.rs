//! Runtime feature toggles: the CLI's own copy of the flags the menu reads, the agent-side
//! switch that must move with them, and the argument parsers for the toggle commands.

use colored::Colorize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::commands::chat::ui::ChatUi;
use crate::commands::chat::ChatAgent;

#[cfg(test)]
use harness_core::Config;

#[cfg(test)]
use super::command_entries;

/// A feature the user can switch off at runtime with a slash command.
///
/// The atomics are the CLI's own copy: the menu and the non-interactive loop read them without
/// taking the agent lock, while the agent's [`harness_core::AgentConfig`] holds the authoritative
/// flags the turn loop and dispatcher check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Feature {
    Memory,
    Notes,
    Skills,
    Mcp,
}

impl Feature {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Feature::Memory => "memory",
            Feature::Notes => "notes",
            Feature::Skills => "skills",
            Feature::Mcp => "mcp",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Feature::Memory => "Saved memory",
            Feature::Notes => "Notes",
            Feature::Skills => "Skills",
            Feature::Mcp => "MCP",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FeatureToggles {
    memory: Arc<AtomicBool>,
    notes: Arc<AtomicBool>,
    skills: Arc<AtomicBool>,
    mcp: Arc<AtomicBool>,
}

impl Default for FeatureToggles {
    fn default() -> Self {
        Self {
            memory: Arc::new(AtomicBool::new(true)),
            notes: Arc::new(AtomicBool::new(true)),
            skills: Arc::new(AtomicBool::new(true)),
            mcp: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl FeatureToggles {
    pub(crate) fn enabled(&self, feature: Feature) -> bool {
        match feature {
            Feature::Memory => self.memory.load(Ordering::Relaxed),
            Feature::Notes => self.notes.load(Ordering::Relaxed),
            Feature::Skills => self.skills.load(Ordering::Relaxed),
            Feature::Mcp => self.mcp.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn set(&self, feature: Feature, enabled: bool) {
        let slot = match feature {
            Feature::Memory => &self.memory,
            Feature::Notes => &self.notes,
            Feature::Skills => &self.skills,
            Feature::Mcp => &self.mcp,
        };
        slot.store(enabled, Ordering::SeqCst);
    }

    pub(crate) fn notes_atomic(&self) -> Arc<AtomicBool> {
        self.notes.clone()
    }
}

/// Parse the argument of `/memory`, `/notes`, `/skills`, or `/mcp`.
///
/// `Ok(None)` is a status query (no argument), `Err(())` is an argument the command does not
/// understand, and the message for it belongs to the caller so the usage line can name the
/// command the user actually typed.
fn parse_feature_arg(arg: Option<&str>) -> Result<Option<bool>, ()> {
    match arg
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("") | Some("status") => Ok(None),
        Some("on") | Some("enable") | Some("enabled") | Some("true") => Ok(Some(true)),
        Some("off") | Some("disable") | Some("disabled") | Some("false") => Ok(Some(false)),
        Some(_) => Err(()),
    }
}

/// Parse `/profile ...` into the CLI subcommand it maps to.
pub(super) fn parse_profile_command(
    arg: Option<&str>,
) -> Option<crate::commands::profile::ProfileCommand> {
    use crate::commands::profile::ProfileCommand;
    let mut parts = arg?.split_whitespace();
    match parts.next()? {
        "list" | "ls" => Some(ProfileCommand::List),
        "show" => Some(ProfileCommand::Show {
            name: parts.next().map(str::to_string),
            full: parts.any(|part| part == "--full"),
        }),
        "path" => Some(ProfileCommand::Path {
            name: parts.next().map(str::to_string),
        }),
        "create" | "new" => Some(ProfileCommand::Create {
            name: parts.next().map(str::to_string),
        }),
        "delete" | "rm" => Some(ProfileCommand::Delete {
            name: parts.next()?.to_string(),
            force: false,
        }),
        _ => None,
    }
}

/// Apply a runtime feature toggle to both the CLI's copy and the live agent.
///
/// The agent holds the flags the turn loop filters on, so a command that only changed the CLI's
/// atomics would show "OFF" while the model kept its tools.
pub(crate) async fn handle_feature_toggle(
    ui: &ChatUi,
    toggles: &FeatureToggles,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    feature: Feature,
    arg: Option<&str>,
) {
    let desired = match parse_feature_arg(arg) {
        Ok(value) => value,
        Err(()) => {
            ui.emit_above(&format!("{} Usage: /{} on|off", "•".cyan(), feature.name()));
            return;
        }
    };

    let enabled = match desired {
        Some(enabled) => {
            toggles.set(feature, enabled);
            let mut agent = agent.lock().await;
            match feature {
                Feature::Memory => agent.set_memory_enabled(enabled),
                Feature::Notes => agent.set_notes_enabled(enabled),
                Feature::Skills => agent.set_skills_enabled(enabled),
                Feature::Mcp => agent.set_mcp_enabled(enabled),
            }
            enabled
        }
        None => toggles.enabled(feature),
    };

    let state = if enabled {
        "ON".green().bold().to_string()
    } else {
        "OFF".dimmed().to_string()
    };
    ui.emit_above(&format!(
        "{} {} is {} for this session",
        "•".cyan(),
        feature.title(),
        state
    ));
}

/// Whether a line that does NOT begin with `/` is a bare feature toggle command like `memory off`
/// or `notes status` or `skills off`.
pub(crate) fn is_bare_feature_command(line: &str) -> bool {
    let unquoted = line.trim().trim_matches(['"', '\'', '`']);
    let mut tokens = unquoted.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };
    let first_lower = first.to_ascii_lowercase();
    if !matches!(first_lower.as_str(), "memory" | "notes" | "skills" | "mcp") {
        return false;
    }
    match tokens.next() {
        None | Some("on") | Some("off") | Some("enable") | Some("disable") | Some("enabled")
        | Some("disabled") | Some("status") | Some("list") | Some("true") | Some("false") => {
            tokens.next().is_none()
        }
        _ => false,
    }
}

#[cfg(test)]
mod feature_toggle_tests {
    use super::*;

    /// `/memory off` and its siblings accept the words people actually type; a bare command is a
    /// status query, and anything else is a usage error rather than a silent no-op.
    #[test]
    fn feature_arguments_are_on_off_or_a_status_query() {
        assert_eq!(parse_feature_arg(None), Ok(None));
        assert_eq!(parse_feature_arg(Some("")), Ok(None));
        assert_eq!(parse_feature_arg(Some("status")), Ok(None));
        for on in ["on", "enable", "enabled", "true", "ON"] {
            assert_eq!(parse_feature_arg(Some(on)), Ok(Some(true)), "{on}");
        }
        for off in ["off", "disable", "disabled", "false", "OFF"] {
            assert_eq!(parse_feature_arg(Some(off)), Ok(Some(false)), "{off}");
        }
        assert_eq!(parse_feature_arg(Some("maybe")), Err(()));
    }

    /// Each toggle is independent: turning one off must not silently take the others with it.
    #[test]
    fn toggles_default_on_and_flip_independently() {
        let toggles = FeatureToggles::default();
        let all = [
            Feature::Memory,
            Feature::Notes,
            Feature::Skills,
            Feature::Mcp,
        ];
        for feature in all {
            assert!(toggles.enabled(feature), "{feature:?} starts on");
        }

        toggles.set(Feature::Memory, false);
        assert!(!toggles.enabled(Feature::Memory));
        assert!(toggles.enabled(Feature::Notes));
        assert!(toggles.enabled(Feature::Skills));
        assert!(toggles.enabled(Feature::Mcp));

        toggles.set(Feature::Memory, true);
        assert!(toggles.enabled(Feature::Memory));
    }

    /// The skill commands are the one part of the menu that belongs to a toggle: with skills off
    /// they disappear, while the built-in `/skills` command stays so they can be turned back on.
    #[test]
    fn disabled_skills_disappear_from_the_command_menu() {
        use harness_plugins::Skill;

        let skill = Skill {
            name: "review".to_string(),
            description: "Review a diff carefully.".to_string(),
            instructions: "Do the thing.".to_string(),
            file_path: std::path::PathBuf::from("review/SKILL.md"),
            command: None,
            timeout_secs: None,
            parameters: None,
        };
        let config = Config::default();

        let on = command_entries(&config, std::slice::from_ref(&skill), true);
        assert!(on.iter().any(|entry| entry.name == "/review"));
        assert!(on.iter().any(|entry| entry.name == "/skills"));

        let off = command_entries(&config, std::slice::from_ref(&skill), false);
        assert!(!off.iter().any(|entry| entry.name == "/review"));
        assert!(off.iter().any(|entry| entry.name == "/skills"));
        assert!(off.iter().any(|entry| entry.name == "/memory"));
        assert!(off.iter().any(|entry| entry.name == "/notes"));
    }

    #[test]
    fn bare_feature_command_recognition() {
        assert!(is_bare_feature_command("memory off"));
        assert!(is_bare_feature_command("memory on"));
        assert!(is_bare_feature_command("memory status"));
        assert!(is_bare_feature_command("notes off"));
        assert!(is_bare_feature_command("skills off"));
        assert!(is_bare_feature_command("mcp off"));
        assert!(is_bare_feature_command("  notes   disable  "));
        assert!(is_bare_feature_command("memory"));
        assert!(is_bare_feature_command("\"memory off\""));

        assert!(!is_bare_feature_command("notes off please"));
        assert!(!is_bare_feature_command("memory is a programming concept"));
        assert!(!is_bare_feature_command("fix this bug"));
    }
}
