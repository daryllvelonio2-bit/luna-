use std::collections::HashSet;
use serde::{Deserialize, Serialize};

use crate::error::HarnessError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StageKind {
    Baseline,
    Invariant,
    Domain,
    Composition,
}

impl StageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Baseline => "BASE",
            Self::Invariant => "INV",
            Self::Domain => "DOM",
            Self::Composition => "COMP",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RolloutRecord {
    pub stage: StageKind,
    pub candidate_id: String,
    pub rollout_idx: usize,
    pub success: bool,
    pub tokens_used: usize,
    pub duration_ms: u64,
    pub notes: Option<String>,
}

impl RolloutRecord {
    pub fn identity(&self) -> String {
        format!("{}:{}:{}", self.stage.as_str(), self.candidate_id, self.rollout_idx)
    }
}

/// RolloutLedger: Budget accounting and strict identity enforcement for recursive refinement
/// R = 4k rollouts per task (k base + 2k candidates + k compositions)
/// P = 3k improvement stages (k INV + k DOM + k compositions)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutLedger {
    pub k: usize,
    pub max_rollouts: usize,
    pub max_stages: usize,
    pub completed_rollouts: Vec<RolloutRecord>,
    pub seen_identities: HashSet<String>,
    pub stages_executed: usize,
}

impl RolloutLedger {
    pub fn new(k: usize) -> Self {
        let safe_k = if k == 0 { 1 } else { k };
        Self {
            k: safe_k,
            max_rollouts: 4 * safe_k,
            max_stages: 3 * safe_k,
            completed_rollouts: Vec::new(),
            seen_identities: HashSet::new(),
            stages_executed: 0,
        }
    }

    pub fn can_allocate_stage(&self) -> bool {
        self.stages_executed < self.max_stages
    }

    pub fn record_stage(&mut self) -> Result<(), HarnessError> {
        if !self.can_allocate_stage() {
            return Err(HarnessError::LedgerError(format!(
                "stage budget exhausted: {}/{} stages executed",
                self.stages_executed, self.max_stages
            )));
        }
        self.stages_executed += 1;
        Ok(())
    }

    pub fn can_allocate_rollout(&self) -> bool {
        self.completed_rollouts.len() < self.max_rollouts
    }

    pub fn record_rollout(&mut self, record: RolloutRecord) -> Result<(), HarnessError> {
        if !self.can_allocate_rollout() {
            return Err(HarnessError::LedgerError(format!(
                "rollout budget exhausted: {}/{} rollouts recorded",
                self.completed_rollouts.len(), self.max_rollouts
            )));
        }

        let identity = record.identity();
        // Strict identity enforcement: duplicates mean a silent overwrite bug
        if self.seen_identities.contains(&identity) {
            return Err(HarnessError::LedgerError(format!(
                "duplicate rollout identity detected: '{}' already exists in ledger",
                identity
            )));
        }

        self.seen_identities.insert(identity);
        self.completed_rollouts.push(record);
        Ok(())
    }

    pub fn total_tokens(&self) -> usize {
        self.completed_rollouts.iter().map(|r| r.tokens_used).sum()
    }

    pub fn success_count(&self) -> usize {
        self.completed_rollouts.iter().filter(|r| r.success).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ledger_budget_accounting() {
        let mut ledger = RolloutLedger::new(1); // 4 rollouts, 3 stages
        assert!(ledger.can_allocate_rollout());
        assert!(ledger.can_allocate_stage());

        assert!(ledger.record_stage().is_ok());
        assert!(ledger.record_stage().is_ok());
        assert!(ledger.record_stage().is_ok());
        assert!(ledger.record_stage().is_err()); // stage 4 exceeds P=3k

        for i in 0..4 {
            let record = RolloutRecord {
                stage: StageKind::Invariant,
                candidate_id: "cand1".into(),
                rollout_idx: i,
                success: true,
                tokens_used: 100,
                duration_ms: 50,
                notes: None,
            };
            assert!(ledger.record_rollout(record).is_ok());
        }

        // Exceeding max_rollouts = 4
        let extra = RolloutRecord {
            stage: StageKind::Invariant,
            candidate_id: "cand1".into(),
            rollout_idx: 4,
            success: true,
            tokens_used: 100,
            duration_ms: 50,
            notes: None,
        };
        assert!(ledger.record_rollout(extra).is_err());
    }

    #[test]
    fn test_ledger_strict_identity_enforcement() {
        let mut ledger = RolloutLedger::new(2);
        let record = RolloutRecord {
            stage: StageKind::Domain,
            candidate_id: "cand_dom".into(),
            rollout_idx: 0,
            success: true,
            tokens_used: 150,
            duration_ms: 70,
            notes: None,
        };

        assert!(ledger.record_rollout(record.clone()).is_ok());
        // Repeating same rollout identity must fail closed
        let dup_err = ledger.record_rollout(record);
        assert!(dup_err.is_err());
    }
}
