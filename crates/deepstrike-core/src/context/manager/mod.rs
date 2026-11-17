use super::cache_ledger::BreakCause;
use super::compression::CompressionPipeline;
use super::config::{ContextConfig, PromptBudgetConfig};
use super::item::{
    Authority, ExternalEdge, ExternalItem, HISTORY_ITEM_DETAIL_CAP, HistoryItemRecord, ItemEdge,
    ItemEnd, ItemId, ItemOrigin, ItemRecord, ItemTag, LeaseEvent, OriginTally, Relation,
};
use super::partitions::{ContextPartitions, SignalItem};
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
/// work. The repeat fuse leaves them out of a turn's signature.
const META_TOOL_NAMES: &[&str] = &[
    "pace",
    "update_plan",
    super::notes::NOTE_TOOL_NAME,
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
    super::notes::NOTE_TOOL_NAME,
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
    /// Plan task 8.0 · the latest prefix breaks (what `cache_loss_tokens` sums), and how many the
    /// cap dropped.
    pub prefix_breaks: Vec<crate::context::cache_ledger::PrefixBreak>,
    pub prefix_breaks_omitted: u32,
    /// State-turn tokens of every request a later one followed: uncached by design, not a break.
    pub cache_tail_tokens: u64,
    /// Plan task 9.1 · notes written (rewrites count again) and notes held now.
    pub notes_written: u32,
    pub notes_active: u32,
    /// Plan task 1.7 · tokens of kernel-written text the model was shown (see `voice`).
    pub kernel_text_tokens: u64,
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
    /// Plan task 8.0 · every prefix break between consecutive provider requests, attributed.
    pub(crate) cache_ledger: super::cache_ledger::CacheLedger,
    /// Long-term memory recall (plan task 2.1): `Some(top_k)` when the operation can read memory.
    /// The kernel recalls at the start of a run and after every renewal.
    pub(crate) recall_top_k: Option<u32>,
    /// A recall is owed before the next provider call.
    pub(crate) recall_due: bool,
    /// Plan task 3.1 · dependency outputs by agent id, read in full with
    /// `read_result(call_id="dep:<agent_id>")`. Only previews are in the rendered context.
    pub dependency_bodies: std::collections::BTreeMap<String, String>,
    /// `read_result` has been offered in this run, so it stays offered. Withdrawing it when the
    /// last evicted result came back (a page-in, a compaction boundary) left the model calling a
    /// tool its own history shows it using — and broke the cache prefix besides.
    pub read_result_offered: bool,
    /// Plan task 1.7 · tokens of text the kernel itself put in front of the model: signals, once
    /// the request that carried them committed, and the results it answered calls with.
    pub kernel_text_tokens: u64,
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

/// Plan task 8.0 · a message's content as it reaches a provider, for the cache ledger's
/// fingerprint: a tool result's `durable_content` is kernel-side detail (a restored message carries
/// it, a live one may not) and never goes on the wire, so it must not count as a change.
fn wire_content(content: &Content) -> std::borrow::Cow<'_, Content> {
    let Content::Parts(parts) = content else {
        return std::borrow::Cow::Borrowed(content);
    };
    if !parts
        .iter()
        .any(|part| matches!(part, ContentPart::ToolResult { durable_content: Some(_), .. }))
    {
        return std::borrow::Cow::Borrowed(content);
    }
    let parts = parts
        .iter()
        .map(|part| match part {
            ContentPart::ToolResult {
                call_id,
                output,
                is_error,
                ..
            } => ContentPart::ToolResult {
                call_id: call_id.clone(),
                output: output.clone(),
                is_error: *is_error,
                durable_content: None,
            },
            other => other.clone(),
        })
        .collect();
    std::borrow::Cow::Owned(Content::Parts(parts))
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
            read_result_offered: false,
            kernel_text_tokens: 0,
            cache_invalidated_tokens: 0,
            cache_ledger: super::cache_ledger::CacheLedger::default(),
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

    /// Install a configuration and rebuild every component derived from it. The wire's resolved
    /// context policy reaches the kernel this way (`build_engine`); it has been validated there.
    pub fn apply_config(&mut self, config: ContextConfig) {
        self.config = config;
        self.compression = CompressionPipeline::new(&self.config);
        self.pressure = PressureMonitor::new(self.max_tokens, self.config.clone());
        self.renewal = RenewalPolicy::from_config(&self.config);
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
}

mod handles;
mod pressure;
mod items;
mod cache;
mod report;
mod knowledge;
mod capabilities;

#[cfg(test)]
mod tests;
