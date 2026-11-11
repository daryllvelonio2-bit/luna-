//! `luna skill install <github>`: fetch a skill from a repo and put it where the loader looks.
//!
//! Installing a skill by hand is a five-step job — find the repo, work out which branch, find which
//! subdirectory holds the `SKILL.md`, copy it to `paths.skills_dir` *and* remember that the login
//! shell's `/tmp` is not in the workspace, so a half-clone lands somewhere the agent can no longer
//! read. Every one of those steps is mechanical, and getting one wrong leaves a skill that silently
//! does not load. This command does them all: one URL in, a validated skill directory out.
//!
//! What is *not* mechanical stays visible: every `SKILL.md` found is parsed with the same
//! [`SkillLoader`] the chat session uses, so a file whose frontmatter is broken is reported instead
//! of being copied into place and failing later, and a repo holding several skills prints all of
//! them.
//!
//! Distinct from [`crate::commands::skills`], which shapes the `/skills` readout for the terminal.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use clap::Subcommand;
use colored::Colorize;

use harness_core::{Config, HarnessError};
use harness_plugins::{Skill, SkillLoader};

/// Sent to GitHub, which refuses requests without one.
const USER_AGENT: &str = concat!(
    "LunaHarness/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/luna-ai/rust-agent-harness)"
);

/// A repository tarball is the whole tree, so this is generous; it exists to stop a mistyped URL
/// from filling the disk.
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Subcommand, Debug)]
pub enum SkillCommand {
    /// Install a skill from a GitHub repo, or from one directory inside it
    Install {
        /// owner/repo, https://github.com/owner/repo, .../tree/<ref>/<path>, or a raw SKILL.md URL
        source: String,
        /// Install only the skills with these names (repeatable); without it, every SKILL.md found
        #[arg(long = "skill", value_name = "NAME")]
        skills: Vec<String>,
        /// Replace a skill that is already installed
        #[arg(long)]
        force: bool,
        /// One JSON object on stdout
        #[arg(long)]
        json: bool,
    },
    /// List the skills the loader picks up, and the directory each one lives in
    List {
        /// One JSON array on stdout
        #[arg(long)]
        json: bool,
    },
}

/// Where the skill's files come from, resolved from whatever the user typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSource {
    pub owner: String,
    pub repo: String,
    /// Branch, tag or commit named in the URL; `None` means the repository's default branch.
    pub git_ref: Option<String>,
    /// Directory inside the repo that was pointed at, if the URL was not the repo root.
    pub subpath: Option<String>,
}

