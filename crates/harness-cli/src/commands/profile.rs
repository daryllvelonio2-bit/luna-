//! Profile selection, listing, and the guided creation wizard.
//!
//! A profile is opened by invocation: bare `luna` runs the built-in `default`, `luna terra` runs
//! `terra`, and creating a profile installs a `~/.local/bin/<name>` launcher so typing the name
//! alone works. There is no persistent "active profile" config key.
//!
//! Creation is a wizard rather than a flag dump: name and summary, editable prompts, provider
//! credentials (same or fresh), and checkbox pickers for the notes and memories that should
//! follow the new profile. Sessions always start fresh. The data is assembled in a staging
//! directory and renamed into place, so a crashed create never appears in `profile list`.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use colored::Colorize;
use dialoguer::{Confirm, Input, MultiSelect};
use harness_core::config::{self, home_dir, Config};
use harness_core::profiles::{self, Profile, ProfilePaths, ProfileText, DEFAULT_PROFILE};
use harness_core::HarnessError;
use rusqlite::{Connection, OpenFlags};

/// The subcommand surface.
#[derive(clap::Subcommand, Debug)]
pub enum ProfileCommand {
    /// List the built-in profile and every custom profile on disk
    List,
    /// Create a profile from the built-in default, with a guided flow
    Create {
        /// Profile name; prompted when omitted
        name: Option<String>,
    },
    /// Show a profile's summary, persona, and behavior
    Show {
        /// Profile name (default: `default`)
        name: Option<String>,
        /// Print the assembled prompt sections too
        #[arg(long)]
        full: bool,
    },
    /// Delete a custom profile and its launcher
    Delete {
        /// Profile name
        name: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        force: bool,
    },
    /// Print where a profile keeps its data
    Path {
        /// Profile name (default: `default`)
        name: Option<String>,
    },
}

/// What the argument scan decided before clap runs.
pub struct Invocation {
    /// The selected profile, or `None` for the built-in default.
    pub profile: Option<String>,
    /// Arguments with the profile selector removed.
    pub args: Vec<OsString>,
}

impl Invocation {
    /// The profile name this run should use.
    pub fn name(&self) -> &str {
        self.profile.as_deref().unwrap_or(DEFAULT_PROFILE)
    }
}

/// Subcommands clap already owns. A first argument in this set is never a profile.
const SUBCOMMANDS: &[&str] = &[
    "auth",
    "chat",
    "config",
    "diff",
    "eval",
    "exec",
    "fetch",
    "gateway",
    "hb",
    "heartbeat",
    "help",
    "hooks",
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

/// Scan the process arguments for a profile selector before clap sees them.
///
/// Precedence: `--profile` (one run) → the binary name (a launcher) → a leading profile argument
/// (`luna terra`) → `LUNA_PROFILE`. The selector is removed from the argument list so clap sees an
/// ordinary command line.
pub fn parse_invocation(
    mut args: Vec<OsString>,
    profiles_dir: &Path,
) -> Result<Invocation, HarnessError> {
    let program = if args.is_empty() {
        OsString::from("luna")
    } else {
        args.remove(0)
    };
    let mut profile = None;
    let mut filtered = Vec::with_capacity(args.len());
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        let text = arg.to_string_lossy();
        if text == "--profile" {
            let Some(value) = args.get(index + 1) else {
                return Err(HarnessError::ConfigurationError(
                    "--profile needs a name".to_string(),
                ));
            };
            profile = Some(value.to_string_lossy().to_string());
            index += 2;
            continue;
        }
        if let Some(value) = text.strip_prefix("--profile=") {
            profile = Some(value.to_string());
            index += 1;
            continue;
        }
        filtered.push(arg.clone());
        index += 1;
    }

    let invoked_as = Path::new(&program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("luna")
        .trim_end_matches(".exe")
        .to_string();
    if profile.is_none() && invoked_as != "luna" && is_profile_name(&invoked_as, profiles_dir) {
        profile = Some(invoked_as);
    }

    if profile.is_none() {
        if let Some(first) = filtered.first() {
            let text = first.to_string_lossy();
            if !text.starts_with('-')
                && !SUBCOMMANDS.contains(&text.as_ref())
                && is_profile_name(&text, profiles_dir)
            {
                profile = Some(text.to_string());
                filtered.remove(0);
            }
        }
    }

    if profile.is_none() {
        if let Ok(name) = std::env::var("LUNA_PROFILE") {
            let name = name.trim();
            if !name.is_empty() {
                profile = Some(name.to_string());
            }
        }
    }

    Ok(Invocation {
        profile,
        args: std::iter::once(program).chain(filtered).collect(),
    })
}

fn is_profile_name(name: &str, profiles_dir: &Path) -> bool {
    if name == DEFAULT_PROFILE {
        return true;
    }
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return false;
    }
    profiles_dir
        .join(name)
        .join(profiles::PROFILE_FILE)
        .is_file()
}

