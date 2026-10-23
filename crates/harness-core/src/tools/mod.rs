pub mod delegate;
pub mod fs;
pub mod netguard;
pub mod process;
pub mod sandbox;
pub mod shell;
pub mod todo;

pub use delegate::DelegateTaskTool;
pub use fs::{ListFilesTool, PatchTool, ReadCache, ReadFileTool, SearchFilesTool, WriteFileTool};
pub use process::ProcessTool;
pub use shell::{
    check_sudo_nopasswd, contains_sudo, is_sudo_auth_failure, rewrite_sudo_command, SudoPrompter,
    TerminalTool,
};
use std::sync::{Arc, Mutex};
pub use todo::{read_snapshot, TodoItem, TodoListTool, TodoScope, TodoSnapshot, TodoStatus};

use crate::config::Config;
use crate::error::HarnessError;
use crate::traits::ToolRegistry;

/// Registers the filesystem and shell tools into the registry based on config.
pub async fn register_core_tools(
    registry: &Arc<dyn ToolRegistry>,
    config: &Config,
    read_cache: ReadCache,
    todo_scope: TodoScope,
) -> Result<(), HarnessError> {
    register_core_tools_with_sudo(registry, config, read_cache, None, None, todo_scope)
        .await
        .map(|_| ())
}

/// Registers the filesystem and shell tools with optional session sudo caching and prompting.
///
/// The `terminal` and `process` tools MUST share one [`BackgroundTaskQueue`]: `terminal` spawns
/// background jobs into it and `process` polls it. Constructing a queue per tool makes the two
/// disagree — `terminal(background=true)` would report a `job_id` that `process poll` cannot find.
/// The queue that was used is returned so hosts (e.g. `/jobs`, the task viewer) observe the same
/// registry the tools read.
pub async fn register_core_tools_with_sudo(
    registry: &Arc<dyn ToolRegistry>,
    config: &Config,
    read_cache: ReadCache,
    sudo_cache: Option<Arc<Mutex<Option<String>>>>,
    sudo_prompter: Option<Arc<dyn SudoPrompter>>,
    todo_scope: TodoScope,
) -> Result<Arc<crate::tasks::BackgroundTaskQueue>, HarnessError> {
    // One queue shared by `terminal` (producer) and `process` (consumer) — see the doc comment.
    let bg_queue = Arc::new(
        crate::tasks::BackgroundTaskQueue::new(config.paths.jobs_dir.clone())
            .with_network_policy(config.tools.network),
    );
    register_core_tools_shared_queue(
        registry,
        config,
        read_cache,
        sudo_cache,
        sudo_prompter,
        bg_queue,
        todo_scope,
    )
    .await
}

/// Registers the filesystem and shell tools into an existing [`BackgroundTaskQueue`].
///
/// Same contract as [`register_core_tools_with_sudo`]; the caller supplies the queue the rest of
/// the host already holds (so `/jobs` and the task viewer see the exact registry the tools use).
pub async fn register_core_tools_shared_queue(
    registry: &Arc<dyn ToolRegistry>,
    config: &Config,
    read_cache: ReadCache,
    sudo_cache: Option<Arc<Mutex<Option<String>>>>,
    sudo_prompter: Option<Arc<dyn SudoPrompter>>,
    bg_queue: Arc<crate::tasks::BackgroundTaskQueue>,
    todo_scope: TodoScope,
) -> Result<Arc<crate::tasks::BackgroundTaskQueue>, HarnessError> {
    let ws = config.paths.workspace_root.clone();
    // The sandbox's reach, in one place: every filesystem and shell tool gets the same roots, so
    // `read_file` and `terminal` cannot disagree about what "outside the workspace" means.
    let extra_roots = config.paths.extra_roots.clone();

    for tool_name in &config.tools.enabled {
        match tool_name.as_str() {
            "read_file" => {
                registry
                    .register(Arc::new(
                        ReadFileTool::new(
                            ws.clone(),
                            config.tools.read_max_chars,
                            read_cache.clone(),
                        )
                        .with_extra_roots(extra_roots.clone()),
                    ))
                    .await?;
            }
            "write_file" => {
                registry
                    .register(Arc::new(
                        WriteFileTool::new(ws.clone(), read_cache.clone())
                            .with_extra_roots(extra_roots.clone()),
                    ))
                    .await?;
            }
            "patch" => {
                registry
                    .register(Arc::new(
                        PatchTool::new(ws.clone(), read_cache.clone())
                            .with_extra_roots(extra_roots.clone()),
                    ))
                    .await?;
            }
            "search_files" => {
                registry
                    .register(Arc::new(
                        SearchFilesTool::new(ws.clone()).with_extra_roots(extra_roots.clone()),
                    ))
                    .await?;
            }
            "list_files" => {
                registry
                    .register(Arc::new(
                        ListFilesTool::new(ws.clone()).with_extra_roots(extra_roots.clone()),
                    ))
                    .await?;
            }
            "todo_list" => {
                // The list belongs to the session the scope points at, not to the
                // workspace every tool else is rooted in.
                registry
                    .register(Arc::new(TodoListTool::new(todo_scope.clone())))
                    .await?;
            }
            "terminal" => {
                let mut term = TerminalTool::new(
                    ws.clone(),
                    config.tools.terminal_timeout_secs,
                    config.tools.terminal_output_max_bytes,
                )
                .with_background_queue(bg_queue.clone())
                .with_extra_roots(extra_roots.clone())
                .with_network_policy(config.tools.network)
                .with_prefix(config.tools.terminal_prefix.clone())
                .with_rtk_rewrite(config.tools.rtk_rewrite);

                if let Some(cache) = sudo_cache.clone() {
                    term = term.with_sudo_cache(cache);
                }
                if let Some(prompter) = sudo_prompter.clone() {
                    term = term.with_sudo_prompter(prompter);
                }
                registry.register(Arc::new(term)).await?;
            }
            "process" => {
                registry
                    .register(Arc::new(ProcessTool::new(bg_queue.clone())))
                    .await?;
            }
            _ => {}
        }
    }

    Ok(bg_queue)
}