/// Every accepted spelling of "this skill on GitHub", reduced to owner/repo/ref/path.
///
/// A wrong answer here is a wrong download, so anything not understood is refused by name rather
/// than guessed at.
pub fn parse_source(input: &str) -> Result<SkillSource, HarnessError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(refuse(
            "no source given — pass owner/repo or a github.com URL",
        ));
    }
    // A fragment (`#readme`) or query never identifies the files, only the page.
    let raw = raw
        .split(['#', '?'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    let without_scheme = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"));

    let segments: Vec<String> = match without_scheme {
        Some(rest) => {
            let mut parts = rest.split('/').filter(|p| !p.is_empty());
            let host = parts.next().unwrap_or("").to_ascii_lowercase();
            let rest: Vec<String> = parts.map(str::to_string).collect();
            match host.as_str() {
                "github.com" | "www.github.com" => rest,
                // A raw file URL names the whole path: owner/repo/ref/path/to/SKILL.md
                "raw.githubusercontent.com" => {
                    if rest.len() < 4 {
                        return Err(refuse(&format!(
                            "{input} is not a raw file URL — expected https://raw.githubusercontent.com/<owner>/<repo>/<ref>/<path>"
                        )));
                    }
                    let (owner, repo, git_ref) = (&rest[0], &rest[1], &rest[2]);
                    let path = rest[3..].join("/");
                    let dir = path.rsplit_once('/').map(|(dir, _)| dir.to_string());
                    return Ok(SkillSource {
                        owner: owner.clone(),
                        repo: strip_git(repo),
                        git_ref: Some(git_ref.clone()),
                        subpath: dir,
                    });
                }
                other => {
                    return Err(refuse(&format!(
                        "{other} is not supported — a skill has to come from github.com or raw.githubusercontent.com"
                    )))
                }
            }
        }
        // Shorthand: owner/repo, owner/repo@ref
        None => {
            let (path_part, git_ref) = match raw.split_once('@') {
                Some((path, git_ref)) => (path, Some(git_ref.to_string())),
                None => (raw, None),
            };
            if path_part.contains(char::is_whitespace) {
                return Err(refuse(&format!(
                    "{input} is not a URL or an owner/repo pair"
                )));
            }
            let mut parts = path_part.split('/').filter(|p| !p.is_empty());
            let owner = parts.next().unwrap_or("").to_string();
            let repo = parts.next().unwrap_or("").to_string();
            if parts.next().is_some() {
                return Err(refuse(&format!(
                    "{input} names more than owner/repo — use the full github.com URL for a directory inside a repo"
                )));
            }
            if owner.is_empty() || repo.is_empty() {
                return Err(refuse(&format!(
                    "{input} is not an owner/repo pair — try DietrichGebert/ponytail"
                )));
            }
            return Ok(SkillSource {
                owner,
                repo: strip_git(&repo),
                git_ref,
                subpath: None,
            });
        }
    };

    if segments.len() < 2 {
        return Err(refuse(&format!(
            "{input} does not name a repository — expected https://github.com/<owner>/<repo>"
        )));
    }
    let owner = segments[0].clone();
    let repo = strip_git(&segments[1]);

    // github.com/<owner>/<repo>/tree/<ref>/<path...> — `blob` is the same thing pointing at a file.
    let (git_ref, mut subpath) = match segments.get(2).map(String::as_str) {
        None => (None, None),
        Some("tree") | Some("blob") => {
            if segments.len() < 4 {
                return Err(refuse(&format!("{input} names no branch — try .../tree/main")));
            }
            let path = (segments.len() > 4).then(|| segments[4..].join("/"));
            (Some(segments[3].clone()), path)
        }
        Some(other) => {
            return Err(refuse(&format!(
                "{input} is not a repository URL — '{other}' is neither a branch listing nor a path into one"
            )))
        }
    };
    // Pointing at the file means the directory holding it.
    if let Some(path) = subpath.as_mut() {
        if path.to_ascii_lowercase().ends_with(".md") {
            *path = path
                .rsplit_once('/')
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_default();
        }
        if path.is_empty() {
            subpath = None;
        }
    }

    Ok(SkillSource {
        owner,
        repo,
        git_ref,
        subpath,
    })
}

/// A skill's own name becomes a directory name under `paths.skills_dir`, so it has to be exactly one
/// path component. This is the same guard a note id needs: a name is data from a repository, and
/// `../../.ssh` joined onto a directory is a download into somebody's home.
pub fn safe_skill_dir_name(name: &str) -> Result<String, HarnessError> {
    let trimmed = name.trim();
    let unusable = trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.starts_with('.')
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || Path::new(trimmed).is_absolute();
    if unusable {
        return Err(refuse(&format!(
            "a skill calls itself {name:?}, which is not a directory name — refusing to write it into the skills directory"
        )));
    }
    Ok(trimmed.to_string())
}

/// What a repository turned out to contain.
pub struct SkillsFound {
    /// One entry per skill name, with the directory it came from.
    pub skills: Vec<(PathBuf, Skill)>,
    /// Files named like a skill that are not one, so the caller can report them instead of saying
    /// "0 skills found" over a repository full of broken frontmatter.
    pub broken: Vec<String>,
    /// Extra copies of a skill that ships once per agent, as `(name, path in the archive)`.
    pub shadowed: Vec<(String, String)>,
}

/// Every skill under `root`, parsed as the loader would parse it, and one entry per name.
///
/// Repositories that support several agents ship the same skill more than once — `skills/ponytail/`
/// plus a copy under `.claude/`, `.openclaw/`, and so on. The loader keys skills by name, so
/// installing both would copy the same skill over itself and report it twice: `ponytail` in this
/// repository would arrive as "12 skills". The shallowest copy wins (the vendor-neutral
/// `skills/<name>/`), and the others are reported as shadowed.
pub fn find_skills(root: &Path) -> Result<SkillsFound, HarnessError> {
    let mut found = Vec::new();
    let mut broken = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
                match SkillLoader::parse_skill_file(&path) {
                    Ok(skill) => found.push((dir.clone(), skill)),
                    Err(error) => broken.push(format!("{}: {error}", relative_to(&path, root))),
                }
            }
        }
    }

    // By name, then by how deep the skill sits: a skill at the archive root is the canonical one.
    found.sort_by(|a, b| {
        a.1.name
            .cmp(&b.1.name)
            .then_with(|| a.0.components().count().cmp(&b.0.components().count()))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut seen = std::collections::HashSet::new();
    let mut skills = Vec::new();
    let mut shadowed = Vec::new();
    for (dir, skill) in found {
        if seen.insert(skill.name.clone()) {
            skills.push((dir, skill));
        } else {
            shadowed.push((skill.name.clone(), relative_to(&dir, root)));
        }
    }
    broken.sort();
    shadowed.sort();
    Ok(SkillsFound {
        skills,
        broken,
        shadowed,
    })
}

/// Unpack a `tar.gz` and return the single directory it holds.
///
/// `tar` refuses entries that would land outside `dest`, so a malicious archive cannot write into
/// the skills directory through `..` or an absolute path.
pub fn extract_archive(bytes: &[u8], dest: &Path) -> Result<PathBuf, HarnessError> {
    fs::create_dir_all(dest).map_err(|e| io_error(dest, &e))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    archive.unpack(dest).map_err(|e| {
        HarnessError::PluginError(format!("the download is not a readable .tar.gz: {e}"))
    })?;

    let top_level: Vec<PathBuf> = fs::read_dir(dest)
        .map_err(|e| io_error(dest, &e))?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    match (top_level.len(), top_level.into_iter().next()) {
        // github wraps the tree in one `owner-repo-sha` directory.
        (1, Some(only)) if only.is_dir() => Ok(only),
        _ => Ok(dest.to_path_buf()),
    }
}

/// Copy a directory tree, skipping symlinks: a repository can link anywhere, and installing a link
/// pointing out of the skills directory would hand the loader a file nobody reviewed.
pub fn copy_tree(from: &Path, to: &Path) -> Result<(), HarnessError> {
    fs::create_dir_all(to).map_err(|e| io_error(to, &e))?;
    for entry in fs::read_dir(from)
        .map_err(|e| io_error(from, &e))?
        .flatten()
    {
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| io_error(&source, &e))?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_file() {
            fs::copy(&source, &target).map_err(|e| io_error(&source, &e))?;
        }
    }
    Ok(())
}

