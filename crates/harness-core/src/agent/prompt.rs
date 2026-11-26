//! System-prompt assembly: the `prompts.md` template with memories and the runtime environment
//! substituted in.

use super::Agent;
use crate::error::HarnessError;
use crate::traits::*;

impl<P, M, N, R> Agent<P, M, N, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    /// Build the full system prompt for a turn, with memories and memory mode instructions substituted.
    pub async fn build_system_prompt(&self, user_input: &str) -> Result<String, HarnessError> {
        self.system_prompt(user_input).await
    }

    /// The system prompt: the static `prompts.md` contract with the active profile's persona and
    /// behavior, the configured prefix, and the retrieved memories substituted in.
    ///
    /// Substitution is a literal `replace`, not `format!`, so the prompt text stays free of format
    /// syntax — a stray brace in the file can never abort a turn.
    ///
    /// Order matters. The profile's behavior is substituted *last*, after every static marker has
    /// been resolved, so text a user wrote in a profile cannot remove or duplicate the locked
    /// tool/memory blocks. Markers inside profile text stay literal.
    pub(super) async fn system_prompt(&self, user_input: &str) -> Result<String, HarnessError> {
        let memory_prompt = self.memory_prompt(user_input).await?;
        let persistent_memory_tag = if self.config.memory_enabled {
            " with persistent memory"
        } else {
            ""
        };
        let skills_protocol = if self.config.skills_enabled {
            "- Skills: if `skills_list` is available, call it before starting a task that could match an installed skill; when one matches, load it with `skill_view(name)` and follow its instructions. Load companion reference files directly with `skill_view(name, path)` or `skill_view(\"skill/path\")`.\n  To install a skill from GitHub or the web, run `luna skill install <source>` via `terminal` (accepts `owner/repo[@ref]`, a GitHub repo/tree/blob URL, or a raw `SKILL.md` URL; use `--skill <name>` to pick specific skills and `--force` to overwrite). If given only a skill name, use `web_search` first to find its GitHub repository or URL. Never clone or download skills manually into `/tmp` or `~/.config/luna/skills`; `luna skill install` validates and installs directly into the configured skills directory (`luna skill list` shows installed paths)."
        } else {
            ""
        };
        let mcp_protocol = if self.config.mcp_enabled {
            let mcp_prompt = self.mcp_tools_prompt().await?;
            format!(
                "- Configured MCP Tools: {mcp_prompt}\n  Invoke MCP tools via mcp__<server>__<tool>."
            )
        } else {
            String::new()
        };
        let active_memories_block = if self.config.memory_enabled {
            format!("Active Memories:\n{memory_prompt}")
        } else {
            String::new()
        };
        let memory_instruction = self.memory_instruction()?;
        let rich_output = if self.config.tui_output {
            crate::prompts::section(crate::prompts::TUI_RENDER_PROMPT)?
        } else {
            String::new()
        };

        let env_info = self.runtime_environment();
        let voice_instruction = if self.config.voice_mode {
            "\n\nVOICE MODE ACTIVE (DUAL WRITTEN + SPOKEN EXPERIENCE):\n- Your written reply in the chat is rendered visually and read on screen; a separate spoken voice layer reads a concise conversational summary aloud.\n- Keep your full written reply thorough and well-formatted (Markdown tables, code blocks, lists, and complete details are great in the chat).\n- IMPORTANT FOR RESPONSIVENESS: Whenever you are about to run tools to perform a task (e.g. cleaning files, running commands, searching, or editing), write a brief, friendly 1-sentence acknowledgment FIRST (such as \"Alright, on it — cleaning up the build artifacts now.\") before invoking your first tool call."
        } else {
            ""
        };

        let pinned_skills_block = if !self.config.pinned_skill_instructions.is_empty() {
            let mut block = String::from(
                "\n\nPINNED SKILLS (ALWAYS ACTIVE):\nThe following skill instructions are pinned and MUST be strictly obeyed on every turn:\n",
            );
            for (name, instructions) in &self.config.pinned_skill_instructions {
                block.push_str(&format!("\n--- PINNED SKILL: {name} ---\n{instructions}\n"));
            }
            block
        } else {
            String::new()
        };

        if let Some(ref orch_prompt) = self.config.orchestrator_prompt {
            if !orch_prompt.trim().is_empty() {
                let base = self.fill_orchestrator_prompt(
                    orch_prompt,
                    persistent_memory_tag,
                    &active_memories_block,
                    skills_protocol,
                    &mcp_protocol,
                    &memory_instruction,
                    &rich_output,
                )?;
                return Ok(format!(
                    "{}{}{}{}",
                    base, env_info, voice_instruction, pinned_skills_block
                ));
            }
        }

        let (persona, behavior) = self.persona_and_behavior()?;
        let base = self.fill_base_prompt(
            &persona,
            &behavior,
            persistent_memory_tag,
            &active_memories_block,
            skills_protocol,
            &mcp_protocol,
            &memory_instruction,
            &rich_output,
        )?;

        Ok(format!(
            "{}{}{}{}",
            base, env_info, voice_instruction, pinned_skills_block
        ))
    }

    /// The active profile's persona and behavior. `system_prompt_override` replaces only the
    /// behavior section; the static contract below it still applies, which is what makes it safe
    /// for specialized subagents.
    fn persona_and_behavior(&self) -> Result<(String, String), HarnessError> {
        let default_profile = crate::profiles::default_profile()?;
        let persona = self
            .config
            .profile_persona
            .as_deref()
            .unwrap_or(&default_profile.persona)
            .to_string();
        let behavior = self
            .config
            .system_prompt_override
            .as_deref()
            .or(self.config.profile_behavior.as_deref())
            .unwrap_or(&default_profile.behavior)
            .to_string();
        Ok((persona, behavior))
    }

    /// The retrieved memory cards, as the block the system prompt carries.
    async fn memory_prompt(&self, user_input: &str) -> Result<String, HarnessError> {
        if !self.config.memory_enabled {
            return Ok(String::new());
        }
        let project_filter = self.config.memory_project_filter.as_deref();
        let query = if let Some(filter) = project_filter {
            if !user_input.contains(filter) {
                format!("{user_input} {filter}")
            } else {
                user_input.to_string()
            }
        } else if let Some(root) = &self.config.workspace_root {
            let project_name = root.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !project_name.is_empty() && !user_input.contains(project_name) {
                format!("{user_input} {project_name}")
            } else {
                user_input.to_string()
            }
        } else {
            user_input.to_string()
        };
        let relevant_memories = self
            .memory
            .retrieve_cards_scoped(&query, project_filter, 12)
            .await?;
        Ok(self.memory.format_as_system_prompt(&relevant_memories))
    }

    /// The memory/notes protocol for the configured mode.
    fn memory_instruction(&self) -> Result<String, HarnessError> {
        match self.config.memory_mode {
            crate::config::MemoryMode::Session => {
                let mut sections = Vec::new();
                if self.config.memory_enabled {
                    sections.push(crate::prompts::section(
                        crate::prompts::MEMORY_SESSION_PROMPT,
                    )?);
                }
                if self.config.notes_enabled {
                    sections.push(crate::prompts::section(
                        crate::prompts::NOTES_SESSION_PROMPT,
                    )?);
                }
                if sections.is_empty() {
                    Ok(String::new())
                } else {
                    Ok(format!("{}\n\n", sections.join("\n\n")))
                }
            }
            crate::config::MemoryMode::Background => {
                let mut sections = Vec::new();
                match (self.config.memory_enabled, self.config.notes_enabled) {
                    (true, true) => {
                        sections.push(crate::prompts::section(
                            crate::prompts::MEMORY_BACKGROUND_PROMPT,
                        )?);
                    }
                    (true, false) => {
                        sections.push("Memory Protocol (Background Saver Mode):\n- Memories are processed asynchronously in the background.\n- Focus 100% of the turn context on solving the technical objective directly.\n- Do not invoke memory creation or mutation tools.\n- Consult active memory index cards in Active Memories.".to_string());
                    }
                    (false, true) => {
                        sections.push("Project Notes Protocol (Background Saver Mode):\n- Long-term notes are processed asynchronously in the background.\n- Focus 100% of the turn context on solving the technical objective directly.\n- Inspect existing documentation using read_note or search_notes.".to_string());
                    }
                    (false, false) => {}
                }
                if sections.is_empty() {
                    Ok(String::new())
                } else {
                    Ok(format!("{}\n\n", sections.join("\n\n")))
                }
            }
        }
    }

    /// Substitute the dedicated `ORCHESTRATOR_PROMPT` markers.
    #[allow(clippy::too_many_arguments)]
    fn fill_orchestrator_prompt(
        &self,
        subagents_block: &str,
        persistent_memory_tag: &str,
        active_memories_block: &str,
        skills_protocol: &str,
        mcp_protocol: &str,
        memory_instruction: &str,
        rich_output: &str,
    ) -> Result<String, HarnessError> {
        Ok(
            crate::prompts::section(crate::prompts::ORCHESTRATOR_PROMPT)?
                .replace("{{PERSISTENT_MEMORY_TAG}}", persistent_memory_tag)
                .replace("{{ACTIVE_MEMORIES_BLOCK}}", active_memories_block)
                .replace("{{SKILLS_PROTOCOL}}", skills_protocol)
                .replace("{{MCP_PROTOCOL}}", mcp_protocol)
                .replace("{{RICH_OUTPUT_PROTOCOL}}", rich_output)
                .replace("{{MEMORY_AND_NOTE_PROTOCOL}}\n\n", memory_instruction)
                .replace("{{MEMORY_AND_NOTE_PROTOCOL}}", memory_instruction)
                .replace(
                    "{{SYSTEM_PROMPT_PREFIX}}",
                    self.config.system_prompt_prefix.as_deref().unwrap_or(""),
                )
                .replace("{{ORCHESTRATOR_SUBAGENTS}}", subagents_block),
        )
    }

    /// Substitute the static `prompts.md` contract's markers. Behavior is substituted *last*, after
    /// every other marker has been resolved, so text a user wrote in a profile cannot remove or
    /// duplicate the locked tool/memory blocks; markers inside profile text stay literal.
    #[allow(clippy::too_many_arguments)]
    fn fill_base_prompt(
        &self,
        persona: &str,
        behavior: &str,
        persistent_memory_tag: &str,
        active_memories_block: &str,
        skills_protocol: &str,
        mcp_protocol: &str,
        memory_instruction: &str,
        rich_output: &str,
    ) -> Result<String, HarnessError> {
        Ok(crate::prompts::section(crate::prompts::SYSTEM_PROMPT)?
            .replace("{{PERSISTENT_MEMORY_TAG}}", persistent_memory_tag)
            .replace("{{ACTIVE_MEMORIES_BLOCK}}", active_memories_block)
            .replace("{{SKILLS_PROTOCOL}}", skills_protocol)
            .replace("{{MCP_PROTOCOL}}", mcp_protocol)
            .replace("{{RICH_OUTPUT_PROTOCOL}}", rich_output)
            .replace("{{MEMORY_AND_NOTE_PROTOCOL}}\n\n", memory_instruction)
            .replace("{{MEMORY_AND_NOTE_PROTOCOL}}", memory_instruction)
            .replace(
                "{{SYSTEM_PROMPT_PREFIX}}",
                self.config.system_prompt_prefix.as_deref().unwrap_or(""),
            )
            .replace("{{PROFILE_PERSONA}}", persona)
            // A profile cannot re-open a marker the static contract already resolved, and a
            // marker it writes stays literal text.
            .replace("{{PROFILE_BEHAVIOR}}", behavior))
    }

    /// The runtime-environment footer: workspace root, cwd and OS.
    fn runtime_environment(&self) -> String {
        if let Some(ref root) = self.config.workspace_root {
            let cwd = std::env::current_dir().unwrap_or_else(|_| root.clone());
            let mut env = format!(
                "\n\nRUNTIME ENVIRONMENT:\n- Workspace root: {}\n- Current working directory: {}\n- Operating system: {}",
                root.display(),
                cwd.display(),
                std::env::consts::OS,
            );
            if let Ok(rel) = cwd.strip_prefix(root) {
                if !rel.as_os_str().is_empty() {
                    env.push_str(&format!(
                        "\n- Note: Current working directory is inside the workspace at '{}'. All relative tool paths (read_file, search_files, patch, write_file, list_files) resolve against the workspace root ('{}'), NOT the current working directory. Prefix tool paths with '{}/' when targeting files in the current working directory.",
                        rel.display(),
                        root.display(),
                        rel.display(),
                    ));
                } else {
                    env.push_str("\nAll relative tool paths (read_file, search_files, patch, write_file) resolve against the workspace root.");
                }
            } else if let Ok(rel) = root.strip_prefix(&cwd) {
                if !rel.as_os_str().is_empty() {
                    env.push_str(&format!(
                        "\n- Note: Workspace root is at '{}' relative to current working directory. All relative tool paths resolve against the workspace root ('{}').",
                        rel.display(),
                        root.display(),
                    ));
                } else {
                    env.push_str("\nAll relative tool paths (read_file, search_files, patch, write_file) resolve against the workspace root.");
                }
            } else {
                env.push_str(&format!(
                    "\n- Note: All relative tool paths (read_file, search_files, patch, write_file) resolve against the workspace root ('{}'), NOT the current working directory.",
                    root.display(),
                ));
            }
            env.push_str("\nChanging directory with cd in terminal does not persist to native tools or across turns; specify workdir on terminal calls when running commands in subdirectories.");
            env
        } else if let Ok(cwd) = std::env::current_dir() {
            format!(
                "\n\nRUNTIME ENVIRONMENT:\n- Workspace root: {}\n- Current working directory: {}\n- Operating system: {}\nAll relative tool paths (read_file, search_files, patch, write_file) resolve against the workspace root.",
                cwd.display(),
                cwd.display(),
                std::env::consts::OS,
            )
        } else {
            String::new()
        }
    }

    /// The registered MCP tools as a `- name: description` index for the prompt.
    ///
    /// Read from the registry rather than the config so the index is exactly the surface the model
    /// can call: a server that failed to start contributes nothing, and its absence is honest
    /// instead of a promise the tools cannot keep.
    async fn mcp_tools_prompt(&self) -> Result<String, HarnessError> {
        Ok(format_mcp_tools(&self.registry.list_tools().await?))
    }
}

