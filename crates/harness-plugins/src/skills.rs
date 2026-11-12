use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use harness_core::{HarnessError, Tool, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub file_path: PathBuf,
    pub command: Option<String>,
    pub timeout_secs: Option<u64>,
    pub parameters: Option<serde_json::Value>,
}

impl Skill {
    /// A skill that ships inside the binary.
    ///
    /// A built-in skill has no file: its text is compiled in, so it is always
    /// present, always version-matched, and cannot be half-installed. The
    /// path says so, because a caller that wants to edit a skill needs to know
    /// that this one cannot be edited on disk.
    pub fn builtin(name: &str, description: &str, instructions: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            instructions: instructions.to_string(),
            file_path: PathBuf::from("<builtin>"),
            command: None,
            timeout_secs: None,
            parameters: None,
        }
    }
}

/// Whether two skill names name the same skill.
///
/// A hyphen and an underscore are the same character to a person typing a
/// command, and `agent-desktop` is as likely to be written `agent_desktop` as
/// not. The comparison ignores case and treats the two separators as one.
pub fn names_match(left: &str, right: &str) -> bool {
    normalize_name(left) == normalize_name(right)
}

fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}

pub struct SkillLoader;

impl SkillLoader {
    pub fn load_skills_from_dir(dir: &Path) -> Result<Vec<Skill>, HarnessError> {
        let mut skills = Vec::new();
        if !dir.exists() {
            return Ok(skills);
        }

        for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                if let Ok(skill) = Self::parse_skill_file(path) {
                    skills.push(skill);
                }
            }
        }
        Ok(skills)
    }

    pub fn parse_skill_file(path: &Path) -> Result<Skill, HarnessError> {
        let content = fs::read_to_string(path).map_err(|e| HarnessError::Io(e.to_string()))?;
        let trimmed = content.trim_start();

        if !trimmed.starts_with("---") {
            return Err(HarnessError::PluginError(format!(
                "Skill file {} missing YAML frontmatter delimiter '---'",
                path.display()
            )));
        }

        let parts: Vec<&str> = trimmed.splitn(3, "---").collect();
        if parts.len() < 3 {
            return Err(HarnessError::PluginError(format!(
                "Skill file {} has invalid frontmatter structure",
                path.display()
            )));
        }

        let frontmatter_str = parts[1];
        let instructions = parts[2].trim().to_string();

        let meta: SkillMetadata = serde_yaml::from_str(frontmatter_str).map_err(|e| {
            HarnessError::PluginError(format!(
                "Failed to parse YAML frontmatter in {}: {}",
                path.display(),
                e
            ))
        })?;

        Ok(Skill {
            name: meta.name,
            description: meta.description,
            instructions,
            file_path: path.to_path_buf(),
            command: meta.command,
            timeout_secs: meta.timeout_secs,
            parameters: meta.parameters,
        })
    }
}

pub struct SkillToolAdapter {
    skill: Skill,
}

impl SkillToolAdapter {
    pub fn new(skill: Skill) -> Self {
        Self { skill }
    }
}

#[async_trait]
impl Tool for SkillToolAdapter {
    fn name(&self) -> &str {
        &self.skill.name
    }

    fn description(&self) -> &str {
        &self.skill.description
    }