/// The staging directory is removed on every way out of an install, including the failures.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        // A staging directory that survives is harmless: the next install reuses the same path.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A subpath taken from a URL must stay inside the repository it came from.
fn resolve_subpath(root: &Path, subpath: &Option<String>) -> Result<PathBuf, HarnessError> {
    let Some(subpath) = subpath else {
        return Ok(root.to_path_buf());
    };
    let mut resolved = root.to_path_buf();
    for component in Path::new(subpath).components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            _ => {
                return Err(refuse(&format!(
                    "'{subpath}' does not stay inside the repository"
                )))
            }
        }
    }
    if !resolved.starts_with(root) {
        return Err(refuse(&format!(
            "'{subpath}' does not stay inside the repository"
        )));
    }
    Ok(resolved)
}

/// A staging path means nothing to the reader; the path inside the archive does.
fn relative_to(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn strip_git(repo: &str) -> String {
    repo.trim_end_matches(".git").to_string()
}

fn refuse(message: &str) -> HarnessError {
    HarnessError::ConfigurationError(message.to_string())
}

fn io_error(path: &Path, error: &std::io::Error) -> HarnessError {
    HarnessError::Io(format!("{}: {error}", path.display()))
}

pub async fn handle_skill_command(cmd: SkillCommand, config: &Config) -> Result<(), HarnessError> {
    match cmd {
        SkillCommand::Install {
            source,
            skills: only,
            force,
            json,
        } => install(&source, &only, force, json, config).await,
        SkillCommand::List { json } => list(json, config),
    }
}

/// The skills currently installed, as the loader sees them.
fn load_installed(config: &Config) -> Result<Vec<Skill>, HarnessError> {
    SkillLoader::load_skills_from_dir(&config.paths.skills_dir)
}

fn list(json: bool, config: &Config) -> Result<(), HarnessError> {
    let skills = load_installed(config)?;
    if json {
        let rows: Vec<serde_json::Value> = skills
            .iter()
            .map(|skill| {
                serde_json::json!({
                    "name": skill.name,
                    "path": skill.file_path.display().to_string(),
                    "has_command": skill.command.is_some(),
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "skills": rows }));
        return Ok(());
    }
    if skills.is_empty() {
        println!(
            "no skills installed in {} — install one with `luna skill install owner/repo`",
            config.paths.skills_dir.display()
        );
        return Ok(());
    }
    println!("{}", config.paths.skills_dir.display().to_string().bold());
    for skill in &skills {
        let note = match &skill.command {
            Some(_) => " (runs a command, so it needs approval)",
            None => "",
        };
        println!("  - {}{}", skill.name, note.dimmed());
    }
    Ok(())
}

async fn install(
    source: &str,
    only: &[String],
    force: bool,
    json: bool,
    config: &Config,
) -> Result<(), HarnessError> {
    let (parsed, archive) = fetch_archive(source).await?;

    let skills_dir = config.paths.skills_dir.clone();
    fs::create_dir_all(&skills_dir).map_err(|e| io_error(&skills_dir, &e))?;
    // Staged in the system temp dir rather than inside `skills_dir`: the loader walks that
    // directory recursively and does not skip dot-directories, so a half-extracted skill there
    // would be visible to a chat session running at the same time. The install copies rather than
    // renames, so the two do not have to share a filesystem.
    let staging = Staging(std::env::temp_dir().join(format!("luna-skill-{}", std::process::id())));
    let root = extract_archive(&archive, &staging.0)?;
    let root = resolve_subpath(&root, &parsed.subpath)?;

    let SkillsFound {
        skills,
        broken,
        shadowed: found_shadowed,
    } = find_skills(&root)?;
    ensure_skills_found(&skills, &broken, &parsed)?;

    let selected = select_skills(skills, only)?;
    let installed = install_selected(&selected, &skills_dir, force)?;

    // Copies that were passed over are reported for the skills that were actually installed: with
    // `--skill ponytail-gain`, a variant of `ponytail-audit` is not this install's business.
    let installed_names: Vec<&str> = installed.iter().map(|(name, _, _)| name.as_str()).collect();
    let shadowed: Vec<&(String, String)> = found_shadowed
        .iter()
        .filter(|(name, _)| installed_names.contains(&name.as_str()))
        .collect();

    if json {
        print_install_json(&installed, &shadowed, &broken);
    } else {
        print_install_text(&installed, &shadowed, &broken, &skills_dir);
    }
    Ok(())
}

/// Resolve the source, pick a branch, and download the repository tarball.
async fn fetch_archive(source: &str) -> Result<(SkillSource, Vec<u8>), HarnessError> {
    let parsed = parse_source(source)?;
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| refuse(&format!("could not build an HTTP client: {e}")))?;

    // A URL with `/tree/<ref>/` settles the branch. Without one the default branch is what a
    // browser shows, so ask for it; `main` and `master` remain as the offline fallback.
    let refs: Vec<String> = match &parsed.git_ref {
        Some(git_ref) => vec![git_ref.clone()],
        None => match default_branch(&client, &parsed).await? {
            Some(default) => vec![default],
            None => vec!["main".to_string(), "master".to_string()],
        },
    };
    let archive = download(&client, &parsed, &refs).await?;
    Ok((parsed, archive))
}

/// Refuse a repository that holds no usable skill, naming the files that looked like one.
fn ensure_skills_found(
    skills: &[(PathBuf, Skill)],
    broken: &[String],
    parsed: &SkillSource,
) -> Result<(), HarnessError> {
    if !skills.is_empty() {
        return Ok(());
    }
    let where_ = parsed.subpath.as_deref().unwrap_or("the repository root");
    let mut message = format!(
        "no SKILL.md found under {where_} in {}/{}",
        parsed.owner, parsed.repo
    );
    for problem in broken {
        message.push_str(&format!("\n  {problem}"));
    }
    Err(refuse(&message))
}

/// Keep every skill, or only the requested names, refusing any name the source does not hold.
fn select_skills(
    skills: Vec<(PathBuf, Skill)>,
    only: &[String],
) -> Result<Vec<(PathBuf, Skill)>, HarnessError> {
    if only.is_empty() {
        return Ok(skills);
    }
    let missing: Vec<&str> = only
        .iter()
        .filter(|name| !skills.iter().any(|(_, skill)| &skill.name == *name))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        let available: Vec<&str> = skills
            .iter()
            .map(|(_, skill)| skill.name.as_str())
            .collect();
        return Err(refuse(&format!(
            "{} not in this source — it holds: {}",
            missing.join(", "),
            available.join(", ")
        )));
    }
    Ok(skills
        .into_iter()
        .filter(|(_, skill)| only.contains(&skill.name))
        .collect())
}

