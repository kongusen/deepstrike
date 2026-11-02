use super::compression::CompressionPipeline;
use super::config::{ContextConfig, PromptBudgetConfig};
use super::item::{
    Authority, ExternalEdge, ExternalItem, HISTORY_ITEM_DETAIL_CAP, HistoryItemRecord, ItemEdge,
    ItemEnd, ItemId, ItemOrigin, ItemRecord, ItemTag, LeaseEvent, OriginTally, Relation,
};
use super::partitions::ContextPartitions;
use super::policy::ContextPolicy;
use super::pressure::{PressureAction, PressureMonitor};
use super::renderer::InternalRenderedContext;
use super::renewal::RenewalPolicy;
use super::skill_catalog::SkillCatalog;
use super::task_state::{TaskState, TaskUpdate};
use super::token_engine::ContextTokenEngine;
use crate::mm::handle::{Handle, HandleId, HandleKind, HandleTable, Residency};
use crate::types::capability::{Capability, CapabilityKind, CapabilityManifest};
use crate::types::message::{Content, ContentPart, CoreMessage, ToolSchema};
use crate::types::skill::SkillMetadata;
use compact_str::CompactString;

pub const MEMORY_TOOL_NAME: &str = "memory";
pub const PROPOSE_MEMORY_TOOL_NAME: &str = "propose_memory";
pub const KNOWLEDGE_TOOL_NAME: &str = "knowledge";
/// O7: the evicted-result re-fetch meta-tool (see `read_result_tool_schema`).
pub const READ_RESULT_TOOL_NAME: &str = "read_result";

/// Control-plane meta-tools: kernel-handled tools that drive state/capabilities rather than do task
/// work. Excluded from the `recent_actions` progress log (2b) so the footer reflects real progress.
const META_TOOL_NAMES: &[&str] = &[
    "pace",
    "update_plan",
    "skill",
    MEMORY_TOOL_NAME,
    PROPOSE_MEMORY_TOOL_NAME,
    KNOWLEDGE_TOOL_NAME,
    READ_RESULT_TOOL_NAME,
    "submit_workflow_nodes",
    "start_workflow",
];

/// Control-plane meta-tools are noise, not task progress — filtered out of the recency log (2b)
/// and out of the O6 repeat-fuse signature (the two must agree on what "an action" is).
pub(crate) fn is_meta_tool(name: &str) -> bool {
    META_TOOL_NAMES.contains(&name)
}

/// The kernel-owned meta surfaces injected by [`ContextManager::meta_tool_schemas`] — exactly the
/// tools the kernel itself adds to every turn's toolset, and the single source of truth for which
/// names tool *config* must not be able to remove.
///
/// **Invariant**: these are how the model reaches kernel state — load a skill, search
/// memory/knowledge, update the plan, re-read a result the kernel evicted (truncation markers
/// literally instruct the model to "call the read_result tool with call_id …"). Run-level tool
/// config (`allowedToolIds` ⇒ `capability_filter.allowed_ids`) and skill-level `allowed_tools` both
/// enumerate *task* tools; neither is a statement about kernel surfaces, so neither may narrow one
/// away — the same rationale the pace tool encodes by being pushed after every filter.
///
/// Scope: this exempts the **id axis** only. An explicit *kind* restriction
/// (`capability_filter.allowed_kinds`, e.g. sub-agent isolation admitting only
/// [`CapabilityKind::Tool`]) is a deliberate statement about capability families and still applies.
///
/// Distinct from [`META_TOOL_NAMES`], which is the wider "not task progress" set (it also covers
/// pace and workflow authoring) used for the recency log and repeat fuse.
pub(crate) const EXPOSURE_EXEMPT_META_TOOLS: &[&str] = &[
    "skill",
    MEMORY_TOOL_NAME,
    PROPOSE_MEMORY_TOOL_NAME,
    KNOWLEDGE_TOOL_NAME,
    "update_plan",
    READ_RESULT_TOOL_NAME,
];

/// Whether `name` is a kernel-owned meta surface exempt from tool-config narrowing
/// (see [`EXPOSURE_EXEMPT_META_TOOLS`]). Used by both filters in `emit_call_llm`.
pub(crate) fn is_exposure_exempt_meta_tool(name: &str) -> bool {
    EXPOSURE_EXEMPT_META_TOOLS.contains(&name)
}

/// How many recent (estimate, provider count) pairs the calibration is fitted on.
pub const ESTIMATE_CALIBRATION_SAMPLES: usize = 8;

/// End-of-run context lifecycle metrics (plan task 4.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleMetrics {
    pub knowledge_entries: u32,
    pub knowledge_referenced: u32,
    pub regret: u32,
    pub waste_tokens: u64,
    pub cache_loss_tokens: u64,
    /// Memory records the kernel recalled into the run.
    pub recalled: u32,
    /// Of those, the ones the model read in full with `memory(record_id)`.
    pub recall_expanded: u32,
    /// Every knowledge item of the run: the retired ones in the order they left, then the resident.
    pub items: Vec<ItemRecord>,
    /// History items by origin: admitted, still in history, read back.
    pub history: Vec<OriginTally>,
    /// The latest history items one by one (at most [`HISTORY_ITEM_DETAIL_CAP`]): the retired
    /// ones in the order they left, then the ones still in history.
    pub history_items: Vec<HistoryItemRecord>,
    /// History items too old to be listed in `history_items`.
    pub history_items_omitted: u32,
    /// Plan task 7.1 · the item graph's edges and how many pruning dropped.
    pub edges: Vec<ItemEdge>,
    pub edges_omitted: u32,
    /// Knowledge items nothing referenced and no recorded edge points to (a subset of what
    /// `waste_tokens` counts: a recall the model read back is not waste).
    pub waste_items: Vec<ItemId>,
    /// Plan task 7.4 · edges to items of other runs (inherited copies, dependency reads).
    pub external_edges: Vec<ExternalEdge>,
    /// The parent run's items whose inherited copies this run used.
    pub parent_items_used: Vec<ExternalItem>,
}

/// Internal context engine backing the canonical operation driver.
///
/// Exposed for in-crate use and tests; external callers should drive the kernel
/// through the canonical kernel rather than this type directly.
#[doc(hidden)]
pub struct ContextManager {
    pub partitions: ContextPartitions,
    pub max_tokens: u32,
    pub config: ContextConfig,
    pub engine: ContextTokenEngine,
    /// Provider envelope/tool-schema overhead plus output and safety reserves. Deducted from the
    /// model context window before any system/state/history content is selected.
    pub prompt_budget: PromptBudgetConfig,
    pub sprint: u32,
    pub skills: SkillCatalog,
    /// P1-B tool gating: the skills the model has loaded this session (by name), each with an
    /// optional lease expiry turn (K3: `None` = permanent, today's default). Their declared
    /// `allowed_tools` are unioned to narrow the exposed toolset in `emit_call_llm`. A map (not a
    /// single value) because the model may load several skills and still needs each one's tools
    /// (D1). K3 adds eviction: explicit `deactivate_skill` or lease expiry — both also unpin the
    /// skill's `skill:<name>` knowledge entry (boundary-swept). NOT snapshotted — rebuilt on wake
    /// by replaying `skill` tool calls (graceful).
    pub active_skills: std::collections::BTreeMap<CompactString, Option<u32>>,
    /// P1-B/D stable-core: tool ids that stay exposed even when a skill narrows the toolset (the
    /// "everyone uses these" set — read/search/bash etc.). Configured once by the SDK; empty by
    /// default (铁律: no config ⇒ skills narrow to exactly their declared tools + meta-tools).
    pub stable_core_tools: std::collections::HashSet<CompactString>,
    pub capabilities: CapabilityManifest,
    pub memory_enabled: bool,
    pub propose_memory_enabled: bool,
    pub knowledge_enabled: bool,
    pub plan_tool_enabled: bool,
    last_observed_prompt_tokens: Option<u32>,
    /// Plan task 5.1 · the uncalibrated estimate of the last provider request, and the factor (in
    /// parts per million) that brings an estimate to what the provider counted. Both move only on
    /// recorded inputs (the provider's observed count), so replay reproduces them.
    pub(crate) last_request_estimate: u32,
    /// The last few (uncalibrated estimate, provider count) pairs. A provider's count differs
    /// from the kernel's by a roughly fixed overhead (message framing, tool templates) plus a
    /// proportional tokenizer gap, so the calibration is the least-squares line through them.
    pub(crate) estimate_samples: std::collections::VecDeque<(u32, u32)>,
    compression: CompressionPipeline,
    pressure: PressureMonitor,
    renewal: RenewalPolicy,

    // ── Layer 3: Time tracking for decay ─────────────────────────────────
    /// Last activity timestamp (milliseconds since epoch).
    /// Updated on each ProviderResult and ToolResults.
    pub last_activity_ms: u64,

    /// Last compression timestamp (milliseconds since epoch).
    /// Updated on each compression pass.
    pub last_compact_ms: Option<u64>,

    // ── P3: handle table (context as address space) ─────────────────────────
    /// Per-task handle table: one [`Handle`] per addressable working-context object (tool results
    /// today). Residency transitions on these handles drive read-time projection; the original
    /// messages in `partitions` are never mutated by projection.
    pub handles: HandleTable,
    /// Monotonic allocator for [`HandleId`]s.
    next_handle_id: HandleId,

    /// P1-E: history length (message count) as of the last compaction/renewal. Messages below this
    /// index are the **frozen prefix** — byte-stable until the next compaction — so the renderer can
    /// hand providers a `frozen_prefix_len` for a long-lived deep cache breakpoint. 0 before any
    /// compaction (no frozen region yet). The canonical logical checkpoint projects this boundary:
    /// it is part of the next provider effect (`frozen_prefix_len`) and therefore must survive a
    /// restore even though it does not change model-visible text.
    frozen_history_len: usize,

    /// K1: boundary-sweep results awaiting drain into `KnowledgeSwept` observations. Not
    /// snapshotted (observation-only bookkeeping, same class as `frozen_history_len`).
    pending_knowledge_sweeps: Vec<crate::context::partitions::KnowledgeSweep>,

    /// K2: whether the budget warning already fired this cache generation (warn-once; reset by
    /// the boundary sweep). Not snapshotted — a resume re-warns at most once, harmless.
    knowledge_budget_warned: bool,
    /// Monotonic, input-derived clock for knowledge-reference recency.
    knowledge_reference_step: u64,
    /// A provider call has sent the current system[1] since the last boundary, so a change to
    /// system[1] now would invalidate the cached history behind it.
    prefix_sent: bool,
    /// History tokens whose cache a mid-run system[1] promotion discarded, summed over the run.
    cache_invalidated_tokens: u64,
    /// Long-term memory recall (plan task 2.1): `Some(top_k)` when the operation can read memory.
    /// The kernel recalls at the start of a run and after every renewal.
    pub(crate) recall_top_k: Option<u32>,
    /// A recall is owed before the next provider call.
    pub(crate) recall_due: bool,
    /// Plan task 3.1 · dependency outputs by agent id, read in full with
    /// `read_result(call_id="dep:<agent_id>")`. Only previews are in the rendered context.
    pub dependency_bodies: std::collections::BTreeMap<String, String>,
    /// Plan task 1.1b · the last item id handed out, to a history message or (plan task 7.1) a
    /// knowledge entry: one allocator for both.
    pub(crate) last_item_id: ItemId,
    /// Plan task 7.1 · typed edges between items. Reported, checkpointed; no decision reads it yet.
    pub(crate) graph: super::graph::ItemGraph,
    /// History items admitted and read back, by origin (plan task 4.1).
    pub(crate) history_tally: std::collections::BTreeMap<ItemOrigin, OriginTally>,
    /// History items that left, latest [`HISTORY_ITEM_DETAIL_CAP`] kept, in the order they left.
    pub(crate) retired_history: std::collections::VecDeque<HistoryItemRecord>,
    /// Retired history items dropped from `retired_history` by the cap.
    pub(crate) retired_history_omitted: u32,
    /// Messages that arrived while the history tail still waited for tool results (a memory hit
    /// resolving before a sibling tool in the same batch). A provider requires a call's results to
    /// follow it directly, so these wait until the last result lands.
    pub(crate) held_history: Vec<(CoreMessage, u32)>,
    /// Plan task 1.1c · provider replay state kept with assistant history items, by item id.
    /// Each with the tokens it adds to a request: replay is sent back with its turn, so it
    /// occupies the model's context like the turn's own text.
    pub(crate) item_replays: std::collections::BTreeMap<ItemId, (serde_json::Value, u32)>,
    /// Replay for the assistant message being admitted now (set around one provider completion).
    pub(crate) pending_assistant_replay: Option<serde_json::Value>,
}

impl ContextManager {
    pub fn new(max_tokens: u32) -> Self {
        // spc_011-C-01: production default is the real-BPE-backed estimator, not char/4
        // approximation — see `ContextTokenEngine::fallback_estimator` doc comment.
        Self::with_config(
            max_tokens,
            ContextConfig::default(),
            ContextTokenEngine::fallback_estimator(),
        )
    }

    pub fn with_config(max_tokens: u32, config: ContextConfig, engine: ContextTokenEngine) -> Self {
        let compression = CompressionPipeline::new(&config);
        let pressure = PressureMonitor::new(max_tokens, config.clone());
        let renewal = RenewalPolicy::from_config(&config);
        let partitions = ContextPartitions::new(&config);
        Self {
            partitions,
            max_tokens,
            config,
            engine,
            prompt_budget: PromptBudgetConfig::default(),
            sprint: 0,
            skills: SkillCatalog::new(),
            active_skills: std::collections::BTreeMap::new(),
            stable_core_tools: std::collections::HashSet::new(),
            capabilities: CapabilityManifest::new(),
            memory_enabled: false,
            propose_memory_enabled: false,
            knowledge_enabled: false,
            plan_tool_enabled: false,
            last_observed_prompt_tokens: None,
            last_request_estimate: 0,
            estimate_samples: std::collections::VecDeque::new(),
            compression,
            pressure,
            renewal,
            last_activity_ms: 0,
            last_compact_ms: None,
            handles: HandleTable::new(),
            next_handle_id: 0,
            frozen_history_len: 0,
            pending_knowledge_sweeps: Vec::new(),
            knowledge_budget_warned: false,
            knowledge_reference_step: 0,
            prefix_sent: false,
            recall_top_k: None,
            recall_due: false,
            dependency_bodies: std::collections::BTreeMap::new(),
            cache_invalidated_tokens: 0,
            last_item_id: 0,
            graph: super::graph::ItemGraph::default(),
            history_tally: std::collections::BTreeMap::new(),
            retired_history: std::collections::VecDeque::new(),
            retired_history_omitted: 0,
            held_history: Vec::new(),
            item_replays: std::collections::BTreeMap::new(),
            pending_assistant_replay: None,
        }
    }

    /// Atomically install the stable replay policy and rebuild every component derived from it.
    pub fn apply_context_policy(&mut self, policy: &ContextPolicy) {
        policy.apply_to(&mut self.config);
        self.compression = CompressionPipeline::new(&self.config);
        self.pressure = PressureMonitor::new(self.max_tokens, self.config.clone());
        self.renewal = RenewalPolicy::from_config(&self.config);
    }

    // ── Layer 3: Time-based decay ─────────────────────────────────────────────

    /// Update activity timestamp (call on each ProviderResult and ToolResults).
    pub fn record_activity(&mut self, now_ms: u64) {
        self.last_activity_ms = now_ms;
    }

    /// Check if Micro-Compact should trigger based on time decay (Layer 3).
    /// Returns true if idle time exceeds `micro_compact_idle_minutes`.
    pub fn should_time_decay_compact(&self, now_ms: u64) -> bool {
        // Idle decay excerpts old tool results — the provider's tier when it clears them natively.
        if self.config.delegate_tool_result_clearing {
            return false;
        }
        let idle_ms = if let Some(last_compact) = self.last_compact_ms {
            // Time since last compression
            now_ms.saturating_sub(last_compact)
        } else {
            // Time since first activity
            now_ms.saturating_sub(self.last_activity_ms)
        };

        let idle_minutes = idle_ms / 60_000;
        idle_minutes >= self.config.micro_compact_idle_minutes as u64
    }

    // ── Layer 4: read-time projection (handle residency) ────────────────────

