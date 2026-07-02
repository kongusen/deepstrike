use std::collections::HashMap;

use super::milestone::MilestoneTracker;
use super::policy::LoopPolicy;
use super::tcb::{ScheduleDecision, TaskState, TaskTable, Tcb, WaitReason};
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
use crate::types::signal::RuntimeSignal;
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
/// on the root task's [`TaskState`] in the kernel's `TaskTable`, queried via
/// [`LoopStateMachine::lifecycle`]. `LoopPhase` is now orthogonal: it only records *which step of a
/// running turn* the loop is in. When the task is `Ready/Suspended/Done`, the phase value is
/// inert (left at its last step) and ignored.
#[derive(Debug, Clone)]
pub enum LoopPhase {
    Reason,
    Act { tool_calls: Vec<ToolCall> },
    Observe { results: Vec<ToolResult> },
    Delta { pressure: f64 },
}

/// Why the loop entered `Suspended` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendReason {
    /// Governance `AskUser` — waiting for SDK to resolve human approval.
    AskUser,
    /// Sub-agent spawned — waiting for sub-agent to complete.
    SubAgentAwait,
    /// Externally requested suspension.
    External,
}

/// What the loop is blocked waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// Awaiting a tool's continuation (tool suspend pattern).
    ToolSuspend,
    /// Awaiting milestone evaluation result.
    MilestoneAwait,
}

/// Events fed into the state machine from the SDK layer.
#[derive(Debug)]
pub enum LoopEvent {
    Start {
        task: RuntimeTask,
    },
    LLMResponse {
        message: Message,
    },
    ToolResults {
        results: Vec<ToolResult>,
    },
    /// Inbound signal from SignalRouter — Critical/High urgency may interrupt.
    Signal {
        signal: RuntimeSignal,
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
    Timeout,
}

/// Actions the state machine outputs — SDK layer executes the I/O.
#[derive(Debug)]
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
    /// Kernel is suspended — SDK must resolve (e.g. human approval) and feed `Resume`.
    AwaitingResume,
}

/// Payload held while the loop is in `Suspended`.
#[derive(Debug, Clone)]
pub(super) enum SuspendState {
    /// Governance AskUser — awaiting `Resume { approved_calls, denied_calls }`.
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
    Suspended,
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
    pub(super) policy: LoopPolicy,
    pub(super) total_tokens: u64,
    /// L1 (RunGroup): cumulative tokens spent by *other* members of this run's governance domain,
    /// seeded at boot via `seed_group_budget`. The run-level token cap is enforced against
    /// `group_tokens_base + total_tokens` so the budget spans the whole group, not one vehicle.
    /// 0 (default) ⇒ no group (N=1) ⇒ pre-L1 per-kernel behavior (byte-identical).
    pub(super) group_tokens_base: u64,
    /// L1 (RunGroup): sub-agents spawned by *other* members of this run's governance domain, seeded
    /// at boot. `max_total_subagents` is enforced against `group_spawns_base + local spawns`. 0 ⇒ N=1.
    pub(super) group_spawns_base: u32,
    /// When set, the next LLM call strips tools to force a text response,
    /// then terminates with this reason once the response arrives.
    pub(super) pending_termination: Option<TerminationReason>,
    /// Reactive context-overflow recovery: consecutive compact-and-retry attempts since the last
    /// successful provider turn. Bounds the recovery ladder (anti-spiral) and resets to 0 on any
    /// `LLMResponse`, mirroring the per-turn `hasAttemptedReactiveCompact` reset the SDK runners
    /// used to own. See `recover_from_provider_error`.
    pub(super) recovery_attempts: u8,
    /// Max-output-tokens recovery: consecutive continue-and-retry turns since the model last
    /// finished a response WITHOUT hitting the output cap. When a turn is cut off at the cap
    /// (provider `stop_reason` = max_tokens/length) the kernel keeps the partial, nudges the model
    /// to resume mid-thought, and re-calls — bounded by `MAX_OUTPUT_RECOVERY` (mirrors query.ts's
    /// MAX_OUTPUT_TOKENS_RECOVERY_LIMIT). Resets to 0 on any non-truncated response.
    pub(super) output_recovery_attempts: u8,
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
    /// Optional in-kernel signal router. When set, inbound signals are routed
    /// through dedup + attention policy + queue here (the kernel owns disposition).
    /// `None` (default) keeps the legacy hardcoded urgency handling in `feed`.
    pub(super) signal_router: Option<SignalRouter>,
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

