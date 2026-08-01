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

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::context::pressure::PressureAction;

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
    /// A sub-agent join result occupying context.
    SubAgentJoin,
}

/// Where a handle's content currently lives. Page-in/page-out are transitions on this.
///
/// [`Self::External`] and [`Self::PagedOut`] are deliberately distinct (§7.10, cluster-b B19): the
/// first is "generated over the inline threshold and never was resident", the second is "was
/// resident, archived under context pressure".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Residency {
    /// Full content present in working context.
    Resident,
    /// §7.10 · the body was over the inline threshold when it was generated: the host persisted it
    /// before the kernel ever saw it, and only `preview` was ever resident.
    External {
        /// Opaque host locator. Never a path, never joined, never opened by the kernel.
        payload_ref: String,
        digest: String,
        original_size: u64,
    },
    /// §7.10 · the body *was* resident and left under context pressure (page-out archive).
    PagedOut { payload_ref: String, digest: String },
    /// Original kept locally but projected out of the rendered view (Layer 4 read-time projection).
    Collapsed,
}

impl Residency {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Resident => "resident",
            Self::External { .. } => "external",
            Self::PagedOut { .. } => "paged_out",
            Self::Collapsed => "collapsed",
        }
    }

    /// Whether the handle's full content currently counts against the token budget.
    pub fn occupies_context(&self) -> bool {
        matches!(self, Self::Resident)
    }

    /// The opaque locator a page-in must hand back to the host, when one exists.
    ///
    /// `None` for every residency the kernel can satisfy on its own — `Resident` and `Collapsed`
    /// still hold the body locally.
    pub fn payload_ref(&self) -> Option<&str> {
        match self {
            Self::External { payload_ref, .. } | Self::PagedOut { payload_ref, .. } => {
                Some(payload_ref.as_str())
            }
            Self::Resident | Self::Collapsed => None,
        }
    }

    /// The digest a paged-in body must reproduce. Paired with [`Self::payload_ref`]: a residency
    /// that can be loaded is exactly one that can be verified.
    pub fn digest(&self) -> Option<&str> {
        match self {
            Self::External { digest, .. } | Self::PagedOut { digest, .. } => Some(digest.as_str()),
            Self::Resident | Self::Collapsed => None,
        }
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
    /// Link back to the source object in working context — for [`HandleKind::ToolResult`] this is
    /// the tool `call_id`, letting the renderer project a handle's residency onto its message
    /// (read-time projection) without mutating the stored message. `None` for handles with no
    /// in-context anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CompactString>,
}

impl Handle {
    pub fn resident(id: HandleId, kind: HandleKind, tokens: u32) -> Self {
        Self {
            id,
            kind,
            residency: Residency::Resident,
            tokens,
            source: None,
        }
    }