/// Load the selected profile and apply it to an already-resolved config.
pub fn activate(config: &mut Config, name: &str) -> Result<Profile, HarnessError> {
    let profile = profiles::load(name, &config.paths.profiles_dir)?;
    profiles::apply_to_config(config, &profile)?;
    Ok(profile)
}

/// The global config, without any profile overlay. Used as the copy source when a profile is
/// created, so "from default" means the default even when the wizard was opened from a custom
/// profile.
pub fn global_config() -> Result<Config, HarnessError> {
    let layers = config::process_layers(crate::commands::config::catalog_defaults())?;
    Ok(config::resolve(&layers)?.into_config())
}

/// Run `luna profile ...`.
pub fn handle_profile_command(
    command: ProfileCommand,
    config: &Config,
    global: &Config,
) -> Result<(), HarnessError> {
    let profiles_dir = config.paths.profiles_dir.clone();
    match command {
        ProfileCommand::List => {
            println!("{}", list_text(&profiles_dir));
            Ok(())
        }
        ProfileCommand::Create { name } => {
            let dir = create_profile_interactive(name, global)?;
            println!("{} Created {}", "✔".green().bold(), dir.display());
            Ok(())
        }
        ProfileCommand::Show { name, full } => {
            println!(
                "{}",
                show_text(
                    name.as_deref().unwrap_or(DEFAULT_PROFILE),
                    &profiles_dir,
                    full
                )?
            );
            Ok(())
        }
        ProfileCommand::Delete { name, force } => delete(&name, &profiles_dir, force),
        ProfileCommand::Path { name } => {
            println!(
                "{}",
                path_text(name.as_deref().unwrap_or(DEFAULT_PROFILE), config)?
            );
            Ok(())
        }
    }
}

/// The `profile list` body, as text the caller prints or hands to the chat UI.
pub(crate) fn list_text(profiles_dir: &Path) -> String {
    let catalog = profiles::catalog(profiles_dir);
    if catalog.is_empty() {
        return "No profiles found.".yellow().to_string();
    }
    let mut out = String::new();
    for entry in catalog {
        let marker = if entry.builtin {
            format!(" {}", "◂ default".cyan())
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  {}{}  {}\n",
            entry.name.bold(),
            marker,
            entry.summary.dimmed()
        ));
    }
    out.trim_end_matches('\n').to_string()
}

/// The `profile show` body, as text the caller prints or hands to the chat UI.
pub(crate) fn show_text(
    name: &str,
    profiles_dir: &Path,
    full: bool,
) -> Result<String, HarnessError> {
    let profile = profiles::load(name, profiles_dir)?;
    let mut out = String::new();
    out.push_str(&format!("{}\n", profile.name.bold()));
    if !profile.summary.is_empty() {
        out.push_str(&format!("{}\n", profile.summary.dimmed()));
    }
    if let Some(dir) = &profile.dir {
        out.push_str(&format!("{} {}\n", "path:".dimmed(), dir.display()));
    }
    out.push('\n');
    out.push_str(&format!("{}\n", "Persona".cyan().bold()));
    out.push_str(&format!("{}\n", profile.persona));
    out.push('\n');
    out.push_str(&format!("{}\n", "Behavior".cyan().bold()));
    out.push_str(&format!("{}\n", profile.behavior));
    if full {
        out.push('\n');
        out.push_str(&format!(
            "{}\n",
            "Static contract (not editable through a profile)".dimmed()
        ));
        out.push_str(&format!("{}\n", static_contract_preview()));
    }
    Ok(out.trim_end_matches('\n').to_string())
}

