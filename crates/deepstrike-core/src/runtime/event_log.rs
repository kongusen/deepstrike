//! Unified kernel OS event log — category taxonomy for observations and session events.
//!
//! Phase 5: every kernel decision is classifiable as `syscall` / `sched` / `mm` / `proc` / `ipc`
//! so SDK session logs can be audited and replayed as a single OS event stream.

use serde::{Deserialize, Serialize};

/// Agent OS event category (kernel decision plane).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelEventCategory {
    /// Governance gates, capability mount/unmount, tool approval.
    Syscall,
    /// Scheduling, budgets, suspend/resume, checkpoints, milestones, rollback.
    Sched,
    /// Working ↔ long-term memory: compression, page-in/out, renewal.
    Mm,
    /// Sub-agent process table lifecycle.
    Proc,
    /// Signal routing and disposition.
    Ipc,
}

/// Snake_case observation / session `kind` string.
pub fn category_for_kind(kind: &str) -> KernelEventCategory {
    match kind {
        "tool_gated" | "capability_changed" => KernelEventCategory::Syscall,
        "suspended"
        | "resumed"
        | "budget_exceeded"
        | "checkpoint_taken"
        | "rollbacked"
        | "milestone_advanced"
        | "milestone_blocked"
        | "milestone_evidence" => KernelEventCategory::Sched,
        "compressed"
        | "page_out"
        | "page_in"
        | "page_in_requested"
        | "renewed"
        | "context_renewed" => KernelEventCategory::Mm,
        "agent_process_changed" | "agent_spawned" => KernelEventCategory::Proc,
        "signal_disposed" => KernelEventCategory::Ipc,
        "memory_written" | "memory_queried" => KernelEventCategory::Mm,
        _ => KernelEventCategory::Sched,
    }
}

/// All kernel observation kinds that should appear in a unified OS event log.
pub const KERNEL_OBSERVATION_KINDS: &[&str] = &[
    "compressed",
    "page_out",
    "page_in_requested",
    "renewed",
    "rollbacked",
    "capability_changed",
    "milestone_advanced",
    "milestone_blocked",
    "milestone_evidence",
    "checkpoint_taken",
    "agent_process_changed",
    "tool_gated",
    "signal_disposed",
    "budget_exceeded",
    "suspended",
    "resumed",
    "memory_written",
    "memory_queried",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_observation_kinds_to_categories() {
        assert_eq!(category_for_kind("tool_gated"), KernelEventCategory::Syscall);
        assert_eq!(category_for_kind("page_out"), KernelEventCategory::Mm);
        assert_eq!(category_for_kind("agent_process_changed"), KernelEventCategory::Proc);
        assert_eq!(category_for_kind("signal_disposed"), KernelEventCategory::Ipc);
        assert_eq!(category_for_kind("suspended"), KernelEventCategory::Sched);
    }

    #[test]
    fn kernel_observation_kinds_cover_abi_surface() {
        assert!(KERNEL_OBSERVATION_KINDS.contains(&"page_out"));
        assert!(KERNEL_OBSERVATION_KINDS.contains(&"resumed"));
    }
}
