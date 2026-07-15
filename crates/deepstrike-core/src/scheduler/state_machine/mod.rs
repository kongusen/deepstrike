use std::collections::{HashMap, VecDeque};

use super::entropy::{EntropyTracker, EntropyWatchConfig};
use super::milestone::MilestoneTracker;
use super::policy::SchedulerBudget;
use super::tcb::{TaskLifecycle, TaskTable, Tcb, WaitReason};
use crate::AgentRunSpec;
use crate::context::manager::ContextManager;
use crate::governance::pipeline::GovernancePipeline;
use crate::governance::repeat_fuse::RepeatFuseConfig;
use crate::signals::router::SignalRouter;
use crate::types::result::SubAgentResult;
use crate::context::renderer::RenderedContext;
// `pub use` so external integration tests that glob `state_machine::*` resolve the observation
// type here — exactly as they did for the former `pub enum LoopObservation` this replaced.
pub use crate::runtime::kernel::KernelObservation;
use crate::runtime::session::RollbackReason;
use crate::types::message::{
    Content, ContentPart, Message, ToolCall, ToolErrorKind, ToolResult, ToolSchema,
};
use crate::types::milestone::MilestoneCheckResult;
use crate::types::result::{LoopResult, TerminationReason};
use crate::types::task::RuntimeTask;

/// Compact digest of a tool call's arguments for the recency log (2b). Kept short and CJK-safe — it
/// only needs to make `same-tool / different-args` calls distinguishable (so a legit loop isn't
/// flagged as a no-progress repeat) and to read sensibly in the "just did: …" footer. Empty for
/// no-arg / `{}` calls. Lives in the volatile State turn, so length here never churns the cache.
fn compact_tool_args(args: &serde_json::Value) -> String {
    if args.is_null() {
        return String::new();
    }
    let s = args.to_string();
    if s == "{}" {
        return String::new();
    }
    const MAX: usize = 48;
    if s.chars().count() <= MAX {
        s
    } else {
        format!("{}…", s.chars().take(MAX).collect::<String>())
    }
}

/// The *turn step* of the L* execution loop (M1d).
///
/// Schedulability (`Ready/Running/Blocked/Suspended/Done`) is no longer carried here — it lives
/// on the root task's [`TaskLifecycle`] in the kernel's `TaskTable`, queried via
/// [`LoopStateMachine::lifecycle`]. `LoopPhase` is now orthogonal: it only records *which step of a
/// running turn* the loop is in. When the task is `Ready/Suspended/Done`, the phase value is
/// inert (left at its last step) and ignored.
#[derive(Debug, Clone)]
pub enum LoopPhase {
    Reason,
    Act { tool_calls: Vec<ToolCall> },
}

/// Events fed into the state machine from the SDK layer.
#[derive(Debug)]
pub enum LoopEvent {
    LLMResponse {
        message: Message,
    },
    ToolResults {
        results: Vec<ToolResult>,
    },
    /// Result of evaluating the current milestone phase's criteria.
    /// Feed this back after handling `LoopAction::EvaluateMilestone`.
    MilestoneResult {
        result: MilestoneCheckResult,
    },
    /// Sub-agent run completed — result is injected into the loop as context.
    SubAgentCompleted {
        result: SubAgentResult,
    },
    Complete,
    Timeout,
}

