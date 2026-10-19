use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use harness_core::{
    HarnessError, RolloutLedger, RolloutRecord, StageKind, Substrate, SubstrateOutcome,
    SurfaceSpec,
};

/// Recursion mode for task-level harness refinement
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecursionMode {
    /// Parallel exploration: probe k invariant + k domain candidates, then compose
    Parallel,
    /// Sequential refinement: carry refined harness through k successive propose-probe-compose cycles
    Sequential,
}

/// Execution evidence collected across rollouts for a task
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskEvidence {
    pub task_instruction: String,
    pub trajectories: Vec<String>,
    pub errors: Vec<String>,
}

/// Evaluated probe result for a candidate SurfaceSpec
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub candidate_id: String,
    pub stage: StageKind,
    pub surface: SurfaceSpec,
    pub outcome: SubstrateOutcome,
}

/// TaskHarnessRefiner: Implements task-level Propose–Probe–Compose recursion (Harness²)
/// Bound by budget ledger accounting: R = 4k rollouts, P = 3k stages.
pub struct TaskHarnessRefiner {
    pub k: usize,
    pub mode: RecursionMode,
    pub ledger: RolloutLedger,
}

impl TaskHarnessRefiner {
    pub fn new(k: usize, mode: RecursionMode) -> Self {
        let safe_k = if k == 0 { 1 } else { k };
        Self {
            k: safe_k,
            mode,
            ledger: RolloutLedger::new(safe_k),
        }
    }

    /// Proposes candidate SurfaceSpecs given task instructions and trajectory evidence.
    /// Invariant (INV) candidates tune guardrails and reasoning guidance.
    /// Domain (DOM) candidates tune tool guidance and domain skills.
    pub fn propose_candidates(
        &self,
        base: &SurfaceSpec,
        evidence: &TaskEvidence,
        stage: StageKind,
        count: usize,
    ) -> Vec<(String, SurfaceSpec)> {
        let mut candidates = Vec::with_capacity(count);

        for i in 0..count {
            let mut cand = base.clone();
            let candidate_id = format!("{}_cand_{}", stage.as_str().to_lowercase(), i + 1);

            match stage {
                StageKind::Invariant => {
                    // Refine guardrails and general tool discipline from error evidence
                    if !evidence.errors.is_empty() {
                        let failure_advice = format!(
                            "Execution guidance: prior attempt had errors ({}); verify state before mutating.",
                            evidence.errors.join("; ")
                        );
                        if !cand.c1_system_prompt.always_on_prose.is_empty() {
                            cand.c1_system_prompt.always_on_prose.push('\n');
                        }
                        cand.c1_system_prompt.always_on_prose.push_str(&failure_advice);
                    }
                    cand.c2_guardrails.max_consecutive_failures = 3;
                }
                StageKind::Domain => {
                    // Refine domain tool guidance based on task instruction
                    let tool_guidance = format!(
                        "Domain guidance for task '{}': prefer atomic edits and verify output.",
                        evidence.task_instruction
                    );
                    cand.c5_tool_guidance.insert("task_discipline".into(), tool_guidance);
                }
                StageKind::Composition | StageKind::Baseline => {}
            }

            candidates.push((candidate_id, cand));
        }

        candidates
    }

    /// Probes a batch of candidate SurfaceSpecs on the provided Substrate within ledger budget
    pub async fn probe_candidates(
        &mut self,
        candidates: &[(String, SurfaceSpec)],
        task_instruction: &str,
        stage: StageKind,
        substrate: &dyn Substrate,
        cancel_token: &CancellationToken,
    ) -> Result<Vec<ProbeResult>, HarnessError> {
        self.ledger.record_stage()?;

        let mut results = Vec::new();

        for (idx, (cand_id, surface)) in candidates.iter().enumerate() {
            if !self.ledger.can_allocate_rollout() {
                break;
            }

            let outcome = substrate
                .execute_task(surface, task_instruction, cancel_token)
                .await?;

            let record = RolloutRecord {
                stage,
                candidate_id: cand_id.clone(),
                rollout_idx: idx,
                success: outcome.success,
                tokens_used: outcome.tokens_used,
                duration_ms: outcome.duration_ms,
                notes: Some(format!("Output length: {} bytes", outcome.output.len())),
            };

            self.ledger.record_rollout(record)?;

            results.push(ProbeResult {
                candidate_id: cand_id.clone(),
                stage,
                surface: surface.clone(),
                outcome,
            });
        }

        Ok(results)
    }

