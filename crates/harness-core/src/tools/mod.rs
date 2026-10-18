pub mod fs;
pub mod process;
pub mod shell;
pub mod todo;

use std::sync::{Arc, Mutex};
pub use fs::{ListFilesTool, PatchTool, ReadCache, ReadFileTool, SearchFilesTool, WriteFileTool};
pub use process::ProcessTool;
pub use shell::{
    check_sudo_nopasswd, contains_sudo, is_sudo_auth_failure, rewrite_sudo_command, SudoPrompter,
    TerminalTool,
};
pub use todo::TodoListTool;

use crate::config::Config;
use crate::error::HarnessError;
use crate::traits::ToolRegistry;

/// Registers the filesystem and shell tools into the registry based on config.
pub async fn register_core_tools(
    registry: &Arc<dyn ToolRegistry>,
    config: &Config,
    read_cache: ReadCache,
) -> Result<(), HarnessError> {
    register_core_tools_with_sudo(registry, config, read_cache, None, None).await
}

/// Registers the filesystem and shell tools with optional session sudo caching and prompting.
pub async fn register_core_tools_with_sudo(
    registry: &Arc<dyn ToolRegistry>,
    config: &Config,
    read_cache: ReadCache,
    sudo_cache: Option<Arc<Mutex<Option<String>>>>,
    sudo_prompter: Option<Arc<dyn SudoPrompter>>,
) -> Result<(), HarnessError> {
    let ws = config.paths.workspace_root.clone();

    for tool_name in &config.tools.enabled {
        match tool_name.as_str() {
            "read_file" => {
                registry
                    .register(Arc::new(ReadFileTool::new(
                        ws.clone(),
                        config.tools.read_max_chars,
                        read_cache.clone(),
                    )))
                    .await?;
            }
            "write_file" => {
                registry
                    .register(Arc::new(WriteFileTool::new(ws.clone(), read_cache.clone())))
                    .await?;
            }
            "patch" => {
                registry
                    .register(Arc::new(PatchTool::new(ws.clone(), read_cache.clone())))
                    .await?;
            }
            "search_files" => {
                registry
                    .register(Arc::new(SearchFilesTool::new(ws.clone())))
                    .await?;
            }
            "list_files" => {
                registry
                    .register(Arc::new(ListFilesTool::new(ws.clone())))
                    .await?;
            }
            "todo_list" => {
                registry
                    .register(Arc::new(TodoListTool::new(ws.clone())))
                    .await?;
            }
            "terminal" => {
                let bg_queue = Arc::new(crate::tasks::BackgroundTaskQueue::new(config.paths.jobs_dir.clone()));
                let mut term = TerminalTool::new(
                    ws.clone(),
                    config.tools.terminal_timeout_secs,
                    config.tools.terminal_output_max_bytes,
                )
                .with_background_queue(bg_queue);

                if let Some(cache) = sudo_cache.clone() {
                    term = term.with_sudo_cache(cache);
                }
                if let Some(prompter) = sudo_prompter.clone() {
                    term = term.with_sudo_prompter(prompter);
                }
                registry.register(Arc::new(term)).await?;
            }
            "process" => {
                let bg_queue = Arc::new(crate::tasks::BackgroundTaskQueue::new(config.paths.jobs_dir.clone()));
                registry.register(Arc::new(ProcessTool::new(bg_queue))).await?;
            }
            _ => {}
        }
    }

    Ok(())
}
