pub mod fs;
pub mod shell;
pub mod todo;

use std::sync::Arc;
pub use fs::{ListFilesTool, PatchTool, ReadCache, ReadFileTool, SearchFilesTool, WriteFileTool};
pub use shell::TerminalTool;
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
                registry
                    .register(Arc::new(TerminalTool::new(
                        ws.clone(),
                        config.tools.terminal_timeout_secs,
                        config.tools.terminal_output_max_bytes,
                    )))
                    .await?;
            }
            _ => {}
        }
    }

    Ok(())
}
