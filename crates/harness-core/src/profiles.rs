//! Profiles: a named persona + behavior pair.
//!
//! A profile is deliberately small. It carries only the two sections a user may rewrite — the
//! persona (voice, tone, boundaries) and the behavior (output rules, execution policy) — and the
//! harness appends the static contract from `prompts.md` around them. Nothing in a profile can
//! remove, reorder, or duplicate the tool contract or the memory/note protocols.
//!
//! Built-in profiles are compiled in from `profiles.md`. Custom profiles are directories under
//! `paths.profiles_dir`; each holds a `profile.md` and every piece of state that belongs to it.
//! A directory without `profile.md` is ignored, so a crashed creation never appears in the catalog.

use std::path::{Path, PathBuf};

use crate::config::{Config, PathsSettings};
use crate::error::HarnessError;

/// The built-in profile every installation starts with.
pub const DEFAULT_PROFILE: &str = "default";
/// The file that holds the built-in profiles.
const PROFILES_FILE: &str = include_str!("../profiles.md");
/// The marker that names the single built-in section in `profiles.md`.
const PROFILE_DEFAULT_SECTION: &str = "PROFILE_DEFAULT";
/// The file that describes a custom profile inside its directory.
pub const PROFILE_FILE: &str = "profile.md";

/// The editable parts of a profile as they appear on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileText {
    pub summary: Option<String>,
    pub persona: Option<String>,
    pub behavior: Option<String>,
}

impl ProfileText {
    /// Parse `# Summary`, `# Persona`, and `# Behavior` sections.
    ///
    /// A missing section is `None` (inherit). A section that is present but empty is an error: it
    /// is almost always a truncated edit, and silently inheriting would hide the mistake.
    pub fn parse(raw: &str) -> Result<Self, HarnessError> {
        let mut out = ProfileText::default();
        let mut current: Option<&str> = None;
        let mut body = String::new();

        for line in raw.lines() {
            if let Some(heading) = line.strip_prefix("# ") {
                let heading = heading.trim();
                if matches!(heading, "Summary" | "Persona" | "Behavior") {
                    flush(&mut out, current, &mut body)?;
                    current = Some(match heading {
                        "Summary" => "Summary",
                        "Persona" => "Persona",
                        _ => "Behavior",
                    });
                    continue;
                }
            }
            if current.is_some() {
                body.push_str(line);
                body.push('\n');
            }
        }
        flush(&mut out, current, &mut body)?;

        if out.persona.is_none() && out.behavior.is_none() {
            return Err(HarnessError::ConfigurationError(
                "profile has neither a '# Persona' nor a '# Behavior' section".to_string(),
            ));
        }
        Ok(out)
    }
}

/// A fully resolved profile: missing sections have inherited `default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub summary: String,
    pub persona: String,
    pub behavior: String,
    pub builtin: bool,
    /// The profile's own directory, or `None` for the built-in profile (which keeps the legacy
    /// `~/.luna/*` state paths).
    pub dir: Option<PathBuf>,
}

impl Profile {
    pub fn is_default(&self) -> bool {
        self.name == DEFAULT_PROFILE
    }
}

/// One catalog entry, enough to render a menu without loading every profile body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSummary {
    pub name: String,
    pub summary: String,
    pub builtin: bool,
    pub dir: Option<PathBuf>,
}

/// The state paths a custom profile owns. Everything here lives under the profile directory, so
/// two profiles can never read or write each other's transcript, notes, or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePaths {
    pub root: PathBuf,
    pub memory_db: PathBuf,
    pub notes_db: PathBuf,
    pub notes_dir: PathBuf,
    pub sessions_db: PathBuf,
    pub credentials: PathBuf,
    pub usage_db: PathBuf,
    pub checkpoints_dir: PathBuf,
    pub todos_dir: PathBuf,
    pub jobs_dir: PathBuf,
    pub skills_dir: PathBuf,
}