/// The locked part of the system prompt, for `profile show --full`.
pub fn static_contract_preview() -> String {
    harness_core::prompts::section(harness_core::prompts::SYSTEM_PROMPT)
        .unwrap_or_else(|error| format!("(could not load prompts.md: {error})"))
}

/// The `profile path` body, as text the caller prints or hands to the chat UI.
pub(crate) fn path_text(name: &str, config: &Config) -> Result<String, HarnessError> {
    if name == DEFAULT_PROFILE {
        Ok(home_dir().join(".luna").display().to_string())
    } else {
        let dir = profiles::profile_dir(&config.paths.profiles_dir, name);
        if !dir.join(profiles::PROFILE_FILE).is_file() {
            return Err(HarnessError::ConfigurationError(format!(
                "profile '{}' not found",
                name
            )));
        }
        Ok(dir.display().to_string())
    }
}

fn delete(name: &str, profiles_dir: &Path, force: bool) -> Result<(), HarnessError> {
    if !force {
        if !std::io::stdin().is_terminal() {
            return Err(HarnessError::ConfigurationError(
                "refusing to delete a profile without --force on a non-interactive terminal"
                    .to_string(),
            ));
        }
        let confirmed = Confirm::new()
            .with_prompt(format!("Delete profile '{}' and all of its data?", name))
            .default(false)
            .interact()
            .map_err(|error| HarnessError::Io(error.to_string()))?;
        if !confirmed {
            println!("{}", "Cancelled.".dimmed());
            return Ok(());
        }
    }
    delete_profile(name, profiles_dir)?;
    println!("{} Deleted {}", "✔".green().bold(), name);
    Ok(())
}

/// Remove a custom profile and its launcher. The caller owns any confirmation prompt.
pub(crate) fn delete_profile(name: &str, profiles_dir: &Path) -> Result<(), HarnessError> {
    profiles::validate_name(name)?;
    let dir = profiles::profile_dir(profiles_dir, name);
    if !dir.join(profiles::PROFILE_FILE).is_file() {
        return Err(HarnessError::ConfigurationError(format!(
            "profile '{}' not found",
            name
        )));
    }
    std::fs::remove_dir_all(&dir).map_err(|error| {
        HarnessError::Io(format!("failed to remove {}: {}", dir.display(), error))
    })?;
    remove_launcher(name)?;
    Ok(())
}

