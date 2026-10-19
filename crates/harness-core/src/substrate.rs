use std::sync::Arc;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentConfig};
use crate::error::HarnessError;
use crate::surface::SurfaceSpec;
use crate::traits::{EvolverEngine, MemoryEngine, ModelProvider, NotesEngine, ToolRegistry};

/// Outcome produced by executing a task through an agent substrate
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubstrateOutcome {
    pub success: bool,
    pub output: String,
    pub tokens_used: usize,
    pub duration_ms: u64,
}

/// Substrate: Pluggable executor abstraction decoupling search/recursion algorithms
/// from the underlying runtime harness (Local Agent Loop, OpenCode, Codex, headless sandbox).
#[async_trait]
pub trait Substrate: Send + Sync {
    /// Identifier of the substrate
    fn name(&self) -> &str;

    /// Run a task to completion using the given SurfaceSpec configuration
    async fn execute_task(
        &self,
        surface: &SurfaceSpec,
        task_instruction: &str,
        cancel_token: &CancellationToken,
    ) -> Result<SubstrateOutcome, HarnessError>;
}

/// Scripted/mock substrate for deterministic testing and offline evaluations
pub struct MockSubstrate {
    pub name: String,
    pub canned_outcome: SubstrateOutcome,
}

impl MockSubstrate {
    pub fn new(name: impl Into<String>, canned_outcome: SubstrateOutcome) -> Self {
        Self {
            name: name.into(),
            canned_outcome,
        }
    }
}

#[async_trait]
impl Substrate for MockSubstrate {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute_task(
        &self,
        surface: &SurfaceSpec,
        _task_instruction: &str,
        _cancel_token: &CancellationToken,
    ) -> Result<SubstrateOutcome, HarnessError> {
        surface.validate()?;
        Ok(self.canned_outcome.clone())
    }
}

/// Local agent loop substrate: executes tasks directly through the configured Luna Agent
pub struct LocalAgentSubstrate<P, M, N, E, R> {
    pub name: String,
    pub provider: Arc<P>,
    pub memory: Arc<M>,
    pub notes: Arc<N>,
    pub evolver: Arc<E>,
    pub registry: Arc<R>,
    pub agent_config: AgentConfig,
}

impl<P, M, N, E, R> LocalAgentSubstrate<P, M, N, E, R> {
    pub fn new(
        name: impl Into<String>,
        provider: Arc<P>,
        memory: Arc<M>,
        notes: Arc<N>,
        evolver: Arc<E>,
        registry: Arc<R>,
        agent_config: AgentConfig,
    ) -> Self {
        Self {
            name: name.into(),
            provider,
            memory,
            notes,
            evolver,
            registry,
            agent_config,
        }
    }
}

#[async_trait]
impl<P, M, N, E, R> Substrate for LocalAgentSubstrate<P, M, N, E, R>
where
    P: ModelProvider + 'static,
    M: MemoryEngine + 'static,
    N: NotesEngine + 'static,
    E: EvolverEngine + 'static,
    R: ToolRegistry + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute_task(
        &self,
        surface: &SurfaceSpec,
        task_instruction: &str,
        cancel_token: &CancellationToken,
    ) -> Result<SubstrateOutcome, HarnessError> {
        let agent = Agent::new(
            self.provider.clone(),
            self.memory.clone(),
            self.notes.clone(),
            self.evolver.clone(),
            self.registry.clone(),
            self.agent_config.clone(),
        )
        .with_surface(surface.clone())?;

        let start = std::time::Instant::now();
        let outcome = agent.run_turn(task_instruction, cancel_token.clone()).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        match outcome {
            Ok(turn) => Ok(SubstrateOutcome {
                success: true,
                output: turn.reply,
                tokens_used: turn.tool_calls.len() * 100 + 50,
                duration_ms,
            }),
            Err(e) => Ok(SubstrateOutcome {
                success: false,
                output: e.to_string(),
                tokens_used: 0,
                duration_ms,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_substrate_execution() {
        let substrate = MockSubstrate::new(
            "test_runner",
            SubstrateOutcome {
                success: true,
                output: "Done".into(),
                tokens_used: 120,
                duration_ms: 15,
            },
        );

        let surface = SurfaceSpec::default();
        let cancel = CancellationToken::new();
        let outcome = substrate.execute_task(&surface, "Write test", &cancel).await;
        assert!(outcome.is_ok());
        assert_eq!(outcome.unwrap().tokens_used, 120);
    }
}
