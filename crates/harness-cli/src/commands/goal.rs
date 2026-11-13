//! `luna goal`: run an autonomous multi-turn loop headlessly until the goal finishes or stops.
//!
//! Mirrors [`super::exec`] for session resolution, tool registration, fail-closed approvals,
//! lifecycle hooks, and NDJSON output, while driving [`harness_core::GoalRunState`] across turns.

use std::sync::Arc;

use harness_core::{
    Agent, GoalOutcome, GoalRunState, GoalVerdict, HarnessError, MemoryEngine, ModelProvider,
    NotesEngine, ToolRegistry,
};
use harness_providers::{CredentialManager, ModelDiscovery};
use tokio_util::sync::CancellationToken;

use super::exec::{
    build_headless_agent, emit, install_exec_observer, read_prompt, resolve_exec_session,
};
use super::sessions::SessionSelection;
use super::usage::UsageRecorder;

/// How `luna goal` was invoked.
pub struct GoalOptions {
    /// Objective text, or `None` / `"-"` to read it from stdin.
    pub objective: Option<String>,
    /// Optional iteration limit overriding `goal.max_iterations`.
    pub max_iterations: Option<usize>,
    /// Emit NDJSON events instead of plain output.
    pub json: bool,
    /// Which session to run in.
    pub selection: SessionSelection,
    /// Approve every mutating tool call without asking.
    pub yes: bool,
    /// Exit non-zero if any tool call failed or the goal did not complete.
    pub strict: bool,
}

/// Entrypoint for `luna goal`.
pub async fn run_goal(
    config: harness_core::Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    options: GoalOptions,
) -> Result<(), HarnessError> {
    let objective = read_prompt(options.objective.clone())?;
    let max_iterations = options.max_iterations.unwrap_or(config.goal.max_iterations);
    let state = GoalRunState::new(objective, max_iterations)?;

    let (store, session_id, provider_id, model_id) =
        resolve_exec_session(&config, options.selection).await?;

    let mut agent = build_headless_agent(
        &config,
        &cred_manager,
        &discovery,
        &store,
        &session_id,
        &provider_id,
        &model_id,
        options.yes,
    )
    .await?;

    install_exec_observer(&mut agent, options.json);
    let usage = Arc::new(UsageRecorder::from_config(&config));

    let cancel = CancellationToken::new();
    let signal_cancel = cancel.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancel.cancel();
        }
    });

    if options.json {
        emit(&serde_json::json!({
            "type": "goal_start",
            "session": session_id,
            "provider": provider_id,
            "model": model_id,
            "objective": state.objective,
            "max_iterations": state.max_iterations,
        }));
    } else {
        eprintln!(
            "goal: {} (max {} iterations)",
            state.objective, state.max_iterations
        );
    }

    let loop_result = drive_goal_loop(
        &mut agent,
        state,
        &usage,
        &provider_id,
        &model_id,
        &session_id,
        options.json,
        &cancel,
    )
    .await;

    signal_task.abort();

    match loop_result {
        Ok(stats) => report_goal_outcome(&stats, options.json, options.strict, &session_id),
        Err(error) => {
            if options.json {
                emit(&serde_json::json!({
                    "type": "goal_result",
                    "ok": false,
                    "status": "error",
                    "error": error.to_string(),
                    "session": session_id,
                }));
            }
            Err(error)
        }
    }
}

struct GoalRunStats {
    outcome: GoalOutcome,
    last_reply: String,
    failed_tools: usize,
    denied_tools: usize,
}

