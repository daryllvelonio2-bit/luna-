use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

use harness_core::{HarnessError, Tool, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub file_path: PathBuf,
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
            HarnessError::PluginError(format!("Failed to parse YAML frontmatter in {}: {}", path.display(), e))
        })?;

        Ok(Skill {
            name: meta.name,
            description: meta.description,
            instructions,
            file_path: path.to_path_buf(),
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
        json!({
            "type": "object",
            "properties": {
                "input": { "type": "string", "description": "Input parameters or query for this skill" }
            },
            "required": ["input"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let input = args.get("input").and_then(|i| i.as_str()).unwrap_or_default();
        let rendered = format!(
            "SKILL: {}\nINSTRUCTIONS:\n{}\n\nUSER INPUT:\n{}",
            self.skill.name, self.skill.instructions, input
        );
        Ok(ToolResult::success(call_id, rendered))
    }
}