/// The prompt's MCP block body: one `- name: description` line per registered `mcp__` tool, in
/// stable name order, or a fallback line when none are registered.
///
/// Stable order matters: the tool index sits in the request prefix, so an order that shifts between
/// turns is a prompt-cache miss on every turn.
pub(crate) fn format_mcp_tools(tools: &[crate::types::ToolDefinition]) -> String {
    let mut mcp: Vec<&crate::types::ToolDefinition> = tools
        .iter()
        .filter(|tool| tool.name.starts_with("mcp__"))
        .collect();

    if mcp.is_empty() {
        return "(no MCP tools registered — add an `mcp.servers` block to the config to enable them)"
            .to_string();
    }

    mcp.sort_by(|left, right| left.name.cmp(&right.name));
    mcp.into_iter()
        .map(|tool| format!("- {}: {}", tool.name, tool.description))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolDefinition;

    fn tool(name: &str, description: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    /// Native tools never leak into the MCP block, and the block says so plainly when the config
    /// declared no servers.
    #[test]
    fn no_mcp_tools_yields_the_fallback_line() {
        let body = format_mcp_tools(&[tool("read_file", "Read a file"), tool("terminal", "shell")]);
        assert!(body.starts_with("(no MCP tools registered"), "got: {body}");
    }

    /// Only `mcp__` tools are listed, one per line, sorted by name for a stable cache prefix.
    #[test]
    fn mcp_tools_are_listed_sorted_and_namespaced() {
        let body = format_mcp_tools(&[
            tool("read_file", "native"),
            tool("mcp__z__do", "z tool"),
            tool("mcp__a__ping", "a tool"),
        ]);
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(
            lines,
            vec!["- mcp__a__ping: a tool", "- mcp__z__do: z tool",],
            "got: {body}"
        );
    }
}