/// Derive a profile's state paths from its directory.
pub fn paths_for(root: &Path) -> ProfilePaths {
    ProfilePaths {
        root: root.to_path_buf(),
        memory_db: root.join("memory.db"),
        notes_db: root.join("notes.db"),
        notes_dir: root.join("notes"),
        sessions_db: root.join("sessions.db"),
        credentials: root.join("credentials.json"),
        usage_db: root.join("usage.db"),
        checkpoints_dir: root.join("checkpoints"),
        todos_dir: root.join("todos"),
        jobs_dir: root.join("jobs"),
        skills_dir: root.join("skills"),
    }
}

impl ProfilePaths {
    /// Create every directory the profile writes into. Called before any write, so a profile
    /// that exists on disk is also a profile whose notes folder exists.
    pub fn ensure_dirs(&self) -> Result<(), HarnessError> {
        for dir in [
            &self.root,
            &self.notes_dir,
            &self.checkpoints_dir,
            &self.todos_dir,
            &self.jobs_dir,
            &self.skills_dir,
        ] {
            std::fs::create_dir_all(dir).map_err(|error| {
                HarnessError::Io(format!(
                    "failed to create profile directory {}: {}",
                    dir.display(),
                    error
                ))
            })?;
        }
        Ok(())
    }
}

/// The default profile directory root: `~/.luna/profiles`.
///
/// Resolved through [`PathsSettings`] so a relocated `paths.profiles_dir` cannot be bypassed by a
/// second spelling of the home directory here.
pub fn profiles_root() -> PathBuf {
    PathsSettings::default().profiles_dir
}

fn flush(
    out: &mut ProfileText,
    heading: Option<&str>,
    body: &mut String,
) -> Result<(), HarnessError> {
    let Some(heading) = heading else {
        body.clear();
        return Ok(());
    };
    let text = body.trim();
    if text.is_empty() {
        return Err(HarnessError::ConfigurationError(format!(
            "profile section '# {}' is empty",
            heading
        )));
    }
    match heading {
        "Summary" => out.summary = Some(text.to_string()),
        "Persona" => out.persona = Some(text.to_string()),
        "Behavior" => out.behavior = Some(text.to_string()),
        _ => {}
    }
    body.clear();
    Ok(())
}

/// Indices of the first `===== NAME =====` section body, or an error when it is missing.
fn section_from(file: &str, name: &str, source: &str) -> Result<String, HarnessError> {
    let marker = format!("===== {} =====", name);
    let body = file
        .split('\n')
        .skip_while(|line| line.trim() != marker)
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with("====="))
        .collect::<Vec<_>>()
        .join("\n");
    if body.trim().is_empty() {
        return Err(HarnessError::ConfigurationError(format!(
            "profile section '{}' is missing or empty in {}",
            name, source
        )));
    }
    Ok(body.trim_matches('\n').to_string())
}

/// The compiled-in `default` profile.
pub fn default_profile() -> Result<Profile, HarnessError> {
    let raw = section_from(
        PROFILES_FILE,
        PROFILE_DEFAULT_SECTION,
        "crates/harness-core/profiles.md",
    )?;
    let text = ProfileText::parse(&raw)?;
    let persona = text.persona.ok_or_else(|| {
        HarnessError::ConfigurationError(
            "the built-in default profile has no '# Persona' section".to_string(),
        )
    })?;
    let behavior = text.behavior.ok_or_else(|| {
        HarnessError::ConfigurationError(
            "the built-in default profile has no '# Behavior' section".to_string(),
        )
    })?;
    let summary = text
        .summary
        .unwrap_or_else(|| first_line(&persona).to_string());
    Ok(Profile {
        name: DEFAULT_PROFILE.to_string(),
        summary,
        persona,
        behavior,
        builtin: true,
        dir: None,
    })
}

