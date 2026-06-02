//! Primitive P3: the resource handle table + paging (context as address space).
//!
//! M0 scaffold (see `.local-docs/specs/agent-os-three-primitives.md`): types + a pure
//! eviction-plan stub only — **no wiring, no behavior change**. A later milestone (M3, which is the
//! compression optimization) builds a [`HandleTable`] over the context manager and replaces the
//! scattered compactors in [`crate::context::compression`] with a single pure [`plan_eviction`].
//!
//! Concept overlap this primitive collapses: the 5-layer compression pyramid (5 compactors each
//! deciding its own trigger) becomes one [`EvictionPlan`] of uniform [`EvictionOp`]s; page-out (④)
//! and long-term memory residency (⑦) ride on [`Residency`].

use serde::{Deserialize, Serialize};

use crate::context::pressure::PressureAction;
use crate::mm::MemoryTierHint;

/// Opaque handle id. M3 assigns these as tool results / knowledge / memory pages enter context.
pub type HandleId = u32;

/// What a handle refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandleKind {
    /// A tool result occupying working context.
    ToolResult,
    /// A working-memory page (compressible / pageable).
    MemoryPage,
    /// A knowledge entry paged in from long-term storage.
    KnowledgeEntry,
    /// A large result spooled to disk with a preview left in context (Layer 1).
    SpoolFile,
    /// A sub-agent join result occupying context.
    SubAgentJoin,
}

/// Where a handle's content currently lives. Page-in/page-out are transitions on this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Residency {
    /// Full content present in working context.
    Resident,
    /// Content written to disk; a preview reference remains (Layer 1 spool).
    SpooledOut { r: String },
    /// Content archived to long-term storage at the given tier (page-out).
    PagedOut { tier: MemoryTierHint },
    /// Original kept locally but projected out of the rendered view (Layer 4 read-time projection).
    Collapsed,
}

impl Residency {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Resident => "resident",
            Self::SpooledOut { .. } => "spooled_out",
            Self::PagedOut { .. } => "paged_out",
            Self::Collapsed => "collapsed",
        }
    }

    /// Whether the handle's full content currently counts against the token budget.
    pub fn occupies_context(&self) -> bool {
        matches!(self, Self::Resident)
    }
}

/// One addressable resource the agent holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handle {
    pub id: HandleId,
    pub kind: HandleKind,
    pub residency: Residency,
    /// Token cost of the resident form (used by the eviction planner).
    pub tokens: u32,
}

impl Handle {
    pub fn resident(id: HandleId, kind: HandleKind, tokens: u32) -> Self {
        Self { id, kind, residency: Residency::Resident, tokens }
    }
}

/// Per-task handle table. M3 makes the context manager's partitions a view over this.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandleTable {
    handles: Vec<Handle>,
}

impl HandleTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, handle: Handle) {
        if let Some(existing) = self.handles.iter_mut().find(|h| h.id == handle.id) {
            *existing = handle;
        } else {
            self.handles.push(handle);
        }
    }

    pub fn get(&self, id: HandleId) -> Option<&Handle> {
        self.handles.iter().find(|h| h.id == id)
    }

    pub fn all(&self) -> &[Handle] {
        &self.handles
    }

    /// Sum of tokens for handles still occupying working context.
    pub fn resident_tokens(&self) -> u32 {
        self.handles
            .iter()
            .filter(|h| h.residency.occupies_context())
            .map(|h| h.tokens)
            .sum()
    }
}

/// One ordered eviction action in an [`EvictionPlan`]. The 5-layer pyramid maps onto these,
/// preserving the distinct compactors the engine already has:
/// L1 → [`EvictionOp::Spool`]; L3 time-decay → [`EvictionOp::TimeDecayMicro`];
/// L2/L4/L5 (and rho-driven micro) → [`EvictionOp::Pressure`] carrying the exact [`PressureAction`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvictionOp {
    /// Layer 1: spool a large handle to disk, keep a preview reference.
    Spool(HandleId),
    /// Layer 3: idle/time-decay micro-compact (`MicroCompact`), independent of rho. Distinct from a
    /// rho-driven action because it stamps `last_compact_ms` and uses the non-time compress path.
    TimeDecayMicro,
    /// Layers 2/4/5 (+ rho-driven micro): the pressure-recommended compaction action. `None` is
    /// never emitted (the planner omits the op entirely when no compaction is needed).
    Pressure(PressureAction),
}

impl EvictionOp {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Spool(_) => "spool",
            Self::TimeDecayMicro => "time_decay_micro",
            Self::Pressure(_) => "pressure",
        }
    }
}

