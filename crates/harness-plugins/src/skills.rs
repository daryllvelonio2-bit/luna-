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
            let skill_dir = self
                .skill
                .file_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_string_lossy();

            let cmd_str = cmd_template.replace("{skill_dir}", &skill_dir);
            let timeout = self.skill.timeout_secs.unwrap_or(60);
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
                .current_dir(
                    self.skill
                        .file_path
                        .parent()
                        .unwrap_or_else(|| Path::new(".")),
                )
                .env("SKILL_DIR", skill_dir.as_ref())
                .env("SKILL_NAME", &self.skill.name)
                .env("SKILL_ARGS", &args_json)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let mut child = cmd.spawn().map_err(|e| HarnessError::ToolExecutionFailed {
                tool: self.skill.name.clone(),
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
        } else {
            let input = args
                .get("input")
                .and_then(|i| i.as_str())
                .unwrap_or_default();
            let rendered = format!(
                "SKILL: {}\nINSTRUCTIONS:\n{}\n\nUSER INPUT:\n{}",
                self.skill.name, self.skill.instructions, input
            );
            Ok(ToolResult::success(call_id, rendered))
        }
    }
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
        "View the full instructions and guidelines for an available skill on demand."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the skill to inspect"
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
        let name = args
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .trim();
        if let Some(skill) = self
            .skills
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
        {
            let mut output = format!(
                "# Skill: {}\n{}\n\n## Instructions\n{}",
                skill.name, skill.description, skill.instructions
            );
            if let Some(ref cmd) = skill.command {
                output.push_str(&format!("\n\n## Command Template\n{}", cmd));
            }
            Ok(ToolResult::success(call_id, output))
        } else {
            let available: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
            Ok(ToolResult::error(
                call_id,
                format!(
                    "Skill '{}' not found. Available skills: {}",
                    name,
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                ),
            ))
        }
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
        "List all available skills with their short descriptions."
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
}