/// The guided creation flow. Returns the new profile directory.
pub fn create_profile_interactive(
    name: Option<String>,
    source: &Config,
) -> Result<PathBuf, HarnessError> {
    let profiles_dir = source.paths.profiles_dir.clone();
    std::fs::create_dir_all(&profiles_dir).map_err(|error| {
        HarnessError::Io(format!(
            "failed to create {}: {}",
            profiles_dir.display(),
            error
        ))
    })?;

    let name = match name {
        Some(name) => name,
        None => prompt_line("Profile name (lowercase, digits, '-'): ", "")?,
    };
    profiles::validate_name(&name)?;
    let dest = profiles::profile_dir(&profiles_dir, &name);
    if dest.exists() {
        return Err(HarnessError::ConfigurationError(format!(
            "profile '{}' already exists at {}",
            name,
            dest.display()
        )));
    }

    let summary = prompt_line("One-line summary: ", "Custom profile")?;
    let seed = profiles::default_profile()?;
    println!();
    println!("{}", "Current prompts (seeded from default)".cyan().bold());
    println!("{}", "Persona".dimmed());
    println!("{}", seed.persona);
    println!();
    println!("{}", "Behavior".dimmed());
    println!("{}", seed.behavior);
    println!();

    let mut text = ProfileText {
        summary: Some(summary.clone()),
        persona: Some(seed.persona.clone()),
        behavior: Some(seed.behavior.clone()),
    };
    if std::io::stdin().is_terminal() {
        text = edit_prompts(&name, &text).unwrap_or(text);
    }

    let persona = text.persona.unwrap_or(seed.persona.clone());
    let behavior = text.behavior.unwrap_or(seed.behavior.clone());
    let summary = text.summary.unwrap_or(summary);

    let copy_providers = if std::io::stdin().is_terminal() {
        Confirm::new()
            .with_prompt("Use the same provider credentials as the default profile?")
            .default(true)
            .interact()
            .unwrap_or(true)
    } else {
        false
    };

    let notes = pick_notes(source)?;
    let memories = pick_memories(source)?;

    println!();
    println!("{}", "Review".cyan().bold());
    println!("  name:      {}", name.bold());
    println!("  summary:   {}", summary);
    println!(
        "  providers: {}",
        if copy_providers { "same" } else { "fresh" }
    );
    println!("  notes:     {}", count_or_fresh(notes.len()));
    println!("  memories:  {}", count_or_fresh(memories.len()));

    if std::io::stdin().is_terminal() {
        let proceed = Confirm::new()
            .with_prompt("Create this profile?")
            .default(true)
            .interact()
            .map_err(|error| HarnessError::Io(error.to_string()))?;
        if !proceed {
            println!("{}", "Cancelled.".dimmed());
            return Ok(dest);
        }
    }

    create_profile_with(
        &profiles_dir,
        source,
        &name,
        summary,
        persona,
        behavior,
        copy_providers,
        &notes,
        &memories,
    )
}

/// Assemble a profile from already-collected answers and move it into place.
///
/// The interactive (dialoguer) and in-chat (modal) wizards both gather their answers differently
/// but must produce the same profile, so the filesystem work lives here only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_profile_with(
    profiles_dir: &Path,
    source: &Config,
    name: &str,
    summary: String,
    persona: String,
    behavior: String,
    copy_providers: bool,
    notes: &[PathBuf],
    memories: &[String],
) -> Result<PathBuf, HarnessError> {
    profiles::validate_name(name)?;
    let dest = profiles::profile_dir(profiles_dir, name);
    if dest.exists() {
        return Err(HarnessError::ConfigurationError(format!(
            "profile '{}' already exists at {}",
            name,
            dest.display()
        )));
    }

    let staging = profiles_dir.join(format!(".staging-{}-{}", name, std::process::id()));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).ok();
    }
    let paths = ProfilePaths {
        root: staging.clone(),
        memory_db: staging.join("memory.db"),
        notes_db: staging.join("notes.db"),
        notes_dir: staging.join("notes"),
        sessions_db: staging.join("sessions.db"),
        credentials: staging.join("credentials.json"),
        usage_db: staging.join("usage.db"),
        checkpoints_dir: staging.join("checkpoints"),
        todos_dir: staging.join("todos"),
        jobs_dir: staging.join("jobs"),
        skills_dir: staging.join("skills"),
    };
    paths.ensure_dirs()?;

    let profile = Profile {
        name: name.to_string(),
        summary,
        persona,
        behavior,
        builtin: false,
        dir: Some(staging.clone()),
    };
    std::fs::write(
        staging.join(profiles::PROFILE_FILE),
        profiles::render(&profile),
    )
    .map_err(|error| HarnessError::Io(format!("failed to write profile.md: {}", error)))?;
    std::fs::write(staging.join("config.yaml"), "{}\n")
        .map_err(|error| HarnessError::Io(format!("failed to write config.yaml: {}", error)))?;

    if copy_providers {
        copy_credentials(&source.paths.credentials, &paths.credentials)?;
    }
    // Create the destination schema now so the profile opens without a first-write delay.
    // Sessions are never carried into a new profile.
    harness_core::session::SqliteSessionStore::new(Some(paths.sessions_db.clone()))?;
    copy_notes(notes, &paths.notes_dir)?;
    copy_memories(&source.paths.memory_db, &paths.memory_db, memories)?;

    if dest.exists() {
        return Err(HarnessError::ConfigurationError(format!(
            "profile '{}' appeared while creating; refusing to overwrite",
            name
        )));
    }
    std::fs::rename(&staging, &dest).map_err(|error| {
        HarnessError::Io(format!(
            "failed to move {} into place: {}",
            staging.display(),
            error
        ))
    })?;

    install_launcher(name)?;
    Ok(dest)
}