/// Actions the state machine outputs — SDK layer executes the I/O.
#[derive(Debug, Clone)]
pub enum LoopAction {
    /// Structured context ready for a provider call.
    /// `context.system_text` → provider system param.
    /// `context.turns`       → provider messages array (strictly alternating).
    /// `tools`               → tool schemas (skill / memory / knowledge / user tools).
    CallLLM {
        context: RenderedContext,
        tools: Vec<ToolSchema>,
    },
    ExecuteTools {
        calls: Vec<ToolCall>,
    },
    /// Host-owned approval effect. The kernel remains suspended until the host
    /// returns the correlated result through the ABI.
    RequestApproval {
        requests: Vec<ApprovalRequest>,
    },
    /// Host-owned workflow orchestration effect. The kernel has reserved the
    /// batch but records no spawn fact until the correlated result arrives.
    SpawnWorkflow {
        nodes: Vec<crate::orchestration::workflow::WorkflowSpawnInfo>,
        budget: Option<crate::orchestration::workflow::WorkflowBudget>,
    },
    /// Host-owned cancellation of in-flight child agents.
    PreemptSubAgents {
        agent_ids: Vec<String>,
        reason: String,
    },
    PersistMemory {
        memory: crate::mm::memory::MemoryWriteRequest,
    },
    QueryMemory {
        query: crate::mm::memory::MemoryQuery,
        requested_k: usize,
    },
    SpoolLargeResult {
        call_id: String,
        tool: String,
        output: String,
        original_size: u32,
        preview_size: u32,
    },
    ArchivePageOut {
        turn: u32,
        action: crate::runtime::kernel::KernelPressureAction,
        summary: Option<String>,
        archived: Vec<Message>,
        tier: String,
    },
    Done {
        result: LoopResult,
    },
    /// Kernel requests the SDK to evaluate the current milestone phase.
    ///
    /// The SDK should assess `criteria` against the agent's output using the
    /// specified `verifier`, then feed back `LoopEvent::MilestoneResult { result }`.
    EvaluateMilestone {
        phase_id: String,
        criteria: Vec<String>,
        verifier: Option<crate::types::milestone::MilestoneVerifier>,
        required_evidence: Vec<String>,
    },
    /// Kernel is suspended awaiting a non-approval internal continuation.
    AwaitingResume,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(super) struct PendingWorkflowSpawn {
    pub nodes: Vec<crate::orchestration::workflow::WorkflowSpawnInfo>,
    pub budget: Option<crate::orchestration::workflow::WorkflowBudget>,
}

#[derive(Debug, Clone)]
pub(super) struct PendingPreempt {
    pub agent_ids: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(super) enum PendingHostEffect {
    SpoolLargeResult {
        call_id: String,
        tool: String,
        output: String,
        original_size: u32,
        preview_size: u32,
    },
    ArchivePageOut {
        turn: u32,
        action: crate::runtime::kernel::KernelPressureAction,
        summary: Option<String>,
        archived: Vec<Message>,
        tier: String,
    },
}

impl PendingHostEffect {
    fn action(&self) -> LoopAction {
        match self {
            Self::SpoolLargeResult {
                call_id,
                tool,
                output,
                original_size,
                preview_size,
            } => LoopAction::SpoolLargeResult {
                call_id: call_id.clone(),
                tool: tool.clone(),
                output: output.clone(),
                original_size: *original_size,
                preview_size: *preview_size,
            },
            Self::ArchivePageOut {
                turn,
                action,
                summary,
                archived,
                tier,
            } => LoopAction::ArchivePageOut {
                turn: *turn,
                action: *action,
                summary: summary.clone(),
                archived: archived.clone(),
                tier: tier.clone(),
            },
        }
    }
}

/// Payload held while the loop is in `Suspended`.
#[derive(Debug, Clone)]
pub(super) enum SuspendState {
    /// Governance AskUser — awaiting a correlated approval result.
    AskUser {
        calls: Vec<ToolCall>,
        gated_reasons: HashMap<String, String>,
    },
    /// Sub-agent spawn — awaiting `SubAgentCompleted` for each listed agent id.
    SubAgentAwait {
        agent_ids: Vec<String>,
    },
}

pub(super) enum GateToolOutcome {
    Proceed,
    Blocked(LoopAction),
    ApprovalRequired(Vec<ApprovalRequest>),
}

/// Snapshot of context lengths captured just before each LLM call.
/// Used internally to restore state on rollback.
#[derive(Debug, Clone, Default)]
pub struct TurnCheckpoint {
    pub history_len: usize,
    pub signals_len: usize,
    pub task_state: Option<crate::context::task_state::TaskState>,
}

/// Pure state machine for the L* execution loop. No I/O — only state transitions.
///
/// Internal engine backing [`crate::runtime::KernelRuntime`]. Exposed for in-crate
/// use and tests; external callers should drive the kernel through `KernelRuntime`.
#[doc(hidden)]
pub struct LoopStateMachine {
    pub phase: LoopPhase,
    pub turn: u32,
    pub ctx: ContextManager,
    pub tools: Vec<ToolSchema>,
    pub observations: Vec<KernelObservation>,
    pub(super) policy: SchedulerBudget,
    pub(super) total_tokens: u64,
    /// Reservation-backed hard limits for this operation. Shared accounting stays in the host;
    /// the kernel tracks only this run's local usage.
    pub(super) budget_grant: Option<crate::runtime::kernel::BudgetGrant>,
    pub(super) local_rounds_completed: u32,
    /// ③ the adjudicated `pace` decision awaiting attachment to this round's LoopResult.
    pub(super) pending_pace: Option<crate::types::result::PaceDecision>,
    /// When set, the next LLM call strips tools to force a text response,
    /// then terminates with this reason once the response arrives.
    pub(super) pending_termination: Option<TerminationReason>,
    /// Reactive context-overflow recovery: consecutive compact-and-retry attempts since the last
    /// successful provider turn. Bounds the recovery ladder (anti-spiral) and resets to 0 on any
    /// `LLMResponse`, mirroring the per-turn `hasAttemptedReactiveCompact` reset the SDK runners
    /// used to own. See `recover_from_provider_error`.
    pub(super) recovery_attempts: u8,
    pub(crate) provider_recovery_attempt_limit: u8,
    /// Max-output-tokens recovery: consecutive continue-and-retry turns since the model last
    /// finished a response WITHOUT hitting the output cap. When a turn is cut off at the cap
    /// (provider `stop_reason` = max_tokens/length) the kernel keeps the partial, nudges the model
    /// to resume mid-thought, and re-calls — bounded by `MAX_OUTPUT_RECOVERY` (mirrors query.ts's
    /// MAX_OUTPUT_TOKENS_RECOVERY_LIMIT). Resets to 0 on any non-truncated response.
    pub(super) output_recovery_attempts: u8,
    pub(crate) output_recovery_attempt_limit: u8,
    pub(crate) host_effect_retry_attempt_limit: u8,
    /// Transient carrier for the provider `stop_reason` of the in-flight response, set by the
    /// kernel ABI just before `feed(LLMResponse)` and taken (cleared) inside it. `None` when the
    /// SDK/provider doesn't report one (every non-Anthropic provider today ⇒ no-op).
    pub(super) pending_stop_reason: Option<String>,
    /// Number of history messages present at session start (after preload_history).
    /// drain_new_messages() returns the slice from this offset onward.
    pub(super) session_history_baseline: usize,
    pub(super) checkpoint: TurnCheckpoint,
    /// Milestone contract tracker (extracted to reduce state machine bloat).
    pub(super) milestone: MilestoneTracker,
    pub run_spec: Option<AgentRunSpec>,
    /// M1 収口: the single source of truth for schedulability *and* sub-agent lineage. Root is
    /// task `"root"`; each sub-agent is a child task carrying its `ProcInfo`. The former
    /// `ProcessTable` is now a derived view over this (`agent_process(es)` rebuild `AgentProcess`
    /// rows on demand via `AgentProcess::from_tcb`).
    pub(super) tasks: TaskTable,
    /// Optional governance pipeline. When set, every tool call proposed by the
    /// model is evaluated before `ExecuteTools` is emitted. `None` (default)
    /// skips the gate entirely, preserving the pre-governance behavior.
    pub(super) governance: Option<GovernancePipeline>,
    /// Optional resource quota evaluated at the syscall trap (M2). `None` (default) leaves spawn /
    /// memory syscalls unconditionally allowed, preserving pre-M2 behavior.
    pub(super) resource_quota: Option<crate::governance::quota::ResourceQuota>,
    /// Timestamps of recent allowed `WriteMemory` syscalls, for the rolling-window rate limit.
    /// Only populated when `resource_quota.memory_writes_per_window` is set.
    pub(super) memory_write_times: Vec<u64>,
    /// Optional long-term memory policy (`set_memory_policy`). `None` (default) preserves
    /// pre-policy behavior: default-rule validation + verbatim retrieval `top_k`.
    pub(super) memory_policy: Option<crate::mm::memory::MemoryPolicy>,
    /// Kernel-owned signal routing: dedup set + attention policy + bounded queue.
    /// Always initialized; `set_attention` rebuilds it with a new queue size.
    pub(super) signal_router: SignalRouter,
    /// Wall-clock timestamp of the first `ProviderResult.now_ms` received.
    /// Used by the wall-time budget axis in `SchedulerBudget::should_terminate`.
    pub(super) started_at_ms: Option<u64>,
    /// Most-recent `now_ms` value from `ProviderResult`, forwarded to the budget check.
    pub(super) last_now_ms: Option<u64>,
    /// Tool batch awaiting `Resume` after an AskUser suspend.
    pub(super) suspend_state: Option<SuspendState>,
    /// Denied tool results to merge into the next `ToolResults` feed after resume.
    pub(super) pending_denied_results: Vec<ToolResult>,
    /// W0: an in-flight workflow DAG, when one is loaded. The kernel spawns its ready nodes as
    /// gated batches (each through `evaluate_syscall(Syscall::Spawn)`) and advances on
    /// completions. `None` (default) preserves the single-spawn `spawn_sub_agent` behavior.
    pub(super) workflow: Option<crate::orchestration::workflow::WorkflowRun>,
    /// Workflow batch reserved by the kernel and awaiting the host's correlated
    /// spawn result. This is intent, not an observed external fact.
    pub(super) pending_workflow_spawn: Option<PendingWorkflowSpawn>,
    pub(super) pending_preempt: Option<PendingPreempt>,
    /// Ordered host-owned durability effects produced during a pure state-machine
    /// transition. The normal continuation is held until every effect commits.
    pub(super) pending_host_effects: VecDeque<PendingHostEffect>,
    pub(super) active_host_effect: Option<PendingHostEffect>,
    pub(super) active_host_effect_failures: u8,
    pub(super) deferred_action: Option<Box<LoopAction>>,
    /// O6: repeat-fuse thresholds (the hard rungs above the 2c soft STOP). Default enabled with
    /// generous thresholds; tune/disable via `SetRepeatFuse` / `ConfigureRun.repeat_fuse`.
    pub(super) repeat_fuse: RepeatFuseConfig,
    /// O6: the previous turn's action signature (non-meta `name(args)` joined — the same key the
    /// 2c STOP uses). NOT part of the turn checkpoint: a fuse deny's rollback must not launder
    /// the streak it just tripped on.
    pub(super) repeat_sig: Option<String>,
    /// O6: consecutive turns whose signature equalled `repeat_sig` (1 = first occurrence).
    pub(super) repeat_count: u32,
    /// O4: turn-end criteria gate (the Stop-hook analog). When the model finishes (no tool calls)
    /// while explicit acceptance criteria stand, inject ONE bounded self-check turn before
    /// accepting `Completed`. 2c guards "won't stop"; this guards "stops too early".
    pub(super) criteria_gate_enabled: bool,
    /// O4: whether the gate already fired this run (it fires at most once — no nag loops).
    pub(super) criteria_gate_fired: bool,
    /// Session-entropy sliding window + watch state (see `scheduler::entropy`). Like the
    /// RepeatFuse streak, NOT part of the turn checkpoint — a rollback must not launder
    /// the disorder it just evidenced.
    pub(super) entropy: EntropyTracker,
    /// Opt-in threshold watch over the per-turn entropy score. Default disabled; the
    /// unconditional per-turn `EntropySample` observation does not depend on it.
    pub(super) entropy_watch: EntropyWatchConfig,
}

mod signal;
mod capability;
mod gate;
mod eviction;
mod process;
mod workflow;
mod milestone_exec;

impl LoopStateMachine {
    fn message_tokens(&self, message: &Message) -> u32 {
        message
            .token_count
            .unwrap_or_else(|| self.ctx.engine.count_message(message))
    }