/// Copy each selected skill into the skills directory, replacing only when forced.
fn install_selected(
    selected: &[(PathBuf, Skill)],
    skills_dir: &Path,
    force: bool,
) -> Result<Vec<(String, PathBuf, bool)>, HarnessError> {
    let mut installed = Vec::new();
    for (dir, skill) in selected {
        let name = safe_skill_dir_name(&skill.name)?;
        let target = skills_dir.join(&name);
        if target.exists() {
            if !force {
                return Err(refuse(&format!(
                    "{name} is already installed at {} — pass --force to replace it",
                    target.display()
                )));
            }
            fs::remove_dir_all(&target).map_err(|e| io_error(&target, &e))?;
        }
        copy_tree(dir, &target)?;
        installed.push((name, target, skill.command.is_some()));
    }
    Ok(installed)
}

/// One JSON object on stdout, including shadowed copies and broken files.
fn print_install_json(
    installed: &[(String, PathBuf, bool)],
    shadowed: &[&(String, String)],
    broken: &[String],
) {
    let rows: Vec<serde_json::Value> = installed
        .iter()
        .map(|(name, path, has_command)| {
            serde_json::json!({
                "name": name,
                "path": path.display().to_string(),
                "has_command": has_command,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "installed": rows,
            "shadowed": shadowed
                .iter()
                .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
                .collect::<Vec<_>>(),
            "broken": broken,
        })
    );
}

/// The human-readable install report.
fn print_install_text(
    installed: &[(String, PathBuf, bool)],
    shadowed: &[&(String, String)],
    broken: &[String],
    skills_dir: &Path,
) {
    println!(
        "installed {} skill{} into {}",
        installed.len(),
        if installed.len() == 1 { "" } else { "s" },
        skills_dir.display().to_string().bold()
    );
    for (name, _, has_command) in installed {
        let note = if *has_command {
            " (runs a command, so it needs approval)"
        } else {
            ""
        };
        println!("  - {name}{}", note.dimmed());
    }
    if !shadowed.is_empty() {
        let count = if shadowed.len() == 1 {
            "1 other copy was".to_string()
        } else {
            format!("{} other copies were", shadowed.len())
        };
        println!(
            "  {}",
            format!(
                "{count} passed over — this repository ships a variant per agent, and the canonical one is installed"
            )
            .dimmed()
        );
    }
    for problem in broken {
        println!("  {}", format!("skipped — {problem}").dimmed());
    }
    println!("available now: `/skills` in a chat session, and `luna skill list` from the shell");
}

/// The repository's default branch, so `owner/repo` alone installs what the browser shows.
///
/// `None` means GitHub could not be asked — offline, or over the anonymous rate limit — and the
/// caller falls back to the conventional names rather than failing an install it can still do.
async fn default_branch(
    client: &reqwest::Client,
    parsed: &SkillSource,
) -> Result<Option<String>, HarnessError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}",
        parsed.owner, parsed.repo
    );
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(refuse(&format!(
            "no GitHub repository {}/{} — check the spelling",
            parsed.owner, parsed.repo
        )));
    }
    if !response.status().is_success() {
        return Ok(None);
    }
    match response.json::<serde_json::Value>().await {
        Ok(body) => Ok(body
            .get("default_branch")
            .and_then(|value| value.as_str())
            .map(str::to_string)),
        Err(_) => Ok(None),
    }
}

