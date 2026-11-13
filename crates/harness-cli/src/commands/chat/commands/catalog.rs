//! The slash-command surface as data: the static table the picker lists, the row type it
//! carries, the merge with configured workers and installed skills, and the one predicate that
//! tells a typed absolute path apart from a command.

use harness_core::Config;

use super::worker;

pub struct SlashCommand {
    pub name: &'static str,
    pub details: &'static str,
}

/// One row in the slash menu: a built-in command or a configured worker.
///
/// Built-ins are `&'static str`, workers are owned, so the menu works on owned rows uniformly.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub details: String,
}

/// The built-in command table plus every configured worker, as `/<name>`.
///
/// A worker whose name collides with a built-in never appears: the dispatcher gives built-ins
/// priority, so listing it as a worker would be a lie.
pub fn command_entries(
    config: &Config,
    skills: &[harness_plugins::Skill],
    skills_enabled: bool,
) -> Vec<CommandEntry> {
    let mut entries: Vec<CommandEntry> = SLASH_COMMANDS
        .iter()
        .map(|command| CommandEntry {
            name: command.name.to_string(),
            details: command.details.to_string(),
        })
        .collect();

    for (name, worker) in &config.workers {
        let command = format!("/{name}");
        if entries
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(&command))
            || worker::RESERVED_COMMAND_NAMES.contains(&command.as_str())
        {
            continue;
        }
        entries.push(CommandEntry {
            name: command,
            details: format!("Worker · {} ({})", worker.model, worker.provider),
        });
    }

    // Installed skills are invokable too: `/<skill-name> [input]` runs the skill's instructions as
    // the next turn. Built-ins and workers keep their names; a skill cannot shadow them. With
    // `/skills off` the skill commands disappear from the menu and from `/help`, but the built-in
    // `/skills` command stays so the feature can be turned back on.
    if skills_enabled {
        for skill in skills {
            let command = format!("/{}", skill.name);
            if skill.name.is_empty()
                || skill.name.contains(char::is_whitespace)
                || skill.name.contains('/')
                || entries
                    .iter()
                    .any(|entry| entry.name.eq_ignore_ascii_case(&command))
            {
                continue;
            }
            entries.push(CommandEntry {
                name: command,
                details: skill_command_details(&skill.description),
            });
        }
    }

    entries
}

/// Whether a line that begins with `/` is a filesystem path rather than a slash command.
///
/// The composer routes every line starting with `/` to the command dispatcher, which rejects an
/// absolute path such as `/home/me/notes.txt` as an unknown command before it can reach the agent.
/// A line is a path when its first token — the only part that can name a command — starts with `/`
/// and either contains a second separator or names an existing absolute path. A command with a
/// path argument (`/dir /tmp`) is still a command because only the first token is inspected.
pub(crate) fn looks_like_path(line: &str) -> bool {
    // Quotes are how a path with spaces arrives from a paste; they are not part of the path.
    let unquoted = line.trim_start().trim_start_matches(['"', '\'', '`']);
    let token = unquoted.split_whitespace().next().unwrap_or_default();
    let Some(rest) = token.strip_prefix('/') else {
        return false;
    };
    if rest.is_empty() {
        // A bare `/` opens the command menu; it is not a path.
        return false;
    }
    // `/home/me/notes.txt` — a second separator settles it without touching the filesystem.
    if rest.contains('/') {
        return true;
    }
    // `/tmp`, `/etc`: a single component can still be a real path.
    std::path::Path::new(token).exists()
}

