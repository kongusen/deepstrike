use super::*;
use std::collections::{HashMap, VecDeque};

pub(super) const EVENT_REPLAY_WINDOW_CAPACITY: usize = 256;
const COMPLETED_EFFECT_REPLAY_WINDOW_CAPACITY: usize = 256;

#[derive(Clone)]
struct RecordedTransition {
    fingerprint: serde_json::Value,
    step: KernelStep,
}

struct ReplayWindow {
    entries: HashMap<String, RecordedTransition>,
    order: VecDeque<String>,
    capacity: usize,
}

impl ReplayWindow {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&self, identity: &str) -> Option<&RecordedTransition> {
        self.entries.get(identity)
    }

    fn insert(&mut self, identity: String, transition: RecordedTransition) {
        if self.entries.len() == self.capacity {
            if let Some(expired_identity) = self.order.pop_front() {
                self.entries.remove(&expired_identity);
            }
        }
        self.order.push_back(identity.clone());
        self.entries.insert(identity, transition);
    }

    fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        while self.entries.len() > capacity {
            if let Some(expired_identity) = self.order.pop_front() {
                self.entries.remove(&expired_identity);
            }
        }
        self.entries.shrink_to(capacity);
        self.order.shrink_to(capacity);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingEffectKind {
    Provider,
    Tool,
    Milestone,
    Approval,
    WorkflowSpawn,
    Preempt,
    MemoryPersist,
    MemoryQuery,
    LargeResultSpool,
    PageOutArchive,
}

#[derive(Clone, Copy)]
enum LifecycleTransition {
    Stay,
    Configure,
    Start,
    Resume,
}

struct StepIdentity {
    operation_id: String,
    input_event_id: String,
    step_seq: u64,
}

impl StepIdentity {
    fn empty(&self, observations: Vec<KernelObservation>) -> KernelStep {
        KernelStep::empty(
            self.operation_id.clone(),
            self.input_event_id.clone(),
            self.step_seq,
            observations,
        )
    }

    fn single(self, action: LoopAction, observations: Vec<KernelObservation>) -> KernelStep {
        KernelStep::single(
            self.operation_id,
            self.input_event_id,
            self.step_seq,
            action,
            observations,
        )
    }
}

/// Pure kernel runtime wrapper. SDKs should migrate toward feeding
/// `KernelInput` values here instead of directly driving `LoopStateMachine`.
pub struct KernelRuntime {
    sm: LoopStateMachine,
    lifecycle: KernelLifecycle,
    operation_id: Option<String>,
    next_step_seq: u64,
    recorded_events: ReplayWindow,
    pending_effects: HashMap<String, PendingEffectKind>,
    completed_effects: ReplayWindow,
    pending_memory_write: Option<crate::mm::memory::MemoryWriteRequest>,
    pending_memory_query: Option<(crate::mm::memory::MemoryQuery, usize)>,
}

impl KernelRuntime {
    pub fn new(policy: SchedulerBudget) -> Self {
        Self {
            sm: LoopStateMachine::new(policy),
            lifecycle: KernelLifecycle::Created,
            operation_id: None,
            next_step_seq: 1,
            recorded_events: ReplayWindow::new(EVENT_REPLAY_WINDOW_CAPACITY),
            pending_effects: HashMap::new(),
            completed_effects: ReplayWindow::new(COMPLETED_EFFECT_REPLAY_WINDOW_CAPACITY),
            pending_memory_write: None,
            pending_memory_query: None,
        }
    }

    #[cfg(test)]
    pub(super) fn state_machine(&self) -> &LoopStateMachine {
        &self.sm
    }

    #[cfg(test)]
    pub(super) fn state_machine_mut(&mut self) -> &mut LoopStateMachine {
        &mut self.sm
    }

    pub fn is_terminal(&self) -> bool {
        self.lifecycle.is_terminal()
    }

    pub fn lifecycle(&self) -> KernelLifecycle {
        self.lifecycle
    }

    pub fn turn(&self) -> u32 {
        self.sm.turn
    }

    pub fn recovery_content_bytes(&self) -> usize {
        let tokens = self
            .sm
            .ctx
            .config
            .recovery_content_tokens(self.sm.ctx.max_tokens);
        self.sm.ctx.engine.token_budget_to_bytes(tokens)
    }

    pub fn render(&self) -> RenderedContext {
        self.sm.ctx.render()
    }

    pub fn drain_new_messages(&mut self) -> Vec<Message> {
        self.sm.drain_new_messages()
    }

    pub fn preserved_refs(&self) -> Vec<String> {
        self.sm.ctx.partitions.task_state.preserved_refs.clone()
    }

    pub fn count_tokens(&self, text: &str) -> u32 {
        self.sm.ctx.engine.count(text)
    }

    /// L1 (RunGroup): this vehicle's cumulative sub-agent spawns this run, read back by the SDK at run
    /// end to charge the group ledger (so the next member's cumulative spawn cap is seeded correctly).
    pub fn local_subagents_spawned(&self) -> u32 {
        self.sm.local_subagents_spawned()
    }

    #[cfg(test)]
    pub(super) fn pending_provider_effect_id(&self) -> String {
        self.pending_effect_id(PendingEffectKind::Provider)
    }

    #[cfg(test)]
    pub(super) fn pending_tool_effect_id(&self) -> String {
        self.pending_effect_id(PendingEffectKind::Tool)
    }

