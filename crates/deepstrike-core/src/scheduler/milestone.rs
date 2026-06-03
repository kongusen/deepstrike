//! Milestone contract tracking extracted from LoopStateMachine.
//!
//! This module holds the milestone contract state (contract, current phase,
//! blocked count) to reduce bloat in the main state machine. The complex
//! handling logic remains in LoopStateMachine for now.

use crate::types::milestone::MilestoneContract;

/// Tracks milestone contract progress.
///
/// Extracted from `LoopStateMachine` to reduce state machine bloat.
/// This struct only holds state; the actual milestone evaluation logic
/// remains in `LoopStateMachine::handle_milestone_result`.
pub struct MilestoneTracker {
    /// Optional milestone contract loaded before the run starts.
    pub(crate) contract: Option<MilestoneContract>,
    /// Index of the current (not-yet-passed) phase within `contract`.
    pub(crate) current_phase: usize,
    /// How many times the current phase has been blocked (reset on advance).
    pub(crate) blocked_count: usize,
}

impl MilestoneTracker {
    /// Create a new milestone tracker with no contract loaded.
    pub fn new() -> Self {
        Self {
            contract: None,
            current_phase: 0,
            blocked_count: 0,
        }
    }

    /// Load a milestone contract. Must be called before the run starts.
    pub fn load_contract(&mut self, contract: MilestoneContract) {
        self.contract = Some(contract);
        self.current_phase = 0;
        self.blocked_count = 0;
    }

    /// Returns the ID of the current (not-yet-passed) phase, or `None` when
    /// no contract is loaded or all phases are complete.
    pub fn current_phase_id(&self) -> Option<&str> {
        self.contract
            .as_ref()
            .and_then(|c| c.phases.get(self.current_phase))
            .map(|p| p.id.as_str())
    }

    /// Returns the acceptance criteria of the current phase as a slice.
    pub fn current_criteria(&self) -> &[String] {
        self.contract
            .as_ref()
            .and_then(|c| c.phases.get(self.current_phase))
            .map(|p| p.criteria.as_slice())
            .unwrap_or(&[])
    }

    /// Returns `true` when there is no contract or all phases have passed.
    pub fn is_complete(&self) -> bool {
        match &self.contract {
            None => true,
            Some(c) => self.current_phase >= c.phases.len(),
        }
    }
}

impl Default for MilestoneTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::milestone::{MilestonePhase, MilestoneRollbackPolicy};

    #[test]
    fn test_tracker_no_contract_is_complete() {
        let tracker = MilestoneTracker::new();
        assert!(tracker.is_complete());
        assert_eq!(tracker.current_phase_id(), None);
        assert!(tracker.current_criteria().is_empty());
    }

    #[test]
    fn test_tracker_single_phase_is_incomplete_until_passed() {
        use crate::types::milestone::MilestoneUnlockPolicy;
        let contract = MilestoneContract {
            phases: vec![MilestonePhase {
                id: "phase1".to_string(),
                criteria: vec!["c1".to_string()],
                unlocks: vec![],
                retry_policy: None,
                verifier: None,
                required_evidence: vec![],
                unlock_policy: MilestoneUnlockPolicy::Immediate,
                rollback_policy: MilestoneRollbackPolicy::Terminate,
            }],
        };
        let mut tracker = MilestoneTracker::new();
        tracker.load_contract(contract);

        assert!(!tracker.is_complete());
        assert_eq!(tracker.current_phase_id(), Some("phase1"));
        assert_eq!(tracker.current_criteria(), &["c1".to_string()]);
    }

    #[test]
    fn test_tracker_multi_phase_advances_on_pass() {
        use crate::types::milestone::MilestoneUnlockPolicy;
        let contract = MilestoneContract {
            phases: vec![
                MilestonePhase {
                    id: "phase1".to_string(),
                    criteria: vec!["c1".to_string()],
                    unlocks: vec![],
                    retry_policy: None,
                    verifier: None,
                    required_evidence: vec![],
                    unlock_policy: MilestoneUnlockPolicy::Immediate,
                    rollback_policy: MilestoneRollbackPolicy::Terminate,
                },
                MilestonePhase {
                    id: "phase2".to_string(),
                    criteria: vec!["c2".to_string()],
                    unlocks: vec![],
                    retry_policy: None,
                    verifier: None,
                    required_evidence: vec![],
                    unlock_policy: MilestoneUnlockPolicy::Immediate,
                    rollback_policy: MilestoneRollbackPolicy::Terminate,
                },
            ],
        };
        let mut tracker = MilestoneTracker::new();
        tracker.load_contract(contract);

        assert_eq!(tracker.current_phase_id(), Some("phase1"));
        tracker.current_phase += 1; // Simulate phase advance
        assert_eq!(tracker.current_phase_id(), Some("phase2"));
        tracker.current_phase += 1;
        assert!(tracker.is_complete());
        assert_eq!(tracker.current_phase_id(), None);
    }
}