    /// A resident handle anchored to a source object (e.g. a tool `call_id`).
    pub fn resident_for(
        id: HandleId,
        kind: HandleKind,
        tokens: u32,
        source: impl Into<CompactString>,
    ) -> Self {
        Self {
            id,
            kind,
            residency: Residency::Resident,
            tokens,
            source: Some(source.into()),
        }
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

    pub fn get_mut(&mut self, id: HandleId) -> Option<&mut Handle> {
        self.handles.iter_mut().find(|h| h.id == id)
    }

    pub fn all(&self) -> &[Handle] {
        &self.handles
    }

    pub fn all_mut(&mut self) -> &mut [Handle] {
        &mut self.handles
    }

    /// Retain only the handles for which `keep` returns true; drop the rest. The GC primitive the
    /// context manager uses to evict handles whose backing message has left working context
    /// (archived by compression / dropped on renewal) — bounding the table to the working set
    /// instead of growing with total session length.
    pub fn retain(&mut self, keep: impl FnMut(&Handle) -> bool) {
        self.handles.retain(keep);
    }

    /// Residency of the handle anchored to `source` (e.g. a tool `call_id`), if any.
    /// The renderer uses this to project a tool result without touching the stored message.
    pub fn residency_for_source(&self, source: &str) -> Option<&Residency> {
        self.handles
            .iter()
            .find(|h| h.source.as_deref() == Some(source))
            .map(|h| &h.residency)
    }

    /// Tool-result handles in insertion (recency) order — oldest first. Used by the residency
    /// planner to decide which older results to project out under context pressure.
    pub fn tool_result_handles_mut(&mut self) -> impl Iterator<Item = &mut Handle> {
        self.handles
            .iter_mut()
            .filter(|h| matches!(h.kind, HandleKind::ToolResult))
    }

    /// Sum of tokens for handles still occupying working context.
    pub fn resident_tokens(&self) -> u32 {
        self.handles
            .iter()
            .filter(|h| h.residency.occupies_context())
            .map(|h| h.tokens)
            .sum()
    }

    /// Sum of tokens for handles that have left working context (`Collapsed` / `External` /
    /// `PagedOut`). Their anchored messages still sit in `partitions` at full weight (collapse is
    /// non-destructive), so this is exactly the over-count that the *estimate* rho path must
    /// discount to become paging-aware — see [`crate::context::manager::ContextManager::effective_rho`].
    pub fn non_resident_tokens(&self) -> u32 {
        self.handles
            .iter()
            .filter(|h| !h.residency.occupies_context())
            .map(|h| h.tokens)
            .sum()
    }
}

/// One ordered eviction action in an [`EvictionPlan`]. Maps the pressure pyramid onto explicit
/// ops the planner emits directly (the old `Pressure(PressureAction)` umbrella is deleted), each
/// annotated with cache-aware metadata via [`EvictionOp::invalidates_prefix_at`].
///
/// P1-6 (async LLM semantic summary) is **not** a distinct op here: every archiving op already
/// emits the drained messages as `archived` on the `Compressed` observation, and the SDK upgrades
/// that summary out-of-band (LLM call = SDK I/O, a kernel non-goal), writing back a second
/// `compressed` event. A separate in-kernel `Summarize` op would be a never-produced dead variant.
///
/// **Layer boundary vs [`crate::context::pressure::PressureAction`] (do not collapse the two):**
/// `EvictionOp` is the *planner-op* vocabulary — what `plan_eviction` decides to do, carrying the
/// per-op payload (`target_tokens` / `per_msg_ratio` / `preserve_turns`). `PressureAction` is the
/// *pressure-level* vocabulary owned by the pressure subsystem: it is what `PressureMonitor::recommend`
/// and `ContextManager::should_compress` return, the `Ord`-keyed cascade selector inside the
/// compression pipeline, and the canonical wire label. They map ~1:1 by layer but are not redundant —
/// `TimeDecayMicro` doesn't sit on the linear pressure cascade, and `PressureAction` carries no
/// per-op data. The one bridge is `execute_eviction_op`, which is the intended seam, not duplication.
#[derive(Debug, Clone)]
pub enum EvictionOp {
    /// Layer 2: cap oversized messages at a per-message token limit (in-place rewrite).
    Snip { per_msg_ratio: f64 },
    /// Layer 3: idle/time-decay micro-compact — excerpt large tool results to placeholders.
    /// Independent of rho; stamps `last_compact_ms` and uses the non-time compress path.
    TimeDecayMicro,
    /// Layer 4: collapse (read-time projection) — drop oldest messages until within target.
    /// Now a distinct op (no longer bundled under `Pressure`), so the planner can annotate it
    /// with cache-aware metadata and order it explicitly.
    Collapse { target_tokens: u32 },
    /// Layer 5: auto-compact — collapse history entirely except last K turns. Distinct from Collapse
    /// for the same reason: the planner needs to control ordering and metadata.
    AutoCompact { preserve_turns: usize },
}

impl EvictionOp {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Snip { .. } => "snip",
            Self::TimeDecayMicro => "time_decay_micro",
            Self::Collapse { .. } => "collapse",
            Self::AutoCompact { .. } => "auto_compact",
        }
    }

    /// Cache-aware metadata: the message index at which this op invalidates the prompt cache
    /// prefix, if any. `None` = prefix-safe (op only affects late content).
    /// Earlier index = higher cache cost (Anthropic cache keys off the first N messages).
    pub fn invalidates_prefix_at(&self) -> Option<usize> {
        match self {
            // Snip: in-place rewrite of oversized messages anywhere in history. May hit early
            // messages if an early turn was oversized → conservative: assume prefix invalidation.
            Self::Snip { .. } => Some(0), // Conservative: may affect any message including early ones.
            // TimeDecayMicro: excerpts large tool results to placeholders. Tool results are always
            // interleaved (after their call), so they're typically mid/late history. Assuming the
            // system prompt + first few user messages are untouched → prefix-safe for most sessions.
            Self::TimeDecayMicro => None,
            // Collapse: drops oldest messages to reach target. By definition modifies early history
            // → prefix invalidation at the drop point.
            Self::Collapse { .. } => Some(0),
            // AutoCompact: drops all but last K turns → even more aggressive prefix invalidation.
            Self::AutoCompact { .. } => Some(0),
        }
    }
}

