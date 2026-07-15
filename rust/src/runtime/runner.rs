use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use crate::runtime::sandboxed_skill::scan_skill_dir;
use crate::runtime::skill_watcher::SkillWatcher;
use async_stream::try_stream;
use deepstrike_core::governance::quota::ResourceQuota;
use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryPolicy, MemoryProvenance, MemoryQuery, MemoryRecall,
    MemoryRecord, MemoryScope, MemoryTrustLevel,
};
use deepstrike_core::runtime::kernel::{
    CancellationReason, KernelAction, KernelEffect, KernelInput, KernelInputEvent,
    KernelObservation, KernelPressureAction, KernelReliabilityConfig, KernelRuntime, KernelStep,
    RunConfig,
};
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::scheduler::policy::SchedulerBudget as KernelBudget;
use deepstrike_core::scheduler::policy::SchedulerPolicyConfig;
use deepstrike_core::types::message::{Message, ToolCall};
use deepstrike_core::types::milestone::MilestoneCheckResult;
use deepstrike_core::types::signal::{
    RuntimeSignal as KernelSignal, SignalSource as KernelSignalSource,
    SignalType as KernelSignalType, Urgency,
};
use deepstrike_core::types::task::RuntimeTask;
use futures::StreamExt;

use crate::governance::Governance;
use crate::knowledge::KnowledgeSource;
use crate::memory::DreamStore;
use crate::providers::{LLMProvider, StreamEvent};
use crate::run_event::RunEvent;
use crate::runtime::archive::ArchiveStore;
use crate::runtime::execution_plane::{
    ExecutionPlane, LocalExecutionPlane, PermissionRequest, PermissionRequestHandler,
    PermissionResponse, RunContext, ToolSuspendHandler,
};
use crate::runtime::os_profile::{
    assert_native_profile, GovernancePolicy, OsProfile, SignalPolicy,
};
use crate::runtime::provider_replay::{peek_provider_replay, seed_provider_replay_from_events};
use crate::runtime::replay::{
    is_mid_run, repair_entries_with_cap, replay_messages_with_cap,
    replay_messages_with_cap_and_loader,
};
use crate::runtime::session_log::{SessionEntry, SessionLog};
use crate::{Error, Result};
use crate::{SignalDeliveryReceipt, SignalSource};
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

/// P0-C tool-gating telemetry: per-LLM-turn metrics, delivered to [`RuntimeOptions::on_turn_metrics`].
/// Pure observation — no behavior change. `tools_exposed` vs `tools_called` quantifies over-exposure;
/// consecutive equal `active_skill` values measure skill dwell `D`; the cache split gives the
/// prompt-cache hit baseline. Mirrors the node SDK `TurnMetrics`.
#[derive(Debug, Clone)]
pub struct TurnMetrics {
    pub turn: u32,
    pub tools_exposed: usize,
    pub tools_called: usize,
    pub active_skill: Option<String>,
    pub input_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    /// I1: pro-rata per-slot attribution of `cache_read_tokens` (Anthropic only). Mirrors Node.
    pub cache_read_tokens_by_slot: Option<crate::providers::CacheReadBySlot>,
}

/// Sink for per-turn [`TurnMetrics`]. Synchronous, infallible — it must never affect the run.
pub type OnTurnMetricsHandler = std::sync::Arc<dyn Fn(TurnMetrics) + Send + Sync>;

/// Configuration for a `RuntimeRunner` (aligned with Node/Python `RuntimeOptions`).
pub struct RuntimeOptions {
    pub provider: Box<dyn LLMProvider>,
    pub execution_plane: Option<Box<dyn ExecutionPlane>>,
    pub session_log: Option<Arc<dyn SessionLog>>,
    pub compression_store: Option<Arc<dyn ArchiveStore>>,
    /// Directory used by the built-in large-result spool effect executor.
    /// Defaults to `.spool` when omitted.
    pub spool_dir: Option<std::path::PathBuf>,
    /// Bounded ABI-v2 replay, retry, and large-result policy. Omitted fields retain kernel defaults.
    pub kernel_reliability: Option<KernelReliabilityConfig>,
    /// When set, `execute` reuses this session id.
    pub session_id: Option<String>,
    pub max_tokens: u32,
    pub max_turns: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub extensions: Option<serde_json::Value>,
    pub agent_id: Option<String>,
    /// Required by host-generated memory queries and semantic page-out writes.
    pub memory_scope: Option<MemoryScope>,
    /// I4: optional run-start memory pre-fetch hook. The runner calls this once per run, before
    /// the first LLM turn, with the goal string; each returned query becomes a `dream_store.search`
    /// and the resulting hits page into the knowledge partition before turn 1. Mirrors the Node
    /// SDK `preQueryMemory`. Sync-only in Rust today — async hosts can pre-compute. Errs-open
    /// when `dream_store` or `agent_id` is missing.
    pub pre_query_memory: Option<std::sync::Arc<dyn Fn(&str) -> Vec<MemoryQuery> + Send + Sync>>,
    pub system_prompt: Option<String>,
    pub initial_memory: Vec<String>,
    pub skill_dir: Option<std::path::PathBuf>,
    pub dream_store: Option<Box<dyn DreamStore>>,
    pub knowledge_source: Option<Box<dyn KnowledgeSource>>,
    pub signal_source: Option<Box<dyn SignalSource>>,
    pub governance: Option<Arc<tokio::sync::Mutex<Governance>>>,
    pub os_profile: Option<OsProfile>,
    pub governance_policy: Option<GovernancePolicy>,
    pub signal_policy: Option<SignalPolicy>,
    pub scheduler_policy: Option<SchedulerPolicyConfig>,
    pub resource_quota: Option<ResourceQuota>,
    /// Opt-in long-term memory policy (`set_memory_policy`), enforced at the kernel memory traps.
    pub memory_policy: Option<MemoryPolicy>,
    pub tokenizer: Option<String>,
    pub enable_plan_tool: Option<bool>,
    pub on_tool_suspend: Option<ToolSuspendHandler>,
    pub on_permission_request: Option<PermissionRequestHandler>,
    /// How to handle `EvaluateMilestone` actions. Default: `RequireVerifier`.
    pub milestone_policy: MilestonePolicy,
    pub milestone_contract: Option<deepstrike_core::types::milestone::MilestoneContract>,
    pub run_spec: Option<deepstrike_core::types::agent::AgentRunSpec>,
    /// P0-A tool gating: a static per-run tool profile — only these tool ids (plus the
    /// skill/memory/knowledge/update_plan meta-tools) are exposed to the model each turn.
    /// Lowers to the same `capability_filter` sub-agents use; byte-stable across the run, so it
    /// never busts the prompt-cache prefix. Augments `run_spec`'s filter when both are set;
    /// synthesizes a minimal top-level spec otherwise. `None`/empty ⇒ no gating (no config = old).
    pub allowed_tool_ids: Option<Vec<String>>,
    /// P0-C: optional per-turn metrics sink for tool-gating telemetry (see [`TurnMetrics`]). Pure
    /// observation; invoked once per LLM turn. Panics are not caught — keep the sink trivial.
    pub on_turn_metrics: Option<OnTurnMetricsHandler>,
    /// P1-B/D stable-core: tool ids always exposed under skill gating. Empty ⇒ skills narrow to
    /// exactly their declared tools + meta-tools. Opt-in: no skill declaring tools ⇒ never engages.
    pub stable_core_tool_ids: Vec<String>,
    pub on_milestone_evaluate: Option<MilestoneEvaluationHandler>,
}

