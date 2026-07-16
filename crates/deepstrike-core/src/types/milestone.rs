use serde::{Deserialize, Serialize};

use super::capability::CapabilityDescriptor;

/// How the kernel should evaluate a milestone phase.
///
/// Carried inside `EvaluateMilestone` so the SDK/runner knows which
/// evaluation path to take. Defaults to `HarnessEval` when unset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MilestoneVerifier {
    /// Deterministic machine check (e.g., regex, JSON schema, test suite).
    MachineCheck,
    /// Harness-driven evaluation (the default `EvaluateMilestone` callback).
    HarnessEval,
    /// LLM-as-judge: a secondary LLM call scores the output against criteria.
    LlmJudge,
    /// Requires explicit human approval before the phase can advance.
    HumanApproval,
    /// Runs an external command and uses its exit code as pass/fail.
    ExternalCommand { cmd: String },
}

/// Policy governing capability unlock on a phase advance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneUnlockPolicy {
    /// Mount capabilities immediately when the phase passes (default).
    #[default]
    Immediate,
    /// Defer capability mounting — caller mounts manually via `CapabilityCommand`.
    Deferred,
}

/// What the kernel does when a blocked milestone exceeds its retry budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneRollbackPolicy {
    /// Terminate the run with `MilestoneExceeded` (default).
    #[default]
    Terminate,
    /// Roll back context to the last checkpoint once, then terminate with `MilestoneExceeded` —
    /// the budget is already exhausted, so re-entering the same retry loop is never productive.
    Rollback,
    /// Continue injecting blocked messages indefinitely (no budget enforcement).
    Continue,
}

/// Controls how many times a blocked phase is retried before policy kicks in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of blocked retries. `0` means unlimited (no enforcement).
    pub max_attempts: u32,
}

impl RetryPolicy {
    pub fn max(max_attempts: u32) -> Self {
        Self { max_attempts }
    }
}

/// One named phase in a [`MilestoneContract`].
///
/// A phase defines the criteria the agent must satisfy before the kernel
/// advances to the next phase and mounts the associated capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestonePhase {
    /// Stable identifier (e.g., `"plan"`, `"implement"`, `"verify"`).
    pub id: String,
    /// Human-readable criteria text passed to the verifier or AttemptLoop.
    #[serde(default)]
    pub criteria: Vec<String>,
    /// Capabilities unlocked and mounted when this phase passes.
    #[serde(default)]
    pub unlocks: Vec<CapabilityDescriptor>,
    /// How to evaluate this phase. `None` → defaults to `HarnessEval`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<MilestoneVerifier>,
    /// Evidence keys the verifier must supply in `MilestoneCheckResult`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_evidence: Vec<String>,
    /// How capabilities are mounted on advance.
    #[serde(default)]
    pub unlock_policy: MilestoneUnlockPolicy,
    /// What happens when `retry_policy.max_attempts` is exceeded.
    #[serde(default)]
    pub rollback_policy: MilestoneRollbackPolicy,
    /// Retry budget for blocked phases. `None` → unlimited retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<RetryPolicy>,
}

impl MilestonePhase {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            criteria: Vec::new(),
            unlocks: Vec::new(),
            verifier: None,
            required_evidence: Vec::new(),
            unlock_policy: MilestoneUnlockPolicy::default(),
            rollback_policy: MilestoneRollbackPolicy::default(),
            retry_policy: None,
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

    pub fn with_verifier(mut self, verifier: MilestoneVerifier) -> Self {
        self.verifier = Some(verifier);
        self
    }

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    pub fn with_rollback_policy(mut self, policy: MilestoneRollbackPolicy) -> Self {
        self.rollback_policy = policy;
        self
    }

    pub fn requiring_evidence(mut self, key: impl Into<String>) -> Self {
        self.required_evidence.push(key.into());
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
