//! System-prompt assembly: the `prompts.md` template with memories, skills and the runtime
//! environment substituted in.

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
    /// Build the full system prompt for a turn, with memories, skills, and memory mode instructions substituted.
    pub async fn build_system_prompt(&self, user_input: &str) -> Result<String, HarnessError> {
        self.system_prompt(user_input).await
    }

    /// The system prompt: the `prompts.md` template with the configured prefix and the retrieved
    /// memories substituted in.
    ///
    /// Substitution is a literal `replace`, not `format!`, so the prompt text stays free of format
    /// syntax — a stray brace in the file can never abort a turn.
    pub(super) async fn system_prompt(&self, user_input: &str) -> Result<String, HarnessError> {
        if let Some(ref custom_prompt) = self.config.system_prompt_override {
            return Ok(custom_prompt.clone());
        }

        let relevant_memories = self.memory.retrieve(user_input, 10).await?;
        let memory_prompt = self.memory.format_as_system_prompt(&relevant_memories);
        let skills_prompt = self
            .config
            .skills_summary
            .as_deref()
            .unwrap_or("(no specialized skills installed)");
        let mcp_prompt = self.mcp_tools_prompt().await?;

        let memory_instruction = match self.config.memory_mode {
            crate::config::MemoryMode::Session => {
                format!(
                    "{}\n\n",
                    crate::prompts::section(crate::prompts::MEMORY_SESSION_PROMPT)?
                )
            }
            crate::config::MemoryMode::Background => String::new(),
        };

        let base = crate::prompts::section(crate::prompts::SYSTEM_PROMPT)?
            .replace(
                "{{SYSTEM_PROMPT_PREFIX}}",
                self.config.system_prompt_prefix.as_deref().unwrap_or(""),
            )
            .replace("{{ACTIVE_MEMORIES}}", &memory_prompt)
            .replace("{{AVAILABLE_SKILLS}}", skills_prompt)
            .replace("{{MCP_TOOLS}}", &mcp_prompt)
            .replace("{{MEMORY_AND_NOTE_PROTOCOL}}\n\n", &memory_instruction)
            .replace("{{MEMORY_AND_NOTE_PROTOCOL}}", &memory_instruction);

        let env_info = if let Some(ref root) = self.config.workspace_root {
            let cwd = std::env::current_dir().unwrap_or_else(|_| root.clone());
            format!(
                "\n\nRUNTIME ENVIRONMENT:\n- Workspace root: {}\n- Current working directory: {}\n- Operating system: {}\nAll relative tool paths (read_file, search_files, patch, write_file) resolve against the workspace root. Changing directory with cd in terminal does not persist to native tools or across turns.",
                root.display(),
                cwd.display(),
                std::env::consts::OS,
            )
        } else if let Ok(cwd) = std::env::current_dir() {
            format!(
                "\n\nRUNTIME ENVIRONMENT:\n- Workspace root: {}\n- Current working directory: {}\n- Operating system: {}\nAll relative tool paths (read_file, search_files, patch, write_file) resolve against the workspace root.",
                cwd.display(),
                cwd.display(),
                std::env::consts::OS,
            )
        } else {
            String::new()
        };

        Ok(format!("{}{}", base, env_info))
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