fn prompt_line(message: &str, default: &str) -> Result<String, HarnessError> {
    if !std::io::stdin().is_terminal() {
        return Ok(default.to_string());
    }
    let input: String = Input::new()
        .with_prompt(message)
        .allow_empty(true)
        .interact_text()
        .map_err(|error| HarnessError::Io(error.to_string()))?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

/// Open the three editable sections in `$EDITOR` and read them back. A missing editor or a failed
/// spawn leaves the seed text untouched rather than aborting creation.
pub(crate) fn edit_prompts(name: &str, text: &ProfileText) -> Result<ProfileText, HarnessError> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let file =
        std::env::temp_dir().join(format!("luna-profile-{}-{}.md", name, std::process::id()));
    let draft = format!(
        "# Summary\n{}\n\n# Persona\n{}\n\n# Behavior\n{}\n",
        text.summary.as_deref().unwrap_or(""),
        text.persona.as_deref().unwrap_or(""),
        text.behavior.as_deref().unwrap_or(""),
    );
    std::fs::write(&file, draft)
        .map_err(|error| HarnessError::Io(format!("failed to write draft: {}", error)))?;
    let status = Command::new(&editor).arg(&file).status();
    let parsed = match status {
        Ok(status) if status.success() => std::fs::read_to_string(&file)
            .map_err(|error| HarnessError::Io(format!("failed to read draft: {}", error)))
            .and_then(|raw| ProfileText::parse(&raw)),
        _ => Ok(text.clone()),
    };
    let _ = std::fs::remove_file(&file);
    parsed
}

struct NoteChoice {
    path: PathBuf,
    label: String,
}

/// The notes a new profile can inherit, as `(label, path)` pairs. Shared with the chat wizard.
pub(crate) fn notes_for_copy(source: &Config) -> Vec<(String, PathBuf)> {
    list_notes(&source.paths.notes_dir)
        .into_iter()
        .map(|choice| (choice.label, choice.path))
        .collect()
}

/// The memories a new profile can inherit, as `(label, id)` pairs. Shared with the chat wizard.
pub(crate) fn memories_for_copy(source: &Config) -> Vec<(String, String)> {
    list_memories(&source.paths.memory_db)
        .into_iter()
        .map(|choice| (choice.label, choice.id))
        .collect()
}

/// How the review line describes a copied set: a count, or `fresh` when the user chose to start
/// with nothing. Shared with the chat wizard.
pub(crate) fn count_or_fresh(count: usize) -> String {
    if count == 0 {
        "fresh".to_string()
    } else {
        count.to_string()
    }
}

fn pick_notes(source: &Config) -> Result<Vec<PathBuf>, HarnessError> {
    if !std::io::stdin().is_terminal() {
        return Ok(Vec::new());
    }
    let choices = list_notes(&source.paths.notes_dir);
    if choices.is_empty() {
        println!("{}", "No notes to copy; starting fresh.".dimmed());
        return Ok(Vec::new());
    }
    // The copy is opt-in: a new profile starts with an empty notebook unless the user says which
    // notes to bring along.
    let copy = Confirm::new()
        .with_prompt("Copy notes into the new profile? (No = start fresh)")
        .default(false)
        .interact()
        .map_err(|error| HarnessError::Io(error.to_string()))?;
    if !copy {
        return Ok(Vec::new());
    }
    let labels: Vec<&str> = choices.iter().map(|choice| choice.label.as_str()).collect();
    let picked = MultiSelect::new()
        .with_prompt("Notes to copy (space toggles, ↑/↓ moves, Enter saves)")
        .items(&labels)
        .interact()
        .map_err(|error| HarnessError::Io(error.to_string()))?;
    Ok(picked
        .into_iter()
        .map(|index| choices[index].path.clone())
        .collect())
}