    /// Composes the winning harness components from probe results into a single refined SurfaceSpec
    pub fn compose(&self, base: &SurfaceSpec, probe_results: &[ProbeResult]) -> SurfaceSpec {
        let mut composite = base.clone();

        for probe in probe_results {
            if probe.outcome.success {
                // Adopt improved tool guidance
                for (tool, guidance) in &probe.surface.c5_tool_guidance {
                    composite.c5_tool_guidance.insert(tool.clone(), guidance.clone());
                }
                // Adopt tighter guardrails
                if probe.surface.c2_guardrails.max_consecutive_failures < composite.c2_guardrails.max_consecutive_failures {
                    composite.c2_guardrails.max_consecutive_failures = probe.surface.c2_guardrails.max_consecutive_failures;
                }
                // Adopt valid skills
                for skill in &probe.surface.c4_skills {
                    if !composite.c4_skills.iter().any(|s| s.name == skill.name) {
                        composite.c4_skills.push(skill.clone());
                    }
                }
            }
        }

        composite
    }

    /// Executes the full recursive Propose–Probe–Compose loop on a task
    pub async fn run_recursion(
        &mut self,
        base_surface: &SurfaceSpec,
        task_instruction: &str,
        substrate: &dyn Substrate,
        cancel_token: &CancellationToken,
    ) -> Result<SurfaceSpec, HarnessError> {
        let mut current_surface = base_surface.clone();

        match self.mode {
            RecursionMode::Parallel => {
                let evidence = TaskEvidence {
                    task_instruction: task_instruction.to_string(),
                    trajectories: Vec::new(),
                    errors: Vec::new(),
                };

                // Probe k general (INV) candidates
                let inv_candidates = self.propose_candidates(
                    &current_surface,
                    &evidence,
                    StageKind::Invariant,
                    self.k,
                );
                let inv_results = self
                    .probe_candidates(&inv_candidates, task_instruction, StageKind::Invariant, substrate, cancel_token)
                    .await?;

                // Probe k domain (DOM) candidates
                let dom_candidates = self.propose_candidates(
                    &current_surface,
                    &evidence,
                    StageKind::Domain,
                    self.k,
                );
                let dom_results = self
                    .probe_candidates(&dom_candidates, task_instruction, StageKind::Domain, substrate, cancel_token)
                    .await?;

                let mut all_results = inv_results;
                all_results.extend(dom_results);

                current_surface = self.compose(&current_surface, &all_results);
            }
            RecursionMode::Sequential => {
                for step in 0..self.k {
                    if !self.ledger.can_allocate_stage() || !self.ledger.can_allocate_rollout() {
                        break;
                    }

                    let evidence = TaskEvidence {
                        task_instruction: task_instruction.to_string(),
                        trajectories: vec![format!("step_{}", step)],
                        errors: Vec::new(),
                    };

                    let cands = self.propose_candidates(
                        &current_surface,
                        &evidence,
                        StageKind::Invariant,
                        1,
                    );
                    let probes = self
                        .probe_candidates(&cands, task_instruction, StageKind::Invariant, substrate, cancel_token)
                        .await?;

                    current_surface = self.compose(&current_surface, &probes);
                }
            }
        }

        current_surface.validate()?;
        Ok(current_surface)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::MockSubstrate;

    #[tokio::test]
    async fn test_parallel_task_recursion() {
        let mut refiner = TaskHarnessRefiner::new(1, RecursionMode::Parallel);
        let substrate = MockSubstrate::new(
            "mock_executor",
            SubstrateOutcome {
                success: true,
                output: "Success".into(),
                tokens_used: 250,
                duration_ms: 30,
            },
        );

        let base_surface = SurfaceSpec::default();
        let cancel = CancellationToken::new();

        let refined = refiner
            .run_recursion(&base_surface, "Optimize matrix multiplication", &substrate, &cancel)
            .await;

        assert!(refined.is_ok());
        let refined_spec = refined.unwrap();
        assert!(refined_spec.c5_tool_guidance.contains_key("task_discipline"));
        assert_eq!(refiner.ledger.completed_rollouts.len(), 2); // 1 INV + 1 DOM
    }
}
