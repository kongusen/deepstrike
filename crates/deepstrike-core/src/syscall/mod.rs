//! Primitive P1: the single syscall trap boundary.
//!
//! M0 scaffold (see `.local-docs/specs/agent-os-three-primitives.md`): types + conversions
//! only — **no wiring, no behavior change**. A later milestone (M2) generalizes
//! [`crate::governance::pipeline`] so its request becomes [`Syscall`] and its result becomes
//! [`Disposition`], and routes spawn / page-in / write-memory through the same gate (today they
//! bypass governance entirely).
//!
//! Concept overlap this primitive collapses: the two parallel decision vocabularies
//! ([`crate::types::policy::GovernanceVerdict`] and `SignalDisposition`). Tool/spawn/memory
//! decisions converge on [`Disposition`]; signals feed the P2 scheduler instead.

use crate::mm::PageInRequest;
use crate::mm::memory::{MemoryQuery, MemoryWriteRequest};
use crate::scheduler::tcb::WaitReason;
use crate::types::agent::IsolationManifest;
use crate::types::message::ToolCall;
use crate::types::policy::GovernanceVerdict;

/// An effectful request from the SDK that the kernel must adjudicate.
///
/// Every side-effecting service request becomes a `Syscall` variant; the opcode is **data**, so
/// adding a service does not add a new ABI shape (unlike the per-feature `Load*Policy` events today).
#[derive(Debug, Clone)]
pub enum Syscall {
    /// Model-proposed tool call (today: the only thing through the governance gate).
    Invoke(ToolCall),
    /// Spawn a sub-agent (today: bypasses the gate).
    Spawn(IsolationManifest),
    /// Page long-term memory into working context (today: bypasses the gate).
    PageIn(PageInRequest),
    /// Persist a long-term memory entry.
    WriteMemory(MemoryWriteRequest),
    /// Retrieve long-term memory entries.
    QueryMemory(MemoryQuery),
    /// R3-1: append `count` nodes to the in-flight workflow DAG at runtime. Gating DAG growth through
    /// the trap lets a `ResourceQuota` backstop a runaway loop-until-done (denied past
    /// `max_workflow_nodes`); per-node spawns are still gated separately by `Spawn`.
    SubmitNodes { count: usize },
    /// M5/G1: an agent authors a whole workflow `spec` (`node_count` nodes). Bootstraps the DAG when
    /// none is active, else flattens onto it — either way it is gated by the same `max_workflow_nodes`
    /// quota as `SubmitNodes` (a spec is just a node batch with a bootstrap fast-path), so an
    /// agent-authored harness cannot overgrow the DAG past the run's budget.
    LoadWorkflow { node_count: usize },
}

impl Syscall {
    /// Stable opcode label for audit/event-log categorization.
    pub fn opcode(&self) -> &'static str {
        match self {
            Self::Invoke(_) => "invoke",
            Self::Spawn(_) => "spawn",
            Self::PageIn(_) => "page_in",
            Self::WriteMemory(_) => "write_memory",
            Self::QueryMemory(_) => "query_memory",
            Self::SubmitNodes { .. } => "submit_nodes",
            Self::LoadWorkflow { .. } => "load_workflow",
        }
    }
}

/// The kernel's adjudication of a [`Syscall`]. Generalizes [`GovernanceVerdict`]:
/// `AskUser` becomes [`Disposition::Gate`] (suspend the calling task via the P2 TCB),
/// which is where this primitive meets P2.
#[derive(Debug, Clone)]
pub enum Disposition {
    /// Proceed as requested.
    Allow,
    /// Reject. `stage` names the gate stage that vetoed.
    Deny { stage: &'static str, reason: String },
    /// Suspend the calling task until an external party resolves it (e.g. human approval).
    /// `reason` carries the human-readable justification (e.g. the governance `AskUser` reason).
    Gate { wait: WaitReason, reason: String },
    /// Accept but queue for later scheduling (backpressure).
    Defer { slot: u32 },
    /// Rejected by a rate limiter; retry permitted after the delay.
    RateLimited { retry_after_ms: u64 },
}

impl Disposition {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny { .. } => "deny",
            Self::Gate { .. } => "gate",
            Self::Defer { .. } => "defer",
            Self::RateLimited { .. } => "rate_limited",
        }
    }

    /// Whether the syscall may proceed to execution now.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Bridge from the existing tool-decision vocabulary. `AskUser` → `Gate(Approval)`: a tool
/// awaiting human approval suspends the task, which M2+M1 realize via the TCB.
impl From<GovernanceVerdict> for Disposition {
    fn from(verdict: GovernanceVerdict) -> Self {
        match verdict {
            GovernanceVerdict::Allow => Disposition::Allow,
            GovernanceVerdict::Deny { stage, reason } => Disposition::Deny { stage, reason },
            GovernanceVerdict::RateLimited { retry_after_ms } => {
                Disposition::RateLimited { retry_after_ms }
            }
            GovernanceVerdict::AskUser { reason } => Disposition::Gate {
                wait: WaitReason::Approval,
                reason,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_allow_maps_to_allow() {
        let d: Disposition = GovernanceVerdict::Allow.into();
        assert!(d.is_allowed());
        assert_eq!(d.label(), "allow");
    }

    #[test]
    fn verdict_deny_preserves_stage_and_reason() {
        let d: Disposition = GovernanceVerdict::Deny {
            stage: "veto",
            reason: "blocked".into(),
        }
        .into();
        match d {
            Disposition::Deny { stage, reason } => {
                assert_eq!(stage, "veto");
                assert_eq!(reason, "blocked");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
        assert!(!Disposition::Deny { stage: "veto", reason: String::new() }.is_allowed());
    }

    #[test]
    fn verdict_ask_user_maps_to_gate_approval() {
        let d: Disposition = GovernanceVerdict::AskUser {
            reason: "confirm".into(),
        }
        .into();
        assert!(matches!(
            &d,
            Disposition::Gate { wait: WaitReason::Approval, reason } if reason == "confirm"
        ));
        assert!(!d.is_allowed());
    }

    #[test]
    fn verdict_rate_limited_preserves_delay() {
        let d: Disposition = GovernanceVerdict::RateLimited { retry_after_ms: 500 }.into();
        assert!(matches!(d, Disposition::RateLimited { retry_after_ms: 500 }));
    }

    #[test]
    fn syscall_opcode_labels() {
        use crate::types::message::ToolCall;
        let call = ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        };
        assert_eq!(Syscall::Invoke(call).opcode(), "invoke");
    }
}