/// Load a profile by name, resolving missing sections from `default`.
pub fn load(name: &str, profiles_dir: &Path) -> Result<Profile, HarnessError> {
    if name == DEFAULT_PROFILE {
        return default_profile();
    }
    let dir = profile_dir(profiles_dir, name);
    let path = dir.join(PROFILE_FILE);
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            HarnessError::ConfigurationError(format!(
                "profile '{}' not found at {}",
                name,
                path.display()
            ))
        } else {
            HarnessError::Io(format!("failed to read {}: {}", path.display(), error))
        }
    })?;
    let text = ProfileText::parse(&raw)?;
    let default = default_profile()?;
    let persona = text.persona.unwrap_or(default.persona);
    let behavior = text.behavior.unwrap_or(default.behavior);
    let summary = text
        .summary
        .unwrap_or_else(|| first_line(&persona).to_string());
    Ok(Profile {
        name: name.to_string(),
        summary,
        persona,
        behavior,
        builtin: false,
        dir: Some(dir),
    })
}

/// The directory a custom profile owns.
pub fn profile_dir(profiles_dir: &Path, name: &str) -> PathBuf {
    profiles_dir.join(name)
}

/// The default profile plus every readable custom profile, default first and custom sorted by
/// name. A malformed profile is skipped with a warning rather than breaking the list.
pub fn catalog(profiles_dir: &Path) -> Vec<ProfileSummary> {
    let mut out = Vec::new();
    match default_profile() {
        Ok(profile) => out.push(ProfileSummary {
            name: profile.name,
            summary: profile.summary,
            builtin: true,
            dir: None,
        }),
        Err(error) => tracing::warn!("built-in default profile is unreadable: {}", error),
    }

    let entries = match std::fs::read_dir(profiles_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return out,
        Err(error) => {
            tracing::warn!(
                "could not read profiles directory '{}': {}",
                profiles_dir.display(),
                error
            );
            return out;
        }
    };

    let mut custom = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || !path.join(PROFILE_FILE).is_file() {
            continue;
        }
        match load(name, profiles_dir) {
            Ok(profile) => custom.push(ProfileSummary {
                name: profile.name,
                summary: profile.summary,
                builtin: false,
                dir: profile.dir,
            }),
            Err(error) => tracing::warn!("skipping profile '{}': {}", name, error),
        }
    }
    custom.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(custom);
    out
}

