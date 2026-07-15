use super::compression::CompressionPipeline;
use super::config::{ContextConfig, PromptBudgetConfig};
use super::partitions::ContextPartitions;
use super::policy::ContextPolicyV1;
use super::pressure::{PressureAction, PressureMonitor};
use super::renderer::RenderedContext;
use super::renewal::RenewalPolicy;
use super::skill_catalog::SkillCatalog;
use super::task_state::{TaskState, TaskUpdate};
use super::token_engine::ContextTokenEngine;
use crate::mm::handle::{Handle, HandleId, HandleKind, HandleTable, Residency};
use crate::types::capability::{CapabilityKind, CapabilityManifest};
use crate::types::message::{Content, ContentPart, Message, ToolSchema};
use crate::types::skill::SkillMetadata;
use compact_str::CompactString;

pub const MEMORY_TOOL_NAME: &str = "memory";
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

/// Internal context engine backing [`crate::runtime::KernelRuntime`].
///
/// Exposed for in-crate use and tests; external callers should drive the kernel
/// through `KernelRuntime` rather than this type directly.
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
    /// today). Residency transitions on these handles drive read-time projection (Layer 4) and
    /// spool (Layer 1) — the original messages in `partitions` are never mutated by projection.
    pub handles: HandleTable,
    /// Monotonic allocator for [`HandleId`]s.
    next_handle_id: HandleId,

    /// P1-E: history length (message count) as of the last compaction/renewal. Messages below this
    /// index are the **frozen prefix** — byte-stable until the next compaction — so the renderer can
    /// hand providers a `frozen_prefix_len` for a long-lived deep cache breakpoint. 0 before any
    /// compaction (no frozen region yet). Not snapshotted: on resume it resets to 0 and rebuilds at
    /// the next compaction (graceful — only the deep-cache durability lapses, never correctness).
    frozen_history_len: usize,

    /// K1: boundary-sweep results awaiting drain into `KnowledgeSwept` observations. Not
    /// snapshotted (observation-only bookkeeping, same class as `frozen_history_len`).
    pending_knowledge_sweeps: Vec<crate::context::partitions::KnowledgeSweep>,

    /// K2: whether the budget warning already fired this cache generation (warn-once; reset by
    /// the boundary sweep). Not snapshotted — a resume re-warns at most once, harmless.
    knowledge_budget_warned: bool,
    /// Monotonic, input-derived clock for knowledge-reference recency.
    knowledge_reference_step: u64,
}

impl ContextManager {
    pub fn new(max_tokens: u32) -> Self {
        Self::with_config(
            max_tokens,
            ContextConfig::default(),
            ContextTokenEngine::char_approx(),
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
        }
    }

    /// Atomically install the stable replay policy and rebuild every component derived from it.
    pub fn apply_context_policy(&mut self, policy: &ContextPolicyV1) {
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
        // Monotonic: below the threshold we never *un*-collapse, so there is nothing to do.
        if self.rho() < self.config.collapse_threshold {
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
            // been spooled or paged out, and never reverse an existing collapse mid-generation.
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
    pub fn prune_orphaned_handles(&mut self) {
        let live: std::collections::HashSet<CompactString> = self
            .partitions
            .history
            .messages
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
        self.handles
            .retain(|h| h.source.as_ref().is_none_or(|s| live.contains(s)));
    }

    /// Mark the handle anchored to `call_id` as spooled to disk (Layer 1): the SDK persists the
    /// full output, working context keeps only the preview. Keeps the handle out of the
    /// Resident↔Collapsed projection cycle. No-op if no handle is anchored to `call_id`.
    pub fn mark_spooled(&mut self, call_id: &str, spool_ref: impl Into<String>) {
        let spool_ref = spool_ref.into();
        if let Some(handle) = self
            .handles
            .all_mut()
            .iter_mut()
            .find(|h| h.source.as_deref() == Some(call_id))
        {
            handle.residency = Residency::SpooledOut { r: spool_ref };
        }
    }

    // ── Pressure ──────────────────────────────────────────────────────────────

    /// **Raw** rho — full partition weight (or provider-observed tokens when available). This is the
    /// projection-decision rho: [`Self::recompute_handle_residency`] marks the Resident↔Collapsed set
    /// from *this* value, so it must NOT discount paged content (else collapse → rho drops →
    /// un-collapse would oscillate).
    pub fn rho(&self) -> f64 {
        self.pressure.pressure(
            &self.partitions,
            &self.engine,
            self.last_observed_prompt_tokens,
        )
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
        self.pressure.recommend(self.rho())
    }

    pub fn compress(
        &mut self,
        action: PressureAction,
    ) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
        self.compress_with_time(action, None)
    }

    pub fn compress_with_time(
        &mut self,
        action: PressureAction,
        now_ms: Option<u64>,
    ) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
        let target = self.config.target_tokens(self.max_tokens);
        self.compress_with_target(action, target, now_ms)
    }

