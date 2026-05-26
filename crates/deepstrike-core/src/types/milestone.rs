use serde::{Deserialize, Serialize};

use super::capability::CapabilityDescriptor;

/// One named phase in a [`MilestoneContract`].
///
/// A phase defines the criteria the agent must satisfy before the kernel
/// advances to the next phase and mounts the associated capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestonePhase {
    /// Stable identifier (e.g., `"plan"`, `"implement"`, `"verify"`).
    pub id: String,
    /// Human-readable criteria text passed to the verifier or HarnessLoop.
    #[serde(default)]
    pub criteria: Vec<String>,
    /// Capabilities unlocked and mounted when this phase passes.
    #[serde(default)]
    pub unlocks: Vec<CapabilityDescriptor>,
}

impl MilestonePhase {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            criteria: Vec::new(),
            unlocks: Vec::new(),
        }
    }

    pub fn with_criterion(mut self, criterion: impl Into<String>) -> Self {
        self.criteria.push(criterion.into());
        self
    }

    pub fn unlocking(mut self, capability: CapabilityDescriptor) -> Self {
        self.unlocks.push(capability);
        self
    }
}

/// Cascade of named phases that define milestone-level acceptance gates.
///
/// The agent must satisfy each phase in order. On success the kernel mounts
/// the phase's `unlocks` capabilities and advances to the next phase.
/// On failure the kernel injects a blocked-message into the working partition
/// and gives the LLM another chance without advancing the phase index.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MilestoneContract {
    pub phases: Vec<MilestonePhase>,
}

impl MilestoneContract {
    pub fn new() -> Self {
        Self { phases: Vec::new() }
    }

    pub fn phase(mut self, phase: MilestonePhase) -> Self {
        self.phases.push(phase);
        self
    }
}

/// Outcome of evaluating the acceptance criteria for the current milestone phase.
///
/// Created by a verifier (external LLM call, machine check, or explicit user
/// decision) and fed back to the kernel via `LoopEvent::MilestoneResult`.
#[derive(Debug, Clone)]
pub struct MilestoneCheckResult {
    pub phase_id: String,
    pub passed: bool,
    pub reason: Option<String>,
}

impl MilestoneCheckResult {
    pub fn pass(phase_id: impl Into<String>) -> Self {
        Self {
            phase_id: phase_id.into(),
            passed: true,
            reason: None,
        }
    }

    pub fn fail(phase_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            phase_id: phase_id.into(),
            passed: false,
            reason: Some(reason.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milestone_phase_builder() {
        let phase = MilestonePhase::new("plan")
            .with_criterion("Plan covers all requirements")
            .with_criterion("No ambiguous steps");
        assert_eq!(phase.id, "plan");
        assert_eq!(phase.criteria.len(), 2);
        assert!(phase.unlocks.is_empty());
    }

    #[test]
    fn milestone_contract_cascade() {
        let contract = MilestoneContract::new()
            .phase(MilestonePhase::new("phase-a"))
            .phase(MilestonePhase::new("phase-b"))
            .phase(MilestonePhase::new("phase-c"));
        assert_eq!(contract.phases.len(), 3);
        assert_eq!(contract.phases[1].id, "phase-b");
    }

    #[test]
    fn check_result_variants() {
        let p = MilestoneCheckResult::pass("phase-a");
        assert!(p.passed);
        assert!(p.reason.is_none());

        let f = MilestoneCheckResult::fail("phase-a", "missing evidence");
        assert!(!f.passed);
        assert_eq!(f.reason.as_deref(), Some("missing evidence"));
    }
}
