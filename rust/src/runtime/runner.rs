use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::runtime::sandboxed_skill::scan_skill_dir;
use crate::runtime::skill_watcher::SkillWatcher;
use async_stream::try_stream;
use deepstrike_core::governance::quota::ResourceQuota;
use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord,
    MemoryScope, MemoryTrustLevel, validate_memory_write,
};
use deepstrike_core::runtime::kernel::wire::{CancellationReason, MemoryPolicy};
use deepstrike_core::runtime::kernel::{KernelObservation, KernelPressureAction};
use deepstrike_core::runtime::session::SessionEvent;
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
use crate::runtime::canonical_kernel::CanonicalKernel;
use crate::runtime::canonical_runner_runtime::{
    CanonicalRunnerOptions, CanonicalRunnerRuntime, PersistPayloadFn, PersistedPayload,
    canonical_kernel_action, canonical_kernel_apply,
};
use crate::runtime::execution_plane::{
    ExecutionPlane, LocalExecutionPlane, PermissionRequest, PermissionRequestHandler,
    PermissionResponse, RunContext, ToolSuspendHandler,
};
use crate::runtime::host_projection::{HostAction, HostEffect};
use crate::runtime::os_profile::{
    GovernancePolicy, OsProfile, SignalPolicy, assert_native_profile,
};
use crate::runtime::payload_store::{FilePayloadStore, PayloadStore};
use crate::runtime::provider_replay::{peek_provider_replay, seed_provider_replay_from_events};
use crate::runtime::replay::{
    is_mid_run, replay_messages_with_cap, replay_messages_with_cap_and_loader,
};
use crate::runtime::session_log::{SessionEntry, SessionLog};
use crate::runtime::{InMemoryKernelJournal, KernelJournal};
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

/// Canonical recovery and input-bound overrides. Omitted fields retain core defaults.
#[derive(Debug, Clone, Default)]
pub struct KernelReliability {
    pub provider_recovery_attempts: Option<u8>,
    pub output_recovery_attempts: Option<u8>,
    pub max_input_bytes: Option<u32>,
}

/// Configuration for a `RuntimeRunner` (aligned with Node/Python `RuntimeOptions`).
pub struct RuntimeOptions {
    pub provider: Box<dyn LLMProvider>,
    pub execution_plane: Option<Box<dyn ExecutionPlane>>,
    pub session_log: Option<Arc<dyn SessionLog>>,
    pub compression_store: Option<Arc<dyn ArchiveStore>>,
    /// Storage for canonical opaque external payload locators.
    pub payload_store: Option<Arc<dyn PayloadStore>>,
    /// Bounded recovery and replay policy. Omitted fields retain kernel defaults.
    pub kernel_reliability: Option<KernelReliability>,
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
    /// The run's **exposure ceiling** — the outer bound on what this run may EVER advertise. Not a
    /// static profile: an INTERSECTION applied every turn (`exposed ⊆ ceiling`), so `baseline_tool_ids`,
    /// `stable_core_tool_ids`, and skill `allowed_tools` all narrow *within* it and none can widen
    /// past it. The kernel meta-tools (skill/memory/knowledge/update_plan/read_result) are exempt on
    /// the id axis; the KIND axis still applies. Lowers to the same `capability_filter` sub-agents
    /// use; byte-stable across the run, so it never busts the prompt-cache prefix. Augments
    /// `run_spec`'s filter when both are set; synthesizes a minimal top-level spec otherwise.
    /// `None`/empty ⇒ no ceiling (no config = old); use `baseline_tool_ids` for a minimal surface.
    pub allowed_tool_ids: Option<Vec<String>>,
    /// The PRE-ACTIVATION exposure surface, selected from under the `allowed_tool_ids` ceiling
    /// (`AgentRunSpec::exposure_baseline`). Makes narrow→wide progressive disclosure expressible:
    /// `exposed = meta ∪ ((baseline ∪ stable_core ∪ ⋃ active skills' allowed_tools) ∩ ceiling)`.
    /// `None` ⇒ legacy behavior; `Some(vec![])` is DISTINCT and legitimate — the minimal surface
    /// (meta-tools + stable-core only). Entries outside the ceiling silently intersect away.
    pub baseline_tool_ids: Option<Vec<String>>,
    /// P1 dispatch enforcement (`OperationConfig.feature_policy.tool_dispatch_gate`). `None` ⇒ the kernel default
    /// `"exposed"`: fail-closed, a call to a tool this run never advertised commits a model-visible
    /// `governance_denied` result instead of executing. `Some("registered")` is the escape hatch
    /// restoring the pre-gate permissive dispatch.
    pub tool_dispatch_gate: Option<String>,
    /// P0-C: optional per-turn metrics sink for tool-gating telemetry (see [`TurnMetrics`]). Pure
    /// observation; invoked once per LLM turn. Panics are not caught — keep the sink trivial.
    pub on_turn_metrics: Option<OnTurnMetricsHandler>,
    /// P1-B/D stable-core: tool ids always exposed under skill gating. Empty ⇒ skills narrow to
    /// exactly their declared tools + meta-tools. Opt-in: no skill declaring tools ⇒ never engages.
    pub stable_core_tool_ids: Vec<String>,
    pub on_milestone_evaluate: Option<MilestoneEvaluationHandler>,
}

