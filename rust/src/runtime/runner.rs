use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::runtime::sandboxed_skill::scan_skill_dir;
use crate::runtime::skill_watcher::SkillWatcher;
use async_stream::try_stream;
use deepstrike_core::governance::quota::ResourceQuota;
use deepstrike_core::memory::idle_pipeline::{IdleAction, IdleEvent, IdlePipeline, IdlePolicy};
use deepstrike_core::mm::memory::{MemoryQuery, MemoryRetrieval, MemoryWriteRequest};
use deepstrike_core::runtime::kernel::{
    KernelAction, KernelInput, KernelInputEvent, KernelObservation, KernelPressureAction,
    KernelRuntime, KernelStep,
};
use deepstrike_core::runtime::event_log::{category_for_kind, primitive_for_kind};
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::signals::router::SignalRouter;
use deepstrike_core::types::message::{Message, ToolCall};
use deepstrike_core::types::milestone::MilestoneCheckResult;
use deepstrike_core::types::policy::SignalDisposition;
use deepstrike_core::types::signal::{
    RuntimeSignal as KernelSignal, SignalSource as KernelSignalSource,
    SignalType as KernelSignalType, Urgency,
};
use deepstrike_core::types::task::RuntimeTask;
use futures::StreamExt;

use crate::SignalSource;
use crate::governance::Governance;
use crate::knowledge::KnowledgeSource;
use crate::memory::{DreamResult, DreamStore};
use crate::providers::{LLMProvider, StreamEvent};
use crate::run_event::RunEvent;
use crate::runtime::archive::ArchiveStore;
use crate::runtime::execution_plane::{
    ExecutionPlane, LocalExecutionPlane, PermissionRequestHandler, RunContext, ToolSuspendHandler,
};
use crate::runtime::os_profile::{
    assert_native_profile, AttentionPolicy, GovernancePolicy, OsProfile, SchedulerBudget,
};
use crate::runtime::provider_replay::{peek_provider_replay, seed_provider_replay_from_events};
use crate::runtime::replay::{
    is_mid_run, repair_entries_with_cap, replay_messages_with_cap,
    replay_messages_with_cap_and_loader,
};
use crate::runtime::session_log::{SessionEntry, SessionLog};
use crate::{Error, Result};
use deepstrike_core::context::task_state::TaskUpdate;
use deepstrike_core::runtime::repair::repair_llm_completed;

/// Controls what the runner does when the state machine returns
/// `EvaluateMilestone` — i.e., the LLM finished a turn but a milestone phase
/// has not yet been evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MilestonePolicy {
    /// Wait for a verifier callback or suspend if none is configured (default).
    #[default]
    RequireVerifier,
    /// Terminate the run immediately with `status = "milestone_pending"`.
    Terminate,
    /// Unconditionally pass every milestone phase.  Useful in unit tests and
    /// capability-unlock–only scenarios where the criteria check is a no-op.
    AutoPass,
}

#[derive(Debug, Clone)]
pub struct MilestoneEvaluationContext {
    pub phase_id: String,
    pub criteria: Vec<String>,
    pub required_evidence: Vec<String>,
}

pub type MilestoneEvaluationHandler = std::sync::Arc<
    dyn Fn(
            MilestoneEvaluationContext,
        ) -> futures::future::BoxFuture<'static, Result<MilestoneCheckResult>>
        + Send
        + Sync,
>;

/// Configuration for a `RuntimeRunner` (aligned with Node/Python `RuntimeOptions`).
pub struct RuntimeOptions {
    pub provider: Box<dyn LLMProvider>,
    pub execution_plane: Option<Box<dyn ExecutionPlane>>,
    pub session_log: Option<Arc<dyn SessionLog>>,
    pub compression_store: Option<Arc<dyn ArchiveStore>>,
    /// When set, `execute` reuses this session id.
    pub session_id: Option<String>,
    pub max_tokens: u32,
    pub max_turns: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub extensions: Option<serde_json::Value>,
    pub agent_id: Option<String>,
    pub system_prompt: Option<String>,
    pub initial_memory: Vec<String>,
    pub skill_dir: Option<std::path::PathBuf>,
    pub dream_store: Option<Box<dyn DreamStore>>,
    pub knowledge_source: Option<Box<dyn KnowledgeSource>>,
    pub signal_source: Option<Box<dyn SignalSource>>,
    pub governance: Option<Arc<tokio::sync::Mutex<Governance>>>,
    pub os_profile: Option<OsProfile>,
    pub governance_policy: Option<GovernancePolicy>,
    pub attention_policy: Option<AttentionPolicy>,
    pub scheduler_budget: Option<SchedulerBudget>,
    pub resource_quota: Option<ResourceQuota>,
    pub tokenizer: Option<String>,
    pub enable_plan_tool: Option<bool>,
    pub on_tool_suspend: Option<ToolSuspendHandler>,
    pub on_permission_request: Option<PermissionRequestHandler>,
    /// How to handle `EvaluateMilestone` actions. Default: `RequireVerifier`.
    pub milestone_policy: MilestonePolicy,
    pub milestone_contract: Option<deepstrike_core::types::milestone::MilestoneContract>,
    pub run_spec: Option<deepstrike_core::types::agent::AgentRunSpec>,
    pub on_milestone_evaluate: Option<MilestoneEvaluationHandler>,
}

/// Orchestrates the agentic turn loop via the runtime kernel + session event log.
pub struct RuntimeRunner {
    opts: RuntimeOptions,
    plane: Box<dyn ExecutionPlane>,
    interrupted: AtomicBool,
    active_kernel: std::sync::Mutex<Option<std::sync::Arc<std::sync::Mutex<KernelRuntime>>>>,
    local_page_out_cache: std::sync::Mutex<Vec<Message>>,
}

impl RuntimeRunner {
    pub fn new(mut opts: RuntimeOptions) -> Self {
        let plane = opts
            .execution_plane
            .take()
            .unwrap_or_else(|| Box::new(LocalExecutionPlane::new()));
        Self {
            opts,
            plane,
            interrupted: AtomicBool::new(false),
            active_kernel: std::sync::Mutex::new(None),
            local_page_out_cache: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Relaxed);
    }

    pub fn execution_plane(&self) -> &dyn ExecutionPlane {
        self.plane.as_ref()
    }

    pub async fn write_memory(
        &self,
        memory: MemoryWriteRequest,
        session_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let Some(store) = &self.opts.dream_store else {
            return Ok(());
        };
        let Some(agent_id) = agent_id.or(self.opts.agent_id.as_deref()) else {
            return Ok(());
        };

        let observations = self.apply_memory_syscall(KernelInputEvent::WriteMemory {
            memory: memory.clone(),
        });
        if observations
            .iter()
            .any(|obs| matches!(obs, KernelObservation::MemoryWritten { .. }))
        {
            let existing = store.load_memories(agent_id).await?;
            let mut metadata =
                serde_json::to_value(&memory.metadata).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = metadata.as_object_mut() {
                obj.insert(
                    "source".to_string(),
                    serde_json::Value::String("write_memory_syscall".to_string()),
                );
            }
            let result = deepstrike_core::memory::curator::CurationResult {
                to_add: vec![deepstrike_core::memory::semantic::MemoryEntry {
                    text: memory.content,
                    score: 1.0,
                    metadata,
                }],
                to_remove_indices: vec![],
                stats: deepstrike_core::memory::curator::CurationStats {
                    insights_processed: 1,
                    duplicates_removed: 0,
                    conflicts_resolved: 0,
                    entries_added: 1,
                },
            };
            store.commit(agent_id, result, &existing).await?;
        }
        self.append_memory_syscall_observations(session_id, observations)
            .await;
        Ok(())
    }

