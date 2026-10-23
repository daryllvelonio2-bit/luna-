use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::HarnessError;

/// c1: System prompt configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SystemPromptSpec {
    pub base_template: String,
    pub always_on_prose: String,
    pub instruction_files: Vec<String>,
}

/// c2: Guardrails and execution safety constraints
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuardrailsSpec {
    pub max_consecutive_failures: usize,
    pub require_hitl_for_mutation: bool,
    pub banned_tools: Vec<String>,
}

impl Default for GuardrailsSpec {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 5,
            require_hitl_for_mutation: true,
            banned_tools: Vec::new(),
        }
    }
}

/// c3: Memory indexing and retrieval spec
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySpec {
    pub enabled: bool,
    pub max_retrieved_memories: usize,
    pub enable_pointer_distillation: bool,
}

impl Default for MemorySpec {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retrieved_memories: 10,
            enable_pointer_distillation: true,
        }
    }
}

/// c4: Declarative on-demand skill
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SkillSpec {
    pub name: String,
    pub trigger: String,
    pub description: String,
    pub instructions: String,
}

/// c6: Workspace helper script definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ScriptSpec {
    pub interpreter: String,
    pub content: String,
}

/// c7: Delegated subagent profile
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SubagentSpec {
    pub name: String,
    pub role: String,
    pub model: Option<String>,
    pub prompt: String,
}

/// c8: Plugin / middleware hook definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PluginSpec {
    pub name: String,
    pub path: Option<String>,
    pub priority: i32,
    pub config: serde_json::Value,
}

/// SurfaceSpec: Formalized 8-component editable harness surface (c1–c8)
/// Grouped into:
/// - Context Construction: c1 (system prompt), c2 (guardrails), c3 (memory), c4 (skills)
/// - Tool Interface: c5 (tool guidance), c6 (scripts)
/// - Delegation: c7 (subagents)
/// - Middleware: c8 (plugins)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SurfaceSpec {
    pub c1_system_prompt: SystemPromptSpec,
    pub c2_guardrails: GuardrailsSpec,
    pub c3_memory: MemorySpec,
    pub c4_skills: Vec<SkillSpec>,
    pub c5_tool_guidance: HashMap<String, String>,
    pub c6_scripts: HashMap<String, ScriptSpec>,
    pub c7_subagents: Vec<SubagentSpec>,
    pub c8_plugins: Vec<PluginSpec>,
}

impl SurfaceSpec {
    /// Validates the surface against the core kill rules:
    /// 1. Every skill needs a trigger line mentioned in always-on system prompt text.
    /// 2. No unresolved template syntax ('{{' or '{%') leaking into always-on prompt prose.
    /// 3. Every declared script must have an interpreter and non-empty content.
    pub fn validate(&self) -> Result<(), HarnessError> {
        let always_on = &self.c1_system_prompt.always_on_prose;

        // Kill rule 2: no raw template tokens in always-on prompt
        if always_on.contains("{{") || always_on.contains("{%") {
            return Err(HarnessError::SurfaceValidationError(
                "always-on system prompt contains unresolved template tokens ({{ or {%)".into(),
            ));
        }

        // Kill rule 1: every skill needs a trigger line in always-on prompt
        for skill in &self.c4_skills {
            if skill.name.trim().is_empty() {
                return Err(HarnessError::SurfaceValidationError(
                    "skill declared with empty name".into(),
                ));
            }
            let trigger = skill.trigger.trim();
            if !trigger.is_empty()
                && !always_on.contains(trigger)
                && !always_on.contains(&skill.name)
            {
                return Err(HarnessError::SurfaceValidationError(format!(
                    "skill '{}' trigger '{}' not found in always-on system prompt",
                    skill.name, trigger
                )));
            }
        }

        // Kill rule 3: every named script must exist and have content
        for (name, script) in &self.c6_scripts {
            if name.trim().is_empty() {
                return Err(HarnessError::SurfaceValidationError(
                    "script declared with empty name".into(),
                ));
            }
            if script.interpreter.trim().is_empty() {
                return Err(HarnessError::SurfaceValidationError(format!(
                    "script '{}' missing interpreter",
                    name
                )));
            }
            if script.content.trim().is_empty() {
                return Err(HarnessError::SurfaceValidationError(format!(
                    "script '{}' has empty body content",
                    name
                )));
            }
        }

        Ok(())
    }

    /// Renders combined system prompt instructions incorporating guidance, guardrails, and memory
    pub fn render_system_prompt(&self) -> String {
        let mut sections = Vec::new();

        if !self.c1_system_prompt.base_template.is_empty() {
            sections.push(self.c1_system_prompt.base_template.clone());
        }

        if !self.c1_system_prompt.always_on_prose.is_empty() {
            sections.push(self.c1_system_prompt.always_on_prose.clone());
        }

        if !self.c5_tool_guidance.is_empty() {
            let mut guidance_section = String::from("## Tool Guidance\n");
            for (tool, guidance) in &self.c5_tool_guidance {
                guidance_section.push_str(&format!("- {}: {}\n", tool, guidance));
            }
            sections.push(guidance_section);
        }

        sections.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_surface_spec_validation_passes() {
        let mut spec = SurfaceSpec::default();
        spec.c1_system_prompt.always_on_prose = "Use browser for web lookup.".into();
        spec.c4_skills.push(SkillSpec {
            name: "browser".into(),
            trigger: "browser".into(),
            description: "Browse websites".into(),
            instructions: "Run browse script".into(),
        });
        spec.c6_scripts.insert(
            "run_eval".into(),
            ScriptSpec {
                interpreter: "python3".into(),
                content: "print('eval')".into(),
            },
        );

        assert!(spec.validate().is_ok());
    }

    #[test]
    fn test_surface_spec_fails_unresolved_template() {
        let mut spec = SurfaceSpec::default();
        spec.c1_system_prompt.always_on_prose = "Hello {{user_name}}".into();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn test_surface_spec_fails_missing_skill_trigger() {
        let mut spec = SurfaceSpec::default();
        spec.c1_system_prompt.always_on_prose = "Normal agent prompt.".into();
        spec.c4_skills.push(SkillSpec {
            name: "calculator".into(),
            trigger: "calc_command".into(),
            description: "Math".into(),
            instructions: "Do math".into(),
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn test_surface_spec_fails_empty_script() {
        let mut spec = SurfaceSpec::default();
        spec.c6_scripts.insert(
            "empty".into(),
            ScriptSpec {
                interpreter: "bash".into(),
                content: "   ".into(),
            },
        );
        assert!(spec.validate().is_err());
    }
}