/// P0-A: compute the effective top-level run spec from an optional explicit `run_spec`, an optional
/// `allowed_tool_ids` ceiling, and an optional `baseline_tool_ids` pre-activation surface. Each
/// augments an explicit spec, or synthesizes a minimal `custom`-role spec when none is given.
/// Returns `None` when all are unset ⇒ no gating (no config = old behavior).
///
/// The two id-lists use DIFFERENT presence idioms on purpose: an empty `allowed_tool_ids` means
/// "unset" (the runner-wide "empty = no gating" convention), while an empty `baseline_tool_ids` is
/// the legitimate minimal surface and must reach the kernel as `Some(vec![])`.
fn build_run_spec(
    explicit: Option<deepstrike_core::types::agent::AgentRunSpec>,
    allowed_tool_ids: Option<&[String]>,
    baseline_tool_ids: Option<&[String]>,
    verification_contract_id: Option<&str>,
    agent_id: Option<&str>,
    session_id: &str,
    goal: &str,
) -> Option<deepstrike_core::types::agent::AgentRunSpec> {
    use deepstrike_core::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
    let profile = allowed_tool_ids.filter(|ids| !ids.is_empty());
    let mut spec = match (explicit, profile) {
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
        (None, None) => baseline_tool_ids.map(|_| {
            AgentRunSpec::new(
                AgentIdentity::new(agent_id.unwrap_or("root"), session_id),
                AgentRole::Custom,
                goal.to_string(),
            )
        }),
    };
    if spec.is_none() && verification_contract_id.is_some() {
        spec = Some(AgentRunSpec::new(
            AgentIdentity::new(agent_id.unwrap_or("root"), session_id),
            AgentRole::Custom,
            goal.to_string(),
        ));
    }
    if let (Some(spec), Some(baseline)) = (spec.as_mut(), baseline_tool_ids) {
        spec.exposure_baseline = Some(baseline.iter().map(|s| s.as_str().into()).collect());
    }
    if let (Some(spec), Some(contract_id)) = (spec.as_mut(), verification_contract_id)
        && spec.verification_contract_id.is_none()
    {
        spec.verification_contract_id = Some(contract_id.into());
    }
    spec
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Orchestrates the agentic turn loop via the runtime kernel + session event log.
pub struct RuntimeRunner {
    opts: RuntimeOptions,
    plane: Box<dyn ExecutionPlane>,
    kernel_journal: Arc<dyn KernelJournal>,
    interrupted: AtomicBool,
    cancellation_reason: AtomicU8,
    active_kernel:
        std::sync::Mutex<Option<std::sync::Arc<tokio::sync::Mutex<CanonicalRunnerRuntime>>>>,
    memory_write_timestamps: tokio::sync::Mutex<std::collections::VecDeque<u64>>,
    local_page_out_cache: std::sync::Mutex<Vec<Message>>,
}

impl RuntimeRunner {
    pub fn new(opts: RuntimeOptions) -> Self {
        Self::new_with_kernel_journal(opts, Arc::new(InMemoryKernelJournal::new()))
    }

    /// Construct a runner with an explicit durable canonical journal implementation.
    pub fn new_with_kernel_journal(
        mut opts: RuntimeOptions,
        kernel_journal: Arc<dyn KernelJournal>,
    ) -> Self {
        if opts.payload_store.is_none() {
            opts.payload_store = Some(Arc::new(FilePayloadStore::new(".payloads")));
        }
        let plane = opts
            .execution_plane
            .take()
            .unwrap_or_else(|| Box::new(LocalExecutionPlane::new()));
        Self {
            opts,
            plane,
            kernel_journal,
            interrupted: AtomicBool::new(false),
            cancellation_reason: AtomicU8::new(0),
            active_kernel: std::sync::Mutex::new(None),
            memory_write_timestamps: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
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

        let turn = self.active_kernel_turn().await;
        let validation = match self.opts.memory_policy.as_ref() {
            Some(policy) if policy.validation_enabled == Some(false) => Ok(()),
            Some(policy) => {
                let mut validation = deepstrike_core::mm::memory::MemoryValidation::default();
                if let Some(max_content_bytes) = policy.max_content_bytes {
                    validation.max_size_bytes = max_content_bytes;
                }
                if let Some(max_name_length) = policy.max_name_length {
                    validation.max_name_length = max_name_length as usize;
                }
                validation.validate(&memory)
            }
            None => validate_memory_write(&memory),
        };
        if let Err(error) = validation {
            self.append_memory_syscall_observations(
                session_id,
                vec![KernelObservation::MemoryValidationFailed {
                    turn,
                    record_id: memory.record_id.clone(),
                    error: format!("{error:?}"),
                }],
            )
            .await;
            return Ok(());
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let write_limit = self
            .opts
            .resource_quota
            .as_ref()
            .and_then(|quota| quota.memory_writes_per_window);
        let mut quota_guard = if write_limit.is_some() {
            Some(self.memory_write_timestamps.lock().await)
        } else {
            None
        };
        if let (Some((max_writes, window_ms)), Some(timestamps)) =
            (write_limit, quota_guard.as_mut())
        {
            let cutoff = now_ms.saturating_sub(window_ms);
            while timestamps
                .front()
                .is_some_and(|timestamp| *timestamp < cutoff)
            {
                timestamps.pop_front();
            }
            if window_ms == 0 || timestamps.len() >= max_writes as usize {
                drop(quota_guard);
                self.append_memory_syscall_observations(
                    session_id,
                    vec![KernelObservation::MemoryValidationFailed {
                        turn,
                        record_id: memory.record_id.clone(),
                        error: format!(
                            "memory write quota exceeded: max {max_writes} writes per {window_ms}ms"
                        ),
                    }],
                )
                .await;
                return Ok(());
            }
        }

        store.upsert(agent_id, memory.clone()).await?;
        if let Some(timestamps) = quota_guard.as_mut() {
            timestamps.push_back(now_ms);
        }
        drop(quota_guard);
        self.append_memory_syscall_observations(
            session_id,
            vec![KernelObservation::MemoryWritten {
                turn,
                record_id: memory.record_id,
                scope: memory.scope,
                memory_kind: memory.kind,
                name: memory.name,
                size_bytes: memory.content.len() as u32,
            }],
        )
        .await;
        Ok(())
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

        let turn = self.active_kernel_turn().await;
        let mut canonical_query = query;
        if let Some(top_k) = self
            .opts
            .memory_policy
            .as_ref()
            .and_then(|policy| policy.retrieval_top_k)
        {
            canonical_query.top_k = canonical_query.top_k.min(top_k as usize);
        }
        let hits = store.search(agent_id, &canonical_query).await?;
        self.append_memory_syscall_observations(
            session_id,
            vec![KernelObservation::MemoryQueried {
                turn,
                scope: canonical_query.scope.clone(),
                query: canonical_query.query.clone(),
                requested_k: canonical_query.top_k,
                requires_async_response: true,
            }],
        )
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
            Message::system(
                "Extract durable, reusable facts from this completed session. Return only JSON; do not include transient progress or guesses.",
            ),
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

    async fn log_memory_retrieval_result(&self, session_id: Option<&str>, hits: Vec<MemoryRecall>) {
        let Some(session_id) = session_id.or(self.opts.session_id.as_deref()) else {
            return;
        };
        // The session-log record is the durable audit artifact; the kernel needs no
        // acknowledgment (the former kernel event was a no-op and was removed).
        self.log(session_id, SessionEvent::MemoryRetrievalResult { hits })
            .await;
    }

    /// Test-only probe: the live kernel's `pending_effects` size. Valid while the run stream is
    /// suspended at a `yield` (the active-kernel guard is still in scope); `None` once the run
    /// generator has finished. Used by the effect-leak regressions (R-B27).
    #[cfg(test)]
    pub(crate) fn active_pending_effect_count(&self) -> Option<usize> {
        self.active_kernel
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|kernel| {
                kernel
                    .try_lock()
                    .ok()
                    .map(|runtime| runtime.pending_effect_count())
            })
    }

    async fn active_kernel_turn(&self) -> u32 {
        let active = self.active_kernel.lock().unwrap().clone();
        match active {
            Some(kernel) => kernel.lock().await.turn(),
            None => 0,
        }
    }

    fn create_canonical_runtime(
        &self,
        operation_id: String,
        session_id: &str,
    ) -> Result<CanonicalRunnerRuntime> {
        let provider_policy = self.opts.provider.runtime_policy();
        let effective_max_turns = self
            .opts
            .max_turns
            .or(provider_policy.max_turns)
            .unwrap_or(25);
        let effective_timeout = self.opts.timeout_ms.or(provider_policy.timeout_ms);
        let payload_store = self
            .opts
            .payload_store
            .clone()
            .expect("runtime constructor installs a payload store");
        let payload_session = session_id.to_string();
        let persist_payload: PersistPayloadFn =
            Arc::new(move |_call_id, content, preview_bytes| {
                let payload_store = payload_store.clone();
                let payload_session = payload_session.clone();
                Box::pin(async move {
                    let digest = deepstrike_core::runtime::kernel::wire::canonical_digest(
                        content.as_bytes(),
                    )
                    .as_str()
                    .to_string();
                    let payload_ref = format!(
                        "payload:{}",
                        digest
                            .trim_start_matches("sha256:")
                            .chars()
                            .take(32)
                            .collect::<String>()
                    );
                    payload_store.persist(&payload_session, &payload_ref, &content)?;
                    Ok(PersistedPayload {
                        payload_ref,
                        digest,
                        original_size: content.len().to_string(),
                        preview: utf8_prefix(&content, preview_bytes).to_string(),
                    })
                })
            });
        let mut runtime = CanonicalRunnerRuntime::new(
            CanonicalKernel::default(),
            self.kernel_journal.clone(),
            operation_id,
            CanonicalRunnerOptions {
                max_context_tokens: self.opts.max_tokens,
                max_turns: Some(effective_max_turns),
                max_total_tokens: None,
                max_wall_ms: effective_timeout,
                memory_binding_id: self
                    .opts
                    .agent_id
                    .clone()
                    .unwrap_or_else(|| format!("memory:{session_id}")),
                persist_payload: Some(persist_payload),
            },
        )?;
        if let Some(contract) = self.opts.milestone_contract.as_ref() {
            runtime.remember_milestone_contract(contract);
        }
        Ok(runtime)
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
        self.run_streaming_with_attachments(goal, criteria, extensions, session_id, &[])
            .await
    }

    /// Like [`Self::run_streaming`], but seeds multimodal `attachments` into kernel history
    /// before the first render (parity with Node/Python `run({ attachments })`).
    pub async fn run_streaming_with_attachments<'a>(
        &'a self,
        goal: &'a str,
        criteria: &'a [String],
        extensions: Option<&'a serde_json::Value>,
        session_id: Option<&'a str>,
        attachments: &'a [deepstrike_core::types::message::ContentPart],
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + 'a>>> {
        let session_id = session_id
            .map(str::to_string)
            .or_else(|| self.opts.session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let prior = self.read_entries(&session_id).await?;
        let mut mid_run = is_mid_run(&prior);
        if !mid_run {
            if let Some(operation_id) = prior.iter().rev().find_map(|entry| match &entry.event {
                SessionEvent::RunStarted { run_id, .. } => Some(run_id.clone()),
                _ => None,
            }) {
                if self.kernel_journal.head(&operation_id).await?.is_some() {
                    let mut authoritative =
                        self.create_canonical_runtime(operation_id, &session_id)?;
                    authoritative.restore().await?;
                    mid_run = !authoritative.is_terminal();
                }
            }
        }

        let operation_id = if mid_run {
            prior
                .iter()
                .rev()
                .find_map(|entry| match &entry.event {
                    SessionEvent::RunStarted { run_id, .. } => Some(run_id.clone()),
                    _ => None,
                })
                .ok_or_else(|| {
                    Error::Other(format!(
                        "mid-run session has no run_started identity: {session_id}"
                    ))
                })?
        } else {
            let run_id = uuid::Uuid::new_v4().to_string();
            self.log(
                &session_id,
                SessionEvent::RunStarted {
                    run_id: run_id.clone(),
                    goal: goal.to_string(),
                    criteria: criteria.to_vec(),
                    agent_id: self.opts.agent_id.clone(),
                    system_prompt: self.opts.system_prompt.clone(),
                    attachments: attachments.to_vec(),
                },
            )
            .await;
            run_id
        };

        let goal_owned = goal.to_string();
        let criteria_owned = criteria.to_vec();
        let extensions_owned = extensions.cloned();
        let attachments_owned = attachments.to_vec();
        let prior_events = if prior.is_empty() { None } else { Some(prior) };

        Ok(Box::pin(self.execute_inner(
            session_id,
            operation_id,
            goal_owned,
            criteria_owned,
            extensions_owned,
            prior_events,
            mid_run,
            attachments_owned,
        )))
    }

    pub async fn wake_streaming(
        &self,
        session_id: &str,
        extensions: Option<&serde_json::Value>,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + '_>>> {
        let prior = self.read_entries(session_id).await?;
        let (start_index, start) = prior
            .iter()
            .enumerate()
            .rev()
            .find(|(_, entry)| matches!(entry.event, SessionEvent::RunStarted { .. }))
            .ok_or_else(|| Error::Other(format!("no run_started for session: {session_id}")))?;
        let (operation_id, goal, criteria, attachments) = match &start.event {
            SessionEvent::RunStarted {
                run_id,
                goal,
                criteria,
                attachments,
                ..
            } => (
                run_id.clone(),
                goal.clone(),
                criteria.clone(),
                attachments.clone(),
            ),
            _ => unreachable!(),
        };

        if prior[start_index + 1..]
            .iter()
            .any(|entry| matches!(entry.event, SessionEvent::RunTerminal { .. }))
        {
            if self.kernel_journal.head(&operation_id).await?.is_none() {
                return Err(Error::Other(
                    "run_terminal projection has no canonical journal".into(),
                ));
            }
            let mut authoritative =
                self.create_canonical_runtime(operation_id.clone(), session_id)?;
            authoritative.restore().await?;
            if authoritative.is_terminal() {
                return Ok(Box::pin(futures::stream::empty()));
            }
        }

        Ok(Box::pin(self.execute_inner(
            session_id.to_string(),
            operation_id,
            goal,
            criteria,
            extensions.cloned(),
            Some(prior),
            true,
            attachments,
        )))
    }

    pub async fn wake(&self, session_id: &str) -> Result<String> {
        collect_text(self.wake_streaming(session_id, None).await?).await
    }

    fn execute_inner(
        &self,
        session_id: String,
        operation_id: String,
        goal: String,
        criteria: Vec<String>,
        extensions: Option<serde_json::Value>,
        prior_events: Option<Vec<SessionEntry>>,
        resume_mid_run: bool,
        attachments: Vec<deepstrike_core::types::message::ContentPart>,
    ) -> impl futures::Stream<Item = Result<RunEvent>> + '_ {
        try_stream! {
            self.interrupted.store(false, Ordering::Relaxed);
            self.cancellation_reason.store(0, Ordering::Relaxed);

            if let Some(ks) = &self.opts.knowledge_source {
                ks.init().await?;
            }

            let mut runtime = self.create_canonical_runtime(operation_id, &session_id)?;
            if resume_mid_run {
                runtime.restore().await?;
            }
            let kernel = std::sync::Arc::new(tokio::sync::Mutex::new(runtime));
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

            let mut pending_observations = Vec::new();
            let mut pending_page_out_starts = std::collections::VecDeque::new();
            let mut active_page_out_start = None;
            let skill_watcher = self.opts.skill_dir.as_deref().and_then(SkillWatcher::start);

            if !resume_mid_run {
                if self.opts.kernel_reliability.is_some()
                    || self.opts.scheduler_policy.is_some()
                    || self.opts.tool_dispatch_gate.is_some()
                {
                    let mut config = serde_json::Map::new();
                    if let Some(reliability) = self.opts.kernel_reliability.as_ref() {
                        config.insert(
                            "reliability".into(),
                            serde_json::json!({
                                "provider_recovery_attempts": reliability.provider_recovery_attempts,
                                "output_recovery_attempts": reliability.output_recovery_attempts,
                                "max_input_bytes": reliability.max_input_bytes,
                            }),
                        );
                    }
                    if let Some(policy) = self.opts.scheduler_policy {
                        let policy = serde_json::to_value(policy).map_err(|error| {
                            Error::Other(format!("scheduler policy is not serializable: {error}"))
                        })?;
                        config.insert("scheduler_policy".into(), policy);
                    }
                    if let Some(gate) = self.opts.tool_dispatch_gate.as_ref() {
                        config.insert("tool_dispatch_gate".into(), gate.clone().into());
                    }
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({
                            "kind": "configure_run",
                            "config": config,
                        }),
                    )
                    .await?;
                }

                if let Some(tokenizer_name) = &self.opts.tokenizer {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({ "kind": "set_tokenizer", "name": tokenizer_name }),
                    ).await?;
                }
                if let Some(enabled) = self.opts.enable_plan_tool {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({ "kind": "set_plan_tool_enabled", "enabled": enabled }),
                    ).await?;
                }

                kernel_apply(
                    &kernel,
                    &mut pending_observations,
                    serde_json::json!({ "kind": "set_tools", "tools": self.plane.schemas() }),
                ).await?;

                if self.opts.dream_store.is_some() && self.opts.agent_id.is_some() {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({ "kind": "set_memory_enabled", "enabled": true }),
                    ).await?;
                }
                if self.opts.knowledge_source.is_some() {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({ "kind": "set_knowledge_enabled", "enabled": true }),
                    ).await?;
                }

                if let Some(sp) = &self.opts.system_prompt {
                    let tokens = ((sp.len() / 4) as u32).max(1);
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({
                            "kind": "add_system_message",
                            "content": sp,
                            "tokens": tokens,
                        }),
                    ).await?;
                }
                for mem in &self.opts.initial_memory {
                    let tokens = ((mem.len() / 4) as u32).max(1);
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({
                            "kind": "add_knowledge_message",
                            "content": mem,
                            "tokens": tokens,
                            "pinned": false,
                        }),
                    ).await?;
                }

                if let Some(skill_dir) = &self.opts.skill_dir {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({
                            "kind": "set_available_skills",
                            "skills": scan_skill_dir(skill_dir),
                        }),
                    ).await?;
                }

                // P1-B/D: configure stable-core tool ids (always exposed under skill gating).
                if !self.opts.stable_core_tool_ids.is_empty() {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({
                            "kind": "set_stable_core_tools",
                            "tool_ids": self.opts.stable_core_tool_ids,
                        }),
                    ).await?;
                }

                if let Some(milestones) = self.opts.milestone_contract.clone() {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({
                            "kind": "load_milestone_contract",
                            "contract": milestones,
                        }),
                    ).await?;
                }

                let max_bytes = {
                    let k = kernel.lock().await;
                    k.recovery_content_bytes()
                };

                if let Some(ref events) = prior_events {
                    seed_provider_replay_from_events(self.opts.provider.as_ref(), events);

                    let messages = if let Some(ref store) = self.opts.compression_store {
                        let store_clone = store.clone();
                        replay_messages_with_cap_and_loader(events, max_bytes, move |archive_ref| {
                            store_clone.read(archive_ref).map_err(|_| {
                                deepstrike_core::context::fault::ContextFault::MissingArchive {
                                    session_id: String::new(),
                                    seq: 0,
                                }
                            })
                        })
                    } else {
                        replay_messages_with_cap(events, max_bytes)
                    };

                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({ "kind": "preload_history", "messages": messages }),
                    ).await?;
                }
            } else if let Some(ref events) = prior_events {
                seed_provider_replay_from_events(self.opts.provider.as_ref(), events);
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

            if !resume_mid_run {
                let os_profile = assert_native_profile(self.opts.os_profile.clone())?;
                let governance_policy = self
                    .opts
                    .governance_policy
                    .clone()
                    .unwrap_or(os_profile.governance_policy);
                kernel_apply(
                    &kernel,
                    &mut pending_observations,
                    governance_policy.into_host_fact(),
                ).await?;

                let signal_policy = self
                    .opts
                    .signal_policy
                    .unwrap_or(os_profile.signal_policy);
                kernel_apply(
                    &kernel,
                    &mut pending_observations,
                    serde_json::json!({
                        "kind": "set_signal_policy",
                        "policy": signal_policy.into_kernel(),
                    }),
                ).await?;

                if let Some(quota) = self.opts.resource_quota.clone() {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({ "kind": "set_resource_quota", "quota": quota }),
                    ).await?;
                }

                if let Some(policy) = self.opts.memory_policy.clone() {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        memory_policy_host_fact(policy),
                    ).await?;
                }

                // Multimodal upload: seed attachments before the canonical root start (Node/Python parity).
                if !resume_mid_run && !attachments.is_empty() {
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({
                            "kind": "add_history_message",
                            "message": Message::user_multimodal(attachments.clone()),
                        }),
                    ).await?;
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
                                &kernel,
                                &mut pending_observations,
                                serde_json::json!({
                                    "kind": "add_history_message",
                                    "message": Message::user(recalled.join("\n")),
                                }),
                            ).await?;
                        }
                    }
                }
            }

            let mut action = if resume_mid_run {
                let mut runtime = kernel.lock().await;
                let action = runtime.resume_action()?.ok_or_else(|| {
                    Error::Other(
                        "restored canonical operation has no pending effect or terminal".into(),
                    )
                })?;
                pending_observations.extend(runtime.drain_host_observations());
                action
            } else {
                // P0-A: fold an explicit `run_spec`, the `allowed_tool_ids` ceiling, and/or the
                // `baseline_tool_ids` pre-activation surface into the kernel run spec (reuses the
                // existing run_spec wire — no new ABI).
                let run_spec = build_run_spec(
                    self.opts.run_spec.clone(),
                    self.opts.allowed_tool_ids.as_deref(),
                    self.opts.baseline_tool_ids.as_deref(),
                    self.opts
                        .milestone_contract
                        .as_ref()
                        .map(|_| "rust-default"),
                    self.opts.agent_id.as_deref(),
                    &session_id,
                    &goal,
                );
                kernel_start_agent(
                    &kernel,
                    &mut pending_observations,
                    RuntimeTask::new(&goal).with_criteria(criteria),
                    run_spec,
                ).await?
            };

            let mut last_skill_version: u64 = skill_watcher.as_ref().map(|w| w.version()).unwrap_or(0);

            while !kernel.lock().await.is_terminal() {
                // Hot-reload: refresh skill catalog if the watcher detected changes.
                if let (Some(watcher), Some(skill_dir)) =
                    (&skill_watcher, &self.opts.skill_dir)
                {
                    let cur = watcher.version();
                    if cur != last_skill_version {
                        last_skill_version = cur;
                        kernel_apply(
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "set_available_skills",
                                "skills": scan_skill_dir(skill_dir),
                            }),
                        ).await?;
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
                    let operation_id = kernel.lock().await.operation_id().to_string();
                    kernel_apply(
                        &kernel,
                        &mut pending_observations,
                        serde_json::json!({
                            "kind": "cancel_operation",
                            "operation_id": operation_id,
                            "reason": cancellation_reason_from_code(self.cancellation_reason.load(Ordering::Relaxed)),
                            "pending_call_ids": pending_call_ids(&action),
                        }),
                    ).await?;
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
                        // §7.7 · the claim's signal id is the business identity, kept verbatim; it
                        // no longer has to parse as a UUID.
                        kernel_sig.id = claim.signal_id.as_str().into();
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
                        let observation_start = pending_observations.len();
                        let signal_action = kernel_transition(
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "deliver_signal",
                                "delivery_id": claim.delivery_id,
                                "attempt": claim.delivery_attempt,
                                "signal": kernel_sig,
                            }),
                        )
                        .await;
                        let receipt = SignalDeliveryReceipt {
                            delivery_id: claim.delivery_id.clone(),
                            lease_token: claim.lease_token.clone(),
                        };
                        let signal_action = match signal_action {
                            Ok(action) => action,
                            Err(error) => {
                                let _ = ss.nack_signal(&receipt).await?;
                                Err(error)?
                            }
                        };
                        let disposition_matches = pending_observations[observation_start..]
                            .iter()
                            .filter(|observation| match observation {
                                KernelObservation::SignalDeliveryDisposed {
                                    delivery_id,
                                    attempt,
                                    ..
                                } => {
                                    delivery_id == &claim.delivery_id
                                        && attempt == &claim.delivery_attempt
                                }
                                _ => false,
                            })
                            .count()
                            == 1;
                        if !disposition_matches {
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
                        if let Some(sig_action) = signal_action {
                            action = sig_action;
                        }
                        // Critical attention/preemption is distinct from operation cancellation.
                    }
                }
                if kernel.lock().await.is_terminal() {
                    break;
                }

                match &action.effect {
                    HostEffect::CallProvider { context, tools } => {
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
                                    &kernel,
                                    &mut pending_observations,
                                    serde_json::json!({
                                        "kind": "provider_error",
                                        "effect_id": provider_effect_id,
                                        "message": msg,
                                    }),
                                ).await?;
                                // Withholding (query.ts parity): surface the raw provider error only
                                // when the kernel could NOT recover (it returned a terminal). On a
                                // recovered retry (CallProvider) the error stays hidden. `continue`
                                // re-enters the loop: a recovered turn persists its compaction
                                // archive at the loop's normal append point, and a terminal Done
                                // exits through `is_terminal()` into the run_terminal emit.
                                if matches!(&action.effect, HostEffect::Done { .. }) {
                                    yield RunEvent::Error(msg);
                                }
                                continue;
                            }
                        };

                        // R-B27/R-B29 sibling: an exception raised AFTER the first chunk must not
                        // escape through `?`. Doing so leaves the `call_provider` effect pending
                        // forever (nothing ever resolves it) and skips the kernel's reactive
                        // recovery ladder. Capture it here and feed `provider_error` below, exactly
                        // like the stream-open error path and like node/python.
                        let mut stream_error: Option<String> = None;
                        while let Some(evt) = provider_stream.next().await {
                            if self.interrupted.load(Ordering::Relaxed) {
                                break;
                            }
                            let evt = match evt {
                                Ok(evt) => evt,
                                Err(e) => {
                                    stream_error = Some(e.to_string());
                                    break;
                                }
                            };
                            match evt {
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
                            let operation_id = kernel.lock().await.operation_id().to_string();
                            action = kernel_action(
                                &kernel,
                                &mut pending_observations,
                                serde_json::json!({
                                    "kind": "cancel_operation",
                                    "operation_id": operation_id,
                                    "reason": cancellation_reason_from_code(self.cancellation_reason.load(Ordering::Relaxed)),
                                    "pending_call_ids": [provider_effect_id],
                                }),
                            ).await?;
                            break;
                        }

                        if let Some(msg) = stream_error {
                            // Same contract as the stream-open failure above: hand the raw provider
                            // error to the kernel, which resolves the pending provider effect and
                            // decides recover-and-retry (`CallProvider`) vs honest terminal (`Done`).
                            // Surface the error to the caller only when the kernel gave up, so a
                            // recovered turn does not emit a phantom failure.
                            action = kernel_action(
                                &kernel,
                                &mut pending_observations,
                                serde_json::json!({
                                    "kind": "provider_error",
                                    "effect_id": provider_effect_id,
                                    "message": msg,
                                }),
                            ).await?;
                            if matches!(&action.effect, HostEffect::Done { .. }) {
                                yield RunEvent::Error(msg);
                            }
                            continue;
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
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "provider_result",
                                "effect_id": provider_effect_id,
                                "message": assistant,
                                // Phase 4: stop_reason drives the kernel's max-output-tokens recovery.
                                "stop_reason": turn_stop_reason,
                            }),
                        ).await?;
                        self.log(
                            &session_id,
                            SessionEvent::LlmCompleted {
                                turn: kernel.lock().await.turn(),
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
                                turn: kernel.lock().await.turn(),
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
                    HostEffect::RequestApproval { requests } => {
                        let approval_effect_id = action.effect_id.clone();
                        let mut approved_calls = Vec::new();
                        let mut denied_calls = Vec::new();
                        for request in requests {
                            let arguments = request.arguments.to_string();
                            self.log(
                                &session_id,
                                SessionEvent::PermissionRequested {
                                    turn: kernel.lock().await.turn(),
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
                                    turn: kernel.lock().await.turn(),
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
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "approval_result",
                                "effect_id": approval_effect_id,
                                "approved_calls": approved_calls,
                                "denied_calls": denied_calls,
                            }),
                        ).await?;
                    }
                    HostEffect::SpawnWorkflow { nodes, .. } => {
                        // This runner has no workflow child orchestrator. Report each
                        // requested spawn as a completed failure instead of treating the
                        // action as an observation or leaving the effect unresolved.
                        let workflow_effect_id = action.effect_id.clone();
                        let failures: Vec<deepstrike_core::runtime::kernel::WorkflowSpawnFailure> = nodes
                            .into_iter()
                            .map(|node| deepstrike_core::runtime::kernel::WorkflowSpawnFailure {
                                agent_id: node.agent_id.clone(),
                                error: "Rust RuntimeRunner has no workflow orchestrator".to_string(),
                            })
                            .collect();
                        action = kernel_action(
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "workflow_spawn_result",
                                "effect_id": workflow_effect_id,
                                "started_agent_ids": [],
                                "failures": failures,
                            }),
                        ).await?;
                    }
                    HostEffect::PreemptSubAgents { .. } => {
                        // RuntimeRunner does not launch external child runners, so
                        // there is no host process to cancel before acknowledging.
                        let preempt_effect_id = action.effect_id.clone();
                        action = kernel_action(
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "preempt_result",
                                "effect_id": preempt_effect_id,
                            }),
                        ).await?;
                    }
                    HostEffect::PersistMemory { memory } => {
                        let effect_id = action.effect_id.clone();
                        let error = match (
                            self.opts.dream_store.as_ref(),
                            self.opts.agent_id.as_deref(),
                        ) {
                            (Some(store), Some(agent_id)) => {
                                let mut memory = memory.clone();
                                if let Some(scope) = self.opts.memory_scope.as_ref() {
                                    memory.scope = scope.clone();
                                }
                                memory.provenance.session_id = Some(session_id.clone());
                                store
                                    .upsert(agent_id, memory)
                                    .await
                                    .err()
                                    .map(|error| error.to_string())
                            }
                            _ => Some(
                                "memory persistence is unavailable without dream_store and agent_id"
                                    .to_string(),
                            ),
                        };
                        action = kernel_action(
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "memory_persist_result",
                                "effect_id": effect_id,
                                "error": error,
                            }),
                        ).await?;
                    }
                    HostEffect::QueryMemory { query, requested_k } => {
                        let effect_id = action.effect_id.clone();
                        let (hits, error) = match (
                            self.opts.dream_store.as_ref(),
                            self.opts.agent_id.as_deref(),
                        ) {
                            (Some(store), Some(agent_id)) => {
                                let mut query = query.clone();
                                query.top_k = *requested_k;
                                if let Some(scope) = self.opts.memory_scope.as_ref() {
                                    query.scope = scope.clone();
                                }
                                match store.search(agent_id, &query).await {
                                    Ok(hits) => (hits, None),
                                    Err(error) => (Vec::new(), Some(error.to_string())),
                                }
                            }
                            _ => (
                                Vec::new(),
                                Some(
                                    "memory query is unavailable without dream_store and agent_id"
                                        .to_string(),
                                ),
                            ),
                        };
                        if error.is_none() {
                            self.log_memory_retrieval_result(Some(&session_id), hits.clone())
                                .await;
                        }
                        action = kernel_action(
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "memory_query_result",
                                "effect_id": effect_id,
                                "hits": hits,
                                "error": error,
                            }),
                        ).await?;
                    }
                    HostEffect::ArchivePageOut { archived, tier, action: pressure_action, .. } => {
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
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "page_out_archive_result",
                                "effect_id": effect_id,
                                "archive_ref": archive_ref,
                                "error": error,
                            }),
                        ).await?;
                    }
                    HostEffect::LoadPayload { handle_id, payload_ref } => {
                        let effect_id = action.effect_id.clone();
                        let content = self
                            .opts
                            .payload_store
                            .as_ref()
                            .expect("runtime constructor installs a payload store")
                            .load(&session_id, payload_ref)?;
                        let event = match content {
                            Some(content) => serde_json::json!({
                                "kind": "payload_loaded",
                                "effect_id": effect_id,
                                "handle_id": handle_id,
                                "digest": deepstrike_core::runtime::kernel::wire::canonical_digest(
                                    content.as_bytes(),
                                )
                                .as_str(),
                                "original_size": content.len(),
                                "content": content,
                            }),
                            None => serde_json::json!({
                                "kind": "payload_load_failed",
                                "effect_id": effect_id,
                                "error": format!("payload is unavailable: {payload_ref}"),
                            }),
                        };
                        action = kernel_action(
                            &kernel,
                            &mut pending_observations,
                            event,
                        ).await?;
                    }
                    HostEffect::ExecuteTool { calls } => {
                        let tool_effect_id = action.effect_id.clone();
                        let tool_calls = calls.clone();
                        self.log(
                            &session_id,
                            SessionEvent::ToolRequested {
                                turn: kernel.lock().await.turn(),
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
                                &kernel,
                                &mut pending_observations,
                                serde_json::json!({ "kind": "update_task", "update": update }),
                            ).await?;
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
                                                turn: kernel.lock().await.turn(),
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
                                                turn: kernel.lock().await.turn(),
                                                call_id: call_id.clone(),
                                                tool_name: tool_name.clone(),
                                                reason: reason.clone(),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::ToolDenied { call_id, tool_name, reason };
                                    }
                                    RunEvent::PermissionRequest { call_id, tool_name, arguments, reason } => {
                                        let turn = kernel.lock().await.turn();
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
                                        let turn = kernel.lock().await.turn();
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
                                &kernel,
                                &mut pending_observations,
                                serde_json::json!({
                                    "kind": "update_task",
                                    "update": TaskUpdate {
                                        progress: Some(format!("Executed tools: {}", names.join(", "))),
                                        ..Default::default()
                                    },
                                }),
                            ).await?;
                        }

                        self.log(
                            &session_id,
                            SessionEvent::ToolCompleted {
                                turn: kernel.lock().await.turn(),
                                results: tool_results.clone(),
                            },
                        )
                        .await;

                        action = kernel_action(
                            &kernel,
                            &mut pending_observations,
                            serde_json::json!({
                                "kind": "tool_results",
                                "effect_id": tool_effect_id,
                                "results": tool_results,
                            }),
                        ).await?;
                    }
                    HostEffect::EvaluateMilestone {
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
                                &kernel,
                                &mut pending_observations,
                                serde_json::json!({
                                    "kind": "milestone_result",
                                    "effect_id": milestone_effect_id,
                                    "result": result,
                                }),
                            ).await?;
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
                                &kernel,
                                &mut pending_observations,
                                serde_json::json!({
                                    "kind": "milestone_result",
                                    "effect_id": milestone_effect_id,
                                    "result": result,
                                }),
                            ).await?;
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
                            // R-B27: no verifier and no evaluation hook. The run still suspends
                            // with `milestone_pending`, but the `evaluate_milestone` effect MUST be
                            // resolved first — returning without a result leaves a dangling entry
                            // in the kernel's `pending_effects`, which becomes an unresolvable
                            // pending item once logical-checkpoint recovery lands.
                            //
                            // `MilestoneCheckResult` has no error channel on the wire today
                            // (Phase 1 adds one), so the most conservative shape the current
                            // contract can express is `passed = false` with an explanatory
                            // `reason`: fail-closed, the phase does not advance and no capability
                            // is unlocked. The returned action is intentionally dropped — this
                            // branch terminates the run regardless.
                            let result = MilestoneCheckResult::fail(
                                phase_id.clone(),
                                "milestone unverified: no verifier configured and no host evaluation hook (fail-closed)",
                            );
                            let _unverified = kernel_action(
                                &kernel,
                                &mut pending_observations,
                                serde_json::json!({
                                    "kind": "milestone_result",
                                    "effect_id": milestone_effect_id,
                                    "result": result,
                                }),
                            ).await?;
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
                                    turns_used: kernel.lock().await.turn().max(1),
                                    total_tokens: 0,
                                },
                            )
                            .await;
                            yield RunEvent::Done {
                                iterations: kernel.lock().await.turn().max(1),
                                total_tokens: 0,
                                status: "milestone_pending".to_string(),
                            };
                            return;
                        }
                    }
                    HostEffect::Done { result } => {
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
                            let new_msgs = kernel.lock().await.drain_new_messages();
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
                HostEffect::Done { result } => (
                    format!("{:?}", result.termination).to_lowercase(),
                    result.turns_used.max(1),
                    result.total_tokens_used,
                ),
                _ => ("error".to_string(), kernel.lock().await.turn().max(1), 0),
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

            if let HostEffect::Done { .. } = &action.effect {
                if let (Some(store), Some(agent_id)) =
                    (&self.opts.dream_store, &self.opts.agent_id)
                {
                    let new_msgs = kernel.lock().await.drain_new_messages();
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
        kernel_mutex: &Arc<tokio::sync::Mutex<CanonicalRunnerRuntime>>,
        observations: &mut Vec<KernelObservation>,
        pending_page_out_starts: &mut std::collections::VecDeque<u64>,
        mut next_archive_start: u64,
    ) -> u64 {
        let drained = std::mem::take(observations);
        let (turn, preserved_refs, summary_tokens_by_index) = {
            let kernel = kernel_mutex.lock().await;
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
                // Payload residency is already durable in the canonical transaction record.
                KernelObservation::PayloadResidencyChanged { .. }
                | KernelObservation::PayloadLoadFailed { .. } => {}
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
                // M3/M4 lifecycle observations. Durable-store mirroring is a Node/Python SDK
                // concern; this Rust session-log loop does not persist them (parity follow-up).
                KernelObservation::MemoryRecalled { .. }
                | KernelObservation::PromotionSuggested { .. } => {}
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
                // §13.2 · live policy patches are not exposed through this SDK's public runner, so
                // no host path currently produces this canonical observation.
                KernelObservation::LivePolicyChanged { .. } => {}
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
                // Rejections are already durable in the kernel transaction record. Call-specific
                // APIs inspect the observation directly; the generic runner has no host effect.
                KernelObservation::ControlRequestRejected { .. } => {}
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

pub(crate) async fn kernel_apply(
    kernel: &Arc<tokio::sync::Mutex<CanonicalRunnerRuntime>>,
    pending_observations: &mut Vec<KernelObservation>,
    event: serde_json::Value,
) -> Result<()> {
    let mut runtime = kernel.lock().await;
    canonical_kernel_apply(&mut runtime, pending_observations, event).await
}

async fn kernel_transition(
    kernel: &Arc<tokio::sync::Mutex<CanonicalRunnerRuntime>>,
    pending_observations: &mut Vec<KernelObservation>,
    event: serde_json::Value,
) -> Result<Option<HostAction>> {
    let mut runtime = kernel.lock().await;
    let action = runtime.apply_host_event(event).await?;
    pending_observations.extend(runtime.drain_host_observations());
    Ok(action)
}

async fn kernel_action(
    kernel: &Arc<tokio::sync::Mutex<CanonicalRunnerRuntime>>,
    pending_observations: &mut Vec<KernelObservation>,
    event: serde_json::Value,
) -> Result<HostAction> {
    let mut runtime = kernel.lock().await;
    canonical_kernel_action(&mut runtime, pending_observations, event).await
}

async fn kernel_start_agent(
    kernel: &Arc<tokio::sync::Mutex<CanonicalRunnerRuntime>>,
    pending_observations: &mut Vec<KernelObservation>,
    task: RuntimeTask,
    run_spec: Option<deepstrike_core::types::agent::AgentRunSpec>,
) -> Result<HostAction> {
    let task = serde_json::to_value(task)
        .map_err(|error| Error::Other(format!("canonical task is not serializable: {error}")))?;
    let run_spec = run_spec
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| {
            Error::Other(format!("canonical run spec is not serializable: {error}"))
        })?;
    let mut runtime = kernel.lock().await;
    let action = runtime.start_agent_value(task, run_spec).await?;
    pending_observations.extend(runtime.drain_host_observations());
    action.ok_or_else(|| Error::Other("canonical agent root must return one host action".into()))
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

fn pending_call_ids(action: &HostAction) -> Vec<String> {
    match &action.effect {
        HostEffect::CallProvider { .. } => vec![action.effect_id.clone()],
        HostEffect::ExecuteTool { calls } => calls.iter().map(|call| call.id.to_string()).collect(),
        HostEffect::RequestApproval { requests } => requests
            .iter()
            .map(|request| request.call_id.clone())
            .collect(),
        HostEffect::SpawnWorkflow { nodes, .. } => {
            nodes.iter().map(|node| node.agent_id.clone()).collect()
        }
        HostEffect::PreemptSubAgents { agent_ids, .. } => agent_ids.clone(),
        HostEffect::Done { .. } => Vec::new(),
        _ => vec![action.effect_id.clone()],
    }
}

/// Map the ergonomic [`MemoryPolicy`] onto the SDK-owned bootstrap fact.
fn memory_policy_host_fact(policy: MemoryPolicy) -> serde_json::Value {
    let mut value = serde_json::to_value(policy).expect("canonical memory policy serializes");
    value
        .as_object_mut()
        .expect("canonical memory policy is a JSON object")
        .insert("kind".into(), serde_json::json!("set_memory_policy"));
    value
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