    #[cfg(test)]
    fn pending_effect_id(&self, kind: PendingEffectKind) -> String {
        let matches = self
            .pending_effects
            .iter()
            .filter(|(_, pending_kind)| **pending_kind == kind)
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "test transition must have one {kind:?} effect"
        );
        matches[0].0.clone()
    }

    #[cfg(test)]
    pub(super) fn recorded_event_count(&self) -> usize {
        self.recorded_events.len()
    }

    /// Decode and execute one wire input. The version is probed before decoding
    /// the v2 envelope so a v1 payload receives a structured kernel fault rather
    /// than a host-language deserialization error.
    pub fn step_json(&mut self, input_json: &str) -> Result<KernelStep, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(input_json)?;
        if value.get("version").and_then(serde_json::Value::as_u64)
            != Some(KERNEL_ABI_VERSION as u64)
        {
            let operation_id = value
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let event_id = value
                .get("event_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let received_version = value
                .get("version")
                .and_then(serde_json::Value::as_u64)
                .map_or_else(|| "missing".to_string(), |version| version.to_string());
            return Ok(self.fault_step(
                operation_id,
                event_id,
                KernelFaultCode::VersionMismatch,
                format!(
                    "kernel ABI version mismatch: input v{received_version}, kernel v{KERNEL_ABI_VERSION}"
                ),
                None,
            ));
        }

        serde_json::from_value(value).map(|input| self.step(input))
    }

    pub fn step(&mut self, input: KernelInput) -> KernelStep {
        let operation_id = input.operation_id.clone();
        let event_id = input.event_id.clone();

        if input.version != KERNEL_ABI_VERSION {
            return self.fault_step(
                operation_id,
                event_id,
                KernelFaultCode::VersionMismatch,
                format!(
                    "kernel ABI version mismatch: input v{}, kernel v{}",
                    input.version, KERNEL_ABI_VERSION
                ),
                None,
            );
        }

        if operation_id.is_empty() || event_id.is_empty() {
            return self.fault_step(
                operation_id,
                event_id,
                KernelFaultCode::InvalidConfig,
                "operation_id and event_id must be non-empty".to_string(),
                None,
            );
        }

        let fingerprint = serde_json::to_value(&input)
            .expect("KernelInput serialization must succeed after typed construction");
        if let Some(recorded) = self.recorded_events.get(&event_id) {
            if recorded.fingerprint == fingerprint {
                return recorded.step.clone();
            }
            return self.fault_step(
                operation_id,
                event_id,
                KernelFaultCode::DuplicateEventConflict,
                "event_id was already accepted with a different payload".to_string(),
                None,
            );
        }

        if let Some(bound_operation_id) = &self.operation_id {
            if bound_operation_id != &operation_id {
                return self.fault_step(
                    operation_id,
                    event_id,
                    KernelFaultCode::OperationMismatch,
                    format!("input operation does not match bound operation {bound_operation_id}"),
                    None,
                );
            }
        }

        let result_fingerprint = result_effect(&input.event).map(|(effect_id, _)| {
            (
                effect_id.to_string(),
                serde_json::to_value(&input.event)
                    .expect("KernelInputEvent serialization must succeed after typed construction"),
            )
        });
        if let Some((effect_id, result_fingerprint)) = &result_fingerprint {
            if let Some(completed) = self.completed_effects.get(effect_id) {
                if completed.fingerprint == *result_fingerprint {
                    let step = completed.step.clone();
                    self.recorded_events.insert(
                        event_id,
                        RecordedTransition {
                            fingerprint,
                            step: step.clone(),
                        },
                    );
                    return step;
                }
                return self.fault_step(
                    operation_id,
                    event_id,
                    KernelFaultCode::UnexpectedEffectResult,
                    format!("effect result conflicts with the completed result: {effect_id}"),
                    Some(effect_id.clone()),
                );
            }
        }

        let lifecycle_transition = match self.lifecycle_transition(&input.event) {
            Ok(transition) => transition,
            Err(message) => {
                return self.fault_step(
                    operation_id,
                    event_id,
                    KernelFaultCode::InvalidLifecycle,
                    message,
                    None,
                );
            }
        };
        if let KernelInputEvent::ConfigureRun { config } = &input.event {
            if let Err(message) = validate_run_config(config) {
                return self.fault_step(
                    operation_id,
                    event_id,
                    KernelFaultCode::InvalidConfig,
                    message,
                    None,
                );
            }
        }
        if let KernelInputEvent::WorkflowSpawnResult {
            effect_id,
            started_agent_ids,
            failures,
            error,
        } = &input.event
        {
            if let Err(message) = self.sm.validate_workflow_spawn_result(
                started_agent_ids,
                failures,
                error.as_deref(),
            ) {
                return self.fault_step(
                    operation_id,
                    event_id,
                    KernelFaultCode::UnexpectedEffectResult,
                    message,
                    Some(effect_id.clone()),
                );
            }
        }
        if let KernelInputEvent::LargeResultSpoolResult {
            effect_id,
            spool_ref,
            error,
        } = &input.event
        {
            if error.is_none() && spool_ref.as_deref().map_or(true, str::is_empty) {
                return self.fault_step(
                    operation_id,
                    event_id,
                    KernelFaultCode::UnexpectedEffectResult,
                    "successful spool result requires a non-empty spool_ref".to_string(),
                    Some(effect_id.clone()),
                );
            }
        }

        if let Some((effect_id, expected_kind)) = result_effect(&input.event) {
            match self.pending_effects.get(effect_id) {
                Some(actual_kind) if actual_kind == &expected_kind => {
                    self.pending_effects.remove(effect_id);
                }
                _ => {
                    return self.fault_step(
                        operation_id,
                        event_id,
                        KernelFaultCode::UnexpectedEffectResult,
                        format!("effect result does not match a pending effect: {effect_id}"),
                        Some(effect_id.to_string()),
                    );
                }
            }
        }

        if self.operation_id.is_none() {
            self.operation_id = Some(operation_id.clone());
        }

        if input.observed_at_ms > 0 {
            self.sm.set_observed_time(input.observed_at_ms);
        }

        let step_seq = self.allocate_step_seq();
        let step = self.dispatch(
            StepIdentity {
                operation_id: operation_id.clone(),
                input_event_id: event_id.clone(),
                step_seq,
            },
            input.event,
        );
        self.advance_lifecycle(lifecycle_transition, &step);
        for action in &step.actions {
            if let Some(kind) = pending_effect_kind(&action.effect) {
                self.pending_effects
                    .retain(|_, pending_kind| *pending_kind != kind);
                self.pending_effects.insert(action.effect_id.clone(), kind);
            }
        }
        if let Some((effect_id, result_fingerprint)) = result_fingerprint {
            self.completed_effects.insert(
                effect_id,
                RecordedTransition {
                    fingerprint: result_fingerprint,
                    step: step.clone(),
                },
            );
        }
        self.recorded_events.insert(
            event_id,
            RecordedTransition {
                fingerprint,
                step: step.clone(),
            },
        );
        step
    }

    fn dispatch(&mut self, identity: StepIdentity, event: KernelInputEvent) -> KernelStep {
        let action = match event {
            KernelInputEvent::SetTools { tools } => {
                self.sm.tools = tools;
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetAvailableSkills { skills } => {
                self.sm.ctx.set_available_skills(skills);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SkillActivated { name, lease_turns } => {
                // B1: record the activation (B2 reads it in emit_call_llm to narrow tools).
                // The returned `changed` flag is the epoch boundary for D's cache re-anchor.
                // K3: a lease converts to an absolute expiry turn here (the manager is turn-blind).
                let expires_at_turn = lease_turns.map(|n| self.sm.turn.saturating_add(n));
                self.sm.ctx.activate_skill_leased(name, expires_at_turn);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SkillDeactivated { name } => {
                self.sm.ctx.deactivate_skill(&name);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetStableCoreTools { tool_ids } => {
                self.sm
                    .ctx
                    .set_stable_core_tools(tool_ids.into_iter().map(Into::into));
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetMemoryEnabled { enabled } => {
                self.sm.ctx.set_memory_enabled(enabled);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetKnowledgeEnabled { enabled } => {
                self.sm.ctx.set_knowledge_enabled(enabled);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetPlanToolEnabled { enabled } => {
                self.sm.ctx.set_plan_tool_enabled(enabled);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetTokenizer { .. } => {
                // Local BPE tokenisers are no longer used — accuracy comes from
                // observed_input_tokens reported by the provider API (P0-1 Step 2).
                // char_approx is always used for pre-flight truncation estimates.
                self.sm.ctx.engine = ContextTokenEngine::char_approx();
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::AddSystemMessage { content, tokens } => {
                self.sm
                    .ctx
                    .partitions
                    .system
                    .push(Message::system(content), tokens.max(1));
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::AddKnowledgeMessage {
                content,
                tokens,
                key,
                pinned,
            } => {
                // P1-B2 cache contract: the knowledge partition renders into the cached system[1]
                // block. Appending here is the right home for *stable* reference material (skill
                // defs, durable artifacts) — it's append-only, so the existing prefix stays
                // byte-stable, and a fresh append costs only a one-time system[1] re-cache. Do NOT
                // route *per-turn* retrievals (a memory/knowledge lookup that changes every turn)
                // through here: each would rewrite the cached block and invalidate it plus the
                // history cache every turn. Volatile per-turn context belongs on the signal/tail
                // path (`push_signal` → state_turn), which is uncached *and* high-attention (P1-F).
                //
                // K1: a `key` gives the entry identity — a same-key push stages a boundary-deferred
                // upsert instead of appending a duplicate (the cache contract above is why the swap
                // waits for the next compaction/renewal boundary).
                self.sm.ctx.push_knowledge_entry(
                    key.map(compact_str::CompactString::from),
                    Message::system(content),
                    tokens.max(1),
                    pinned,
                );
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::RemoveKnowledge { key } => {
                self.sm.ctx.remove_knowledge(&key);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::AddHistoryMessage { message, tokens } => {
                let tokens = tokens.unwrap_or_else(|| self.sm.ctx.engine.count_message(&message));
                self.sm.ctx.push_history(message, tokens.max(1));
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::PreloadHistory { messages } => {
                self.sm.preload_history(messages);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::MountCapability { capability } => {
                self.sm.mount_capability(capability, None, None);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::UnmountCapability {
                capability_kind,
                id,
            } => {
                self.sm.unmount_capability(capability_kind, &id);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::LoadMilestoneContract { contract } => {
                self.sm.load_milestone_contract(contract);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::LoadGovernancePolicy {
                default_action,
                rules,
                vetoed_tools,
                rate_limits,
                constraints,
            } => {
                self.sm.set_governance(build_governance_pipeline(
                    default_action,
                    rules,
                    vetoed_tools,
                    rate_limits,
                    constraints,
                ));
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::ConfigureRun { config } => {
                // K2: apply a bundle of run-setup config in one event (tools / governance / attention /
                // quota / scheduler / toggles), replacing the ~10 separate `Set*` / `Load*` events the
                // SDK used to fire one-by-one. Each field is optional; an absent field is left untouched.
                // The individual events remain for runtime mutation (skill mount, mid-run budget change).
                // Each branch delegates to exactly the method its granular event uses, so the two paths
                // can never diverge.
                let RunConfig {
                    tools,
                    available_skills,
                    stable_core_tools,
                    memory_enabled,
                    knowledge_enabled,
                    plan_tool_enabled,
                    tokenizer,
                    governance,
                    attention_max_queue_size,
                    scheduler_max_wall_ms,
                    resource_quota,
                    group_tokens_base,
                    group_spawns_base,
                    group_rounds_base,
                    repeat_fuse,
                    criteria_gate,
                    knowledge_budget_ratio,
                    entropy_watch,
                    reliability,
                } = config;
                if let Some(tools) = tools {
                    self.sm.tools = tools;
                }
                if let Some(skills) = available_skills {
                    self.sm.ctx.set_available_skills(skills);
                }
                if let Some(ids) = stable_core_tools {
                    self.sm
                        .ctx
                        .set_stable_core_tools(ids.into_iter().map(Into::into));
                }
                if let Some(enabled) = memory_enabled {
                    self.sm.ctx.set_memory_enabled(enabled);
                }
                if let Some(enabled) = knowledge_enabled {
                    self.sm.ctx.set_knowledge_enabled(enabled);
                }
                if let Some(enabled) = plan_tool_enabled {
                    self.sm.ctx.set_plan_tool_enabled(enabled);
                }
                if tokenizer.is_some() {
                    self.sm.ctx.engine = ContextTokenEngine::char_approx();
                }
                if let Some(g) = governance {
                    self.sm.set_governance(build_governance_pipeline(
                        g.default_action,
                        g.rules,
                        g.vetoed_tools,
                        g.rate_limits,
                        g.constraints,
                    ));
                }
                if let Some(max_queue) = attention_max_queue_size {
                    self.sm.set_attention(max_queue as usize);
                }
                if let Some(ms) = scheduler_max_wall_ms {
                    self.sm.set_wall_budget(Some(ms));
                }
                if let Some(quota) = resource_quota {
                    self.sm.set_resource_quota(quota);
                }
                if let Some(base) = group_tokens_base {
                    self.sm.seed_group_budget(base);
                }
                if let Some(base) = group_spawns_base {
                    self.sm.seed_group_spawns(base);
                }
                if let Some(base) = group_rounds_base {
                    self.sm.seed_group_rounds(base);
                }
                if let Some(fuse) = repeat_fuse {
                    self.sm.set_repeat_fuse(fuse);
                }
                if let Some(enabled) = criteria_gate {
                    self.sm.set_criteria_gate(enabled);
                }
                if let Some(ratio) = knowledge_budget_ratio {
                    self.sm.ctx.config.knowledge_budget_ratio = ratio;
                }
                if let Some(watch) = entropy_watch {
                    self.sm.set_entropy_watch(watch);
                }
                if let Some(reliability) = reliability {
                    if let Some(capacity) = reliability.event_replay_capacity {
                        self.recorded_events.set_capacity(capacity);
                    }
                    if let Some(capacity) = reliability.completed_effect_replay_capacity {
                        self.completed_effects.set_capacity(capacity);
                    }
                    self.sm.set_reliability_config(&reliability);
                }
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetAttentionPolicy { max_queue_size } => {
                self.sm.set_attention(max_queue_size as usize);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::PageIn { entries } => {
                self.sm.apply_page_in(&entries);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::ForceCompact => {
                self.sm.force_compact();
                self.sm
                    .externalize_pending_host_effect(LoopAction::AwaitingResume)
            }
            KernelInputEvent::UpdateTask { update } => {
                self.sm.ctx.update_task(update);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::StartRun { task, run_spec } => {
                self.sm.run_spec = run_spec;
                self.sm.start(task)
            }
            KernelInputEvent::CapabilityCommand { command } => {
                self.sm.execute_capability_command(command);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::Resume => self.sm.resume_after_preload(),
            KernelInputEvent::ApprovalResult {
                effect_id: _,
                approved_calls,
                denied_calls,
                error,
            } => match error {
                Some(error) => self.sm.retry_approval(error),
                None => self.sm.resolve_approval(approved_calls, denied_calls),
            },
            KernelInputEvent::WorkflowSpawnResult {
                effect_id: _,
                started_agent_ids,
                failures,
                error,
            } => match error {
                Some(error) => self.sm.retry_workflow_spawn(error),
                None => self.sm.resolve_workflow_spawn(started_agent_ids, failures),
            },
            KernelInputEvent::PreemptResult {
                effect_id: _,
                error,
            } => match error {
                Some(error) => self.sm.retry_preempt(error),
                None => self.sm.resolve_preempt(),
            },
            KernelInputEvent::MemoryPersistResult {
                effect_id: _,
                error,
            } => {
                let memory = self
                    .pending_memory_write
                    .take()
                    .expect("validated memory result requires pending write");
                let turn = self.sm.turn;
                match error {
                    Some(error) => self.sm.observations.push(
                        KernelObservation::MemoryWriteFailed {
                            turn,
                            memory_id: memory.metadata.name,
                            error,
                        },
                    ),
                    None => self.sm.observations.push(KernelObservation::MemoryWritten {
                        turn,
                        memory_id: memory.metadata.name,
                        memory_kind: memory
                            .metadata
                            .kind
                            .map(|kind| kind.label())
                            .unwrap_or("unclassified")
                            .to_string(),
                        size_bytes: memory.content.len() as u32,
                    }),
                }
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::MemoryQueryResult {
                effect_id: _,
                entries,
                error,
            } => {
                let (query, requested_k) = self
                    .pending_memory_query
                    .take()
                    .expect("validated memory result requires pending query");
                let turn = self.sm.turn;
                match error {
                    Some(error) => self.sm.observations.push(
                        KernelObservation::MemoryQueryFailed {
                            turn,
                            query_context: query.current_context,
                            error,
                        },
                    ),
                    None => {
                        self.sm.apply_page_in(&entries);
                        self.sm.observations.push(KernelObservation::MemoryQueried {
                            turn,
                            query_context: query.current_context,
                            requested_k,
                            requires_async_response: false,
                        });
                    }
                }
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::LargeResultSpoolResult {
                effect_id: _,
                spool_ref,
                error,
            } => self.sm.resolve_large_result_spool(spool_ref, error),
            KernelInputEvent::PageOutArchiveResult {
                effect_id: _,
                archive_ref,
                error,
            } => self.sm.resolve_page_out_archive(archive_ref, error),
            KernelInputEvent::SetSchedulerBudget { max_wall_ms } => {
                self.sm.set_wall_budget(max_wall_ms);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetResourceQuota { quota } => {
                self.sm.set_resource_quota(quota);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetCriteriaGate { enabled } => {
                self.sm.set_criteria_gate(enabled);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetKnowledgeBudget { ratio } => {
                self.sm.ctx.config.knowledge_budget_ratio = ratio;
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetRepeatFuse {
                enabled,
                deny_after,
                terminate_after,
            } => {
                let mut cfg = self.sm.repeat_fuse_config();
                if let Some(e) = enabled {
                    cfg.enabled = e;
                }
                if let Some(d) = deny_after {
                    cfg.deny_after = d;
                }
                if let Some(t) = terminate_after {
                    cfg.terminate_after = t;
                }
                self.sm.set_repeat_fuse(cfg);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::SetEntropyWatch {
                enabled,
                threshold,
                hysteresis,
                cooldown_turns,
                notify_model,
            } => {
                let mut cfg = self.sm.entropy_watch_config();
                if let Some(e) = enabled {
                    cfg.enabled = e;
                }
                if let Some(t) = threshold {
                    cfg.threshold = t;
                }
                if let Some(h) = hysteresis {
                    cfg.hysteresis = h;
                }
                if let Some(c) = cooldown_turns {
                    cfg.cooldown_turns = c;
                }
                if let Some(n) = notify_model {
                    cfg.notify_model = n;
                }
                self.sm.set_entropy_watch(cfg);
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::ProviderResult {
                effect_id: _,
                message,
                observed_input_tokens,
                observed_output_tokens: _,
                now_ms,
                stop_reason,
            } => {
                if let Some(tokens) = observed_input_tokens {
                    self.sm.ctx.set_observed_prompt_tokens(tokens);
                }
                // Feed the clock before the governance gate fires inside `feed`, so the
                // rate limiter sees a real timestamp (no-op when no policy is loaded).
                if let Some(ms) = now_ms {
                    self.sm.set_observed_time(ms);
                }
                // Stash stop_reason so `feed` can detect an output-cap truncation and drive recovery.
                self.sm.set_pending_stop_reason(stop_reason);
                self.sm.feed(LoopEvent::LLMResponse { message })
            }
            KernelInputEvent::ToolResults {
                effect_id: _,
                results,
            } => self.sm.feed(LoopEvent::ToolResults { results }),
            KernelInputEvent::ProviderError {
                effect_id: _,
                message,
            } => {
                // Reactive recovery is a kernel decision: classify + bounded compact-and-retry,
                // returning the next action (retry or honest terminal) through the common tail.
                self.sm.recover_from_provider_error(&message)
            }
            KernelInputEvent::Signal { signal } => match self.sm.signal_event(signal) {
                Some(action) => action,
                // Non-actionable disposition (queued / observed / ignored / dropped):
                // no provider call this step, just the SignalDisposed observation.
                None => {
                    return identity.empty(self.sm.take_observations());
                }
            },
            KernelInputEvent::MilestoneResult {
                effect_id: _,
                result,
            } => self.sm.feed(LoopEvent::MilestoneResult { result }),
            KernelInputEvent::SpawnSubAgent {
                spec,
                parent_session_id,
            } => self.sm.spawn_sub_agent(spec, &parent_session_id),
            KernelInputEvent::LoadWorkflow {
                spec,
                parent_session_id,
                resumed_completed,
                resumed_submissions,
                resumed_submission_bases,
                resumed_results,
            } => {
                if resumed_completed.is_empty()
                    && resumed_results.is_empty()
                    && resumed_submissions.is_empty()
                {
                    self.sm.load_workflow(spec, &parent_session_id)
                } else {
                    // W-1: merge legacy bare ids with signal-carrying records (records win).
                    use crate::orchestration::workflow::ResumedCompletion;
                    let mut completed: Vec<ResumedCompletion> = resumed_completed
                        .iter()
                        .filter(|id| resumed_results.iter().all(|r| &r.agent_id != *id))
                        .map(ResumedCompletion::bare)
                        .collect();
                    completed.extend(resumed_results);
                    self.sm.load_workflow_resumed(
                        spec,
                        &parent_session_id,
                        &resumed_submissions,
                        &resumed_submission_bases,
                        &completed,
                    )
                }
            }
            KernelInputEvent::SubAgentCompleted { result } => {
                self.sm.feed(LoopEvent::SubAgentCompleted { result })
            }
            KernelInputEvent::SubmitWorkflowNodes {
                nodes,
                submitter_agent_id,
            } => self
                .sm
                .submit_workflow_nodes(nodes, submitter_agent_id.as_deref()),
            KernelInputEvent::SubmitWorkflow {
                spec,
                parent_session_id,
                submitter_agent_id,
            } => self
                .sm
                .submit_workflow(spec, &parent_session_id, submitter_agent_id.as_deref()),
            KernelInputEvent::SetMemoryPolicy {
                memory_path,
                stale_warning_days,
                retrieval_top_k,
                validation_enabled,
                max_content_bytes,
                max_name_length,
            } => {
                // Phase 7: install the memory policy. The kernel enforces validation_enabled +
                // retrieval_top_k + size/name overrides at the WriteMemory/QueryMemory traps;
                // memory_path / stale_warning_days are carried for the SDK's recall I/O.
                self.sm.set_memory_policy(crate::mm::memory::MemoryPolicy {
                    memory_path,
                    stale_warning_days,
                    retrieval_top_k,
                    validation_enabled,
                    max_content_bytes,
                    max_name_length,
                });
                return identity.empty(self.sm.take_observations());
            }
            KernelInputEvent::WriteMemory { memory } => {
                // Phase 7: Validate memory write request.
                // Kernel validates; SDK performs I/O.
                use crate::mm::memory::validate_memory_write;
                let turn = self.sm.turn;
                // M2: route the write through the syscall trap so the resource quota (write-rate
                // limit) applies. A rate-limited / denied write surfaces as a validation failure
                // (the write does not happen) and short-circuits before validation.
                let disposition = self
                    .sm
                    .gate_syscall(&crate::syscall::Syscall::WriteMemory(memory.clone()));
                if !disposition.is_allowed() {
                    let error = match disposition {
                        crate::syscall::Disposition::RateLimited { retry_after_ms } => {
                            format!("memory write rate limited; retry after {retry_after_ms}ms")
                        }
                        crate::syscall::Disposition::Deny { reason, .. } => {
                            format!("memory write denied: {reason}")
                        }
                        _ => "memory write not permitted".to_string(),
                    };
                    self.sm
                        .observations
                        .push(KernelObservation::MemoryValidationFailed {
                            turn,
                            memory_id: memory.metadata.name.clone(),
                            error,
                        });
                    return identity.empty(self.sm.take_observations());
                }
                // Validate honoring any installed memory policy: a policy with validation disabled
                // admits the write outright; a policy with size/name overrides validates against
                // those; no policy uses the default rules (pre-policy behavior).
                let validation_result = match self.sm.memory_policy() {
                    Some(p) if !p.validation_enabled => Ok(()),
                    Some(p) => p.validation().validate(&memory),
                    None => validate_memory_write(&memory),
                };
                match validation_result {
                    Ok(()) => {
                        self.pending_memory_write = Some(memory.clone());
                        LoopAction::PersistMemory { memory }
                    }
                    Err(err) => {
                        // Emit validation error observation
                        use crate::mm::memory::MemoryValidationError;
                        let error_msg = match err {
                            MemoryValidationError::MissingRequiredField { field } => {
                                format!("Missing required field: {}", field)
                            }
                            MemoryValidationError::ContentTooLarge { size, limit } => {
                                format!("Content too large: {} bytes (limit: {})", size, limit)
                            }
                            MemoryValidationError::ForbiddenPattern { pattern, reason } => {
                                format!("Forbidden pattern '{}': {}", pattern, reason)
                            }
                            MemoryValidationError::InvalidKind { kind } => {
                                format!("Invalid kind: {}", kind)
                            }
                            MemoryValidationError::NameTooLong { length, limit } => {
                                format!("Name too long: {} chars (limit: {})", length, limit)
                            }
                        };
                        self.sm
                            .observations
                            .push(KernelObservation::MemoryValidationFailed {
                                turn,
                                memory_id: memory.metadata.name.clone(),
                                error: error_msg,
                            });
                        return identity.empty(self.sm.take_observations());
                    }
                }
            }
            KernelInputEvent::QueryMemory { query } => {
                // Phase 7: Query memory for context.
                // Kernel emits observation; SDK responds asynchronously.
                // An installed policy caps retrieval breadth: requested_k = min(query.top_k, policy).
                let requested_k = match self.sm.memory_policy() {
                    Some(p) => p.clamp_top_k(query.top_k),
                    None => query.top_k,
                };
                self.pending_memory_query = Some((query.clone(), requested_k));
                LoopAction::QueryMemory { query, requested_k }
            }
            KernelInputEvent::Timeout => self.sm.feed(LoopEvent::Timeout),
        };
        let action = self.sm.externalize_pending_host_effect(action);
        if matches!(action, LoopAction::AwaitingResume) {
            return identity.empty(self.sm.take_observations());
        }
        identity.single(action, self.sm.take_observations())
    }

    fn lifecycle_transition(
        &self,
        event: &KernelInputEvent,
    ) -> Result<LifecycleTransition, String> {
        if self.lifecycle.is_terminal() {
            return Err(format!(
                "kernel is terminal in lifecycle {:?}",
                self.lifecycle
            ));
        }

        match event {
            KernelInputEvent::ConfigureRun { .. } => match self.lifecycle {
                KernelLifecycle::Created | KernelLifecycle::Configured => {
                    Ok(LifecycleTransition::Configure)
                }
                _ => Err(format!(
                    "configure_run is not valid in lifecycle {:?}",
                    self.lifecycle
                )),
            },
            KernelInputEvent::StartRun { .. } => match self.lifecycle {
                KernelLifecycle::Created | KernelLifecycle::Configured => {
                    Ok(LifecycleTransition::Start)
                }
                _ => Err(format!(
                    "start_run is not valid in lifecycle {:?}",
                    self.lifecycle
                )),
            },
            KernelInputEvent::Resume => match self.lifecycle {
                KernelLifecycle::Configured => Ok(LifecycleTransition::Resume),
                _ => Err(format!(
                    "resume is not valid in lifecycle {:?}",
                    self.lifecycle
                )),
            },
            KernelInputEvent::ApprovalResult { .. }
            | KernelInputEvent::WorkflowSpawnResult { .. }
            | KernelInputEvent::PreemptResult { .. } => match self.lifecycle {
                KernelLifecycle::Suspended => Ok(LifecycleTransition::Resume),
                _ => Err(format!(
                    "effect result is not valid in lifecycle {:?}",
                    self.lifecycle
                )),
            },
            KernelInputEvent::MemoryPersistResult { .. }
            | KernelInputEvent::MemoryQueryResult { .. }
            | KernelInputEvent::LargeResultSpoolResult { .. }
            | KernelInputEvent::PageOutArchiveResult { .. } => match self.lifecycle {
                KernelLifecycle::Configured | KernelLifecycle::Running => {
                    Ok(LifecycleTransition::Stay)
                }
                _ => Err(format!(
                    "memory effect result is not valid in lifecycle {:?}",
                    self.lifecycle
                )),
            },
            KernelInputEvent::ProviderResult { .. }
            | KernelInputEvent::ProviderError { .. }
            | KernelInputEvent::ToolResults { .. }
            | KernelInputEvent::MilestoneResult { .. }
            | KernelInputEvent::LoadWorkflow { .. }
            | KernelInputEvent::SpawnSubAgent { .. } => match self.lifecycle {
                KernelLifecycle::Running => Ok(LifecycleTransition::Stay),
                _ => Err(format!(
                    "execution input is not valid in lifecycle {:?}",
                    self.lifecycle
                )),
            },
            KernelInputEvent::SubmitWorkflow { .. }
            | KernelInputEvent::SubmitWorkflowNodes { .. }
            | KernelInputEvent::Signal { .. }
            | KernelInputEvent::Timeout => match self.lifecycle {
                KernelLifecycle::Running | KernelLifecycle::Suspended => {
                    Ok(LifecycleTransition::Stay)
                }
                _ => Err(format!(
                    "execution input is not valid in lifecycle {:?}",
                    self.lifecycle
                )),
            },
            KernelInputEvent::SubAgentCompleted { .. } => match self.lifecycle {
                KernelLifecycle::Running | KernelLifecycle::Suspended => {
                    Ok(LifecycleTransition::Resume)
                }
                _ => Err(format!(
                    "sub_agent_completed is not valid in lifecycle {:?}",
                    self.lifecycle
                )),
            },
            _ => match self.lifecycle {
                KernelLifecycle::Suspended => Err(
                    "only resume, sub_agent_completed, or cancellation is valid while suspended"
                        .to_string(),
                ),
                KernelLifecycle::Created => Ok(LifecycleTransition::Configure),
                _ => Ok(LifecycleTransition::Stay),
            },
        }
    }

    fn advance_lifecycle(&mut self, transition: LifecycleTransition, step: &KernelStep) {
        if let Some(result) = step.actions.iter().find_map(|action| match &action.effect {
            KernelEffect::Done { result } => Some(result),
            _ => None,
        }) {
            self.lifecycle = match result.termination {
                crate::types::result::TerminationReason::Completed => KernelLifecycle::Completed,
                crate::types::result::TerminationReason::UserAbort => KernelLifecycle::Cancelled,
                _ => KernelLifecycle::Failed,
            };
            return;
        }

        if self.sm.is_suspended() {
            self.lifecycle = KernelLifecycle::Suspended;
            return;
        }

        self.lifecycle = match transition {
            LifecycleTransition::Configure if self.lifecycle == KernelLifecycle::Created => {
                KernelLifecycle::Configured
            }
            LifecycleTransition::Start | LifecycleTransition::Resume => KernelLifecycle::Running,
            _ => self.lifecycle,
        };
    }

    fn allocate_step_seq(&mut self) -> u64 {
        let step_seq = self.next_step_seq;
        self.next_step_seq = self.next_step_seq.saturating_add(1);
        step_seq
    }

    fn fault_step(
        &mut self,
        operation_id: String,
        event_id: String,
        code: KernelFaultCode,
        message: String,
        effect_id: Option<String>,
    ) -> KernelStep {
        let step_seq = self.allocate_step_seq();
        KernelStep::fault(
            operation_id.clone(),
            event_id.clone(),
            step_seq,
            KernelFault {
                code,
                message,
                operation_id: Some(operation_id),
                event_id: Some(event_id),
                effect_id,
            },
        )
    }
}

fn result_effect(event: &KernelInputEvent) -> Option<(&str, PendingEffectKind)> {
    match event {
        KernelInputEvent::ProviderResult { effect_id, .. }
        | KernelInputEvent::ProviderError { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::Provider))
        }
        KernelInputEvent::ToolResults { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::Tool))
        }
        KernelInputEvent::MilestoneResult { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::Milestone))
        }
        KernelInputEvent::ApprovalResult { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::Approval))
        }
        KernelInputEvent::WorkflowSpawnResult { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::WorkflowSpawn))
        }
        KernelInputEvent::PreemptResult { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::Preempt))
        }
        KernelInputEvent::MemoryPersistResult { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::MemoryPersist))
        }
        KernelInputEvent::MemoryQueryResult { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::MemoryQuery))
        }
        KernelInputEvent::LargeResultSpoolResult { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::LargeResultSpool))
        }
        KernelInputEvent::PageOutArchiveResult { effect_id, .. } => {
            Some((effect_id, PendingEffectKind::PageOutArchive))
        }
        _ => None,
    }
}

fn pending_effect_kind(effect: &KernelEffect) -> Option<PendingEffectKind> {
    match effect {
        KernelEffect::CallProvider { .. } => Some(PendingEffectKind::Provider),
        KernelEffect::ExecuteTool { .. } => Some(PendingEffectKind::Tool),
        KernelEffect::EvaluateMilestone { .. } => Some(PendingEffectKind::Milestone),
        KernelEffect::RequestApproval { .. } => Some(PendingEffectKind::Approval),
        KernelEffect::SpawnWorkflow { .. } => Some(PendingEffectKind::WorkflowSpawn),
        KernelEffect::PreemptSubAgents { .. } => Some(PendingEffectKind::Preempt),
        KernelEffect::PersistMemory { .. } => Some(PendingEffectKind::MemoryPersist),
        KernelEffect::QueryMemory { .. } => Some(PendingEffectKind::MemoryQuery),
        KernelEffect::SpoolLargeResult { .. } => Some(PendingEffectKind::LargeResultSpool),
        KernelEffect::ArchivePageOut { .. } => Some(PendingEffectKind::PageOutArchive),
        KernelEffect::Done { .. } => None,
    }
}

fn validate_run_config(config: &RunConfig) -> Result<(), String> {
    if let Some(reliability) = &config.reliability {
        for (name, capacity) in [
            ("event_replay_capacity", reliability.event_replay_capacity),
            (
                "completed_effect_replay_capacity",
                reliability.completed_effect_replay_capacity,
            ),
        ] {
            if let Some(capacity) = capacity {
                if !(1..=65_536).contains(&capacity) {
                    return Err(format!("{name} must be between 1 and 65536"));
                }
            }
        }
        if reliability.provider_recovery_attempts.is_some_and(|value| value > 16) {
            return Err("provider_recovery_attempts must be at most 16".to_string());
        }
        if reliability.output_recovery_attempts.is_some_and(|value| value > 16) {
            return Err("output_recovery_attempts must be at most 16".to_string());
        }
        if reliability.host_effect_retry_attempts.is_some_and(|value| value > 16) {
            return Err("host_effect_retry_attempts must be at most 16".to_string());
        }
        let threshold = reliability
            .spool_threshold_bytes
            .unwrap_or(50 * 1024);
        let preview = reliability.spool_preview_bytes.unwrap_or(2 * 1024);
        if threshold == 0 {
            return Err("spool_threshold_bytes must be greater than zero".to_string());
        }
        if preview == 0 || preview > threshold {
            return Err(
                "spool_preview_bytes must be greater than zero and no larger than spool_threshold_bytes"
                    .to_string(),
            );
        }
    }
    if matches!(config.attention_max_queue_size, Some(0)) {
        return Err("attention_max_queue_size must be greater than zero".to_string());
    }
    if matches!(config.scheduler_max_wall_ms, Some(0)) {
        return Err("scheduler_max_wall_ms must be greater than zero".to_string());
    }
    if let Some(ratio) = config.knowledge_budget_ratio {
        if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
            return Err("knowledge_budget_ratio must be finite and between 0 and 1".to_string());
        }
    }
    if let Some(fuse) = config.repeat_fuse {
        if fuse.enabled
            && fuse.deny_after > 0
            && fuse.terminate_after > 0
            && fuse.deny_after >= fuse.terminate_after
        {
            return Err("repeat_fuse terminate_after must be greater than deny_after".to_string());
        }
    }
    if let Some(watch) = config.entropy_watch {
        if !watch.threshold.is_finite() || !(0.0..=1.0).contains(&watch.threshold) {
            return Err("entropy_watch threshold must be finite and between 0 and 1".to_string());
        }
        if !watch.hysteresis.is_finite()
            || watch.hysteresis < 0.0
            || watch.hysteresis > watch.threshold
        {
            return Err(
                "entropy_watch hysteresis must be finite and no greater than threshold".to_string(),
            );
        }
    }
    if let Some(quota) = &config.resource_quota {
        if matches!(quota.memory_writes_per_window, Some((_, 0))) {
            return Err("memory write quota window must be greater than zero".to_string());
        }
    }
    if let Some(governance) = &config.governance {
        if governance
            .rate_limits
            .iter()
            .any(|limit| limit.window_ms == 0)
        {
            return Err("governance rate-limit windows must be greater than zero".to_string());
        }
        for constraint in &governance.constraints {
            if let ConstraintSpec::Range {
                min: Some(min),
                max: Some(max),
                ..
            } = constraint
            {
                if !min.is_finite() || !max.is_finite() || min > max {
                    return Err(
                        "governance range constraints require finite min <= max".to_string()
                    );
                }
            }
        }
    }
    Ok(())
}