    pub fn force_compress(&mut self) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
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
    ) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
        let result = self.compression.compress(
            &mut self.partitions,
            action,
            self.max_tokens,
            target_tokens,
            &self.engine,
        );
        if let Some(ts) = now_ms {
            self.last_compact_ms = Some(ts);
        }
        // Archived messages have left history — drop their now-orphaned handles (bounds the table).
        if !result.2.is_empty() {
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
        if result.3.is_some() {
            self.frozen_history_len = self.partitions.history.messages.len();
        }
        result
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
        self.partitions = self.renewal.renew(&self.partitions, self.max_tokens);
        self.sprint += 1;
        // History was rebuilt wholesale — drop handles anchored to messages it no longer carries,
        // and start a fresh collapse generation (P0-C) since the whole prefix changed.
        self.prune_orphaned_handles();
        self.reset_collapse_generation();
        // K1: renewal is a boundary — apply deferred knowledge upserts/removals now.
        self.sweep_knowledge_at_boundary();
        // P1-E: the renewed history is the new frozen base.
        self.frozen_history_len = self.partitions.history.messages.len();
    }

    // ── Render ────────────────────────────────────────────────────────────────

    pub fn set_prompt_budget(&mut self, prompt_budget: PromptBudgetConfig) {
        self.prompt_budget = prompt_budget;
    }

    pub fn available_input_tokens(&self) -> u32 {
        self.max_tokens
            .saturating_sub(self.prompt_budget.reserved_tokens())
    }

    pub fn render(&self) -> RenderedContext {
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

    pub fn push_history(&mut self, msg: Message, tokens: u32) {
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
        self.partitions.history.push(msg, tokens);
    }

    fn alloc_handle_id(&mut self) -> HandleId {
        let id = self.next_handle_id;
        self.next_handle_id = self.next_handle_id.wrapping_add(1);
        id
    }

    /// Push content into the Knowledge slot (memory retrievals, skill defs, artifacts).
    pub fn push_knowledge(&mut self, msg: Message, tokens: u32) {
        self.partitions.knowledge.push(msg, tokens);
    }

    /// K1: keyed knowledge push — fresh key appends immediately (cache-cheap direction), an
    /// existing key stages a boundary-deferred upsert. `pinned` entries are exempt from the
    /// K2 budget sweep.
    pub fn push_knowledge_entry(
        &mut self,
        key: Option<CompactString>,
        msg: Message,
        tokens: u32,
        pinned: bool,
    ) {
        self.partitions
            .knowledge
            .push_entry(key, msg, tokens, pinned);
    }

    /// K1: mark a keyed knowledge entry for removal at the next compaction/renewal boundary.
    /// Errs-open: unknown key is a no-op (returns false).
    pub fn remove_knowledge(&mut self, key: &str) -> bool {
        self.partitions.knowledge.remove(key)
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
            .filter(|e| e.evict_at_boundary)
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
                !entry.evict_at_boundary
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
            entry.evict_at_boundary = true;
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

    pub fn skill_tool_schema(&self) -> Option<ToolSchema> {
        self.skills.build_tool_schema()
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
    /// (spooled to disk / collapsed / paged out). Exposed DYNAMICALLY: only once at least one
    /// handle has actually left residency, so runs that never evict see an unchanged toolset
    /// (progressive disclosure; golden fixtures and cache prefixes stay byte-stable). Content is
    /// host-resolved (spool file / session log) — the kernel only advertises the capability.
    pub fn read_result_tool_schema(&self) -> Option<ToolSchema> {
        let any_evicted = self
            .handles
            .all()
            .iter()
            .any(|h| !h.residency.occupies_context());
        if !any_evicted {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(READ_RESULT_TOOL_NAME),
            description: "Re-read a tool result that was evicted from context (you see a \
                          placeholder like '[…tool result spooled…]' or a collapsed entry). \
                          Pass the tool call's call_id; use offset/max_bytes to page through \
                          large content."
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
            description:
                "Search your long-term memory for relevant past experiences and knowledge."
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
    use crate::types::message::Message;
    use crate::types::skill::SkillMetadata;

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
        mgr.partitions.system.push(Message::system("rules"), 10);
        for i in 0..10 {
            mgr.push_history(Message::user(format!("msg {i}")), 50);
        }
        mgr.renew();
        assert_eq!(mgr.partitions.task_state.goal, "test goal");
        assert_eq!(mgr.sprint, 1);
    }

    #[test]
    fn compress_only_touches_history() {
        let mut mgr = ContextManager::new(1_000);
        mgr.push_knowledge(Message::system("knowledge content"), 100);
        for _ in 0..30 {
            mgr.push_history(Message::user("history msg"), 50);
        }
        let knowledge_before = mgr.partitions.knowledge.token_count;
        let history_before = mgr.partitions.history.token_count;
        mgr.compress(PressureAction::AutoCompact);
        assert_eq!(mgr.partitions.knowledge.token_count, knowledge_before);
        assert!(mgr.partitions.history.token_count < history_before);
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
            mgr.push_history(Message::user("filler"), 50);
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
                Message::user(format!("turn {i}: {}", "ctx ".repeat(40))),
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
    fn meta_tool_schemas_are_sorted() {
        let mut mgr = ContextManager::new(1_000);
        mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
        mgr.set_memory_enabled(true);
        mgr.set_knowledge_enabled(true);
        let names = mgr
            .meta_tool_schemas()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, ["knowledge", "memory", "skill"]);
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
    fn update_collapse_mode_collapses_old_tool_results_under_pressure() {
        let mut mgr = ContextManager::new(1_000);
        for i in 0..10 {
            let m = Message::tool(vec![ContentPart::ToolResult {
                call_id: format!("c{i}").into(),
                output: "x".repeat(40),
                is_error: false,
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
    fn frozen_prefix_len_anchors_at_compaction_and_holds_across_appends() {
        let mut mgr = ContextManager::new(1_000);
        // Pre-compaction: no frozen region yet → providers use the rolling-pair fallback.
        for i in 0..30 {
            mgr.push_history(
                Message::user(format!("turn {i}: {}", "ctx ".repeat(30))),
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
        mgr.push_history(Message::user("new 1"), 5);
        let f1 = mgr
            .render()
            .frozen_prefix_len
            .expect("frozen region exists once the tail grows");
        mgr.push_history(Message::assistant("reply 1"), 5);
        mgr.push_history(Message::user("new 2"), 5);
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
            mgr.push_history(Message::user(format!("m{i}")), 5);
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
    fn mark_spooled_sets_residency_and_survives_residency_recompute() {
        let mut mgr = ContextManager::new(1_000);
        mgr.push_history(
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "big".into(),
                output: "preview only".to_string(),
                is_error: false,
            }]),
            10,
        );
        mgr.mark_spooled("big", "disk://big");
        assert_eq!(
            mgr.handles.residency_for_source("big"),
            Some(&Residency::SpooledOut {
                r: "disk://big".to_string()
            })
        );

        // Even under collapse pressure, a spooled handle is not pulled into the
        // Resident<->Collapsed projection cycle.
        mgr.set_observed_prompt_tokens(990);
        mgr.recompute_handle_residency();
        assert_eq!(
            mgr.handles.residency_for_source("big"),
            Some(&Residency::SpooledOut {
                r: "disk://big".to_string()
            })
        );
    }

    #[test]
    fn push_history_indexes_tool_results_as_resident_handles() {
        let mut mgr = ContextManager::new(10_000);
        let msg = Message::tool(vec![ContentPart::ToolResult {
            call_id: "call_1".into(),
            output: "the tool output".to_string(),
            is_error: false,
        }]);
        mgr.push_history(msg, 20);
        // A handle was indexed, anchored to the call_id, resident by default.
        assert_eq!(mgr.handles.all().len(), 1);
        assert_eq!(
            mgr.handles.residency_for_source("call_1"),
            Some(&Residency::Resident)
        );
        // A plain text turn allocates no handle.
        mgr.push_history(Message::user("hello"), 5);
        assert_eq!(mgr.handles.all().len(), 1);
    }

    // ── W1-3: handle-table GC (prune orphaned handles + bounded recompute) ──

    fn tool_result_msg(call_id: &str, output: &str) -> Message {
        Message::tool(vec![ContentPart::ToolResult {
            call_id: call_id.into(),
            output: output.to_string(),
            is_error: false,
        }])
    }

    #[test]
    fn prune_orphaned_handles_drops_handles_whose_message_left_history() {
        let mut mgr = ContextManager::new(10_000);
        mgr.push_history(tool_result_msg("c0", "out 0"), 20);
        mgr.push_history(tool_result_msg("c1", "out 1"), 20);
        assert_eq!(mgr.handles.all().len(), 2);

        // Simulate compaction archiving the oldest tool-result message out of history.
        mgr.partitions.history.messages.remove(0);
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
            .messages
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

    #[test]
    fn recompute_residency_index_semantics_with_spooled_in_the_middle() {
        // Locks the O(n)-rewrite's index/cutoff semantics against the old id+get_mut version:
        // a spooled handle still occupies an index position but is never toggled.
        let mut mgr = ContextManager::new(1_000);
        for i in 0..6 {
            mgr.push_history(tool_result_msg(&format!("c{i}"), &"y".repeat(40)), 40);
        }
        mgr.mark_spooled("c2", "disk://c2");

        mgr.set_observed_prompt_tokens(950); // rho >= collapse_threshold
        mgr.recompute_handle_residency();

        // Spooled stays spooled; the most recent configured tool-result handles stay resident.
        assert_eq!(
            mgr.handles.residency_for_source("c2"),
            Some(&Residency::SpooledOut {
                r: "disk://c2".to_string()
            })
        );
        assert_eq!(
            mgr.handles.residency_for_source("c0"),
            Some(&Residency::Collapsed)
        );
        assert_eq!(
            mgr.handles.residency_for_source("c5"),
            Some(&Residency::Resident)
        );
    }

    // ── K2: knowledge budget ─────────────────────────────────────────────────

    #[test]
    fn knowledge_budget_uses_stable_order_for_equal_value_and_warns_once() {
        // max_tokens 100 × default ratio 0.25 ⇒ budget 25. Four 10-token entries (40 used):
        // two evictable, one pinned, one skill pin.
        let mut mgr = ContextManager::new(100);
        mgr.push_knowledge(Message::system("oldest unkeyed"), 10);
        mgr.push_knowledge_entry(Some("a".into()), Message::system("keyed"), 10, false);
        mgr.push_knowledge_entry(Some("p".into()), Message::system("pinned"), 10, true);
        mgr.push_knowledge_entry(Some("skill:x".into()), Message::system("skill"), 10, false);

        let warn = mgr.enforce_knowledge_budget();
        assert_eq!(warn, Some((40, 25)));
        // Equal scores retain the deterministic insertion-order tie-break: unkeyed then "a".
        let e = &mgr.partitions.knowledge.entries;
        assert!(e[0].evict_at_boundary);
        assert!(e[1].evict_at_boundary);
        assert!(!e[2].evict_at_boundary, "pinned exempt");
        assert!(!e[3].evict_at_boundary, "skill pin exempt");

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
        mgr.push_knowledge_entry(Some("p".into()), Message::system("pinned heavy"), 30, true);
        mgr.push_knowledge_entry(
            Some("skill:x".into()),
            Message::system("skill heavy"),
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
                .all(|e| !e.evict_at_boundary)
        );
    }

    #[test]
    fn knowledge_budget_retains_old_referenced_entry_over_new_irrelevant_entry() {
        let mut mgr = ContextManager::new(100);
        mgr.push_knowledge_entry(
            Some("project:orchid".into()),
            Message::system("ORCHID uses the Atlas storage engine"),
            10,
            false,
        );
        // A committed history input is the deterministic usage fact.
        mgr.push_history(Message::user("For project:orchid keep using Atlas"), 5);
        mgr.push_knowledge_entry(
            Some("project:new".into()),
            Message::system("unrelated fresh material"),
            10,
            false,
        );
        mgr.push_knowledge_entry(
            Some("project:other".into()),
            Message::system("another unused reference"),
            10,
            false,
        );

        assert_eq!(mgr.enforce_knowledge_budget(), Some((30, 25)));
        let entries = &mgr.partitions.knowledge.entries;
        assert!(
            !entries[0].evict_at_boundary,
            "a real reference must raise retention"
        );
        assert!(
            entries[1].evict_at_boundary,
            "lowest-value entry evicts first"
        );
        assert!(
            !entries[2].evict_at_boundary,
            "one eviction is enough to fit"
        );
    }

    #[test]
    fn knowledge_budget_ratio_zero_disables() {
        let mut mgr = ContextManager::new(100);
        mgr.config.knowledge_budget_ratio = 0.0;
        mgr.push_knowledge(Message::system("huge"), 90);
        assert_eq!(mgr.enforce_knowledge_budget(), None);
        assert!(!mgr.partitions.knowledge.entries[0].evict_at_boundary);
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
        mgr.partitions
            .system
            .push(Message::system("x".repeat(240)), 60);

        let rendered = mgr.render();
        let overflow = rendered
            .budget_overflow
            .expect("fixed context exceeds input allowance");
        assert_eq!(overflow.max_tokens, 50);
        assert!(overflow.required_tokens > overflow.max_tokens);
    }
}
