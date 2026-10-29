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
}