    pub fn new(policy: SchedulerBudget) -> Self {
        let mut tasks = TaskTable::new();
        // M1d: the root task carries the authoritative schedulability lifecycle. It starts
        // `Ready`; `start()`/`resume_*` flip it to `Running`, suspends set `Suspended`, and
        // `terminate()` sets `Done`. `phase` is now only the intra-turn step.
        tasks.insert(Tcb::root("root", policy.clone()));
        Self {
            // Inert placeholder step; meaningful only while the root task is `Running`.
            phase: LoopPhase::Reason,
            turn: 0,
            ctx: ContextManager::new(policy.max_tokens),
            tools: Vec::new(),
            observations: Vec::new(),
            policy,
            total_tokens: 0,
            budget_grant: None,
            local_rounds_completed: 0,
            pending_pace: None,
            pending_termination: None,
            recovery_attempts: 0,
            provider_recovery_attempt_limit: 2,
            output_recovery_attempts: 0,
            output_recovery_attempt_limit: 3,
            host_effect_retry_attempt_limit: 3,
            pending_stop_reason: None,
            session_history_baseline: 0,
            checkpoint: TurnCheckpoint::default(),
            milestone: MilestoneTracker::new(),
            run_spec: None,
            tasks,
            governance: None,
            resource_quota: None,
            memory_write_times: Vec::new(),
            memory_policy: None,
            signal_router: SignalRouter::new(64),
            started_at_ms: None,
            last_now_ms: None,
            suspend_state: None,
            pending_denied_results: Vec::new(),
            workflow: None,
            pending_workflow_spawn: None,
            pending_preempt: None,
            pending_host_effects: VecDeque::new(),
            active_host_effect: None,
            active_host_effect_failures: 0,
            deferred_action: None,
            repeat_fuse: RepeatFuseConfig::default(),
            repeat_sig: None,
            repeat_count: 0,
            criteria_gate_enabled: true,
            criteria_gate_fired: false,
            entropy: EntropyTracker::default(),
            entropy_watch: EntropyWatchConfig::default(),
        }
    }

    /// O4: enable/disable the turn-end criteria gate (default enabled; no-op without criteria).
    pub fn set_criteria_gate(&mut self, enabled: bool) {
        self.criteria_gate_enabled = enabled;
    }

    pub(crate) fn set_reliability_config(
        &mut self,
        config: &crate::runtime::kernel::KernelReliabilityConfig,
    ) {
        if let Some(limit) = config.provider_recovery_attempts {
            self.provider_recovery_attempt_limit = limit;
        }
        if let Some(limit) = config.output_recovery_attempts {
            self.output_recovery_attempt_limit = limit;
        }
        if let Some(limit) = config.host_effect_retry_attempts {
            self.host_effect_retry_attempt_limit = limit;
        }
        if let Some(bytes) = config.spool_threshold_bytes {
            self.ctx.config.spool_threshold_bytes = bytes;
        }
        if let Some(bytes) = config.spool_preview_bytes {
            self.ctx.config.spool_preview_bytes = bytes;
        }
    }

    pub(crate) fn externalize_pending_host_effect(
        &mut self,
        continuation: LoopAction,
    ) -> LoopAction {
        if self.active_host_effect.is_some() {
            return continuation;
        }
        let Some(pending) = self.pending_host_effects.pop_front() else {
            return continuation;
        };
        assert!(
            self.deferred_action.is_none(),
            "host effect continuation must be unique"
        );
        self.deferred_action = Some(Box::new(continuation));
        self.active_host_effect = Some(pending);
        self.active_host_effect_failures = 0;
        self.active_host_effect
            .as_ref()
            .expect("host effect was just activated")
            .action()
    }

    fn next_after_host_effect(&mut self) -> LoopAction {
        if let Some(pending) = self.pending_host_effects.pop_front() {
            self.active_host_effect = Some(pending);
            self.active_host_effect_failures = 0;
            self.active_host_effect
                .as_ref()
                .expect("host effect was just activated")
                .action()
        } else {
            match self.deferred_action.take().map(|action| *action) {
                // Durability effects can change rendered context and conditional meta-tools
                // (notably `read_result`). Never return the pre-commit frozen provider action.
                Some(LoopAction::CallLLM { .. }) => self.emit_call_llm(),
                Some(action) => action,
                None => LoopAction::AwaitingResume,
            }
        }
    }

    pub(crate) fn resolve_large_result_spool(
        &mut self,
        spool_ref: Option<String>,
        error: Option<String>,
    ) -> LoopAction {
        let pending = self
            .active_host_effect
            .as_ref()
            .expect("spool result requires an active host effect");
        let PendingHostEffect::SpoolLargeResult {
            call_id,
            tool,
            original_size,
            preview_size,
            ..
        } = pending
        else {
            panic!("spool result does not match active page-out effect");
        };
        if let Some(error) = error {
            self.observations
                .push(KernelObservation::LargeResultSpoolFailed {
                    turn: self.turn,
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    error,
                });
            self.active_host_effect_failures = self.active_host_effect_failures.saturating_add(1);
            if self.active_host_effect_failures > self.host_effect_retry_attempt_limit {
                self.active_host_effect = None;
                self.pending_host_effects.clear();
                self.deferred_action = None;
                return self.terminate(TerminationReason::Error, None);
            }
            return pending.action();
        }
        let spool_ref = spool_ref.expect("successful spool result requires spool_ref");
        let call_id = call_id.clone();
        let tool = tool.clone();
        let original_size = *original_size;
        let preview_size = *preview_size;
        self.ctx.mark_spooled(&call_id, spool_ref.clone());
        self.observations.push(KernelObservation::LargeResultSpooled {
            turn: self.turn,
            call_id,
            tool,
            original_size,
            preview_size,
            spool_ref: Some(spool_ref),
        });
        self.active_host_effect = None;
        self.active_host_effect_failures = 0;
        self.next_after_host_effect()
    }

    pub(crate) fn resolve_page_out_archive(
        &mut self,
        archive_ref: Option<String>,
        error: Option<String>,
    ) -> LoopAction {
        let pending = self
            .active_host_effect
            .as_ref()
            .expect("page-out result requires an active host effect");
        let PendingHostEffect::ArchivePageOut {
            turn,
            action,
            summary,
            archived,
            tier,
        } = pending
        else {
            panic!("page-out result does not match active spool effect");
        };
        if let Some(error) = error {
            self.observations
                .push(KernelObservation::PageOutArchiveFailed {
                    turn: *turn,
                    action: *action,
                    tier: tier.clone(),
                    message_count: archived.len() as u32,
                    error,
                });
            self.active_host_effect_failures = self.active_host_effect_failures.saturating_add(1);
            if self.active_host_effect_failures > self.host_effect_retry_attempt_limit {
                self.active_host_effect = None;
                self.pending_host_effects.clear();
                self.deferred_action = None;
                return self.terminate(TerminationReason::Error, None);
            }
            return pending.action();
        }
        self.observations.push(KernelObservation::PageOutArchived {
            turn: *turn,
            action: *action,
            summary: summary.clone(),
            tier: tier.clone(),
            message_count: archived.len() as u32,
            archive_ref,
        });
        self.active_host_effect = None;
        self.active_host_effect_failures = 0;
        self.next_after_host_effect()
    }

    /// O6: tune or disable the repeat fuse (see [`RepeatFuseConfig`]).
    pub fn set_repeat_fuse(&mut self, config: RepeatFuseConfig) {
        self.repeat_fuse = config;
    }

    /// Configure the opt-in entropy threshold watch (see [`EntropyWatchConfig`]).
    /// The per-turn `EntropySample` observation is unconditional and unaffected.
    pub fn set_entropy_watch(&mut self, config: EntropyWatchConfig) {
        self.entropy_watch = config;
    }