    fn parameters(&self) -> serde_json::Value {
        if let Some(ref custom_params) = self.skill.parameters {
            return custom_params.clone();
        }
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "description": "Action to perform (e.g. search, fetch, open)" },
                "query": { "type": "string", "description": "Search query or target URL" },
                "url": { "type": "string", "description": "Target website URL" },
                "input": { "type": "string", "description": "Input parameters or payload for this skill" }
            }
        })
    }

    fn is_mutating(&self) -> bool {
        // Any skill that executes external commands must be gated through approval
        self.skill.command.is_some()
    }

    async fn preview(&self, args: &serde_json::Value) -> Option<harness_core::ToolPreview> {
        self.skill.command.as_ref().map(|cmd| {
            let skill_dir = self
                .skill
                .file_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_string_lossy();
            let cmd_str = cmd.replace("{skill_dir}", &skill_dir);
            harness_core::ToolPreview {
                target: Some(format!(
                    "{} '{}' with args {}",
                    if cfg!(windows) { "cmd /C" } else { "sh -c" },
                    cmd_str,
                    args
                )),
                diff: None,
            }
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if let Some(ref cmd_template) = self.skill.command {
            run_skill_command(&self.skill, call_id, args, cancel_token, cmd_template).await
        } else {
            Ok(ToolResult::success(
                call_id,
                render_instructions(&self.skill, args),
            ))
        }
    }
}

async fn run_skill_command(
    skill: &Skill,
    call_id: &str,
    args: &serde_json::Value,
    cancel_token: &CancellationToken,
    cmd_template: &str,
) -> Result<ToolResult, HarnessError> {
    let skill_dir = skill
        .file_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_string_lossy();

    let cmd_str = cmd_template.replace("{skill_dir}", &skill_dir);
    let timeout = skill.timeout_secs.unwrap_or(60);
    let args_json = serde_json::to_string(args).unwrap_or_default();

    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C");
        // `cmd /C` does not follow CommandLineToArgvW quoting; pass the skill command
        // through exactly as written, pipes and all.
        cmd.raw_arg(&cmd_str);
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&cmd_str);
        cmd
    };
    cmd.kill_on_drop(true)
        .current_dir(skill.file_path.parent().unwrap_or_else(|| Path::new(".")))
        .env("SKILL_DIR", skill_dir.as_ref())
        .env("SKILL_NAME", &skill.name)
        .env("SKILL_ARGS", &args_json)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| HarnessError::ToolExecutionFailed {
        tool: skill.name.clone(),
        reason: format!("Failed to spawn skill command: {}", e),
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(args_json.as_bytes()).await;
        let _ = stdin.flush().await;
    }

    let timeout_duration = std::time::Duration::from_secs(timeout);
    let output = tokio::select! {
        _ = cancel_token.cancelled() => {
            return Err(HarnessError::Cancelled);
        }
        res = tokio::time::timeout(timeout_duration, child.wait_with_output()) => match res {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Ok(ToolResult::error(call_id, format!("Process error: {}", e))),
            Err(_) => {
                return Ok(ToolResult::error(call_id, format!("Skill timed out after {}s", timeout)));
            }
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        let text = if stdout.trim().is_empty() && !stderr.trim().is_empty() {
            stderr
        } else {
            stdout
        };
        Ok(ToolResult::success(call_id, text))
    } else {
        let err_msg = if !stderr.trim().is_empty() {
            stderr
        } else if !stdout.trim().is_empty() {
            stdout
        } else {
            format!("Command exited with status code: {}", output.status)
        };
        Ok(ToolResult::error(call_id, err_msg))
    }
}

fn render_instructions(skill: &Skill, args: &serde_json::Value) -> String {
    let input = args
        .get("input")
        .and_then(|i| i.as_str())
        .unwrap_or_default();
    format!(
        "SKILL: {}\nINSTRUCTIONS:\n{}\n\nUSER INPUT:\n{}",
        skill.name, skill.instructions, input
    )
}

fn list_companion_files(base_dir: &Path) -> Vec<String> {
    if !base_dir.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(base_dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if let Ok(rel) = path.strip_prefix(base_dir) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if file_name.eq_ignore_ascii_case("SKILL.md")
                || rel_str.starts_with(".git/")
                || rel_str.starts_with("node_modules/")
                || rel_str.contains("/__pycache__/")
            {
                continue;
            }
            files.push(rel_str);
        }
    }
    files.sort();
    files
}

pub struct SkillViewTool {
    skills: Arc<Vec<Skill>>,
}