fn list_notes(dir: &Path) -> Vec<NoteChoice> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut notes: Vec<NoteChoice> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                return None;
            }
            let title = note_title(&path);
            Some(NoteChoice { label: title, path })
        })
        .collect();
    notes.sort_by(|a, b| a.label.cmp(&b.label));
    notes
}

fn note_title(path: &Path) -> String {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    for line in raw.lines().take(40) {
        if let Some(value) = line.strip_prefix("title:") {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("untitled")
        .to_string()
}

struct MemoryChoice {
    id: String,
    label: String,
}

fn pick_memories(source: &Config) -> Result<Vec<String>, HarnessError> {
    if !std::io::stdin().is_terminal() {
        return Ok(Vec::new());
    }
    let choices = list_memories(&source.paths.memory_db);
    if choices.is_empty() {
        println!("{}", "No memories to copy; starting fresh.".dimmed());
        return Ok(Vec::new());
    }
    let copy = Confirm::new()
        .with_prompt("Copy memories into the new profile? (No = start fresh)")
        .default(false)
        .interact()
        .map_err(|error| HarnessError::Io(error.to_string()))?;
    if !copy {
        return Ok(Vec::new());
    }
    let labels: Vec<&str> = choices.iter().map(|choice| choice.label.as_str()).collect();
    let picked = MultiSelect::new()
        .with_prompt("Memories to copy (space toggles, ↑/↓ moves, Enter saves)")
        .items(&labels)
        .interact()
        .map_err(|error| HarnessError::Io(error.to_string()))?;
    Ok(picked
        .into_iter()
        .map(|index| choices[index].id.clone())
        .collect())
}

fn list_memories(db: &Path) -> Vec<MemoryChoice> {
    if !db.exists() {
        return Vec::new();
    }
    let conn = match Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(error) => {
            tracing::warn!("could not read memories from {}: {}", db.display(), error);
            return Vec::new();
        }
    };
    let mut statement = match conn.prepare(
        "SELECT id, subject, predicate, object FROM memories WHERE is_active = 1
         ORDER BY updated_at DESC",
    ) {
        Ok(statement) => statement,
        Err(error) => {
            tracing::warn!("could not list memories: {}", error);
            return Vec::new();
        }
    };
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.flatten()
        .map(|(id, subject, predicate, object)| MemoryChoice {
            label: format!("{} · {} {}", subject, predicate, object),
            id,
        })
        .collect()
}

fn copy_credentials(source: &Path, dest: &Path) -> Result<(), HarnessError> {
    if !source.exists() {
        return Ok(());
    }
    std::fs::copy(source, dest).map_err(|error| {
        HarnessError::Io(format!(
            "failed to copy credentials from {}: {}",
            source.display(),
            error
        ))
    })?;
    set_private(dest);
    Ok(())
}

#[cfg(unix)]
fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_private(_path: &Path) {}

fn copy_notes(selected: &[PathBuf], dest_dir: &Path) -> Result<(), HarnessError> {
    std::fs::create_dir_all(dest_dir).map_err(|error| {
        HarnessError::Io(format!(
            "failed to create {}: {}",
            dest_dir.display(),
            error
        ))
    })?;
    for source in selected {
        let Some(name) = source.file_name() else {
            continue;
        };
        let dest = dest_dir.join(name);
        std::fs::copy(source, &dest).map_err(|error| {
            HarnessError::Io(format!(
                "failed to copy note {}: {}",
                source.display(),
                error
            ))
        })?;
    }
    Ok(())
}

fn copy_memories(
    source_db: &Path,
    dest_db: &Path,
    selected: &[String],
) -> Result<(), HarnessError> {
    // Creates the schema when it does not exist yet.
    let _ = harness_memory::SqliteMemoryEngine::new(Some(dest_db.to_path_buf()))?;
    if selected.is_empty() || !source_db.exists() {
        return Ok(());
    }

    let source = Connection::open_with_flags(source_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| HarnessError::MemoryError(format!("open memories: {}", error)))?;
    let dest = Connection::open(dest_db)
        .map_err(|error| HarnessError::MemoryError(format!("open new memories: {}", error)))?;

    let mut wanted: HashSet<String> = selected.iter().cloned().collect();
    // Include rows referenced by `superseded_by` so a copied chain does not dangle.
    let mut pending: Vec<String> = wanted.iter().cloned().collect();
    while let Some(id) = pending.pop() {
        let superseded: Option<String> = source
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = ?1",
                rusqlite::params![&id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        if let Some(next) = superseded {
            if wanted.insert(next.clone()) {
                pending.push(next);
            }
        }
    }

    for id in &wanted {
        let row = source
            .query_row(
                "SELECT id, subject, predicate, object, raw_content, category, confidence,
                        created_at, updated_at, is_active, superseded_by, name, path
                 FROM memories WHERE id = ?1",
                rusqlite::params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, f64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                    ))
                },
            )
            .ok();
        let Some(row) = row else { continue };
        let superseded = row.10.filter(|next| wanted.contains(next));
        dest.execute(
            "INSERT OR REPLACE INTO memories
             (id, subject, predicate, object, raw_content, category, confidence,
              created_at, updated_at, is_active, superseded_by, name, path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, superseded,
                row.11, row.12,
            ],
        )
        .map_err(|error| HarnessError::MemoryError(format!("copy memory: {}", error)))?;

        let embedding: Option<Vec<u8>> = source
            .query_row(
                "SELECT embedding FROM memory_embeddings WHERE memory_id = ?1",
                rusqlite::params![&row.0],
                |row| row.get(0),
            )
            .ok();
        if let Some(embedding) = embedding {
            dest.execute(
                "INSERT OR REPLACE INTO memory_embeddings (memory_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![&row.0, embedding],
            )
            .map_err(|error| HarnessError::MemoryError(format!("copy embedding: {}", error)))?;
        }
    }
    Ok(())
}

