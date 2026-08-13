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

/// spc_006-05: what an [`ObjectDescriptor`] refers to. A pure additive extension over
/// [`HandleKind`] (see [`From<HandleKind> for ObjectKind`](#impl-From<HandleKind>-for-ObjectKind))
/// — every existing `HandleKind` maps onto exactly one `ObjectKind`, `HandleKind` itself is
/// untouched, and `Handle`/`HandleTable` keep working unmodified (spc_006 §4: Public
/// `Memory`/`Knowledge`/`Artifact`/`ToolResult` naming is unchanged; only the Kernel-internal
/// descriptor is unified).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    ToolResult,
    Memory,
    Knowledge,
    Artifact,
    AgentResult,
    Dataset,
    File,
    WorkflowOutput,
    Custom(CompactString),
}

impl From<HandleKind> for ObjectKind {
    fn from(kind: HandleKind) -> Self {
        match kind {
            HandleKind::ToolResult => Self::ToolResult,
            HandleKind::MemoryPage => Self::Memory,
            HandleKind::KnowledgeEntry => Self::Knowledge,
            HandleKind::SubAgentJoin => Self::Custom(CompactString::from("sub_agent_join")),
        }
    }
}

/// spc_006-05: the unified Kernel-internal descriptor spc_006 §4 targets for
/// ToolResult/Memory/Knowledge/Artifact/AgentResult (and the three IPC-facing kinds
/// Dataset/File/WorkflowOutput `ObjectKind` adds room for). Additive and parallel to
/// [`Handle`]/[`HandleTable`] — this card does not replace or migrate either.
///
/// Field type choices (spec left both open, resolved here):
/// - `id: ObjectId` reuses [`HandleId`] rather than a new id space — `ObjectDescriptor` is framed
///   throughout spc_006 §4-§5 as the *same* addressable-object concept `Handle` already is, just
///   generalized beyond in-context residency (this file's own header already anticipates the
///   context manager becoming "a view over" the handle table); a second parallel id space would
///   fight that convergence instead of serving it.
/// - `digest: String` and `payload_ref: Option<String>` mirror [`Residency::External`]'s own field
///   types exactly (spc_006-05's own instruction: "复用 Residency::External 里已有的 digest 类
///   型") — no `Digest`/`PayloadRef` newtype exists anywhere in this codebase to reuse, and
///   inventing one here would be exactly the "重新发明" the card says not to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectDescriptor {
    pub id: ObjectId,
    pub kind: ObjectKind,
    pub owner: crate::scheduler::tcb::TaskId,
    pub digest: String,
    pub size: u64,
    pub residency: Residency,
    pub payload_ref: Option<String>,
    pub version: u64,
    /// spc_006-06: a short excerpt a cross-task reader sees by default — never the full body.
    /// `None` for descriptors with no externally-stored content (e.g. small `Resident` objects
    /// short enough that the whole thing already fits, per `Residency::occupies_context`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<CompactString>,
}

impl ObjectDescriptor {
    /// Project an existing handle into the unified object registry without copying its body.
    pub fn from_handle(
        owner: crate::scheduler::tcb::TaskId,
        handle: &Handle,
        version: u64,
    ) -> Self {
        let digest = handle.residency.digest().unwrap_or_default().to_string();
        let payload_ref = handle.residency.payload_ref().map(str::to_string);
        let size = match &handle.residency {
            Residency::External { original_size, .. } => *original_size,
            _ => handle.tokens as u64,
        };
        Self {
            id: handle.id,
            kind: handle.kind.into(),
            owner,
            digest,
            size,
            residency: handle.residency.clone(),
            payload_ref,
            version,
            preview: None,
        }
    }