/// P0-A: compute the effective top-level run spec from an optional explicit `run_spec` and an
/// optional `allowed_tool_ids` static profile. The profile sets the capability filter's allowed
/// ids — augmenting an explicit spec, or synthesizing a minimal `custom`-role spec when none is
/// given. Returns `None` when neither is set ⇒ no gating (no config = old behavior).
fn build_run_spec(
    explicit: Option<deepstrike_core::types::agent::AgentRunSpec>,
    allowed_tool_ids: Option<&[String]>,
    agent_id: Option<&str>,
    session_id: &str,
    goal: &str,
) -> Option<deepstrike_core::types::agent::AgentRunSpec> {
    use deepstrike_core::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
    let profile = allowed_tool_ids.filter(|ids| !ids.is_empty());
    match (explicit, profile) {
        (Some(mut spec), Some(ids)) => {
            spec.capability_filter.allowed_ids = ids.iter().map(|s| s.as_str().into()).collect();
            Some(spec)
        }
        (Some(spec), None) => Some(spec),
        (None, Some(ids)) => {
            let mut spec = AgentRunSpec::new(
                AgentIdentity::new(agent_id.unwrap_or("root"), session_id),
                AgentRole::Custom,
                goal.to_string(),
            );
            spec.capability_filter.allowed_ids = ids.iter().map(|s| s.as_str().into()).collect();
            Some(spec)
        }
        (None, None) => None,
    }
}

/// Orchestrates the agentic turn loop via the runtime kernel + session event log.
pub struct RuntimeRunner {
    opts: RuntimeOptions,
    plane: Box<dyn ExecutionPlane>,
    interrupted: AtomicBool,
    cancellation_reason: AtomicU8,
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
            cancellation_reason: AtomicU8::new(0),
            active_kernel: std::sync::Mutex::new(None),
            local_page_out_cache: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn interrupt(&self) {
        self.interrupt_with_reason(CancellationReason::User);
    }

    pub fn interrupt_with_reason(&self, reason: CancellationReason) {
        self.cancellation_reason
            .store(cancellation_reason_code(reason), Ordering::Relaxed);
        self.interrupted.store(true, Ordering::Relaxed);
    }

    pub fn execution_plane(&self) -> &dyn ExecutionPlane {
        self.plane.as_ref()
    }

    pub async fn write_memory(
        &self,
        memory: MemoryRecord,
        session_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let Some(store) = &self.opts.dream_store else {
            return Ok(());
        };
        let Some(agent_id) = agent_id.or(self.opts.agent_id.as_deref()) else {
            return Ok(());
        };

        let (kernel, requested) = self.begin_memory_syscall(KernelInputEvent::WriteMemory {
            memory: memory.clone(),
        });
        let persist_effect = requested
            .actions
            .iter()
            .find_map(|action| match &action.effect {
                KernelEffect::PersistMemory { memory } => {
                    Some((action.effect_id.clone(), memory.clone()))
                }
                _ => None,
            });
        let Some((effect_id, canonical)) = persist_effect else {
            self.append_memory_syscall_observations(session_id, requested.observations)
                .await;
            return Ok(());
        };

        let io_result: Result<()> = async {
            store.upsert(agent_id, canonical).await?;
            Ok(())
        }
        .await;
        let completed =
            kernel
                .lock()
                .unwrap()
                .step(KernelInput::new(KernelInputEvent::MemoryPersistResult {
                    effect_id,
                    error: io_result.as_ref().err().map(ToString::to_string),
                }));
        self.append_memory_syscall_observations(session_id, completed.observations)
            .await;
        io_result
    }