/// An ordered set of eviction actions returned by the planner. Empty = no compression needed
/// ("能不压就不压"). The order is the execution order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictionPlan {
    pub ops: Vec<EvictionOp>,
}

impl EvictionPlan {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Whether the plan includes the Layer-3 idle/time-decay micro op.
    pub fn has_time_decay(&self) -> bool {
        self.ops.iter().any(|op| matches!(op, EvictionOp::TimeDecayMicro))
    }

    /// The pressure-driven compaction action in the plan, if any (Layers 2/4/5 + rho micro).
    pub fn pressure_action(&self) -> Option<PressureAction> {
        self.ops.iter().find_map(|op| match op {
            EvictionOp::Pressure(a) => Some(*a),
            _ => None,
        })
    }
}

/// Pure eviction planner (M3): the **single decision point** for the per-turn compression
/// checkpoint. Packages the two previously-scattered decisions — Layer-3 idle/time-decay and the
/// rho-driven pressure recommendation — into one ordered [`EvictionPlan`], in execution order
/// (time-decay micro first, then the pressure action). Behavior-preserving: the inputs are exactly
/// what the state machine already computed (`ContextManager::should_time_decay_compact` and
/// `PressureMonitor::recommend`); this only centralizes their ordering and makes the plan testable.
///
/// Layer-1 spool is decided at tool-result ingestion (handle size), not here.
pub fn plan_eviction(recommended: PressureAction, idle_decay: bool) -> EvictionPlan {
    let mut ops = Vec::new();
    if idle_decay {
        ops.push(EvictionOp::TimeDecayMicro);
    }
    if recommended != PressureAction::None {
        ops.push(EvictionOp::Pressure(recommended));
    }
    EvictionPlan { ops }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_tokens_counts_only_resident() {
        let mut table = HandleTable::new();
        table.insert(Handle::resident(1, HandleKind::ToolResult, 100));
        table.insert(Handle {
            id: 2,
            kind: HandleKind::SpoolFile,
            residency: Residency::SpooledOut { r: "disk://x".into() },
            tokens: 5000,
        });
        table.insert(Handle {
            id: 3,
            kind: HandleKind::MemoryPage,
            residency: Residency::Collapsed,
            tokens: 200,
        });
        assert_eq!(table.resident_tokens(), 100);
    }

    #[test]
    fn handle_table_insert_is_idempotent_by_id() {
        let mut table = HandleTable::new();
        table.insert(Handle::resident(1, HandleKind::ToolResult, 100));
        table.insert(Handle::resident(1, HandleKind::ToolResult, 250));
        assert_eq!(table.all().len(), 1);
        assert_eq!(table.get(1).unwrap().tokens, 250);
    }

    #[test]
    fn residency_occupies_context_only_when_resident() {
        assert!(Residency::Resident.occupies_context());
        assert!(!Residency::Collapsed.occupies_context());
        assert!(!Residency::PagedOut { tier: MemoryTierHint::Semantic }.occupies_context());
    }

    #[test]
    fn plan_eviction_empty_when_no_pressure_and_no_idle() {
        assert!(plan_eviction(PressureAction::None, false).is_empty());
    }

    #[test]
    fn plan_eviction_emits_pressure_op_for_recommended_action() {
        let plan = plan_eviction(PressureAction::AutoCompact, false);
        assert_eq!(plan.ops, vec![EvictionOp::Pressure(PressureAction::AutoCompact)]);
    }

    #[test]
    fn plan_eviction_orders_time_decay_before_pressure() {
        // Idle + rho both fire: time-decay micro runs first, then the pressure action — matching
        // the legacy checkpoint order exactly.
        let plan = plan_eviction(PressureAction::ContextCollapse, true);
        assert_eq!(
            plan.ops,
            vec![
                EvictionOp::TimeDecayMicro,
                EvictionOp::Pressure(PressureAction::ContextCollapse),
            ]
        );
    }

    #[test]
    fn plan_eviction_time_decay_only() {
        let plan = plan_eviction(PressureAction::None, true);
        assert_eq!(plan.ops, vec![EvictionOp::TimeDecayMicro]);
    }

    #[test]
    fn eviction_op_labels() {
        assert_eq!(EvictionOp::Spool(1).label(), "spool");
        assert_eq!(EvictionOp::TimeDecayMicro.label(), "time_decay_micro");
        assert_eq!(EvictionOp::Pressure(PressureAction::AutoCompact).label(), "pressure");
    }
}