    pub fn entropy_watch_config(&self) -> EntropyWatchConfig {
        self.entropy_watch
    }

    /// O6: the active repeat-fuse config (for read-modify-write from the ABI event).
    pub fn repeat_fuse_config(&self) -> RepeatFuseConfig {
        self.repeat_fuse
    }

    /// The authoritative schedulability lifecycle of the loop (root task state). Replaces the
    /// removed `LoopPhase::{Idle,Suspended,Blocked,Terminal}` reads.
    pub fn lifecycle(&self) -> TaskLifecycle {
        self.tasks.get("root").map(|t| t.state).unwrap_or(TaskLifecycle::Ready)
    }

    /// The wait reason while suspended/blocked, if any.
    pub fn wait_reason(&self) -> Option<WaitReason> {
        self.tasks.get("root").and_then(|t| t.wait.clone())
    }

    /// Whether the loop has terminated.
    pub fn is_terminal(&self) -> bool {
        matches!(self.lifecycle(), TaskLifecycle::Done(_))
    }

    /// Whether the loop is suspended awaiting external resolution.
    pub fn is_suspended(&self) -> bool {
        matches!(self.lifecycle(), TaskLifecycle::Suspended)
    }

    /// Set the root task's lifecycle (and wait reason). Single mutation point for schedulability.
    fn set_lifecycle(&mut self, state: TaskLifecycle, wait: Option<WaitReason>) {
        if let Some(root) = self.tasks.get_mut("root") {
            root.state = state;
            root.wait = wait;
        } else {
            let mut root = Tcb::root("root", self.policy.clone());
            root.state = state;
            root.wait = wait;
            self.tasks.insert(root);
        }
    }

    /// Build a transient root [`Tcb`] mirroring the current scheduling facts (budget counters,
    /// wall-clock anchors, lifecycle). M1b uses this to run the pure `schedule()` spine in
    /// parallel with the legacy budget path; later milestones promote it to the live task row.
    fn root_tcb(&self) -> Tcb {
        let mut tcb = Tcb::root("root", self.policy.clone());
        tcb.budget.turns = self.turn;
        tcb.budget.total_tokens = self.total_tokens;
        if let Some(tokens) = self.budget_grant.as_ref().and_then(|grant| grant.tokens) {
            tcb.budget.limits.max_total_tokens = tcb.budget.limits.max_total_tokens.min(tokens);
        }
        tcb.budget.started_at_ms = self.started_at_ms;
        tcb.state = self.lifecycle();
        tcb
    }

    /// Adjust the wall-clock budget axis at runtime.
    pub fn set_wall_budget(&mut self, max_wall_ms: Option<u64>) {
        self.policy.max_wall_ms = max_wall_ms;
    }

    /// Install a governance pipeline. Once set, all model-proposed tool calls
    /// are evaluated before execution. Denied/rate-limited calls roll the turn
    /// back (reusing the `GovernanceDenied` path); `AskUser` calls surface a
    /// `ToolGated` observation for the SDK to enforce.
    pub fn set_governance(&mut self, pipeline: GovernancePipeline) {
        self.governance = Some(pipeline);
    }

    /// Install resource quotas (M2). Once set, `Spawn` and `WriteMemory` syscalls are bounded by
    /// the quota at the trap. Not setting it (the default) leaves them unconditionally allowed.
    pub fn set_resource_quota(&mut self, quota: crate::governance::quota::ResourceQuota) {
        self.resource_quota = Some(quota);
    }

    pub fn set_budget_grant(&mut self, grant: crate::runtime::kernel::BudgetGrant) {
        self.budget_grant = Some(grant);
    }

    /// L1: this vehicle's cumulative sub-agent spawns this run — every child task ever registered in
    /// the `TaskTable` (running + completed), distinct from the *instantaneous* running count. Used
    /// for the cumulative spawn quota and read back by the SDK to charge the group ledger at run end.
    pub fn local_subagents_spawned(&self) -> u32 {
        self.tasks.all().iter().filter(|t| t.proc.is_some()).count() as u32
    }

    pub fn local_budget_usage(&self) -> (u64, u32, u32) {
        (
            self.total_tokens,
            self.local_subagents_spawned(),
            self.local_rounds_completed,
        )
    }

    pub fn budget_grant(&self) -> Option<&crate::runtime::kernel::BudgetGrant> {
        self.budget_grant.as_ref()
    }

    /// Install the long-term memory policy (`set_memory_policy`). Once set it gates `write_memory`
    /// validation and bounds `query_memory` retrieval breadth. Not setting it (the default)
    /// preserves pre-policy behavior.
    pub fn set_memory_policy(&mut self, policy: crate::mm::memory::MemoryPolicy) {
        self.memory_policy = Some(policy);
    }

    /// The installed memory policy, if any. `None` means default-rule validation + verbatim top_k.
    pub fn memory_policy(&self) -> Option<&crate::mm::memory::MemoryPolicy> {
        self.memory_policy.as_ref()
    }

    /// Feed the current wall-clock time (ms) to scheduler/governance budget axes.
    pub fn set_observed_time(&mut self, now_ms: u64) {
        if self.started_at_ms.is_none() {
            self.started_at_ms = Some(now_ms);
        }
        self.last_now_ms = Some(now_ms);
        if let Some(pipeline) = self.governance.as_mut() {
            pipeline.set_time(now_ms);
        }
    }

    /// Stash the in-flight response's provider `stop_reason` so `feed(LLMResponse)` can detect an
    /// output-cap truncation. Set by the kernel ABI right before feeding the result; `None` clears it.
    pub fn set_pending_stop_reason(&mut self, stop_reason: Option<String>) {
        self.pending_stop_reason = stop_reason;
    }

    /// Pre-populate the history partition with messages from a prior session.
    ///
    /// Call **before** `start()` when resuming a conversation. Sets the baseline
    /// so `drain_new_messages()` returns only the messages from the current run.
    pub fn preload_history(&mut self, messages: Vec<Message>) {
        for msg in messages {
            let tokens = self.message_tokens(&msg);
            self.ctx.push_history(msg, tokens);
        }
        self.session_history_baseline = self.ctx.partitions.history.messages.len();
    }

    /// Continue from preloaded history without appending a new user turn.
    /// Use after `preload_history` when recovering a session that ended mid-run.
    ///
    /// If the last assistant turn has tool calls without matching tool results,
    /// resumes with `ExecuteTools` instead of calling the LLM again.
    pub fn resume_after_preload(&mut self) -> LoopAction {
        self.observations.clear();
        let calls = crate::runtime::repair::pending_tool_calls_from_messages(
            &self.ctx.partitions.history.messages,
        );
        if !calls.is_empty() {
            self.phase = LoopPhase::Act {
                tool_calls: calls.clone(),
            };
            self.set_lifecycle(TaskLifecycle::Running, None);
            return LoopAction::ExecuteTools { calls };
        }
        self.phase = LoopPhase::Reason;
        self.emit_call_llm()
    }

    /// Return all messages added to history during the current run
    /// (since the last `preload_history` call or since construction).
    ///
    /// Call after `LoopAction::Done` to get the complete turn transcript
    /// for persistence to a SessionStore.
    pub fn drain_new_messages(&self) -> Vec<Message> {
        let history = &self.ctx.partitions.history.messages;
        let start = self.session_history_baseline.min(history.len());
        history[start..].to_vec()
    }