#[allow(clippy::too_many_arguments)]
async fn drive_goal_loop<P, M, N, R>(
    agent: &mut Agent<P, M, N, R>,
    mut state: GoalRunState,
    usage: &UsageRecorder,
    provider_id: &str,
    model_id: &str,
    session_id: &str,
    json: bool,
    cancel: &CancellationToken,
) -> Result<GoalRunStats, HarnessError>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    R: ToolRegistry + 'static,
{
    agent
        .emit_hook_event(harness_core::HookEvent::SessionStart, None)
        .await?;

    let mut last_reply = String::new();
    let mut failed_tools = 0usize;
    let mut denied_tools = 0usize;

    let loop_outcome: Result<GoalOutcome, HarnessError> = loop {
        if cancel.is_cancelled() {
            break Ok(GoalOutcome::Stopped {
                iterations: state.iteration.saturating_sub(1),
            });
        }

        let prompt = match state.prompt_for_current_iteration() {
            Ok(prompt) => prompt,
            Err(error) => break Err(error),
        };
        let prompt = crate::commands::prompt_images::inline_image_paths(&prompt);

        if json {
            emit(&serde_json::json!({
                "type": "iteration_start",
                "iteration": state.iteration,
                "max_iterations": state.max_iterations,
                "session": session_id,
            }));
        } else {
            eprintln!("↻ iteration {}/{}", state.iteration, state.max_iterations);
        }

        match agent.run_turn(&prompt, cancel.child_token()).await {
            Ok(turn) => {
                usage
                    .record(
                        provider_id,
                        model_id,
                        Some(session_id),
                        &turn,
                        harness_core::usage::SOURCE_GOAL,
                    )
                    .await;

                let turn_failed = turn.tool_calls.iter().filter(|c| c.is_error).count();
                let turn_denied = turn.tool_calls.iter().filter(|c| c.denied).count();
                failed_tools += turn_failed;
                denied_tools += turn_denied;
                last_reply = turn.reply.clone();

                let verdict = GoalRunState::evaluate_reply(&turn.reply);
                if json {
                    let verdict_label = match &verdict {
                        GoalVerdict::Continue => "continue",
                        GoalVerdict::Complete(_) => "complete",
                        GoalVerdict::Blocked(_) => "blocked",
                    };
                    emit(&serde_json::json!({
                        "type": "iteration_end",
                        "iteration": state.iteration,
                        "verdict": verdict_label,
                        "reply": turn.reply,
                        "duration_ms": turn.duration_ms,
                    }));
                }

                match verdict {
                    GoalVerdict::Complete(summary) => {
                        if !summary.is_empty() {
                            last_reply = summary.clone();
                        }
                        break Ok(GoalOutcome::Completed {
                            iterations: state.iteration,
                            summary,
                        });
                    }
                    GoalVerdict::Blocked(reason) => {
                        break Ok(GoalOutcome::Blocked {
                            iterations: state.iteration,
                            reason,
                        });
                    }
                    GoalVerdict::Continue => {
                        if !state.advance() {
                            break Ok(GoalOutcome::MaxIterationsReached {
                                iterations: state.iteration,
                            });
                        }
                    }
                }
            }
            Err(HarnessError::Cancelled) => {
                break Ok(GoalOutcome::Stopped {
                    iterations: state.iteration,
                });
            }
            Err(error) => break Err(error),
        }
    };

    if let Err(error) = agent
        .emit_hook_event(harness_core::HookEvent::SessionEnd, None)
        .await
    {
        tracing::warn!("session_end hook refused: {}", error);
    }

    let outcome = loop_outcome?;
    Ok(GoalRunStats {
        outcome,
        last_reply,
        failed_tools,
        denied_tools,
    })
}