    pub async fn query_memory(
        &self,
        query: MemoryQuery,
        session_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Vec<deepstrike_core::memory::semantic::MemoryEntry>> {
        let Some(store) = &self.opts.dream_store else {
            return Ok(Vec::new());
        };
        let Some(agent_id) = agent_id.or(self.opts.agent_id.as_deref()) else {
            return Ok(Vec::new());
        };

        let observations = self.apply_memory_syscall(KernelInputEvent::QueryMemory {
            query: query.clone(),
        });

        let all_memories = store.load_memories(agent_id).await?;
        let mut retrieval = select_memories(&query, &all_memories);
        let hits = if !retrieval.selected_memory_ids.is_empty() {
            let selected: std::collections::HashSet<_> =
                retrieval.selected_memory_ids.iter().cloned().collect();
            all_memories
                .into_iter()
                .filter(|entry| {
                    entry
                        .metadata
                        .get("name")
                        .and_then(|value| value.as_str())
                        .is_some_and(|name| selected.contains(name))
                })
                .take(query.top_k)
                .collect()
        } else {
            let hits = store
                .search(agent_id, &query.current_context, query.top_k)
                .await?;
            if !hits.is_empty()
                && retrieval.selection_rationale == "No candidates after filtering"
            {
                retrieval.selected_memory_ids = hits
                    .iter()
                    .filter_map(|entry| {
                        entry
                            .metadata
                            .get("name")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    })
                    .collect();
                retrieval.selection_rationale =
                    format!("DreamStore.search returned {} hit(s)", hits.len());
            }
            hits
        };

        self.append_memory_syscall_observations(session_id, observations)
            .await;
        self.log_memory_retrieval_result(session_id, retrieval).await;
        Ok(hits)
    }

    async fn log_memory_retrieval_result(
        &self,
        session_id: Option<&str>,
        retrieval: MemoryRetrieval,
    ) {
        let Some(session_id) = session_id.or(self.opts.session_id.as_deref()) else {
            return;
        };
        self.log(
            session_id,
            SessionEvent::MemoryRetrievalResult {
                retrieval: retrieval.clone(),
            },
        )
        .await;
        self.apply_memory_syscall(KernelInputEvent::MemoryRetrievalResult { retrieval });
    }

    fn apply_memory_syscall(&self, event: KernelInputEvent) -> Vec<KernelObservation> {
        if let Some(active) = self.active_kernel.lock().unwrap().clone() {
            let mut kernel = active.lock().unwrap();
            let step = kernel.step(KernelInput::new(event));
            return step.observations;
        }

        let mut kernel = KernelRuntime::new(LoopPolicy {
            max_tokens: self.opts.max_tokens,
            max_turns: self.opts.max_turns.unwrap_or(25),
            max_wall_ms: effective_wall_budget(self.opts.scheduler_budget, self.opts.timeout_ms),
            ..Default::default()
        });
        if let Ok(profile) = assert_native_profile(self.opts.os_profile.clone()) {
            kernel.step(KernelInput::new(
                self.opts
                    .governance_policy
                    .clone()
                    .unwrap_or(profile.governance_policy)
                    .into_kernel_event(),
            ));
            let attention = self.opts.attention_policy.unwrap_or(profile.attention_policy);
            kernel.step(KernelInput::new(KernelInputEvent::SetAttentionPolicy {
                max_queue_size: attention.max_queue_size.unwrap_or(64),
            }));
        }
        if let Some(max_wall_ms) = effective_wall_budget(self.opts.scheduler_budget, self.opts.timeout_ms) {
            kernel.step(KernelInput::new(KernelInputEvent::SetSchedulerBudget {
                max_wall_ms: Some(max_wall_ms),
            }));
        }
        if let Some(quota) = self.opts.resource_quota.clone() {
            kernel.step(KernelInput::new(KernelInputEvent::SetResourceQuota { quota }));
        }
        let step = kernel.step(KernelInput::new(event));
        step.observations
    }

    async fn append_memory_syscall_observations(
        &self,
        session_id: Option<&str>,
        observations: Vec<KernelObservation>,
    ) {
        let Some(session_id) = session_id.or(self.opts.session_id.as_deref()) else {
            return;
        };
        for obs in observations {
            match obs {
                KernelObservation::MemoryWritten {
                    turn,
                    memory_id,
                    memory_kind,
                    size_bytes,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryWritten {
                            turn,
                            category: Some(category_for_kind("memory_written")),
                            primitive: Some(primitive_for_kind("memory_written")),
                            memory_id,
                            memory_kind,
                            size_bytes,
                        },
                    )
                    .await;
                }
                KernelObservation::MemoryQueried {
                    turn,
                    query_context,
                    requested_k,
                    requires_async_response,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryQueried {
                            turn,
                            category: Some(category_for_kind("memory_queried")),
                            primitive: Some(primitive_for_kind("memory_queried")),
                            query_context,
                            requested_k,
                            requires_async_response,
                        },
                    )
                    .await;
                }
                KernelObservation::MemoryValidationFailed {
                    turn,
                    memory_id,
                    error,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryValidationFailed {
                            turn,
                            category: Some(category_for_kind("memory_validation_failed")),
                            primitive: Some(primitive_for_kind("memory_validation_failed")),
                            memory_id,
                            error,
                        },
                    )
                    .await;
                }
                _ => {}
            }
        }
    }

    pub async fn execute(&self, goal: &str) -> Result<String> {
        collect_text(self.run_streaming(goal, &[], None, None).await?).await
    }

    pub async fn execute_with_criteria(&self, goal: &str, criteria: &[String]) -> Result<String> {
        collect_text(self.run_streaming(goal, criteria, None, None).await?).await
    }

    pub async fn run_streaming<'a>(
        &'a self,
        goal: &'a str,
        criteria: &'a [String],
        extensions: Option<&'a serde_json::Value>,
        session_id: Option<&'a str>,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + 'a>>> {
        let session_id = session_id
            .map(str::to_string)
            .or_else(|| self.opts.session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let prior = self.read_entries(&session_id).await?;
        let mid_run = is_mid_run(&prior);

        if !mid_run {
            self.log(
                &session_id,
                SessionEvent::RunStarted {
                    run_id: uuid::Uuid::new_v4().to_string(),
                    goal: goal.to_string(),
                    criteria: criteria.to_vec(),
                    agent_id: self.opts.agent_id.clone(),
                    system_prompt: self.opts.system_prompt.clone(),
                },
            )
            .await;
        }

        let goal_owned = goal.to_string();
        let criteria_owned = criteria.to_vec();
        let extensions_owned = extensions.cloned();
        let prior_events = if prior.is_empty() { None } else { Some(prior) };

        Ok(Box::pin(self.execute_inner(
            session_id,
            goal_owned,
            criteria_owned,
            extensions_owned,
            prior_events,
            mid_run,
        )))
    }

    pub async fn wake_streaming(
        &self,
        session_id: &str,
        extensions: Option<&serde_json::Value>,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + '_>>> {
        let prior = self.read_entries(session_id).await?;
        if prior
            .iter()
            .any(|e| matches!(e.event, SessionEvent::RunTerminal { .. }))
        {
            return Ok(Box::pin(futures::stream::empty()));
        }
        let start = prior
            .iter()
            .rev()
            .find(|e| matches!(e.event, SessionEvent::RunStarted { .. }))
            .ok_or_else(|| Error::Other(format!("no run_started for session: {session_id}")))?;

        let (goal, criteria) = match &start.event {
            SessionEvent::RunStarted { goal, criteria, .. } => (goal.clone(), criteria.clone()),
            _ => unreachable!(),
        };

        Ok(Box::pin(self.execute_inner(
            session_id.to_string(),
            goal,
            criteria,
            extensions.cloned(),
            Some(prior),
            true,
        )))
    }

    pub async fn wake(&self, session_id: &str) -> Result<String> {
        collect_text(self.wake_streaming(session_id, None).await?).await
    }

    pub async fn dream(&self, agent_id: &str, now_ms: u64) -> Result<DreamResult> {
        let store = self
            .opts
            .dream_store
            .as_ref()
            .ok_or_else(|| Error::Other("dream_store not configured".into()))?;

        let sessions = store.load_sessions(agent_id).await?;
        let existing_memories = store.load_memories(agent_id).await?;
        if sessions.is_empty() {
            return Ok(DreamResult::default());
        }

        let policy = IdlePolicy::new(agent_id);
        let mut pipeline = IdlePipeline::new(policy);

        let messages = match pipeline.feed(IdleEvent::Trigger {
            sessions,
            existing_memories: existing_memories.clone(),
            now_ms,
        }) {
            IdleAction::SynthesizeInsights { messages } => messages,
            IdleAction::Noop => return Ok(DreamResult::default()),
            _ => {
                return Err(Error::Other(
                    "unexpected IdlePipeline::Trigger action".into(),
                ));
            }
        };

        let mut synthesis_text = String::new();
        let context = rendered_context_from_messages(messages);
        let synth_state = self.opts.provider.create_run_state();
        let mut stream = self
            .opts
            .provider
            .stream(&context, &[], None, synth_state.as_ref())
            .await?;
        while let Some(evt) = stream.next().await {
            if let Ok(StreamEvent::TextDelta { delta }) = evt {
                synthesis_text.push_str(&delta);
            }
        }

        let (curation_result, run_result) = match pipeline.feed(IdleEvent::SynthesisResult {
            content: synthesis_text,
        }) {
            IdleAction::CommitMemories {
                result, run_result, ..
            } => (result, run_result),
            _ => {
                return Err(Error::Other(
                    "unexpected IdlePipeline::SynthesisResult action".into(),
                ));
            }
        };

        let entries_added = curation_result.stats.entries_added;
        let entries_removed = curation_result.to_remove_indices.len();
        store
            .commit(agent_id, curation_result, &existing_memories)
            .await?;

        Ok(DreamResult {
            sessions_processed: run_result.sessions_processed,
            insights_extracted: run_result.insights_extracted,
            entries_added,
            entries_removed,
        })
    }

    fn execute_inner(
        &self,
        session_id: String,
        goal: String,
        criteria: Vec<String>,
        extensions: Option<serde_json::Value>,
        prior_events: Option<Vec<SessionEntry>>,
        resume_mid_run: bool,
    ) -> impl futures::Stream<Item = Result<RunEvent>> + '_ {
        try_stream! {
            self.interrupted.store(false, Ordering::Relaxed);

            if let Some(ks) = &self.opts.knowledge_source {
                ks.init().await?;
            }

            let provider_policy = self.opts.provider.runtime_policy();
            let effective_max_turns = self.opts.max_turns.or(provider_policy.max_turns).unwrap_or(25);
            let effective_timeout = self.opts.timeout_ms.or(provider_policy.timeout_ms);
            let effective_wall_budget = effective_wall_budget(self.opts.scheduler_budget, effective_timeout);

            let policy = LoopPolicy {
                max_tokens: self.opts.max_tokens,
                max_turns: effective_max_turns,
                max_wall_ms: effective_wall_budget,
                ..Default::default()
            };

            let mut kernel = std::sync::Arc::new(std::sync::Mutex::new(KernelRuntime::new(policy)));
            {
                let mut active = self.active_kernel.lock().unwrap();
                *active = Some(kernel.clone());
            }

            struct ActiveKernelGuard<'a> {
                runner: &'a RuntimeRunner,
            }
            impl<'a> Drop for ActiveKernelGuard<'a> {
                fn drop(&mut self) {
                    if let Ok(mut active) = self.runner.active_kernel.lock() {
                        *active = None;
                    }
                }
            }
            let _guard = ActiveKernelGuard { runner: self };

            let kernel_apply = |kernel_arc: &mut std::sync::Arc<std::sync::Mutex<KernelRuntime>>, pending: &mut Vec<KernelObservation>, event| {
                kernel_apply(&mut *kernel_arc.lock().unwrap(), pending, event)
            };
            let kernel_action = |kernel_arc: &mut std::sync::Arc<std::sync::Mutex<KernelRuntime>>, pending: &mut Vec<KernelObservation>, event| {
                kernel_action(&mut *kernel_arc.lock().unwrap(), pending, event)
            };

            let mut pending_observations = Vec::new();
            let mut pending_spool_outputs: std::collections::HashMap<String, (String, String)> =
                std::collections::HashMap::new();

            if let Some(tokenizer_name) = &self.opts.tokenizer {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetTokenizer {
                        name: tokenizer_name.clone(),
                    },
                );
            }
            if let Some(enabled) = self.opts.enable_plan_tool {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetPlanToolEnabled { enabled },
                );
            }

            kernel_apply(
                &mut kernel,
                &mut pending_observations,
                KernelInputEvent::SetTools {
                    tools: self.plane.schemas(),
                },
            );

            if self.opts.dream_store.is_some() && self.opts.agent_id.is_some() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetMemoryEnabled { enabled: true },
                );
            }
            if self.opts.knowledge_source.is_some() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetKnowledgeEnabled { enabled: true },
                );
            }

            if let Some(sp) = &self.opts.system_prompt {
                let tokens = ((sp.len() / 4) as u32).max(1);
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::AddSystemMessage {
                        content: sp.clone(),
                        tokens,
                    },
                );
            }
            for mem in &self.opts.initial_memory {
                let tokens = ((mem.len() / 4) as u32).max(1);
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::AddKnowledgeMessage {
                        content: mem.clone(),
                        tokens,
                    },
                );
            }

            let skill_watcher = self.opts.skill_dir.as_deref().and_then(SkillWatcher::start);
            if let Some(skill_dir) = &self.opts.skill_dir {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetAvailableSkills {
                        skills: scan_skill_dir(skill_dir),
                    },
                );
            }

            if let Some(milestones) = self.opts.milestone_contract.clone() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::LoadMilestoneContract { contract: milestones },
                );
            }

            let recovery_tokens = {
                let k = kernel.lock().unwrap();
                k.state_machine()
                    .ctx
                    .config
                    .recovery_content_tokens(k.state_machine().ctx.max_tokens)
            };
            let max_bytes = {
                let k = kernel.lock().unwrap();
                k.state_machine()
                    .ctx
                    .engine
                    .token_budget_to_bytes(recovery_tokens)
            };

            if let Some(ref events) = prior_events {
                let repaired = repair_entries_with_cap(events, max_bytes);
                seed_provider_replay_from_events(self.opts.provider.as_ref(), &repaired);

                let messages = if let Some(ref store) = self.opts.compression_store {
                    let store_clone = store.clone();
                    replay_messages_with_cap_and_loader(&repaired, max_bytes, move |archive_ref| {
                        store_clone.read(archive_ref).map_err(|_| {
                            deepstrike_core::context::snapshot::ContextFault::MissingArchive {
                                session_id: String::new(),
                                seq: 0,
                            }
                        })
                    })
                } else {
                    replay_messages_with_cap(&repaired, max_bytes)
                };

                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::PreloadHistory {
                        messages,
                    },
                );
            }

            let ext = merge_extensions(self.opts.extensions.as_ref(), extensions.as_ref());
            let provider_state = self.opts.provider.create_run_state();
            let mut router = SignalRouter::new(256);
            let mut next_archive_start = next_archived_seq_start(prior_events.as_deref());
            let mut has_attempted_reactive_compact = false;
            let session_start_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let os_profile = assert_native_profile(self.opts.os_profile.clone())?;
            let governance_policy = self
                .opts
                .governance_policy
                .clone()
                .unwrap_or(os_profile.governance_policy);
            kernel_apply(
                &mut kernel,
                &mut pending_observations,
                governance_policy.into_kernel_event(),
            );

            let attention_policy = self
                .opts
                .attention_policy
                .unwrap_or(os_profile.attention_policy);
            kernel_apply(
                &mut kernel,
                &mut pending_observations,
                KernelInputEvent::SetAttentionPolicy {
                    max_queue_size: attention_policy.max_queue_size.unwrap_or(64),
                },
            );

            if let Some(max_wall_ms) = effective_wall_budget {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetSchedulerBudget {
                        max_wall_ms: Some(max_wall_ms),
                    },
                );
            }

            if let Some(quota) = self.opts.resource_quota.clone() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetResourceQuota { quota },
                );
            }

            let mut action = if resume_mid_run {
                kernel_action(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::Resume {
                        approved_calls: vec![],
                        denied_calls: vec![],
                    },
                )
            } else {
                kernel_action(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::StartRun {
                        task: RuntimeTask::new(&goal).with_criteria(criteria),
                        run_spec: self.opts.run_spec.clone(),
                    },
                )
            };

            let mut last_skill_version: u64 = skill_watcher.as_ref().map(|w| w.version()).unwrap_or(0);

            while !kernel.lock().unwrap().is_terminal() {
                if let KernelAction::ExecuteTool { .. } = &action {
                    self.apply_kernel_page_in(
                        &session_id,
                        &kernel,
                        &mut pending_observations,
                    )
                    .await;
                }

                // Hot-reload: refresh skill catalog if the watcher detected changes.
                if let (Some(watcher), Some(skill_dir)) =
                    (&skill_watcher, &self.opts.skill_dir)
                {
                    let cur = watcher.version();
                    if cur != last_skill_version {
                        last_skill_version = cur;
                        kernel_apply(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::SetAvailableSkills {
                                skills: scan_skill_dir(skill_dir),
                            },
                        );
                    }
                }

                next_archive_start = self
                    .append_observations(
                        &session_id,
                        &kernel,
                        &mut pending_observations,
                        &mut pending_spool_outputs,
                        next_archive_start,
                    )
                    .await;

                if self.interrupted.load(Ordering::Relaxed) {
                    kernel_apply(
                        &mut kernel,
                        &mut pending_observations,
                        KernelInputEvent::Timeout,
                    );
                    break;
                }

                if let Some(ss) = &self.opts.signal_source {
                    if let Some(sdk_sig) = ss.next_signal().await? {
                        let urgency = match sdk_sig.kind.as_str() {
                            "interrupt" => Urgency::Critical,
                            _ => Urgency::Normal,
                        };
                        let kernel_sig = KernelSignal::new(
                            KernelSignalSource::Custom,
                            KernelSignalType::Event,
                            urgency,
                            sdk_sig.kind.as_str(),
                        )
                        .with_payload(sdk_sig.payload.clone())
                        .with_timestamp(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                        );
                        let executing = matches!(action, KernelAction::ExecuteTool { .. });
                        match router.ingest(kernel_sig, executing) {
                            SignalDisposition::InterruptNow | SignalDisposition::Interrupt => {
                                kernel_apply(
                                    &mut kernel,
                                    &mut pending_observations,
                                    KernelInputEvent::Timeout,
                                );
                                break;
                            }
                            _ => {}
                        }
                    }
                }

                let mut queued = router.next();
                while let Some(sig) = queued {
                    if sig.urgency == Urgency::Critical {
                        kernel_apply(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::Timeout,
                        );
                        break;
                    }
                    queued = router.next();
                }
                if kernel.lock().unwrap().is_terminal() {
                    break;
                }

                match &action {
                    KernelAction::CallProvider { context, tools } => {
                        let mut final_text = String::new();
                        let mut final_tool_calls: Vec<ToolCall> = Vec::new();
                        let mut turn_tokens: u32 = 0;

                        let mut provider_stream = match self
                            .opts
                            .provider
                            .stream(context, tools, ext.as_ref(), provider_state.as_ref())
                            .await
                        {
                            Ok(s) => s,
                            Err(e) => {
                                if is_prompt_too_long_error(&e)
                                    && !has_attempted_reactive_compact
                                {
                                    has_attempted_reactive_compact = true;
                                    let compact_step = kernel.lock().unwrap().step(KernelInput::new(
                                        KernelInputEvent::ForceCompact,
                                    ));
                                    let compacted = compact_step.observations.iter().any(|obs| {
                                        matches!(obs, KernelObservation::Compressed { .. })
                                    });
                                    pending_observations.extend(compact_step.observations);
                                    if compacted {
                                        next_archive_start = self
                                            .append_observations(
                                                &session_id,
                                                &kernel,
                                                &mut pending_observations,
                                                &mut pending_spool_outputs,
                                                next_archive_start,
                                            )
                                            .await;
                                        action = KernelAction::CallProvider {
                                            context: kernel.lock().unwrap().state_machine().ctx.render(),
                                            tools: tools.clone(),
                                        };
                                        continue;
                                    }
                                }
                                yield RunEvent::Error(e.to_string());
                                kernel_apply(
                                    &mut kernel,
                                    &mut pending_observations,
                                    KernelInputEvent::Timeout,
                                );
                                break;
                            }
                        };

                        while let Some(evt) = provider_stream.next().await {
                            match evt? {
                                StreamEvent::TextDelta { delta } => {
                                    final_text.push_str(&delta);
                                    yield RunEvent::TextDelta(delta);
                                }
                                StreamEvent::ThinkingDelta { delta } => {
                                    yield RunEvent::ThinkingDelta(delta);
                                }
                                StreamEvent::ToolCall { id, name, arguments } => {
                                    yield RunEvent::ToolCall { id: id.clone(), name: name.clone() };
                                    final_tool_calls.push(ToolCall {
                                        id: compact_str::CompactString::new(&id),
                                        name: compact_str::CompactString::new(&name),
                                        arguments,
                                    });
                                }
                                StreamEvent::Usage { total_tokens } => {
                                    turn_tokens = total_tokens;
                                }
                                StreamEvent::Done => {}
                            }
                        }

                        let mut assistant = Message {
                            role: deepstrike_core::types::message::Role::Assistant,
                            content: deepstrike_core::types::message::Content::Text(final_text.clone()),
                            tool_calls: final_tool_calls.clone(),
                            token_count: if turn_tokens > 0 { Some(turn_tokens) } else { None },
                        };

                        self.opts.provider.commit_stream_replay(&final_text, &final_tool_calls);
                        let mut provider_replay = peek_provider_replay(
                            self.opts.provider.as_ref(),
                            &final_text,
                            &final_tool_calls,
                        );
                        repair_llm_completed(&mut assistant, &mut provider_replay);

                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::ProviderResult {
                                message: assistant.clone(),
                                observed_input_tokens: None,
                                observed_output_tokens: None,
                                // COMPAT(gov-clock): rust SDK does not yet drive the in-kernel
                                // governance gate, so no clock is fed. Set once it adopts governancePolicy.
                                now_ms: None,
                            },
                        );
                        self.log(
                            &session_id,
                            SessionEvent::LlmCompleted {
                                turn: kernel.lock().unwrap().state_machine().turn,
                                message: assistant,
                                provider_replay,
                            },
                        )
                        .await;
                    }
                    KernelAction::ExecuteTool { calls } => {
                        let tool_calls = calls.clone();
                        self.log(
                            &session_id,
                            SessionEvent::ToolRequested {
                                turn: kernel.lock().unwrap().state_machine().turn,
                                calls: tool_calls.clone(),
                            },
                        )
                        .await;

                        if let Some(gov) = &self.opts.governance {
                            let mut g = gov.lock().await;
                            if let Some(aid) = &self.opts.agent_id {
                                g.set_identity(aid, &session_id);
                            }
                        }

                        let run_ctx = RunContext {
                            agent_id: self.opts.agent_id.as_deref(),
                            skill_dir: self.opts.skill_dir.as_deref(),
                            dream_store: self.opts.dream_store.as_deref(),
                            knowledge_source: self.opts.knowledge_source.as_deref(),
                            governance: self.opts.governance.clone(),
                            on_tool_suspend: self.opts.on_tool_suspend.clone(),
                            on_permission_request: self.opts.on_permission_request.clone(),
                        };

                        let mut tool_results = Vec::new();
                        let mut normal_calls = Vec::new();
                        let mut plan_calls = Vec::new();

                        for call in &tool_calls {
                            if call.name == "update_plan" {
                                plan_calls.push(call);
                            } else {
                                normal_calls.push(call.clone());
                            }
                        }

                        for call in plan_calls {
                            let update = parse_update_plan_args(&call.arguments);
                            kernel_apply(
                                &mut kernel,
                                &mut pending_observations,
                                KernelInputEvent::UpdateTask { update },
                            );
                            tool_results.push(deepstrike_core::types::message::ToolResult {
                                call_id: call.id.clone(),
                                output: deepstrike_core::types::message::Content::Text("success".to_string()),
                                is_error: false,
                                is_fatal: false,
                                error_kind: None,
                                token_count: None,
                            });
                            yield RunEvent::ToolResult {
                                call_id: call.id.to_string(),
                                content: "success".to_string(),
                                is_error: false,
                                is_fatal: false,
                                error_kind: None,
                            };
                        }

                        if !normal_calls.is_empty() {
                            let plane_stream = self.plane.execute_all(&normal_calls, run_ctx);
                            let mut stream = plane_stream;
                            while let Some(evt) = stream.next().await {
                                match evt? {
                                    RunEvent::ToolResult {
                                        call_id,
                                        content,
                                        is_error,
                                        is_fatal,
                                        error_kind,
                                    } => {
                                        tool_results.push(deepstrike_core::types::message::ToolResult {
                                            call_id: compact_str::CompactString::new(&call_id),
                                            output: deepstrike_core::types::message::Content::Text(content),
                                            is_error,
                                            is_fatal,
                                            error_kind,
                                            token_count: None,
                                        });
                                    }
                                    RunEvent::ToolArgumentRepaired { call_id, name, original_arguments, repaired_arguments } => {
                                        self.log(
                                            &session_id,
                                            SessionEvent::ToolArgumentRepaired {
                                                turn: kernel.lock().unwrap().state_machine().turn,
                                                tool: name.clone(),
                                                original_arguments: original_arguments.clone(),
                                                repaired_arguments: repaired_arguments.clone(),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::ToolArgumentRepaired {
                                            call_id,
                                            name,
                                            original_arguments,
                                            repaired_arguments,
                                        };
                                    }
                                    RunEvent::ToolDenied { call_id, tool_name, reason } => {
                                        self.log(
                                            &session_id,
                                            SessionEvent::ToolDenied {
                                                turn: kernel.lock().unwrap().state_machine().turn,
                                                call_id: call_id.clone(),
                                                tool_name: tool_name.clone(),
                                                reason: reason.clone(),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::ToolDenied { call_id, tool_name, reason };
                                    }
                                    RunEvent::PermissionRequest { call_id, tool_name, arguments, reason } => {
                                        let turn = kernel.lock().unwrap().state_machine().turn;
                                        self.log(
                                            &session_id,
                                            SessionEvent::PermissionRequested {
                                                turn,
                                                tool: tool_name.clone(),
                                                arguments: arguments.clone(),
                                                reason: Some(reason.clone()),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::PermissionRequest { call_id, tool_name, arguments, reason };
                                    }
                                    RunEvent::PermissionResolved { call_id, tool_name, approved, responder, reason } => {
                                        let turn = kernel.lock().unwrap().state_machine().turn;
                                        self.log(
                                            &session_id,
                                            SessionEvent::PermissionResolved {
                                                turn,
                                                approved,
                                                responder: responder.clone(),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::PermissionResolved { call_id, tool_name, approved, responder, reason };
                                    }
                                    other => yield other,
                                }
                            }
                            let names: Vec<String> = normal_calls.iter().map(|c| c.name.to_string()).collect();
                            kernel_apply(
                                &mut kernel,
                                &mut pending_observations,
                                KernelInputEvent::UpdateTask {
                                    update: TaskUpdate {
                                        progress: Some(format!("Executed tools: {}", names.join(", "))),
                                        ..Default::default()
                                    },
                                },
                            );
                        }

                        self.log(
                            &session_id,
                            SessionEvent::ToolCompleted {
                                turn: kernel.lock().unwrap().state_machine().turn,
                                results: tool_results.clone(),
                            },
                        )
                        .await;

                        for call in &normal_calls {
                            if let Some(result) = tool_results
                                .iter()
                                .find(|r| r.call_id.as_str() == call.id.as_str())
                            {
                                let output = match &result.output {
                                    deepstrike_core::types::message::Content::Text(s) => s.to_string(),
                                    deepstrike_core::types::message::Content::Parts(parts) => {
                                        serde_json::to_string(parts).unwrap_or_default()
                                    }
                                };
                                pending_spool_outputs.insert(
                                    call.id.to_string(),
                                    (call.name.to_string(), output),
                                );
                            }
                        }

                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::ToolResults {
                                results: tool_results,
                            },
                        );
                    }
                    KernelAction::EvaluateMilestone {
                        phase_id,
                        criteria,
                        required_evidence,
                        ..
                    } => {
                        let policy = self.opts.milestone_policy;
                        if policy == MilestonePolicy::AutoPass {
                            let result = MilestoneCheckResult::pass(phase_id.clone());
                            action = kernel_action(
                                &mut kernel,
                                &mut pending_observations,
                                KernelInputEvent::MilestoneResult { result },
                            );
                            next_archive_start = self
                                .append_observations(
                                    &session_id,
                                    &kernel,
                                    &mut pending_observations,
                                    &mut pending_spool_outputs,
                                    next_archive_start,
                                )
                                .await;
                        } else if let Some(handler) = &self.opts.on_milestone_evaluate {
                            let context = MilestoneEvaluationContext {
                                phase_id: phase_id.clone(),
                                criteria: criteria.clone(),
                                required_evidence: required_evidence.clone(),
                            };
                            let check_future = handler(context);
                            let result = check_future.await?;
                            action = kernel_action(
                                &mut kernel,
                                &mut pending_observations,
                                KernelInputEvent::MilestoneResult { result },
                            );
                            next_archive_start = self
                                .append_observations(
                                    &session_id,
                                    &kernel,
                                    &mut pending_observations,
                                    &mut pending_spool_outputs,
                                    next_archive_start,
                                )
                                .await;
                        } else {
                            next_archive_start = self
                                .append_observations(
                                    &session_id,
                                    &kernel,
                                    &mut pending_observations,
                                    &mut pending_spool_outputs,
                                    next_archive_start,
                                )
                                .await;
                            self.log(
                                &session_id,
                                SessionEvent::RunTerminal {
                                    reason: "milestone_pending".to_string(),
                                    turns_used: kernel.lock().unwrap().state_machine().turn.max(1),
                                    total_tokens: 0,
                                },
                            )
                            .await;
                            yield RunEvent::Done {
                                iterations: kernel.lock().unwrap().state_machine().turn.max(1),
                                total_tokens: 0,
                                status: "milestone_pending".to_string(),
                            };
                            return;
                        }
                    }
                    KernelAction::Done { result } => {
                        let status = format!("{:?}", result.termination).to_lowercase();
                        let turns_used = result.turns_used.max(1);
                        let total_tokens = result.total_tokens_used;

                        next_archive_start = self
                            .append_observations(
                                &session_id,
                                &kernel,
                                &mut pending_observations,
                                &mut pending_spool_outputs,
                                next_archive_start,
                            )
                            .await;

                        self.log(
                            &session_id,
                            SessionEvent::RunTerminal {
                                reason: status.clone(),
                                turns_used,
                                total_tokens,
                            },
                        )
                        .await;

                        if let (Some(store), Some(agent_id)) =
                            (&self.opts.dream_store, &self.opts.agent_id)
                        {
                            let new_msgs = kernel.lock().unwrap().state_machine_mut().drain_new_messages();
                            if !new_msgs.is_empty() {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64;
                                let session = deepstrike_core::memory::durable::SessionData {
                                    session_id: uuid::Uuid::new_v4().to_string(),
                                    agent_id: agent_id.clone(),
                                    messages: new_msgs,
                                    metadata: serde_json::Value::Null,
                                    created_at_ms: session_start_ms,
                                    updated_at_ms: now_ms,
                                };
                                let _ = store.save_session(session).await;
                            }
                        }

                        yield RunEvent::Done {
                            iterations: turns_used,
                            total_tokens,
                            status,
                        };
                        return;
                    }
                }
            }

            next_archive_start = self
                .append_observations(
                    &session_id,
                    &kernel,
                    &mut pending_observations,
                    &mut pending_spool_outputs,
                    next_archive_start,
                )
                .await;

            let (status, turns_used, total_tokens) = match &action {
                KernelAction::Done { result } => (
                    format!("{:?}", result.termination).to_lowercase(),
                    result.turns_used.max(1),
                    result.total_tokens_used,
                ),
                _ => (
                    "error".to_string(),
                    kernel.lock().unwrap().state_machine().turn.max(1),
                    0,
                ),
            };

            self.log(
                &session_id,
                SessionEvent::RunTerminal {
                    reason: status.clone(),
                    turns_used,
                    total_tokens,
                },
            )
            .await;

            if let KernelAction::Done { .. } = &action {
                if let (Some(store), Some(agent_id)) =
                    (&self.opts.dream_store, &self.opts.agent_id)
                {
                    let new_msgs = kernel.lock().unwrap().state_machine_mut().drain_new_messages();
                    if !new_msgs.is_empty() {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let session = deepstrike_core::memory::durable::SessionData {
                            session_id: uuid::Uuid::new_v4().to_string(),
                            agent_id: agent_id.clone(),
                            messages: new_msgs,
                            metadata: serde_json::Value::Null,
                            created_at_ms: session_start_ms,
                            updated_at_ms: now_ms,
                        };
                        let _ = store.save_session(session).await;
                    }
                }
            }

            yield RunEvent::Done {
                iterations: turns_used,
                total_tokens,
                status,
            };
        }
    }

    pub(crate) async fn append_observations(
        &self,
        session_id: &str,
        kernel_mutex: &std::sync::Mutex<KernelRuntime>,
        observations: &mut Vec<KernelObservation>,
        pending_spool_outputs: &mut std::collections::HashMap<String, (String, String)>,
        mut next_archive_start: u64,
    ) -> u64 {
        let drained = std::mem::take(observations);
        let (turn, preserved_refs, summary_tokens_by_index) = {
            let kernel = kernel_mutex.lock().unwrap();
            let sm = kernel.state_machine();
            let summary_tokens_by_index = drained
                .iter()
                .map(|obs| match obs {
                    KernelObservation::Compressed { summary, .. } => {
                        summary.as_ref().map(|s| sm.ctx.engine.count(s))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            (
                sm.turn,
                sm.ctx.partitions.task_state.preserved_refs.clone(),
                summary_tokens_by_index,
            )
        };

        for (index, obs) in drained.into_iter().enumerate() {
            match obs {
                KernelObservation::Compressed {
                    action,
                    rho_after: _,
                    summary,
                    archived,
                } => {
                    let Some(log) = &self.opts.session_log else {
                        continue;
                    };
                    let latest = log.latest_seq(session_id).await.unwrap_or(-1) as u64;
                    if latest < next_archive_start {
                        continue;
                    }
                    let end = latest;

                    let mut archive_ref = None;
                    if let Some(store) = &self.opts.compression_store {
                        if !archived.is_empty() {
                            if let Ok(path_ref) =
                                store.write(session_id, next_archive_start, &archived)
                            {
                                if !path_ref.is_empty() {
                                    archive_ref = Some(path_ref);
                                }
                            }
                        }
                    }

                    let summary_tokens = summary_tokens_by_index.get(index).copied().flatten();
                    let action_str = match action {
                        KernelPressureAction::None => "none".to_string(),
                        KernelPressureAction::SnipCompact => "snip_compact".to_string(),
                        KernelPressureAction::MicroCompact => "micro_compact".to_string(),
                        KernelPressureAction::ContextCollapse => "context_collapse".to_string(),
                        KernelPressureAction::AutoCompact => "auto_compact".to_string(),
                    };

                    if let Ok(compressed_seq) = log
                        .append(
                            session_id,
                            SessionEvent::Compressed {
                                turn,
                                archived_seq_range: (next_archive_start, end),
                                category: Some(category_for_kind("compressed")),
                                primitive: Some(primitive_for_kind("compressed")),
                                action: Some(action_str),
                                summary: summary.clone(),
                                summary_tokens,
                                archive_ref,
                                preserved_refs: preserved_refs.clone(),
                            },
                        )
                        .await
                    {
                        next_archive_start = compressed_seq + 1;
                    }
                }
                KernelObservation::Rollbacked {
                    turn,
                    checkpoint_history_len,
                    reason,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::Rollbacked {
                            turn,
                            category: Some(category_for_kind("rollbacked")),
                            primitive: Some(primitive_for_kind("rollbacked")),
                            checkpoint_history_len,
                            reason,
                        },
                    )
                    .await;
                }
                KernelObservation::CapabilityChanged {
                    turn,
                    added,
                    removed,
                    change_kind,
                    capability_id,
                    version,
                    mounted_by,
                    mount_reason,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::CapabilityChanged {
                            turn,
                            category: Some(category_for_kind("capability_changed")),
                            primitive: Some(primitive_for_kind("capability_changed")),
                            added,
                            removed,
                            change_kind,
                            capability_id,
                            version,
                            mounted_by,
                            mount_reason,
                        },
                    )
                    .await;
                }
                KernelObservation::MilestoneAdvanced {
                    turn,
                    phase_id,
                    capabilities_unlocked,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MilestoneAdvanced {
                            turn,
                            category: Some(category_for_kind("milestone_advanced")),
                            primitive: Some(primitive_for_kind("milestone_advanced")),
                            phase_id,
                            capabilities_unlocked,
                        },
                    )
                    .await;
                }
                KernelObservation::MilestoneBlocked {
                    turn,
                    phase_id,
                    reason,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MilestoneBlocked {
                            turn,
                            category: Some(category_for_kind("milestone_blocked")),
                            primitive: Some(primitive_for_kind("milestone_blocked")),
                            phase_id,
                            reason,
                        },
                    )
                    .await;
                }
                KernelObservation::MilestoneEvidence {
                    turn,
                    phase_id,
                    evidence,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MilestoneEvidence {
                            turn,
                            category: Some(category_for_kind("milestone_evidence")),
                            primitive: Some(primitive_for_kind("milestone_evidence")),
                            phase_id,
                            evidence,
                        },
                    )
                    .await;
                }
                KernelObservation::Renewed { .. } => {}
                KernelObservation::CheckpointTaken { turn, history_len } => {
                    self.log(
                        session_id,
                        SessionEvent::CheckpointTaken {
                            turn,
                            category: Some(category_for_kind("checkpoint_taken")),
                            primitive: Some(primitive_for_kind("checkpoint_taken")),
                            history_len,
                        },
                    )
                    .await;
                }
                KernelObservation::AgentProcessChanged { .. } => {}
                // Governance flagged a tool call for user approval. The kernel does
                // not block it; the SDK-side human-approval workflow is a follow-up.
                KernelObservation::ToolGated { .. } => {}
                // In-kernel signal routing decision. The rust SDK does not yet drive
                // signals through the kernel attention policy; observation is logged
                // by the generic observation path elsewhere if needed.
                KernelObservation::SignalDisposed { .. } => {}
                KernelObservation::BudgetExceeded { .. } => {}
                KernelObservation::Suspended { .. } => {}
                KernelObservation::Resumed { .. } => {}
                KernelObservation::PageOut {
                    turn,
                    action,
                    rho_after: _,
                    summary,
                    archived,
                    tier_hint,
                } => {
                    if !archived.is_empty() {
                        self.local_page_out_cache
                            .lock()
                            .unwrap()
                            .extend(archived.clone());
                    }

                    let action_str = match action {
                        KernelPressureAction::None => "none".to_string(),
                        KernelPressureAction::SnipCompact => "snip_compact".to_string(),
                        KernelPressureAction::MicroCompact => "micro_compact".to_string(),
                        KernelPressureAction::ContextCollapse => "context_collapse".to_string(),
                        KernelPressureAction::AutoCompact => "auto_compact".to_string(),
                    };

                    self.log(
                        session_id,
                        SessionEvent::PageOut {
                            turn,
                            category: Some(category_for_kind("page_out")),
                            primitive: Some(primitive_for_kind("page_out")),
                            action: Some(action_str.clone()),
                            summary: summary.clone(),
                            tier_hint: Some(tier_hint.clone()),
                            message_count: archived.len() as u32,
                        },
                    )
                    .await;

                    if tier_hint == "semantic" && !archived.is_empty() {
                        self.archive_semantic_page_out(archived, Some(action_str))
                            .await;
                    }
                }
                KernelObservation::PageInRequested { .. } => {}
                KernelObservation::MemoryWritten {
                    turn,
                    memory_id,
                    memory_kind,
                    size_bytes,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryWritten {
                            turn,
                            category: Some(category_for_kind("memory_written")),
                            primitive: Some(primitive_for_kind("memory_written")),
                            memory_id,
                            memory_kind,
                            size_bytes,
                        },
                    )
                    .await;
                }
                KernelObservation::MemoryQueried {
                    turn,
                    query_context,
                    requested_k,
                    requires_async_response,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryQueried {
                            turn,
                            category: Some(category_for_kind("memory_queried")),
                            primitive: Some(primitive_for_kind("memory_queried")),
                            query_context,
                            requested_k,
                            requires_async_response,
                        },
                    )
                    .await;
                }
                // Phase 7 / M3: no dedicated session kinds yet in rust SDK.
                KernelObservation::MemoryValidationFailed {
                    turn,
                    memory_id,
                    error,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryValidationFailed {
                            turn,
                            category: Some(category_for_kind("memory_validation_failed")),
                            primitive: Some(primitive_for_kind("memory_validation_failed")),
                            memory_id,
                            error,
                        },
                    )
                    .await;
                }
                KernelObservation::LargeResultSpooled {
                    turn,
                    call_id,
                    tool,
                    original_size,
                    preview_size,
                    spool_ref: _,
                } => {
                    let mut spool_ref = None;
                    let mut tool_name = tool;
                    if let Some((stored_tool, output)) = pending_spool_outputs.remove(&call_id) {
                        if tool_name.is_empty() {
                            tool_name = stored_tool;
                        }
                        if let Ok(path) =
                            crate::runtime::large_result_spool::persist_output(&call_id, &output)
                        {
                            spool_ref = Some(path);
                        }
                    }
                    self.log(
                        session_id,
                        SessionEvent::LargeResultSpooled {
                            turn,
                            category: Some(category_for_kind("large_result_spooled")),
                            primitive: Some(primitive_for_kind("large_result_spooled")),
                            call_id,
                            tool: tool_name,
                            original_size,
                            preview_size,
                            spool_ref,
                        },
                    )
                    .await;
                }
            }
        }
        next_archive_start
    }

    async fn read_entries(&self, session_id: &str) -> Result<Vec<SessionEntry>> {
        if let Some(log) = &self.opts.session_log {
            log.read(session_id, 0, None).await.map_err(Error::Io)
        } else {
            Ok(Vec::new())
        }
    }

    async fn log(&self, session_id: &str, event: SessionEvent) {
        if let Some(log) = &self.opts.session_log {
            let _ = log.append(session_id, event).await;
        }
    }

    async fn apply_kernel_page_in(
        &self,
        session_id: &str,
        kernel_mutex: &std::sync::Mutex<KernelRuntime>,
        observations: &mut Vec<KernelObservation>,
    ) {
        let requests: Vec<_> = observations
            .iter()
            .filter_map(|obs| match obs {
                KernelObservation::PageInRequested {
                    turn: _,
                    call_id,
                    tool,
                    query,
                    top_k,
                } => Some((call_id.clone(), tool.clone(), query.clone(), *top_k)),
                _ => None,
            })
            .collect();

        if requests.is_empty() {
            return;
        }

        let mut entries = Vec::new();
        for (_call_id, tool, query, top_k) in requests {
            let top_k = top_k as usize;
            if tool == "memory" {
                // Priority 1: Local Page-Out Cache (keyword matching)
                let local_hits = {
                    let cache = self.local_page_out_cache.lock().unwrap();
                    cache
                        .iter()
                        .filter(|m| {
                            let content_str = message_content_as_text(&m.content);
                            content_str.to_lowercase().contains(&query.to_lowercase())
                        })
                        .cloned()
                        .take(top_k)
                        .collect::<Vec<_>>()
                };

                for hit in &local_hits {
                    let role_str = match hit.role {
                        deepstrike_core::types::message::Role::System => "system",
                        deepstrike_core::types::message::Role::User => "user",
                        deepstrike_core::types::message::Role::Assistant => "assistant",
                        deepstrike_core::types::message::Role::Tool => "tool",
                    };
                    let content_str = message_content_as_text(&hit.content);
                    entries.push(deepstrike_core::mm::PageInEntry {
                        content: format!("[local semantic cache] {}: {}", role_str, content_str),
                        tokens: None,
                        source: Some("semantic_cache".to_string()),
                    });
                }

                let remaining_k = top_k.saturating_sub(local_hits.len());
                if remaining_k > 0 {
                    if let (Some(store), Some(agent_id)) = (&self.opts.dream_store, &self.opts.agent_id) {
                        if let Ok(hits) = store.search(agent_id, &query, remaining_k).await {
                            for hit in hits {
                                entries.push(deepstrike_core::mm::PageInEntry {
                                    content: format!("[memory score={:.3}] {}", hit.score, hit.text),
                                    tokens: None,
                                    source: Some("memory".to_string()),
                                });
                            }
                        }
                    }
                }
            } else if tool == "knowledge" {
                if let Some(source) = &self.opts.knowledge_source {
                    if let Ok(snippets) = source.retrieve(&query, top_k).await {
                        for snippet in snippets {
                            entries.push(deepstrike_core::mm::PageInEntry {
                                content: snippet,
                                tokens: None,
                                source: Some("knowledge".to_string()),
                            });
                        }
                    }
                }
            }
        }

        if entries.is_empty() {
            return;
        }

        // Apply back to the kernel
        let mut kernel = kernel_mutex.lock().unwrap();
        let turn = kernel.state_machine().turn;
        let step = kernel.step(KernelInput::new(KernelInputEvent::PageIn {
            entries: entries.clone(),
        }));
        observations.extend(step.observations);

        // Append PageIn event to session log
        self.log(
            session_id,
            SessionEvent::PageIn {
                turn,
                category: Some(category_for_kind("page_in")),
                primitive: Some(primitive_for_kind("page_in")),
                entry_count: entries.len() as u32,
            },
        )
        .await;
    }

    async fn archive_semantic_page_out(&self, archived: Vec<Message>, action: Option<String>) {
        let (Some(store), Some(agent_id)) = (&self.opts.dream_store, &self.opts.agent_id) else {
            return;
        };

        let summary = match self.summarize_for_long_term_memory(&archived).await {
            Ok(s) => s,
            Err(_) => return, // non-fatal
        };

        if let Ok(existing) = store.load_memories(agent_id).await {
            let curation_result = deepstrike_core::memory::curator::CurationResult {
                to_add: vec![deepstrike_core::memory::semantic::MemoryEntry {
                    text: summary,
                    score: 1.0,
                    metadata: serde_json::json!({
                        "source": "semantic_page_out",
                        "action": action,
                    }),
                }],
                to_remove_indices: vec![],
                stats: deepstrike_core::memory::curator::CurationStats {
                    insights_processed: 1,
                    duplicates_removed: 0,
                    conflicts_resolved: 0,
                    entries_added: 1,
                },
            };
            let _ = store.commit(agent_id, curation_result, &existing).await;
        }
    }

    async fn summarize_for_long_term_memory(&self, archived: &[Message]) -> crate::Result<String> {
        let transcript = archived
            .iter()
            .map(|m| {
                let role_str = match m.role {
                    deepstrike_core::types::message::Role::System => "system",
                    deepstrike_core::types::message::Role::User => "user",
                    deepstrike_core::types::message::Role::Assistant => "assistant",
                    deepstrike_core::types::message::Role::Tool => "tool",
                };
                let content_str = message_content_as_text(&m.content);
                format!("{}: {}", role_str, content_str)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let system_prompt_opt = self.opts.system_prompt.as_deref();
        let system_text = match system_prompt_opt {
            Some(sp) => format!(
                "{}\n\nSummarize the following conversation for long-term memory. Preserve key facts, decisions, and open questions.",
                sp
            ),
            None => "Summarize the following conversation for long-term memory. Preserve key facts, decisions, and open questions.".to_string(),
        };

        let context = deepstrike_core::context::renderer::RenderedContext {
            system_text,
            system_stable: String::new(),
            system_knowledge: String::new(),
            turns: vec![deepstrike_core::types::message::Message {
                role: deepstrike_core::types::message::Role::User,
                content: deepstrike_core::types::message::Content::Text(transcript.clone()),
                tool_calls: vec![],
                token_count: None,
            }],
        };

        let synth_state = self.opts.provider.create_run_state();
        let mut stream = self
            .opts
            .provider
            .stream(&context, &[], None, synth_state.as_ref())
            .await?;

        let mut synthesis_text = String::new();
        while let Some(evt) = stream.next().await {
            if let Ok(StreamEvent::TextDelta { delta }) = evt {
                synthesis_text.push_str(&delta);
            }
        }

        let text = synthesis_text.trim();
        if text.is_empty() {
            Ok(transcript.chars().take(2000).collect())
        } else {
            Ok(text.to_string())
        }
    }
}

fn message_content_as_text(content: &deepstrike_core::types::message::Content) -> String {
    match content {
        deepstrike_core::types::message::Content::Text(s) => s.clone(),
        deepstrike_core::types::message::Content::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                deepstrike_core::types::message::ContentPart::Text { text } => Some(text.as_str()),
                deepstrike_core::types::message::ContentPart::ToolResult { output, .. } => Some(output.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn kernel_apply(
    kernel: &mut KernelRuntime,
    pending_observations: &mut Vec<KernelObservation>,
    event: KernelInputEvent,
) {
    let step = kernel.step(KernelInput::new(event));
    pending_observations.extend(step.observations);
}

fn kernel_action(
    kernel: &mut KernelRuntime,
    pending_observations: &mut Vec<KernelObservation>,
    event: KernelInputEvent,
) -> KernelAction {
    let mut step = kernel.step(KernelInput::new(event));
    pending_observations.append(&mut step.observations);
    take_single_action(step)
}

fn take_single_action(mut step: KernelStep) -> KernelAction {
    step.actions
        .pop()
        .expect("kernel transition must return one action")
}

pub async fn collect_text(
    mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + '_>>,
) -> Result<String> {
    let mut text = String::new();
    while let Some(evt) = stream.next().await {
        if let RunEvent::TextDelta(d) = evt? {
            text.push_str(&d);
        }
    }
    Ok(text)
}

fn merge_extensions(
    base: Option<&serde_json::Value>,
    over: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    match (base, over) {
        (Some(b), Some(o)) => {
            let mut merged = b.clone();
            if let (Some(m), Some(obj)) = (merged.as_object_mut(), o.as_object()) {
                for (k, v) in obj {
                    m.insert(k.clone(), v.clone());
                }
            }
            Some(merged)
        }
        (Some(b), None) => Some(b.clone()),
        (None, Some(o)) => Some(o.clone()),
        (None, None) => None,
    }
}

fn is_prompt_too_long_error(error: &Error) -> bool {
    let msg = error.to_string().to_lowercase();
    msg.contains("413")
        || msg.contains("too long")
        || msg.contains("prompt too long")
        || msg.contains("context length exceeded")
        || msg.contains("context_length_exceeded")
}

fn effective_wall_budget(
    scheduler_budget: Option<SchedulerBudget>,
    fallback_timeout_ms: Option<u64>,
) -> Option<u64> {
    scheduler_budget
        .and_then(|budget| budget.max_wall_ms)
        .or(fallback_timeout_ms)
}

fn next_archived_seq_start(events: Option<&[SessionEntry]>) -> u64 {
    let mut next = 0u64;
    for entry in events.unwrap_or_default() {
        if let SessionEvent::Compressed {
            archived_seq_range, ..
        } = &entry.event
        {
            next = next.max(archived_seq_range.1 + 1);
        }
    }
    next
}

fn rendered_context_from_messages(
    messages: Vec<Message>,
) -> deepstrike_core::context::renderer::RenderedContext {
    let mut system_parts = Vec::new();
    let mut turns = Vec::new();
    for message in messages {
        if message.role == deepstrike_core::types::message::Role::System {
            if let Some(text) = message.content.as_text() {
                system_parts.push(text.to_owned());
            }
        } else {
            turns.push(message);
        }
    }
    let system_text = system_parts.join("\n\n");
    deepstrike_core::context::renderer::RenderedContext {
        system_text: system_text.clone(),
        system_stable: system_text,
        system_knowledge: String::new(),
        turns,
    }
}

fn parse_update_plan_args(val: &serde_json::Value) -> TaskUpdate {
    let plan = val.get("plan").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
    });
    let current_step = val
        .get("current_step")
        .or_else(|| val.get("currentStep"))
        .and_then(|v| v.as_u64().map(|x| x as usize));
    let progress = val
        .get("progress")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    let scratchpad = val
        .get("scratchpad")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    let blocked_on = val
        .get("blocked_on")
        .or_else(|| val.get("blockedOn"))
        .and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });
    let preserved_refs = val
        .get("preserved_refs")
        .or_else(|| val.get("preservedRefs"))
        .and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });
    TaskUpdate {
        plan,
        current_step,
        progress,
        scratchpad,
        blocked_on,
        preserved_refs,
    }
}

fn select_memories(
    query: &MemoryQuery,
    entries: &[deepstrike_core::memory::semantic::MemoryEntry],
) -> MemoryRetrieval {
    let filter_out: std::collections::HashSet<String> = query
        .already_surfaced
        .iter()
        .chain(query.active_tools.iter())
        .cloned()
        .collect();
    let candidates: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry
                .metadata
                .get("name")
                .and_then(|value| value.as_str())
                .is_none_or(|name| !filter_out.contains(name))
        })
        .collect();
    if candidates.is_empty() {
        return MemoryRetrieval {
            selected_memory_ids: vec![],
            selection_rationale: "No candidates after filtering".to_string(),
        };
    }
    MemoryRetrieval {
        selected_memory_ids: candidates
            .iter()
            .take(query.top_k)
            .filter_map(|entry| {
                entry
                    .metadata
                    .get("name")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect(),
        selection_rationale: "Stub selector ranked index entries".to_string(),
    }
}