    pub fn start(&mut self, task: RuntimeTask) -> LoopAction {
        self.observations.clear();
        self.ctx.init_task(task.goal.clone(), task.criteria.clone());

        // A loop vehicle with no admitted round capacity must not make even one provider call.
        // The host may have raced another member between reading its durable loop log and reserve;
        // the reservation is the authoritative admission decision.
        if self
            .run_spec
            .as_ref()
            .and_then(|spec| spec.loop_round.as_ref())
            .is_some()
            && self.budget_grant.as_ref().and_then(|grant| grant.rounds) == Some(0)
        {
            self.observations.push(KernelObservation::BudgetExceeded {
                turn: self.turn,
                budget: "rounds".into(),
                operation_id: String::new(),
                reservation_id: self
                    .budget_grant
                    .as_ref()
                    .map(|grant| grant.reservation_id.clone()),
            });
            self.pending_pace = Some(crate::types::result::PaceDecision {
                action: crate::types::result::PaceAction::Stop,
                delay_ms: None,
                reason: "round budget grant exhausted before start".into(),
                coerced_from: None,
            });
            return self.terminate(TerminationReason::Completed, None);
        }

        let user_msg = "Proceed with the task described in [TASK STATE].".to_string();

        // User message goes into history so it appears at the correct chronological
        // position: [prior turns...] → [current user message] — LLM reads left-to-right
        // and responds to the last message. working is reserved for runtime signals only.
        // Estimate tokens (1 token ≈ 4 chars) with a minimum of 1 so the renderer
        // does not skip this message (it skips zero-token entries).
        let user_tokens = self.ctx.engine.count(&user_msg).max(1);
        self.ctx.push_history(Message::user(user_msg), user_tokens);
        self.phase = LoopPhase::Reason;
        // Root task (seeded `Ready` in `new()`) becomes `Running`; `emit_call_llm` sets it.
        self.emit_call_llm()
    }

