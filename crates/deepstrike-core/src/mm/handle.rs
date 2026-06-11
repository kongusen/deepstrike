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
    /// Link back to the source object in working context — for [`HandleKind::ToolResult`] this is
    /// the tool `call_id`, letting the renderer project a handle's residency onto its message
    /// (read-time projection) without mutating the stored message. `None` for handles with no
    /// in-context anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CompactString>,
}

impl Handle {
    pub fn resident(id: HandleId, kind: HandleKind, tokens: u32) -> Self {
        Self { id, kind, residency: Residency::Resident, tokens, source: None }
    }

    /// A resident handle anchored to a source object (e.g. a tool `call_id`).
    pub fn resident_for(
        id: HandleId,
        kind: HandleKind,
        tokens: u32,
        source: impl Into<CompactString>,
    ) -> Self {
        Self { id, kind, residency: Residency::Resident, tokens, source: Some(source.into()) }
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

    /// Sum of tokens for handles that have left working context (`Collapsed` / `SpooledOut` /
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
#[derive(Debug, Clone)]
pub enum EvictionOp {
    /// Layer 1: spool a large handle to disk, keep a preview reference in context.
    Spool(HandleId),
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
            Self::Spool(_) => "spool",
            Self::Snip { .. } => "snip",
            Self::TimeDecayMicro => "time_decay_micro",
            Self::Collapse { .. } => "collapse",
            Self::AutoCompact { .. } => "auto_compact",
        }
    }

    /// Cache-aware metadata: the message index at which this op invalidates the prompt cache
    /// prefix, if any. `None` = prefix-safe (op only affects late content or is layer-1 spool).
    /// Earlier index = higher cache cost (Anthropic cache keys off the first N messages).
    pub fn invalidates_prefix_at(&self) -> Option<usize> {
        match self {
            // Spool: layer-1 disk spool of single large result; no message reordering → no impact.
            Self::Spool(_) => None,
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
        self.ops.iter().any(|op| matches!(op, EvictionOp::TimeDecayMicro))
    }

    /// Map legacy `PressureAction` → the new specific op (for behavior-preserving migration).
    /// The old `recommend()` returns one of 5 actions; we map them 1:1 onto the new ops.
    pub fn from_legacy_action(action: PressureAction, target_tokens: u32, preserve_turns: usize) -> Self {
        let ops = match action {
            PressureAction::None => vec![],
            PressureAction::SnipCompact => vec![EvictionOp::Snip { per_msg_ratio: 0.10 }],
            PressureAction::MicroCompact => vec![EvictionOp::TimeDecayMicro],
            PressureAction::ContextCollapse => vec![EvictionOp::Collapse { target_tokens }],
            PressureAction::AutoCompact => vec![EvictionOp::AutoCompact { preserve_turns }],
        };
        Self { ops }
    }
}

/// Layer-1 spool decision for a single tool result (kernel decides; SDK writes to disk).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolDecision {
    /// Byte size of the full (un-spooled) output.
    pub original_size: u32,
    /// The preview text the kernel keeps in working context in place of the full output.
    pub preview: String,
}

/// Pure Layer-1 spool planner: if `output` exceeds `threshold_bytes` (and threshold > 0), return a
/// [`SpoolDecision`] whose `preview` is the first `preview_bytes` (truncated at a char boundary)
/// plus a marker. `None` means keep the output inline. The kernel keeps `preview` in context and
/// emits `LargeResultSpooled`; the SDK persists the full content to disk. No I/O here.
pub fn plan_spool(output: &str, threshold_bytes: u32, preview_bytes: u32) -> Option<SpoolDecision> {
    let size = output.len();
    if threshold_bytes == 0 || size <= threshold_bytes as usize {
        return None;
    }
    let mut end = (preview_bytes as usize).min(size);
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    let preview = format!(
        "{}\n[…tool result spooled: {} bytes total, {} byte preview shown; full content persisted to disk by the SDK…]",
        &output[..end], size, end
    );
    Some(SpoolDecision { original_size: size as u32, preview })
}