    pub fn new(policy: LoopPolicy) -> Self {
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
            group_tokens_base: 0,
            group_spawns_base: 0,
            pending_termination: None,
            recovery_attempts: 0,
            output_recovery_attempts: 0,
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
            signal_router: Some(SignalRouter::new(64)),
            started_at_ms: None,
            last_now_ms: None,
            suspend_state: None,
            pending_denied_results: Vec::new(),
            workflow: None,
            repeat_fuse: RepeatFuseConfig::default(),
            repeat_sig: None,
            repeat_count: 0,
            criteria_gate_enabled: true,
            criteria_gate_fired: false,
        }
    }

    /// O4: enable/disable the turn-end criteria gate (default enabled; no-op without criteria).
    pub fn set_criteria_gate(&mut self, enabled: bool) {
        self.criteria_gate_enabled = enabled;
    }

    /// O6: tune or disable the repeat fuse (see [`RepeatFuseConfig`]).
    pub fn set_repeat_fuse(&mut self, config: RepeatFuseConfig) {
        self.repeat_fuse = config;
    }

    /// O6: the active repeat-fuse config (for read-modify-write from the ABI event).
    pub fn repeat_fuse_config(&self) -> RepeatFuseConfig {
        self.repeat_fuse
    }

    /// The authoritative schedulability lifecycle of the loop (root task state). Replaces the
    /// removed `LoopPhase::{Idle,Suspended,Blocked,Terminal}` reads.
    pub fn lifecycle(&self) -> TaskState {
        self.tasks.get("root").map(|t| t.state).unwrap_or(TaskState::Ready)
    }

    /// The wait reason while suspended/blocked, if any.
    pub fn wait_reason(&self) -> Option<WaitReason> {
        self.tasks.get("root").and_then(|t| t.wait.clone())
    }

    /// Whether the loop has terminated.
    pub fn is_terminal(&self) -> bool {
        matches!(self.lifecycle(), TaskState::Done(_))
    }

    /// Whether the loop is suspended awaiting external resolution.
    pub fn is_suspended(&self) -> bool {
        matches!(self.lifecycle(), TaskState::Suspended)
    }

    /// Set the root task's lifecycle (and wait reason). Single mutation point for schedulability.
    fn set_lifecycle(&mut self, state: TaskState, wait: Option<WaitReason>) {
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
        // L1: the token-budget axis is evaluated against the whole governance domain's cumulative
        // spend (this vehicle's `total_tokens` plus other members' `group_tokens_base`).
        tcb.budget.total_tokens = self.total_tokens.saturating_add(self.group_tokens_base);
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

    /// L1 (RunGroup): seed the cumulative tokens already spent by other members of this run's
    /// governance domain. The run-level token cap is then enforced against the group total. Seeding
    /// 0 (the default) preserves pre-L1 per-vehicle behavior.
    pub fn seed_group_budget(&mut self, tokens_spent: u64) {
        self.group_tokens_base = tokens_spent;
    }

    /// L1 (RunGroup): seed the sub-agents already spawned by other members of this run's governance
    /// domain. `max_total_subagents` is then enforced against the group total. 0 ⇒ pre-L1 behavior.
    pub fn seed_group_spawns(&mut self, subagents_spawned: u32) {
        self.group_spawns_base = subagents_spawned;
    }

    /// L1: this vehicle's cumulative sub-agent spawns this run — every child task ever registered in
    /// the `TaskTable` (running + completed), distinct from the *instantaneous* running count. Used
    /// for the cumulative spawn quota and read back by the SDK to charge the group ledger at run end.
    pub fn local_subagents_spawned(&self) -> u32 {
        self.tasks.all().iter().filter(|t| t.proc.is_some()).count() as u32
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
            self.set_lifecycle(TaskState::Running, None);
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

        match event {
            LoopEvent::Start { task } => self.start(task),

            LoopEvent::LLMResponse { message } => {
                // A response arrived ⇒ the prompt fit ⇒ the overflow recovery ladder is reset.
                self.recovery_attempts = 0;
                let tokens = self.message_tokens(&message);
                self.total_tokens += tokens as u64;

                // Max-output-tokens recovery (mirrors query.ts): a response cut off at the output
                // cap reports stop_reason = max_tokens (Anthropic) / length (OpenAI). A clean finish
                // resets the ladder.
                const MAX_OUTPUT_RECOVERY: u8 = 3;
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
                    if truncated && self.output_recovery_attempts < MAX_OUTPUT_RECOVERY {
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
                            .contract
                            .as_ref()
                            .and_then(|c| c.phases.get(self.milestone.current_phase))
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
                    GateToolOutcome::Suspended => return LoopAction::AwaitingResume,
                    GateToolOutcome::Proceed => {}
                }
                self.phase = LoopPhase::Act {
                    tool_calls: calls.clone(),
                };
                self.set_lifecycle(TaskState::Running, None);
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

                for r in &results {
                    self.total_tokens += r.token_count.unwrap_or(0) as u64;
                    // Preserve Content::Parts (structured / multimodal tool output).
                    // Parts are serialised to JSON so the text can be restored faithfully.
                    let raw_output = match &r.output {
                        Content::Text(s) => s.clone(),
                        Content::Parts(parts) => serde_json::to_string(parts).unwrap_or_default(),
                    };
                    // Layer 1 spool: oversized results keep only a preview in context; the kernel
                    // emits `LargeResultSpooled` so the SDK persists the full output it still holds.
                    let (output, spooled) = match crate::mm::plan_spool(
                        &raw_output,
                        self.ctx.config.spool_threshold_bytes,
                        self.ctx.config.spool_preview_bytes,
                    ) {
                        Some(decision) => {
                            self.observations.push(KernelObservation::LargeResultSpooled {
                                turn: self.turn,
                                call_id: r.call_id.to_string(),
                                // ToolResult carries no tool name; the SDK maps call_id -> tool.
                                tool: String::new(),
                                original_size: decision.original_size,
                                preview_size: decision.preview.len() as u32,
                                spool_ref: None,
                            });
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
                    // Layer 1: a spooled result's handle is marked SpooledOut (its full output now
                    // lives on disk via the SDK); the SDK maps call_id -> the persisted ref.
                    if spooled {
                        self.ctx.mark_spooled(&r.call_id, r.call_id.to_string());
                    }
                }
                self.turn += 1;

                // M1 收口: the pure `schedule()` is now the single budget decision point.
                // It evaluates the same three axes (turn/token/wall) via `BudgetLedger`, which
                // delegates to `SchedulerBudget::should_terminate` internally — one source of truth.
                if let ScheduleDecision::Terminate { reason: term, .. } =
                    super::tcb::schedule(&self.root_tcb(), self.last_now_ms)
                {
                    let budget = match term {
                        TerminationReason::MaxTurns => "max_turns",
                        TerminationReason::Timeout => "wall_time",
                        _ => "token_budget",
                    };
                    self.observations.push(KernelObservation::BudgetExceeded {
                        turn: self.turn,
                        budget: budget.to_string(),
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
                self.phase = LoopPhase::Delta {
                    pressure: self.ctx.rho(),
                };

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
                    if let Some(router) = self.signal_router.as_mut() {
                        router.clear_dedup();
                    }
                    self.observations.push(KernelObservation::Renewed {
                        sprint: self.ctx.sprint,
                    });
                    // K1: renewal is a boundary — surface the knowledge sweep it just ran.
                    self.emit_knowledge_sweep_observations();
                }

                // Turn boundary: drain any kernel-queued signals into context so they
                // are seen on the next reasoning turn (ready queue → running).
                self.drain_queued_signals();

                self.phase = LoopPhase::Reason;
                self.emit_call_llm()
            }

            LoopEvent::Signal { signal } => {
                // `feed` always returns an action; non-actionable dispositions
                // (queue/observe/ignore) fall back to a plain provider call here.
                // The kernel-routed path (`dispatch_signal`) is driven via the ABI.
                self.dispatch_signal(signal)
                    .unwrap_or_else(|| self.emit_call_llm())
            }

            LoopEvent::MilestoneResult { result } => self.handle_milestone_result(result),

            LoopEvent::SubAgentCompleted { result } => self.handle_sub_agent_completed(result),

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

    /// W2-2: Create a snapshot of the current kernel state for crash recovery or migration.
    pub fn snapshot(&self) -> crate::runtime::snapshot::KernelSnapshot {
        use crate::runtime::snapshot::{ContextSnapshot, KernelSnapshot};
        let context = ContextSnapshot::from_context(&self.ctx);
        KernelSnapshot::from_state(
            self.turn,
            self.total_tokens,
            &self.tasks,
            &context,
            self.run_spec.as_ref(),
        )
    }

    /// W2-2: Restore kernel state from a snapshot. Returns a new LoopStateMachine rebuilt from the snapshot.
    /// Note: This is a foundational restore - some state (governance, milestone, signal router dedup) is
    /// recreated from policy/config rather than serialized, following the principle that strategy is data.
    pub fn restore(snap: &crate::runtime::snapshot::KernelSnapshot) -> Self {
        use crate::signals::router::SignalRouter;

        // Reconstruct policy from the max_tokens in snapshot
        let policy = crate::scheduler::policy::LoopPolicy {
            max_tokens: snap.context.max_tokens,
            ..Default::default()
        };

        // Rebuild TaskTable from snapshot TCBs
        let mut tasks = TaskTable::new();
        for tcb_snap in &snap.tasks {
            if let Some(tcb) = snap.restore_tcb(tcb_snap) {
                tasks.insert(tcb);
            }
        }

        // Rebuild context partitions from snapshot
        let mut ctx = ContextManager::new(snap.context.max_tokens);
        ctx.sprint = snap.context.sprint;

        // Restore messages
        for msg in &snap.context.system_messages {
            let tokens = ctx.engine.count_message(msg);
            ctx.partitions.system.push(msg.clone(), tokens);
        }
        // K1: restore entry identity (key/pinned) from the index-parallel meta vec; pre-K1
        // snapshots have no meta ⇒ every entry restores unkeyed/unpinned (graceful).
        for (i, msg) in snap.context.knowledge_messages.iter().enumerate() {
            let tokens = ctx.engine.count_message(msg);
            let meta = snap.context.knowledge_entries_meta.get(i);
            ctx.partitions.knowledge.push_entry(
                meta.and_then(|m| m.key.as_deref()).map(compact_str::CompactString::new),
                msg.clone(),
                tokens,
                meta.map(|m| m.pinned).unwrap_or(false),
            );
        }
        for msg in &snap.context.history_messages {
            let tokens = ctx.engine.count_message(msg);
            ctx.partitions.history.push(msg.clone(), tokens);
        }

        // Restore task state
        if let Some(goal) = &snap.context.task_goal {
            ctx.partitions.task_state.goal = goal.clone();
        }
        if let Some(plan_json) = &snap.context.task_plan {
            if let Ok(plan_steps) = serde_json::from_str::<Vec<crate::context::task_state::PlanStep>>(plan_json) {
                ctx.partitions.task_state.plan = plan_steps;
            }
        }
        if let Some(progress) = &snap.context.task_progress {
            ctx.partitions.task_state.progress = progress.clone();
        }
        ctx.partitions.task_state.directives = snap.context.task_directives.clone();

        // Restore signals
        ctx.partitions.signals = snap.context.signals.clone();

        Self {
            phase: LoopPhase::Reason,
            turn: snap.turn,
            ctx,
            tools: Vec::new(),  // Tools are rebuilt from capabilities on next LLM call
            observations: Vec::new(),
            policy,
            total_tokens: snap.total_tokens,
            // Re-seeded from the replayed `ConfigureRun` (strategy is data, not serialized state).
            group_tokens_base: 0,
            group_spawns_base: 0,
            pending_termination: None,
            recovery_attempts: 0,
            output_recovery_attempts: 0,
            pending_stop_reason: None,
            session_history_baseline: 0,
            checkpoint: TurnCheckpoint::default(),
            milestone: crate::scheduler::milestone::MilestoneTracker::new(),
            run_spec: snap.run_spec(),
            tasks,
            governance: None,  // Governance is policy data, recreated from config
            resource_quota: None,
            memory_write_times: Vec::new(),
            memory_policy: None,
            signal_router: Some(SignalRouter::new(64)),  // Dedup cleared on restore
            started_at_ms: None,
            last_now_ms: None,
            suspend_state: None,
            pending_denied_results: Vec::new(),
            workflow: None,
            // Re-seeded from the replayed `ConfigureRun` / `SetRepeatFuse` (config, not state);
            // the streak itself intentionally restarts on restore (stale across a suspend).
            repeat_fuse: RepeatFuseConfig::default(),
            repeat_sig: None,
            repeat_count: 0,
            criteria_gate_enabled: true,
            criteria_gate_fired: false,
        }
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
        let result = LoopResult {
            termination,
            final_message,
            turns_used: self.turn,
            total_tokens_used: self.total_tokens,
            loop_continue: None,
            classify_branch: None,
            tournament_winner: None,
        };
        self.set_lifecycle(TaskState::Done(termination), None);
        LoopAction::Done { result }
    }

    /// Build the `CallLLM` action with a structured `RenderedContext`.
    /// Meta-tools (skill / memory / knowledge) are appended to the tool list
    /// when configured. When `pending_termination` is set, tools are stripped
    /// to force a plain-text response before the loop terminates.
    fn emit_call_llm(&mut self) -> LoopAction {
        // Calling the provider is definitionally "running" — the single funnel for entering the
        // Running lifecycle (covers start, resume, signal-driven turns, budget final-call).
        self.set_lifecycle(TaskState::Running, None);
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

        LoopAction::CallLLM { context, tools }
    }

    pub fn rollback(&mut self, reason: RollbackReason) {
        self.ctx.partitions.history.messages.truncate(self.checkpoint.history_len);
        self.ctx.partitions.signals.truncate(self.checkpoint.signals_len);
        if let Some(ref state) = self.checkpoint.task_state {
            self.ctx.partitions.task_state = state.clone();
        }
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