    pub fn feed(&mut self, event: LoopEvent) -> LoopAction {
        self.observations.clear();
        self.sweep_expired_leases();
        // K3: skill leases expire on the same head-of-event cadence as capability leases.
        self.ctx.sweep_expired_skill_leases(self.turn);

        match event {

            LoopEvent::LLMResponse { message } => {
                // A response arrived ⇒ the prompt fit ⇒ the overflow recovery ladder is reset.
                self.recovery_attempts = 0;
                let tokens = self.message_tokens(&message);
                self.total_tokens += tokens as u64;

                // Max-output-tokens recovery (mirrors query.ts): a response cut off at the output
                // cap reports stop_reason = max_tokens (Anthropic) / length (OpenAI). A clean finish
                // resets the ladder.
                const OUTPUT_TRUNCATION_NUDGE: &str = "Output token limit hit. Resume directly — no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.";
                let truncated = matches!(
                    self.pending_stop_reason.take().as_deref(),
                    Some("max_tokens") | Some("length"),
                );
                if !truncated {
                    self.output_recovery_attempts = 0;
                }

                if let Some(reason) = self.pending_termination.take() {
                    return self.terminate(reason, Some(message));
                }

                if message.tool_calls.is_empty() {
                    // The model was cut off at the output cap with no tool call. Keep the partial,
                    // nudge it to resume mid-thought, and re-call — instead of mistaking the
                    // truncation for a finished turn. Bounded by MAX_OUTPUT_RECOVERY; once exhausted
                    // the partial stands and the turn terminates normally below. (A truncated
                    // *tool-call* turn isn't handled here — it falls through to tool execution.)
                    if truncated
                        && self.output_recovery_attempts < self.output_recovery_attempt_limit
                    {
                        self.output_recovery_attempts += 1;
                        self.ctx.push_history(message, tokens);
                        self.ctx.push_signal(OUTPUT_TRUNCATION_NUDGE.to_string());
                        self.phase = LoopPhase::Reason;
                        return self.emit_call_llm();
                    }
                    // When a milestone contract is active and not yet complete,
                    // request evaluation instead of terminating.
                    if !self.milestone.is_complete() {
                        let phase_id = self.milestone.current_phase_id().unwrap_or("").to_string();
                        let criteria = self.milestone.current_criteria().to_vec();
                        let (verifier, required_evidence) = self
                            .milestone
                            .current_phase()
                            .map(|p| (p.verifier.clone(), p.required_evidence.clone()))
                            .unwrap_or_default();
                        // `tokens` was already computed for this message above.
                        self.ctx.push_history(message, tokens);
                        return LoopAction::EvaluateMilestone {
                            phase_id,
                            criteria,
                            verifier,
                            required_evidence,
                        };
                    }
                    // O4 criteria gate (the Stop-hook analog): the model is finishing while explicit
                    // acceptance criteria stand. Before accepting `Completed`, inject ONE bounded
                    // self-check at the peak-attention slot — verify each criterion, continue if any
                    // is unmet, else confirm. Fires at most once per run (no nag loop); runs with no
                    // criteria are untouched. 2c guards "won't stop"; this guards "stops too early".
                    if self.criteria_gate_enabled
                        && !self.criteria_gate_fired
                        && !self.ctx.partitions.task_state.criteria.is_empty()
                    {
                        self.criteria_gate_fired = true;
                        let criteria = self.ctx.partitions.task_state.criteria.clone();
                        self.ctx.push_history(message, tokens);
                        self.ctx.push_signal(format!(
                            "[CRITERIA CHECK] You are about to finish. Verify each acceptance \
                             criterion first: {}. If any is NOT met, continue working on it now. \
                             If all are met, give the final answer.",
                            criteria.join(" | ")
                        ));
                        self.observations.push(KernelObservation::CriteriaGateFired {
                            turn: self.turn,
                            criteria,
                        });
                        self.phase = LoopPhase::Reason;
                        return self.emit_call_llm();
                    }
                    return self.terminate(TerminationReason::Completed, Some(message));
                }

                let calls = message.tool_calls.clone();
                self.ctx.push_history(message, tokens);

                // ━━ 记录活动时间（Layer 3时间衰减使用）
                if let Some(now_ms) = self.last_now_ms {
                    self.ctx.record_activity(now_ms);
                }

                // ③ pacing trap: a `pace` call is a kernel-adjudicated round-end proposal,
                // never an SDK tool. Handled before the fuse/gate — it is a control verb,
                // not task work.
                if self.run_spec.as_ref().and_then(|r| r.loop_round.as_ref()).is_some() {
                    if let Some(pace_call) = calls.iter().find(|c| c.name.as_str() == "pace") {
                        let call = pace_call.clone();
                        return self.handle_pace_call(call);
                    }
                }

                // 2b: record this turn's tool activity into the task-state recency log (meta-tools
                // filtered inside). The State-turn footer renders it as "just did: …" + a forward
                // nudge / STOP, so progress is kernel-derived and never depends on the model
                // remembering to call `update_plan`. Tool *names* live only on the request (results
                // carry call_id only), so this is the turn to capture them.
                //
                // Capture name AND a compact arg digest: the no-progress STOP keys on whether the
                // SAME call repeats, and a legit loop (same tool, DIFFERENT args — e.g. processing 20
                // items) is real progress, not a stall. Keying on the name alone false-positives those
                // loops; including args distinguishes "step(n=1), step(n=2)…" from a true repeat.
                let action_sigs: Vec<(String, String)> = calls
                    .iter()
                    .map(|c| (c.name.to_string(), compact_tool_args(&c.arguments)))
                    .collect();
                self.ctx.note_tool_actions(&action_sigs);

                // O6 RepeatFuse: the hard rungs above the 2c soft STOP. Runs BEFORE the governance
                // gate and independent of whether a policy is loaded — a batteries-included kernel
                // protection, not a policy feature. Deny rolls the turn back with a directive note;
                // the terminate rung ends the run `NoProgress` after one final no-tools report turn.
                if let Some(action) = self.check_repeat_fuse(&calls) {
                    return action;
                }

                match self.gate_tool_calls(&calls) {
                    GateToolOutcome::Blocked(action) => return action,
                    GateToolOutcome::ApprovalRequired(requests) => {
                        return LoopAction::RequestApproval { requests };
                    }
                    GateToolOutcome::Proceed => {}
                }
                self.phase = LoopPhase::Act {
                    tool_calls: calls.clone(),
                };
                self.set_lifecycle(TaskLifecycle::Running, None);
                LoopAction::ExecuteTools { calls }
            }

            LoopEvent::ToolResults { mut results } => {
                if !self.pending_denied_results.is_empty() {
                    results.append(&mut self.pending_denied_results);
                }
                if let Some(reason) = results
                    .iter()
                    .find_map(|result| self.rollback_reason_for_tool_result(result))
                {
                    let note = Message::user(super::rollback::build_rollback_note(
                        &reason,
                        self.ctx.config.verbose_control_notes,
                    ));
                    self.rollback(reason);
                    self.ctx.push_signal(note.content.as_text().unwrap_or_default().to_string());
                    self.phase = LoopPhase::Reason;
                    return self.emit_call_llm();
                }
                // Non-fatal errors are committed to history so the LLM can
                // see them and self-correct without losing turn state.

                // Entropy: this completed turn's failure tally (fatal errors never get
                // here — they rolled back above and accrued via `note_rollback`).
                let errored_results = results.iter().filter(|r| r.is_error).count() as u32;
                let total_results = results.len() as u32;
                let tool_by_call_id: HashMap<String, String> = match &self.phase {
                    LoopPhase::Act { tool_calls } => tool_calls
                        .iter()
                        .map(|call| (call.id.to_string(), call.name.to_string()))
                        .collect(),
                    LoopPhase::Reason => HashMap::new(),
                };

                for r in &results {
                    self.total_tokens += r.token_count.unwrap_or(0) as u64;
                    // Preserve Content::Parts (structured / multimodal tool output).
                    // Parts are serialised to JSON so the text can be restored faithfully.
                    let raw_output = match &r.output {
                        Content::Text(s) => s.clone(),
                        Content::Parts(parts) => serde_json::to_string(parts).unwrap_or_default(),
                    };
                    // Layer 1 spool: oversized results keep only a preview in context. The full
                    // output becomes a host effect and no success fact is recorded until its
                    // correlated result commits.
                    let (output, spooled) = match crate::mm::plan_spool(
                        &raw_output,
                        self.ctx.config.spool_threshold_bytes,
                        self.ctx.config.spool_preview_bytes,
                    ) {
                        Some(decision) => {
                            self.pending_host_effects.push_back(
                                PendingHostEffect::SpoolLargeResult {
                                    call_id: r.call_id.to_string(),
                                    tool: tool_by_call_id
                                        .get(r.call_id.as_str())
                                        .cloned()
                                        .unwrap_or_default(),
                                    output: raw_output.clone(),
                                    original_size: decision.original_size,
                                    preview_size: decision.preview.len() as u32,
                                },
                            );
                            (decision.preview, true)
                        }
                        None => (raw_output, false),
                    };
                    let parts = vec![ContentPart::ToolResult {
                        call_id: r.call_id.clone(),
                        output,
                        is_error: r.is_error,
                    }];
                    let tool_msg = Message::tool(parts);
                    // When spooled, `r.token_count` reflects the full output — recount the preview.
                    let tokens = if spooled {
                        self.ctx.engine.count_message(&tool_msg)
                    } else {
                        r.token_count
                            .unwrap_or_else(|| self.ctx.engine.count_message(&tool_msg))
                    };
                    self.ctx.push_history(tool_msg, tokens);
                }
                self.turn += 1;

                // M1 收口: the pure `schedule()` is now the single budget decision point.
                // It evaluates the same three axes (turn/token/wall) via `BudgetLedger`, which
                // delegates to `SchedulerBudget::should_terminate` internally — one source of truth.
                if let Some(term) = super::tcb::budget_verdict(&self.root_tcb(), self.last_now_ms) {
                    let budget = match term {
                        TerminationReason::MaxTurns => "max_turns",
                        TerminationReason::Timeout => "wall_time",
                        _ => "token_budget",
                    };
                    self.observations.push(KernelObservation::BudgetExceeded {
                        turn: self.turn,
                        budget: budget.to_string(),
                        operation_id: String::new(),
                        reservation_id: self
                            .budget_grant
                            .as_ref()
                            .map(|grant| grant.reservation_id.clone()),
                    });
                    self.pending_termination = Some(term);
                    self.phase = LoopPhase::Reason;
                    return self.emit_call_llm();
                }

                // ━━ Eviction checkpoint (M3): one decision model (`plan_eviction`), one
                // execution funnel (`execute_eviction_op`). Layer 3 (idle/time-decay) must run
                // before the rho recommendation is read, since it mutates token usage — so the
                // plan is built in that interleaved order and the ops are executed in plan order.
                let idle_decay = self
                    .last_now_ms
                    .is_some_and(|now_ms| self.ctx.should_time_decay_compact(now_ms));
                if idle_decay {
                    self.execute_eviction_op(&crate::mm::EvictionOp::TimeDecayMicro);
                }

                // Layer 4 read-time projection: recompute handle residency on the post-time-decay rho.
                self.ctx.recompute_handle_residency();
                // K2: knowledge budget check — marks over-budget unpinned entries for the next
                // boundary sweep (marks are idempotent; drops only apply there) and stashes a
                // warn-once-per-generation notice, drained into an observation here.
                if let Some((used, budget)) = self.ctx.enforce_knowledge_budget() {
                    self.observations.push(KernelObservation::KnowledgeBudgetExceeded {
                        turn: self.turn,
                        used,
                        budget,
                    });
                }
                // Layers 2/4/5: execute the pressure-driven ops from the plan (skip TimeDecayMicro
                // if already executed). The plan carries specific ops stamped with real config-derived
                // params (W1-1 収口 — no magic-number placeholders), not the umbrella `Pressure` wrapper.
                let (target_tokens, preserve_turns) = self.ctx.plan_compaction_params();
                let plan =
                    crate::mm::plan_eviction(self.ctx.should_compress(), idle_decay, target_tokens, preserve_turns);
                // `idle_decay` ⇒ the plan carries a `TimeDecayMicro` (so the skip-on-already-executed
                // below is meaningful). The converse does NOT hold: a pressure-driven `MicroCompact`
                // also emits `TimeDecayMicro` independent of `idle_decay` (W1 unified planner), so we
                // assert the implication, not equality.
                debug_assert!(!idle_decay || plan.has_time_decay());
                for op in &plan.ops {
                    // Skip TimeDecayMicro if we already executed it (prevents double-execution).
                    if matches!(op, crate::mm::EvictionOp::TimeDecayMicro) && idle_decay {
                        continue;
                    }
                    self.execute_eviction_op(op);
                }

                // Renewal: when compression alone cannot recover enough headroom,
                // start a new sprint — carry forward system + memory + last N history turns.
                if self.ctx.should_renew() {
                    self.ctx.renew();
                    // A new sprint is a session boundary for signal identity: clear the dedup set so
                    // it cannot grow unbounded across a long run, and so a signal seen in a prior
                    // sprint may legitimately re-fire in the new one.
                    self.signal_router.clear_dedup();
                    self.observations.push(KernelObservation::Renewed {
                        sprint: self.ctx.sprint,
                    });
                    // K1: renewal is a boundary — surface the knowledge sweep it just ran.
                    self.emit_knowledge_sweep_observations();
                }

                // Session-entropy sample (the heartbeat watch source): fold this completed
                // turn's outcomes into the sliding window and surface the measurement.
                // Unconditional, like `CheckpointTaken`; only the watch alert below is opt-in.
                let repeat_streak = if self.repeat_fuse.enabled { self.repeat_count } else { 0 };
                let sample = self.entropy.sample(
                    self.turn,
                    self.ctx.rho(),
                    repeat_streak,
                    self.repeat_fuse.deny_after,
                    errored_results,
                    total_results,
                );
                self.observations.push(KernelObservation::EntropySample {
                    turn: sample.turn,
                    score: sample.score,
                    score_version: super::entropy::ENTROPY_SCORE_VERSION,
                    rho: sample.rho,
                    repeat_pressure: sample.repeat_pressure,
                    failure_rate: sample.failure_rate,
                    rollbacks_in_window: sample.rollbacks_in_window,
                    window_turns: sample.window_turns,
                });
                // Opt-in entropy watch: threshold + hysteresis + cooldown. The alert is an
                // observation (host-facing); with `notify_model` it is ALSO routed through
                // the kernel's own signal dispatch as a Heartbeat/Alert directive — High
                // urgency while running ⇒ a durable [SIGNAL] note on the turn we are about
                // to emit anyway, never an extra provider call.
                if self.entropy.should_alert(&self.entropy_watch, &sample) {
                    self.observations.push(KernelObservation::EntropyAlert {
                        turn: sample.turn,
                        score: sample.score,
                        threshold: self.entropy_watch.threshold,
                    });
                    if self.entropy_watch.notify_model {
                        use crate::types::signal::{RuntimeSignal, SignalSource, SignalType, Urgency};
                        let signal = RuntimeSignal::new(
                            SignalSource::Heartbeat,
                            SignalType::Alert,
                            Urgency::High,
                            format!(
                                "[entropy] session disorder {:.2} ≥ {:.2} (repeat {:.2} / failures {:.2} / pressure {:.2}). \
                                 Stop and reassess: state what is not working and try a different approach.",
                                sample.score,
                                self.entropy_watch.threshold,
                                sample.repeat_pressure,
                                sample.failure_rate,
                                sample.rho,
                            ),
                        )
                        .with_dedupe(format!("entropy_alert:{}", sample.turn));
                        let _ = self.dispatch_signal(signal);
                    }
                }

                // Turn boundary: drain any kernel-queued signals into context so they
                // are seen on the next reasoning turn (ready queue → running).
                self.drain_queued_signals();

                self.phase = LoopPhase::Reason;
                self.emit_call_llm()
            }

            LoopEvent::MilestoneResult { result } => self.handle_milestone_result(result),

            LoopEvent::SubAgentCompleted { result } => self.handle_sub_agent_completed(result),

            LoopEvent::Complete => self.terminate(TerminationReason::Completed, None),

            LoopEvent::Timeout => {
                let reason = RollbackReason::Timeout;
                let note = Message::user(super::rollback::build_rollback_note(
                    &reason,
                    self.ctx.config.verbose_control_notes,
                ));
                self.rollback(reason);
                self.ctx.push_signal(note.content.as_text().unwrap_or_default().to_string());
                self.phase = LoopPhase::Reason;
                self.emit_call_llm()
            }
        }
    }