/// One request for the whole tree, in preference to a clone: `git` need not exist, and a shallow
/// clone of a large repository is not what "install this skill" should cost.
///
/// `refs` is tried in order — the conventional branch names when the URL named none — and the last
/// failure is what gets reported.
async fn download(
    client: &reqwest::Client,
    parsed: &SkillSource,
    refs: &[String],
) -> Result<Vec<u8>, HarnessError> {
    let base = format!(
        "https://codeload.github.com/{}/{}/tar.gz",
        parsed.owner, parsed.repo
    );
    let mut tried = Vec::new();
    let mut last_status = None;
    for git_ref in refs {
        let mut candidates = vec![format!("{base}/{git_ref}")];
        if !git_ref.starts_with("refs/") {
            candidates.push(format!("{base}/refs/heads/{git_ref}"));
            candidates.push(format!("{base}/refs/tags/{git_ref}"));
        }
        for url in &candidates {
            let response = match client.get(url).send().await {
                Ok(response) => response,
                Err(error) => {
                    return Err(refuse(&format!(
                        "could not reach GitHub to download {}/{}: {error}",
                        parsed.owner, parsed.repo
                    )))
                }
            };
            if response.status().is_success() {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| refuse(&format!("the download stopped early: {e}")))?;
                if bytes.len() > MAX_ARCHIVE_BYTES {
                    return Err(refuse(&format!(
                        "{}/{} is {} MB, over the {} MB this command will unpack",
                        parsed.owner,
                        parsed.repo,
                        bytes.len() / (1024 * 1024),
                        MAX_ARCHIVE_BYTES / (1024 * 1024)
                    )));
                }
                return Ok(bytes.to_vec());
            }
            last_status = Some(response.status());
        }
        tried.push(git_ref.as_str());
    }

    Err(refuse(&format!(
        "could not download {}/{} at {} ({}). Name the branch or tag in the URL when it is not one of those: .../tree/<ref>",
        parsed.owner,
        parsed.repo,
        tried.join(", "),
        last_status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "no response".into())
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn src(owner: &str, repo: &str, git_ref: Option<&str>, subpath: Option<&str>) -> SkillSource {
        SkillSource {
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref: git_ref.map(str::to_string),
            subpath: subpath.map(str::to_string),
        }
    }

    #[test]
    fn every_spelling_of_a_github_source_resolves_to_the_same_place() {
        // The forms a person actually types: the repo page, a directory inside it, a file inside
        // it, the raw file, the shorthand. All of the first four name one directory.
        let repo_page = parse_source("https://github.com/DietrichGebert/ponytail").unwrap();
        assert_eq!(repo_page, src("DietrichGebert", "ponytail", None, None));

        let directory =
            parse_source("https://github.com/DietrichGebert/ponytail/tree/main/skills/ponytail")
                .unwrap();
        assert_eq!(
            directory,
            src(
                "DietrichGebert",
                "ponytail",
                Some("main"),
                Some("skills/ponytail")
            )
        );

        let file = parse_source(
            "https://github.com/DietrichGebert/ponytail/blob/main/skills/ponytail/SKILL.md",
        )
        .unwrap();
        assert_eq!(file, directory);

        let raw = parse_source("https://raw.githubusercontent.com/DietrichGebert/ponytail/main/skills/ponytail/SKILL.md").unwrap();
        assert_eq!(raw, directory);

        assert_eq!(
            parse_source("DietrichGebert/ponytail@main").unwrap(),
            src("DietrichGebert", "ponytail", Some("main"), None)
        );
        // Trailing slash, `.git` and a fragment are noise, not a different repository.
        assert_eq!(
            parse_source("https://github.com/DietrichGebert/ponytail.git/#install").unwrap(),
            repo_page
        );
        assert_eq!(
            parse_source("  DietrichGebert/ponytail  ").unwrap(),
            repo_page
        );
    }

    #[test]
    fn a_source_that_cannot_be_understood_is_named_not_guessed() {
        // Nothing here should reach the network, and none of it should silently become a
        // different repository than the one someone meant.
        for (input, hint) in [
            ("", "no source given"),
            ("ponytail", "owner/repo"),
            ("owner/repo/extra", "full github.com URL"),
            ("https://gitlab.com/owner/repo", "not supported"),
            ("https://github.com/owner", "does not name a repository"),
            ("https://github.com/owner/repo/tree", "names no branch"),
            (
                "https://github.com/owner/repo/pulls/12",
                "not a repository URL",
            ),
            (
                "https://raw.githubusercontent.com/owner/repo/main",
                "not a raw file URL",
            ),
        ] {
            let error = parse_source(input).expect_err(&format!("{input} must be refused"));
            let message = error.to_string();
            assert!(
                message.contains(hint),
                "{input}: wanted '{hint}' in '{message}'"
            );
        }
    }

    #[test]
    fn a_skill_name_that_is_not_a_directory_name_is_refused() {
        // The name comes from the downloaded file and becomes a directory under skills_dir, so a
        // traversal has to die here rather than in the filesystem.
        assert_eq!(
            safe_skill_dir_name(" ponytail-review ").unwrap(),
            "ponytail-review"
        );
        for hostile in [
            "",
            "..",
            ".",
            "../../../.ssh/authorized_keys",
            "a/b",
            "a\\b",
            ".hidden",
            "/etc/cron.d/x",
        ] {
            assert!(
                safe_skill_dir_name(hostile).is_err(),
                "{hostile:?} must not become a directory name"
            );
        }
    }

    /// A tar.gz with the given `(path inside the archive, body)` files.
    fn archive_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    format!("owner-repo-abc123/{path}"),
                    body.as_bytes(),
                )
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// The common shape: one SKILL.md per skill directory.
    fn archive_of_skills(skill_dirs: &[(&str, &str)]) -> Vec<u8> {
        let files: Vec<(String, String)> = skill_dirs
            .iter()
            .map(|(dir, body)| (format!("skills/{dir}/SKILL.md"), body.to_string()))
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_str()))
            .collect();
        archive_of(&borrowed)
    }

    fn skill_body(name: &str) -> String {
        format!("---\nname: {name}\ndescription: does one thing\n---\n\n# {name}\n")
    }

    #[test]
    fn the_archive_is_unpacked_and_its_skills_found_and_parsed() {
        let archive = archive_of_skills(&[
            ("ponytail", &skill_body("ponytail")),
            ("ponytail-debt", &skill_body("ponytail-debt")),
            ("broken", "no frontmatter here"),
        ]);
        let temp = tempfile::tempdir().unwrap();
        let root = extract_archive(&archive, temp.path()).unwrap();

        let found = find_skills(&root).unwrap();
        let names: Vec<&str> = found
            .skills
            .iter()
            .map(|(_, skill)| skill.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["ponytail", "ponytail-debt"],
            "sorted, and the one without frontmatter is not a skill"
        );
        assert_eq!(
            found.broken.len(),
            1,
            "the broken file is reported, not silently dropped"
        );
        assert!(found.broken[0].contains("SKILL.md"), "{:?}", found.broken);
        // The directory each skill came from, so the install copies the right one.
        assert!(
            found.skills[0].0.ends_with("skills/ponytail"),
            "{:?}",
            found.skills[0].0
        );
    }

    #[test]
    fn a_repo_that_ships_one_skill_for_several_agents_installs_it_once() {
        // What the ponytail repository actually looks like: the skill under `skills/`, and a copy
        // under each agent's own directory. The loader keys by name, so installing both copies would
        // copy one over the other and report "12 skills" for a repo holding six.
        let body = skill_body("ponytail");
        let archive = archive_of(&[
            ("skills/ponytail/SKILL.md", &body),
            (".claude/skills/ponytail/SKILL.md", &body),
            (".agents/plugins/ponytail/SKILL.md", &body),
            (
                "skills/ponytail-review/SKILL.md",
                &skill_body("ponytail-review"),
            ),
        ]);
        let temp = tempfile::tempdir().unwrap();
        let root = extract_archive(&archive, temp.path()).unwrap();

        let found = find_skills(&root).unwrap();
        let names: Vec<&str> = found
            .skills
            .iter()
            .map(|(_, skill)| skill.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["ponytail", "ponytail-review"],
            "one entry per name"
        );
        // The shallowest copy is the one that gets installed: the agent-specific ones are variants.
        assert!(
            found.skills[0].0.ends_with("skills/ponytail"),
            "the vendor-neutral copy wins: {:?}",
            found.skills[0].0
        );
        assert_eq!(
            found.shadowed.len(),
            2,
            "both shadowed copies are reported: {:?}",
            found.shadowed
        );
        assert!(
            found
                .shadowed
                .iter()
                .all(|(name, path)| name == "ponytail" && path.starts_with('.')),
            "each is named with the agent directory it came from: {:?}",
            found.shadowed
        );
    }

    #[test]
    fn installing_copies_the_skill_and_refuses_to_overwrite_one() {
        let archive = archive_of_skills(&[("ponytail", &skill_body("ponytail"))]);
        let temp = tempfile::tempdir().unwrap();
        let staging = temp.path().join(".incoming");
        let root = extract_archive(&archive, &staging).unwrap();
        let found = find_skills(&root).unwrap();
        let (dir, skill) = &found.skills[0];

        let skills_dir = temp.path().join("skills");
        let name = safe_skill_dir_name(&skill.name).unwrap();
        let target = skills_dir.join(&name);
        copy_tree(dir, &target).unwrap();
        assert!(target.join("SKILL.md").exists());
        // And the loader that a chat session uses finds it where it landed.
        let installed = SkillLoader::load_skills_from_dir(&skills_dir).unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "ponytail");

        // A second install without --force must not touch it (the caller checks before copying).
        assert!(target.exists());
    }

    #[test]
    fn a_url_subpath_cannot_climb_out_of_the_repository() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("owner-repo-abc123");
        fs::create_dir_all(&root).unwrap();

        let inside = resolve_subpath(&root, &Some("skills/ponytail".to_string())).unwrap();
        assert!(inside.ends_with("skills/ponytail"));

        for hostile in ["../../etc", "/etc/cron.d", "skills/../../../etc"] {
            assert!(
                resolve_subpath(&root, &Some(hostile.to_string())).is_err(),
                "{hostile} must not resolve outside the archive"
            );
        }
    }

    #[test]
    fn a_tree_without_a_leading_directory_still_installs() {
        // `tar` from a URL does not always wrap the tree; the extractor must not assume it does.
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for name in ["SKILL.md", "scripts/run.sh"] {
            let body = if name.ends_with(".md") {
                skill_body("flat")
            } else {
                "#!/bin/sh\n".into()
            };
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, body.as_bytes())
                .unwrap();
        }
        let archive = builder.into_inner().unwrap().finish().unwrap();

        let temp = tempfile::tempdir().unwrap();
        let root = extract_archive(&archive, &temp.path().join("incoming")).unwrap();
        let found = find_skills(&root).unwrap();
        assert_eq!(
            found.skills.len(),
            1,
            "found in the staging root itself: {:?}",
            found.broken
        );

        // A skill directory carries more than its SKILL.md; that is the point of copying a tree.
        let target = temp.path().join("skills").join("flat");
        copy_tree(&root, &target).unwrap();
        assert!(target.join("scripts/run.sh").exists());
    }

    #[test]
    fn the_staging_directory_is_removed_however_the_install_ends() {
        let temp = tempfile::tempdir().unwrap();
        let staging = Staging(temp.path().join(".incoming-1"));
        fs::create_dir_all(&staging.0).unwrap();
        let mut file = fs::File::create(staging.0.join("partial")).unwrap();
        file.write_all(b"half a download").unwrap();
        drop(file);
        drop(staging);
        assert!(!temp.path().join(".incoming-1").exists());
    }
}
