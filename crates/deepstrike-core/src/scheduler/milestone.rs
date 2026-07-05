//! Milestone contract tracking extracted from LoopStateMachine.

use crate::types::milestone::{MilestoneContract, MilestonePhase};

/// Tracks milestone contract progress and owns its phase-cursor transitions;
/// `LoopStateMachine::handle_milestone_result` drives it via `advance`/`record_block`.
pub struct MilestoneTracker {
    /// Optional milestone contract loaded before the run starts.
    contract: Option<MilestoneContract>,
    /// Index of the current (not-yet-passed) phase within `contract`.
    current_phase: usize,
    /// How many times the current phase has been blocked (reset on advance).
    blocked_count: usize,
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

    /// The full current (not-yet-passed) phase, or `None` when no contract is
    /// loaded or all phases are complete. Callers read verifier/criteria/unlocks/
    /// rollback_policy from here instead of re-deriving from raw indices.
    pub fn current_phase(&self) -> Option<&MilestonePhase> {
        self.contract
            .as_ref()
            .and_then(|c| c.phases.get(self.current_phase))
    }

    /// Returns the ID of the current (not-yet-passed) phase, or `None` when
    /// no contract is loaded or all phases are complete.
    pub fn current_phase_id(&self) -> Option<&str> {
        self.current_phase().map(|p| p.id.as_str())
    }

    /// Returns the acceptance criteria of the current phase as a slice.
    pub fn current_criteria(&self) -> &[String] {
        self.current_phase()
            .map(|p| p.criteria.as_slice())
            .unwrap_or(&[])
    }

    /// A phase passed: move the cursor to the next phase and reset the block counter.
    pub fn advance(&mut self) {
        self.current_phase += 1;
        self.blocked_count = 0;
    }

    /// The current phase was blocked; returns the updated consecutive-block count.
    pub fn record_block(&mut self) -> usize {
        self.blocked_count += 1;
        self.blocked_count
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
        tracker.advance();
        assert_eq!(tracker.current_phase_id(), Some("phase2"));
        tracker.advance();
        assert!(tracker.is_complete());
        assert_eq!(tracker.current_phase_id(), None);
    }
}