impl SkillViewTool {
    pub fn new(skills: Arc<Vec<Skill>>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl Tool for SkillViewTool {
    fn name(&self) -> &str {
        "skill_view"
    }

    fn description(&self) -> &str {
        "Load the full instructions for an available skill, or inspect companion reference files within a skill. Call this after skills_list names a skill that matches the task, then follow those instructions."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the skill to inspect (e.g. 'impeccable'), or combined skill and companion path (e.g. 'impeccable/reference/craft-floor.md')."
                },
                "path": {
                    "type": "string",
                    "description": "Optional relative path to a companion reference file or script within the skill directory (e.g. 'reference/craft-floor.md')."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let raw_name = args
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .trim();
        let explicit_path = args
            .get("path")
            .and_then(|p| p.as_str())
            .map(str::trim)
            .filter(|p| !p.is_empty());

        let (skill_query, rel_path) = if let Some(p) = explicit_path {
            (raw_name, Some(p))
        } else if let Some(skill) = self.skills.iter().find(|s| names_match(&s.name, raw_name)) {
            (skill.name.as_str(), None)
        } else if let Some((skill_part, file_part)) = raw_name
            .split_once('/')
            .or_else(|| raw_name.split_once('\\'))
        {
            (skill_part.trim(), Some(file_part.trim()))
        } else {
            (raw_name, None)
        };

        let Some(skill) = self.skills.iter().find(|s| names_match(&s.name, skill_query)) else {
            let available: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
            return Ok(ToolResult::error(
                call_id,
                format!(
                    "Skill '{}' not found. Available skills: {}",
                    skill_query,
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                ),
            ));
        };

        if let Some(subpath) = rel_path {
            if skill.file_path == Path::new("<builtin>") {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Builtin skill '{}' has no companion files.", skill.name),
                ));
            }
            let base_dir = skill.file_path.parent().unwrap_or(Path::new(""));
            if !base_dir.is_dir() {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Skill directory '{}' does not exist.", base_dir.display()),
                ));
            }
            let Ok(canonical_base) = base_dir.canonicalize() else {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Skill directory '{}' could not be resolved.", base_dir.display()),
                ));
            };

            let clean_subpath = subpath.trim_start_matches('/').trim_start_matches('\\');
            let mut target_path = base_dir.join(clean_subpath);

            if !target_path.exists() && !clean_subpath.ends_with(".md") {
                let with_md = base_dir.join(format!("{}.md", clean_subpath));
                if with_md.exists() {
                    target_path = with_md;
                }
            }

            let canonical_target = match target_path.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    let available = list_companion_files(base_dir);
                    let list_str = if available.is_empty() {
                        "No companion files in this skill.".to_string()
                    } else {
                        let max_suggest = 30;
                        let sample: Vec<String> = available
                            .iter()
                            .take(max_suggest)
                            .map(|f| format!("- {}", f))
                            .collect();
                        let mut s = format!("Available companion files:\n{}", sample.join("\n"));
                        if available.len() > max_suggest {
                            s.push_str(&format!(
                                "\n... and {} more files",
                                available.len() - max_suggest
                            ));
                        }
                        s
                    };
                    return Ok(ToolResult::error(
                        call_id,
                        format!(
                            "Companion file '{}' not found in skill '{}'.\n{}",
                            clean_subpath, skill.name, list_str
                        ),
                    ));
                }
            };

            if !canonical_target.starts_with(&canonical_base) {
                return Ok(ToolResult::error(
                    call_id,
                    format!(
                        "Access denied: companion file '{}' escapes skill directory.",
                        clean_subpath
                    ),
                ));
            }

            if !canonical_target.is_file() {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Path '{}' is a directory, not a file.", clean_subpath),
                ));
            }

            return match fs::read_to_string(&canonical_target) {
                Ok(content) => {
                    let output = format!(
                        "# Skill File: {}/{}\nBase Directory: {}\nFull Path: {}\n\n{}",
                        skill.name,
                        clean_subpath,
                        base_dir.display(),
                        canonical_target.display(),
                        content
                    );
                    Ok(ToolResult::success(call_id, output))
                }
                Err(e) => Ok(ToolResult::error(
                    call_id,
                    format!("Failed to read companion file '{}': {}", clean_subpath, e),
                )),
            };
        }

        let mut output = format!("# Skill: {}\n{}\n", skill.name, skill.description);
        if skill.file_path != Path::new("<builtin>") {
            if let Some(parent) = skill.file_path.parent() {
                if !parent.as_os_str().is_empty() {
                    output.push_str(&format!("Base Directory: {}\n", parent.display()));
                }
                let companion_files = list_companion_files(parent);
                if !companion_files.is_empty() {
                    let count = companion_files.len();
                    let max_show = 40;
                    let mut list_text = String::new();
                    for f in companion_files.iter().take(max_show) {
                        list_text.push_str(&format!("- {}\n", f));
                    }
                    if count > max_show {
                        list_text.push_str(&format!("... and {} more files\n", count - max_show));
                    }
                    output.push_str(&format!(
                        "\n## Companion Files ({})\n{}\n(Inspect any companion file directly with `skill_view(name=\"{}/<path>\")`)\n",
                        count,
                        list_text.trim_end(),
                        skill.name
                    ));
                }
            }
        }
        output.push_str(&format!("\n## Instructions\n{}", skill.instructions));
        if let Some(ref cmd) = skill.command {
            output.push_str(&format!("\n\n## Command Template\n{}", cmd));
        }
        Ok(ToolResult::success(call_id, output))
    }
}

