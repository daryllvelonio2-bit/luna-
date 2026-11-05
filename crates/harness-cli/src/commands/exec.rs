//! `luna exec`: one prompt, one turn, machine-readable output.
//!
//! The interactive chat is built for a person at a terminal — live regions, redraws, prompts. None
//! of that belongs in a CI job. `exec` runs the same agent with the same tool surface and prints
//! either the answer (plain) or a JSON event stream (`--json`), so a script can drive it:
//!
//! ```bash
//! luna exec "which module owns the approval gate?"                 # answer on stdout
//! luna exec --json "fix the failing test" | jq -c 'select(.type=="result")'
//! echo "summarise PROGRESS.md" | luna exec -                         # prompt from stdin
//! ```
//!
//! **Headless is fail-closed.** There is nobody to answer a prompt, so a mutating call that no
//! `allow` rule covers is denied rather than approved by default — `--yes` (or
//! `tools.approval=allow`) is the way to say otherwise. A tool that quietly approved everything
//! because stdin was not a terminal would make `exec` a way around the gate rather than through it.

use std::io::{Read, Write};
use std::sync::Arc;

use harness_core::approval::{AllowAll, ApprovalMode, Approver};
use harness_core::{
    approval, Agent, AgentConfig, AgentEvent, Config, DefaultToolRegistry, HarnessError,
    PermissionEngine, ReplayNotice, SessionBinding, SessionStore, ToolRegistry,
};
use harness_memory::{ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool};
use harness_notes::{
    DeleteNoteTool, FileNotesEngine, ListNotesTool, ReadNoteTool, SaveNoteTool, SearchNotesTool,
    UpdateNoteTool,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use super::approval::approval_settings;
use super::chat::ChatAgent;
use super::sessions::{open_store, resolve_session, SessionSelection};

/// How `luna exec` was invoked.
pub struct ExecOptions {
    /// The prompt, or `None` to read it from stdin.
    pub prompt: Option<String>,
    /// Emit NDJSON events instead of a plain answer.
    pub json: bool,
    /// Which session to run in: resume one (`--session`), fork one, continue the latest, or start
    /// a fresh one. Mirrors `luna chat`.
    pub selection: SessionSelection,
    /// Approve every mutating call: the headless equivalent of `/yes` in the chat.
    pub yes: bool,
    /// Exit non-zero when any tool call failed, even if the turn itself completed.
    pub strict: bool,
}

/// One JSON line, written and flushed immediately so a consumer can read as it happens.
fn emit(line: &serde_json::Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

pub async fn run_exec(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    options: ExecOptions,
) -> Result<(), HarnessError> {
    let prompt = read_prompt(options.prompt)?;
    if prompt.trim().is_empty() {
        return Err(HarnessError::ConfigurationError(
            "luna exec: empty prompt".to_string(),
        ));
    }

    let store = Arc::new(open_store(&config)?);
    let session_id = resolve_session(store.as_ref(), options.selection).await?;

    // The session carries its model, so `--session` resumes on the model it was using.
    let record = store.get(&session_id).await?;
    let (provider_id, model_id) = match record.model.clone() {
        Some((provider, model)) => (provider, model),
        None => (config.model.provider.clone(), config.model.default.clone()),
    };
    if provider_id.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "No LLM provider configured. Run 'luna auth' or pass '--provider <name>'.".to_string(),
        ));
    }
    store
        .set_model(&session_id, &provider_id, &model_id)
        .await?;

    let provider = Arc::new(GenericProviderClient::new(
        provider_id.clone(),
        model_id.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&provider_id),
    )?);

    let memory = Arc::new(SqliteMemoryEngine::new(Some(
        config.paths.memory_db.clone(),
    ))?);
    let notes = Arc::new(FileNotesEngine::new(
        config.paths.notes_dir.clone(),
        Some(config.paths.notes_db.clone()),
    )?);

    // The tool surface matches the interactive chat: a headless run that could do less than the
    // chat could would be a different agent wearing the same name.
    let background_mode = config.memory.mode.is_background();
    let registry = Arc::new(DefaultToolRegistry::new());
    if !background_mode {
        registry
            .register(Arc::new(SaveMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(UpdateMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(ForgetMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(SaveNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(UpdateNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(DeleteNoteTool::new(notes.clone())))
            .await?;
    }
    registry
        .register(Arc::new(SearchNotesTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(ListNotesTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(ReadNoteTool::new(notes.clone())))
        .await?;

    let mut all_skills = Vec::new();
    let _ = std::fs::create_dir_all(&config.paths.skills_dir);
    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir)
    {
        all_skills.extend(skills);
    }
    let workspace_skills = config.paths.workspace_root.join(".harness/skills");
    if workspace_skills.exists() {
        if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&workspace_skills) {
            all_skills.extend(skills);
        }
    }
    // The skills the harness ships with, including the desktop skill.
    all_skills.extend(crate::commands::desktop::builtin_skills());
    if !all_skills.is_empty() {
        let skills_arc = Arc::new(all_skills);
        registry
            .register(Arc::new(harness_plugins::SkillViewTool::new(
                skills_arc.clone(),
            )))
            .await?;
        registry
            .register(Arc::new(harness_plugins::SkillsListTool::new(skills_arc)))
            .await?;
    }
    // The desktop tools, and the skill that documents them.
    crate::commands::desktop::tools::register(registry.as_ref()).await?;

    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    let bg_queue = Arc::new(
        harness_core::tasks::BackgroundTaskQueue::new(config.paths.jobs_dir.clone())
            .with_network_policy(config.tools.network),
    );
    // The todo list belongs to the session this run belongs to.
    let todo_scope =
        harness_core::tools::TodoScope::new(config.paths.todos_dir.clone(), &session_id);
    harness_core::tools::register_core_tools_shared_queue(
        &reg_dyn,
        &config,
        harness_core::tools::ReadCache::new(),
        None,
        None,
        bg_queue,
        todo_scope,
    )
    .await?;

    // Same undo net as the chat: a headless edit is still an edit.
    let checkpoints = Arc::new(harness_core::CheckpointController::new(
        config.paths.checkpoints_dir.clone(),
        config.paths.workspace_root.clone(),
        &config.checkpoints,
    ));
    registry
        .register_middleware(Arc::new(harness_core::CheckpointMiddleware::new(
            checkpoints.clone(),
        )))
        .await;

    let usage = Arc::new(super::usage::UsageRecorder::from_config(&config));
    let permissions = PermissionEngine::validate(config.tools.permissions.clone())?;
    let approver = headless_approver(&config, options.yes);

    let mut agent_cfg = AgentConfig {
        require_hitl_for_mutation: config.agent.require_hitl_for_mutation,
        ..AgentConfig::default()
    };
    agent_cfg.system_prompt_prefix = config.agent.system_prompt_prefix.clone();
    agent_cfg.workspace_root = Some(config.paths.workspace_root.clone());
    agent_cfg.memory_mode = config.memory.mode;
    if let Some(skills_summary) = skills_summary(&config) {
        agent_cfg.skills_summary = Some(skills_summary);
    }

    // Hooks are part of the agent, not part of the REPL: `luna exec` is the path CI and scripts
    // take, and a guardrail that only arms in an interactive session is a guardrail that is off
    // exactly where nobody is watching. Same fail-closed loader as `chat`.
    let hooks = Arc::new(harness_core::HookRunner::load(
        &config.paths.workspace_root,
        &config.hooks,
    )?);

    let mut agent: ChatAgent = Agent::new(provider, memory, notes, registry, agent_cfg)
        .with_permissions(permissions)
        .with_hooks(hooks)
        .with_checkpoints(checkpoints)
        .with_approval(approver, approval_settings(&config))
        .with_session(SessionBinding {
            store: store.clone(),
            session_id: session_id.clone(),
            resume_max_messages: config.session.resume_max_messages,
        });

    if options.json {
        emit(&serde_json::json!({
            "type": "start",
            "session": session_id,
            "provider": provider_id,
            "model": model_id,
            "prompt": prompt,
        }));
        let observer: harness_core::AgentObserver = Arc::new(|event| {
            let line = match event {
                AgentEvent::ReasoningDelta(text) => {
                    serde_json::json!({"type": "reasoning", "text": text})
                }
                AgentEvent::ToolExecuting { name, arguments } => {
                    serde_json::json!({"type": "tool_call", "name": name, "arguments": arguments})
                }
                AgentEvent::ToolExecuted {
                    name,
                    output,
                    is_error,
                } => serde_json::json!({
                    "type": "tool_result",
                    "name": name,
                    "ok": !is_error,
                    "output": output,
                }),
                AgentEvent::Steered { text } => {
                    serde_json::json!({"type": "steered", "text": text})
                }
                AgentEvent::ContextTokensUpdated { tokens_used, .. } => {
                    serde_json::json!({"type": "context_tokens", "tokens_used": tokens_used})
                }
                // Compaction is reported by the turn's own output in headless mode, where there is
                // no animation to drive.
                _ => return,
            };
            emit(&line);
        });
        agent.set_observer(observer);
    } else {
        // Plain mode: the trace goes to stderr, so `$(luna exec …)` captures only the answer.
        let observer: harness_core::AgentObserver = Arc::new(|event| match event {
            AgentEvent::ToolExecuting { name, .. } => eprintln!("→ {name}"),
            AgentEvent::ToolExecuted {
                name,
                is_error,
                output,
            } => {
                let mark = if is_error { "✖" } else { "✔" };
                let first_line = output.lines().next().unwrap_or("").trim();
                let short: String = first_line.chars().take(120).collect();
                eprintln!("{mark} {name} {short}");
            }
            _ => {}
        });
        agent.set_observer(observer);
    }

    // `session_start` before the turn and `session_end` after it, mirroring the REPL.
    agent
        .emit_hook_event(harness_core::HookEvent::SessionStart, None)
        .await?;
    let outcome = agent
        .run_turn(
            &crate::commands::prompt_images::inline_image_paths(&prompt),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    // `session_end` fires even when the turn failed: a hook that tears down a container or
    // flushes a log must not be skipped because the model errored.
    if let Err(error) = agent
        .emit_hook_event(harness_core::HookEvent::SessionEnd, None)
        .await
    {
        tracing::warn!("session_end hook refused: {}", error);
    }

    if let Ok(outcome) = &outcome {
        usage
            .record(
                &provider_id,
                &model_id,
                Some(&session_id),
                outcome,
                harness_core::usage::SOURCE_EXEC,
            )
            .await;
    }

    match outcome {
        Ok(outcome) => {
            let failed_tools = outcome
                .tool_calls
                .iter()
                .filter(|call| call.is_error)
                .count();
            let denied = outcome.tool_calls.iter().filter(|call| call.denied).count();

            if options.json {
                // The turn completed; `ok` says so. Which tools errored is in `tool_calls`, where
                // the caller can see that a failing command or a sandbox refusal was the task's
                // point rather than a broken run — a turn that ends normally is not a failed run.
                emit(&serde_json::json!({
                    "type": "result",
                    "ok": true,
                    "answer": outcome.reply,
                    "reasoning": outcome.reasoning,
                    "session": session_id,
                    "tool_calls": outcome.tool_calls.iter().map(|call| serde_json::json!({
                        "name": call.name,
                        "ok": !call.is_error,
                        "denied": call.denied,
                        "output": call.output,
                    })).collect::<Vec<_>>(),
                    "replayed": outcome.replayed.as_ref().map(|replay: &ReplayNotice| replay.replayed),
                    "usage": outcome.usage.as_ref().map(|usage| serde_json::json!({
                        "prompt_tokens": usage.prompt_tokens,
                        "completion_tokens": usage.completion_tokens,
                        "total_tokens": usage.total_tokens,
                    })),
                    "duration_ms": outcome.duration_ms,
                }));
            } else {
                println!("{}", outcome.reply);
            }

            // A denial always fails the run, `--strict` or not: the caller asked for work that
            // policy refused, and a green exit status would claim it happened.
            if denied > 0 {
                return Err(HarnessError::ApprovalRejected(format!(
                    "luna exec: {denied} tool call(s) were denied — see the tool results above"
                )));
            }
            if options.strict && failed_tools > 0 {
                return Err(HarnessError::StrictFailure(format!(
                    "luna exec --strict: {failed_tools} tool call(s) failed"
                )));
            }
            Ok(())
        }
        Err(error) => {
            if options.json {
                emit(&serde_json::json!({
                    "type": "result",
                    "ok": false,
                    "error": error.to_string(),
                    "session": session_id,
                }));
            }
            Err(error)
        }
    }
}

/// Headless approval is fail-closed unless the caller said `--yes` or the config says `allow`.
///
/// Deliberately *not* `build_approver`, which treats an unattended `Ask` as "approve everything":
/// correct for a chat someone launched, wrong as the default for a job nobody is watching.
fn headless_approver(config: &Config, yes: bool) -> Arc<dyn Approver> {
    if yes || config.tools.approval == ApprovalMode::Allow {
        Arc::new(AllowAll)
    } else {
        eprintln!(
            "luna exec: mutations are denied unless a tools.permissions rule allows them — pass --yes to approve all"
        );
        Arc::new(approval::DenyAll)
    }
}

fn read_prompt(prompt: Option<String>) -> Result<String, HarnessError> {
    match prompt.as_deref() {
        Some("-") | None => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|error| {
                    HarnessError::Io(format!("cannot read the prompt from stdin: {error}"))
                })?;
            Ok(buffer)
        }
        Some(text) => Ok(text.to_string()),
    }
}

fn skills_summary(config: &Config) -> Option<String> {
    let mut all = Vec::new();
    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir)
    {
        all.extend(skills);
    }
    let workspace_skills = config.paths.workspace_root.join(".harness/skills");
    if workspace_skills.exists() {
        if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&workspace_skills) {
            all.extend(skills);
        }
    }
    if all.is_empty() {
        return None;
    }
    Some(
        all.iter()
            .map(|skill| format!("- {}: {}", skill.name, skill.description))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}