    /// Drain observations emitted during the last `start`/`feed` call.
    pub fn take_observations(&mut self) -> Vec<KernelObservation> {
        std::mem::take(&mut self.observations)
    }

    /// ③ the pacing trap. The model PROPOSES `pace(next, delay_ms?, reason)`; the kernel
    /// ADJUDICATES: malformed → governance-style rollback note; sleep delay clamped into
    /// the spec's [min,max]; continue/sleep at the round cap coerced to stop("max_rounds");
    /// stop with standing acceptance criteria routes through the O4 criteria gate ONCE
    /// (one bounded self-check turn) before being honored. An allowed pace ends the round:
    /// the decision is stashed for LoopResult, a synthetic tool result closes the
    /// transcript pair, and the strip-tools final-report turn finishes the round.
    fn handle_pace_call(&mut self, call: ToolCall) -> LoopAction {
        use crate::types::result::{PaceAction, PaceDecision};

        let spec = self
            .run_spec
            .as_ref()
            .and_then(|r| r.loop_round.as_ref())
            .cloned()
            .unwrap_or_default();

        let next = call.arguments.get("next").and_then(|v| v.as_str()).unwrap_or("");
        let reason = call
            .arguments
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let proposed_delay = call.arguments.get("delay_ms").and_then(|v| v.as_u64());

        let mut action = match next {
            "continue" => PaceAction::Continue,
            "sleep" => PaceAction::Sleep,
            "stop" => PaceAction::Stop,
            other => {
                // Malformed proposal: governance-style directive note + fresh reason turn.
                let rb = RollbackReason::GovernanceDenied {
                    tool_name: "pace".to_string(),
                    reason: format!(
                        "invalid pace next={other:?} (expected continue|sleep|stop)"
                    ),
                };
                let note = Message::user(super::rollback::build_rollback_note(
                    &rb,
                    self.ctx.config.verbose_control_notes,
                ));
                self.push_synthetic_tool_result(
                    &call.id,
                    "pace rejected: next must be continue|sleep|stop",
                );
                self.ctx
                    .push_signal(note.content.as_text().unwrap_or_default().to_string());
                self.phase = LoopPhase::Reason;
                return self.emit_call_llm();
            }
        };
        let mut coerced_from: Option<String> = None;

        // Round-cap coercion: both the run spec and reservation grant bound local rounds.
        if action != PaceAction::Stop {
            let granted_rounds = self.budget_grant.as_ref().and_then(|grant| grant.rounds);
            let max_rounds = if granted_rounds == Some(0) {
                Some(0)
            } else {
                spec.max_rounds
            };
            if let Some(max) = max_rounds {
                if self.local_rounds_completed.saturating_add(1) >= max {
                    coerced_from = Some(format!("{} (max_rounds={max})", action.label()));
                    action = PaceAction::Stop;
                }
            }
        }

        // O4 routing: a stop with standing criteria takes the existing criteria-gate
        // self-check turn first; the model re-decides with the checklist in view.
        if action == PaceAction::Stop
            && self.criteria_gate_enabled
            && !self.criteria_gate_fired
            && !self.ctx.partitions.task_state.criteria.is_empty()
        {
            self.criteria_gate_fired = true;
            let criteria = self.ctx.partitions.task_state.criteria.clone();
            self.push_synthetic_tool_result(
                &call.id,
                "pace(stop) noted — verify the acceptance criteria first, then pace again.",
            );
            self.ctx.push_signal(format!(
                "[CRITERIA CHECK] You proposed stopping the loop. Verify each acceptance \
                 criterion first: {}. If any is NOT met, continue working (or pace(continue)). \
                 If all are met, call pace(stop) again.",
                criteria.join(" | ")
            ));
            self.observations.push(KernelObservation::CriteriaGateFired {
                turn: self.turn,
                criteria,
            });
            self.phase = LoopPhase::Reason;
            return self.emit_call_llm();
        }

        // Sleep clamp into [min, max].
        let delay_ms = if action == PaceAction::Sleep {
            let raw = proposed_delay.unwrap_or(spec.min_sleep_ms.unwrap_or(60_000));
            let mut clamped = raw;
            if let Some(min) = spec.min_sleep_ms {
                clamped = clamped.max(min);
            }
            if let Some(max) = spec.max_sleep_ms {
                clamped = clamped.min(max);
            }
            if clamped != raw && coerced_from.is_none() {
                coerced_from = Some(format!("sleep {raw}ms (clamped)"));
            }
            Some(clamped)
        } else {
            None
        };

        self.local_rounds_completed = self.local_rounds_completed.saturating_add(1);
        let decision = PaceDecision { action, delay_ms, reason, coerced_from };
        self.observations.push(KernelObservation::RoundPaced {
            turn: self.turn,
            round: self.local_rounds_completed,
            decision: decision.clone(),
        });
        self.push_synthetic_tool_result(
            &call.id,
            &format!(
                "pace acknowledged: {}{} — wrap up with a brief round report.",
                decision.action.label(),
                decision
                    .delay_ms
                    .map(|d| format!(" {d}ms"))
                    .unwrap_or_default()
            ),
        );
        self.pending_pace = Some(decision);
        self.pending_termination = Some(TerminationReason::Completed);
        self.phase = LoopPhase::Reason;
        self.emit_call_llm()
    }

    /// Close a kernel-handled tool call's transcript pair with a synthetic result so
    /// providers always see call → result.
    fn push_synthetic_tool_result(&mut self, call_id: &str, output: &str) {
        let msg = Message::tool(vec![crate::types::message::ContentPart::ToolResult {
            call_id: call_id.into(),
            output: output.to_string(),
            is_error: false,
        }]);
        let tokens = self.message_tokens(&msg);
        self.ctx.push_history(msg, tokens);
    }