/// An ordered set of eviction actions returned by the planner. Empty = no compression needed
/// ("能不压就不压"). The order is the execution order.
#[derive(Debug, Clone, Default)]
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
        self.ops
            .iter()
            .any(|op| matches!(op, EvictionOp::TimeDecayMicro))
    }

    /// Map legacy `PressureAction` → the new specific op (for behavior-preserving migration).
    /// The old `recommend()` returns one of 5 actions; we map them 1:1 onto the new ops.
    pub fn from_legacy_action(
        action: PressureAction,
        target_tokens: u32,
        preserve_turns: usize,
    ) -> Self {
        let ops = match action {
            PressureAction::None => vec![],
            PressureAction::SnipCompact => vec![EvictionOp::Snip {
                per_msg_ratio: 0.10,
            }],
            PressureAction::MicroCompact => vec![EvictionOp::TimeDecayMicro],
            PressureAction::ContextCollapse => vec![EvictionOp::Collapse { target_tokens }],
            PressureAction::AutoCompact => vec![EvictionOp::AutoCompact { preserve_turns }],
        };
        Self { ops }
    }
}

/// Pure eviction planner (M3): the **single decision point** for the per-turn compression
/// checkpoint. Packages the two previously-scattered decisions — Layer-3 idle/time-decay and the
/// rho-driven pressure recommendation — into one ordered [`EvictionPlan`], in execution order
/// (time-decay micro first, then the pressure action). Behavior-preserving: the inputs are exactly
/// what the state machine already computed (`ContextManager::should_time_decay_compact` and
/// `PressureMonitor::recommend`); this only centralizes their ordering and makes the plan testable.
///
/// W1-1 收口: `target_tokens` / `preserve_turns` are the **real** config-derived values supplied by
/// the caller (`ContextManager::plan_compaction_params`), so the emitted ops carry truthful params
/// instead of the old magic-number placeholders. The plan is now the single decision point for *what*
/// to compact and *to what target*; the executor honors `Collapse { target_tokens }` verbatim rather
/// than re-deriving it. (The richer `(rho, idle_ms, &HandleTable, &cfg)` signature with explicit
/// cache-cost ordering remains a future refinement; the `invalidates_prefix_at` metadata is already
/// carried per op.)
pub fn plan_eviction(
    recommended: PressureAction,
    idle_decay: bool,
    target_tokens: u32,
    preserve_turns: usize,
) -> EvictionPlan {
    let mut ops = Vec::new();
    if idle_decay {
        ops.push(EvictionOp::TimeDecayMicro);
    }
    // Map the pressure recommendation to a specific op; `None` yields an empty plan (no op appended).
    if recommended != PressureAction::None {
        ops.extend(
            EvictionPlan::from_legacy_action(recommended, target_tokens, preserve_turns).ops,
        );
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
            kind: HandleKind::ToolResult,
            residency: Residency::External {
                payload_ref: "payload:x".into(),
                digest: "sha256:".to_string() + &"a".repeat(64),
                original_size: 5_000,
            },
            tokens: 5000,
            source: None,
        });
        table.insert(Handle {
            id: 3,
            kind: HandleKind::MemoryPage,
            residency: Residency::Collapsed,
            tokens: 200,
            source: None,
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
        assert!(!paged_out().occupies_context());
        assert!(!external().occupies_context());
    }

    fn external() -> Residency {
        Residency::External {
            payload_ref: "payload:01J".into(),
            digest: "sha256:".to_string() + &"a".repeat(64),
            original_size: 90_000,
        }
    }

    fn paged_out() -> Residency {
        Residency::PagedOut {
            payload_ref: "payload:02K".into(),
            digest: "sha256:".to_string() + &"b".repeat(64),
        }
    }

    /// §7.10 · a residency is loadable exactly when it is verifiable: the two accessors agree on
    /// the same set, so no page-in can address a body it could not then check.
    #[test]
    fn only_externally_backed_residencies_expose_a_locator_and_a_digest() {
        for residency in [external(), paged_out()] {
            assert!(residency.payload_ref().is_some(), "{residency:?}");
            assert!(residency.digest().is_some(), "{residency:?}");
        }
        for residency in [Residency::Resident, Residency::Collapsed] {
            assert_eq!(residency.payload_ref(), None, "{residency:?}");
            assert_eq!(residency.digest(), None, "{residency:?}");
        }
    }

    /// B19 · "generated over the limit" and "evicted under pressure" are different facts, and the
    /// label is what a host event log reads them by.
    #[test]
    fn external_and_paged_out_are_distinguishable_states() {
        assert_eq!(external().label(), "external");
        assert_eq!(paged_out().label(), "paged_out");
        assert_ne!(external(), paged_out());
    }

    #[test]
    fn plan_eviction_empty_when_no_pressure_and_no_idle() {
        assert!(plan_eviction(PressureAction::None, false, 50_000, 2).is_empty());
    }

    #[test]
    fn plan_eviction_emits_specific_op_for_recommended_action() {
        let plan = plan_eviction(PressureAction::AutoCompact, false, 50_000, 3);
        // The op carries the real preserve_turns the caller passed, not a placeholder.
        assert!(matches!(
            &plan.ops[..],
            [EvictionOp::AutoCompact { preserve_turns: 3 }]
        ));
    }

    #[test]
    fn plan_eviction_collapse_carries_caller_target_tokens() {
        // W1-1 收口: the planner stamps the caller's real target into the Collapse op (no placeholder),
        // and the executor honors it verbatim.
        let plan = plan_eviction(PressureAction::ContextCollapse, false, 12_345, 2);
        assert!(matches!(
            &plan.ops[..],
            [EvictionOp::Collapse {
                target_tokens: 12_345
            }]
        ));
    }

    #[test]
    fn plan_eviction_orders_time_decay_before_pressure() {
        // Idle + rho both fire: time-decay micro runs first, then the specific op — matching
        // the legacy checkpoint order exactly.
        let plan = plan_eviction(PressureAction::ContextCollapse, true, 50_000, 2);
        assert_eq!(plan.ops.len(), 2);
        assert!(matches!(plan.ops[0], EvictionOp::TimeDecayMicro));
        assert!(matches!(plan.ops[1], EvictionOp::Collapse { .. }));
    }

    #[test]
    fn plan_eviction_time_decay_only() {
        let plan = plan_eviction(PressureAction::None, true, 50_000, 2);
        assert_eq!(plan.ops.len(), 1);
        assert!(matches!(plan.ops[0], EvictionOp::TimeDecayMicro));
    }

    #[test]
    fn plan_eviction_micro_compact_emits_time_decay_without_idle() {
        // Regression: a pressure-driven MicroCompact emits a TimeDecayMicro op *independent* of the
        // idle-decay flag. So `has_time_decay()` can be true while `idle_decay` is false — the state
        // machine's compaction checkpoint must assert the implication (`idle_decay ⇒ has_time_decay`),
        // NOT equality (the old `debug_assert_eq!(has_time_decay, idle_decay)` wrongly aborted here).
        let plan = plan_eviction(PressureAction::MicroCompact, false, 50_000, 2);
        assert!(
            plan.has_time_decay(),
            "MicroCompact yields a time-decay op even when not idle"
        );
        // And the checkpoint invariant the fixed assertion encodes holds for every combination:
        for recommended in [
            PressureAction::None,
            PressureAction::MicroCompact,
            PressureAction::AutoCompact,
            PressureAction::ContextCollapse,
        ] {
            for idle in [false, true] {
                let p = plan_eviction(recommended, idle, 50_000, 2);
                assert!(
                    !idle || p.has_time_decay(),
                    "idle_decay must imply a time-decay op"
                );
            }
        }
    }

    #[test]
    fn eviction_op_labels() {
        assert_eq!(EvictionOp::Snip { per_msg_ratio: 0.1 }.label(), "snip");
        assert_eq!(EvictionOp::TimeDecayMicro.label(), "time_decay_micro");
        assert_eq!(
            EvictionOp::Collapse {
                target_tokens: 5000
            }
            .label(),
            "collapse"
        );
        assert_eq!(
            EvictionOp::AutoCompact { preserve_turns: 2 }.label(),
            "auto_compact"
        );
    }
}