fn report_goal_outcome(
    stats: &GoalRunStats,
    json: bool,
    strict: bool,
    session_id: &str,
) -> Result<(), HarnessError> {
    let (ok, status, iterations, detail) = match &stats.outcome {
        GoalOutcome::Completed {
            iterations,
            summary,
        } => (true, "completed", *iterations, Some(summary.clone())),
        GoalOutcome::Blocked { iterations, reason } => {
            (false, "blocked", *iterations, Some(reason.clone()))
        }
        GoalOutcome::MaxIterationsReached { iterations } => {
            (false, "max_iterations", *iterations, None)
        }
        GoalOutcome::Stopped { iterations } => (false, "stopped", *iterations, None),
    };

    if json {
        emit(&serde_json::json!({
            "type": "goal_result",
            "ok": ok,
            "status": status,
            "iterations": iterations,
            "answer": stats.last_reply,
            "detail": detail,
            "session": session_id,
            "failed_tools": stats.failed_tools,
            "denied_tools": stats.denied_tools,
        }));
    } else {
        if !stats.last_reply.trim().is_empty() {
            println!("{}", stats.last_reply);
        }
        match &stats.outcome {
            GoalOutcome::Completed { iterations, .. } => {
                eprintln!("✔ goal completed in {iterations} iteration(s)");
            }
            GoalOutcome::Blocked { iterations, reason } => {
                eprintln!("✖ goal blocked at iteration {iterations}: {reason}");
            }
            GoalOutcome::MaxIterationsReached { iterations } => {
                eprintln!("⏸ goal stopped after reaching max iterations ({iterations})");
            }
            GoalOutcome::Stopped { iterations } => {
                eprintln!("✖ goal stopped at iteration {iterations}");
            }
        }
    }

    if stats.denied_tools > 0 {
        return Err(HarnessError::ApprovalRejected(format!(
            "luna goal: {} tool call(s) were denied",
            stats.denied_tools
        )));
    }
    if strict && stats.failed_tools > 0 {
        return Err(HarnessError::StrictFailure(format!(
            "luna goal --strict: {} tool call(s) failed",
            stats.failed_tools
        )));
    }
    if strict && !ok {
        return Err(HarnessError::StrictFailure(format!(
            "luna goal --strict: goal ended with status '{status}'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use harness_core::{
        AgentConfig, CompletionOptions, Config, DefaultToolRegistry, MemoryRecord, Message, Note,
        ToolDefinition,
    };
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct ScriptedProvider {
        replies: Mutex<VecDeque<String>>,
    }

    impl ScriptedProvider {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().map(str::to_string).collect()),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        fn provider_id(&self) -> &str {
            "scripted"
        }

        async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
            Ok(vec!["scripted-model".to_string()])
        }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _options: &CompletionOptions,
        ) -> Result<Message, HarnessError> {
            let text = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| "<goal_complete>".to_string());
            Ok(Message::assistant(text))
        }

        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
            Ok(Vec::new())
        }
    }

    struct NoopMemory;
    #[async_trait]
    impl MemoryEngine for NoopMemory {
        async fn retrieve(&self, _: &str, _: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
            Ok(Vec::new())
        }
        async fn retrieve_cards(
            &self,
            _: &str,
            _: usize,
        ) -> Result<Vec<MemoryRecord>, HarnessError> {
            Ok(Vec::new())
        }
        async fn save(&self, _: &MemoryRecord) -> Result<(), HarnessError> {
            Ok(())
        }
        async fn update(&self, _: &str, _: &MemoryRecord) -> Result<(), HarnessError> {
            Ok(())
        }
        async fn forget(&self, _: &str) -> Result<(), HarnessError> {
            Ok(())
        }
        async fn add_note_pointer(&self, _: &str, _: &str) -> Result<(), HarnessError> {
            Ok(())
        }
        fn format_as_system_prompt(&self, _: &[MemoryRecord]) -> String {
            String::new()
        }
    }

    struct NoopNotes;
    #[async_trait]
    impl NotesEngine for NoopNotes {
        async fn save_note(&self, _: &str, _: &str, _: &[String]) -> Result<Note, HarnessError> {
            Err(HarnessError::Io("noop".to_string()))
        }
        async fn search_notes(&self, _: &str, _: usize) -> Result<Vec<Note>, HarnessError> {
            Ok(Vec::new())
        }
        async fn list_notes(&self, _: usize, _: usize) -> Result<Vec<Note>, HarnessError> {
            Ok(Vec::new())
        }
        async fn read_note(&self, _: &str) -> Result<Note, HarnessError> {
            Err(HarnessError::Io("noop".to_string()))
        }
        async fn update_note(
            &self,
            _: &str,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&[String]>,
        ) -> Result<Note, HarnessError> {
            Err(HarnessError::Io("noop".to_string()))
        }
        async fn delete_note(&self, _: &str) -> Result<bool, HarnessError> {
            Ok(false)
        }
    }

    fn test_agent(
        replies: Vec<&str>,
    ) -> Agent<ScriptedProvider, NoopMemory, NoopNotes, DefaultToolRegistry> {
        Agent::new(
            Arc::new(ScriptedProvider::new(replies)),
            Arc::new(NoopMemory),
            Arc::new(NoopNotes),
            Arc::new(DefaultToolRegistry::new()),
            AgentConfig::default(),
        )
    }

    fn disabled_usage() -> UsageRecorder {
        let mut cfg = Config::default();
        cfg.usage.enabled = false;
        UsageRecorder::from_config(&cfg)
    }

    #[tokio::test]
    async fn headless_goal_loop_iterates_until_complete() {
        let mut agent = test_agent(vec![
            "Step 1 done, running checks next.",
            "All checks pass.\n<goal_complete>",
        ]);
        let state = GoalRunState::new("verify tests", 5).unwrap();
        let stats = drive_goal_loop(
            &mut agent,
            state,
            &disabled_usage(),
            "scripted",
            "scripted-model",
            "s1",
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(
            stats.outcome,
            GoalOutcome::Completed {
                iterations: 2,
                summary: "All checks pass.".to_string(),
            }
        );
        assert_eq!(stats.last_reply, "All checks pass.");
    }

    #[tokio::test]
    async fn headless_goal_loop_stops_on_blocker_or_max_iterations() {
        let mut blocked_agent = test_agent(vec!["<goal_blocked: missing token>"]);
        let blocked = drive_goal_loop(
            &mut blocked_agent,
            GoalRunState::new("deploy", 4).unwrap(),
            &disabled_usage(),
            "scripted",
            "scripted-model",
            "s2",
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            blocked.outcome,
            GoalOutcome::Blocked {
                iterations: 1,
                reason: "missing token".to_string(),
            }
        );

        let mut capped_agent = test_agent(vec!["step 1", "step 2"]);
        let capped = drive_goal_loop(
            &mut capped_agent,
            GoalRunState::new("keep going", 2).unwrap(),
            &disabled_usage(),
            "scripted",
            "scripted-model",
            "s3",
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            capped.outcome,
            GoalOutcome::MaxIterationsReached { iterations: 2 }
        );
    }
}