/// Install `~/.local/bin/<name>` as a symlink to the running `luna` binary.
pub fn install_launcher(name: &str) -> Result<PathBuf, HarnessError> {
    profiles::validate_name(name)?;
    let bin_dir = home_dir().join(".local").join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|error| {
        HarnessError::Io(format!("failed to create {}: {}", bin_dir.display(), error))
    })?;
    let link = bin_dir.join(name);
    let target = std::env::current_exe()
        .map_err(|error| HarnessError::Io(format!("cannot find the luna binary: {}", error)))?;
    let target = std::fs::canonicalize(&target).unwrap_or(target);

    if let Ok(metadata) = std::fs::symlink_metadata(&link) {
        let existing = std::fs::read_link(&link).ok();
        if existing.as_deref() == Some(target.as_path())
            || std::fs::canonicalize(&link).ok().as_deref() == Some(target.as_path())
        {
            warn_if_not_on_path(&bin_dir);
            return Ok(link);
        }
        if metadata.file_type().is_symlink() {
            // A stale launcher from a previous install: replacing it is safe.
            std::fs::remove_file(&link).map_err(|error| {
                HarnessError::Io(format!("failed to replace {}: {}", link.display(), error))
            })?;
        } else {
            return Err(HarnessError::ConfigurationError(format!(
                "{} already exists and is not a luna launcher",
                link.display()
            )));
        }
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).map_err(|error| {
        HarnessError::Io(format!(
            "failed to create launcher {}: {}",
            link.display(),
            error
        ))
    })?;
    #[cfg(not(unix))]
    {
        std::fs::write(
            &link,
            format!("@echo off\r\n\"{}\" %*\r\n", target.display()),
        )
        .map_err(|error| HarnessError::Io(format!("failed to create launcher: {}", error)))?;
    }

    warn_if_not_on_path(&bin_dir);
    Ok(link)
}