/// Pure eviction planner (M3): the **single decision point** for the per-turn compression
/// checkpoint. Packages the two previously-scattered decisions — Layer-3 idle/time-decay and the
/// rho-driven pressure recommendation — into one ordered [`EvictionPlan`], in execution order
/// (time-decay micro first, then the pressure action). Behavior-preserving: the inputs are exactly
/// what the state machine already computed (`ContextManager::should_time_decay_compact` and
/// `PressureMonitor::recommend`); this only centralizes their ordering and makes the plan testable.
///
/// Layer-1 spool is decided at tool-result ingestion (handle size), not here.
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
        ops.extend(EvictionPlan::from_legacy_action(recommended, target_tokens, preserve_turns).ops);
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
        assert!(!Residency::PagedOut { tier: MemoryTierHint::Semantic }.occupies_context());
    }

    #[test]
    fn plan_eviction_empty_when_no_pressure_and_no_idle() {
        assert!(plan_eviction(PressureAction::None, false, 50_000, 2).is_empty());
    }

    #[test]
    fn plan_eviction_emits_specific_op_for_recommended_action() {
        let plan = plan_eviction(PressureAction::AutoCompact, false, 50_000, 3);
        // The op carries the real preserve_turns the caller passed, not a placeholder.
        assert!(matches!(&plan.ops[..], [EvictionOp::AutoCompact { preserve_turns: 3 }]));
    }

    #[test]
    fn plan_eviction_collapse_carries_caller_target_tokens() {
        // W1-1 收口: the planner stamps the caller's real target into the Collapse op (no placeholder),
        // and the executor honors it verbatim.
        let plan = plan_eviction(PressureAction::ContextCollapse, false, 12_345, 2);
        assert!(matches!(&plan.ops[..], [EvictionOp::Collapse { target_tokens: 12_345 }]));
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
        assert!(plan.has_time_decay(), "MicroCompact yields a time-decay op even when not idle");
        // And the checkpoint invariant the fixed assertion encodes holds for every combination:
        for recommended in [
            PressureAction::None,
            PressureAction::MicroCompact,
            PressureAction::AutoCompact,
            PressureAction::ContextCollapse,
        ] {
            for idle in [false, true] {
                let p = plan_eviction(recommended, idle, 50_000, 2);
                assert!(!idle || p.has_time_decay(), "idle_decay must imply a time-decay op");
            }
        }
    }

    #[test]
    fn eviction_op_labels() {
        assert_eq!(EvictionOp::Spool(1).label(), "spool");
        assert_eq!(EvictionOp::Snip { per_msg_ratio: 0.1 }.label(), "snip");
        assert_eq!(EvictionOp::TimeDecayMicro.label(), "time_decay_micro");
        assert_eq!(EvictionOp::Collapse { target_tokens: 5000 }.label(), "collapse");
        assert_eq!(EvictionOp::AutoCompact { preserve_turns: 2 }.label(), "auto_compact");
    }

    #[test]
    fn plan_spool_keeps_small_output_inline() {
        assert_eq!(plan_spool("small", 50, 16), None);
        // threshold 0 disables spooling.
        assert_eq!(plan_spool(&"x".repeat(1000), 0, 16), None);
    }

    #[test]
    fn plan_spool_previews_large_output() {
        let output = "y".repeat(1000);
        let d = plan_spool(&output, 100, 32).expect("should spool");
        assert_eq!(d.original_size, 1000);
        assert!(d.preview.starts_with(&"y".repeat(32)));
        assert!(d.preview.contains("1000 bytes total"));
        assert!(d.preview.len() < output.len());
    }

    #[test]
    fn plan_spool_truncates_on_char_boundary() {
        // multi-byte chars: preview cut must not split a char.
        let output = "🚀".repeat(100); // 4 bytes each = 400 bytes
        let d = plan_spool(&output, 50, 10).expect("should spool");
        // No panic / valid UTF-8 preview is the assertion.
        assert!(d.preview.contains("400 bytes total"));
    }
}