    /// Recompute tool-result handle residency for Layer-4 read-time projection (call before
    /// `render`). When pressure (`rho`) reaches `collapse_threshold`, all but the most recent
    /// `preserved_tool_results` tool results are marked `Collapsed` (rendered as previews).
    ///
    /// **Monotonic within a cache generation (P0-C):** collapse is one-way here —
    /// `Resident → Collapsed` only, never the reverse. The old two-way version un-collapsed when
    /// `rho` fell back below the threshold, which (a) rewrote mid-history bytes and invalidated the
    /// prompt-cache prefix on every threshold oscillation, and (b) re-billed a full tool-result body
    /// for near-zero attention gain (an old result that already faded). Un-collapsing now happens
    /// only at compaction/renewal boundaries via [`Self::reset_collapse_generation`] — the one moment
    /// the prefix is rewritten anyway, so the cache cost is already paid. Non-destructive:
    /// `partitions` is untouched. Spooled/paged-out handles are left as-is.
    pub fn recompute_handle_residency(&mut self) {
        // Monotonic: below the threshold we never *un*-collapse, so there is nothing to do. A
        // provider that clears tool results natively owns this projection: collapsing them here too
        // would send the provider previews it could never have cleared from.
        if self.config.delegate_tool_result_clearing
            || self.rho() < self.config.collapse_threshold
        {
            return;
        }
        let keep = self.config.preserved_tool_results;
        // Single mutable pass in insertion order. `tool_result_handles_mut().enumerate()` yields the
        // collapse candidates oldest-first; `i < cutoff` protects the most recent `keep` results.
        let total = self
            .handles
            .all()
            .iter()
            .filter(|h| matches!(h.kind, HandleKind::ToolResult))
            .count();
        let cutoff = total.saturating_sub(keep);
        for (i, handle) in self.handles.tool_result_handles_mut().enumerate() {
            // Only fold the reversible Resident → Collapsed axis; never clobber a handle that has
            // been externalized or paged out, and never reverse an existing collapse mid-generation.
            if i < cutoff && matches!(handle.residency, Residency::Resident) {
                handle.residency = Residency::Collapsed;
            }
        }
    }

    /// Start a fresh collapse generation: un-collapse every `Collapsed` handle back to `Resident`.
    /// Called only at compaction/renewal boundaries — the sole points where un-collapsing is
    /// cache-free, since the rendered prefix is rewritten there regardless. Between boundaries
    /// [`Self::recompute_handle_residency`] keeps collapse strictly one-way (P0-C). Spooled/paged-out
    /// handles are untouched (they leave the Resident↔Collapsed cycle deliberately).
    pub fn reset_collapse_generation(&mut self) {
        for handle in self.handles.all_mut() {
            if matches!(handle.residency, Residency::Collapsed) {
                handle.residency = Residency::Resident;
            }
        }
    }