    fn terminate(
        &mut self,
        termination: TerminationReason,
        final_message: Option<Message>,
    ) -> LoopAction {
        // Commit the final response into history so subsequent session restores
        // include the complete transcript: user → [tool turns] → final assistant.
        if let Some(ref msg) = final_message {
            let tokens = self.message_tokens(msg);
            self.ctx.push_history(msg.clone(), tokens);
        }
        // ③ attach the round's pacing decision. Stashed by the trap when the model
        // called `pace`; otherwise the spec's default_action ("stop" for goal loops,
        // "sleep" for cron loops) — but ONLY on a clean Completed. NoProgress /
        // ContextOverflow / Error rounds stop and surface (nothing nags the model).
        let pace_decision = self.pending_pace.take().or_else(|| {
            let spec = self.run_spec.as_ref()?.loop_round.as_ref()?;
            if termination != TerminationReason::Completed {
                return Some(crate::types::result::PaceDecision {
                    action: crate::types::result::PaceAction::Stop,
                    delay_ms: None,
                    reason: format!("round terminated: {}", termination.label()),
                    coerced_from: None,
                });
            }
            match spec.default_action.as_deref() {
                Some("sleep") => Some(crate::types::result::PaceDecision {
                    action: crate::types::result::PaceAction::Sleep,
                    delay_ms: spec.min_sleep_ms.or(Some(60_000)),
                    reason: "default_action: sleep (cron loop)".to_string(),
                    coerced_from: None,
                }),
                _ => Some(crate::types::result::PaceDecision {
                    action: crate::types::result::PaceAction::Stop,
                    delay_ms: None,
                    reason: "default_action: stop (no pace call this round)".to_string(),
                    coerced_from: None,
                }),
            }
        });
        let result = LoopResult {
            termination,
            final_message,
            turns_used: self.turn,
            total_tokens_used: self.total_tokens,
            loop_continue: None,
            classify_branch: None,
            tournament_winner: None,
            pace_decision,
        };
        self.set_lifecycle(TaskLifecycle::Done(termination), None);
        LoopAction::Done { result }
    }

    /// Build the `CallLLM` action with a structured `RenderedContext`.
    /// Meta-tools (skill / memory / knowledge) are appended to the tool list
    /// when configured. When `pending_termination` is set, tools are stripped
    /// to force a plain-text response before the loop terminates.
    fn emit_call_llm(&mut self) -> LoopAction {
        // Calling the provider is definitionally "running" — the single funnel for entering the
        // Running lifecycle (covers start, resume, signal-driven turns, budget final-call).
        self.set_lifecycle(TaskLifecycle::Running, None);
        self.checkpoint.history_len = self.ctx.partitions.history.messages.len();
        self.checkpoint.signals_len = self.ctx.partitions.signals.len();
        self.checkpoint.task_state = Some(self.ctx.partitions.task_state.clone());
        self.observations.push(KernelObservation::CheckpointTaken {
            turn: self.turn,
            history_len: self.checkpoint.history_len as u32,
        });

        let context = self.ctx.render();
        if self.pending_termination.is_some() {
            return LoopAction::CallLLM {
                context,
                tools: Vec::new(),
            };
        }
        let mut tools = self.tools.clone();
        tools.extend(self.ctx.meta_tool_schemas());

        if let Some(ref spec) = self.run_spec {
            use crate::types::capability::CapabilityKind;
            tools.retain(|tool| {
                let kind = match tool.name.as_str() {
                    "skill" => CapabilityKind::Skill,
                    "memory" => CapabilityKind::Memory,
                    "knowledge" => CapabilityKind::Knowledge,
                    _ => CapabilityKind::Tool,
                };
                let desc = crate::types::capability::CapabilityDescriptor::marker(
                    kind,
                    tool.name.clone(),
                    &tool.description,
                );
                spec.capability_filter.allows(&desc)
            });
        }

        // P1-B epoch skill gating (applied *after* the run-level filter ③, so A is the outer bound
        // and B narrows within it — D6). When skills are active and declare tools, expose only
        // `meta-tools ∪ stable-core ∪ ⋃(active skills' allowed_tools)`. `None` ⇒ no active/declared
        // skill ⇒ no narrowing (D3, errs-open). Meta-tools are always exempt (D5) so the model can
        // still load more skills. Byte-stable within an epoch: the set only changes on activation.
        if let Some(allowed) = self.ctx.active_skill_tool_filter() {
            let stable = &self.ctx.stable_core_tools;
            tools.retain(|tool| {
                matches!(tool.name.as_str(), "skill" | "memory" | "knowledge" | "update_plan")
                    || stable.contains(&tool.name)
                    || allowed.contains(&tool.name)
            });
        }

        // ③ pace meta-tool: exposed ONLY when this run is a round of a paced loop
        // (run_spec.loop_round present) — the same conditional-exposure pattern as
        // skill/memory/read_result. Pushed after every filter: pacing is kernel-owned
        // and must never be narrowed away by skills or capability filters.
        if self.run_spec.as_ref().and_then(|r| r.loop_round.as_ref()).is_some() {
            tools.push(pace_tool_schema());
        }

        LoopAction::CallLLM { context, tools }
    }

    pub fn rollback(&mut self, reason: RollbackReason) {
        self.ctx.partitions.history.messages.truncate(self.checkpoint.history_len);
        self.ctx.partitions.signals.truncate(self.checkpoint.signals_len);
        if let Some(ref state) = self.checkpoint.task_state {
            self.ctx.partitions.task_state = state.clone();
        }
        // Rolled-back turns never reach the boundary sample point; accrue here so the
        // disorder they evidence lands in the next completed turn's entropy window.
        self.entropy.note_rollback();
        self.observations.push(KernelObservation::Rollbacked {
            turn: self.turn,
            checkpoint_history_len: self.checkpoint.history_len as u32,
            reason: Some(reason),
        });
    }

    fn rollback_reason_for_tool_result(&self, result: &ToolResult) -> Option<RollbackReason> {
        let tool_name = self.tool_name_for_call(&result.call_id);
        let output = super::rollback::tool_result_output_text(result);

        if result.is_fatal {
            return Some(RollbackReason::FatalToolError {
                tool_name,
                error: output,
            });
        }

        match result.error_kind {
            Some(ToolErrorKind::Fatal) => Some(RollbackReason::FatalToolError {
                tool_name,
                error: output,
            }),
            Some(ToolErrorKind::GovernanceDenied) => Some(RollbackReason::GovernanceDenied {
                tool_name,
                reason: output,
            }),
            Some(ToolErrorKind::ProviderFailure) => {
                Some(RollbackReason::ProviderFailure { error: output })
            }
            Some(ToolErrorKind::Timeout) => Some(RollbackReason::Timeout),
            Some(ToolErrorKind::UserInterrupt) => Some(RollbackReason::UserInterrupt),
            Some(ToolErrorKind::Recoverable) | None => None,
        }
    }

    fn tool_name_for_call(&self, call_id: &compact_str::CompactString) -> String {
        match &self.phase {
            LoopPhase::Act { tool_calls } => tool_calls
                .iter()
                .find(|call| call.id == *call_id)
                .map(|call| call.name.to_string())
                .unwrap_or_else(|| call_id.to_string()),
            _ => call_id.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// ③ the `pace` meta-tool schema — exposed only on loop-round runs.
fn pace_tool_schema() -> crate::types::message::ToolSchema {
    crate::types::message::ToolSchema {
        name: compact_str::CompactString::new("pace"),
        description: "End this round and decide what happens next: continue immediately, \
sleep then run another round, or stop the loop. Call this when the round's work is done."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "next": { "type": "string", "enum": ["continue", "sleep", "stop"] },
                "delay_ms": { "type": "integer", "minimum": 0 },
                "reason": { "type": "string" }
            },
            "required": ["next", "reason"]
        }),
    }
}
