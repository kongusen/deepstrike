use std::collections::HashMap;

use super::milestone::MilestoneTracker;
use super::policy::LoopPolicy;
use super::tcb::{ScheduleDecision, TaskState, TaskTable, Tcb, WaitReason};
use crate::mm::{page_in_requests_from_calls, tier_hint_for_compress};
use crate::AgentRunSpec;
use crate::context::manager::ContextManager;
use crate::governance::pipeline::GovernancePipeline;
use crate::syscall::{Disposition, Syscall};
use crate::proc::{AgentProcess, ProcessState, ProcessTable};
use crate::signals::router::SignalRouter;
use crate::types::agent::AgentIdentity;
use crate::types::policy::SignalDisposition;
use crate::types::result::SubAgentResult;
use crate::context::pressure::PressureAction;
use crate::context::renderer::RenderedContext;
use crate::runtime::session::RollbackReason;
use crate::types::message::{
    Content, ContentPart, Message, ToolCall, ToolErrorKind, ToolResult, ToolSchema,
};
use crate::types::milestone::{MilestoneCheckResult, MilestoneContract};
use crate::types::result::{LoopResult, TerminationReason};
use crate::types::signal::{RuntimeSignal, Urgency};
use crate::types::task::RuntimeTask;

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
enum SuspendState {
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

enum GateToolOutcome {
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

/// One-shot observation emitted by the kernel during `feed`.
/// SDK drains this between calls for telemetry/UI updates.
#[derive(Debug, Clone)]
pub enum LoopObservation {
    Compressed {
        action: PressureAction,
        rho_after: f64,
        summary: Option<String>,
        archived: Vec<Message>,
    },
    /// Working memory paged out to long-term — SDK persists `archived` and optional summary.
    PageOut {
        turn: u32,
        action: PressureAction,
        rho_after: f64,
        summary: Option<String>,
        archived: Vec<Message>,
        tier_hint: String,
    },
    /// Kernel requests SDK to fetch long-term memory before executing a meta-tool.
    PageInRequested {
        turn: u32,
        call_id: String,
        tool: String,
        query: String,
        top_k: u32,
    },
    /// Context renewal fired — a new sprint started to carry the conversation forward.
    Renewed { sprint: u32 },
    /// Rollback event indicating a turn execution failure led to restoring state
    Rollbacked {
        turn: u32,
        checkpoint_history_len: u32,
        reason: RollbackReason,
    },
    /// Capabilities dynamically updated
    CapabilityChanged {
        turn: u32,
        added: Vec<String>,
        removed: Vec<String>,
        change_kind: Option<String>,
        capability_id: Option<String>,
        version: Option<String>,
        mounted_by: Option<String>,
        mount_reason: Option<String>,
    },
    /// Milestone phase satisfied — capabilities unlocked, phase advanced.
    MilestoneAdvanced {
        turn: u32,
        phase_id: String,
        capabilities_unlocked: Vec<String>,
    },
    /// Milestone assertion failed — loop continues without phase advancement.
    MilestoneBlocked {
        turn: u32,
        phase_id: String,
        reason: String,
    },
    /// Evidence collected by the verifier during milestone evaluation.
    MilestoneEvidence {
        turn: u32,
        phase_id: String,
        evidence: Vec<String>,
    },
    /// Checkpoint taken at the start of a turn transaction (before LLM call).
    CheckpointTaken {
        turn: u32,
        history_len: u32,
    },
    /// Kernel process table changed for a spawned sub-agent.
    AgentProcessChanged {
        turn: u32,
        agent_id: String,
        parent_session_id: String,
        role: crate::types::agent::AgentRole,
        isolation: crate::types::agent::AgentIsolation,
        context_inheritance: crate::types::agent::ContextInheritance,
        state: ProcessState,
        permitted_capability_ids: Vec<String>,
        result_termination: Option<String>,
    },
    /// A tool call requires user approval (governance `AskUser` verdict).
    /// The kernel does not block it — the SDK is responsible for obtaining
    /// approval before executing the named call.
    ToolGated {
        turn: u32,
        call_id: String,
        tool: String,
        reason: String,
    },
    /// An inbound signal was routed by the in-kernel attention policy.
    /// `disposition` is the kernel's decision; `queue_depth` is the post-routing
    /// queue length (signals awaiting a turn boundary).
    SignalDisposed {
        turn: u32,
        signal_id: String,
        disposition: String,
        queue_depth: u32,
    },
    /// Budget axis exhausted (turns / tokens / wall-time). Emitted alongside the
    /// pending-termination path; SDK uses this for telemetry.
    BudgetExceeded {
        turn: u32,
        budget: String,
    },
    /// Loop entered `Suspended` state (AskUser / SubAgentAwait / External).
    Suspended {
        turn: u32,
        reason: String,
        /// call IDs awaiting approval (for AskUser).
        #[allow(dead_code)]
        pending_calls: Vec<String>,
    },
    /// Loop resumed from `Suspended` state.
    Resumed {
        turn: u32,
        approved: Vec<String>,
        denied: Vec<String>,
    },
    /// Memory entry written successfully (Phase 7).
    MemoryWritten {
        turn: u32,
        memory_id: String,
        memory_kind: String,
        size_bytes: u32,
    },
    /// Memory validation failed (Phase 7).
    MemoryValidationFailed {
        turn: u32,
        memory_id: String,
        error: String,
    },
    /// Memory query request (Phase 7).
    MemoryQueried {
        turn: u32,
        query_context: String,
        requested_k: usize,
        requires_async_response: bool,
    },
    /// Large tool result spooled to disk (Layer 1).
    LargeResultSpooled {
        turn: u32,
        call_id: String,
        tool: String,
        original_size: u32,
        preview_size: u32,
        spool_ref: Option<String>,
    },
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
    pub observations: Vec<LoopObservation>,
    policy: LoopPolicy,
    total_tokens: u64,
    /// When set, the next LLM call strips tools to force a text response,
    /// then terminates with this reason once the response arrives.
    pending_termination: Option<TerminationReason>,
    /// Number of history messages present at session start (after preload_history).
    /// drain_new_messages() returns the slice from this offset onward.
    session_history_baseline: usize,
    checkpoint: TurnCheckpoint,
    /// Milestone contract tracker (extracted to reduce state machine bloat).
    milestone: MilestoneTracker,
    pub run_spec: Option<AgentRunSpec>,
    processes: ProcessTable,
    /// M1c: canonical task registry (root task + one row per sub-agent). Maintained in
    /// parallel with `processes` and `debug_assert`-equal to its lineage/lifecycle, so M1d can
    /// make `ProcessTable` a derived view and drop its storage. Root is task `"root"`.
    tasks: TaskTable,
    /// Optional governance pipeline. When set, every tool call proposed by the
    /// model is evaluated before `ExecuteTools` is emitted. `None` (default)
    /// skips the gate entirely, preserving the pre-governance behavior.
    governance: Option<GovernancePipeline>,
    /// Optional in-kernel signal router. When set, inbound signals are routed
    /// through dedup + attention policy + queue here (the kernel owns disposition).
    /// `None` (default) keeps the legacy hardcoded urgency handling in `feed`.
    signal_router: Option<SignalRouter>,
    /// Wall-clock timestamp of the first `ProviderResult.now_ms` received.
    /// Used by the wall-time budget axis in `SchedulerBudget::should_terminate`.
    started_at_ms: Option<u64>,
    /// Most-recent `now_ms` value from `ProviderResult`, forwarded to the budget check.
    last_now_ms: Option<u64>,
    /// Tool batch awaiting `Resume` after an AskUser suspend.
    suspend_state: Option<SuspendState>,
    /// Denied tool results to merge into the next `ToolResults` feed after resume.
    pending_denied_results: Vec<ToolResult>,
}

/// Stable snake_case label for a signal disposition, used in `SignalDisposed`
/// observations (part of the observation wire format).
fn disposition_label(d: &SignalDisposition) -> &'static str {
    match d {
        SignalDisposition::Ignore => "ignore",
        SignalDisposition::Observe => "observe",
        SignalDisposition::Queue => "queue",
        SignalDisposition::Run { .. } => "run",
        SignalDisposition::Interrupt => "interrupt",
        SignalDisposition::InterruptNow => "interrupt_now",
        SignalDisposition::Dropped => "dropped",
    }
}

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
            pending_termination: None,
            session_history_baseline: 0,
            checkpoint: TurnCheckpoint::default(),
            milestone: MilestoneTracker::new(),
            run_spec: None,
            processes: ProcessTable::new(),
            tasks,
            governance: None,
            signal_router: None,
            started_at_ms: None,
            last_now_ms: None,
            suspend_state: None,
            pending_denied_results: Vec::new(),
        }
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
        tcb.budget.total_tokens = self.total_tokens;
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

    /// Enable in-kernel signal routing with the default urgency-based attention
    /// policy and a bounded queue. Once set, inbound signals are dispatched through
    /// the kernel (dedup + disposition + queue) instead of the legacy `feed` path.
    pub fn set_attention(&mut self, max_queue_size: usize) {
        self.signal_router = Some(SignalRouter::new(max_queue_size));
    }

    /// ABI entry for an inbound signal: clears observations, sweeps leases, then
    /// dispatches through the in-kernel router (or the legacy path). Returns
    /// `None` when the signal does not drive a provider call this step
    /// (queued / observed / ignored / dropped).
    pub fn signal_event(&mut self, signal: RuntimeSignal) -> Option<LoopAction> {
        self.observations.clear();
        self.sweep_expired_leases();
        self.dispatch_signal(signal)
    }

    /// Route a signal and decide whether it drives a turn now. Assumes the caller
    /// has already cleared observations / swept leases (see `feed` and `signal_event`).
    fn dispatch_signal(&mut self, signal: RuntimeSignal) -> Option<LoopAction> {
        let is_running = !matches!(self.lifecycle(), TaskState::Ready | TaskState::Done(_));
        match self.signal_router.as_mut() {
            Some(router) => {
                let signal_id = signal.id.to_string();
                let summary = signal.summary.to_string();
                let disposition = router.ingest(signal, is_running);
                let queue_depth = router.depth() as u32;
                self.observations.push(LoopObservation::SignalDisposed {
                    turn: self.turn,
                    signal_id,
                    disposition: disposition_label(&disposition).to_string(),
                    queue_depth,
                });
                match disposition {
                    SignalDisposition::InterruptNow | SignalDisposition::Interrupt => {
                        self.ctx.push_signal(format!("[INTERRUPT] {summary}"));
                        self.phase = LoopPhase::Reason;
                        Some(self.emit_call_llm())
                    }
                    SignalDisposition::Run { .. } => {
                        self.ctx.push_signal(format!("[SIGNAL] {summary}"));
                        self.phase = LoopPhase::Reason;
                        Some(self.emit_call_llm())
                    }
                    // Observe: note it in context but don't force a turn.
                    SignalDisposition::Observe => {
                        self.ctx.push_signal(format!("[SIGNAL] {summary}"));
                        None
                    }
                    // Queued in the kernel (drained at the next turn boundary), or
                    // deduped / dropped — no provider call this step.
                    SignalDisposition::Queue
                    | SignalDisposition::Ignore
                    | SignalDisposition::Dropped => None,
                }
            }
            // COMPAT(signal-legacy): hardcoded urgency handling, pre-attention-policy.
            // Active only when no SetAttentionPolicy was issued. Removable once all
            // SDKs drive signals through the in-kernel router.
            None => Some(self.legacy_signal(signal)),
        }
    }

    /// Drain all kernel-queued signals into the current context as runtime notes.
    /// No-op when no router is configured. Called at turn boundaries.
    fn drain_queued_signals(&mut self) {
        let drained: Vec<String> = match self.signal_router.as_mut() {
            Some(router) => {
                let mut out = Vec::new();
                while let Some(sig) = router.next() {
                    out.push(sig.summary.to_string());
                }
                out
            }
            None => Vec::new(),
        };
        for summary in drained {
            self.ctx.push_signal(format!("[SIGNAL] {summary}"));
        }
    }

    fn legacy_signal(&mut self, signal: RuntimeSignal) -> LoopAction {
        match signal.urgency {
            Urgency::Critical => {
                self.ctx.push_signal(format!("[INTERRUPT] {}", signal.summary));
                self.phase = LoopPhase::Reason;
                self.emit_call_llm()
            }
            Urgency::High => {
                self.ctx.push_signal(format!("[SIGNAL] {}", signal.summary));
                self.emit_call_llm()
            }
            _ => self.emit_call_llm(),
        }
    }

    /// Drop capability leases whose expiry turn has passed. Runs at the head of
    /// every event so expired temporary capabilities are unmounted promptly.
    fn sweep_expired_leases(&mut self) {
        let current_turn = self.turn;
        let mut to_remove = Vec::new();
        for cap in self.ctx.capabilities.capabilities() {
            if let Some(ref lease) = cap.lease {
                if current_turn >= lease.expires_at_turn {
                    to_remove.push((cap.kind, cap.id.to_string()));
                }
            }
        }
        for (kind, id) in to_remove {
            self.unmount_capability(kind, &id);
        }
    }

    /// P1 (M2): the single syscall trap. Every effectful request the SDK proposes is adjudicated
    /// here, returning a unified [`Disposition`]. Tool calls run the governance pipeline (mapping
    /// its verdict via `GovernanceVerdict -> Disposition`); `Spawn`/`PageIn`/memory carry no policy
    /// stages yet and default to `Allow` — the chokepoint exists so quotas/rules can attach later
    /// without a new ABI.
    fn evaluate_syscall(&mut self, sys: &Syscall) -> Disposition {
        match sys {
            Syscall::Invoke(call) => {
                let caller = self
                    .run_spec
                    .as_ref()
                    .map(|s| s.identity.clone())
                    .unwrap_or_else(|| AgentIdentity::new("agent", "session"));
                match self.governance.as_mut() {
                    Some(pipeline) => pipeline.evaluate(call, &caller).into(),
                    None => Disposition::Allow,
                }
            }
            Syscall::Spawn(_)
            | Syscall::PageIn(_)
            | Syscall::WriteMemory(_)
            | Syscall::QueryMemory(_) => Disposition::Allow,
        }
    }

    /// Evaluate proposed tool calls through the syscall trap (governance gate).
    fn gate_tool_calls(&mut self, calls: &[ToolCall]) -> GateToolOutcome {
        if self.governance.is_none() {
            return GateToolOutcome::Proceed;
        }

        let mut gated: Vec<(String, String, String)> = Vec::new();
        let mut hard_block: Option<(String, String)> = None;
        for call in calls {
            match self.evaluate_syscall(&Syscall::Invoke(call.clone())) {
                Disposition::Allow | Disposition::Transform(_) => {}
                Disposition::Gate { reason, .. } => {
                    gated.push((call.id.to_string(), call.name.to_string(), reason));
                }
                Disposition::Deny { reason, .. } => {
                    if hard_block.is_none() {
                        hard_block = Some((call.name.to_string(), reason));
                    }
                }
                Disposition::RateLimited { retry_after_ms } => {
                    if hard_block.is_none() {
                        hard_block = Some((
                            call.name.to_string(),
                            format!("rate limited, retry after {retry_after_ms}ms"),
                        ));
                    }
                }
                // Backpressure deferral is not produced by the governance gate today.
                Disposition::Defer { .. } => {}
            }
        }

        if let Some((tool_name, reason)) = hard_block {
            let rb = RollbackReason::GovernanceDenied { tool_name, reason };
            let note = Message::user(super::rollback::build_rollback_note(
                &rb,
                self.ctx.config.verbose_control_notes,
            ));
            self.rollback(rb);
            self.ctx
                .push_signal(note.content.as_text().unwrap_or_default().to_string());
            self.phase = LoopPhase::Reason;
            return GateToolOutcome::Blocked(self.emit_call_llm());
        }

        if gated.is_empty() {
            return GateToolOutcome::Proceed;
        }

        let pending_calls: Vec<String> = gated.iter().map(|(id, _, _)| id.clone()).collect();
        let gated_reasons: HashMap<String, String> = gated
            .iter()
            .map(|(id, _, reason)| (id.clone(), reason.clone()))
            .collect();
        for (call_id, tool, reason) in &gated {
            self.observations.push(LoopObservation::ToolGated {
                turn: self.turn,
                call_id: call_id.clone(),
                tool: tool.clone(),
                reason: reason.clone(),
            });
        }
        self.suspend_state = Some(SuspendState::AskUser {
            calls: calls.to_vec(),
            gated_reasons,
        });
        self.set_lifecycle(TaskState::Suspended, Some(WaitReason::Approval));
        self.observations.push(LoopObservation::Suspended {
            turn: self.turn,
            reason: "ask_user".to_string(),
            pending_calls,
        });
        GateToolOutcome::Suspended
    }

    /// Resume from `Suspended` after SDK resolves human approval (or wake preload).
    pub fn resume_from_suspend(
        &mut self,
        approved_calls: Vec<String>,
        denied_calls: Vec<String>,
    ) -> LoopAction {
        self.observations.clear();

        if self.suspend_state.is_none() && approved_calls.is_empty() && denied_calls.is_empty() {
            return self.resume_after_preload();
        }

        let Some(state) = self.suspend_state.take() else {
            if approved_calls.is_empty() && denied_calls.is_empty() {
                return self.resume_after_preload();
            }
            return LoopAction::AwaitingResume;
        };

        if !self.is_suspended() {
            return LoopAction::AwaitingResume;
        }

        self.observations.push(LoopObservation::Resumed {
            turn: self.turn,
            approved: approved_calls.clone(),
            denied: denied_calls.clone(),
        });

        let approved_set: std::collections::HashSet<String> = approved_calls.into_iter().collect();
        let denied_set: std::collections::HashSet<String> = denied_calls.into_iter().collect();

        let SuspendState::AskUser { calls, gated_reasons } = state else {
            return LoopAction::AwaitingResume;
        };

        let mut to_execute = Vec::new();
        let mut synthetic_results = Vec::new();

        for call in calls {
            let id = call.id.to_string();
            if let Some(reason) = gated_reasons.get(&id) {
                if approved_set.contains(&id) {
                    to_execute.push(call.clone());
                } else if denied_set.contains(&id) || !approved_set.contains(&id) {
                    synthetic_results.push(ToolResult {
                        call_id: call.id.clone(),
                        output: Content::Text(format!("permission denied: {reason}")),
                        is_error: true,
                        is_fatal: false,
                        error_kind: Some(ToolErrorKind::GovernanceDenied),
                        token_count: None,
                    });
                }
            } else {
                to_execute.push(call.clone());
            }
        }

        self.pending_denied_results = synthetic_results;

        if to_execute.is_empty() {
            let results = std::mem::take(&mut self.pending_denied_results);
            self.phase = LoopPhase::Reason;
            self.set_lifecycle(TaskState::Running, None);
            return self.feed(LoopEvent::ToolResults { results });
        }

        self.phase = LoopPhase::Act {
            tool_calls: to_execute.clone(),
        };
        self.set_lifecycle(TaskState::Running, None);
        LoopAction::ExecuteTools {
            calls: to_execute,
        }
    }

    /// 强行进行一次最大力度的压缩归档。通常用于收到模型 API 413 (Prompt too long) 时做兜底重试。
    pub fn force_compact(&mut self) -> bool {
        let action = PressureAction::AutoCompact;
        let (saved, summary, archived) = self.ctx.force_compress();
        if saved > 0 {
            self.push_compression_observations(action, summary, archived);
            true
        } else {
            false
        }
    }

    fn push_compression_observations(
        &mut self,
        action: PressureAction,
        summary: Option<String>,
        archived: Vec<Message>,
    ) {
        let rho_after = self.ctx.rho();
        self.observations.push(LoopObservation::Compressed {
            action,
            rho_after,
            summary: summary.clone(),
            archived: archived.clone(),
        });
        if archived.is_empty() {
            return;
        }
        let tier_hint = tier_hint_for_compress(action).label().to_string();
        self.observations.push(LoopObservation::PageOut {
            turn: self.turn,
            action,
            rho_after,
            summary,
            archived,
            tier_hint,
        });
    }

    fn emit_page_in_requested(&mut self, calls: &[ToolCall]) {
        for req in page_in_requests_from_calls(calls) {
            self.observations.push(LoopObservation::PageInRequested {
                turn: self.turn,
                call_id: req.call_id,
                tool: req.tool,
                query: req.query,
                top_k: req.top_k,
            });
        }
    }

    /// Apply SDK-fetched long-term entries into the knowledge partition (page-in).
    pub fn apply_page_in(&mut self, entries: &[crate::mm::PageInEntry]) {
        for entry in entries {
            let tokens = entry
                .tokens
                .unwrap_or_else(|| self.ctx.engine.count(&entry.content).max(1));
            self.ctx.push_knowledge(Message::system(entry.content.clone()), tokens);
        }
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
            self.emit_page_in_requested(&calls);
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
                let tokens = self.message_tokens(&message);
                self.total_tokens += tokens as u64;

                if let Some(reason) = self.pending_termination.take() {
                    return self.terminate(reason, Some(message));
                }

                if message.tool_calls.is_empty() {
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
                    return self.terminate(TerminationReason::Completed, Some(message));
                }

                let calls = message.tool_calls.clone();
                self.ctx.push_history(message, tokens);

                // ━━ 记录活动时间（Layer 3时间衰减使用）
                if let Some(now_ms) = self.last_now_ms {
                    self.ctx.record_activity(now_ms);
                }

                match self.gate_tool_calls(&calls) {
                    GateToolOutcome::Blocked(action) => return action,
                    GateToolOutcome::Suspended => return LoopAction::AwaitingResume,
                    GateToolOutcome::Proceed => {}
                }
                self.emit_page_in_requested(&calls);
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
                    let output = match &r.output {
                        Content::Text(s) => s.clone(),
                        Content::Parts(parts) => serde_json::to_string(parts).unwrap_or_default(),
                    };
                    let parts = vec![ContentPart::ToolResult {
                        call_id: r.call_id.clone(),
                        output,
                        is_error: r.is_error,
                    }];
                    let tool_msg = Message::tool(parts);
                    let tokens = r
                        .token_count
                        .unwrap_or_else(|| self.ctx.engine.count_message(&tool_msg));
                    self.ctx.push_history(tool_msg, tokens);
                }
                self.turn += 1;

                if let Some(reason) = self.policy.should_terminate(
                    self.turn,
                    self.total_tokens,
                    self.last_now_ms,
                    self.started_at_ms,
                ) {
                    self.observations.push(LoopObservation::BudgetExceeded {
                        turn: self.turn,
                        budget: reason.to_string(),
                    });
                    let term = match reason {
                        "max_turns" => TerminationReason::MaxTurns,
                        "wall_time" => TerminationReason::Timeout,
                        _ => TerminationReason::TokenBudget,
                    };
                    // M1b: the pure scheduler must reach the identical terminate verdict + reason.
                    debug_assert!(
                        matches!(
                            super::tcb::schedule(&self.root_tcb(), self.last_now_ms),
                            ScheduleDecision::Terminate { reason: r, .. } if r == term
                        ),
                        "M1b schedule() disagrees with should_terminate (legacy reason {reason})"
                    );
                    self.pending_termination = Some(term);
                    self.phase = LoopPhase::Reason;
                    return self.emit_call_llm();
                }
                // M1b: conversely, within budget the pure scheduler must say `Run`.
                debug_assert!(
                    matches!(
                        super::tcb::schedule(&self.root_tcb(), self.last_now_ms),
                        ScheduleDecision::Run { .. }
                    ),
                    "M1b schedule() should Run when should_terminate returned None"
                );

                // ━━ Layer 3: 时间衰减检查（独立于rho）
                if let Some(now_ms) = self.last_now_ms {
                    if self.ctx.should_time_decay_compact(now_ms) {
                        // 强制MicroCompact，无论rho值
                        let (saved, summary, archived) = self.ctx.compress(PressureAction::MicroCompact);
                        self.push_compression_observations(PressureAction::MicroCompact, summary, archived);

                        // 记录压缩时间
                        self.ctx.last_compact_ms = Some(now_ms);
                    }
                }

                // ━━ 更新CollapseMode（Layer 4读时投影策略）
                self.ctx.update_collapse_mode();

                // ━━ 原有rho检查（Layer 2/4/5触发）
                let action = self.ctx.should_compress();
                self.phase = LoopPhase::Delta {
                    pressure: self.ctx.rho(),
                };
                if action != PressureAction::None {
                    let (saved, summary, archived) = self.ctx.compress_with_time(action, self.last_now_ms);
                    self.push_compression_observations(action, summary, archived);
                }

                // Renewal: when compression alone cannot recover enough headroom,
                // start a new sprint — carry forward system + memory + last N history turns.
                if self.ctx.should_renew() {
                    self.ctx.renew();
                    self.observations.push(LoopObservation::Renewed {
                        sprint: self.ctx.sprint,
                    });
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
    pub fn take_observations(&mut self) -> Vec<LoopObservation> {
        std::mem::take(&mut self.observations)
    }

    /// Spawn a sub-agent: registers a kernel process, emits `AgentProcessChanged`,
    /// and enters `Suspended(SubAgentAwait)` until the SDK feeds `SubAgentCompleted`.
    pub fn spawn_sub_agent(
        &mut self,
        spec: AgentRunSpec,
        parent_session_id: &str,
    ) -> LoopAction {
        let manifest = crate::types::agent::IsolationManifest::from_spec(
            &spec,
            parent_session_id,
            &self.ctx.capabilities,
        );
        // M2b: spawning is an effectful request — route it through the same syscall trap as tool
        // calls. No spawn policy stages exist yet, so this defaults to `Allow`; a `Deny` rolls the
        // turn back exactly like a denied tool call. Establishing the chokepoint now means quotas /
        // spawn rules can attach later without a new code path.
        if let Disposition::Deny { reason, .. } =
            self.evaluate_syscall(&Syscall::Spawn(manifest.clone()))
        {
            let rb = RollbackReason::GovernanceDenied {
                tool_name: format!("spawn:{}", manifest.agent_id),
                reason,
            };
            let note = Message::user(super::rollback::build_rollback_note(
                &rb,
                self.ctx.config.verbose_control_notes,
            ));
            self.rollback(rb);
            self.ctx
                .push_signal(note.content.as_text().unwrap_or_default().to_string());
            self.phase = LoopPhase::Reason;
            return self.emit_call_llm();
        }
        let agent_id = manifest.agent_id.to_string();
        let process = self.processes.register_spawn(&manifest);
        // M1c: mirror the spawn as a child task under the root.
        let mut child = Tcb::root(manifest.agent_id.clone(), self.policy.clone());
        child.parent = Some("root".into());
        child.state = TaskState::from(process.state);
        child.caps = process.permitted_capability_ids.clone();
        self.tasks.insert(child);
        self.push_agent_process_changed(process);
        self.debug_assert_tasks_mirror_processes();
        self.suspend_state = Some(SuspendState::SubAgentAwait {
            agent_ids: vec![agent_id.clone()],
        });
        self.set_lifecycle(
            TaskState::Suspended,
            Some(WaitReason::SubAgentJoin(manifest.agent_id.clone())),
        );
        self.observations.push(LoopObservation::Suspended {
            turn: self.turn,
            reason: "sub_agent_await".to_string(),
            pending_calls: vec![agent_id],
        });
        LoopAction::AwaitingResume
    }

    fn handle_sub_agent_completed(&mut self, result: SubAgentResult) -> LoopAction {
        if let Some(process) = self.processes.complete(result.clone()) {
            // M1c: mirror the join onto the child task's lifecycle.
            let mirrored = TaskState::from(process.state);
            if let Some(task) = self.tasks.get_mut(process.agent_id.as_str()) {
                task.state = mirrored;
            }
            self.push_agent_process_changed(process);
            self.debug_assert_tasks_mirror_processes();
        }
        let summary = result
            .result
            .final_message
            .as_ref()
            .and_then(|m| m.content.as_text())
            .unwrap_or_default();
        self.ctx
            .push_signal(format!("[sub-agent {}] {}", result.agent_id, summary));

        let agent_id = result.agent_id.to_string();
        // Suspended awaiting a sub-agent join (lifecycle on the root task, M1d).
        let awaiting_sub_agent =
            self.is_suspended() && matches!(self.wait_reason(), Some(WaitReason::SubAgentJoin(_)));
        let resume_parent = match self.suspend_state.as_mut() {
            Some(SuspendState::SubAgentAwait { agent_ids }) if awaiting_sub_agent => {
                agent_ids.retain(|id| id != &agent_id);
                if agent_ids.is_empty() {
                    self.suspend_state = None;
                    self.observations.push(LoopObservation::Resumed {
                        turn: self.turn,
                        approved: vec![agent_id],
                        denied: Vec::new(),
                    });
                    true
                } else {
                    false
                }
            }
            _ => true,
        };

        if resume_parent {
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        } else {
            LoopAction::AwaitingResume
        }
    }

    pub fn agent_process(&self, agent_id: &str) -> Option<&AgentProcess> {
        self.processes.get(agent_id)
    }

    pub fn agent_processes(&self) -> &[AgentProcess] {
        self.processes.all()
    }

    /// M1c: the canonical task registry (root task + one row per sub-agent). This is the
    /// schedulability/lineage source of truth; `agent_processes()` remains the rich record store
    /// until M1d makes it a derived view.
    pub fn task_table(&self) -> &TaskTable {
        &self.tasks
    }

    /// Debug-only invariant: the TaskTable carries the root plus exactly one task per process,
    /// with each child's lifecycle equal to its process state. Guards drift before M1d flips the
    /// canonical/derived relationship.
    fn debug_assert_tasks_mirror_processes(&self) {
        #[cfg(debug_assertions)]
        {
            debug_assert!(
                self.tasks.get("root").is_some(),
                "M1c: root task missing from TaskTable"
            );
            for p in self.processes.all() {
                match self.tasks.get(p.agent_id.as_str()) {
                    Some(task) => debug_assert_eq!(
                        task.state,
                        TaskState::from(p.state),
                        "M1c: TaskTable lifecycle drifted from ProcessTable for {}",
                        p.agent_id
                    ),
                    None => panic!("M1c: process {} missing from TaskTable", p.agent_id),
                }
            }
            debug_assert_eq!(
                self.tasks.all().len(),
                self.processes.all().len() + 1,
                "M1c: TaskTable should hold root + one task per process"
            );
        }
    }

    fn push_agent_process_changed(&mut self, process: AgentProcess) {
        self.observations.push(LoopObservation::AgentProcessChanged {
            turn: self.turn,
            agent_id: process.agent_id.to_string(),
            parent_session_id: process.parent_session_id.to_string(),
            role: process.role,
            isolation: process.isolation,
            context_inheritance: process.context_inheritance,
            state: process.state,
            permitted_capability_ids: process
                .permitted_capability_ids
                .iter()
                .map(|id| id.to_string())
                .collect(),
            result_termination: process.result_termination_label().map(str::to_string),
        });
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
        self.observations.push(LoopObservation::CheckpointTaken {
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

        LoopAction::CallLLM { context, tools }
    }

    pub fn rollback(&mut self, reason: RollbackReason) {
        self.ctx.partitions.history.messages.truncate(self.checkpoint.history_len);
        self.ctx.partitions.signals.truncate(self.checkpoint.signals_len);
        if let Some(ref state) = self.checkpoint.task_state {
            self.ctx.partitions.task_state = state.clone();
        }
        self.observations.push(LoopObservation::Rollbacked {
            turn: self.turn,
            checkpoint_history_len: self.checkpoint.history_len as u32,
            reason,
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


    /// Emit a `CapabilityChanged` observation for the current turn.
    /// Single construction site for all mount/unmount/replace/pin changes.
    #[allow(clippy::too_many_arguments)]
    fn push_capability_change(
        &mut self,
        added: Vec<String>,
        removed: Vec<String>,
        change_kind: &str,
        capability_id: Option<String>,
        version: Option<String>,
        mounted_by: Option<String>,
        mount_reason: Option<String>,
    ) {
        self.observations.push(LoopObservation::CapabilityChanged {
            turn: self.turn,
            added,
            removed,
            change_kind: Some(change_kind.to_string()),
            capability_id,
            version,
            mounted_by,
            mount_reason,
        });
    }

    pub fn execute_capability_command(&mut self, cmd: crate::types::capability::CapabilityCommand) {
        use crate::types::capability::CapabilityCommand;
        match cmd {
            CapabilityCommand::Mount {
                capability,
                mounted_by,
                mount_reason,
            } => {
                self.mount_capability(capability, mounted_by, mount_reason);
            }
            CapabilityCommand::Unmount { kind, id } => {
                self.unmount_capability(kind, &id);
            }
            CapabilityCommand::Replace {
                old_kind,
                old_id,
                new_capability,
            } => {
                let new_id = new_capability.id.to_string();
                let version = new_capability.version.clone();
                let old_kind_str = old_kind.label();
                let new_kind_str = new_capability.kind.label();

                self.ctx.capabilities.remove(old_kind, &old_id);
                self.ctx.capabilities.upsert(new_capability);

                self.push_capability_change(
                    vec![format!("{}:{}", new_kind_str, new_id)],
                    vec![format!("{}:{}", old_kind_str, old_id)],
                    "replace",
                    Some(new_id),
                    version,
                    None,
                    None,
                );
            }
            CapabilityCommand::Pin { kind, id } => {
                let version = self
                    .ctx
                    .capabilities
                    .get_mut(kind, &id)
                    .and_then(|c| c.version.clone());
                if let Some(cap) = self.ctx.capabilities.get_mut(kind, &id) {
                    cap.is_pinned = true;
                    self.push_capability_change(
                        vec![],
                        vec![],
                        "pin",
                        Some(id),
                        version,
                        None,
                        None,
                    );
                }
            }
        }
    }

    pub fn mount_capability(
        &mut self,
        mut descriptor: crate::types::capability::CapabilityDescriptor,
        mounted_by: Option<String>,
        mount_reason: Option<String>,
    ) {
        if mounted_by.is_some() {
            descriptor.mounted_by = mounted_by.clone();
        }
        if mount_reason.is_some() {
            descriptor.mount_reason = mount_reason.clone();
        }
        let id = descriptor.id.to_string();
        let kind_str = descriptor.kind.label();
        let version = descriptor.version.clone();
        self.ctx.capabilities.upsert(descriptor);
        self.push_capability_change(
            vec![format!("{}:{}", kind_str, id)],
            vec![],
            "mount",
            Some(id),
            version,
            mounted_by,
            mount_reason,
        );
    }

    pub fn unmount_capability(&mut self, kind: crate::types::capability::CapabilityKind, id: &str) {
        let version = self
            .ctx
            .capabilities
            .get_mut(kind, id)
            .and_then(|c| c.version.clone());
        self.ctx.capabilities.remove(kind, id);
        let kind_str = kind.label();
        self.push_capability_change(
            vec![],
            vec![format!("{}:{}", kind_str, id)],
            "unmount",
            Some(id.to_string()),
            version,
            None,
            None,
        );
    }

    // ─── Milestone contract ────────────────────────────────────────────────

    /// Load a milestone contract.  Must be called before `start()`.
    pub fn load_milestone_contract(&mut self, contract: MilestoneContract) {
        self.milestone.load_contract(contract);
    }

    /// Returns the ID of the current (not-yet-passed) phase, or `None` when
    /// no contract is loaded or all phases are complete.
    pub fn current_milestone_phase_id(&self) -> Option<&str> {
        self.milestone.current_phase_id()
    }

    /// Returns the acceptance criteria of the current phase as a slice.
    pub fn current_milestone_criteria(&self) -> &[String] {
        self.milestone.current_criteria()
    }

    /// Returns `true` when there is no contract or all phases have passed.
    pub fn is_milestone_complete(&self) -> bool {
        self.milestone.is_complete()
    }

    fn handle_milestone_result(&mut self, result: MilestoneCheckResult) -> LoopAction {
        self.observations.clear();

        if result.passed {
            // Advance phase: mount unlocked capabilities with milestone provenance.
            let mut unlocked: Vec<String> = Vec::new();
            if let Some(contract) = &self.milestone.contract.clone() {
                if let Some(phase) = contract.phases.get(self.milestone.current_phase) {
                    let mounted_by = Some(format!("milestone:{}", phase.id));
                    for cap in phase.unlocks.clone() {
                        let kind_str = cap.kind.label();
                        let id = cap.id.to_string();
                        unlocked.push(format!("{}:{}", kind_str, id));
                        self.mount_capability(
                            cap,
                            mounted_by.clone(),
                            Some("phase_advance".to_string()),
                        );
                    }
                    self.observations.push(LoopObservation::MilestoneAdvanced {
                        turn: self.turn,
                        phase_id: phase.id.clone(),
                        capabilities_unlocked: unlocked,
                    });
                }
            }
            self.milestone.current_phase += 1;
            self.milestone.blocked_count = 0;

            if self.is_milestone_complete() {
                return self.terminate(TerminationReason::Completed, None);
            }

            // Prompt the LLM with the next phase context.
            if let Some(criteria) = self
                .milestone
                .contract
                .as_ref()
                .and_then(|c| c.phases.get(self.milestone.current_phase))
                .map(|p| {
                    if p.criteria.is_empty() {
                        format!("[NEXT MILESTONE PHASE: {}]", p.id)
                    } else {
                        format!(
                            "[NEXT MILESTONE PHASE: {} — Criteria: {}]",
                            p.id,
                            p.criteria.join("; ")
                        )
                    }
                })
            {
                self.ctx.push_signal(criteria);
            }
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        } else {
            // Phase blocked — increment retry count.
            self.milestone.blocked_count += 1;
            let reason = result.reason.as_deref().unwrap_or("milestone criteria not met");

            // Retrieve the rollback_policy and retry budget for the current phase.
            let (rollback_policy, max_attempts) = self
                .milestone
                .contract
                .as_ref()
                .and_then(|c| c.phases.get(self.milestone.current_phase))
                .map(|p| {
                    let max = p
                        .retry_policy
                        .as_ref()
                        .map(|rp| rp.max_attempts)
                        .unwrap_or(0);
                    (p.rollback_policy.clone(), max)
                })
                .unwrap_or_default();

            // Check retry budget (0 = unlimited).
            let budget_exceeded = max_attempts > 0
                && self.milestone.blocked_count as u32 >= max_attempts;

            if budget_exceeded {
                use crate::types::milestone::MilestoneRollbackPolicy;
                match rollback_policy {
                    MilestoneRollbackPolicy::Terminate => {
                        self.observations.push(LoopObservation::MilestoneBlocked {
                            turn: self.turn,
                            phase_id: result.phase_id.clone(),
                            reason: format!("retry budget exhausted: {reason}"),
                        });
                        return self.terminate(TerminationReason::MilestoneExceeded, None);
                    }
                    MilestoneRollbackPolicy::Rollback => {
                        self.observations.push(LoopObservation::MilestoneBlocked {
                            turn: self.turn,
                            phase_id: result.phase_id.clone(),
                            reason: format!("retry budget exhausted (rollback): {reason}"),
                        });
                        let rb_reason = crate::runtime::session::RollbackReason::MalformedReplay {
                            reason: format!("milestone {} retry budget exhausted", result.phase_id),
                        };
                        self.rollback(rb_reason);
                        self.phase = LoopPhase::Reason;
                        return self.emit_call_llm();
                    }
                    MilestoneRollbackPolicy::Continue => {
                        // Fall through to normal blocked handling below.
                    }
                }
            }

            // Normal blocked: inject message and retry.
            self.ctx.push_signal(format!(
                "[MILESTONE BLOCKED: {} — {}. Address the criteria and try again.]",
                result.phase_id, reason
            ));
            self.observations.push(LoopObservation::MilestoneBlocked {
                turn: self.turn,
                phase_id: result.phase_id,
                reason: reason.to_string(),
            });
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        }
    }
}

#[cfg(test)]
#[path = "state_machine_tests.rs"]
mod tests;