    /// Drop handles whose anchored source message no longer lives in `partitions.history` — i.e.
    /// archived by a compaction or dropped on renewal. Without this the handle table grows with
    /// total session length (a handle per tool result, never removed), which also inflates the
    /// per-turn `recompute_handle_residency` scan. Called at compaction/renewal boundaries, so the
    /// table tracks the working set, not the whole session. Handles with no `source` anchor (future
    /// non-tool-result kinds) are always kept — they can't be orphaned by this check.
    ///
    /// §25.9 · a handle whose body lives **outside** core is also always kept, whatever happened to
    /// the message it was anchored to. The handle table is the kernel's only record of an external
    /// payload: pruning one does not free the body, it only makes the body permanently unreachable
    /// — and page-in is exactly the operation that outlives the preview's stay in working context.
    pub fn prune_orphaned_handles(&mut self) {
        let live_items: std::collections::HashSet<ItemId> =
            self.partitions.history.tags().iter().map(|tag| tag.id).collect();
        // A handle restored from a checkpoint without item ids is matched by its call_id.
        let live: std::collections::HashSet<CompactString> = self
            .partitions
            .history
            .messages()
            .iter()
            .flat_map(|m| match &m.content {
                Content::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::ToolResult { call_id, .. } => Some(call_id.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        self.handles.retain(|h| {
            h.residency.payload_ref().is_some()
                || if h.item != 0 {
                    live_items.contains(&h.item)
                } else {
                    h.source.as_ref().is_none_or(|s| live.contains(s))
                }
        });
    }

    /// Give every handle restored without an item id the id of the history item holding its body.
    pub(crate) fn backfill_handle_items(&mut self) {
        let missing: Vec<(HandleId, CompactString)> = self
            .handles
            .all()
            .iter()
            .filter(|h| h.item == 0)
            .filter_map(|h| Some((h.id, h.source.clone()?)))
            .collect();
        for (id, source) in missing {
            if let Some(item) = self.history_item_holding(&source) {
                if let Some(handle) = self.handles.get_mut(id) {
                    handle.item = item;
                }
            }
        }
    }

    /// The id of the history item holding the tool result `call_id`, if one does.
    fn history_item_holding(&self, call_id: &str) -> Option<ItemId> {
        let history = &self.partitions.history;
        history.messages().iter().position(|message| match &message.content {
            Content::Parts(parts) => parts.iter().any(|part| {
                matches!(part, ContentPart::ToolResult { call_id: id, .. } if id.as_str() == call_id)
            }),
            _ => false,
        })
        .map(|index| history.tag(index).id)
        .filter(|id| *id != 0)
    }

    /// What history item `index` renders: a handle while one of its bodies is collapsed or paged
    /// out, otherwise its stored form (full, or the excerpt compaction left).
    pub fn history_fidelity(&self, index: usize) -> crate::context::item::Fidelity {
        let tag = self.partitions.history.tag(index);
        if tag.id != 0 && self.handles.projected_items().contains(&tag.id) {
            crate::context::item::Fidelity::Handle
        } else {
            tag.fidelity
        }
    }

    /// §7.10 / §25.9 · move the payload residency of the handle addressed by `source`, minting the
    /// handle when the kernel is hearing about this body for the first time (a page-out archive has
    /// no anchored message: its handle *is* the address). Returns the residency it replaced, or
    /// `None` when the handle was minted here.
    ///
    /// This is the **single** writer of the external/paged-out axis, which is what makes "the P3
    /// handle table is the only kernel fact about where a body lives" checkable rather than
    /// aspirational: every caller goes through here, and every caller therefore has a residency
    /// transition to observe.
    ///
    /// `tokens` is the weight this handle contributes to working context, so it is `0` for every
    /// body that lives outside core — an external result's anchored message holds the *preview*,
    /// not the body, so unlike a `Collapsed` handle there is no over-count to discount — and the
    /// body's real weight only when a page-in brought it home.
    pub fn set_payload_residency(
        &mut self,
        source: &str,
        kind: HandleKind,
        tokens: u32,
        residency: Residency,
    ) -> Option<Residency> {
        if let Some(handle) = self
            .handles
            .all_mut()
            .iter_mut()
            .find(|h| h.source.as_deref() == Some(source))
        {
            let previous = std::mem::replace(&mut handle.residency, residency);
            handle.tokens = tokens;
            return Some(previous);
        }
        let id = self.alloc_handle_id();
        let item = self.history_item_holding(source).unwrap_or(0);
        self.handles.insert(Handle {
            id,
            kind,
            residency,
            tokens,
            source: Some(source.into()),
            item,
        });
        None
    }

    /// The payload residency of the handle addressed by `source`. The read half of
    /// [`Self::set_payload_residency`] — a page-in asks this before it can address anything.
    ///
    /// `source` **is** the wire address of a handle: the tool `call_id` for a result, which is
    /// exactly what the truncation marker tells the model to pass back to `read_result`, and the
    /// kernel-minted archive id for a page-out.
    pub fn payload_residency(&self, source: &str) -> Option<&Residency> {
        self.handles.residency_for_source(source)
    }

    // ── Pressure ──────────────────────────────────────────────────────────────

    /// **Raw** rho — full partition weight (or provider-observed tokens when available). This is the
    /// projection-decision rho: [`Self::recompute_handle_residency`] marks the Resident↔Collapsed set
    /// from *this* value, so it must NOT discount paged content (else collapse → rho drops →
    /// un-collapse would oscillate).
    pub fn rho(&self) -> f64 {
        let observed = self.pressure.pressure(
            &self.partitions,
            &self.engine,
            self.last_observed_prompt_tokens,
        );
        // A provider sees a selected projection, not the entire resident history. Its smaller
        // count must not suppress compaction while the unselected history still triggers renewal.
        // Calibration may raise the estimate (the provider counts more than the kernel does, as
        // for CJK text or tool templates) but never lowers it: a provider or proxy reporting too
        // little must not delay compaction past the point the kernel's own count calls for.
        let max_tokens = f64::from(self.max_tokens.max(1));
        let raw = self
            .partitions
            .total_tokens(&self.engine)
            .saturating_add(self.replay_tokens());
        let estimated = if self.max_tokens == 0 {
            0.0
        } else {
            f64::from(self.calibrated(raw).max(raw)) / max_tokens
        };
        observed.max(estimated)
    }

    pub fn set_observed_prompt_tokens(&mut self, tokens: u32) {
        self.last_observed_prompt_tokens = Some(tokens);
        if self.last_request_estimate > 0 {
            if self.estimate_samples.len() == ESTIMATE_CALIBRATION_SAMPLES {
                self.estimate_samples.pop_front();
            }
            self.estimate_samples.push_back((self.last_request_estimate, tokens));
            self.last_request_estimate = 0;
        }
    }

    /// Remember `raw`, the uncalibrated estimate of the request about to be sent, and return it
    /// calibrated.
    pub fn note_request_estimate(&mut self, raw: u32) -> u32 {
        self.last_request_estimate = raw;
        self.calibrated(raw)
    }

    /// `tokens` (an uncalibrated estimate) as the provider would count it: through the
    /// least-squares line of the recent samples; with one sample (or samples of one size) by
    /// their mean offset, the gap being mostly fixed overhead; unchanged with none. Integer arithmetic, so replay reproduces
    /// it; bounded to 1/2..4x of the estimate.
    fn calibrated(&self, tokens: u32) -> u32 {
        let n = self.estimate_samples.len() as i128;
        if n == 0 || tokens == 0 {
            return tokens;
        }
        let (mut sx, mut sy, mut sxx, mut sxy) = (0i128, 0i128, 0i128, 0i128);
        for &(x, y) in &self.estimate_samples {
            let (x, y) = (i128::from(x), i128::from(y));
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let x = i128::from(tokens);
        let denominator = n * sxx - sx * sx;
        let fitted = if n >= 2 && denominator > 0 {
            (sy * sxx - sx * sxy + x * (n * sxy - sx * sy)) / denominator
        } else {
            x + (sy - sx) / n
        };
        fitted.clamp(x / 2, x * 4).clamp(0, i128::from(u32::MAX)) as u32
    }

    pub fn should_compress(&self) -> PressureAction {
        // Compaction-tier recommendation runs on **raw** rho. A paging-aware discount
        // (`effective_rho`) was tried during W1-1 and over-relieved pressure: once
        // `micro_compact` paged out tool-result handles, the discounted rho fell below the
        // collapse/auto_compact thresholds and the heavy tiers never fired. Raw rho keeps
        // escalation intact (recoverable from git if a cache-aware planner ever lands).
        self.kernel_owned_tier(self.pressure.recommend(self.rho()))
    }

    /// The highest compaction tier at or below `action` that the kernel still runs itself. A tier
    /// the provider performs natively is skipped, never escalated past: delegated compaction falls
    /// back to the kernel's own tool-result tier when that one is still kernel-owned. A forced
    /// compaction (`force_compress`, the overflow ladder) is a recovery, not a pressure tier, and is
    /// never filtered; renewal is decided apart from this and is never delegated.
    pub fn kernel_owned_tier(&self, action: PressureAction) -> PressureAction {
        let clearing = self.config.delegate_tool_result_clearing;
        match action {
            PressureAction::ContextCollapse | PressureAction::AutoCompact
                if self.config.delegate_compaction =>
            {
                if clearing {
                    PressureAction::None
                } else {
                    PressureAction::MicroCompact
                }
            }
            PressureAction::SnipCompact | PressureAction::MicroCompact if clearing => {
                PressureAction::None
            }
            other => other,
        }
    }

    pub fn compress(
        &mut self,
        action: PressureAction,
    ) -> (u32, Option<String>, Vec<CoreMessage>, Option<usize>) {
        self.compress_with_time(action, None)
    }

    pub fn compress_with_time(
        &mut self,
        action: PressureAction,
        now_ms: Option<u64>,
    ) -> (u32, Option<String>, Vec<CoreMessage>, Option<usize>) {
        let target = self.config.target_tokens(self.max_tokens);
        self.compress_with_target(action, target, now_ms)
    }

    pub fn force_compress(&mut self) -> (u32, Option<String>, Vec<CoreMessage>, Option<usize>) {
        self.compress_with_target(PressureAction::AutoCompact, 0, None)
    }

    /// W1-1 收口: run one compaction `action` toward an **explicit** `target_tokens`, instead of
    /// re-deriving the target from config. This is what lets `EvictionOp::Collapse { target_tokens }`
    /// flow from the planner (the single decision point) straight to the executor — the compactor no
    /// longer re-decides the target. This is the single compaction implementation;
    /// `compress_with_time` (config-derived target) and `force_compress` (AutoCompact, target 0)
    /// are thin delegations.
    pub fn compress_with_target(
        &mut self,
        action: PressureAction,
        target_tokens: u32,
        now_ms: Option<u64>,
    ) -> (u32, Option<String>, Vec<CoreMessage>, Option<usize>) {
        let (saved, summary, archived, cache_at) = self.compression.compress(
            &mut self.partitions,
            action,
            self.max_tokens,
            target_tokens,
            &self.engine,
        );
        for row in &archived {
            let record = HistoryItemRecord::of(
                row.tag,
                row.measurement.tokens,
                row.tag.fidelity,
                Some(ItemEnd::Compacted),
            );
            self.retire_history_item(record);
        }
        let archived: Vec<CoreMessage> = archived.into_iter().map(|row| row.message).collect();
        if saved > 0 || !archived.is_empty() {
            // The measurement describes the request before this rewrite, not the new context.
            self.last_observed_prompt_tokens = None;
        }
        if let Some(ts) = now_ms {
            self.last_compact_ms = Some(ts);
        }
        // Archived messages have left history — drop their now-orphaned handles (bounds the table).
        if !archived.is_empty() {
            self.prune_orphaned_handles();
            self.prune_item_replays();
            // Compaction rewrote the history prefix — start a fresh collapse generation so
            // surviving handles re-evaluate from Resident (P0-C: the one cache-free un-collapse point).
            self.reset_collapse_generation();
            // K1: the prompt-cache prefix is being rebuilt anyway — the one cache-free moment to
            // apply deferred knowledge upserts/removals (rewriting system[1] bytes).
            self.sweep_knowledge_at_boundary();
        }
        // P2-D × P1-E: re-anchor the frozen-prefix boundary only when the compaction actually broke
        // the prompt-cache prefix (`result.3` = the planner's per-step `cache_at` cost, `Some` ⇒ a
        // prefix break). A prefix-safe compaction (late Snip/Excerpt that touches no early message)
        // leaves `[0..frozen]` byte-stable, so the deep cache survives the compaction and the boundary
        // holds — strictly more precise than the old `archived`-keyed reset, which missed an early
        // in-place Snip and needlessly re-anchored after a prefix-safe pass.
        if cache_at.is_some() {
            self.frozen_history_len = self.partitions.history.len();
        }
        (saved, summary, archived, cache_at)
    }

    /// W1-1 收口: the truthful compaction parameters the planner stamps into the [`EvictionPlan`],
    /// read once from config so the ops carry real values (not magic-number placeholders) and the
    /// executor stays a pure executor. Returns `(target_tokens, preserve_recent_turns)`.
    pub fn plan_compaction_params(&self) -> (u32, usize) {
        (
            self.config.target_tokens(self.max_tokens),
            self.config.preserve_recent_turns,
        )
    }

    // ── Renewal ───────────────────────────────────────────────────────────────

    pub fn should_renew(&self) -> bool {
        self.renewal
            .should_renew(&self.pressure, &self.partitions, &self.engine)
    }

    pub fn renew(&mut self) {
        let renewed = self
            .renewal
            .renew(&self.partitions, self.max_tokens, &self.engine);
        let carried: std::collections::HashSet<ItemId> =
            renewed.history.tags().iter().map(|tag| tag.id).collect();
        for index in 0..self.partitions.history.len() {
            let tag = self.partitions.history.tag(index);
            if !carried.contains(&tag.id) {
                let tokens = self.partitions.history.measured_tokens(index, &self.engine);
                let fidelity = self.history_fidelity(index);
                self.retire_history_item(HistoryItemRecord::of(
                    tag,
                    tokens,
                    fidelity,
                    Some(ItemEnd::Renewed),
                ));
            }
        }
        self.partitions = renewed;
        self.sprint += 1;
        self.last_observed_prompt_tokens = None;
        // The renewed history dropped earlier recalls with the rest; recall again for the new sprint.
        self.recall_due = self.recall_top_k.is_some();
        // History was rebuilt wholesale — drop handles anchored to messages it no longer carries,
        // and start a fresh collapse generation (P0-C) since the whole prefix changed.
        self.prune_orphaned_handles();
        self.prune_item_replays();
        self.reset_collapse_generation();
        // K1: renewal is a boundary — apply deferred knowledge upserts/removals now.
        self.sweep_knowledge_at_boundary();
        // P1-E: the renewed history is the new frozen base.
        self.frozen_history_len = self.partitions.history.len();
    }

    // ── Render ────────────────────────────────────────────────────────────────

    pub fn set_prompt_budget(&mut self, prompt_budget: PromptBudgetConfig) {
        self.prompt_budget = prompt_budget;
    }

    pub fn available_input_tokens(&self) -> u32 {
        self.max_tokens
            .saturating_sub(self.prompt_budget.reserved_tokens())
    }

    pub fn render(&self) -> InternalRenderedContext {
        let mut rendered = super::renderer::render_projected(
            &self.partitions,
            self.available_input_tokens(),
            &self.engine,
            self.config.preserve_recent_units,
            &self.handles,
            self.frozen_history_len,
            self.config.collapse_assistant_narration,
        );
        if !self.item_replays.is_empty() {
            let replay_tokens: u32 = rendered
                .turn_items
                .iter()
                .filter_map(|id| self.item_replays.get(id).map(|(_, tokens)| *tokens))
                .sum();
            rendered.estimated_tokens = rendered.estimated_tokens.saturating_add(replay_tokens);
            rendered.turn_replays = rendered
                .turn_items
                .iter()
                .map(|id| self.item_replays.get(id).map(|(replay, _)| replay.clone()))
                .collect();
        }
        rendered
    }

    // ── History / Knowledge ───────────────────────────────────────────────────

    /// Plan task 5.1 · the provider summarized history through item `through` server-side. The
    /// covered items leave as compacted (extended to the end of a tool transaction, never split),
    /// and one `provider_summary` item heads history in their place, rendering `replay` (the
    /// provider's block) — so pressure, renewal and the next render see what the model sees.
    /// Returns whether `through` named a history item.
    pub fn absorb_provider_compaction(
        &mut self,
        through: ItemId,
        replay: serde_json::Value,
        summary_tokens: u32,
    ) -> bool {
        let history = &self.partitions.history;
        let Some(position) = history.tags().iter().position(|tag| tag.id == through) else {
            return false;
        };
        let end = super::units::unit_boundaries(history.messages())
            .into_iter()
            .find(|unit| unit.contains(&position))
            .map_or(position + 1, |unit| unit.end);
        let covered_tokens: u32 = (0..end)
            .map(|index| history.measured_tokens(index, &self.engine))
            .sum();
        let retained = history.token_count().saturating_sub(covered_tokens);
        let covered: std::collections::BTreeSet<usize> = (0..end).collect();
        let rows = self.partitions.history.take_rows(&covered, retained);
        for row in &rows {
            let record = HistoryItemRecord::of(
                row.tag,
                row.measurement.tokens,
                row.tag.fidelity,
                Some(ItemEnd::Compacted),
            );
            self.retire_history_item(record);
        }
        let tag = ItemTag::new(
            self.alloc_item_id(),
            ItemOrigin::ProviderSummary,
            self.partitions.knowledge.turn,
        );
        for row in &rows {
            self.add_edge(tag.id, row.tag.id, Relation::Summarizes);
        }
        let message = CoreMessage::assistant("[earlier conversation summarized by the provider]");
        let tally = self.tally(ItemOrigin::ProviderSummary);
        tally.items += 1;
        tally.tokens += u64::from(summary_tokens);
        self.partitions.history.prepend_row(super::partitions::PartitionRow {
            tag,
            measurement: super::measurement::TokenMeasurement::for_message(&message, summary_tokens),
            message,
        });
        self.keep_item_replay(tag.id, replay);
        self.prune_orphaned_handles();
        self.prune_item_replays();
        self.reset_collapse_generation();
        self.sweep_knowledge_at_boundary();
        self.frozen_history_len = self.partitions.history.len();
        true
    }

    /// Plan task 5.1 · the provider cleared these tool results server-side: render them as
    /// handles, as the model now sees them.
    pub fn absorb_cleared_tool_results<'a>(&mut self, call_ids: impl IntoIterator<Item = &'a str>) {
        let cleared: std::collections::HashSet<&str> = call_ids.into_iter().collect();
        for handle in self.handles.tool_result_handles_mut() {
            if handle.source.as_deref().is_some_and(|source| cleared.contains(source))
                && matches!(handle.residency, Residency::Resident)
            {
                handle.residency = Residency::Collapsed;
            }
        }
    }

    /// Keep `replay` with item `id`, counting what it adds to a request.
    pub(crate) fn keep_item_replay(&mut self, id: ItemId, replay: serde_json::Value) {
        let tokens = self.engine.count(&replay.to_string());
        self.item_replays.insert(id, (replay, tokens));
    }

    /// Tokens the replay of every item still in history adds to a request.
    fn replay_tokens(&self) -> u32 {
        self.item_replays.values().map(|(_, tokens)| *tokens).sum()
    }

    /// Drop replay state whose item left history.
    fn prune_item_replays(&mut self) {
        if self.item_replays.is_empty() {
            return;
        }
        let live: std::collections::HashSet<ItemId> =
            self.partitions.history.tags().iter().map(|tag| tag.id).collect();
        self.item_replays.retain(|id, _| live.contains(id));
    }

    /// Append `msg` to history after the tool results the last assistant turn still waits for.
    /// Used for content an effect delivers mid-batch (a memory hit that resolves before a sibling
    /// tool): placed now, it would split the batch's results, which a provider rejects.
    pub fn push_history_after_tool_results(&mut self, msg: CoreMessage, tokens: u32) {
        if self.awaits_tool_results() {
            self.held_history.push((msg, tokens));
        } else {
            self.push_history(msg, tokens);
        }
    }

    /// Append whatever is still held, whether or not the batch completed. Called before a provider
    /// call, so held content is never lost to a batch whose results never all arrived.
    pub fn release_held_history(&mut self) {
        for (held, tokens) in std::mem::take(&mut self.held_history) {
            self.admit_history(held, tokens);
        }
    }

    pub fn push_history(&mut self, msg: CoreMessage, tokens: u32) {
        let is_tool = msg.role == crate::types::message::Role::Tool;
        self.admit_history(msg, tokens);
        if is_tool && !self.held_history.is_empty() && !self.awaits_tool_results() {
            for (held, tokens) in std::mem::take(&mut self.held_history) {
                self.admit_history(held, tokens);
            }
        }
    }

    /// Whether the last assistant message in history asked for a call whose result is not in
    /// history yet.
    fn awaits_tool_results(&self) -> bool {
        let mut answered = std::collections::HashSet::new();
        for message in self.partitions.history.messages().iter().rev() {
            match message.role {
                crate::types::message::Role::Tool => {
                    if let Content::Parts(parts) = &message.content {
                        for part in parts {
                            if let ContentPart::ToolResult { call_id, .. } = part {
                                answered.insert(call_id.as_str());
                            }
                        }
                    }
                }
                crate::types::message::Role::Assistant => {
                    return message
                        .tool_calls
                        .iter()
                        .any(|call| !answered.contains(call.id.as_str()));
                }
                _ => return false,
            }
        }
        false
    }

    fn admit_history(&mut self, msg: CoreMessage, tokens: u32) {
        self.knowledge_reference_step = self.knowledge_reference_step.saturating_add(1);
        let item = self.alloc_item_id();
        let evidence = Self::evidence_of(&msg);
        let references = self
            .partitions
            .knowledge
            .observe_references(&msg, self.knowledge_reference_step, evidence);
        for (to, strong) in references {
            let relation = if strong { Relation::ReferencesStrong } else { Relation::ReferencesLexical };
            self.add_edge(item, to, relation);
        }
        if let Content::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolResult { call_id, .. } = part
                    && let Some(call) = self.item_of_call(call_id)
                {
                    self.add_edge(item, call, Relation::Answers);
                }
            }
        }
        if evidence == super::partitions::Evidence::Any {
            self.credit_expansions(item, &msg);
        }
        if let Some(record) = Self::memory_hit_record(&msg) {
            let key = format!("memory:{record}");
            let recalled = self
                .partitions
                .knowledge
                .entries
                .iter()
                .find(|entry| entry.key.as_deref() == Some(key.as_str()))
                .map(|entry| entry.id);
            if let Some(recalled) = recalled {
                self.add_edge(item, recalled, Relation::Expands);
            }
        }
        // P3 (3a): index each tool result entering working context as a handle on its item,
        // anchored to its call_id. The handle's residency is the item's: a collapsed or paged-out
        // body makes the item render as a handle, without mutating the stored message.
        if let Content::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolResult {
                    call_id, output, ..
                } = part
                {
                    let id = self.alloc_handle_id();
                    let tok = self.engine.count(output).max(1);
                    self.handles.insert(Handle {
                        item,
                        ..Handle::resident_for(id, HandleKind::ToolResult, tok, call_id.clone())
                    });
                }
            }
        }
        let tag = ItemTag::new(
            item,
            ItemOrigin::of_message(&msg),
            self.partitions.knowledge.turn,
        );
        let tally = self.tally(tag.origin);
        tally.items += 1;
        tally.tokens += u64::from(tokens);
        if tag.origin == ItemOrigin::Assistant {
            if let Some(replay) = self.pending_assistant_replay.take() {
                self.keep_item_replay(tag.id, replay);
            }
        }
        self.partitions.history.push_tagged(msg, tokens, tag);
    }

    /// Drop history from `len` on (a rolled-back turn); its items leave as removed.
    pub fn truncate_history(&mut self, len: usize) {
        // Held messages belong to the turn being rolled back.
        self.held_history.clear();
        for index in len..self.partitions.history.len() {
            let tag = self.partitions.history.tag(index);
            let tokens = self.partitions.history.measured_tokens(index, &self.engine);
            let fidelity = self.history_fidelity(index);
            self.retire_history_item(HistoryItemRecord::of(
                tag,
                tokens,
                fidelity,
                Some(ItemEnd::Removed),
            ));
        }
        self.partitions.history.truncate(len);
        self.prune_item_replays();
        self.prune_item_graph();
    }

    fn retire_history_item(&mut self, record: HistoryItemRecord) {
        self.retired_history.push_back(record);
        if self.retired_history.len() > HISTORY_ITEM_DETAIL_CAP {
            self.retired_history.pop_front();
            self.retired_history_omitted += 1;
        }
    }

    fn tally(&mut self, origin: ItemOrigin) -> &mut OriginTally {
        self.history_tally.entry(origin).or_insert(OriginTally {
            origin,
            items: 0,
            tokens: 0,
            resident_items: 0,
            resident_tokens: 0,
            read_back: 0,
        })
    }

    /// The model asked `read_result` for `address`: a dependency output (`dep:<agent_id>`) or an
    /// earlier tool result by call id. Either is an item it is reading back in full.
    pub fn note_read_back(&mut self, address: &str) {
        let origin = if address.starts_with("dep:") {
            ItemOrigin::DependencyOutput
        } else {
            ItemOrigin::ToolResult
        };
        self.tally(origin).read_back += 1;
        if let Some(agent) = address.strip_prefix("dep:")
            && let Some(reader) = self.last_assistant_item()
        {
            let turn = self.partitions.knowledge.turn;
            self.graph.add_external(ExternalEdge {
                from: reader,
                to: ExternalItem { agent: Some(agent.to_string()), ..ExternalItem::default() },
                relation: Relation::DependsOn,
                turn,
            });
        }
        let target = self
            .handles
            .all()
            .iter()
            .find(|handle| {
                handle.source.as_deref() == Some(address) || handle.id.to_string() == address
            })
            .map(|handle| handle.item);
        if let (Some(reader), Some(target)) = (self.last_assistant_item(), target) {
            self.add_edge(reader, target, Relation::ReadsBack);
        }
    }

    /// Plan task 7.1 · the model fetched record `record_id` with `memory(record_id)`: if the kernel
    /// recalled it, the call read that recall back. Report only.
    pub fn note_memory_read(&mut self, record_id: &str) {
        let key = format!("memory:{record_id}");
        let target = self
            .partitions
            .knowledge
            .entries
            .iter()
            .find(|entry| entry.key.as_deref() == Some(key.as_str()))
            .map(|entry| entry.id);
        if let (Some(reader), Some(target)) = (self.last_assistant_item(), target) {
            self.add_edge(reader, target, Relation::ReadsBack);
        }
    }

    /// Plan task 7.4 · the knowledge entry just seeded under `key` (the last entry when unkeyed) is
    /// a copy of `source`, a parent run's item. Report only.
    pub fn note_inherited_from(&mut self, key: Option<&str>, source: ExternalItem) {
        let entries = &self.partitions.knowledge.entries;
        let copy = match key {
            Some(key) => entries.iter().find(|entry| entry.key.as_deref() == Some(key)),
            None => entries.last(),
        };
        if let Some(from) = copy.map(|entry| entry.id) {
            let turn = self.partitions.knowledge.turn;
            self.graph.add_external(ExternalEdge {
                from,
                to: source,
                relation: Relation::InheritedFrom,
                turn,
            });
        }
    }

    pub(crate) fn alloc_item_id(&mut self) -> ItemId {
        self.last_item_id += 1;
        self.last_item_id
    }

    /// Record an edge of the item graph, pruning it against residency when it has grown large.
    fn add_edge(&mut self, from: ItemId, to: ItemId, relation: Relation) {
        let turn = self.partitions.knowledge.turn;
        if self.graph.add(ItemEdge { from, to, relation, turn }) && self.graph.over_limit() {
            self.prune_item_graph();
        }
    }

    /// Ids of every item still in context: history rows and knowledge entries.
    fn resident_item_ids(&self) -> std::collections::BTreeSet<ItemId> {
        let history = &self.partitions.history;
        (0..history.len())
            .map(|index| history.tag(index).id)
            .chain(self.partitions.knowledge.entries.iter().map(|entry| entry.id))
            .filter(|id| *id != 0)
            .collect()
    }

    pub(crate) fn prune_item_graph(&mut self) {
        let resident = self.resident_item_ids();
        self.graph.prune(&resident);
    }

    /// The assistant history item that made tool call `call_id`.
    fn item_of_call(&self, call_id: &str) -> Option<ItemId> {
        let history = &self.partitions.history;
        (0..history.len()).rev().find_map(|index| {
            let message = &history.messages()[index];
            (message.role == crate::types::message::Role::Assistant
                && message.tool_calls.iter().any(|call| call.id.as_str() == call_id))
            .then(|| history.tag(index).id)
            .filter(|id| *id != 0)
        })
    }

    /// The latest assistant history item: the one whose tool calls are being answered.
    fn last_assistant_item(&self) -> Option<ItemId> {
        let history = &self.partitions.history;
        (0..history.len()).rev().find_map(|index| {
            (history.tag(index).origin == ItemOrigin::Assistant)
                .then(|| history.tag(index).id)
                .filter(|id| *id != 0)
        })
    }

    fn alloc_handle_id(&mut self) -> HandleId {
        let id = self.next_handle_id;
        self.next_handle_id = self.next_handle_id.wrapping_add(1);
        id
    }

    /// The next handle id this allocator will hand out.
    ///
    /// Read by the §12.1 checkpoint projection: a restored kernel that restarted the allocator
    /// would re-issue an id an outstanding `LoadPayload` effect still addresses.
    pub fn next_handle_id(&self) -> HandleId {
        self.next_handle_id
    }

    /// §12.2 · reinstall the allocator a checkpoint recorded.
    ///
    /// The mirror of [`Self::next_handle_id`], and the reason handle identity survives a restore:
    /// the handle *table* is repopulated by id, and this is what stops the next allocation from
    /// colliding with one of them.
    pub fn restore_next_handle_id(&mut self, next: HandleId) {
        self.next_handle_id = next;
    }

    pub fn frozen_history_len(&self) -> usize {
        self.frozen_history_len
    }

    pub(crate) fn knowledge_checkpoint_state(&self) -> (u64, bool) {
        (self.knowledge_reference_step, self.knowledge_budget_warned)
    }

    pub(crate) fn restore_knowledge_checkpoint_state(&mut self, reference_step: u64, warned: bool) {
        self.knowledge_reference_step = reference_step;
        self.knowledge_budget_warned = warned;
    }

    pub(crate) fn cache_checkpoint_state(&self) -> (bool, u64) {
        (self.prefix_sent, self.cache_invalidated_tokens)
    }

    pub(crate) fn restore_cache_checkpoint_state(&mut self, prefix_sent: bool, invalidated: u64) {
        self.prefix_sent = prefix_sent;
        self.cache_invalidated_tokens = invalidated;
    }

    /// Turn on kernel-owned memory recall for this operation, `top_k` hits per recall.
    pub fn enable_recall(&mut self, top_k: u32) {
        self.recall_top_k = Some(top_k.max(1));
    }

    /// Take the owed recall, if any: the query text and width the kernel asks the host for.
    pub fn take_due_recall(&mut self) -> Option<(String, u32)> {
        let top_k = self.recall_top_k?;
        if !std::mem::take(&mut self.recall_due) {
            return None;
        }
        let goal = self.partitions.task_state.goal.trim();
        (!goal.is_empty()).then(|| (goal.to_string(), top_k))
    }

    /// The index line one recalled record becomes. The header names the record, its kind and trust;
    /// the body is its one-line description (its name when it has none). The model reads the
    /// record in full with `memory(record_id)`.
    pub fn recall_index_line(record_id: &str, kind: &str, trust: &str, description: &str, name: &str) -> String {
        let summary = if description.trim().is_empty() { name.trim() } else { description.trim() };
        let trust = if trust.is_empty() { String::new() } else { format!(" trust={trust}") };
        format!("[MEMORY record_id={record_id} kind={kind}{trust}] {summary}")
    }

    /// Land recalled records as keyed `memory_recall` items `memory:<record_id>`. The key dedupes a
    /// record across recalls; a record already resident keeps its bytes and has its lease renewed.
    pub fn push_recalled(&mut self, lines: Vec<(String, String)>) {
        for (record_id, line) in lines {
            let key = CompactString::from(format!("memory:{record_id}"));
            if self.partitions.knowledge.renew(&key) {
                continue;
            }
            let tokens = self.engine.count(&line).max(1);
            self.push_knowledge_item(
                ItemOrigin::MemoryRecall,
                Some(key),
                CoreMessage::system(line),
                tokens,
                false,
            );
        }
    }

    /// The model read memory record `record_id` in full: its index line was used.
    pub fn note_memory_expanded(&mut self, record_id: &str) -> bool {
        self.partitions
            .knowledge
            .note_expanded(&format!("memory:{record_id}"))
    }

    /// Advance the item table to `turn` and mark every item whose lease ran out to leave at the
    /// next boundary. Runs at the head of every event, like the skill lease sweep.
    pub fn sweep_expired_item_leases(&mut self, turn: u32) {
        self.partitions.knowledge.turn = turn;
        let held = self.held_by_resident_items();
        self.partitions.knowledge.expire_leases(&held);
    }

    /// Plan task 7.3 · knowledge items a resident item still holds by a strong reference or a
    /// read-back: they neither expire nor yield to the knowledge budget.
    fn held_by_resident_items(&self) -> std::collections::BTreeSet<ItemId> {
        let history: std::collections::BTreeSet<ItemId> = (0..self.partitions.history.len())
            .map(|index| self.partitions.history.tag(index).id)
            .collect();
        self.graph
            .edges()
            .iter()
            .filter(|edge| {
                matches!(edge.relation, Relation::ReferencesStrong | Relation::ReadsBack)
                    && history.contains(&edge.from)
            })
            .map(|edge| edge.to)
            .collect()
    }

    /// Plan task 7.3 · how a history message's mentions of knowledge count: the model's words and
    /// the user's by any evidence, host output only by name, a memory hit (the record's own text,
    /// written into history by the kernel) never.
    fn evidence_of(message: &CoreMessage) -> super::partitions::Evidence {
        use super::partitions::Evidence;
        match message.role {
            crate::types::message::Role::Assistant => Evidence::Any,
            crate::types::message::Role::User if Self::memory_hit_record(message).is_some() => {
                Evidence::None
            }
            crate::types::message::Role::User => Evidence::Any,
            _ => Evidence::StrongOnly,
        }
    }

    /// The record a kernel-written memory hit (`[MEMORY record_ref=<id> kind=…] <content>`) carries.
    fn memory_hit_record(message: &CoreMessage) -> Option<&str> {
        if message.role != crate::types::message::Role::User {
            return None;
        }
        let text = message.content.as_text()?;
        let rest = text.strip_prefix("[MEMORY record_ref=")?;
        let end = rest.find([' ', ']'])?;
        Some(&rest[..end]).filter(|id| !id.is_empty())
    }

    /// Plan task 7.3 · one hop: `item` (counted evidence) used the content of a resident memory hit
    /// that expands a recalled item; the use reaches the recalled item along the `expands` edge.
    fn credit_expansions(&mut self, item: ItemId, message: &CoreMessage) {
        let history = &self.partitions.history;
        let carriers: Vec<(ItemId, ItemId, usize)> = self
            .graph
            .edges()
            .iter()
            .filter(|edge| edge.relation == Relation::Expands)
            .filter_map(|edge| {
                (0..history.len())
                    .find(|index| history.tag(*index).id == edge.from)
                    .map(|index| (edge.from, edge.to, index))
            })
            .collect();
        for (carrier, recalled, index) in carriers {
            let carried = &self.partitions.history.messages()[index];
            if !super::partitions::mentions(message, carried) {
                continue;
            }
            self.add_edge(item, carrier, Relation::ReferencesLexical);
            self.partitions
                .knowledge
                .credit_lexical(recalled, self.knowledge_reference_step);
        }
    }

    /// The rendered context is about to go to a provider: from now on system[1] is cached.
    pub fn mark_prefix_sent(&mut self) {
        self.prefix_sent = true;
    }

    /// The run's context lifecycle metrics (plan task 4.1).
    pub fn lifecycle_metrics(&self) -> LifecycleMetrics {
        let knowledge = &self.partitions.knowledge;
        let referenced = knowledge.entries.iter().filter(|e| e.value.ref_count > 0).count() as u32;
        let resident_waste: u64 = knowledge
            .entries
            .iter()
            .filter(|e| e.value.ref_count == 0)
            .map(|e| u64::from(e.tokens))
            .sum();
        let items = knowledge.item_records();
        let recalls = items.iter().filter(|item| item.origin == ItemOrigin::MemoryRecall);
        let (recalled, recall_expanded) = recalls.fold((0, 0), |(all, expanded), item| {
            (all + 1, expanded + u32::from(item.value.expanded))
        });
        let mut history = self.history_tally.clone();
        let partition = &self.partitions.history;
        for index in 0..partition.len() {
            let tag = partition.tag(index);
            if let Some(tally) = history.get_mut(&tag.origin) {
                tally.resident_items += 1;
                tally.resident_tokens += u64::from(partition.measured_tokens(index, &self.engine));
            }
        }
        if !self.dependency_bodies.is_empty() {
            let tally = history.entry(ItemOrigin::DependencyOutput).or_insert(OriginTally {
                origin: ItemOrigin::DependencyOutput,
                items: 0,
                tokens: 0,
                resident_items: 0,
                resident_tokens: 0,
                read_back: 0,
            });
            tally.items = self.dependency_bodies.len() as u32;
            tally.tokens = self
                .dependency_bodies
                .values()
                .map(|body| u64::from(self.engine.count(body)))
                .sum();
        }
        let history_len = self.partitions.history.len();
        let resident = (0..history_len).map(|index| {
            HistoryItemRecord::of(
                self.partitions.history.tag(index),
                self.partitions.history.measured_tokens(index, &self.engine),
                self.history_fidelity(index),
                None,
            )
        });
        let mut history_items: Vec<HistoryItemRecord> =
            self.retired_history.iter().copied().chain(resident).collect();
        let overflow = history_items.len().saturating_sub(HISTORY_ITEM_DETAIL_CAP);
        history_items.drain(..overflow);
        let waste_items = items
            .iter()
            .filter(|item| {
                item.id != 0
                    && item.value.ref_count == 0
                    && self.graph.in_edges(item.id).next().is_none()
            })
            .map(|item| item.id)
            .collect();
        let used: std::collections::BTreeSet<ItemId> = items
            .iter()
            .filter(|item| item.value.ref_count > 0 || self.graph.in_edges(item.id).next().is_some())
            .map(|item| item.id)
            .collect();
        let parent_items_used = self
            .graph
            .external()
            .iter()
            .filter(|edge| edge.relation == Relation::InheritedFrom && used.contains(&edge.from))
            .map(|edge| edge.to.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        LifecycleMetrics {
            external_edges: self.graph.external().to_vec(),
            parent_items_used,
            edges: self.graph.edges().to_vec(),
            edges_omitted: self.graph.omitted(),
            waste_items,
            history_items,
            history_items_omitted: self.retired_history_omitted + overflow as u32,
            history: history.into_values().collect(),
            knowledge_entries: knowledge.entries.len() as u32,
            knowledge_referenced: referenced,
            regret: knowledge.regret,
            waste_tokens: knowledge.wasted_tokens + resident_waste,
            cache_loss_tokens: self.cache_invalidated_tokens,
            recalled,
            recall_expanded,
            items,
        }
    }

    /// History tokens whose cache mid-run system[1] promotions discarded.
    pub fn cache_invalidated_tokens(&self) -> u64 {
        self.cache_invalidated_tokens
    }

    /// Place a just-appended knowledge entry. Before the prefix is sent it stays in system[1].
    /// After, it is staged in the state turn: re-sending it uncached each turn costs less than
    /// re-caching the history behind system[1] until the staged tokens reach a quarter of the
    /// history, at which point every staged entry is promoted at once.
    fn place_appended_knowledge(&mut self, entries_before: usize) {
        if self.partitions.knowledge.entries.len() == entries_before {
            return;
        }
        let id = self.alloc_item_id();
        let knowledge = &mut self.partitions.knowledge;
        if let Some(entry) = knowledge.entries.last_mut() {
            entry.id = id;
        }
        if !self.prefix_sent {
            return;
        }
        if let Some(entry) = knowledge.entries.last_mut() {
            entry.staged = true;
        }
        let history = self.partitions.history.token_count();
        if self.partitions.knowledge.staged_tokens().saturating_mul(4) >= history {
            self.partitions.knowledge.promote_staged();
            self.cache_invalidated_tokens += u64::from(history);
            self.prefix_sent = false;
        }
    }

    pub fn restore_frozen_history_len(&mut self, len: usize) -> bool {
        if len > self.partitions.history.len() {
            return false;
        }
        self.frozen_history_len = len;
        true
    }

    /// Push content into the Knowledge slot (memory retrievals, skill defs, artifacts).
    pub fn push_knowledge(&mut self, msg: CoreMessage, tokens: u32) {
        let before = self.partitions.knowledge.entries.len();
        self.partitions.knowledge.push(msg, tokens);
        self.place_appended_knowledge(before);
    }

    /// K1: keyed knowledge push — a fresh key appends (placed by
    /// [`Self::place_appended_knowledge`]), an existing key stages a boundary-deferred upsert.
    /// `pinned` entries are exempt from the K2 budget sweep.
    pub fn push_knowledge_entry(
        &mut self,
        key: Option<CompactString>,
        msg: CoreMessage,
        tokens: u32,
        pinned: bool,
    ) {
        let origin = ItemOrigin::for_host_key(key.as_deref());
        self.push_knowledge_item(origin, key, msg, tokens, pinned);
    }

    /// [`Self::push_knowledge_entry`] for an item whose origin the caller knows.
    pub fn push_knowledge_item(
        &mut self,
        origin: ItemOrigin,
        key: Option<CompactString>,
        msg: CoreMessage,
        tokens: u32,
        pinned: bool,
    ) {
        let before = self.partitions.knowledge.entries.len();
        self.partitions
            .knowledge
            .push_item(origin, key, msg, tokens, pinned);
        self.place_appended_knowledge(before);
    }

    /// K1: mark a keyed knowledge entry for removal at the next compaction/renewal boundary.
    /// Errs-open: unknown key is a no-op (returns false).
    /// Plan task 1.1 · scope: the parent's knowledge a child spawned with `inheritance` starts with,
    /// as `(key, content, tokens, pinned, item id)`. Besides what the scope allows, a child never inherits
    /// an item whose life is tied to the parent's own state: one leased until a parent event (an
    /// active skill's body) or one the kernel derived for the parent (the child's kernel derives
    /// its own).
    pub fn inheritable_knowledge(
        &self,
        inheritance: crate::types::agent::ContextInheritance,
    ) -> Vec<(Option<String>, String, u32, bool, ItemId)> {
        self.partitions
            .knowledge
            .entries
            .iter()
            .filter(|entry| !entry.is_leaving())
            .filter(|entry| entry.defaults().scope.inherited_by(inheritance))
            .filter(|entry| !matches!(entry.lease, crate::context::item::Lease::Until(_)))
            .filter(|entry| entry.defaults().authority != Authority::Kernel)
            .filter_map(|entry| {
                let text = entry.message.content.as_text()?.to_string();
                Some((
                    entry.key.as_ref().map(ToString::to_string),
                    text,
                    entry.tokens,
                    entry.pinned,
                    entry.id,
                ))
            })
            .collect()
    }

    /// Plan task 1.1 · authority: whether the host may upsert `upserts` and remove `removals`. A host
    /// command may not touch an item the kernel or the provider wrote, and may not remove a skill
    /// body while the model's activation holds it (deactivating the skill removes it). Returns the
    /// first refused key and why.
    pub fn check_host_knowledge_write<'a>(
        &self,
        upserts: impl IntoIterator<Item = &'a str>,
        removals: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), String> {
        let live = |key: &str| {
            self.partitions
                .knowledge
                .entries
                .iter()
                .find(|entry| !entry.is_leaving() && entry.key.as_deref() == Some(key))
        };
        for key in upserts {
            if let Some(entry) = live(key) {
                let authority = entry.defaults().authority;
                if matches!(authority, Authority::Kernel | Authority::Provider) {
                    return Err(format!(
                        "knowledge {key:?} is a {} item written by the {}; a host may not \
                         overwrite it",
                        entry.origin.label(),
                        authority.label()
                    ));
                }
            }
        }
        for key in removals {
            let Some(entry) = live(key) else { continue };
            match entry.defaults().authority {
                Authority::Kernel | Authority::Provider => {
                    return Err(format!(
                        "knowledge {key:?} is a {} item written by the {}; a host may not remove \
                         it",
                        entry.origin.label(),
                        entry.defaults().authority.label()
                    ));
                }
                Authority::Model
                    if entry.origin == ItemOrigin::Skill
                        && key
                            .strip_prefix("skill:")
                            .is_some_and(|name| self.active_skills.contains_key(name)) =>
                {
                    return Err(format!(
                        "knowledge {key:?} is the body of an active skill; deactivate the skill \
                         instead of removing its body"
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn remove_knowledge(&mut self, key: &str) -> bool {
        let removed = self.partitions.knowledge.remove(key);
        if removed {
            }
        removed
    }

    /// K1: run the boundary sweep (apply pending upserts, drop marked entries) and stash the
    /// result for the state machine to drain into a `KnowledgeSwept` observation. Called only
    /// from the compaction/renewal boundary blocks — the one place system[1] bytes may change.
    fn sweep_knowledge_at_boundary(&mut self) {
        let sweep = self.partitions.knowledge.sweep_at_boundary();
        if sweep.changed {
                // P9: the model must not have knowledge silently vanish under it. The boundary
            // already broke the prompt-cache prefix, so a one-line ephemeral tail note is
            // cache-free; keyed removals name what left and how to get it back.
            if !sweep.removed_keys.is_empty() {
                self.partitions.signals.push(format!(
                    "[KNOWLEDGE] entries removed at this boundary: {} — re-fetch via the memory tool if still needed.",
                    sweep.removed_keys.join(", ")
                ));
            }
            self.pending_knowledge_sweeps.push(sweep);
        }
        // K2: a boundary starts a fresh cache generation — the budget warning may fire again.
        self.knowledge_budget_warned = false;
        self.prefix_sent = false;
        self.prune_item_graph();
    }

    /// K2: knowledge-budget check, run per turn before render. Over budget ⇒ mark the LOWEST-VALUE
    /// unpinned, non-skill entries for eviction at the next boundary until the projected usage
    /// (used − already-marked) fits, and return `Some((used, budget))` ONCE per cache generation
    /// for the `KnowledgeBudgetExceeded` observation (marking itself is idempotent and repeats
    /// harmlessly). Skill pins are exempt — deactivation/lease governs them, the budget never
    /// silently unloads a skill the model believes is active. If marking every eligible entry
    /// still exceeds the budget, the warning stands and the overweight remainder is the host's
    /// explicit choice (errs-open). `knowledge_budget_ratio <= 0.0` disables.
    pub fn enforce_knowledge_budget(&mut self) -> Option<(u32, u32)> {
        let ratio = self.config.knowledge_budget_ratio;
        if ratio <= 0.0 {
            return None;
        }
        let budget = (self.max_tokens as f64 * ratio) as u32;
        let used = self.partitions.knowledge.token_count;
        if used <= budget {
            return None;
        }
        let marked: u32 = self
            .partitions
            .knowledge
            .entries
            .iter()
            .filter(|e| e.is_leaving())
            .map(|e| e.tokens)
            .sum();
        let mut projected = used.saturating_sub(marked);
        let held = self.held_by_resident_items();
        let mut candidates = self
            .partitions
            .knowledge
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                !entry.is_leaving()
                    && !held.contains(&entry.id)
                    && !entry.pinned
                    && !entry
                        .key
                        .as_deref()
                        .is_some_and(|key| key.starts_with("skill:"))
            })
            .map(|(index, _)| {
                let score = self
                    .partitions
                    .knowledge
                    .retention_score(index, self.knowledge_reference_step)
                    .unwrap_or(i64::MIN);
                (score, index)
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        for (_, index) in candidates {
            if projected <= budget {
                break;
            }
            let entry = &mut self.partitions.knowledge.entries[index];
            entry.leaving = Some(ItemEnd::Evicted);
            projected = projected.saturating_sub(entry.tokens);
        }
        if self.knowledge_budget_warned {
            return None;
        }
        self.knowledge_budget_warned = true;
        Some((used, budget))
    }

    /// K1: drain boundary-sweep results (state-machine side turns these into observations).
    pub fn take_knowledge_sweeps(&mut self) -> Vec<crate::context::partitions::KnowledgeSweep> {
        std::mem::take(&mut self.pending_knowledge_sweeps)
    }

    /// Push a runtime signal into the current turn's State slot.
    /// Rendering does not consume signals. The state machine clears only the prefix acknowledged by
    /// a correlated provider result, so provider failures and retries see the same signal payload.
    pub fn push_signal(&mut self, text: String) {
        self.partitions.signals.push(text);
    }

    /// Record a durable user directive in the (non-compressible, renewal-carried) task_state, so a
    /// mid-task user command keeps its salience across compaction/renewal — unlike the ephemeral
    /// signal channel, which is cleared on renewal.
    pub fn record_directive(&mut self, text: impl Into<String>) {
        self.partitions.task_state.record_directive(text);
    }

    // ── Task state ────────────────────────────────────────────────────────────

    pub fn init_task(&mut self, goal: String, criteria: Vec<String>) {
        self.recall_due = self.recall_top_k.is_some();
        self.partitions.task_state = TaskState {
            goal,
            criteria,
            ..Default::default()
        };
    }

    pub fn update_task(&mut self, update: TaskUpdate) {
        self.partitions.task_state.apply(update);
    }

    /// 2b: record this turn's tool activity into the task-state recency log (kernel-derived progress
    /// that feeds the State-turn footer). Each entry is `(name, compact_args)`; the rendered signature
    /// is `name(args)` (or bare `name` for no-arg calls) so the no-progress STOP keys on the WHOLE
    /// call — same tool with different args (a legit loop over items) reads as distinct progress, not
    /// a repeat. Control-plane meta-tools (plan/skill/memory/knowledge/workflow authoring) are noise,
    /// not task progress — filtered by name. A turn with only meta-tool calls records nothing.
    pub fn note_tool_actions(&mut self, calls: &[(String, String)]) {
        let summary = calls
            .iter()
            .filter(|(name, _)| !is_meta_tool(name))
            .map(|(name, args)| {
                if args.is_empty() {
                    name.clone()
                } else {
                    format!("{name}({args})")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.partitions.task_state.note_actions(summary);
    }

    // ── Section pinning ───────────────────────────────────────────────────────

    // ── Skills ────────────────────────────────────────────────────────────────

    pub fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.capabilities.remove_kind(CapabilityKind::Skill);
        for skill in &skills {
            self.capabilities.add_skill(skill.clone());
        }
        self.skills.set_available(skills);
    }

    /// P1-B/D: set the stable-core tool ids (always exposed under skill gating). Replaces any prior.
    pub fn set_stable_core_tools(&mut self, ids: impl IntoIterator<Item = CompactString>) {
        self.stable_core_tools = ids.into_iter().collect();
    }

    /// P1-B: record that the model has loaded a skill (its content is now in context). Returns
    /// `true` if this changed the active set — an epoch boundary the SDK can use to re-anchor the
    /// prompt cache (D). Re-activating an already-active skill refreshes its lease (K3) but
    /// returns false (no epoch change).
    pub fn activate_skill(&mut self, name: impl Into<CompactString>) -> bool {
        self.activate_skill_leased(name, None)
    }

    /// K3: activate with an optional lease expiry turn (`None` = permanent). Same epoch semantics
    /// as [`Self::activate_skill`]; a re-activation overwrites the prior lease (latest wins).
    pub fn activate_skill_leased(
        &mut self,
        name: impl Into<CompactString>,
        expires_at_turn: Option<u32>,
    ) -> bool {
        self.active_skills
            .insert(name.into(), expires_at_turn)
            .is_none()
    }

    /// A refused `skill` activation: withdraw the `skill:<name>` knowledge the host staged for it,
    /// unless the skill is already active under an earlier admission.
    pub fn retract_refused_skill(&mut self, name: &str) -> bool {
        if self.active_skills.contains_key(name) {
            return false;
        }
        self.partitions
            .knowledge
            .retract_staged(&format!("skill:{name}"))
    }

    /// K3: deactivate a skill — the toolset re-widens at the next `emit_call_llm` (an epoch event,
    /// same cache cost class as activation) and the skill's `skill:<name>` knowledge pin is marked
    /// for the next boundary sweep. Errs-open: not-active is a no-op (returns false).
    pub fn deactivate_skill(&mut self, name: &str) -> bool {
        if self.active_skills.remove(name).is_none() {
            return false;
        }
        let key = format!("skill:{name}");
        // A skill body restored from an older checkpoint may hold a permanent lease.
        if !self.partitions.knowledge.end_lease(LeaseEvent::SkillDeactivated, &key) {
            self.partitions.knowledge.remove(&key);
        }
        true
    }

    /// K3: expire skill leases whose turn has passed (mirrors the capability lease sweep — runs at
    /// the head of every event). Each expiry takes the same path as an explicit deactivation.
    pub fn sweep_expired_skill_leases(&mut self, current_turn: u32) {
        let expired: Vec<CompactString> = self
            .active_skills
            .iter()
            .filter(|(_, lease)| lease.is_some_and(|t| current_turn >= t))
            .map(|(name, _)| name.clone())
            .collect();
        for name in expired {
            self.deactivate_skill(&name);
            // P9: lease expiry re-widens the toolset invisibly otherwise — tell the model.
                self.partitions.signals.push(format!(
                "[SKILL] lease expired: {name} unloaded; the full toolset is restored."
            ));
        }
    }

    /// P1-B: the tool-id allow-set to narrow the exposed toolset to, given the active skills.
    /// Returns `None` ⇒ **do not narrow** (no skill active, or some active skill declares no
    /// `allowed_tools` ⇒ unbounded, errs-open per D3). `Some(set)` ⇒ narrow to `set` (the union of
    /// every active skill's declared tools). Meta-tools and stable-core are layered on in
    /// `emit_call_llm`, not here.
    pub fn active_skill_tool_filter(&self) -> Option<std::collections::HashSet<CompactString>> {
        if self.active_skills.is_empty() {
            return None;
        }
        let mut union = std::collections::HashSet::new();
        for name in self.active_skills.keys() {
            let declared = self.skills.allowed_tools(name);
            if declared.is_empty() {
                return None; // an unrestricted active skill ⇒ no narrowing (D3)
            }
            union.extend(declared.iter().cloned());
        }
        Some(union)
    }

    /// Fine-grained authority contributed by active skills. Activation is checked by the canonical
    /// driver before it mutates this set; deactivation and lease expiry remove grants by removing
    /// the skill name from `active_skills`.
    pub fn active_skill_capabilities(&self) -> Vec<Capability> {
        self.active_skills
            .keys()
            .flat_map(|name| self.skills.capability_grants(name).iter().cloned())
            .collect()
    }

    pub fn skill_capability_grants(&self, name: &str) -> &[Capability] {
        self.skills.capability_grants(name)
    }

    pub fn skill_tool_schema(&self) -> Option<ToolSchema> {
        self.skills.build_tool_schema()
    }

    /// Whether the operation's catalog declares this skill (see
    /// [`SkillCatalog::is_available`](crate::context::skill_catalog::SkillCatalog::is_available)).
    pub fn skill_available(&self, name: &str) -> bool {
        self.skills.is_available(name)
    }

    // ── Meta-tools ────────────────────────────────────────────────────────────

    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Memory,
                MEMORY_TOOL_NAME,
                "Search long-term memory through the memory meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Memory, MEMORY_TOOL_NAME);
        }
    }

    pub fn set_propose_memory_enabled(&mut self, enabled: bool) {
        self.propose_memory_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Memory,
                PROPOSE_MEMORY_TOOL_NAME,
                "Propose a candidate for long-term memory review.",
            );
        } else {
            self.capabilities.remove(CapabilityKind::Memory, PROPOSE_MEMORY_TOOL_NAME);
        }
    }

    pub fn set_knowledge_enabled(&mut self, enabled: bool) {
        self.knowledge_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Knowledge,
                KNOWLEDGE_TOOL_NAME,
                "Search external knowledge through the knowledge meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Knowledge, KNOWLEDGE_TOOL_NAME);
        }
    }

    pub fn set_plan_tool_enabled(&mut self, enabled: bool) {
        self.plan_tool_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Tool,
                "update_plan",
                "Update task plan and progress through the planning meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Tool, "update_plan");
        }
    }

    pub fn capability_inventory(&self) -> String {
        self.capabilities.format_inventory()
    }

    pub fn meta_tool_schemas(&self) -> Vec<ToolSchema> {
        let mut tools = Vec::new();
        if let Some(t) = self.skill_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.memory_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.propose_memory_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.knowledge_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.plan_tool_schema() {
            tools.push(t);
        }
        if let Some(t) = self.read_result_tool_schema() {
            tools.push(t);
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    /// O7: the `read_result` meta-tool — re-fetch a tool result the kernel evicted from context
    /// (external / collapsed / paged out). Exposed DYNAMICALLY: only once at least one
    /// handle has actually left residency, so runs that never evict see an unchanged toolset
    /// (progressive disclosure; golden fixtures and cache prefixes stay byte-stable). Content is
    /// host-resolved through the payload store — the kernel only advertises the capability.
    pub fn read_result_tool_schema(&self) -> Option<ToolSchema> {
        let any_evicted = !self.dependency_bodies.is_empty()
            || self
                .handles
                .all()
                .iter()
                .any(|h| !h.residency.occupies_context());
        if !any_evicted {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(READ_RESULT_TOOL_NAME),
            description: "Re-read the full content of a tool result that was truncated to save \
                          context (marked '[Output truncated: … call the read_result tool …]'), \
                          or of a dependency output (call_id \"dep:<agent_id>\"). Pass that \
                          call_id; use offset/max_bytes to page through large content."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "call_id": { "type": "string" },
                    "offset": { "type": "integer", "description": "Byte offset to start from (default 0)." },
                    "max_bytes": { "type": "integer", "description": "Max bytes to return (default 4000)." }
                },
                "required": ["call_id"]
            }),
        })
    }

    pub fn plan_tool_schema(&self) -> Option<ToolSchema> {
        if !self.plan_tool_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new("update_plan"),
            description: "Update your task plan and progress. Call this after completing a step or when the plan changes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": { "type": "array", "items": { "type": "string" } },
                    "current_step": { "type": "integer" },
                    "progress": { "type": "string" },
                    "blocked_on": { "type": "array", "items": { "type": "string" } }
                }
            }),
        })
    }

    pub fn memory_tool_schema(&self) -> Option<ToolSchema> {
        if !self.memory_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(MEMORY_TOOL_NAME),
            description: "Read your long-term memory. Pass `query` to search it for relevant past \
experiences and knowledge, or `record_id` to read one record in full. Recalled memories arrive as \
index lines `[MEMORY record_id=… kind=… trust=…] <description>`: when one looks relevant to the \
task, read it by its record_id before relying on it."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to search for. Omit when passing record_id." },
                    "record_id": { "type": "string", "description": "Read this one record in full, as named by a recalled index line. Omit when passing query." },
                    "top_k": { "type": "integer" }
                }
            }),
        })
    }

    pub fn propose_memory_tool_schema(&self) -> Option<ToolSchema> {
        if !self.propose_memory_enabled { return None; }
        Some(ToolSchema {
            name: CompactString::new(PROPOSE_MEMORY_TOOL_NAME),
            description: "Propose one fact for long-term memory, so it is available in future sessions. \
Propose when you learn something that will still matter later and cannot be recovered from the code, files, or this conversation's history: \
who the user is and how they like to work, a correction or confirmed approach they gave you (with why), \
a project goal, constraint, or decision, or a pointer to an external resource. \
Do not propose what the workspace already records, temporary task state, or guesses. \
Search memory first and reuse an existing name to update a fact rather than duplicating it. \
The proposal is reviewed before it is stored; its outcome arrives in this conversation before your next turn. \
Propose one fact per call and wait for the outcome before proposing another. \
Scope, identity, provenance, and timestamps are derived for you.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string", "description": "Short stable kebab-case slug for this fact; reuse an existing name to update it." },
                    "kind": { "type": "string", "enum": ["user", "feedback", "project", "reference"], "description": "user: who the user is and their preferences. feedback: guidance on how to work. project: goals, constraints, decisions. reference: pointers to external resources." },
                    "content": { "type": "string", "description": "The fact itself, self-contained and stated so a future session can act on it without this conversation." },
                    "description": { "type": "string", "description": "One-line summary used to decide relevance at recall time." },
                    "evidence_refs": { "type": "array", "items": { "type": "string" }, "description": "Optional references (call ids, record ids, paths) supporting the fact." }
                },
                "required": ["name", "kind", "content"]
            }),
        })
    }

    pub fn knowledge_tool_schema(&self) -> Option<ToolSchema> {
        if !self.knowledge_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(KNOWLEDGE_TOOL_NAME),
            description:
                "Search the external knowledge base for facts, documentation, or reference data."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "top_k": { "type": "integer" }
                },
                "required": ["query"]
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::task_state::PlanStep;
    use crate::types::message::CoreMessage;
    use crate::types::skill::SkillMetadata;

    #[test]
    fn low_provider_usage_does_not_hide_resident_history_pressure() {
        let mut mgr = ContextManager::with_config(
            1_000,
            ContextConfig::default(),
            ContextTokenEngine::char_approx(),
        );
        mgr.push_history(CoreMessage::user("large history"), 1_100);
        mgr.set_observed_prompt_tokens(10);
        assert_eq!(mgr.should_compress(), PressureAction::AutoCompact);
        mgr.compress(PressureAction::AutoCompact);
        assert!(!mgr.should_renew());
    }

    fn staged_text(mgr: &ContextManager) -> String {
        mgr.render()
            .state_turn
            .and_then(|turn| turn.content.as_text().map(str::to_string))
            .unwrap_or_default()
    }

    #[test]
    fn lifecycle_metrics_count_regret_waste_and_cache_loss() {
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_knowledge_entry(Some("memory:a".into()), CoreMessage::system("alpha notes"), 7, false);
        mgr.push_knowledge_entry(Some("memory:b".into()), CoreMessage::system("beta notes"), 5, false);
        mgr.push_history(CoreMessage::user("tell me about memory:b"), 10);
        assert!(mgr.remove_knowledge("memory:a"));
        mgr.sweep_knowledge_at_boundary();
        // memory:a left unreferenced (waste) and comes back (regret).
        mgr.push_knowledge_entry(Some("memory:a".into()), CoreMessage::system("alpha notes"), 7, false);
        let metrics = mgr.lifecycle_metrics();
        assert_eq!(
            (metrics.knowledge_entries, metrics.knowledge_referenced, metrics.regret),
            (2, 1, 1)
        );
        assert_eq!(metrics.waste_tokens, 7 + 7, "evicted unreferenced + resident unreferenced");
        assert_eq!(metrics.cache_loss_tokens, 0);
        let items: Vec<_> = metrics
            .items
            .iter()
            .map(|item| (item.key.as_deref().unwrap(), item.end, item.value.paged_in_after_evict))
            .collect();
        assert_eq!(
            items,
            vec![
                ("memory:a", Some(ItemEnd::Removed), false),
                ("memory:b", None, false),
                ("memory:a", None, true),
            ]
        );
    }

    #[test]
    fn the_item_graph_records_answers_references_reads_back_and_waste() {
        use crate::types::message::ToolCall;
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_knowledge_entry(Some("project:orchid".into()), CoreMessage::system("orchid storage layout"), 6, false);
        mgr.push_recalled(vec![
            ("r1".into(), "[MEMORY record_id=r1 kind=project] mercury deploy freeze".into()),
            ("r2".into(), "[MEMORY record_id=r2 kind=project] office wifi rotation".into()),
        ]);
        let ids: Vec<ItemId> = mgr.partitions.knowledge.entries.iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![1, 2, 3], "knowledge items draw from the item allocator");
        let (orchid, r1, r2) = (ids[0], ids[1], ids[2]);

        mgr.push_history(CoreMessage::user("question"), 5);
        let mut call = CoreMessage::assistant("reading");
        call.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: serde_json::json!({ "key": "project:orchid" }),
        }];
        mgr.push_history(call, 10);
        mgr.push_history(tool_result_msg("c1", "the layout"), 10);
        let tag = |index: usize| mgr.partitions.history.tag(index).id;
        let (assistant, result) = (tag(1), tag(2));
        assert!(assistant > r2, "history and knowledge share one allocator");
        mgr.note_read_back("c1");
        mgr.note_memory_read("r1");
        mgr.note_memory_read("no-such-record");

        let metrics = mgr.lifecycle_metrics();
        let edges: Vec<_> = metrics.edges.iter().map(|e| (e.from, e.to, e.relation)).collect();
        assert_eq!(
            edges,
            vec![
                (assistant, orchid, Relation::ReferencesStrong),
                (result, assistant, Relation::Answers),
                (assistant, result, Relation::ReadsBack),
                (assistant, r1, Relation::ReadsBack),
            ]
        );
        assert_eq!(metrics.waste_items, vec![r2], "r1 was read back, orchid referenced");
        let recorded: Vec<_> = metrics.items.iter().map(|item| item.id).collect();
        assert_eq!(recorded, ids);
    }

    #[test]
    fn host_output_and_memory_hits_do_not_count_as_use_but_the_models_words_do() {
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_recalled(vec![
            ("vat".into(), "[MEMORY record_id=vat kind=project] tax ledger VAT rounding per transaction".into()),
        ]);
        let refs = |mgr: &ContextManager| mgr.partitions.knowledge.entries[0].value.ref_count;
        // A ledger page and a search hit share the record's words: recorded, not counted.
        mgr.push_history(tool_result_msg("c1", "transaction ledger page 1: rounding totals"), 10);
        mgr.push_history(
            CoreMessage::user("[MEMORY record_ref=other kind=project] the tax ledger rounds each transaction"),
            10,
        );
        assert_eq!(refs(&mgr), 0);
        assert_eq!(mgr.graph.edges().iter().filter(|e| e.relation == Relation::ReferencesLexical).count(), 2);
        mgr.push_history(CoreMessage::assistant("VAT rounding in the tax ledger applies here"), 10);
        assert_eq!(refs(&mgr), 1, "the model's own words are evidence");
    }

    #[test]
    fn using_a_memory_hits_content_reaches_the_recall_it_expands() {
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_recalled(vec![(
            "flag-rule".into(),
            "[MEMORY record_id=flag-rule kind=project] ledger audit: which flags count".into(),
        )]);
        let recalled = mgr.partitions.knowledge.entries[0].id;
        mgr.push_history(
            CoreMessage::user("[MEMORY record_ref=flag-rule kind=project] REVIEW-HOLD counts as REVIEW in audit totals"),
            10,
        );
        let hit = mgr.partitions.history.tag(mgr.partitions.history.len() - 1).id;
        assert!(mgr.graph.edges().contains(&ItemEdge { from: hit, to: recalled, relation: Relation::Expands, turn: 0 }));
        assert_eq!(mgr.partitions.knowledge.entries[0].value.ref_count, 0);
        mgr.push_history(CoreMessage::assistant("T0042 is REVIEW-HOLD, so it counts toward the totals"), 10);
        let used = mgr.partitions.history.tag(mgr.partitions.history.len() - 1).id;
        assert!(mgr.graph.edges().iter().any(|e| e.from == used && e.to == hit));
        assert_eq!(mgr.partitions.knowledge.entries[0].value.ref_count, 1, "one hop along expands");
    }

    #[test]
    fn an_item_a_resident_item_names_neither_expires_nor_yields_to_the_budget() {
        use crate::context::item::RECALL_LEASE_TURNS;
        let mut mgr = ContextManager::new(1_000);
        mgr.config.knowledge_budget_ratio = 0.01;
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_recalled(vec![
            ("kept".into(), "[MEMORY record_id=kept kind=project] orchid storage layout".into()),
            ("idle".into(), "[MEMORY record_id=idle kind=project] mercury deploy freeze".into()),
        ]);
        let mut call = CoreMessage::assistant("reading");
        call.tool_calls = vec![crate::types::message::ToolCall {
            id: "c1".into(),
            name: "memory".into(),
            arguments: serde_json::json!({ "record_id": "kept" }),
        }];
        mgr.push_history(call, 10);
        mgr.enforce_knowledge_budget();
        let leaving = |mgr: &ContextManager| -> Vec<_> {
            mgr.partitions.knowledge.entries.iter().map(|e| (e.key.clone().unwrap(), e.is_leaving())).collect()
        };
        assert_eq!(leaving(&mgr), vec![("memory:kept".into(), false), ("memory:idle".into(), true)]);
        mgr.sweep_expired_item_leases(10 * RECALL_LEASE_TURNS);
        assert!(!mgr.partitions.knowledge.entries[0].is_leaving(), "held by the resident call");
    }

    #[test]
    fn a_child_reports_which_inherited_parent_items_it_used_and_what_it_read_from_dependencies() {
        use crate::types::message::ToolCall;
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        let parent = |item| ExternalItem { operation: Some("op-parent".into()), item, ..ExternalItem::default() };
        mgr.push_knowledge_entry(Some("style".into()), CoreMessage::system("house style forbids bullets"), 4, true);
        mgr.note_inherited_from(Some("style"), parent(7));
        mgr.push_knowledge_entry(Some("notes".into()), CoreMessage::system("orchid storage layout"), 4, false);
        mgr.note_inherited_from(Some("notes"), parent(9));
        let mut call = CoreMessage::assistant("no bullets per the house style");
        call.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "read_result".into(),
            arguments: serde_json::json!({ "call_id": "dep:wf-node0" }),
        }];
        mgr.push_history(call, 10);
        let reader = mgr.partitions.history.tag(0).id;
        mgr.note_read_back("dep:wf-node0");

        let metrics = mgr.lifecycle_metrics();
        assert_eq!(metrics.parent_items_used, vec![parent(7)], "the notes copy went unused");
        let relations: Vec<_> = metrics.external_edges.iter().map(|e| (e.from, e.relation)).collect();
        let (style, notes) = (mgr.partitions.knowledge.entries[0].id, mgr.partitions.knowledge.entries[1].id);
        assert_eq!(
            relations,
            vec![
                (style, Relation::InheritedFrom),
                (notes, Relation::InheritedFrom),
                (reader, Relation::DependsOn),
            ]
        );
        assert_eq!(metrics.external_edges[2].to.agent.as_deref(), Some("wf-node0"));
    }

    #[test]
    fn a_recall_left_unused_expires_and_leaves_at_the_next_boundary() {
        use crate::context::item::RECALL_LEASE_TURNS;
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.sweep_expired_item_leases(1);
        mgr.push_recalled(vec![
            ("r1".into(), "[MEMORY record_id=r1 kind=project] orchid storage".into()),
            ("r2".into(), "[MEMORY record_id=r2 kind=project] mercury deploys".into()),
        ]);
        mgr.sweep_expired_item_leases(4);
        assert!(mgr.note_memory_expanded("r2"));
        mgr.sweep_expired_item_leases(1 + RECALL_LEASE_TURNS);
        let leaving: Vec<_> = mgr
            .partitions
            .knowledge
            .entries
            .iter()
            .map(|entry| entry.leaving)
            .collect();
        assert_eq!(leaving, vec![Some(ItemEnd::Expired), None], "r2 was used at turn 4");
        // Still rendered until the boundary: an expiry never rewrites cached bytes mid-generation.
        assert!(mgr.render().system_knowledge.contains("orchid storage"));
        mgr.sweep_knowledge_at_boundary();
        assert!(!mgr.render().system_knowledge.contains("orchid storage"));

        let metrics = mgr.lifecycle_metrics();
        assert_eq!((metrics.recalled, metrics.recall_expanded), (2, 1));
        assert_eq!(metrics.items[0].end, Some(ItemEnd::Expired));
        assert_eq!(metrics.items[0].origin, ItemOrigin::MemoryRecall);
    }

    #[test]
    fn a_skill_body_leaves_when_its_skill_is_deactivated_even_if_pinned() {
        use crate::context::item::{Lease, LeaseEvent};
        let mut mgr = ContextManager::new(100_000);
        mgr.activate_skill("debug");
        mgr.push_knowledge_entry(
            Some("skill:debug".into()),
            CoreMessage::system("debug skill body"),
            10,
            true,
        );
        let skill = &mgr.partitions.knowledge.entries[0];
        assert_eq!(skill.lease, Lease::Until(LeaseEvent::SkillDeactivated));
        mgr.sweep_expired_item_leases(1_000);
        assert!(!mgr.partitions.knowledge.entries[0].is_leaving(), "no turn limit");

        assert!(mgr.deactivate_skill("debug"));
        assert_eq!(mgr.partitions.knowledge.entries[0].leaving, Some(ItemEnd::Expired));
        mgr.sweep_knowledge_at_boundary();
        assert!(mgr.partitions.knowledge.is_empty());
        assert_eq!(mgr.lifecycle_metrics().items[0].end, Some(ItemEnd::Expired));
    }

    #[test]
    fn a_full_child_inherits_the_parents_knowledge_but_not_its_own_state() {
        use crate::types::agent::ContextInheritance as I;
        let mut mgr = ContextManager::new(100_000);
        mgr.push_knowledge_entry(Some("style".into()), CoreMessage::system("no bullets"), 3, true);
        mgr.push_recalled(vec![("r1".into(), "[MEMORY record_id=r1] orchid".into())]);
        mgr.activate_skill("debug");
        mgr.push_knowledge_entry(Some("skill:debug".into()), CoreMessage::system("body"), 3, true);
        mgr.push_knowledge_item(
            ItemOrigin::System,
            Some("governance:denied-tools".into()),
            CoreMessage::system("denied"),
            2,
            true,
        );
        mgr.push_knowledge_entry(Some("gone".into()), CoreMessage::system("leaving"), 2, false);
        mgr.remove_knowledge("gone");

        let keys = |inheritance| -> Vec<Option<String>> {
            mgr.inheritable_knowledge(inheritance).into_iter().map(|entry| entry.0).collect()
        };
        assert_eq!(keys(I::Full), vec![Some("style".to_string()), Some("memory:r1".to_string())]);
        assert!(keys(I::SystemOnly).is_empty());
        assert!(keys(I::None).is_empty());
        assert!(mgr.inheritable_knowledge(I::Full)[0].3, "a pin is inherited");
    }

    #[test]
    fn the_report_lists_history_items_and_why_each_left() {
        use crate::context::item::HISTORY_ITEM_DETAIL_CAP;
        let mut mgr = ContextManager::new(10_000);
        for i in 0..6 {
            mgr.push_history(CoreMessage::user(format!("message {i} ").repeat(80)), 400);
        }
        let (_, _, archived, _) = mgr.force_compress();
        assert!(!archived.is_empty());
        mgr.truncate_history(mgr.partitions.history.len() - 1);
        mgr.push_history(CoreMessage::user("after rollback"), 5);
        mgr.renew();

        let metrics = mgr.lifecycle_metrics();
        let ends: Vec<_> = metrics.history_items.iter().map(|item| item.end).collect();
        assert!(ends.contains(&Some(ItemEnd::Compacted)), "{ends:?}");
        assert!(ends.contains(&Some(ItemEnd::Removed)), "{ends:?}");
        let mut ids: Vec<_> = metrics.history_items.iter().map(|item| item.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), metrics.history_items.len(), "each item is listed once");
        assert_eq!(metrics.history_items_omitted, 0);

        // Past the cap only the latest items are listed; the rest are counted.
        let mut long = ContextManager::new(1_000_000);
        for i in 0..HISTORY_ITEM_DETAIL_CAP + 10 {
            long.push_history(CoreMessage::user(format!("m{i}")), 1);
        }
        let metrics = long.lifecycle_metrics();
        assert_eq!(metrics.history_items.len(), HISTORY_ITEM_DETAIL_CAP);
        assert_eq!(metrics.history_items_omitted, 10);
        assert_eq!(metrics.history_items.last().unwrap().id, (HISTORY_ITEM_DETAIL_CAP + 10) as u64);
    }

    #[test]
    fn the_estimate_calibrates_toward_what_the_provider_counts() {
        let mut mgr = ContextManager::new(100_000);
        assert_eq!(mgr.note_request_estimate(600), 600, "uncalibrated before any count");
        mgr.set_observed_prompt_tokens(810);
        assert_eq!(mgr.note_request_estimate(1_200), 1_410, "one sample: its offset");
        mgr.set_observed_prompt_tokens(1_470);
        // A fixed overhead plus a proportional gap (count = 1.1 x estimate + 150) is fitted
        // exactly once there are two sizes.
        for raw in [2_000u32, 2_800] {
            mgr.note_request_estimate(raw);
            mgr.set_observed_prompt_tokens(raw * 11 / 10 + 150);
        }
        assert_eq!(mgr.note_request_estimate(3_600), 4_110);
        // An observation with no estimate pending (a restore, a test) adds no sample.
        let samples = mgr.estimate_samples.len();
        mgr.last_request_estimate = 0;
        mgr.set_observed_prompt_tokens(50);
        assert_eq!(mgr.estimate_samples.len(), samples);
        // Only the most recent samples count.
        for _ in 0..ESTIMATE_CALIBRATION_SAMPLES {
            mgr.note_request_estimate(1_000);
            mgr.set_observed_prompt_tokens(1_000);
        }
        assert_eq!(mgr.note_request_estimate(5_000), 5_000);
    }

    #[test]
    fn replay_sent_back_with_a_turn_counts_toward_the_estimate() {
        let mut mgr = ContextManager::new(100_000);
        mgr.push_history(CoreMessage::user("question"), 5);
        let before = (mgr.render().estimated_tokens, mgr.rho());
        mgr.pending_assistant_replay =
            Some(serde_json::json!({ "reasoning_content": "step ".repeat(400) }));
        mgr.push_history(CoreMessage::assistant("answer"), 5);
        let (estimate, rho) = (mgr.render().estimated_tokens, mgr.rho());
        assert!(estimate >= before.0 + 5 + 300, "{estimate} vs {}", before.0);
        assert!(rho > before.1);
    }

    #[test]
    fn calibration_raises_pressure_but_never_lowers_it() {
        let mut mgr = ContextManager::new(1_000);
        mgr.push_history(CoreMessage::user("x".repeat(400)), 400);
        let raw = mgr.pressure.pressure(&mgr.partitions, &mgr.engine, None);
        mgr.estimate_samples = [(1_000, 500)].into();
        assert!((mgr.rho() - raw).abs() < 1e-9, "a low count does not delay compaction");
        mgr.estimate_samples = [(1_000, 1_200)].into();
        assert!((mgr.rho() - (raw + 0.2)).abs() < 1e-3, "an undercount raises pressure");
    }

    #[test]
    fn a_provider_summary_replaces_the_items_it_covered() {
        use crate::types::message::ToolCall;
        let mut mgr = ContextManager::new(100_000);
        mgr.push_history(CoreMessage::user("question"), 50);
        let mut call = CoreMessage::assistant("reading");
        call.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        }];
        mgr.push_history(call, 20);
        mgr.push_history(tool_result_msg("c1", &"body ".repeat(200)), 400);
        mgr.push_history(CoreMessage::user("next question"), 10);
        let (assistant, tool) = (mgr.partitions.history.tag(1).id, mgr.partitions.history.tag(2).id);

        let block = serde_json::json!({ "protocol": "anthropic-messages", "native_blocks": [{ "type": "compaction" }] });
        // Covering through the call extends to its result: a transaction is never split.
        assert!(mgr.absorb_provider_compaction(assistant, block.clone(), 30));
        let tags = mgr.partitions.history.tags();
        assert_eq!(tags[0].origin, ItemOrigin::ProviderSummary);
        assert_eq!(tags.len(), 2, "summary + the uncovered question");
        assert!(tags.iter().all(|tag| tag.id != tool));
        assert_eq!(mgr.partitions.history.token_count(), 30 + 10);
        assert!(mgr.handles.residency_for_source("c1").is_none(), "the covered result's handle left");

        let rendered = mgr.render();
        let summary = rendered.turn_items.iter().position(|id| *id == tags[0].id).unwrap();
        assert_eq!(rendered.turn_replays[summary], Some(block));
        let ends: Vec<_> = mgr.lifecycle_metrics().history_items.iter().map(|item| item.end).collect();
        assert_eq!(ends.iter().filter(|end| **end == Some(ItemEnd::Compacted)).count(), 3);
        let summarized: Vec<_> = mgr
            .graph
            .edges()
            .iter()
            .filter(|edge| edge.relation == Relation::Summarizes)
            .map(|edge| (edge.from, edge.to))
            .collect();
        assert_eq!(summarized.len(), 3, "the summary stands in for each item it covered");
        assert!(summarized.iter().all(|(from, _)| *from == tags[0].id));
        assert!(summarized.contains(&(tags[0].id, tool)));
        assert!(!mgr.absorb_provider_compaction(9_999, serde_json::json!({}), 1), "unknown item");
    }

    #[test]
    fn tool_results_the_provider_cleared_render_as_handles() {
        let mut mgr = ContextManager::new(100_000);
        mgr.push_history(tool_result_msg("c1", "first"), 5);
        mgr.push_history(tool_result_msg("c2", "second"), 5);
        mgr.absorb_cleared_tool_results(["c1"]);
        assert_eq!(mgr.handles.residency_for_source("c1"), Some(&Residency::Collapsed));
        assert_eq!(mgr.handles.residency_for_source("c2"), Some(&Residency::Resident));
        assert_eq!(mgr.history_fidelity(0), crate::context::item::Fidelity::Handle);
    }

    #[test]
    fn history_messages_are_items_whose_identity_survives_renewal() {
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_history(CoreMessage::user("question"), 5);
        let mut call = CoreMessage::assistant("reading");
        call.tool_calls.push(crate::types::message::ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        });
        mgr.push_history(call, 5);
        mgr.push_history(
            CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: "call-1".into(),
                output: "body".into(),
                is_error: false,
                durable_content: None,
            }]),
            7,
        );
        let tags: Vec<_> = (0..3).map(|i| mgr.partitions.history.tag(i)).collect();
        assert_eq!(tags.iter().map(|tag| tag.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(
            tags.iter().map(|tag| tag.origin).collect::<Vec<_>>(),
            vec![ItemOrigin::User, ItemOrigin::Assistant, ItemOrigin::ToolResult]
        );
        mgr.note_read_back("call-1");
        mgr.note_read_back("dep:wf-node0");

        mgr.renew();
        let carried: Vec<_> = (0..mgr.partitions.history.len())
            .map(|i| mgr.partitions.history.tag(i).id)
            .collect();
        assert!(carried.iter().all(|id| *id > 0), "renewal carries identity: {carried:?}");
        mgr.push_history(CoreMessage::user("next"), 3);
        let last = mgr.partitions.history.len() - 1;
        assert_eq!(mgr.partitions.history.tag(last).id, 4, "ids are never reused");

        let history = mgr.lifecycle_metrics().history;
        let tool = history.iter().find(|tally| tally.origin == ItemOrigin::ToolResult).unwrap();
        assert_eq!((tool.items, tool.tokens, tool.read_back), (1, 7, 1));
        let dependency = history
            .iter()
            .find(|tally| tally.origin == ItemOrigin::DependencyOutput)
            .unwrap();
        assert_eq!(dependency.read_back, 1);
        let user = history.iter().find(|tally| tally.origin == ItemOrigin::User).unwrap();
        assert_eq!(user.items, 2);
    }

    #[test]
    fn a_fresh_recall_of_a_resident_record_renews_its_lease() {
        use crate::context::item::RECALL_LEASE_TURNS;
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        let line = || vec![("r1".to_string(), "[MEMORY record_id=r1 kind=project] a".to_string())];
        mgr.push_recalled(line());
        mgr.sweep_expired_item_leases(5);
        mgr.push_recalled(line());
        mgr.sweep_expired_item_leases(RECALL_LEASE_TURNS);
        assert!(!mgr.partitions.knowledge.entries[0].is_leaving());
        mgr.sweep_expired_item_leases(5 + RECALL_LEASE_TURNS);
        assert!(mgr.partitions.knowledge.entries[0].is_leaving());
    }

    #[test]
    fn the_recall_index_line_matches_the_shared_vector() {
        assert_eq!(
            ContextManager::recall_index_line("record-v", "project", "untrusted", "how briefs are laid out", "fact"),
            "[MEMORY record_id=record-v kind=project trust=untrusted] how briefs are laid out"
        );
        assert_eq!(
            ContextManager::recall_index_line("record-v", "project", "untrusted", " ", "fact"),
            "[MEMORY record_id=record-v kind=project trust=untrusted] fact"
        );
    }

    #[test]
    fn recall_is_owed_at_start_and_after_each_renewal_and_dedupes_by_record() {
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        assert_eq!(mgr.take_due_recall(), None, "recall is off unless enabled");
        mgr.enable_recall(3);
        mgr.init_task("find the bug".to_string(), vec![]);
        assert_eq!(mgr.take_due_recall(), Some(("find the bug".to_string(), 3)));
        assert_eq!(mgr.take_due_recall(), None, "one recall per boundary");
        mgr.push_recalled(vec![("r1".into(), "[MEMORY record_id=r1 kind=project] a".into())]);
        mgr.push_recalled(vec![("r1".into(), "[MEMORY record_id=r1 kind=project] changed".into())]);
        assert_eq!(mgr.partitions.knowledge.len(), 1);
        mgr.renew();
        assert!(mgr.take_due_recall().is_some());
    }

    #[test]
    fn knowledge_before_the_first_send_goes_straight_into_system_knowledge() {
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_history(CoreMessage::user("goal"), 10);
        mgr.push_knowledge_entry(Some("memory:a".into()), CoreMessage::system("recall a"), 5, false);
        assert!(mgr.render().system_knowledge.contains("recall a"));
        assert!(!staged_text(&mgr).contains("recall a"));
    }

    #[test]
    fn knowledge_after_the_send_is_staged_and_keeps_the_cached_prefix() {
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_history(CoreMessage::user("goal ".repeat(200)), 400);
        let before = mgr.render();
        mgr.mark_prefix_sent();
        mgr.push_knowledge_entry(Some("skill:x".into()), CoreMessage::system("skill body"), 20, false);
        let after = mgr.render();
        assert_eq!(after.system_stable, before.system_stable);
        assert_eq!(after.system_knowledge, before.system_knowledge, "system[1] bytes unchanged");
        assert_eq!(after.turns.len(), before.turns.len());
        assert!(staged_text(&mgr).contains("skill body"));
        assert_eq!(mgr.cache_invalidated_tokens(), 0);

        // The next boundary promotes it into system[1].
        mgr.sweep_knowledge_at_boundary();
        assert!(mgr.render().system_knowledge.contains("skill body"));
        assert!(!staged_text(&mgr).contains("skill body"));
    }

    #[test]
    fn large_staged_knowledge_is_promoted_at_once_and_the_invalidation_is_counted() {
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("goal".to_string(), vec![]);
        mgr.push_history(CoreMessage::user("short"), 40);
        mgr.mark_prefix_sent();
        mgr.push_knowledge_entry(Some("ref:big".into()), CoreMessage::system("big reference"), 10, false);
        assert!(mgr.render().system_knowledge.contains("big reference"));
        assert_eq!(mgr.cache_invalidated_tokens(), 40);
        // The promotion rewrote system[1]; the next append before a send is free again.
        mgr.push_knowledge_entry(Some("ref:next".into()), CoreMessage::system("next"), 1, false);
        assert!(mgr.render().system_knowledge.contains("next"));
    }

    #[test]
    fn note_tool_actions_keys_on_name_and_args_so_legit_loops_dont_false_stop() {
        // Same tool, DIFFERENT args across turns = real progress (e.g. process item 1, 2, 3) —
        // must NOT trip the no-progress STOP backstop.
        let mut mgr = ContextManager::new(100_000);
        mgr.init_task("process items".to_string(), vec![]);
        mgr.note_tool_actions(&[("step".to_string(), "{\"n\":1}".to_string())]);
        mgr.note_tool_actions(&[("step".to_string(), "{\"n\":2}".to_string())]);
        mgr.note_tool_actions(&[("step".to_string(), "{\"n\":3}".to_string())]);
        assert_eq!(
            mgr.partitions.task_state.recent_actions,
            ["step({\"n\":1})", "step({\"n\":2})", "step({\"n\":3})"]
        );
        let txt = mgr
            .render()
            .state_turn
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_string();
        assert!(
            !txt.contains("STOP:"),
            "same-tool/diff-args loop must not trip STOP: {txt}"
        );

        // Genuine stall — same tool, SAME args repeated — DOES trip the STOP.
        let mut mgr2 = ContextManager::new(100_000);
        mgr2.init_task("g".to_string(), vec![]);
        for _ in 0..3 {
            mgr2.note_tool_actions(&[("document_read".to_string(), "{\"id\":\"x\"}".to_string())]);
        }
        let txt2 = mgr2
            .render()
            .state_turn
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_string();
        assert!(
            txt2.contains("STOP:"),
            "identical repeated call must trip STOP: {txt2}"
        );

        // Meta-tools are control plane, not task progress — filtered out entirely.
        let mut mgr3 = ContextManager::new(100_000);
        mgr3.init_task("g".to_string(), vec![]);
        mgr3.note_tool_actions(&[(
            "update_plan".to_string(),
            "{\"current_step\":1}".to_string(),
        )]);
        assert!(mgr3.partitions.task_state.recent_actions.is_empty());
    }

    #[test]
    fn manager_renew_advances_sprint_and_keeps_goal() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("test goal".to_string(), vec![]);
        mgr.partitions.system.push(CoreMessage::system("rules"), 10);
        for i in 0..10 {
            mgr.push_history(CoreMessage::user(format!("msg {i}")), 50);
        }
        mgr.renew();
        assert_eq!(mgr.partitions.task_state.goal, "test goal");
        assert_eq!(mgr.sprint, 1);
    }

    #[test]
    fn compress_only_touches_history() {
        let mut mgr = ContextManager::new(1_000);
        mgr.push_knowledge(CoreMessage::system("knowledge content"), 100);
        for _ in 0..30 {
            mgr.push_history(CoreMessage::user("history msg"), 50);
        }
        let knowledge_before = mgr.partitions.knowledge.token_count;
        let history_before = mgr.partitions.history.token_count();
        mgr.compress(PressureAction::AutoCompact);
        assert_eq!(mgr.partitions.knowledge.token_count, knowledge_before);
        assert!(mgr.partitions.history.token_count() < history_before);
    }

    #[test]
    fn init_task_sets_goal_and_criteria() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("analyse data".to_string(), vec!["criterion A".to_string()]);
        assert_eq!(mgr.partitions.task_state.goal, "analyse data");
        assert_eq!(mgr.partitions.task_state.criteria, ["criterion A"]);
    }

    #[test]
    fn update_task_applies_plan() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("g".to_string(), vec![]);
        mgr.update_task(TaskUpdate {
            plan: Some(vec!["step 1".to_string(), "step 2".to_string()]),
            current_step: Some(0),
            ..Default::default()
        });
        assert_eq!(mgr.partitions.task_state.plan.len(), 2);
        assert_eq!(mgr.partitions.task_state.current_step, Some(0));
    }

    #[test]
    fn task_state_survives_autocompact() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("survive compression".to_string(), vec![]);
        mgr.update_task(TaskUpdate {
            plan: Some(vec!["fetch data".to_string(), "analyse".to_string()]),
            ..Default::default()
        });
        for _ in 0..10 {
            mgr.push_history(CoreMessage::user("filler"), 50);
        }
        mgr.compress(PressureAction::AutoCompact);
        assert_eq!(mgr.partitions.task_state.goal, "survive compression");
        assert_eq!(mgr.partitions.task_state.plan.len(), 2);
    }

    #[test]
    fn render_includes_task_state_in_state_turn_not_system() {
        let mut mgr = ContextManager::new(10_000);
        mgr.init_task("find anomalies".to_string(), vec![]);
        let rc = mgr.render();
        assert!(
            !rc.system_text.contains("[TASK STATE]"),
            "task_state must not be in system_text"
        );
        // State turn is separated from the cacheable history (turns).
        let state = rc.state_turn.as_ref().expect("should have a state turn");
        assert!(
            state
                .content
                .as_text()
                .unwrap()
                .contains("[TASK STATE] goal: find anomalies")
        );
    }

    #[test]
    fn renewal_keeps_open_plan_steps_in_task_state() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("g".to_string(), vec![]);
        mgr.partitions.task_state.plan = vec![
            PlanStep {
                label: "done".to_string(),
                done: true,
            },
            PlanStep {
                label: "pending".to_string(),
                done: false,
            },
        ];
        mgr.renew();
        assert_eq!(mgr.partitions.task_state.open_steps(), vec!["pending"]);
    }

    // ── W1-1 完成态 regression gates (Step 0). RED until the planner/pure-executor rewrite. ──

    #[test]
    fn auto_compact_entry_logs_auto_compact_action() {
        // C regression gate: `force_compress` is the auto-compact entry point; the summary the
        // provider eventually sees (rendered from `compression_log`) must carry the **auto_compact**
        // label. The broken W1 cascade ran `compress(AutoCompact, target=0)`, so `CollapseCompactor`
        // drained the whole history first and logged `context_collapse`, then `AutoCompactor` had
        // nothing to archive — the event was labeled `auto_compact` but the log/render showed
        // `context_collapse`. The pure-executor model logs with the op's own label, restoring the
        // op-label == log-label contract end users observe (node K04/K09).
        let mut mgr = ContextManager::new(1_000);
        for i in 0..40 {
            mgr.push_history(
                CoreMessage::user(format!("turn {i}: {}", "ctx ".repeat(40))),
                200,
            );
        }
        let (saved, summary, _, _) = mgr.force_compress();
        assert!(saved > 0, "force_compress should compact a large history");
        assert!(
            summary.is_some(),
            "auto-compact summarizes the archived turns"
        );
        let actions: Vec<&str> = mgr
            .partitions
            .task_state
            .compression_log
            .iter()
            .map(|e| e.action.as_str())
            .collect();
        assert!(
            actions.last() == Some(&"auto_compact"),
            "auto-compact entry must log an auto_compact action; got {actions:?}"
        );
    }

    #[test]
    fn skill_tool_schema_empty_when_no_skills() {
        let mgr = ContextManager::new(10_000);
        assert!(mgr.skill_tool_schema().is_none());
    }

    #[test]
    fn skill_tool_schema_present_when_registered() {
        let mut mgr = ContextManager::new(10_000);
        mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
        assert!(
            mgr.skill_tool_schema()
                .unwrap()
                .description
                .contains("debug")
        );
    }

    #[test]
    fn available_skills_are_reflected_in_capability_manifest() {
        let mut mgr = ContextManager::new(1_000);
        mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
        let inventory = mgr.capability_inventory();
        assert!(inventory.contains("debug"));
        assert!(inventory.contains("Debug helper"));
    }

    #[test]
    fn toggled_meta_tools_are_reflected_in_capability_manifest() {
        let mut mgr = ContextManager::new(1_000);
        mgr.set_memory_enabled(true);
        assert!(mgr.capability_inventory().contains(MEMORY_TOOL_NAME));
        mgr.set_memory_enabled(false);
        assert!(!mgr.capability_inventory().contains(MEMORY_TOOL_NAME));
    }

    #[test]
    fn propose_memory_schema_is_strict_and_capability_gated() {
        let mut mgr = ContextManager::new(1_000);
        assert!(mgr.propose_memory_tool_schema().is_none());
        mgr.set_propose_memory_enabled(true);
        let schema = mgr.propose_memory_tool_schema().expect("proposal capability");
        assert_eq!(schema.parameters["additionalProperties"], false);
        assert_eq!(schema.parameters["required"], serde_json::json!(["name", "kind", "content"]));
        assert!(schema.parameters["properties"].get("record_id").is_none());
    }

    #[test]
    fn meta_tool_schemas_are_sorted() {
        let mut mgr = ContextManager::new(1_000);
        mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
        mgr.set_memory_enabled(true);
        mgr.set_propose_memory_enabled(true);
        mgr.set_knowledge_enabled(true);
        let names = mgr
            .meta_tool_schemas()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, ["knowledge", "memory", "propose_memory", "skill"]);
    }

    #[test]
    fn b1_active_skill_state_and_tool_filter() {
        let mut mgr = ContextManager::new(1_000);
        let mut debug = SkillMetadata::new("debug", "Debug helper");
        debug.allowed_tools = vec![CompactString::new("read"), CompactString::new("grep")];
        let mut review = SkillMetadata::new("review", "Reviewer");
        review.allowed_tools = vec![CompactString::new("git_diff")];
        let plain = SkillMetadata::new("plain", "No tools declared"); // empty allowed_tools
        mgr.set_available_skills(vec![debug, review, plain]);

        // No active skill ⇒ no narrowing.
        assert!(mgr.active_skill_tool_filter().is_none());

        // Activating returns the epoch-boundary changed flag.
        assert!(mgr.activate_skill("debug"));
        assert!(!mgr.activate_skill("debug")); // already active ⇒ no change

        // One restricted skill ⇒ narrow to its tools.
        let f = mgr.active_skill_tool_filter().unwrap();
        assert_eq!(f.len(), 2);
        assert!(f.contains(&CompactString::new("read")) && f.contains(&CompactString::new("grep")));

        // Second restricted skill ⇒ union (D1).
        mgr.activate_skill("review");
        let f = mgr.active_skill_tool_filter().unwrap();
        assert_eq!(f.len(), 3);
        assert!(f.contains(&CompactString::new("git_diff")));

        // An active skill with NO declared tools ⇒ unbounded ⇒ do not narrow (D3, errs-open).
        mgr.activate_skill("plain");
        assert!(mgr.active_skill_tool_filter().is_none());
    }

    #[test]
    fn active_skill_capability_grants_follow_activation_deactivation_and_lease_expiry() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, ConstraintSet, Principal, ResourceSelector,
        };

        let grant = Capability {
            id: CapabilityId("read-src".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: false,
            issuer: Principal("root".into()),
        };
        let mut review = SkillMetadata::new("review", "Review source files");
        review.capability_grants = vec![grant.clone()];

        let mut mgr = ContextManager::new(1_000);
        mgr.set_available_skills(vec![review]);
        assert!(mgr.active_skill_capabilities().is_empty());

        mgr.activate_skill("review");
        assert_eq!(mgr.active_skill_capabilities(), vec![grant.clone()]);

        mgr.deactivate_skill("review");
        assert!(mgr.active_skill_capabilities().is_empty());

        mgr.activate_skill_leased("review", Some(3));
        mgr.sweep_expired_skill_leases(2);
        assert_eq!(mgr.active_skill_capabilities(), vec![grant]);

        mgr.sweep_expired_skill_leases(3);
        assert!(mgr.active_skill_capabilities().is_empty());
    }

    #[test]
    fn update_collapse_mode_collapses_old_tool_results_under_pressure() {
        let mut mgr = ContextManager::new(1_000);
        for i in 0..10 {
            let m = CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: format!("c{i}").into(),
                output: "x".repeat(40),
                is_error: false,
                durable_content: None,
            }]);
            mgr.push_history(m, 40);
        }
        // Drive rho past collapse_threshold deterministically via observed prompt tokens.
        mgr.set_observed_prompt_tokens(950); // 950 / 1000 = 0.95 >= 0.90
        assert!(mgr.rho() >= mgr.config.collapse_threshold);

        mgr.recompute_handle_residency();
        // Oldest is collapsed; the most recent configured tool-result handles stay resident.
        assert_eq!(
            mgr.handles.residency_for_source("c0"),
            Some(&Residency::Collapsed)
        );
        assert_eq!(
            mgr.handles.residency_for_source("c9"),
            Some(&Residency::Resident)
        );

        // P0-C — monotonic within a generation: once collapsed, dropping pressure does NOT
        // un-collapse (un-collapsing would re-bill the body and churn the cache prefix).
        mgr.set_observed_prompt_tokens(100); // 0.10 < 0.90
        mgr.recompute_handle_residency();
        assert_eq!(
            mgr.handles.residency_for_source("c0"),
            Some(&Residency::Collapsed),
            "collapse is sticky until a compaction boundary"
        );

        // Only a generation reset (compaction/renewal) un-collapses.
        mgr.reset_collapse_generation();
        assert_eq!(
            mgr.handles.residency_for_source("c0"),
            Some(&Residency::Resident)
        );
    }

    #[test]
    fn a_collapsed_tool_result_is_an_item_rendered_as_its_handle() {
        use crate::context::item::Fidelity;
        let mut mgr = ContextManager::new(1_000);
        for i in 0..10 {
            mgr.push_history(tool_result_msg(&format!("c{i}"), &"x".repeat(40)), 40);
        }
        let first = mgr.partitions.history.tag(0).id;
        assert_eq!(mgr.handles.all()[0].item, first, "a handle addresses its item");
        assert_eq!(mgr.history_fidelity(0), Fidelity::Full);

        mgr.set_observed_prompt_tokens(950);
        mgr.recompute_handle_residency();
        assert_eq!(mgr.history_fidelity(0), Fidelity::Handle);
        assert_eq!(mgr.history_fidelity(9), Fidelity::Full);
        let turns = mgr.render().turns;
        let stored = mgr.partitions.history.messages();
        let json = |message: &CoreMessage| serde_json::to_string(message).unwrap();
        let rendered = |call: &str| {
            turns.iter().map(json).find(|turn| turn.contains(&format!("\"call_id\":\"{call}\"")))
        };
        assert_ne!(rendered("c0"), Some(json(&stored[0])), "a collapsed item renders as its handle");
        assert_eq!(rendered("c9"), Some(json(&stored[9])), "a full item renders as stored");

        // The handle stays the item's residency when the table is restored without item ids.
        for handle in mgr.handles.all_mut() {
            handle.item = 0;
        }
        mgr.backfill_handle_items();
        assert_eq!(mgr.history_fidelity(0), Fidelity::Handle);

        mgr.reset_collapse_generation();
        assert_eq!(mgr.history_fidelity(0), Fidelity::Full);
    }

    fn delegating(clearing: bool, compaction: bool) -> ContextManager {
        ContextManager::with_config(
            1_000,
            ContextConfig {
                delegate_tool_result_clearing: clearing,
                delegate_compaction: compaction,
                ..ContextConfig::default()
            },
            ContextTokenEngine::char_approx(),
        )
    }

    /// A tier the provider performs natively is skipped, never escalated past: with compaction
    /// delegated but clearing kernel-owned, the heavy tiers fall back to micro-compact.
    #[test]
    fn delegated_tiers_are_filtered_out_of_the_pressure_recommendation() {
        use PressureAction::*;
        let cases = [
            ((true, false), [None, None, None, ContextCollapse, AutoCompact]),
            ((false, true), [None, SnipCompact, MicroCompact, MicroCompact, MicroCompact]),
            ((true, true), [None, None, None, None, None]),
            ((false, false), [None, SnipCompact, MicroCompact, ContextCollapse, AutoCompact]),
        ];
        for ((clearing, compaction), expected) in cases {
            let mgr = delegating(clearing, compaction);
            let got = [None, SnipCompact, MicroCompact, ContextCollapse, AutoCompact]
                .map(|action| mgr.kernel_owned_tier(action));
            assert_eq!(got, expected, "clearing={clearing} compaction={compaction}");
        }
        let mut mgr = delegating(true, true);
        mgr.push_history(CoreMessage::user("large history"), 975);
        assert_eq!(mgr.should_compress(), None);
        assert!(!mgr.should_renew(), "below the renewal threshold nothing happens");
        // Renewal is never delegated: it reads the full local history whatever the provider does.
        mgr.push_history(CoreMessage::user("more"), 100);
        assert!(mgr.should_renew());
    }

    #[test]
    fn delegated_clearing_leaves_tool_results_to_the_provider() {
        let mut mgr = delegating(true, false);
        mgr.record_activity(0);
        for i in 0..10 {
            let m = CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: format!("c{i}").into(),
                output: "x".repeat(40),
                is_error: false,
                durable_content: None,
            }]);
            mgr.push_history(m, 40);
        }
        mgr.set_observed_prompt_tokens(950);
        mgr.recompute_handle_residency();
        assert_eq!(mgr.handles.residency_for_source("c0"), Some(&Residency::Resident));
        assert!(!mgr.should_time_decay_compact(24 * 60 * 60_000));
    }

    #[test]
    fn frozen_prefix_len_anchors_at_compaction_and_holds_across_appends() {
        let mut mgr = ContextManager::new(1_000);
        // Pre-compaction: no frozen region yet → providers use the rolling-pair fallback.
        for i in 0..30 {
            mgr.push_history(
                CoreMessage::user(format!("turn {i}: {}", "ctx ".repeat(30))),
                150,
            );
        }
        assert!(
            mgr.render().frozen_prefix_len.is_none(),
            "no frozen region before any compaction"
        );

        let (saved, _, archived, _) = mgr.compress(PressureAction::AutoCompact);
        assert!(saved > 0 && !archived.is_empty(), "expected archival");

        // Immediately after compaction the hot tail is empty → deep would coincide with the tail → None.
        assert!(
            mgr.render().frozen_prefix_len.is_none(),
            "deep == tail right after compaction"
        );

        // As turns are appended, the deep boundary holds fixed while the tail grows.
        mgr.push_history(CoreMessage::user("new 1"), 5);
        let f1 = mgr
            .render()
            .frozen_prefix_len
            .expect("frozen region exists once the tail grows");
        mgr.push_history(CoreMessage::assistant("reply 1"), 5);
        mgr.push_history(CoreMessage::user("new 2"), 5);
        let rc = mgr.render();
        let f2 = rc.frozen_prefix_len.expect("frozen region holds");
        assert_eq!(
            f1, f2,
            "the deep boundary is fixed between compactions; only the tail grows"
        );
        assert!(
            f2 < rc.turns.len(),
            "deep boundary is distinct from the rolling tail"
        );
    }

    #[test]
    fn frozen_boundary_holds_through_a_prefix_safe_compaction() {
        // P2-D × P1-E: the boundary re-anchors on a prefix-breaking compaction (cache_at = Some) but
        // is preserved through a prefix-safe one (cache_at = None) — the deep cache survives.
        let mut mgr = ContextManager::new(10_000);
        for i in 0..5 {
            mgr.push_history(CoreMessage::user(format!("m{i}")), 5);
        }
        mgr.frozen_history_len = 3; // pretend a prior compaction anchored the deep cache here

        // A no-op / prefix-safe compaction (PressureAction::None ⇒ cache_at None) must NOT move the
        // anchor — the cached [0..3] prefix is untouched, so the deep breakpoint stays put.
        let (_, _, _, cache_at) = mgr.compress(PressureAction::None);
        assert!(cache_at.is_none(), "no-op compaction is prefix-safe");
        assert_eq!(
            mgr.frozen_history_len, 3,
            "prefix-safe compaction preserves the deep-cache anchor"
        );
    }

    #[test]
    fn collapse_generation_resets_on_autocompact() {
        let mut mgr = ContextManager::new(1_000);
        // Many oversized tool results: some will be archived by AutoCompact, the survivors
        // should come back Resident (fresh generation), not stay stuck Collapsed.
        for i in 0..20 {
            mgr.push_history(tool_result_msg(&format!("c{i}"), &"x".repeat(120)), 60);
        }
        mgr.set_observed_prompt_tokens(980); // force collapse of the older results
        mgr.recompute_handle_residency();
        assert_eq!(
            mgr.handles.residency_for_source("c0"),
            Some(&Residency::Collapsed)
        );

        let (saved, _, archived, _) = mgr.compress(PressureAction::AutoCompact);
        assert!(saved > 0 && !archived.is_empty(), "expected archival");

        // Every surviving tool-result handle is Resident again — the compaction boundary
        // rewrote the prefix, so the next pressure cycle re-decides from scratch.
        for h in mgr.handles.all() {
            if matches!(h.kind, HandleKind::ToolResult) {
                assert_eq!(
                    h.residency,
                    Residency::Resident,
                    "generation reset un-collapses survivors"
                );
            }
        }
    }

    #[test]
    fn push_history_indexes_tool_results_as_resident_handles() {
        let mut mgr = ContextManager::new(10_000);
        let msg = CoreMessage::tool(vec![ContentPart::ToolResult {
            call_id: "call_1".into(),
            output: "the tool output".to_string(),
            is_error: false,
            durable_content: None,
        }]);
        mgr.push_history(msg, 20);
        // A handle was indexed, anchored to the call_id, resident by default.
        assert_eq!(mgr.handles.all().len(), 1);
        assert_eq!(
            mgr.handles.residency_for_source("call_1"),
            Some(&Residency::Resident)
        );
        // A plain text turn allocates no handle.
        mgr.push_history(CoreMessage::user("hello"), 5);
        assert_eq!(mgr.handles.all().len(), 1);
    }

    // ── W1-3: handle-table GC (prune orphaned handles + bounded recompute) ──

    fn tool_result_msg(call_id: &str, output: &str) -> CoreMessage {
        CoreMessage::tool(vec![ContentPart::ToolResult {
            call_id: call_id.into(),
            output: output.to_string(),
            is_error: false,
            durable_content: None,
        }])
    }

    #[test]
    fn prune_orphaned_handles_drops_handles_whose_message_left_history() {
        let mut mgr = ContextManager::new(10_000);
        mgr.push_history(tool_result_msg("c0", "out 0"), 20);
        mgr.push_history(tool_result_msg("c1", "out 1"), 20);
        assert_eq!(mgr.handles.all().len(), 2);

        // Simulate compaction archiving the oldest tool-result message out of history.
        mgr.partitions.history.remove(0);
        mgr.prune_orphaned_handles();

        // The handle for the evicted message is gone; the live one is retained.
        assert_eq!(mgr.handles.all().len(), 1);
        assert!(mgr.handles.residency_for_source("c0").is_none());
        assert_eq!(
            mgr.handles.residency_for_source("c1"),
            Some(&Residency::Resident)
        );
    }

    #[test]
    fn autocompact_prunes_handles_for_archived_tool_results() {
        let mut mgr = ContextManager::new(1_000);
        // Enough oversized tool results to force AutoCompact to archive some.
        for i in 0..30 {
            mgr.push_history(tool_result_msg(&format!("c{i}"), &"x".repeat(200)), 80);
        }
        assert_eq!(mgr.handles.all().len(), 30);

        let (saved, _, archived, _) = mgr.compress(PressureAction::AutoCompact);
        assert!(saved > 0 && !archived.is_empty(), "expected archival");

        // After compaction the table tracks only the tool results still in working history —
        // not the whole session. (No handle outlives its backing message.)
        let live_tool_results = mgr
            .partitions
            .history
            .messages()
            .iter()
            .filter(|m| {
                matches!(&m.content, Content::Parts(p)
                if p.iter().any(|x| matches!(x, ContentPart::ToolResult { .. })))
            })
            .count();
        assert_eq!(mgr.handles.all().len(), live_tool_results);
        assert!(
            mgr.handles.all().len() < 30,
            "table must shrink with archival"
        );
    }

    #[test]
    fn renew_prunes_handles_for_dropped_history() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("g".to_string(), vec![]);
        for i in 0..20 {
            mgr.push_history(tool_result_msg(&format!("c{i}"), "data"), 60);
        }
        mgr.renew();
        // Every retained handle must still be anchored to a message present in the renewed history.
        for h in mgr.handles.all() {
            if let Some(src) = h.source.as_ref() {
                assert!(
                    mgr.handles.residency_for_source(src).is_some(),
                    "no dangling handle survives renewal"
                );
            }
        }
        assert!(mgr.handles.all().len() <= 20);
    }

    // ── K2: knowledge budget ─────────────────────────────────────────────────

    #[test]
    fn knowledge_budget_uses_stable_order_for_equal_value_and_warns_once() {
        // max_tokens 100 × default ratio 0.25 ⇒ budget 25. Four 10-token entries (40 used):
        // two evictable, one pinned, one skill pin.
        let mut mgr = ContextManager::new(100);
        mgr.push_knowledge(CoreMessage::system("oldest unkeyed"), 10);
        mgr.push_knowledge_entry(Some("a".into()), CoreMessage::system("keyed"), 10, false);
        mgr.push_knowledge_entry(Some("p".into()), CoreMessage::system("pinned"), 10, true);
        mgr.push_knowledge_entry(
            Some("skill:x".into()),
            CoreMessage::system("skill"),
            10,
            false,
        );

        let warn = mgr.enforce_knowledge_budget();
        assert_eq!(warn, Some((40, 25)));
        // Equal scores retain the deterministic insertion-order tie-break: unkeyed then "a".
        let e = &mgr.partitions.knowledge.entries;
        assert!(e[0].is_leaving());
        assert!(e[1].is_leaving());
        assert!(!e[2].is_leaving(), "pinned exempt");
        assert!(!e[3].is_leaving(), "skill pin exempt");

        // Warn-once per generation; marking stays idempotent.
        assert_eq!(mgr.enforce_knowledge_budget(), None);

        // The boundary sweep drops the marked entries and re-arms the warning.
        let sweep = mgr.partitions.knowledge.sweep_at_boundary();
        assert_eq!(sweep.tokens_freed, 20);
        assert_eq!(mgr.partitions.knowledge.token_count, 20);
        // Back under budget ⇒ no further warning even though it re-armed.
        assert_eq!(mgr.enforce_knowledge_budget(), None);
    }

    #[test]
    fn knowledge_budget_warning_stands_when_only_exempt_weight_remains() {
        let mut mgr = ContextManager::new(100);
        mgr.push_knowledge_entry(
            Some("p".into()),
            CoreMessage::system("pinned heavy"),
            30,
            true,
        );
        mgr.push_knowledge_entry(
            Some("skill:x".into()),
            CoreMessage::system("skill heavy"),
            30,
            false,
        );

        // Over budget (60 > 25) but nothing evictable — warning fires, nothing marked.
        assert_eq!(mgr.enforce_knowledge_budget(), Some((60, 25)));
        assert!(
            mgr.partitions
                .knowledge
                .entries
                .iter()
                .all(|e| !e.is_leaving())
        );
    }

    #[test]
    fn knowledge_budget_retains_old_referenced_entry_over_new_irrelevant_entry() {
        let mut mgr = ContextManager::new(100);
        mgr.push_knowledge_entry(
            Some("project:orchid".into()),
            CoreMessage::system("ORCHID uses the Atlas storage engine"),
            10,
            false,
        );
        // A committed history input is the deterministic usage fact.
        mgr.push_history(CoreMessage::user("For project:orchid keep using Atlas"), 5);
        mgr.push_knowledge_entry(
            Some("project:new".into()),
            CoreMessage::system("unrelated fresh material"),
            10,
            false,
        );
        mgr.push_knowledge_entry(
            Some("project:other".into()),
            CoreMessage::system("another unused reference"),
            10,
            false,
        );

        assert_eq!(mgr.enforce_knowledge_budget(), Some((30, 25)));
        let entries = &mgr.partitions.knowledge.entries;
        assert!(
            !entries[0].is_leaving(),
            "a real reference must raise retention"
        );
        assert!(
            entries[1].is_leaving(),
            "lowest-value entry evicts first"
        );
        assert!(
            !entries[2].is_leaving(),
            "one eviction is enough to fit"
        );
    }

    #[test]
    fn knowledge_budget_ratio_zero_disables() {
        let mut mgr = ContextManager::new(100);
        mgr.config.knowledge_budget_ratio = 0.0;
        mgr.push_knowledge(CoreMessage::system("huge"), 90);
        assert_eq!(mgr.enforce_knowledge_budget(), None);
        assert!(!mgr.partitions.knowledge.entries[0].is_leaving());
    }

    #[test]
    fn provider_and_output_reservations_reduce_the_hard_input_budget() {
        use crate::context::config::PromptBudgetConfig;

        let mut mgr = ContextManager::new(100);
        mgr.set_prompt_budget(PromptBudgetConfig {
            prompt_overhead_tokens: 20,
            output_reserve_tokens: 20,
            safety_margin_tokens: 10,
        });
        // spc_011-C-01: `render()`'s fixed-context accounting always recounts the system
        // partition via `engine.count()` (never trusts a stored `token_count` — see
        // `renderer.rs::render_projected`'s `system_tokens` line), so this text must overflow
        // the 50-token budget under whichever engine is configured. A single repeated character
        // (the previous `"x".repeat(240)`) reliably hit 60 under char/4 math, but real BPE
        // merges long identical-byte runs into a handful of tokens (~30, well under budget) —
        // that was calibrated to the old default, not to the overflow behavior under test.
        mgr.partitions.system.push(
            CoreMessage::system(
                "System policy directive number seven requires strict adherence. ".repeat(10),
            ),
            60,
        );

        let rendered = mgr.render();
        let overflow = rendered
            .budget_overflow
            .expect("fixed context exceeds input allowance");
        assert_eq!(overflow.max_tokens, 50);
        assert!(overflow.required_tokens > overflow.max_tokens);
    }
}