pub struct SkillsListTool {
    skills: Arc<Vec<Skill>>,
}

impl SkillsListTool {
    pub fn new(skills: Arc<Vec<Skill>>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl Tool for SkillsListTool {
    fn name(&self) -> &str {
        "skills_list"
    }

    fn description(&self) -> &str {
        "List all available skills with their short descriptions. Call this before starting a task that could match a skill, then load the match with skill_view."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        _args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if self.skills.is_empty() {
            return Ok(ToolResult::success(call_id, "No skills available."));
        }

        let lines: Vec<String> = self
            .skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect();

        Ok(ToolResult::success(
            call_id,
            format!(
                "Available skills (load with skill_view):\n{}",
                lines.join("\n")
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn test_parse_and_execute_command_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skill_file = dir.path().join("SKILL.md");
        let content = r#"---
name: echo-test
description: "Echo test skill"
command: "echo 'hello from skill:' $SKILL_NAME"
---
Test instructions
"#;
        std::fs::write(&skill_file, content).unwrap();

        let skill = SkillLoader::parse_skill_file(&skill_file).unwrap();
        assert_eq!(skill.name, "echo-test");
        assert_eq!(skill.description, "Echo test skill");
        assert_eq!(
            skill.command,
            Some("echo 'hello from skill:' $SKILL_NAME".to_string())
        );

        let adapter = SkillToolAdapter::new(skill);
        let cancel = CancellationToken::new();
        let args = serde_json::json!({ "query": "world" });
        let res = adapter.execute("call-1", &args, &cancel).await.unwrap();

        assert!(!res.is_error);
        assert!(res.content.contains("hello from skill: echo-test"));
    }

    #[tokio::test]
    async fn test_load_browser_skill() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest_dir.parent().unwrap().parent().unwrap();
        let browser_skill_path = root.join(".harness/skills/browser/SKILL.md");
        if browser_skill_path.exists() {
            let skill = SkillLoader::parse_skill_file(&browser_skill_path).unwrap();
            assert_eq!(skill.name, "browser");
            assert!(skill.command.is_some());
            let adapter = SkillToolAdapter::new(skill);
            assert_eq!(adapter.name(), "browser");
        }
    }

    /// The tools carry the discovery loop themselves: a model that only reads tool definitions
    /// (no prompt index) still learns to list first, then load the match.
    #[test]
    fn the_skill_tools_document_the_discover_then_load_loop() {
        let skills = Arc::new(Vec::new());
        let list = SkillsListTool::new(skills.clone());
        let view = SkillViewTool::new(skills);
        assert!(
            list.description().contains("skill_view"),
            "{}",
            list.description()
        );
        assert!(
            view.description().contains("skills_list"),
            "{}",
            view.description()
        );
    }

    #[tokio::test]
    async fn test_skill_view_and_skills_list_tools() {
        let skills = Arc::new(vec![
            Skill {
                name: "ponytail".to_string(),
                description: "Lazy senior developer guidance".to_string(),
                instructions: "Write the simplest solution.".to_string(),
                file_path: PathBuf::from("/skills/ponytail/SKILL.md"),
                command: None,
                timeout_secs: None,
                parameters: None,
            },
            Skill {
                name: "security".to_string(),
                description: "Security review checklist".to_string(),
                instructions: "Check for injections.".to_string(),
                file_path: PathBuf::from("/skills/security/SKILL.md"),
                command: None,
                timeout_secs: None,
                parameters: None,
            },
        ]);

        let list_tool = SkillsListTool::new(skills.clone());
        let cancel = CancellationToken::new();
        let list_res = list_tool
            .execute("c1", &serde_json::json!({}), &cancel)
            .await
            .unwrap();
        assert!(list_res
            .content
            .contains("ponytail: Lazy senior developer guidance"));
        assert!(list_res
            .content
            .contains("security: Security review checklist"));

        let view_tool = SkillViewTool::new(skills);
        let view_res = view_tool
            .execute("c2", &serde_json::json!({ "name": "ponytail" }), &cancel)
            .await
            .unwrap();
        assert!(!view_res.is_error);
        assert!(view_res.content.contains("# Skill: ponytail"));
        assert!(view_res.content.contains("Write the simplest solution."));

        let not_found = view_tool
            .execute("c3", &serde_json::json!({ "name": "unknown" }), &cancel)
            .await
            .unwrap();
        assert!(not_found.is_error);
        assert!(not_found.content.contains("Skill 'unknown' not found"));
    }

    #[tokio::test]
    async fn test_skill_view_companion_files() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("crafty");
        fs::create_dir_all(skill_dir.join("reference")).unwrap();
        fs::create_dir_all(skill_dir.join("scripts")).unwrap();

        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: crafty\ndescription: UI craft guidelines\n---\nCraft high quality interfaces.",
        )
        .unwrap();
        fs::write(
            skill_dir.join("reference/craft-floor.md"),
            "Strict craft floor checklist.",
        )
        .unwrap();
        fs::write(
            skill_dir.join("scripts/build.sh"),
            "#!/bin/sh\necho built",
        )
        .unwrap();

        let skills = Arc::new(vec![Skill {
            name: "crafty".to_string(),
            description: "UI craft guidelines".to_string(),
            instructions: "Craft high quality interfaces.".to_string(),
            file_path: skill_dir.join("SKILL.md"),
            command: None,
            timeout_secs: None,
            parameters: None,
        }]);

        let view_tool = SkillViewTool::new(skills);
        let cancel = CancellationToken::new();

        // 1. Inspect main skill - should list companion files and base dir
        let main_res = view_tool
            .execute("c1", &serde_json::json!({ "name": "crafty" }), &cancel)
            .await
            .unwrap();
        assert!(!main_res.is_error);
        assert!(main_res.content.contains("Base Directory:"));
        assert!(main_res.content.contains("## Companion Files"));
        assert!(main_res.content.contains("reference/craft-floor.md"));
        assert!(main_res.content.contains("scripts/build.sh"));

        // 2. Inspect companion file via combined slash name
        let comp_res1 = view_tool
            .execute(
                "c2",
                &serde_json::json!({ "name": "crafty/reference/craft-floor.md" }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(!comp_res1.is_error);
        assert!(comp_res1.content.contains("Strict craft floor checklist."));
        assert!(comp_res1.content.contains("Full Path:"));

        // 3. Inspect companion file via name + explicit path
        let comp_res2 = view_tool
            .execute(
                "c3",
                &serde_json::json!({ "name": "crafty", "path": "reference/craft-floor.md" }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(!comp_res2.is_error);
        assert!(comp_res2.content.contains("Strict craft floor checklist."));

        // 4. Inspect companion file without .md extension
        let comp_res3 = view_tool
            .execute(
                "c4",
                &serde_json::json!({ "name": "crafty/reference/craft-floor" }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(!comp_res3.is_error);
        assert!(comp_res3.content.contains("Strict craft floor checklist."));

        // 5. Missing companion file provides available files in error
        let missing_res = view_tool
            .execute(
                "c5",
                &serde_json::json!({ "name": "crafty/reference/missing.md" }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(missing_res.is_error);
        assert!(missing_res
            .content
            .contains("Companion file 'reference/missing.md' not found in skill 'crafty'"));
        assert!(missing_res.content.contains("Available companion files:"));
        assert!(missing_res.content.contains("reference/craft-floor.md"));

        // 6. Security traversal check
        let escape_res = view_tool
            .execute(
                "c6",
                &serde_json::json!({ "name": "crafty/../../secret.txt" }),
                &cancel,
            )
            .await
            .unwrap();
        assert!(escape_res.is_error);
    }
}