/// Keep the slash menu and `/help` scannable: skill descriptions are written for the model and can
/// run to a paragraph, so the command list shows only the first line, capped.
fn skill_command_details(description: &str) -> String {
    const MAX_CHARS: usize = 80;
    let first_line = description.lines().next().unwrap_or("").trim();
    let mut text: String = first_line.chars().take(MAX_CHARS).collect();
    if first_line.chars().count() > MAX_CHARS {
        text.push('…');
    }
    format!("Skill · {text}")
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/model",
        details: "Switch provider and model",
    },
    SlashCommand {
        name: "/models",
        details: "Switch provider and model (alias: /model)",
    },
    SlashCommand {
        name: "/default_model",
        details: "Choose the default provider and model (this session or global)",
    },
    SlashCommand {
        name: "/compact-model",
        details: "Choose provider and model for context compaction",
    },
    SlashCommand {
        name: "/bgsaver-model",
        details: "Choose provider and model for the background memory saver",
    },
    SlashCommand {
        name: "/thinking",
        details: "Set thinking level (off, low, medium, high)",
    },
    SlashCommand {
        name: "/think",
        details: "Alias for /thinking",
    },
    SlashCommand {
        name: "/reasoning",
        details: "Set reasoning effort (alias: /thinking)",
    },
    SlashCommand {
        name: "/name",
        details: "Set or view session name",
    },
    SlashCommand {
        name: "/sessions",
        details: "Interactive session picker to switch chat sessions (alias: /session)",
    },
    SlashCommand {
        name: "/new",
        details: "Start a fresh chat session",
    },
    SlashCommand {
        name: "/summarize",
        details: "Summarize and start a new session with it",
    },
    SlashCommand {
        name: "/rewind",
        details: "List turn checkpoints, or restore the files of one (/rewind 3)",
    },
    SlashCommand {
        name: "/diff",
        details: "Review what a turn changed on disk (/diff, /diff 3, /diff --all)",
    },
    SlashCommand {
        name: "/compact",
        details: "Fold oldest turns into a summary in this session",
    },
    SlashCommand {
        name: "/auto-compact",
        details: "Set auto-compaction in a box: threshold, then turns (on|off to toggle)",
    },
    SlashCommand {
        name: "/expand",
        details: "Expand intermediate tool and reasoning processes (or Ctrl+O)",
    },
    SlashCommand {
        name: "/collapse",
        details: "Collapse intermediate tool and reasoning processes (or Ctrl+O)",
    },
    SlashCommand {
        name: "/skills",
        details: "List available skills, or turn skills on/off (/skills on|off)",
    },
    SlashCommand {
        name: "/mcp",
        details: "List MCP servers and tools, or turn MCP on/off (/mcp on|off)",
    },
    SlashCommand {
        name: "/memory",
        details: "Turn saved memory on/off for this session (/memory on|off)",
    },
    SlashCommand {
        name: "/notes",
        details: "Turn notes on/off for this session (/notes on|off)",
    },
    SlashCommand {
        name: "/remember",
        details: "Run a memory pass over this session now instead of waiting for the interval",
    },
    SlashCommand {
        name: "/plan",
        details: "Toggle read-only plan mode (or Tab key)",
    },
    SlashCommand {
        name: "/goal",
        details:
            "Run an autonomous loop until a goal is achieved (/goal <objective>, status, stop)",
    },
    SlashCommand {
        name: "/fork",
        details: "Fork current session into a new branch session",
    },
    SlashCommand {
        name: "/tasks",
        details: "Inspect background task output and delegated subagents (Ctrl+Shift+B)",
    },
    SlashCommand {
        name: "/jobs",
        details: "Inspect background task output and delegated subagents (Ctrl+Shift+B)",
    },
    SlashCommand {
        name: "/subagents",
        details: "Inspect background task output and delegated subagents (Ctrl+Shift+B)",
    },
    SlashCommand {
        name: "/usage",
        details: "Show tokens, cache, cost and context for this session",
    },
    SlashCommand {
        name: "/todo",
        details: "Fold or unfold the todo list (Ctrl+T)",
    },
    SlashCommand {
        name: "/clear",
        details: "Clear terminal screen",
    },
    SlashCommand {
        name: "/steer",
        details: "Insert guidance that lands right after the next model call (/steer <text>)",
    },
    SlashCommand {
        name: "/queue",
        details: "Hold a message and send it when the turn in flight finishes (/queue <text>)",
    },
    SlashCommand {
        name: "/interrupt",
        details: "Abort the model request in flight and give the prompt back",
    },
    SlashCommand {
        name: "/heartbeat",
        details: "Show harness liveness, system health, and background status (or /hb)",
    },
    SlashCommand {
        name: "/hb",
        details: "Alias for /heartbeat",
    },
    SlashCommand {
        name: "/connect",
        details: "Add providers and API keys",
    },
    SlashCommand {
        name: "/dir",
        details: "Inspect, change, or reset workspace directory (/dir reset to restore default)",
    },
    SlashCommand {
        name: "/workspace",
        details: "Inspect, change, or reset workspace directory (alias: /dir)",
    },
    SlashCommand {
        name: "/delete",
        details: "Delete current session and start a fresh one",
    },
    SlashCommand {
        name: "/profile",
        details: "List, create, show, or delete profiles (/profile create [name])",
    },
    SlashCommand {
        name: "/worker",
        details: "Create, list, or delete named workers invoked as /<name> <task>",
    },
    SlashCommand {
        name: "/settings",
        details: "Configure CLI display limits, collapse behavior, and preferences",
    },
    SlashCommand {
        name: "/exit",
        details: "Exit Luna chat session",
    },
];

#[cfg(test)]
mod path_tests {
    use super::looks_like_path;

    /// A command name and its argument are still a command, even when the argument is a path.
    #[test]
    fn commands_are_not_paths() {
        for line in [
            "/help",
            "/model",
            "/model gpt-5",
            "/dir /tmp",
            "/queue hello",
            "exit",
        ] {
            assert!(!looks_like_path(line), "{line:?} should be a command");
        }
    }

    /// Anything with a second separator in its first token is a path, and so is a quoted path
    /// whose first segment would otherwise look like a command.
    #[test]
    fn multi_segment_absolute_paths_are_paths() {
        for line in [
            "/home/janelle/notes.txt",
            "/var/log/luna/luna.log and explain it",
            "\"/home/me/a file with spaces.txt\"",
            "  /usr/local/bin/luna",
        ] {
            assert!(looks_like_path(line), "{line:?} should be a path");
        }
    }

    /// A bare slash opens the menu, and an unknown single word is still a mistyped command.
    #[test]
    fn a_bare_slash_is_not_a_path() {
        assert!(!looks_like_path("/"));
        assert!(!looks_like_path("   /   "));
        assert!(!looks_like_path("/definitely-not-a-command"));
    }

    /// A single-component absolute path that exists on disk is a path too, so `/tmp` reaches the
    /// agent instead of being rejected as an unknown command.
    #[cfg(unix)]
    #[test]
    fn an_existing_single_component_is_a_path() {
        assert!(looks_like_path("/tmp"));
        assert!(looks_like_path("/etc"));
    }
}