    /// spc_006-06: build the descriptor a cross-task reader (Agent B) receives for an object
    /// whose full body lives outside working context (Agent A's large Artifact/ToolResult/etc.).
    /// By construction — `ObjectDescriptor` has no `payload`/`content` field at all — a caller
    /// holding one physically cannot read the full body without a separate, explicit page-in
    /// (spc_006 §5: "Pass handles, not prompts").
    ///
    /// Takes an already-built [`Residency::External`] (rather than its three fields individually)
    /// so `payload_ref`/`digest` are entered once, not twice over — a caller passing the wrong
    /// [`Residency`] variant gets a clear panic rather than a descriptor silently missing its
    /// locator.
    pub fn external(
        id: ObjectId,
        kind: ObjectKind,
        owner: crate::scheduler::tcb::TaskId,
        version: u64,
        residency: Residency,
        preview: impl Into<CompactString>,
    ) -> Self {
        let Residency::External {
            payload_ref,
            digest,
            original_size,
        } = &residency
        else {
            panic!("ObjectDescriptor::external requires a Residency::External, got {residency:?}");
        };
        let (payload_ref, digest, size) = (payload_ref.clone(), digest.clone(), *original_size);
        Self {
            id,
            kind,
            owner,
            digest,
            size,
            residency,
            payload_ref: Some(payload_ref),
            version,
            preview: Some(preview.into()),
        }
    }
}

/// See [`ObjectDescriptor::id`]'s doc comment for why this is a `HandleId` alias, not a new type.
pub type ObjectId = HandleId;

/// spc_009-07 · the Object invariant plan.md §7.3 names "Capability controls object access":
/// bridges spc_004's [`Capability`] attenuation matching to spc_006's [`ObjectDescriptor`]. The
/// two structures were never designed to reference each other — `ObjectDescriptor` carries an
/// `owner: TaskId`, not a `ResourceSelector`-shaped field a `Capability.resource` could match
/// against directly — so this establishes the minimal resource-naming convention needed to bridge
/// them (`"object:{owner}/{id}"`), reusing [`resource_prefix`]'s exact prefix-containment rule
/// rather than inventing a second matching algorithm. Neither `ObjectDescriptor` nor `Capability`
/// is changed to make this work — the bridge is one pure function, not a structural merge.
///
pub fn object_access_allowed(
    capabilities: &[crate::types::capability::Capability],
    action: &str,
    descriptor: &ObjectDescriptor,
) -> bool {
    object_access_allowed_at(capabilities, action, descriptor, 0)
}

