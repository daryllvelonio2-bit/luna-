//! Structured application event stream for headless, TUI, and desktop GUI consumers.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use harness_core::tools::delegate::{SubagentActivity, SubagentStatus, SubagentTaskEvent};
use harness_core::{AgentEvent, ApprovalRequest, ClarifyRequest};

/// Unified event emitted by a running Luna session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppEvent {
    TurnStarted {
        session_id: String,
        query: String,
    },
    TurnFinished {
        session_id: String,
        reply: String,
        duration_ms: u64,
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    TurnFailed {
        session_id: String,
        error: String,
    },
    ReasoningDelta {
        session_id: String,
        delta: String,
    },
    ContentDelta {
        session_id: String,
        delta: String,
    },
    ToolExecuting {
        session_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolOutput {
        session_id: String,
        name: String,
        chunk: String,
    },
    ToolExecuted {
        session_id: String,
        name: String,
        is_error: bool,
        output: String,
    },
    ContextTokensUpdated {
        session_id: String,
        tokens_used: u32,
        reset: bool,
    },
    CompactionStarted {
        session_id: String,
        forced: bool,
    },
    CompactionFinished {
        session_id: String,
        turns: Option<usize>,
        messages: Option<usize>,
        summary: Option<String>,
        error: Option<String>,
    },
    MemorySaved {
        session_id: String,
        name: String,
        detail: Option<String>,
        status: String,
    },
    NoteSaved {
        session_id: String,
        name: String,
        detail: Option<String>,
        status: String,
    },
    MemoryPassStarted {
        session_id: String,
        model: String,
    },
    MemoryPassFinished {
        session_id: String,
        memories: usize,
        notes: usize,
    },
    Interrupted {
        session_id: String,
        text: String,
        reasoning: String,
        partial: String,
    },
    Steered {
        session_id: String,
        text: String,
    },
    SubagentStarted {
        session_id: String,
        id: String,
        goal: String,
    },
    SubagentProgress {
        session_id: String,
        id: String,
        activity: String,
    },
    SubagentFinished {
        session_id: String,
        id: String,
        success: bool,
        summary: String,
    },
    JobCompleted {
        session_id: String,
        job_id: String,
        command: String,
        status: String,
        elapsed_secs: u64,
    },
    ModelChanged {
        session_id: String,
        provider: String,
        model: String,
    },
    ApprovalRequested {
        request_id: String,
        session_id: String,
        request: ApprovalRequest,
    },
    ClarifyRequested {
        request_id: String,
        session_id: String,
        request: ClarifyRequest,
    },
    SudoRequested {
        request_id: String,
        session_id: String,
        command: String,
    },
}

pub type AppEventSender = broadcast::Sender<AppEvent>;

/// Convert a core [`AgentEvent`] into an [`AppEvent`] scoped to `session_id`.
pub fn from_agent_event(session_id: &str, event: AgentEvent) -> AppEvent {
    let sid = session_id.to_string();
    match event {
        AgentEvent::ReasoningDelta(delta) => AppEvent::ReasoningDelta {
            session_id: sid,
            delta,
        },
        AgentEvent::ContentDelta(delta) => AppEvent::ContentDelta {
            session_id: sid,
            delta,
        },
        AgentEvent::ToolExecuting { name, arguments } => AppEvent::ToolExecuting {
            session_id: sid,
            name,
            arguments,
        },
        AgentEvent::ToolOutput { name, chunk } => AppEvent::ToolOutput {
            session_id: sid,
            name,
            chunk,
        },
        AgentEvent::ToolExecuted {
            name,
            is_error,
            output,
        } => AppEvent::ToolExecuted {
            session_id: sid,
            name,
            is_error,
            output,
        },
        AgentEvent::ContextTokensUpdated { tokens_used, reset } => AppEvent::ContextTokensUpdated {
            session_id: sid,
            tokens_used,
            reset,
        },
        AgentEvent::MemorySaved {
            name,
            detail,
            status,
        } => AppEvent::MemorySaved {
            session_id: sid,
            name,
            detail,
            status: status.as_str().to_string(),
        },
        AgentEvent::NoteSaved {
            name,
            detail,
            status,
        } => AppEvent::NoteSaved {
            session_id: sid,
            name,
            detail,
            status: status.as_str().to_string(),
        },
        AgentEvent::MemoryPassStarted { model } => AppEvent::MemoryPassStarted {
            session_id: sid,
            model,
        },
        AgentEvent::MemoryPassFinished { memories, notes } => AppEvent::MemoryPassFinished {
            session_id: sid,
            memories,
            notes,
        },
        AgentEvent::Interrupted {
            text,
            reasoning,
            partial,
        } => AppEvent::Interrupted {
            session_id: sid,
            text,
            reasoning,
            partial,
        },
        AgentEvent::Steered { text } => AppEvent::Steered {
            session_id: sid,
            text,
        },
        AgentEvent::CompactionStarted { forced } => AppEvent::CompactionStarted {
            session_id: sid,
            forced,
        },
        AgentEvent::CompactionFinished { folded, error } => AppEvent::CompactionFinished {
            session_id: sid,
            turns: folded.as_ref().map(|f| f.turns),
            messages: folded.as_ref().map(|f| f.messages),
            summary: folded.and_then(|f| f.summary),
            error,
        },
    }
}

/// Convert a [`SubagentTaskEvent`] into an [`AppEvent`].
pub fn from_subagent_event(session_id: &str, event: SubagentTaskEvent) -> AppEvent {
    let sid = session_id.to_string();
    match event {
        SubagentTaskEvent::Started { id, goal } => AppEvent::SubagentStarted {
            session_id: sid,
            id,
            goal,
        },
        SubagentTaskEvent::Progress { id, activity, .. } => {
            let label = match activity {
                SubagentActivity::ModelStreaming => "streaming".to_string(),
                SubagentActivity::ToolStarted { name } => format!("{name} running"),
                SubagentActivity::ToolFinished { name, is_error } => {
                    format!("{name} {}", if is_error { "failed" } else { "done" })
                }
                SubagentActivity::ToolOutput { name, chunk } => {
                    let newest = chunk
                        .lines()
                        .rev()
                        .find(|line| !line.trim().is_empty())
                        .map(str::trim);
                    match newest {
                        Some(line) if !line.is_empty() => {
                            let short: String = line.chars().take(60).collect();
                            format!("{name} · {short}")
                        }
                        _ => format!("{name} running"),
                    }
                }
            };
            AppEvent::SubagentProgress {
                session_id: sid,
                id,
                activity: label,
            }
        }
        SubagentTaskEvent::Finished { record } => {
            let (success, summary) = match &record.status {
                SubagentStatus::Completed { reply, .. } => (
                    true,
                    reply.lines().next().unwrap_or("completed").to_string(),
                ),
                SubagentStatus::Failed { error, .. } => (false, error.clone()),
                SubagentStatus::Running => (true, "finished".to_string()),
            };
            AppEvent::SubagentFinished {
                session_id: sid,
                id: record.id,
                success,
                summary,
            }
        }
    }
}

/// Build an [`harness_core::AgentObserver`] that forwards events onto a broadcast sender.
pub fn broadcast_agent_observer(
    session_id: String,
    tx: broadcast::Sender<AppEvent>,
) -> harness_core::AgentObserver {
    Arc::new(move |event| {
        let _ = tx.send(from_agent_event(&session_id, event));
    })
}
