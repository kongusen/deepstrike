//! Declarative resource quotas evaluated at the single syscall trap (M2 资源配额).
//!
//! The syscall gate ([`crate::scheduler::state_machine::LoopStateMachine::gate_syscall`]) is the
//! one chokepoint where effectful requests (`Invoke`/`Spawn`/`WriteMemory`/…) are adjudicated.
//! Governance rules already gate tool *invocation*; this adds the OS notion of **resource
//! quotas** to the *same* gate — without a new ABI shape — so spawning and memory writes become
//! bounded resources rather than unconditional `Allow`s.
//!
//! The kernel stays pure: a quota is declarative config + the facts the kernel already tracks
//! (running child tasks in the `TaskTable`, write timestamps from the observed clock). No I/O.

use serde::{Deserialize, Serialize};

/// Opt-in resource limits. An unset field imposes no limit; an unset `ResourceQuota` (the default,
/// when [`crate::scheduler::state_machine::LoopStateMachine::set_resource_quota`] is never called)
/// preserves the pre-M2 behavior of unconditional `Allow` for spawn / memory syscalls.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceQuota {
    /// Max sub-agents in the `Running` state at once. Further `Spawn`s are denied while at cap.
    /// *Instantaneous* — vehicle-scoped (cannot span stateless replicas; spec §2.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_subagents: Option<u32>,
    /// L1 (RunGroup): max sub-agents spawned *cumulatively* over the whole governance domain. Unlike
    /// `max_concurrent_subagents` this counts every spawn ever (running + completed), seeded across
    /// members via reservation-backed budget grants, so it spans N stateless top-level runs. A hard `Deny` at cap
    /// (a completed sibling never frees a cumulative slot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_subagents: Option<u32>,
    /// Max sub-agent nesting depth (direct children of the root loop are depth 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_depth: Option<u32>,
    /// Rolling-window memory-write rate limit as `(max_writes, window_ms)`: at most `max_writes`
    /// successful `WriteMemory` syscalls may occur within any `window_ms` span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_writes_per_window: Option<(u32, u64)>,
    /// R3-1: max total nodes a single workflow DAG may grow to via runtime `SubmitNodes`. Once the
    /// DAG (existing + submitted) would exceed this, the submission is denied — a backstop against an
    /// unbounded loop-until-done. `None` = no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workflow_nodes: Option<usize>,
}