fn remove_launcher(name: &str) -> Result<(), HarnessError> {
    if !(name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-'))
    {
        return Ok(());
    }
    let link = home_dir().join(".local").join("bin").join(name);
    let Ok(metadata) = std::fs::symlink_metadata(&link) else {
        return Ok(());
    };
    // Only remove a launcher that still points at a luna binary. A file the user replaced with
    // something of their own is left alone.
    let is_luna = std::fs::read_link(&link)
        .ok()
        .and_then(|target| {
            target
                .file_name()
                .map(|name| name.to_string_lossy().eq_ignore_ascii_case("luna"))
        })
        .unwrap_or(false);
    if metadata.file_type().is_symlink() && is_luna {
        std::fs::remove_file(&link).map_err(|error| {
            HarnessError::Io(format!("failed to remove {}: {}", link.display(), error))
        })?;
    }
    Ok(())
}

fn warn_if_not_on_path(bin_dir: &Path) {
    let on_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|entry| entry == bin_dir))
        .unwrap_or(false);
    if !on_path {
        println!(
            "{} {} is not on PATH. Add it with:\n  export PATH=\"{}:$PATH\"",
            "!".yellow().bold(),
            bin_dir.display(),
            bin_dir.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_leading_name_selects_a_profile() {
        let tmp = dir();
        std::fs::create_dir_all(tmp.path().join("terra")).unwrap();
        std::fs::write(
            tmp.path().join("terra/profile.md"),
            "# Persona\nRole: Terra\n",
        )
        .unwrap();

        let invocation =
            parse_invocation(args(&["/usr/bin/luna", "terra", "exec", "hi"]), tmp.path())
                .expect("parses");
        assert_eq!(invocation.name(), "terra");
        assert_eq!(invocation.args, args(&["/usr/bin/luna", "exec", "hi"]));
    }

    #[test]
    fn the_launcher_name_selects_a_profile() {
        let tmp = dir();
        std::fs::create_dir_all(tmp.path().join("terra")).unwrap();
        std::fs::write(
            tmp.path().join("terra/profile.md"),
            "# Persona\nRole: Terra\n",
        )
        .unwrap();

        let invocation =
            parse_invocation(args(&["/usr/local/bin/terra", "exec"]), tmp.path()).expect("parses");
        assert_eq!(invocation.name(), "terra");
        assert_eq!(invocation.args, args(&["/usr/local/bin/terra", "exec"]));
    }

    #[test]
    fn the_profile_flag_is_removed_and_wins() {
        let tmp = dir();
        let invocation = parse_invocation(
            args(&["luna", "--profile", "terra", "exec", "hi"]),
            tmp.path(),
        )
        .expect("parses");
        assert_eq!(invocation.name(), "terra");
        assert_eq!(invocation.args, args(&["luna", "exec", "hi"]));
    }

    #[test]
    fn subcommands_are_not_profiles() {
        let tmp = dir();
        let invocation =
            parse_invocation(args(&["luna", "exec", "hi"]), tmp.path()).expect("parses");
        assert_eq!(invocation.name(), DEFAULT_PROFILE);
        assert_eq!(invocation.args, args(&["luna", "exec", "hi"]));
    }

    #[test]
    fn default_is_a_valid_leading_name() {
        let tmp = dir();
        let invocation = parse_invocation(args(&["luna", "default"]), tmp.path()).expect("parses");
        assert_eq!(invocation.name(), DEFAULT_PROFILE);
        assert_eq!(invocation.args, args(&["luna"]));
    }
}
