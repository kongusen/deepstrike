//! Scheduler observations projected by the canonical operation driver.
//!
//! These are engine facts, not a second host input protocol.

use serde::{Deserialize, Serialize};

use crate::context::pressure::PressureAction;
use crate::runtime::session::RollbackReason;

use super::wire::command::CancellationReason;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowSpawnFailure {
    pub agent_id: String,
    pub error: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelObservation {
    /// Synchronous in-kernel compaction fact. Archived content is carried only by
    /// `ArchivePageOut`; it never rides an observation into host I/O.
    Compressed {
        #[serde(default)]
        turn: u32,
        action: KernelPressureAction,
        rho_after: f64,
        summary: Option<String>,
        archived_count: u32,
        /// W1-1 cache-awareness: the message index at which this compression invalidated the
        /// prompt cache prefix (if any). `None` = prefix-safe. SDK/telemetry can use this to
        /// quantify "tokens saved vs cache rebuild cost". Additive ABI field with default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invalidates_prefix_at: Option<usize>,
    },
    Renewed {
        sprint: u32,
    },
    /// Rendering proved that fixed context or the protected transaction tail cannot fit inside the
    /// declared input budget. No provider effect is emitted for this turn.
    ContextBudgetExceeded {
        turn: u32,
        overflow_kind: crate::context::renderer::ContextBudgetOverflowKind,
        required_tokens: u32,
        max_tokens: u32,
    },
    /// K1: a boundary sweep of the knowledge partition applied deferred upserts and/or dropped
    /// marked entries. `removed_keys` lists keyed removals (unkeyed drops count only in
    /// `tokens_freed`); an upsert-only sweep has empty `removed_keys`.
    KnowledgeSwept {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed_keys: Vec<String>,
        tokens_freed: u32,
    },
    /// K2: the knowledge partition exceeds its configured budget share. Fired at most once per
    /// cache generation; the over-budget unpinned entries are already marked for the next
    /// boundary sweep. Pinned/skill weight that cannot be evicted keeps the warning standing.
    KnowledgeBudgetExceeded {
        turn: u32,
        used: u32,
        budget: u32,
    },
    Rollbacked {
        turn: u32,
        checkpoint_history_len: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<RollbackReason>,
    },
    /// A control-plane request was rejected before its effect started. Unlike `Rollbacked`, this is
    /// a committed result: there is no transaction to undo, and hosts can route the reason back to
    /// the caller without mistaking a missing success observation for an internal failure.
    ControlRequestRejected {
        turn: u32,
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        reason: String,
    },
    CapabilityChanged {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        added: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capability_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mounted_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mount_reason: Option<String>,
    },
    MilestoneAdvanced {
        turn: u32,
        phase_id: String,
        capabilities_unlocked: Vec<String>,
    },
    MilestoneBlocked {
        turn: u32,
        phase_id: String,
        reason: String,
    },
    /// Checkpoint taken at the start of a turn transaction (before LLM call).
    CheckpointTaken {
        turn: u32,
        history_len: u32,
    },
    /// O6: the repeat fuse tripped — the same turn signature (non-meta tool name AND args) was
    /// re-issued `count`x consecutively. `action` = `"deny"` (turn rolled back, directive note fed
    /// back) or `"terminate"` (run ends `no_progress` after one final report turn). Additive ABI.
    RepeatFuseTripped {
        turn: u32,
        signature: String,
        count: u32,
        action: String,
    },
    /// O4: the turn-end criteria gate fired — the model tried to finish while acceptance criteria
    /// stand; the kernel injected one self-check turn before accepting `Completed`. Additive ABI.
    CriteriaGateFired {
        turn: u32,
        criteria: Vec<String>,
    },
    /// Session-entropy sample at a completed turn boundary (the heartbeat watch source).
    /// One per completed turn, unconditional — like `CheckpointTaken`. The component
    /// vector is the contract; `score` is a versioned default fold (`score_version`).
    /// See `scheduler::entropy`. Additive ABI.
    EntropySample {
        turn: u32,
        score: f64,
        score_version: u32,
        rho: f64,
        repeat_pressure: f64,
        failure_rate: f64,
        rollbacks_in_window: u32,
        window_turns: u32,
    },
    /// The opt-in entropy watch tripped: `score` crossed `threshold` while armed and
    /// cooled down (`EntropyWatchConfig`). Correlate components via the same-turn
    /// `EntropySample`. Additive ABI.
    EntropyAlert {
        turn: u32,
        score: f64,
        threshold: f64,
    },
    /// Kernel process table changed for a spawned sub-agent.
    ///
    /// § Task 11 · lineage is the logical parent **task** id. The kernel does not know, and never
    /// echoes, which host session a child belongs to: mapping child task → child session is the
    /// host's (§5.2), and a host projection is free to keep restating its own session on the
    /// SessionLog event it derives from this observation.
    AgentProcessChanged {
        turn: u32,
        agent_id: String,
        parent_task_id: String,
        role: String,
        isolation: String,
        context_inheritance: String,
        state: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permitted_capability_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_termination: Option<String>,
    },
    /// One child attempt failed and the parent's local supervision policy adjudicated it.
    ChildSupervised {
        turn: u32,
        task_id: String,
        attempt: u32,
        strategy: String,
        reason: String,
        terminal: bool,
        relaunched: bool,
    },
    /// Deterministic local merge trace shared by DAG nodes and woken/nested task processes.
    LocalRunnableTrace {
        turn: u32,
        runnable: Vec<crate::scheduler::runnable::LocalRunnable>,
    },
    /// W0-ABI: a workflow batch was spawned — each node's spawn descriptor (agent id + goal +
    /// role/isolation/inheritance) so the SDK can run the kernel-generated nodes.
    WorkflowBatchSpawned {
        turn: u32,
        nodes: Vec<crate::orchestration::workflow::WorkflowSpawnInfo>,
        /// G4 budget-as-signal: the workflow's remaining headroom under the active quota at spawn
        /// time, so a coordinator node can scale its next submission. Additive: omitted when no
        /// resource quota is installed (nothing to report).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<crate::orchestration::workflow::WorkflowBudget>,
    },
    /// The host could not resolve a workflow spawn effect. No node is recorded
    /// as started; the same logical batch remains pending for retry.
    WorkflowSpawnFailed {
        turn: u32,
        error: String,
    },
    /// W0-ABI: a workflow finished (all nodes terminal, or stalled by a gated dependency).
    WorkflowCompleted {
        turn: u32,
        node_outcomes: Vec<crate::orchestration::workflow::run::WorkflowNodeOutcome>,
    },
    /// #2-B: a high-urgency `InterruptNow` signal preempted in-flight work. The kernel has already
    /// marked these agents `Done(UserAbort)` and reclaimed the root to reason about the interrupt; the
    /// SDK must ABORT the listed in-flight child runs and discard their results (do NOT feed their
    /// `SubAgentCompleted`). Additive variant (`agent_preempted`) — byte-identical for SDKs that never
    /// receive it.
    AgentPreempted {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agent_ids: Vec<String>,
        reason: String,
    },
    AgentPreemptFailed {
        turn: u32,
        agent_ids: Vec<String>,
        reason: String,
        error: String,
    },
    /// ③ loop-agent pacing: the kernel adjudicated a `pace` proposal for this round.
    RoundPaced {
        turn: u32,
        round: u32,
        decision: crate::types::result::PaceDecision,
    },
    /// R3-1: a runtime node submission was appended to the in-flight DAG at `base`
    /// (the graph length before the append). The SDK records `base` on the
    /// `workflow_nodes_submitted` session event so resume can re-apply the batch at
    /// the exact original indices (gap-filling any interleaved runtime children).
    WorkflowNodesSubmitted {
        turn: u32,
        base: u32,
        count: u32,
        /// W-N3: the submitting node's agent id (`None` = host/bootstrap). Persisted so resume can
        /// DROP batches whose submitter re-runs (it will re-submit) instead of duplicating them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submitter: Option<String>,
    },
    /// A runtime node batch was rejected before any graph mutation.
    NodesRejected {
        turn: u32,
        node_index: u32,
        reason: String,
    },
    /// A tool call needs user approval (governance `AskUser`). Not blocked by the
    /// kernel — the SDK must obtain approval before executing the named call.
    ToolGated {
        turn: u32,
        call_id: String,
        tool: String,
        reason: String,
    },
    /// A leased inbound signal delivery was routed by the in-kernel attention policy.
    SignalDeliveryDisposed {
        turn: u32,
        operation_id: String,
        delivery_id: String,
        attempt: u32,
        signal_id: String,
        disposition: String,
        queue_depth: u32,
    },
    SignalDisplaced {
        turn: u32,
        admitted_signal_id: String,
        displaced_signal_id: String,
        queue_depth: u32,
    },
    SignalExpired {
        turn: u32,
        signal_id: String,
        queue_depth: u32,
    },
    SignalsPending {
        turn: u32,
        depth: u32,
    },
    /// A budget axis (turns / tokens / wall-time) was exhausted.
    BudgetExceeded {
        turn: u32,
        budget: String,
        operation_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_id: Option<String>,
    },
    /// Terminal local usage for one reservation. Emitted exactly once per operation.
    BudgetUsageReported {
        operation_id: String,
        reservation_id: String,
        tokens: u64,
        subagents: u32,
        rounds: u32,
    },
    /// §13.2 / DEC-6 · a revision-guarded live policy patch was applied.
    ///
    /// A fact, not a command: the new policy is already installed when this is emitted, and the
    /// host is asked for nothing. `revision` is the counter *after* the patch — the value the next
    /// writer must present as its `expected_revision`, which is what makes a refused patch
    /// rebaseable instead of a silent overwrite.
    LivePolicyChanged {
        turn: u32,
        /// Which of the four §13.2 policies changed (`signal` / `governance` / `resource_quota` /
        /// `recovery`).
        policy: String,
        revision: u64,
    },
    /// A host cancellation was committed. Emitted exactly once by the accepted cancellation step.
    OperationCancelled {
        turn: u32,
        operation_id: String,
        reason: CancellationReason,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pending_call_ids: Vec<String>,
    },
    /// Loop entered `Suspended` state (awaiting human approval or sub-agent).
    Suspended {
        turn: u32,
        reason: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pending_calls: Vec<String>,
    },
    /// Loop resumed from `Suspended` state.
    Resumed {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        approved: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        denied: Vec<String>,
    },
    ApprovalResolutionFailed {
        turn: u32,
        error: String,
    },
    /// Memory entry written successfully (Phase 7).
    MemoryWritten {
        turn: u32,
        record_id: String,
        scope: crate::mm::memory::MemoryScope,
        memory_kind: crate::mm::memory::MemoryKind,
        name: String,
        size_bytes: u32,
    },
    /// Memory validation failed (Phase 7).
    MemoryValidationFailed {
        turn: u32,
        record_id: String,
        error: String,
    },
    MemoryWriteFailed {
        turn: u32,
        record_id: String,
        error: String,
    },
    /// Memory query request (Phase 7).
    MemoryQueried {
        turn: u32,
        scope: crate::mm::memory::MemoryScope,
        query: String,
        requested_k: usize,
        requires_async_response: bool,
    },
    MemoryQueryFailed {
        turn: u32,
        scope: crate::mm::memory::MemoryScope,
        query: String,
        error: String,
    },
    /// M3: recall lifecycle was journaled for one or more recalled records. Derived from the routed
    /// hits (each carries its current count); the host mirrors the incremented counts into its
    /// durable store so recall history survives across sessions.
    MemoryRecalled {
        turn: u32,
        scope: crate::mm::memory::MemoryScope,
        recalls: Vec<crate::mm::memory::MemoryRecallLifecycle>,
    },
    /// M4: a recalled record crossed the promotion threshold. Advisory only — the host/model decides
    /// whether to pin it or promote its content into knowledge.
    PromotionSuggested {
        turn: u32,
        record_id: String,
        recall_count: u64,
    },
    PageOutArchived {
        turn: u32,
        action: KernelPressureAction,
        summary: Option<String>,
        tier: String,
        message_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive_ref: Option<String>,
    },
    PageOutArchiveFailed {
        turn: u32,
        action: KernelPressureAction,
        tier: String,
        message_count: u32,
        error: String,
    },
    /// §7.10 / §25.9 · a P3 handle's payload residency moved.
    ///
    /// The handle table is the kernel's only fact about where a body lives, so every transfer is a
    /// committed fact — including the first one, where an external result was never resident at
    /// all (`from` is then absent, because the kernel minted the handle for this very transfer).
    /// `payload_ref` is the opaque host locator while the body is outside core, and absent once it
    /// has come home.
    PayloadResidencyChanged {
        turn: u32,
        handle_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload_ref: Option<String>,
        original_size: u64,
    },
    /// §7.10 · a page-in the host could not satisfy. DEC-5: one decision, taken once — the read is
    /// abandoned and the loop continues, because a body the host cannot produce is a degraded read,
    /// not an unsound operation.
    PayloadLoadFailed {
        turn: u32,
        handle_id: String,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelPressureAction {
    None,
    SnipCompact,
    MicroCompact,
    ContextCollapse,
    AutoCompact,
}

impl From<PressureAction> for KernelPressureAction {
    fn from(action: PressureAction) -> Self {
        match action {
            PressureAction::None => Self::None,
            PressureAction::SnipCompact => Self::SnipCompact,
            PressureAction::MicroCompact => Self::MicroCompact,
            PressureAction::ContextCollapse => Self::ContextCollapse,
            PressureAction::AutoCompact => Self::AutoCompact,
        }
    }
}
