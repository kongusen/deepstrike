use super::compression::CompressionPipeline;
use super::config::ContextConfig;
use super::partitions::ContextPartitions;
use super::pressure::{PressureAction, PressureMonitor};
use super::renderer::RenderedContext;
use super::renewal::{HandoffArtifact, RenewalPolicy};
use super::sections::{ContextSectionPartition, ContextSectionRegistry};
use super::snapshot::{ContextSnapshotHint, ContextSnapshot};
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
    pub sprint: u32,
    pub last_handoff: Option<HandoffArtifact>,
    pub skills: SkillCatalog,
    pub capabilities: CapabilityManifest,
    pub sections: ContextSectionRegistry,
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
}

impl ContextManager {
    pub fn new(max_tokens: u32) -> Self {
        Self::with_config(max_tokens, ContextConfig::default(), ContextTokenEngine::char_approx())
    }

    pub fn with_config(max_tokens: u32, config: ContextConfig, engine: ContextTokenEngine) -> Self {
        let compression = CompressionPipeline::new(&config);
        let pressure = PressureMonitor::new(max_tokens, config.clone());
        let renewal = RenewalPolicy::from_config(&config);
        let partitions = ContextPartitions::new(&config);
        Self {
            partitions, max_tokens, config, engine,
            sprint: 0, last_handoff: None,
            skills: SkillCatalog::new(),
            capabilities: CapabilityManifest::new(),
            sections: ContextSectionRegistry::default_agent_sections(),
            memory_enabled: false, knowledge_enabled: false, plan_tool_enabled: false,
            last_observed_prompt_tokens: None,
            compression, pressure, renewal,
            last_activity_ms: 0,
            last_compact_ms: None,
            handles: HandleTable::new(),
            next_handle_id: 0,
        }
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
    /// `preserve_recent_msgs` tool results are marked `Collapsed` (rendered as previews); when
    /// pressure subsides they return to `Resident`. Non-destructive: `partitions` is untouched, so
    /// projection fully reverses. Spooled/paged-out handles (Layer 1/page-out) are left as-is.
    pub fn recompute_handle_residency(&mut self) {
        let collapse = self.rho() >= self.config.collapse_threshold;
        let keep = self.config.preserve_recent_msgs;
        // Single mutable pass in insertion order. The previous implementation collected ids then
        // re-found each via `get_mut` (a linear scan), making this O(handles²) **every turn** — the
        // long-session cliff. `tool_result_handles_mut().enumerate()` yields the same set in the
        // same order, so `i`/`cutoff` are bit-identical to the old logic, now in O(handles).
        let total = self
            .handles
            .all()
            .iter()
            .filter(|h| matches!(h.kind, HandleKind::ToolResult))
            .count();
        let cutoff = total.saturating_sub(keep);
        for (i, handle) in self.handles.tool_result_handles_mut().enumerate() {
            // Only toggle the reversible Resident<->Collapsed axis; never clobber a handle
            // that has been spooled or paged out.
            if matches!(handle.residency, Residency::Resident | Residency::Collapsed) {
                handle.residency = if collapse && i < cutoff {
                    Residency::Collapsed
                } else {
                    Residency::Resident
                };
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
    /// un-collapse would oscillate). Compaction/renewal triggers use [`Self::effective_rho`] instead.
    pub fn rho(&self) -> f64 {
        self.pressure
            .pressure(&self.partitions, &self.engine, self.last_observed_prompt_tokens)
    }

    /// **Effective** rho — the pressure that actually drives compaction/renewal, made paging-aware.
    ///
    /// When provider usage is authoritative (`observed_prompt_tokens` set), the rendered prompt was
    /// already collapsed (the renderer emits previews for `Collapsed` handles), so the observed count
    /// already reflects paging — raw rho is exact and returned as-is. In the **estimate** path
    /// (no observed tokens) we estimate from `partitions`, which still carry the full weight of
    /// paged-out tool results (collapse is non-destructive); we subtract the non-resident handle
    /// tokens so that collapsing/spooling a result immediately relieves pressure, rather than only
    /// after the next provider round-trip. With no paged handles this equals [`Self::rho`], so the
    /// pre-paging behavior is preserved exactly.
    pub fn effective_rho(&self) -> f64 {
        if self.max_tokens == 0 || self.last_observed_prompt_tokens.is_some() {
            return self.rho();
        }
        let total = self.partitions.total_tokens(&self.engine);
        let effective = total.saturating_sub(self.handles.non_resident_tokens());
        effective as f64 / self.max_tokens as f64
    }

    pub fn set_observed_prompt_tokens(&mut self, tokens: u32) {
        self.last_observed_prompt_tokens = Some(tokens);
    }

    pub fn should_compress(&self) -> PressureAction {
        // Compaction-tier recommendation runs on **raw** rho. The paging-aware `effective_rho` was
        // wired here during W1-1 but it over-relieved pressure: once `micro_compact` paged out
        // tool-result handles, effective rho fell below the collapse/auto_compact thresholds, so the
        // heavy tiers never fired — violating W1-1's own DoD ("既有压缩 golden 不变" /
        // "AutoCompact 后 wake 注入语义摘要"). Until the full cache-aware planner lands (the planner
        // that scores prefix-invalidation per op, `effective_rho` reserved for it), the tier trigger
        // must use raw rho so escalation is preserved. `effective_rho` stays defined + tested for
        // that work; it is intentionally not consulted by the trigger today.
        self.pressure.recommend(self.rho())
    }

    pub fn compress(&mut self, action: PressureAction) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
        self.compress_with_time(action, None)
    }

    pub fn compress_with_time(
        &mut self,
        action: PressureAction,
        now_ms: Option<u64>,
    ) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
        if self.sections.is_partition_pinned(ContextSectionPartition::History) {
            return (0, None, vec![], None);
        }

        let result = {
            let target = self.config.target_tokens(self.max_tokens);
            self.compression.compress(&mut self.partitions, action, self.max_tokens, target, &self.engine)
        };

        // Record compression timestamp if provided
        if let Some(ts) = now_ms {
            self.last_compact_ms = Some(ts);
        }

        // Archived messages have left history — drop their now-orphaned handles (bounds the table).
        if !result.2.is_empty() {
            self.prune_orphaned_handles();
        }

        result
    }

    pub fn force_compress(&mut self) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
        if self.sections.is_partition_pinned(ContextSectionPartition::History) {
            return (0, None, vec![], None);
        }
        let result = self.compression.compress(&mut self.partitions, PressureAction::AutoCompact, self.max_tokens, 0, &self.engine);
        if !result.2.is_empty() {
            self.prune_orphaned_handles();
        }
        result
    }

    /// W1-1 收口: run one compaction `action` toward an **explicit** `target_tokens`, instead of
    /// re-deriving the target from config. This is what lets `EvictionOp::Collapse { target_tokens }`
    /// flow from the planner (the single decision point) straight to the executor — the compactor no
    /// longer re-decides the target. `compress_with_time` remains the config-derived convenience used
    /// by the other layers (Snip/Micro), whose target equals `config.target_tokens(max_tokens)`.
    pub fn compress_with_target(
        &mut self,
        action: PressureAction,
        target_tokens: u32,
        now_ms: Option<u64>,
    ) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
        if self.sections.is_partition_pinned(ContextSectionPartition::History) {
            return (0, None, vec![], None);
        }
        let result =
            self.compression
                .compress(&mut self.partitions, action, self.max_tokens, target_tokens, &self.engine);
        if let Some(ts) = now_ms {
            self.last_compact_ms = Some(ts);
        }
        if !result.2.is_empty() {
            self.prune_orphaned_handles();
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
        self.renewal.should_renew(&self.pressure, &self.partitions, &self.engine)
    }

    pub fn renew(&mut self) {
        let goal = self.partitions.task_state.goal.clone();
        let (renewed, artifact) = self.renewal.renew(&self.partitions, &goal, self.sprint, self.max_tokens);
        self.partitions = renewed;
        self.last_handoff = Some(artifact);
        self.sprint += 1;
        // History was rebuilt wholesale — drop handles anchored to messages it no longer carries.
        self.prune_orphaned_handles();
    }

    // ── Render ────────────────────────────────────────────────────────────────

    pub fn render(&self) -> RenderedContext {
        super::renderer::render_projected(
            &self.partitions,
            self.max_tokens,
            &self.engine,
            self.config.preserve_recent_msgs,
            &self.handles,
        )
    }

    pub fn snapshot_hint(&self) -> ContextSnapshotHint {
        ContextSnapshotHint::from_parts(&self.sections, &self.capabilities)
    }

    pub fn take_snapshot(&self, turn: u32) -> ContextSnapshot {
        ContextSnapshot {
            turn,
            system_messages: self.partitions.system.messages.clone(),
            knowledge_messages: self.partitions.knowledge.messages.clone(),
            history_messages: self.partitions.history.messages.clone(),
            task_state: self.partitions.task_state.clone(),
        }
    }

    // ── History / Knowledge ───────────────────────────────────────────────────

    pub fn push_history(&mut self, msg: Message, tokens: u32) {
        // P3 (3a): index each tool result entering working context as a handle, anchored to its
        // call_id. Pure bookkeeping — render/compression still read `partitions` until 3b. The
        // handle's residency later drives read-time projection without mutating the message.
        if let Content::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolResult { call_id, output, .. } = part {
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

    /// Push a runtime signal into the current turn's State slot.
    /// Signals are ephemeral — cleared after each render.
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
        self.partitions.task_state = TaskState { goal, criteria, ..Default::default() };
    }

    pub fn update_task(&mut self, update: TaskUpdate) {
        self.partitions.task_state.apply(update);
    }

    // ── Section pinning ───────────────────────────────────────────────────────

    pub fn pin_section(&mut self, id: &str) -> bool { self.sections.pin(id) }
    pub fn unpin_section(&mut self, id: &str) -> bool { self.sections.unpin(id) }

    // ── Skills ────────────────────────────────────────────────────────────────

    pub fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.capabilities.remove_kind(CapabilityKind::Skill);
        for skill in &skills { self.capabilities.add_skill(skill.clone()); }
        self.skills.set_available(skills);
    }

    pub fn skill_tool_schema(&self) -> Option<ToolSchema> {
        self.skills.build_tool_schema()
    }

    // ── Meta-tools ────────────────────────────────────────────────────────────

    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(CapabilityKind::Memory, MEMORY_TOOL_NAME,
                "Search long-term memory through the memory meta-tool.");
        } else {
            self.capabilities.remove(CapabilityKind::Memory, MEMORY_TOOL_NAME);
        }
    }

    pub fn set_knowledge_enabled(&mut self, enabled: bool) {
        self.knowledge_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(CapabilityKind::Knowledge, KNOWLEDGE_TOOL_NAME,
                "Search external knowledge through the knowledge meta-tool.");
        } else {
            self.capabilities.remove(CapabilityKind::Knowledge, KNOWLEDGE_TOOL_NAME);
        }
    }

    pub fn set_plan_tool_enabled(&mut self, enabled: bool) {
        self.plan_tool_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(CapabilityKind::Tool, "update_plan",
                "Update task plan and progress through the planning meta-tool.");
        } else {
            self.capabilities.remove(CapabilityKind::Tool, "update_plan");
        }
    }

    pub fn capability_inventory(&self) -> String { self.capabilities.format_inventory() }

    pub fn meta_tool_schemas(&self) -> Vec<ToolSchema> {
        let mut tools = Vec::new();
        if let Some(t) = self.skill_tool_schema() { tools.push(t); }
        if let Some(t) = self.memory_tool_schema() { tools.push(t); }
        if let Some(t) = self.knowledge_tool_schema() { tools.push(t); }
        if let Some(t) = self.plan_tool_schema() { tools.push(t); }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    pub fn plan_tool_schema(&self) -> Option<ToolSchema> {
        if !self.plan_tool_enabled { return None; }
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
        if !self.memory_enabled { return None; }
        Some(ToolSchema {
            name: CompactString::new(MEMORY_TOOL_NAME),
            description: "Search your long-term memory for relevant past experiences and knowledge.".to_string(),
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
        if !self.knowledge_enabled { return None; }
        Some(ToolSchema {
            name: CompactString::new(KNOWLEDGE_TOOL_NAME),
            description: "Search the external knowledge base for facts, documentation, or reference data.".to_string(),
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
    fn manager_renew_uses_task_state_goal() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("test goal".to_string(), vec![]);
        mgr.partitions.system.push(Message::system("rules"), 10);
        for i in 0..10 { mgr.push_history(Message::user(format!("msg {i}")), 50); }
        mgr.renew();
        let artifact = mgr.last_handoff.as_ref().unwrap();
        assert_eq!(artifact.goal, "test goal");
        assert_eq!(mgr.sprint, 1);
    }

    #[test]
    fn compress_only_touches_history() {
        let mut mgr = ContextManager::new(1_000);
        mgr.push_knowledge(Message::system("knowledge content"), 100);
        for _ in 0..30 { mgr.push_history(Message::user("history msg"), 50); }
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
        for _ in 0..10 { mgr.push_history(Message::user("filler"), 50); }
        mgr.compress(PressureAction::AutoCompact);
        assert_eq!(mgr.partitions.task_state.goal, "survive compression");
        assert_eq!(mgr.partitions.task_state.plan.len(), 2);
    }

    #[test]
    fn render_includes_task_state_in_turns_not_system() {
        let mut mgr = ContextManager::new(10_000);
        mgr.init_task("find anomalies".to_string(), vec![]);
        let rc = mgr.render();
        assert!(!rc.system_text.contains("[TASK STATE]"), "task_state must not be in system_text");
        assert!(rc.turns[0].content.as_text().unwrap().contains("[TASK STATE] goal: find anomalies"));
    }

    #[test]
    fn renewal_open_tasks_from_task_state() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("g".to_string(), vec![]);
        mgr.partitions.task_state.plan = vec![
            PlanStep { label: "done".to_string(), done: true },
            PlanStep { label: "pending".to_string(), done: false },
        ];
        mgr.renew();
        let artifact = mgr.last_handoff.as_ref().unwrap();
        assert_eq!(artifact.open_tasks, vec!["pending"]);
    }

    #[test]
    fn pinned_history_section_skips_compression() {
        let mut mgr = ContextManager::new(1_000);
        for _ in 0..30 { mgr.push_history(Message::user("filler message for pinning test"), 50); }
        let tokens_before = mgr.partitions.history.token_count;
        mgr.pin_section("history.rolling");
        let (saved, _, _, _) = mgr.compress(PressureAction::AutoCompact);
        assert_eq!(saved, 0);
        assert_eq!(mgr.partitions.history.token_count, tokens_before);
    }

    #[test]
    fn unpinned_history_section_allows_compression() {
        let mut mgr = ContextManager::new(1_000);
        for _ in 0..30 { mgr.push_history(Message::user("filler"), 50); }
        mgr.pin_section("history.rolling");
        mgr.unpin_section("history.rolling");
        let (saved, _, _, _) = mgr.compress(PressureAction::AutoCompact);
        assert!(saved > 0);
    }

    #[test]
    fn force_compress_also_skips_when_history_pinned() {
        let mut mgr = ContextManager::new(1_000);
        for _ in 0..10 { mgr.push_history(Message::user("filler"), 50); }
        mgr.pin_section("history.rolling");
        let (saved, _, _, _) = mgr.force_compress();
        assert_eq!(saved, 0);
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
            mgr.push_history(Message::user(format!("turn {i}: {}", "ctx ".repeat(40))), 200);
        }
        let (saved, summary, _, _) = mgr.force_compress();
        assert!(saved > 0, "force_compress should compact a large history");
        assert!(summary.is_some(), "auto-compact summarizes the archived turns");
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
        assert!(mgr.skill_tool_schema().unwrap().description.contains("debug"));
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
        let names = mgr.meta_tool_schemas().into_iter().map(|s| s.name.to_string()).collect::<Vec<_>>();
        assert_eq!(names, ["knowledge", "memory", "skill"]);
    }

    #[test]
    fn section_registry_is_available_on_manager() {
        let mgr = ContextManager::new(1_000);
        assert!(mgr.sections.get("capabilities.inventory").is_some());
    }

    #[test]
    fn snapshot_hint_changes_when_capabilities_change() {
        let mut mgr = ContextManager::new(1_000);
        let before = mgr.snapshot_hint();
        mgr.set_memory_enabled(true);
        let after = mgr.snapshot_hint();
        assert_ne!(before.capability_manifest_hash, after.capability_manifest_hash);
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
        // Oldest is collapsed; the most recent (within preserve_recent_msgs) stays resident.
        assert_eq!(mgr.handles.residency_for_source("c0"), Some(&Residency::Collapsed));
        assert_eq!(mgr.handles.residency_for_source("c9"), Some(&Residency::Resident));

        // Reversible: once pressure drops, collapse is undone (read-time projection only).
        mgr.set_observed_prompt_tokens(100); // 0.10 < 0.90
        mgr.recompute_handle_residency();
        assert_eq!(mgr.handles.residency_for_source("c0"), Some(&Residency::Resident));
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
            Some(&Residency::SpooledOut { r: "disk://big".to_string() })
        );

        // Even under collapse pressure, a spooled handle is not pulled into the
        // Resident<->Collapsed projection cycle.
        mgr.set_observed_prompt_tokens(990);
        mgr.recompute_handle_residency();
        assert_eq!(
            mgr.handles.residency_for_source("big"),
            Some(&Residency::SpooledOut { r: "disk://big".to_string() })
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
    fn effective_rho_discounts_paged_out_handles() {
        let mut mgr = ContextManager::new(1_000);
        // A large tool-result output so its handle carries a real token weight.
        let big = "data ".repeat(200);
        let tok = mgr.engine.count(&big);
        mgr.push_history(tool_result_msg("c0", &big), tok);
        mgr.push_history(Message::user("u"), 50);

        let raw = mgr.rho();
        // Everything resident → effective equals raw (behavior-preserving when nothing is paged).
        assert_eq!(mgr.handles.non_resident_tokens(), 0);
        assert!((mgr.effective_rho() - raw).abs() < f64::EPSILON);

        // Page the tool result out of working context.
        mgr.mark_spooled("c0", "disk://c0");
        let paged = mgr.handles.non_resident_tokens();
        assert!(paged > 0, "handle is now non-resident with a real token weight");

        // Raw rho is unchanged (partitions are untouched by the non-destructive projection)...
        assert!((mgr.rho() - raw).abs() < f64::EPSILON, "raw rho unchanged by paging");
        // ...but effective rho drops by exactly the paged tokens — paging relieves pressure now.
        let total = mgr.partitions.total_tokens(&mgr.engine);
        let expected = total.saturating_sub(paged) as f64 / 1_000.0;
        assert!((mgr.effective_rho() - expected).abs() < f64::EPSILON);
        assert!(mgr.effective_rho() < raw, "effective pressure relieved by paging");

        // When provider usage is authoritative, the rendered prompt was already collapsed, so
        // effective falls back to raw (no double-discount).
        mgr.set_observed_prompt_tokens(900);
        assert!((mgr.effective_rho() - mgr.rho()).abs() < f64::EPSILON);
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
            .filter(|m| matches!(&m.content, Content::Parts(p)
                if p.iter().any(|x| matches!(x, ContentPart::ToolResult { .. }))))
            .count();
        assert_eq!(mgr.handles.all().len(), live_tool_results);
        assert!(mgr.handles.all().len() < 30, "table must shrink with archival");
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

        // Spooled stays spooled; the most recent preserve_recent_msgs stay resident; older collapse.
        assert_eq!(
            mgr.handles.residency_for_source("c2"),
            Some(&Residency::SpooledOut { r: "disk://c2".to_string() })
        );
        assert_eq!(mgr.handles.residency_for_source("c0"), Some(&Residency::Collapsed));
        assert_eq!(mgr.handles.residency_for_source("c5"), Some(&Residency::Resident));
    }
}