/// A slug that is safe as a directory name, a launcher name, and a command argument.
pub fn validate_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "profile name is empty".to_string(),
        ));
    }
    if name.len() > 32 {
        return Err(HarnessError::ConfigurationError(
            "profile name must be 32 characters or fewer".to_string(),
        ));
    }
    if name == DEFAULT_PROFILE {
        return Err(HarnessError::ConfigurationError(
            "'default' is the built-in profile and cannot be replaced".to_string(),
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(HarnessError::ConfigurationError(
            "profile name cannot start or end with '-'".to_string(),
        ));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(HarnessError::ConfigurationError(
            "profile name may contain only lowercase letters, digits, and '-'".to_string(),
        ));
    }
    if RESERVED_NAMES.contains(&name) {
        return Err(HarnessError::ConfigurationError(format!(
            "profile name '{}' collides with a luna command",
            name
        )));
    }
    Ok(())
}

/// Names that would shadow a subcommand or a common built-in. A launcher with one of these names
/// would change what `luna` does rather than open a profile.
pub const RESERVED_NAMES: &[&str] = &[
    "auth",
    "chat",
    "config",
    "default",
    "diff",
    "eval",
    "exec",
    "fetch",
    "gateway",
    "hb",
    "heartbeat",
    "help",
    "hooks",
    "luna",
    "mcp",
    "models",
    "onboard",
    "onboarding",
    "profile",
    "sessions",
    "setup",
    "skill",
    "supervisor",
    "usage",
    "version",
    "voice",
];

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// Apply a profile to a resolved config: its text becomes the model-facing persona/behavior, and
/// a custom profile's state paths are forced inside its own directory.
///
/// Forcing the paths after resolution is what makes the isolation a guarantee rather than a
/// convention: a profile's `config.yaml` cannot point `sessions_db` at another profile's data.
pub fn apply_to_config(config: &mut Config, profile: &Profile) -> Result<(), HarnessError> {
    config.agent.profile_persona = Some(profile.persona.clone());
    config.agent.profile_behavior = Some(profile.behavior.clone());

    if let Some(dir) = &profile.dir {
        let paths = paths_for(dir);
        paths.ensure_dirs()?;
        config.paths.memory_db = paths.memory_db;
        config.paths.notes_db = paths.notes_db;
        config.paths.notes_dir = paths.notes_dir;
        config.paths.sessions_db = paths.sessions_db;
        config.paths.credentials = paths.credentials;
        config.paths.usage_db = paths.usage_db;
        config.paths.checkpoints_dir = paths.checkpoints_dir;
        config.paths.todos_dir = paths.todos_dir;
        config.paths.jobs_dir = paths.jobs_dir;
        config.paths.skills_dir = paths.skills_dir;
    }
    Ok(())
}

/// Render a profile back to the on-disk `# Summary` / `# Persona` / `# Behavior` shape.
pub fn render(profile: &Profile) -> String {
    format!(
        "# Summary\n{}\n\n# Persona\n{}\n\n# Behavior\n{}\n",
        profile.summary.trim(),
        profile.persona.trim(),
        profile.behavior.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_catalog_has_exactly_the_default_profile() {
        let default = default_profile().expect("built-in profile loads");
        assert_eq!(default.name, DEFAULT_PROFILE);
        assert!(default.builtin);
        assert!(default.persona.contains("Neutral assistant"));
        assert!(default.behavior.contains("Output Rules"));
        assert!(!default.persona.to_ascii_lowercase().contains("tsundere"));
    }

    #[test]
    fn a_profile_can_override_one_section() {
        let text = ProfileText::parse("# Persona\nRole: Pirate\n\n# Behavior\n- Say arr.\n")
            .expect("valid profile");
        assert_eq!(text.persona.as_deref(), Some("Role: Pirate"));
        assert_eq!(text.behavior.as_deref(), Some("- Say arr."));
    }

    #[test]
    fn a_missing_section_inherits_the_default() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("terra")).unwrap();
        std::fs::write(
            dir.path().join("terra/profile.md"),
            "# Persona\nRole: Terra\n",
        )
        .unwrap();
        let profile = load("terra", dir.path()).expect("loads");
        assert_eq!(profile.persona, "Role: Terra");
        assert_eq!(
            profile.behavior,
            default_profile().unwrap().behavior,
            "reader inherits behavior"
        );
    }

    #[test]
    fn an_empty_section_is_an_error() {
        let error = ProfileText::parse("# Persona\n\n# Behavior\nDo it.\n")
            .expect_err("empty persona rejected");
        assert!(error.to_string().contains("empty"), "{error}");
    }

    #[test]
    fn names_are_slugged_and_reserved_names_are_refused() {
        assert!(validate_name("terra").is_ok());
        assert!(validate_name("terra-2").is_ok());
        assert!(validate_name("Terra").is_err());
        assert!(validate_name("terra_2").is_err());
        assert!(validate_name("-terra").is_err());
        assert!(validate_name("exec").is_err());
        assert!(validate_name("default").is_err());
    }

    #[test]
    fn a_custom_profile_gets_private_state_paths() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("terra");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(PROFILE_FILE), "# Persona\nRole: Terra\n").unwrap();
        let profile = load("terra", dir.path()).expect("loads");
        let mut config = Config::default();
        apply_to_config(&mut config, &profile).expect("applies");
        assert_eq!(config.paths.sessions_db, root.join("sessions.db"));
        assert_eq!(config.paths.notes_dir, root.join("notes"));
        assert!(config.paths.notes_dir.is_dir());
        assert_eq!(config.agent.profile_persona.as_deref(), Some("Role: Terra"));
    }
}