    pub async fn query_memory(
        &self,
        query: MemoryQuery,
        session_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Vec<MemoryRecall>> {
        let Some(store) = &self.opts.dream_store else {
            return Ok(Vec::new());
        };
        let Some(agent_id) = agent_id.or(self.opts.agent_id.as_deref()) else {
            return Ok(Vec::new());
        };

        let (kernel, requested) = self.begin_memory_syscall(KernelInputEvent::QueryMemory {
            query: query.clone(),
        });
        let query_effect = requested
            .actions
            .iter()
            .find_map(|action| match &action.effect {
                KernelEffect::QueryMemory { query, requested_k } => {
                    Some((action.effect_id.clone(), query.clone(), *requested_k))
                }
                _ => None,
            });
        let Some((effect_id, mut canonical_query, requested_k)) = query_effect else {
            self.append_memory_syscall_observations(session_id, requested.observations)
                .await;
            return Ok(Vec::new());
        };

        canonical_query.top_k = requested_k;
        let hits = store.search(agent_id, &canonical_query).await?;
        let completed =
            kernel
                .lock()
                .unwrap()
                .step(KernelInput::new(KernelInputEvent::MemoryQueryResult {
                    effect_id,
                    hits: hits.clone(),
                    error: None,
                }));
        self.append_memory_syscall_observations(session_id, completed.observations)
            .await;
        self.log_memory_retrieval_result(session_id, hits.clone())
            .await;
        Ok(hits)
    }

    async fn extract_session_memories(
        &self,
        session: &deepstrike_core::memory::durable::SessionData,
        scope: &MemoryScope,
    ) -> Result<Vec<MemoryRecord>> {
        let transcript = session
            .messages
            .iter()
            .map(|message| {
                format!(
                    "[{:?}] {}",
                    message.role,
                    message.content.as_text().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(8_000)
            .collect::<String>();
        let prompt = format!(
            "{transcript}\n\nReturn {{\"memories\":[{{\"name\":\"stable-kebab-key\",\"kind\":\"user|feedback|project|reference\",\"content\":\"fact\",\"description\":\"why durable\",\"confidence\":0.0,\"links\":[],\"pinned\":false,\"ttl_days\":null,\"evidence_refs\":[]}}]}} with at most 10 items. Return {{\"memories\":[]}} when nothing is durable."
        );
        let context = rendered_context_from_messages(vec![
            Message::system("Extract durable, reusable facts from this completed session. Return only JSON; do not include transient progress or guesses."),
            Message::user(prompt),
        ]);
        let state = self.opts.provider.create_run_state();
        let mut stream = self
            .opts
            .provider
            .stream(&context, &[], None, state.as_ref())
            .await?;
        let mut output = String::new();
        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta { delta } = event? {
                output.push_str(&delta);
            }
        }
        Ok(crate::memory::parse_extracted_memories(
            &output, session, scope,
        ))
    }

    async fn log_memory_retrieval_result(
        &self,
        session_id: Option<&str>,
        hits: Vec<MemoryRecall>,
    ) {
        let Some(session_id) = session_id.or(self.opts.session_id.as_deref()) else {
            return;
        };
        // The session-log record is the durable audit artifact; the kernel needs no
        // acknowledgment (the former kernel event was a no-op and was removed).
        self.log(
            session_id,
            SessionEvent::MemoryRetrievalResult { hits },
        )
        .await;
    }

    fn begin_memory_syscall(
        &self,
        event: KernelInputEvent,
    ) -> (Arc<std::sync::Mutex<KernelRuntime>>, KernelStep) {
        if let Some(active) = self.active_kernel.lock().unwrap().clone() {
            if !active.lock().unwrap().is_terminal() {
                let step = {
                    let mut kernel = active.lock().unwrap();
                    kernel.step(KernelInput::new(event))
                };
                return (active, step);
            }
        }

        let mut kernel = KernelRuntime::new(KernelBudget {
            max_tokens: self.opts.max_tokens,
            max_turns: self.opts.max_turns.unwrap_or(25),
            max_wall_ms: self.opts.timeout_ms,
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
            let signal_policy = self.opts.signal_policy.unwrap_or(profile.signal_policy);
            kernel.step(KernelInput::new(KernelInputEvent::SetSignalPolicy {
                policy: signal_policy.into_kernel(),
            }));
        }
        if let Some(scheduler_policy) = self.opts.scheduler_policy {
            kernel.step(KernelInput::new(KernelInputEvent::ConfigureRun {
                config: RunConfig {
                    scheduler_policy: Some(scheduler_policy),
                    ..RunConfig::default()
                },
            }));
        }
        if let Some(quota) = self.opts.resource_quota.clone() {
            kernel.step(KernelInput::new(KernelInputEvent::SetResourceQuota {
                quota,
            }));
        }
        if let Some(policy) = self.opts.memory_policy.clone() {
            kernel.step(KernelInput::new(memory_policy_event(policy)));
        }
        let kernel = Arc::new(std::sync::Mutex::new(kernel));
        let step = kernel.lock().unwrap().step(KernelInput::new(event));
        (kernel, step)
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
                    record_id,
                    scope,
                    memory_kind,
                    name,
                    size_bytes,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryWritten {
                            turn,
                            record_id,
                            scope,
                            memory_kind,
                            name,
                            size_bytes,
                        },
                    )
                    .await;
                }
                KernelObservation::MemoryQueried {
                    turn,
                    scope,
                    query,
                    requested_k,
                    requires_async_response,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryQueried {
                            turn,
                            scope,
                            query,
                            requested_k,
                            requires_async_response,
                        },
                    )
                    .await;
                }
                KernelObservation::MemoryValidationFailed {
                    turn,
                    record_id,
                    error,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryValidationFailed {
                            turn,
                            record_id,
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
            self.cancellation_reason.store(0, Ordering::Relaxed);

            if let Some(ks) = &self.opts.knowledge_source {
                ks.init().await?;
            }

            let provider_policy = self.opts.provider.runtime_policy();
            let effective_max_turns = self.opts.max_turns.or(provider_policy.max_turns).unwrap_or(25);
            let effective_timeout = self.opts.timeout_ms.or(provider_policy.timeout_ms);
            let policy = KernelBudget {
                max_tokens: self.opts.max_tokens,
                max_turns: effective_max_turns,
                max_wall_ms: effective_timeout,
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
            let mut pending_page_out_starts = std::collections::VecDeque::new();
            let mut active_page_out_start = None;

            if self.opts.kernel_reliability.is_some() || self.opts.scheduler_policy.is_some() {
                let mut step = kernel.lock().unwrap().step(KernelInput::new(
                    KernelInputEvent::ConfigureRun {
                        config: RunConfig {
                            reliability: self.opts.kernel_reliability.clone(),
                            scheduler_policy: self.opts.scheduler_policy,
                            ..RunConfig::default()
                        },
                    },
                ));
                if let Some(fault) = step.faults.pop() {
                    Err(Error::Other(format!(
                        "kernel {:?}: {}",
                        fault.code, fault.message
                    )))?;
                }
                pending_observations.append(&mut step.observations);
            }

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
                        key: None,
                        pinned: false,
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

            // P1-B/D: configure stable-core tool ids (always exposed under skill gating).
            if !self.opts.stable_core_tool_ids.is_empty() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetStableCoreTools {
                        tool_ids: self.opts.stable_core_tool_ids.clone(),
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

            let max_bytes = {
                let k = kernel.lock().unwrap();
                k.recovery_content_bytes()
            };

            if let Some(ref events) = prior_events {
                let repaired = repair_entries_with_cap(events, max_bytes);
                seed_provider_replay_from_events(self.opts.provider.as_ref(), &repaired);

                let messages = if let Some(ref store) = self.opts.compression_store {
                    let store_clone = store.clone();
                    replay_messages_with_cap_and_loader(&repaired, max_bytes, move |archive_ref| {
                        store_clone.read(archive_ref).map_err(|_| {
                            deepstrike_core::context::fault::ContextFault::MissingArchive {
                                session_id: String::new(),
                                seq: 0,
                            }
                        })
                    })
                } else {
                    replay_messages_with_cap(&repaired, max_bytes)
                };

                // P1-B B3: collect skill activations from the replayed history before `messages` is
                // moved, then re-emit them after preload to rebuild gating (active_skills is not
                // snapshotted — graceful).
                let reactivate: Vec<String> = messages
                    .iter()
                    .flat_map(|m| m.tool_calls.iter())
                    .filter(|c| c.name.as_str() == "skill")
                    .filter_map(|c| c.arguments.get("name").and_then(|v| v.as_str()).map(str::to_string))
                    .collect();

                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::PreloadHistory {
                        messages,
                    },
                );

                for name in reactivate {
                    kernel_apply(
                        &mut kernel,
                        &mut pending_observations,
                        KernelInputEvent::SkillActivated { name, lease_turns: None },
                    );
                }
            }

            let ext = merge_extensions(self.opts.extensions.as_ref(), extensions.as_ref());
            let provider_state = self.opts.provider.create_run_state();
            let mut next_archive_start = next_archived_seq_start(prior_events.as_deref());
            // P0-C: the skill loaded and in effect going into the current turn → per-turn metric.
            let mut active_skill: Option<String> = None;
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

            let signal_policy = self
                .opts
                .signal_policy
                .unwrap_or(os_profile.signal_policy);
            kernel_apply(
                &mut kernel,
                &mut pending_observations,
                KernelInputEvent::SetSignalPolicy {
                    policy: signal_policy.into_kernel(),
                },
            );

            if let Some(quota) = self.opts.resource_quota.clone() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetResourceQuota { quota },
                );
            }

            if let Some(policy) = self.opts.memory_policy.clone() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    memory_policy_event(policy),
                );
            }

            // I4: pre-fetch memory into the knowledge partition before the first LLM turn.
            // Mirrors Node/WASM/Python preQueryMemory. Errs-open: missing dream_store/agent_id
            // or a faulty closure silently skip the pre-fetch.
            if !resume_mid_run {
                if let (Some(pre), Some(store), Some(agent_id)) = (
                    self.opts.pre_query_memory.clone(),
                    self.opts.dream_store.as_ref(),
                    self.opts.agent_id.as_deref(),
                ) {
                    let queries = pre(goal.as_str());
                    let mut recalled = Vec::new();
                    for q in &queries {
                        if q.query.trim().is_empty() {
                            continue;
                        }
                        if let Ok(hits) = store.search(agent_id, q).await {
                            for hit in hits {
                                recalled.push(format!(
                                    "[memory record_id={} trust={} score={:.3}] {}",
                                    hit.record.record_id,
                                    match hit.record.provenance.trust {
                                        MemoryTrustLevel::Untrusted => "untrusted",
                                        MemoryTrustLevel::UserAsserted => "user_asserted",
                                        MemoryTrustLevel::HostVerified => "host_verified",
                                    },
                                    hit.score,
                                    hit.record.content
                                ));
                            }
                        }
                    }
                    if !recalled.is_empty() {
                        kernel_apply(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::AddHistoryMessage {
                                message: Message::user(recalled.join("\n")),
                                tokens: None,
                            },
                        );
                    }
                }
            }

            let mut action = if resume_mid_run {
                kernel_action(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::Resume,
                )
            } else {
                // P0-A: fold an explicit `run_spec` and/or the `allowed_tool_ids` profile into the
                // kernel's `capability_filter` (reuses the existing run_spec wire — no new ABI).
                let run_spec = build_run_spec(
                    self.opts.run_spec.clone(),
                    self.opts.allowed_tool_ids.as_deref(),
                    self.opts.agent_id.as_deref(),
                    &session_id,
                    &goal,
                );
                kernel_action(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::StartRun {
                        task: RuntimeTask::new(&goal).with_criteria(criteria),
                        run_spec,
                    },
                )
            };

            let mut last_skill_version: u64 = skill_watcher.as_ref().map(|w| w.version()).unwrap_or(0);

            while !kernel.lock().unwrap().is_terminal() {
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
                        &mut pending_page_out_starts,
                        next_archive_start,
                    )
                    .await;

                if self.interrupted.load(Ordering::Relaxed) {
                    kernel_apply(
                        &mut kernel,
                        &mut pending_observations,
                        KernelInputEvent::CancelOperation {
                            operation_id: "local-operation".into(),
                            reason: cancellation_reason_from_code(self.cancellation_reason.load(Ordering::Relaxed)),
                            pending_call_ids: pending_call_ids(&action),
                        },
                    );
                    break;
                }

                if let Some(ss) = &self.opts.signal_source {
                    if let Some(claim) = ss.claim_signal().await? {
                        let urgency = match claim.signal.urgency.as_str() {
                            "low" => Urgency::Low,
                            "high" => Urgency::High,
                            "critical" => Urgency::Critical,
                            _ => Urgency::Normal,
                        };
                        let source = match claim.signal.source.as_str() {
                            "cron" => KernelSignalSource::Cron,
                            "gateway" => KernelSignalSource::Gateway,
                            "heartbeat" => KernelSignalSource::Heartbeat,
                            _ => KernelSignalSource::Custom,
                        };
                        let signal_type = match claim.signal.signal_type.as_str() {
                            "job" => KernelSignalType::Job,
                            "alert" => KernelSignalType::Alert,
                            _ => KernelSignalType::Event,
                        };
                        let summary = claim
                            .signal
                            .payload
                            .get("goal")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("signal");
                        let mut kernel_sig = KernelSignal::new(
                            source,
                            signal_type,
                            urgency,
                            summary,
                        )
                        .with_payload(claim.signal.payload.clone())
                        .with_timestamp(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                        );
                        kernel_sig.id = uuid::Uuid::parse_str(&claim.signal_id).map_err(|_| {
                            crate::Error::Other("signal claim requires a UUID signal_id".into())
                        })?;
                        if let Some(dedupe_key) = &claim.signal.dedupe_key {
                            kernel_sig = kernel_sig.with_dedupe(dedupe_key.clone());
                        }
                        if let Some(recipient) = &claim.signal.recipient {
                            kernel_sig = kernel_sig.with_recipient(recipient.clone());
                        }
                        if let Some(deadline_ms) = claim.signal.deadline_ms {
                            kernel_sig = kernel_sig.with_deadline(deadline_ms);
                        }
                        if let Some(coalesce_key) = &claim.signal.coalesce_key {
                            kernel_sig = kernel_sig.with_coalesce(coalesce_key.clone());
                        }
                        kernel_sig.coalesced_count = claim.signal.coalesced_count.max(1);
                        // Kernel-routed (parity with node/py): the kernel's attention policy decides
                        // the disposition (dedup / queue / interrupt / preempt) and emits
                        // `signal_delivery_disposed`; an actionable disposition yields the next action to
                        // adopt (e.g. a forced Reason turn on Critical), queued/observed yields none.
                        let mut kguard = kernel.lock().unwrap();
                        let mut step = kguard.step(KernelInput::new(KernelInputEvent::DeliverSignal {
                            delivery_id: claim.delivery_id.clone(),
                            attempt: claim.delivery_attempt,
                            signal: kernel_sig,
                        }));
                        drop(kguard);
                        let disposition_matches = step.observations.iter().filter(|observation| matches!(
                            observation,
                            KernelObservation::SignalDeliveryDisposed {
                                delivery_id,
                                attempt,
                                ..
                            } if delivery_id == &claim.delivery_id && *attempt == claim.delivery_attempt
                        )).count() == 1;
                        let receipt = SignalDeliveryReceipt {
                            delivery_id: claim.delivery_id,
                            lease_token: claim.lease_token,
                        };
                        if !step.faults.is_empty() || !disposition_matches {
                            let _ = ss.nack_signal(&receipt).await?;
                            Err(crate::Error::Other(
                                "kernel did not return the matching signal delivery disposition".into(),
                            ))?;
                        }
                        if !ss.ack_signal(&receipt).await? {
                            let _ = ss.nack_signal(&receipt).await?;
                            Err(crate::Error::Other(
                                "signal lease was lost before acknowledgement".into(),
                            ))?;
                        }
                        pending_observations.append(&mut step.observations);
                        if let Some(sig_action) = step.actions.pop() {
                            action = sig_action;
                        }
                        // Critical attention/preemption is distinct from operation cancellation.
                    }
                }
                if kernel.lock().unwrap().is_terminal() {
                    break;
                }

                match &action.effect {
                    KernelEffect::CallProvider { context, tools } => {
                        let provider_effect_id = action.effect_id.clone();
                        let mut final_text = String::new();
                        let mut final_tool_calls: Vec<ToolCall> = Vec::new();
                        let mut turn_tokens: u32 = 0;
                        let mut turn_input_tokens: u32 = 0;
                        let mut turn_cache_read_tokens: u32 = 0;
                        let mut turn_cache_creation_tokens: u32 = 0;
                        let mut turn_cache_read_by_slot: Option<crate::providers::CacheReadBySlot> = None;
                        let mut turn_stop_reason: Option<String> = None;
                        // I5: governance schema-level pre-filter. When a GovernancePolicy is loaded
                        // and `surface_denied_in_system` is true (default), drop denied tools from
                        // the schema before the provider sees them.
                        let (filtered_tools, filtered_context_storage);
                        let (provider_tools, provider_context): (&[_], &_) = if let Some(policy) = self.opts.governance_policy.as_ref() {
                            if policy.surface_denied_in_system {
                                let (allowed, denied) = crate::runtime::governance_filter_schema(tools, policy);
                                if !denied.is_empty() {
                                    filtered_tools = allowed;
                                    let mut cloned = context.clone();
                                    let note = format!("[governance] the following tools are denied for this run and will fail if called: {}.", denied.join(", "));
                                    cloned.system_knowledge = if cloned.system_knowledge.is_empty() {
                                        note
                                    } else {
                                        format!("{}\n\n{}", cloned.system_knowledge, note)
                                    };
                                    filtered_context_storage = cloned;
                                    (&filtered_tools[..], &filtered_context_storage)
                                } else {
                                    (&tools[..], context)
                                }
                            } else { (&tools[..], context) }
                        } else { (&tools[..], context) };
                        // P0-C: snapshot the exposed-tool count now — `tools` borrows `action`, which is
                        // reassigned before the metrics emit below.
                        let tools_exposed = provider_tools.len();

                        let mut provider_stream = match self
                            .opts
                            .provider
                            .stream(provider_context, provider_tools, ext.as_ref(), provider_state.as_ref())
                            .await
                        {
                            Ok(s) => s,
                            Err(e) => {
                                // Reactive recovery is now a kernel decision. Forward the raw
                                // provider error and dispatch whatever the kernel returns:
                                // CallProvider to retry with a freshly compacted context, or Done to
                                // terminate with an honest ContextOverflow. The classify + compact +
                                // retry + give-up policy lives in the kernel (one place), not
                                // duplicated across the four SDK runners.
                                let msg = e.to_string();
                                action = kernel_action(
                                    &mut kernel,
                                    &mut pending_observations,
                                    KernelInputEvent::ProviderError {
                                        effect_id: provider_effect_id.clone(),
                                        message: msg.clone(),
                                    },
                                );
                                // Withholding (query.ts parity): surface the raw provider error only
                                // when the kernel could NOT recover (it returned a terminal). On a
                                // recovered retry (CallProvider) the error stays hidden. `continue`
                                // re-enters the loop: a recovered turn persists its compaction
                                // archive at the loop's normal append point, and a terminal Done
                                // exits through `is_terminal()` into the run_terminal emit.
                                if matches!(&action.effect, KernelEffect::Done { .. }) {
                                    yield RunEvent::Error(msg);
                                }
                                continue;
                            }
                        };

                        while let Some(evt) = provider_stream.next().await {
                            if self.interrupted.load(Ordering::Relaxed) {
                                break;
                            }
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
                                StreamEvent::Usage {
                                    total_tokens,
                                    input_tokens,
                                    cache_read_input_tokens,
                                    cache_creation_input_tokens,
                                    cache_read_input_tokens_by_slot,
                                    stop_reason,
                                    ..
                                } => {
                                    turn_tokens = total_tokens;
                                    // P0-C: capture input + prompt-cache split for the hit-rate baseline.
                                    turn_input_tokens = input_tokens;
                                    turn_cache_read_tokens = cache_read_input_tokens;
                                    turn_cache_creation_tokens = cache_creation_input_tokens;
                                    turn_cache_read_by_slot = cache_read_input_tokens_by_slot;
                                    // Phase 4: keep the last non-empty stop_reason for output-cap recovery.
                                    if stop_reason.is_some() { turn_stop_reason = stop_reason; }
                                }
                                StreamEvent::Done => {}
                            }
                        }

                        if self.interrupted.load(Ordering::Relaxed) {
                            action = kernel_action(
                                &mut kernel,
                                &mut pending_observations,
                                KernelInputEvent::CancelOperation {
                                    operation_id: "local-operation".into(),
                                    reason: cancellation_reason_from_code(self.cancellation_reason.load(Ordering::Relaxed)),
                                    pending_call_ids: vec![provider_effect_id],
                                },
                            );
                            break;
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
                                effect_id: provider_effect_id,
                                message: assistant.clone(),
                                observed_input_tokens: None,
                                observed_output_tokens: None,
                                // COMPAT(gov-clock): rust SDK does not yet drive the in-kernel
                                // governance gate, so no clock is fed. Set once it adopts governancePolicy.
                                now_ms: None,
                                // Phase 4: stop_reason drives the kernel's max-output-tokens recovery.
                                stop_reason: turn_stop_reason.clone(),
                            },
                        );
                        self.log(
                            &session_id,
                            SessionEvent::LlmCompleted {
                                turn: kernel.lock().unwrap().turn(),
                                message: assistant,
                                provider_replay,
                            },
                        )
                        .await;

                        // P0-C: per-turn tool-gating telemetry. `active_skill` reflects the skill in
                        // effect GOING INTO this turn; a `skill` call here only takes effect next turn
                        // — emit first, then advance.
                        if let Some(ref sink) = self.opts.on_turn_metrics {
                            sink(TurnMetrics {
                                turn: kernel.lock().unwrap().turn(),
                                tools_exposed,
                                tools_called: final_tool_calls.len(),
                                active_skill: active_skill.clone(),
                                input_tokens: turn_input_tokens,
                                cache_read_tokens: turn_cache_read_tokens,
                                cache_creation_tokens: turn_cache_creation_tokens,
                                cache_read_tokens_by_slot: turn_cache_read_by_slot.clone(),
                            });
                        }
                        if let Some(skill_call) =
                            final_tool_calls.iter().find(|c| c.name.as_str() == "skill")
                        {
                            if let Some(name) = skill_call.arguments.get("name").and_then(|v| v.as_str()) {
                                active_skill = Some(name.to_string());
                            }
                        }
                    }
                    KernelEffect::RequestApproval { requests } => {
                        let approval_effect_id = action.effect_id.clone();
                        let mut approved_calls = Vec::new();
                        let mut denied_calls = Vec::new();
                        for request in requests {
                            let arguments = request.arguments.to_string();
                            self.log(
                                &session_id,
                                SessionEvent::PermissionRequested {
                                    turn: kernel.lock().unwrap().turn(),
                                    tool: request.tool.clone(),
                                    arguments: arguments.clone(),
                                    reason: Some(request.reason.clone()),
                                },
                            )
                            .await;
                            yield RunEvent::PermissionRequest {
                                call_id: request.call_id.clone(),
                                tool_name: request.tool.clone(),
                                arguments: arguments.clone(),
                                reason: request.reason.clone(),
                            };

                            let response = match &self.opts.on_permission_request {
                                Some(handler) => match handler(PermissionRequest {
                                    call_id: request.call_id.clone(),
                                    tool_name: request.tool.clone(),
                                    arguments,
                                    reason: request.reason.clone(),
                                })
                                .await
                                {
                                    Ok(response) => response,
                                    Err(err) => PermissionResponse {
                                        approved: false,
                                        responder: "permission_handler".to_string(),
                                        reason: Some(format!("permission handler failed: {err}")),
                                    },
                                },
                                None => PermissionResponse {
                                    approved: false,
                                    responder: "policy_gate".to_string(),
                                    reason: Some("no permission handler configured".to_string()),
                                },
                            };
                            if response.approved {
                                approved_calls.push(request.call_id.clone());
                            } else {
                                denied_calls.push(request.call_id.clone());
                            }
                            let responder = if response.responder.is_empty() {
                                "host".to_string()
                            } else {
                                response.responder
                            };
                            self.log(
                                &session_id,
                                SessionEvent::PermissionResolved {
                                    turn: kernel.lock().unwrap().turn(),
                                    approved: response.approved,
                                    responder: responder.clone(),
                                },
                            )
                            .await;
                            yield RunEvent::PermissionResolved {
                                call_id: request.call_id.clone(),
                                tool_name: request.tool.clone(),
                                approved: response.approved,
                                responder,
                                reason: response.reason,
                            };
                        }
                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::ApprovalResult {
                                effect_id: approval_effect_id,
                                approved_calls,
                                denied_calls,
                                error: None,
                            },
                        );
                    }
                    KernelEffect::SpawnWorkflow { nodes, .. } => {
                        // This runner has no workflow child orchestrator. Report each
                        // requested spawn as a completed failure instead of treating the
                        // action as an observation or leaving the effect unresolved.
                        let workflow_effect_id = action.effect_id.clone();
                        let failures = nodes
                            .into_iter()
                            .map(|node| deepstrike_core::runtime::kernel::WorkflowSpawnFailure {
                                agent_id: node.agent_id.clone(),
                                error: "Rust RuntimeRunner has no workflow orchestrator".to_string(),
                            })
                            .collect();
                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::WorkflowSpawnResult {
                                effect_id: workflow_effect_id,
                                started_agent_ids: Vec::new(),
                                failures,
                                error: None,
                            },
                        );
                    }
                    KernelEffect::PreemptSubAgents { .. } => {
                        // RuntimeRunner does not launch external child runners, so
                        // there is no host process to cancel before acknowledging.
                        let preempt_effect_id = action.effect_id.clone();
                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::PreemptResult {
                                effect_id: preempt_effect_id,
                                error: None,
                            },
                        );
                    }
                    KernelEffect::PersistMemory { .. } => {
                        let effect_id = action.effect_id.clone();
                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::MemoryPersistResult {
                                effect_id,
                                error: Some(
                                    "memory effects must be executed by RuntimeRunner::write_memory"
                                        .to_string(),
                                ),
                            },
                        );
                    }
                    KernelEffect::QueryMemory { .. } => {
                        let effect_id = action.effect_id.clone();
                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::MemoryQueryResult {
                                effect_id,
                                hits: Vec::new(),
                                error: Some(
                                    "memory effects must be executed by RuntimeRunner::query_memory"
                                        .to_string(),
                                ),
                            },
                        );
                    }
                    KernelEffect::SpoolLargeResult { call_id, output, .. } => {
                        let effect_id = action.effect_id.clone();
                        let spool_dir = self.opts.spool_dir.as_deref().unwrap_or_else(|| std::path::Path::new(".spool"));
                        let (spool_ref, error) = match crate::runtime::large_result_spool::persist_output(spool_dir, call_id, output) {
                            Ok(path) => (Some(path), None),
                            Err(error) => (None, Some(error.to_string())),
                        };
                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::LargeResultSpoolResult { effect_id, spool_ref, error },
                        );
                    }
                    KernelEffect::ArchivePageOut { archived, tier, action: pressure_action, .. } => {
                        let effect_id = action.effect_id.clone();
                        let archived = archived.clone();
                        let tier = tier.clone();
                        let action_name = action_str_of(*pressure_action);
                        let archive_start = *active_page_out_start.get_or_insert_with(|| {
                            pending_page_out_starts.pop_front().unwrap_or(next_archive_start)
                        });
                        let archive_result = if let Some(store) = &self.opts.compression_store {
                            store.write(&session_id, archive_start, &archived)
                                .map(|path| (!path.is_empty()).then_some(path))
                        } else {
                            Ok(None)
                        };
                        let (archive_ref, error) = match archive_result {
                            Ok(archive_ref) => {
                                self.local_page_out_cache.lock().unwrap().extend(archived.clone());
                                if tier == "semantic" {
                                    self.archive_semantic_page_out(archived, Some(action_name)).await;
                                }
                                (archive_ref, None)
                            }
                            Err(error) => (None, Some(error.to_string())),
                        };
                        if error.is_none() {
                            active_page_out_start = None;
                        }
                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::PageOutArchiveResult { effect_id, archive_ref, error },
                        );
                    }
                    KernelEffect::ExecuteTool { calls } => {
                        let tool_effect_id = action.effect_id.clone();
                        let tool_calls = calls.clone();
                        self.log(
                            &session_id,
                            SessionEvent::ToolRequested {
                                turn: kernel.lock().unwrap().turn(),
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
                            memory_scope: self.opts.memory_scope.as_ref(),
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
                                                turn: kernel.lock().unwrap().turn(),
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
                                                turn: kernel.lock().unwrap().turn(),
                                                call_id: call_id.clone(),
                                                tool_name: tool_name.clone(),
                                                reason: reason.clone(),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::ToolDenied { call_id, tool_name, reason };
                                    }
                                    RunEvent::PermissionRequest { call_id, tool_name, arguments, reason } => {
                                        let turn = kernel.lock().unwrap().turn();
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
                                        let turn = kernel.lock().unwrap().turn();
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
                                turn: kernel.lock().unwrap().turn(),
                                results: tool_results.clone(),
                            },
                        )
                        .await;

                        // P1-B B3: a successfully-resolved `skill` call activates that skill for the
                        // next turn (fed before ToolResults, which computes the next action).
                        for call in &tool_calls {
                            if call.name.as_str() != "skill" {
                                continue;
                            }
                            let ok = tool_results
                                .iter()
                                .any(|r| r.call_id.as_str() == call.id.as_str() && !r.is_error);
                            if !ok {
                                continue;
                            }
                            if let Some(name) = call.arguments.get("name").and_then(|v| v.as_str()) {
                                kernel_apply(
                                    &mut kernel,
                                    &mut pending_observations,
                                    KernelInputEvent::SkillActivated { name: name.to_string(), lease_turns: None },
                                );
                            }
                        }

                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::ToolResults {
                                effect_id: tool_effect_id,
                                results: tool_results,
                            },
                        );
                    }
                    KernelEffect::EvaluateMilestone {
                        phase_id,
                        criteria,
                        required_evidence,
                        ..
                    } => {
                        let milestone_effect_id = action.effect_id.clone();
                        let policy = self.opts.milestone_policy;
                        if policy == MilestonePolicy::AutoPass {
                            let result = MilestoneCheckResult::pass(phase_id.clone());
                            action = kernel_action(
                                &mut kernel,
                                &mut pending_observations,
                                KernelInputEvent::MilestoneResult {
                                    effect_id: milestone_effect_id.clone(),
                                    result,
                                },
                            );
                            next_archive_start = self
                                .append_observations(
                                    &session_id,
                                    &kernel,
                                    &mut pending_observations,
                                    &mut pending_page_out_starts,
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
                                KernelInputEvent::MilestoneResult {
                                    effect_id: milestone_effect_id,
                                    result,
                                },
                            );
                            next_archive_start = self
                                .append_observations(
                                    &session_id,
                                    &kernel,
                                    &mut pending_observations,
                                    &mut pending_page_out_starts,
                                    next_archive_start,
                                )
                                .await;
                        } else {
                            next_archive_start = self
                                .append_observations(
                                    &session_id,
                                    &kernel,
                                    &mut pending_observations,
                                    &mut pending_page_out_starts,
                                    next_archive_start,
                                )
                                .await;
                            self.log(
                                &session_id,
                                SessionEvent::RunTerminal {
                                    reason: "milestone_pending".to_string(),
                                    turns_used: kernel.lock().unwrap().turn().max(1),
                                    total_tokens: 0,
                                },
                            )
                            .await;
                            yield RunEvent::Done {
                                iterations: kernel.lock().unwrap().turn().max(1),
                                total_tokens: 0,
                                status: "milestone_pending".to_string(),
                            };
                            return;
                        }
                    }
                    KernelEffect::Done { result } => {
                        let status = format!("{:?}", result.termination).to_lowercase();
                        let turns_used = result.turns_used.max(1);
                        let total_tokens = result.total_tokens_used;

                        next_archive_start = self
                            .append_observations(
                                &session_id,
                                &kernel,
                                &mut pending_observations,
                                &mut pending_page_out_starts,
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
                            let new_msgs = kernel.lock().unwrap().drain_new_messages();
                            if !new_msgs.is_empty() {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64;
                                let session = deepstrike_core::memory::durable::SessionData {
                                    session_id: session_id.clone(),
                                    agent_id: agent_id.clone(),
                                    messages: new_msgs,
                                    metadata: serde_json::Value::Null,
                                    created_at_ms: session_start_ms,
                                    updated_at_ms: now_ms,
                                };
                                let _ = store.save_session(session.clone()).await;
                                if let Some(scope) = self.opts.memory_scope.as_ref() {
                                    if let Ok(memories) = self.extract_session_memories(&session, scope).await {
                                        for memory in memories {
                                            let _ = self.write_memory(memory, Some(&session_id), Some(agent_id)).await;
                                        }
                                    }
                                }
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
                    &mut pending_page_out_starts,
                    next_archive_start,
                )
                .await;

            // I0a: when the loop exits without a clean kernel-done, preserve preempt intent
            // (interrupted flag set) in the run_terminal reason — otherwise an interrupt-curtailed
            // run reports "error" indistinguishable from a real crash. Mirrors Node/WASM/Python.
            let (status, turns_used, total_tokens) = match &action.effect {
                KernelEffect::Done { result } => (
                    format!("{:?}", result.termination).to_lowercase(),
                    result.turns_used.max(1),
                    result.total_tokens_used,
                ),
                _ => ("error".to_string(), kernel.lock().unwrap().turn().max(1), 0),
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

            if let KernelEffect::Done { .. } = &action.effect {
                if let (Some(store), Some(agent_id)) =
                    (&self.opts.dream_store, &self.opts.agent_id)
                {
                    let new_msgs = kernel.lock().unwrap().drain_new_messages();
                    if !new_msgs.is_empty() {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let session = deepstrike_core::memory::durable::SessionData {
                            session_id: session_id.clone(),
                            agent_id: agent_id.clone(),
                            messages: new_msgs,
                            metadata: serde_json::Value::Null,
                            created_at_ms: session_start_ms,
                            updated_at_ms: now_ms,
                        };
                        let _ = store.save_session(session.clone()).await;
                        if let Some(scope) = self.opts.memory_scope.as_ref() {
                            if let Ok(memories) = self.extract_session_memories(&session, scope).await {
                                for memory in memories {
                                    let _ = self.write_memory(memory, Some(&session_id), Some(agent_id)).await;
                                }
                            }
                        }
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
        pending_page_out_starts: &mut std::collections::VecDeque<u64>,
        mut next_archive_start: u64,
    ) -> u64 {
        let drained = std::mem::take(observations);
        let (turn, preserved_refs, summary_tokens_by_index) = {
            let kernel = kernel_mutex.lock().unwrap();
            let summary_tokens_by_index = drained
                .iter()
                .map(|obs| match obs {
                    KernelObservation::Compressed { summary, .. } => {
                        summary.as_ref().map(|s| kernel.count_tokens(s))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            (
                kernel.turn(),
                kernel.preserved_refs(),
                summary_tokens_by_index,
            )
        };

        for (index, obs) in drained.into_iter().enumerate() {
            match obs {
                KernelObservation::Compressed {
                    turn: _,
                    action,
                    rho_after: _,
                    summary,
                    archived_count,
                    invalidates_prefix_at: _,
                } => {
                    let Some(log) = &self.opts.session_log else {
                        continue;
                    };
                    let latest = log.latest_seq(session_id).await.unwrap_or(-1) as u64;
                    if latest < next_archive_start {
                        continue;
                    }
                    let end = latest;
                    if archived_count > 0 {
                        pending_page_out_starts.push_back(next_archive_start);
                    }

                    let summary_tokens = summary_tokens_by_index.get(index).copied().flatten();
                    let action_str = action_str_of(action);

                    if let Ok(compressed_seq) = log
                        .append(
                            session_id,
                            SessionEvent::Compressed {
                                turn,
                                archived_seq_range: (next_archive_start, end),
                                action: Some(action_str),
                                summary: summary.clone(),
                                summary_tokens,
                                preserved_refs: preserved_refs.clone(),
                            },
                        )
                        .await
                    {
                        next_archive_start = compressed_seq + 1;
                    }
                }
                KernelObservation::PageOutArchived {
                    turn,
                    action,
                    summary,
                    tier,
                    message_count,
                    archive_ref,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::PageOut {
                            turn,
                            action: Some(action_str_of(action)),
                            summary,
                            tier_hint: Some(tier),
                            message_count,
                            archive_ref,
                        },
                    )
                    .await;
                }
                KernelObservation::PageOutArchiveFailed { .. } => {}
                KernelObservation::Rollbacked {
                    turn,
                    checkpoint_history_len,
                    reason,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::Rollbacked {
                            turn,
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
                            phase_id,
                            reason,
                        },
                    )
                    .await;
                }
                KernelObservation::Renewed { .. } => {}
                KernelObservation::ContextBudgetExceeded { .. } => {}
                KernelObservation::KnowledgeSwept { .. } => {}
                KernelObservation::KnowledgeBudgetExceeded { .. } => {}
                KernelObservation::RepeatFuseTripped { .. } => {}
                KernelObservation::CriteriaGateFired { .. } => {}
                KernelObservation::CheckpointTaken { turn, history_len } => {
                    self.log(
                        session_id,
                        SessionEvent::CheckpointTaken { turn, history_len },
                    )
                    .await;
                }
                KernelObservation::EntropySample {
                    turn,
                    score,
                    score_version,
                    rho,
                    repeat_pressure,
                    failure_rate,
                    rollbacks_in_window,
                    window_turns,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::EntropySample {
                            turn,
                            score,
                            score_version,
                            rho,
                            repeat_pressure,
                            failure_rate,
                            rollbacks_in_window,
                            window_turns,
                        },
                    )
                    .await;
                }
                KernelObservation::EntropyAlert {
                    turn,
                    score,
                    threshold,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::EntropyAlert {
                            turn,
                            score,
                            threshold,
                        },
                    )
                    .await;
                }
                KernelObservation::AgentProcessChanged { .. } => {}
                // W0-ABI workflow lifecycle. The rust SDK has no workflow drive yet
                // (node/python only), so these are observed-but-ignored here.
                KernelObservation::WorkflowBatchSpawned { .. } => {}
                KernelObservation::WorkflowSpawnFailed { .. } => {}
                KernelObservation::WorkflowCompleted { .. } => {}
                KernelObservation::NodesRejected { .. } => {}
                KernelObservation::AgentPreempted { .. } => {}
                KernelObservation::AgentPreemptFailed { .. } => {}
                KernelObservation::MemoryWriteFailed { .. } => {}
                KernelObservation::MemoryQueryFailed { .. } => {}
                // Governance flagged a tool call for user approval. The kernel does
                // not block it; the SDK-side human-approval workflow is a follow-up.
                KernelObservation::ToolGated { .. } => {}
                // In-kernel signal routing decision. The rust SDK does not yet drive
                // signals through the kernel attention policy; observation is logged
                // by the generic observation path elsewhere if needed.
                KernelObservation::SignalDeliveryDisposed { .. } => {}
                KernelObservation::SignalDisplaced { .. }
                | KernelObservation::SignalExpired { .. }
                | KernelObservation::SignalsPending { .. } => {}
                KernelObservation::BudgetExceeded {
                    turn,
                    operation_id,
                    reservation_id,
                    budget,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::BudgetExceeded {
                            turn,
                            operation_id,
                            reservation_id,
                            budget,
                        },
                    )
                    .await;
                }
                KernelObservation::BudgetUsageReported {
                    operation_id,
                    reservation_id,
                    tokens,
                    subagents,
                    rounds,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::BudgetUsageReported {
                            turn,
                            operation_id,
                            reservation_id,
                            tokens,
                            subagents,
                            rounds,
                        },
                    )
                    .await;
                }
                KernelObservation::OperationCancelled {
                    turn,
                    operation_id,
                    reason,
                    pending_call_ids,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::OperationCancelled {
                            turn,
                            operation_id,
                            reason,
                            pending_call_ids,
                        },
                    )
                    .await;
                }
                KernelObservation::Suspended { .. }
                | KernelObservation::ApprovalResolutionFailed { .. } => {}
                KernelObservation::Resumed { .. } => {}
                // R3-1: submission bookkeeping — the rust SDK has no workflow driver, so the
                // base-index observation has no session record to enrich here.
                KernelObservation::WorkflowNodesSubmitted { .. } => {}
                // ③ loop-agent pacing: the rust SDK has no loop driver yet; the decision also
                // rides LoopResult.pace_decision for embedders that want it.
                KernelObservation::RoundPaced { .. } => {}
                KernelObservation::MemoryWritten {
                    turn,
                    record_id,
                    scope,
                    memory_kind,
                    name,
                    size_bytes,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryWritten {
                            turn,
                            record_id,
                            scope,
                            memory_kind,
                            name,
                            size_bytes,
                        },
                    )
                    .await;
                }
                KernelObservation::MemoryQueried {
                    turn,
                    scope,
                    query,
                    requested_k,
                    requires_async_response,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryQueried {
                            turn,
                            scope,
                            query,
                            requested_k,
                            requires_async_response,
                        },
                    )
                    .await;
                }
                // Phase 7 / M3: no dedicated session kinds yet in rust SDK.
                KernelObservation::MemoryValidationFailed {
                    turn,
                    record_id,
                    error,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MemoryValidationFailed {
                            turn,
                            record_id,
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
                    spool_ref,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::LargeResultSpooled {
                            turn,
                            call_id,
                            tool,
                            original_size,
                            preview_size,
                            spool_ref,
                        },
                    )
                    .await;
                }
                KernelObservation::LargeResultSpoolFailed { .. } => {}
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

    async fn archive_semantic_page_out(&self, archived: Vec<Message>, action: Option<String>) {
        let (Some(_store), Some(agent_id), Some(scope)) = (
            &self.opts.dream_store,
            &self.opts.agent_id,
            &self.opts.memory_scope,
        ) else {
            return;
        };

        let summary = match self.summarize_for_long_term_memory(&archived).await {
            Ok(s) => s,
            Err(_) => return, // non-fatal
        };

        // P2 write-funnel: route through the ONE gated write_memory syscall so validation,
        // the rolling write quota, dedup, and the memory_written audit all apply. Score is
        // advisory (0.6) — an automatic summary must never outrank curated content.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let name = format!("page-out-{now}");
        let request = MemoryRecord {
            record_id: format!("{}:{}:project:{}", scope.tenant_id, scope.namespace, name),
            scope: scope.clone(),
            name,
            kind: MemoryKind::Project,
            content: summary,
            description: format!(
                "auto summary of {} archive",
                action.as_deref().unwrap_or("compaction")
            ),
            provenance: MemoryProvenance {
                session_id: self.opts.session_id.clone(),
                author: MemoryAuthor::Extraction,
                trust: MemoryTrustLevel::Untrusted,
                evidence_refs: Vec::new(),
            },
            created_at: now,
            updated_at: now,
            last_recalled_at: None,
            recall_count: 0,
            confidence: 0.6,
            links: Vec::new(),
            pinned: false,
            ttl_days: None,
        };
        let _ = self.write_memory(request, None, Some(agent_id)).await;
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
            state_turn: None,
            frozen_prefix_len: None,
            budget_overflow: None,
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
                deepstrike_core::types::message::ContentPart::ToolResult { output, .. } => {
                    Some(output.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn action_str_of(action: KernelPressureAction) -> String {
    match action {
        KernelPressureAction::None => "none".to_string(),
        KernelPressureAction::SnipCompact => "snip_compact".to_string(),
        KernelPressureAction::MicroCompact => "micro_compact".to_string(),
        KernelPressureAction::ContextCollapse => "context_collapse".to_string(),
        KernelPressureAction::AutoCompact => "auto_compact".to_string(),
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

fn cancellation_reason_code(reason: CancellationReason) -> u8 {
    match reason {
        CancellationReason::User => 0,
        CancellationReason::Deadline => 1,
        CancellationReason::LeaseLost => 2,
        CancellationReason::HostShutdown => 3,
    }
}

fn cancellation_reason_from_code(code: u8) -> CancellationReason {
    match code {
        1 => CancellationReason::Deadline,
        2 => CancellationReason::LeaseLost,
        3 => CancellationReason::HostShutdown,
        _ => CancellationReason::User,
    }
}

fn pending_call_ids(action: &KernelAction) -> Vec<String> {
    match &action.effect {
        KernelEffect::CallProvider { .. } => vec![action.effect_id.clone()],
        KernelEffect::ExecuteTool { calls } => {
            calls.iter().map(|call| call.id.to_string()).collect()
        }
        KernelEffect::RequestApproval { requests } => requests
            .iter()
            .map(|request| request.call_id.clone())
            .collect(),
        KernelEffect::SpawnWorkflow { nodes, .. } => {
            nodes.iter().map(|node| node.agent_id.clone()).collect()
        }
        KernelEffect::PreemptSubAgents { agent_ids, .. } => agent_ids.clone(),
        KernelEffect::Done { .. } => Vec::new(),
        _ => vec![action.effect_id.clone()],
    }
}

/// Map the ergonomic [`MemoryPolicy`] onto the flat `set_memory_policy` kernel event.
fn memory_policy_event(policy: MemoryPolicy) -> KernelInputEvent {
    KernelInputEvent::SetMemoryPolicy {
        memory_path: policy.memory_path,
        stale_warning_days: policy.stale_warning_days,
        retrieval_top_k: policy.retrieval_top_k,
        validation_enabled: policy.validation_enabled,
        max_content_bytes: policy.max_content_bytes,
        max_name_length: policy.max_name_length,
    }
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
        state_turn: None,
        frozen_prefix_len: None,
        budget_overflow: None,
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
        // Directives are promoted in-kernel from acted-on signals; the SDK update path leaves them
        // untouched here (use `..` semantics) unless a future control plane curates them explicitly.
        directives: None,
    }
}