/// Runtime form of [`object_access_allowed`] that also rejects expired capability leases.
pub fn object_access_allowed_at(
    capabilities: &[crate::types::capability::Capability],
    action: &str,
    descriptor: &ObjectDescriptor,
    now_turn: u32,
) -> bool {
    let resource = format!("object:{}/{}", descriptor.owner, descriptor.id);
    capabilities.iter().any(|capability| {
        capability.actions.0.contains(action)
            && capability
                .lease
                .as_ref()
                .is_none_or(|lease| !lease.is_expired(now_turn))
            && crate::types::capability::resource_matches(&capability.resource, &resource)
    })
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

    /// Map a pressure recommendation to one specific eviction operation.
    /// The old `recommend()` returns one of 5 actions; we map them 1:1 onto the new ops.
    pub fn from_pressure_action(
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
            EvictionPlan::from_pressure_action(recommended, target_tokens, preserve_turns).ops,
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
        // the canonical checkpoint order exactly.
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

    #[test]
    fn spc_006_05_object_descriptor_fields_are_readable() {
        let descriptor = ObjectDescriptor {
            id: 7,
            kind: ObjectKind::Artifact,
            owner: crate::scheduler::tcb::TaskId::from("agent-1"),
            digest: "sha256:".to_string() + &"a".repeat(64),
            size: 1_200_000,
            residency: Residency::Resident,
            payload_ref: Some("payload:x".to_string()),
            version: 1,
            preview: None,
        };

        assert_eq!(descriptor.id, 7);
        assert_eq!(descriptor.kind, ObjectKind::Artifact);
        assert_eq!(
            descriptor.owner,
            crate::scheduler::tcb::TaskId::from("agent-1")
        );
        assert_eq!(descriptor.size, 1_200_000);
        assert_eq!(descriptor.residency, Residency::Resident);
        assert_eq!(descriptor.payload_ref, Some("payload:x".to_string()));
        assert_eq!(descriptor.version, 1);
        assert_eq!(descriptor.preview, None);
    }

    #[test]
    fn spc_006_05_object_kind_from_handle_kind_maps_all_four_variants() {
        assert_eq!(
            ObjectKind::from(HandleKind::ToolResult),
            ObjectKind::ToolResult
        );
        assert_eq!(ObjectKind::from(HandleKind::MemoryPage), ObjectKind::Memory);
        assert_eq!(
            ObjectKind::from(HandleKind::KnowledgeEntry),
            ObjectKind::Knowledge
        );
        assert_eq!(
            ObjectKind::from(HandleKind::SubAgentJoin),
            ObjectKind::Custom(CompactString::from("sub_agent_join"))
        );
    }

    #[test]
    fn spc_006_06_external_descriptor_carries_a_preview_and_locator_never_the_full_body() {
        let full_report = "x".repeat(1_200_000);
        let descriptor = ObjectDescriptor::external(
            7,
            ObjectKind::Artifact,
            crate::scheduler::tcb::TaskId::from("agent-a"),
            1,
            Residency::External {
                payload_ref: "payload:research-report".to_string(),
                digest: "sha256:deadbeef".to_string(),
                original_size: full_report.len() as u64,
            },
            &full_report[..200],
        );

        // The descriptor's own type has no `payload`/`content` field — a caller can only ever see
        // the four fields below. `size` still reports the true full-body size (so a reader knows
        // what a page-in would cost), but the descriptor never carries that many bytes itself.
        assert_eq!(
            descriptor.payload_ref.as_deref(),
            Some("payload:research-report")
        );
        assert_eq!(descriptor.digest, "sha256:deadbeef");
        assert_eq!(descriptor.size, 1_200_000);
        assert_eq!(descriptor.preview.as_deref(), Some(&full_report[..200]));
        assert!(
            descriptor.preview.as_ref().unwrap().len() < descriptor.size as usize,
            "the preview must be far smaller than the full body it stands in for"
        );
        assert!(matches!(descriptor.residency, Residency::External { .. }));
    }

    #[test]
    fn spc_009_07_a_capability_outside_the_objects_resource_denies_access() {
        // Plan §8 / spc_009-07: "Capability controls object access" — task A holds a capability
        // scoped to a *different* object than the one it tries to read on task B. Before this
        // card there was no function at all that could answer this question (no matching call
        // point existed anywhere in the crate), so this — an unconditional `false` — is what "the
        // check doesn't exist" looked like; now it's a real, reasoned denial.
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        let b_object = ObjectDescriptor::external(
            42,
            ObjectKind::Artifact,
            crate::scheduler::tcb::TaskId::from("task-b"),
            1,
            Residency::External {
                payload_ref: "payload:b-report".to_string(),
                digest: "sha256:deadbeef".to_string(),
                original_size: 1_000,
            },
            "preview",
        );

        // A's capability names a *different* object entirely (task-b's object 7, not 42).
        let a_capability = Capability {
            id: CapabilityId("cap-a".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("object:task-b/7".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("task-a".into()),
        };

        assert!(
            !object_access_allowed(&[a_capability], "read", &b_object),
            "a capability scoped to a different object must not authorize this one"
        );
    }

    #[test]
    fn spc_009_07_a_matching_capability_allows_the_requested_action() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        let b_object = ObjectDescriptor::external(
            42,
            ObjectKind::Artifact,
            crate::scheduler::tcb::TaskId::from("task-b"),
            1,
            Residency::External {
                payload_ref: "payload:b-report".to_string(),
                digest: "sha256:deadbeef".to_string(),
                original_size: 1_000,
            },
            "preview",
        );

        let a_capability = Capability {
            id: CapabilityId("cap-a".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("object:task-b/42".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("task-a".into()),
        };

        assert!(
            object_access_allowed(std::slice::from_ref(&a_capability), "read", &b_object),
            "a capability naming this exact object and the requested action must authorize it"
        );
        assert!(
            !object_access_allowed(std::slice::from_ref(&a_capability), "write", &b_object),
            "the same capability must not authorize an action it never granted"
        );
    }

    #[test]
    fn exact_object_capability_does_not_match_an_adjacent_id_prefix() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        let object = ObjectDescriptor::external(
            77,
            ObjectKind::Artifact,
            crate::scheduler::tcb::TaskId::from("owner"),
            1,
            Residency::External {
                payload_ref: "payload:77".to_string(),
                digest: "sha256:77".to_string(),
                original_size: 10,
            },
            "preview",
        );
        let capability = Capability {
            id: CapabilityId("read-7".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("object:owner/7".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: false,
            issuer: Principal("owner".into()),
        };

        assert!(!object_access_allowed(&[capability], "read", &object));
    }
}
