use super::compression::CompressionPipeline;
use super::config::{ContextConfig, PromptBudgetConfig};
use super::item::{ItemEnd, ItemId, ItemOrigin, ItemRecord, ItemTag, OriginTally};
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
    /// Plan task 1.1b · the last item id handed to a history message.
    pub(crate) last_item_id: ItemId,
    /// History items admitted and read back, by origin (plan task 4.1).
    pub(crate) history_tally: std::collections::BTreeMap<ItemOrigin, OriginTally>,
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
            history_tally: std::collections::BTreeMap::new(),
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
                || h.source.as_ref().is_none_or(|s| live.contains(s))
        });
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
        self.handles.insert(Handle {
            id,
            kind,
            residency,
            tokens,
            source: Some(source.into()),
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
        observed.max(self.pressure.pressure(&self.partitions, &self.engine, None))
    }

    pub fn set_observed_prompt_tokens(&mut self, tokens: u32) {
        self.last_observed_prompt_tokens = Some(tokens);
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
        self.partitions = self
            .renewal
            .renew(&self.partitions, self.max_tokens, &self.engine);
        self.sprint += 1;
        self.last_observed_prompt_tokens = None;
        // The renewed history dropped earlier recalls with the rest; recall again for the new sprint.
        self.recall_due = self.recall_top_k.is_some();
        // History was rebuilt wholesale — drop handles anchored to messages it no longer carries,
        // and start a fresh collapse generation (P0-C) since the whole prefix changed.
        self.prune_orphaned_handles();
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
        super::renderer::render_projected(
            &self.partitions,
            self.available_input_tokens(),
            &self.engine,
            self.config.preserve_recent_units,
            &self.handles,
            self.frozen_history_len,
            self.config.collapse_assistant_narration,
        )
    }

    // ── History / Knowledge ───────────────────────────────────────────────────

    pub fn push_history(&mut self, msg: CoreMessage, tokens: u32) {
        self.knowledge_reference_step = self.knowledge_reference_step.saturating_add(1);
        self.partitions
            .knowledge
            .observe_references(&msg, self.knowledge_reference_step);
        // P3 (3a): index each tool result entering working context as a handle, anchored to its
        // call_id. Pure bookkeeping — render/compression still read `partitions` until 3b. The
        // handle's residency later drives read-time projection without mutating the message.
        if let Content::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolResult {
                    call_id, output, ..
                } = part
                {
                    let id = self.alloc_handle_id();
                    let tok = self.engine.count(output).max(1);
                    self.handles.insert(Handle::resident_for(
                        id,
                        HandleKind::ToolResult,
                        tok,
                        call_id.clone(),
                    ));
                }
            }
        }
        self.last_item_id += 1;
        let tag = ItemTag::new(
            self.last_item_id,
            ItemOrigin::of_message(&msg),
            self.partitions.knowledge.turn,
        );
        let tally = self.tally(tag.origin);
        tally.items += 1;
        tally.tokens += u64::from(tokens);
        self.partitions.history.push_tagged(msg, tokens, tag);
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
        self.partitions.knowledge.expire_leases();
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
        LifecycleMetrics {
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
        let knowledge = &mut self.partitions.knowledge;
        if !self.prefix_sent || knowledge.entries.len() == entries_before {
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
        let mut candidates = self
            .partitions
            .knowledge
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                !entry.is_leaving()
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
        self.partitions.knowledge.remove(&format!("skill:{name}"));
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
