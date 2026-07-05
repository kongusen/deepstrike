//! Unified kernel OS event log — category taxonomy for observations and session events.
//!
//! Phase 5: every kernel decision is classifiable as `syscall` / `sched` / `mm` / `proc` / `ipc`
//! so SDK session logs can be audited and replayed as a single OS event stream.
//!
//! Three-primitives lens (M4): every kernel event rolls up to exactly one of the three kernel
//! primitives — **P1 syscall** (the adjudication trap), **P2 sched** (the TCB/task table + the
//! scheduler), **P3 mm** (the handle table + paging). The five wire categories above are retained
//! as finer-grained audit labels (a stable, shipped field), but `proc` and `ipc` are facets of the
//! P2 scheduler — the process table *is* the task table, and signal disposition *feeds* the
//! scheduler — so they project onto [`Primitive::Sched`]. See [`KernelEventCategory::primitive`].

use serde::{Deserialize, Serialize};

/// One of the three kernel primitives every OS event belongs to (the canonical decision planes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primitive {
    /// P1 — the single syscall trap (governance/capability/spawn/memory adjudication).
    Syscall,
    /// P2 — the TCB/task table + scheduler (budgets, lifecycle, process table, signal disposition).
    Sched,
    /// P3 — the handle table + paging (compression, page-in/out, renewal, long-term memory).
    Mm,
}

impl Primitive {
    pub fn label(self) -> &'static str {
        match self {
            Self::Syscall => "syscall",
            Self::Sched => "sched",
            Self::Mm => "mm",
        }
    }
}

/// Agent OS event category (kernel decision plane). Finer-grained than [`Primitive`]; retained as a
/// stable wire field. Use [`KernelEventCategory::primitive`] for the three-primitives rollup.
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

impl KernelEventCategory {
    /// Roll this fine-grained category up to its kernel primitive. `Proc` and `Ipc` are facets of
    /// the P2 scheduler (process table = task table; signals feed the scheduler).
    pub fn primitive(self) -> Primitive {
        match self {
            Self::Syscall => Primitive::Syscall,
            Self::Sched | Self::Proc | Self::Ipc => Primitive::Sched,
            Self::Mm => Primitive::Mm,
        }
    }
}

/// The kernel primitive an observation/session `kind` belongs to (three-primitives rollup).
pub fn primitive_for_kind(kind: &str) -> Primitive {
    category_for_kind(kind).primitive()
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
        | "milestone_blocked" => KernelEventCategory::Sched,
        "compressed"
        | "page_out"
        | "page_in"
        | "renewed"
        | "context_renewed"
        | "large_result_spooled" => KernelEventCategory::Mm,
        "agent_process_changed" | "agent_spawned" | "workflow_batch_spawned"
        | "workflow_completed" | "agent_preempted" => KernelEventCategory::Proc,
        "signal_disposed" => KernelEventCategory::Ipc,
        "memory_written" | "memory_queried" | "memory_validation_failed" => KernelEventCategory::Mm,
        _ => KernelEventCategory::Sched,
    }
}

/// All kernel observation kinds that should appear in a unified OS event log.
pub const KERNEL_OBSERVATION_KINDS: &[&str] = &[
    "compressed",
    "page_out",
    "renewed",
    "rollbacked",
    "capability_changed",
    "milestone_advanced",
    "milestone_blocked",
    "checkpoint_taken",
    "agent_process_changed",
    "workflow_batch_spawned",
    "workflow_completed",
    "agent_preempted",
    "tool_gated",
    "signal_disposed",
    "budget_exceeded",
    "suspended",
    "resumed",
    "memory_written",
    "memory_queried",
    "memory_validation_failed",
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

    #[test]
    fn categories_roll_up_to_three_primitives() {
        // Proc and Ipc are facets of the P2 scheduler.
        assert_eq!(KernelEventCategory::Syscall.primitive(), Primitive::Syscall);
        assert_eq!(KernelEventCategory::Sched.primitive(), Primitive::Sched);
        assert_eq!(KernelEventCategory::Proc.primitive(), Primitive::Sched);
        assert_eq!(KernelEventCategory::Ipc.primitive(), Primitive::Sched);
        assert_eq!(KernelEventCategory::Mm.primitive(), Primitive::Mm);
    }

    #[test]
    fn every_kernel_observation_kind_maps_to_a_primitive() {
        // syscall trap, scheduler, and paging cover the entire ABI surface — no orphans.
        for kind in KERNEL_OBSERVATION_KINDS {
            let p = primitive_for_kind(kind);
            assert!(matches!(p, Primitive::Syscall | Primitive::Sched | Primitive::Mm));
        }
        assert_eq!(primitive_for_kind("agent_process_changed"), Primitive::Sched);
        assert_eq!(primitive_for_kind("signal_disposed"), Primitive::Sched);
        assert_eq!(primitive_for_kind("tool_gated"), Primitive::Syscall);
        assert_eq!(primitive_for_kind("page_out"), Primitive::Mm);
    }
}
