use serde::{Deserialize, Serialize};

use crate::runtime::kernel::wire::CancellationReason;
use crate::types::message::{CoreMessage, ToolCall, ToolResult};

#[path = "session_wire.rs"]
pub mod wire;

#[cfg(test)]
#[path = "session_registry_tests.rs"]
mod session_registry_tests;

/// Provider-native replay payload persisted in `llm_completed` for wake/preload recovery.
///
/// The core is provider-neutral but the envelope is strict: protocol identity is mandatory and
/// removed schema/version fields are rejected before replay can be consumed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderReplay {
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_blocks: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_message: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
}

/// The provider wire evidence bundle carried by `llm_completed` (shared session-event registry,
/// `contracts/session-events.json`). Provider-native replay state rides inside it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WireEvidence {
    pub protocol: String,
    /// The per-turn `ProviderRequestPlan` fingerprint. Always written by the runner; absent only
    /// on events upgraded from a pre-registry Rust log, which never recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_state: Option<ProviderReplay>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RollbackReason {
    FatalToolError { tool_name: String, error: String },
    GovernanceDenied { tool_name: String, reason: String },
    ProviderFailure { error: String },
    Timeout,
    UserInterrupt,
    MalformedReplay { reason: String },
}

/// Append-only session event kinds for the unified Agent OS Runtime.
///
/// Combines execution loop events with OS-level lifecycle control,
/// capability manifest auditing, and governance gates.
///
/// The serialized form is the cross-SDK wire shape registered in `contracts/session-events.json`
/// (kinds, required/optional top-level fields, one sample each); `session_registry_tests` pins it.
/// Optional fields are omitted when absent, never written as `null`. Logs written before the
/// registry are read through [`SessionEvent::from_wire_value`], which upgrades the old Rust shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    // ─── 1. Execution & Inference Loop ───
    RunStarted {
        run_id: String,
        goal: String,
        #[serde(default)]
        criteria: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        /// Multimodal parts seeded into history before the first render (Node/Python/WASM/Rust).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::types::message::ContentPart>,
        /// The resolved provider route (host-shaped; the Rust runner does not record one yet).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route: Option<serde_json::Value>,
        /// Host run metadata, carried verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
    },
    LlmCompleted {
        turn: u32,
        /// On the wire: `content` (text) + `tool_calls` (arguments JSON-encoded), see
        /// [`wire::llm_message`]. The in-memory assistant message is unchanged.
        #[serde(flatten, with = "wire::llm_message")]
        message: CoreMessage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_count: Option<u64>,
        /// The provider effect this completion settles.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invocation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wire_evidence: Option<WireEvidence>,
    },
    ToolRequested {
        turn: u32,
        #[serde(with = "wire::tool_calls")]
        calls: Vec<ToolCall>,
    },
    ToolCompleted {
        turn: u32,
        #[serde(with = "wire::tool_results")]
        results: Vec<ToolResult>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_id: Option<String>,
    },
    Compressed {
        turn: u32,
        archived_seq_range: (u64, u64),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preserved_refs: Vec<String>,
    },
    /// Working memory paged out for long-term storage (kernel `page_out`).
    PageOut {
        turn: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier_hint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive_ref: Option<String>,
    },
    /// Long-term entries injected into knowledge partition (SDK `page_in`).
    PageIn {
        turn: u32,
        entry_count: u32,
    },
    RunTerminal {
        reason: String,
        turns_used: u32,
        total_tokens: u64,
    },
    /// Batch metadata for resume recovery; node specs stay in the kernel observation.
    WorkflowBatchSpawned {
        turn: u32,
        node_count: u32,
        node_ids: Vec<String>,
    },
    WorkflowCompleted {
        turn: u32,
        node_outcomes: Vec<crate::orchestration::workflow::run::WorkflowNodeOutcome>,
        total_nodes: u32,
    },
    /// A kernel observation with no dedicated session kind, kept durable verbatim for audit.
    KernelObservation {
        turn: u32,
        observation_kind: String,
        raw: serde_json::Value,
    },

    // ─── 2. Kernel Governance & Security Gates ───
    /// Tool arguments automatically repaired under white-listed heuristics.
    ToolArgumentRepaired {
        turn: u32,
        tool: String,
        original_arguments: String,
        repaired_arguments: String,
    },
    /// Escalated permission gate requested for a tool, suspending current execution.
    PermissionRequested {
        turn: u32,
        tool: String,
        arguments: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Permission decision resolved by the user or an automated policy engine.
    PermissionResolved {
        turn: u32,
        approved: bool,
        responder: String, // "user" | "policy_gate"
    },
    /// Tool blocked monotonically by security governance policy or denial of consent.
    ToolDenied {
        turn: u32,
        call_id: String,
        tool_name: String,
        reason: String,
    },

    // ─── 3. Dynamic Capability & Context Restructuring ───
    /// Model-visible capabilities dynamically updated (e.g., loading skills or mounting MCPs).
    CapabilityChanged {
        turn: u32,
        #[serde(default)]
        added: Vec<String>,
        #[serde(default)]
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
    /// Context reset and sprint rotated after a context boundary handoff.
    ContextRenewed {
        turn: u32,
        sprint: u32,
        handoff_ref: String,
    },
    /// End-of-run context lifecycle metrics: knowledge regret, never-referenced knowledge
    /// tokens, and history tokens whose prompt cache a mid-run system[1] promotion discarded.
    ContextLifecycleReported {
        turn: u32,
        knowledge_entries: u32,
        knowledge_referenced: u32,
        regret: u32,
        waste_tokens: u64,
        cache_loss_tokens: u64,
        /// Memory records the kernel recalled, and how many the model read with `memory(record_id)`.
        #[serde(default)]
        recalled: u32,
        #[serde(default)]
        recall_expanded: u32,
        /// Every knowledge item of the run: origin, use, and why it left (absent while resident).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        items: Vec<crate::context::item::ItemRecord>,
        /// History items by origin: admitted, still in history at the end, read back.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        history: Vec<crate::context::item::OriginTally>,
        /// The latest history items one by one (capped); older ones are only counted.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        history_items: Vec<crate::context::item::HistoryItemRecord>,
        #[serde(default, skip_serializing_if = "crate::context::item::is_zero_u32")]
        history_items_omitted: u32,
        /// The item graph: resident edges, then the latest edges touching a retired item.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        edges: Vec<crate::context::item::ItemEdge>,
        #[serde(default, skip_serializing_if = "crate::context::item::is_zero_u32")]
        edges_omitted: u32,
        /// Knowledge items nothing referenced or read back.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waste_items: Vec<crate::context::item::ItemId>,
        /// Plan task 7.4 · edges to items of other runs, and the parent items this run used.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        external_edges: Vec<crate::context::item::ExternalEdge>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parent_items_used: Vec<crate::context::item::ExternalItem>,
    },

    /// Execution paused (waiting for human-in-the-loop interaction or long-running tasks).
    Suspended {
        turn: u32,
        reason: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pending_calls: Vec<String>,
    },
    /// Execution resumed.
    Resumed {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        approved: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        denied: Vec<String>,
    },
    /// Kernel governance gate: tool requires approval before execution.
    ToolGated {
        turn: u32,
        call_id: String,
        tool: String,
        reason: String,
    },
    /// In-kernel signal disposition (attention policy).
    SignalDeliveryDisposed {
        turn: u32,
        operation_id: String,
        delivery_id: String,
        attempt: u32,
        signal_id: String,
        disposition: String,
        queue_depth: u32,
    },
    /// Scheduler budget axis exhausted.
    BudgetExceeded {
        turn: u32,
        operation_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_id: Option<String>,
        budget: String,
    },
    /// Terminal local usage for a reservation-backed RunGroup budget grant.
    BudgetUsageReported {
        turn: u32,
        operation_id: String,
        reservation_id: String,
        tokens: u64,
        subagents: u32,
        rounds: u32,
    },
    /// Host-owned external I/O was stopped and the correlated operation cancelled.
    OperationCancelled {
        turn: u32,
        operation_id: String,
        reason: CancellationReason,
        #[serde(default)]
        pending_call_ids: Vec<String>,
    },
    /// Checkpoint taken at the start of a turn transaction (before LLM call).
    CheckpointTaken {
        turn: u32,
        history_len: u32,
    },
    /// Session-entropy sample at a completed turn boundary (see `scheduler::entropy`).
    EntropySample {
        turn: u32,
        score: f64,
        rho: f64,
        repeat_pressure: f64,
        failure_rate: f64,
        rollbacks_in_window: u32,
        window_turns: u32,
    },
    /// The opt-in entropy watch tripped (score crossed the configured threshold).
    EntropyAlert {
        turn: u32,
        score: f64,
        threshold: f64,
    },
    /// Transaction rollback indicating state was restored to a checkpoint.
    Rollbacked {
        turn: u32,
        checkpoint_history_len: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<RollbackReason>,
    },

    // ─── 4. Process Table ───
    /// Kernel process table changed for a spawned sub-agent.
    AgentProcessChanged {
        turn: u32,
        agent_id: String,
        /// The kernel task that spawned the process.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_task_id: Option<String>,
        /// Host audit identity; canonical kernel observations never populate it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<String>,
        role: String,
        isolation: String,
        context_inheritance: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        #[serde(default)]
        permitted_capability_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_termination: Option<String>,
    },

    // ─── 5. Milestone Contracts ───
    /// Milestone phase criteria passed — capabilities unlocked, phase advanced.
    MilestoneAdvanced {
        turn: u32,
        phase_id: String,
        #[serde(default)]
        capabilities_unlocked: Vec<String>,
    },
    /// Milestone phase criteria not met — run continues without advancing the phase.
    MilestoneBlocked {
        turn: u32,
        phase_id: String,
        reason: String,
    },

    // ─── 6. Long-Term Memory (Phase 7) ───
    /// Memory entry written successfully (SDK → kernel acknowledgment).
    MemoryWritten {
        turn: u32,
        record_id: String,
        scope: crate::mm::memory::MemoryScope,
        memory_kind: crate::mm::memory::MemoryKind,
        name: String,
        size_bytes: u32,
    },
    /// Memory query request (kernel → SDK; SDK should respond asynchronously).
    MemoryQueried {
        turn: u32,
        scope: crate::mm::memory::MemoryScope,
        query: String,
        requested_k: usize,
        requires_async_response: bool,
    },
    /// Memory validation failed (kernel rejected a write request).
    MemoryValidationFailed {
        turn: u32,
        record_id: String,
        error: String,
    },
    /// A memory store write failed after the kernel accepted it.
    MemoryWriteFailed {
        turn: u32,
        record_id: String,
        error: String,
    },
    /// A memory store query failed.
    MemoryQueryFailed {
        turn: u32,
        scope: crate::mm::memory::MemoryScope,
        query: String,
        error: String,
    },
    /// A page-out archive was handed to the semantic (long-term memory) archive path.
    SemanticArchivePending {
        effect_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
    },
    /// The semantic archive wrote its record (not logged when it went to a candidate sink).
    SemanticArchiveCompleted {
        effect_id: String,
        record_id: String,
    },
    SemanticArchiveFailed {
        effect_id: String,
        error: String,
    },
    /// Memory retrieval result (SDK → kernel via Resume or other async mechanism).
    MemoryRetrievalResult {
        hits: Vec<crate::mm::memory::MemoryRecall>,
    },
    /// M2 · a write handed to the host's candidate sink and held for review. Not a remembered
    /// record: only an accepted candidate is logged as `memory_written`.
    MemoryCandidatePending {
        turn: u32,
        candidate_id: String,
        /// Which write path produced the candidate (`session_extraction`, `semantic_archive`,
        /// `child_request`).
        source: String,
        name: String,
        /// Absent when projected from the kernel observation, which does not carry it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_id: Option<String>,
    },
    /// M2 · the candidate sink rejected a write.
    MemoryCandidateRejected {
        turn: u32,
        candidate_id: String,
        source: String,
        name: String,
        receipt_id: String,
        reason_code: String,
    },
}

impl SessionEvent {
    /// Kinds older logs may still hold that no longer have a variant. Readers skip them rather
    /// than fail the whole session.
    pub const RETIRED_KINDS: &'static [&'static str] = &["context_prepared"];

    /// Every kind this SDK carries, in declaration order. `session_registry_tests` checks it
    /// against both serde's variant list and the Rust slice of `contracts/session-events.json`.
    pub const KINDS: &'static [&'static str] = &[
        "run_started",
        "llm_completed",
        "tool_requested",
        "tool_completed",
        "compressed",
        "page_out",
        "page_in",
        "run_terminal",
        "workflow_batch_spawned",
        "workflow_completed",
        "kernel_observation",
        "tool_argument_repaired",
        "permission_requested",
        "permission_resolved",
        "tool_denied",
        "capability_changed",
        "context_renewed",
        "context_lifecycle_reported",
        "suspended",
        "resumed",
        "tool_gated",
        "signal_delivery_disposed",
        "budget_exceeded",
        "budget_usage_reported",
        "operation_cancelled",
        "checkpoint_taken",
        "entropy_sample",
        "entropy_alert",
        "rollbacked",
        "agent_process_changed",
        "milestone_advanced",
        "milestone_blocked",
        "memory_written",
        "memory_queried",
        "memory_validation_failed",
        "memory_write_failed",
        "memory_query_failed",
        "semantic_archive_pending",
        "semantic_archive_completed",
        "semantic_archive_failed",
        "memory_retrieval_result",
        "memory_candidate_pending",
        "memory_candidate_rejected",
    ];

    /// Decode one persisted event, upgrading the pre-registry Rust shapes first (see
    /// [`wire::upgrade_legacy`]). Every session-log reader should come through here.
    pub fn from_wire_value(mut value: serde_json::Value) -> Result<Self, serde_json::Error> {
        wire::upgrade_legacy(&mut value);
        serde_json::from_value(value)
    }

    /// An `llm_completed` carrying only the assistant message and its replay state, for logs and
    /// fixtures with no provider request to fingerprint.
    pub fn llm_completed(
        turn: u32,
        message: CoreMessage,
        provider_replay: Option<ProviderReplay>,
    ) -> Self {
        Self::LlmCompleted {
            turn,
            message,
            token_count: None,
            effect_id: None,
            invocation_id: None,
            wire_evidence: provider_replay.map(|replay| WireEvidence {
                protocol: replay.protocol.clone(),
                request_fingerprint: None,
                response_id: None,
                raw_usage: None,
                replay_state: Some(replay),
            }),
        }
    }

    /// The provider-native replay state recorded with an `llm_completed`, if any.
    pub fn provider_replay(&self) -> Option<&ProviderReplay> {
        match self {
            Self::LlmCompleted { wire_evidence, .. } => wire_evidence
                .as_ref()
                .and_then(|evidence| evidence.replay_state.as_ref()),
            _ => None,
        }
    }

    /// Event `kind` string (snake_case tag).
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "run_started",
            Self::LlmCompleted { .. } => "llm_completed",
            Self::ToolRequested { .. } => "tool_requested",
            Self::ToolCompleted { .. } => "tool_completed",
            Self::Compressed { .. } => "compressed",
            Self::PageOut { .. } => "page_out",
            Self::PageIn { .. } => "page_in",
            Self::RunTerminal { .. } => "run_terminal",
            Self::WorkflowBatchSpawned { .. } => "workflow_batch_spawned",
            Self::WorkflowCompleted { .. } => "workflow_completed",
            Self::KernelObservation { .. } => "kernel_observation",
            Self::ToolArgumentRepaired { .. } => "tool_argument_repaired",
            Self::PermissionRequested { .. } => "permission_requested",
            Self::PermissionResolved { .. } => "permission_resolved",
            Self::ToolDenied { .. } => "tool_denied",
            Self::CapabilityChanged { .. } => "capability_changed",
            Self::ContextRenewed { .. } => "context_renewed",
            Self::ContextLifecycleReported { .. } => "context_lifecycle_reported",
            Self::Suspended { .. } => "suspended",
            Self::Resumed { .. } => "resumed",
            Self::ToolGated { .. } => "tool_gated",
            Self::SignalDeliveryDisposed { .. } => "signal_delivery_disposed",
            Self::BudgetExceeded { .. } => "budget_exceeded",
            Self::BudgetUsageReported { .. } => "budget_usage_reported",
            Self::OperationCancelled { .. } => "operation_cancelled",
            Self::CheckpointTaken { .. } => "checkpoint_taken",
            Self::EntropySample { .. } => "entropy_sample",
            Self::EntropyAlert { .. } => "entropy_alert",
            Self::Rollbacked { .. } => "rollbacked",
            Self::AgentProcessChanged { .. } => "agent_process_changed",
            Self::MilestoneAdvanced { .. } => "milestone_advanced",
            Self::MilestoneBlocked { .. } => "milestone_blocked",
            Self::MemoryWritten { .. } => "memory_written",
            Self::MemoryQueried { .. } => "memory_queried",
            Self::MemoryValidationFailed { .. } => "memory_validation_failed",
            Self::MemoryWriteFailed { .. } => "memory_write_failed",
            Self::MemoryQueryFailed { .. } => "memory_query_failed",
            Self::SemanticArchivePending { .. } => "semantic_archive_pending",
            Self::SemanticArchiveCompleted { .. } => "semantic_archive_completed",
            Self::SemanticArchiveFailed { .. } => "semantic_archive_failed",
            Self::MemoryRetrievalResult { .. } => "memory_retrieval_result",
            Self::MemoryCandidatePending { .. } => "memory_candidate_pending",
            Self::MemoryCandidateRejected { .. } => "memory_candidate_rejected",
        }
    }

    /// Whether this event is a kernel OS decision (replay ignores for message reconstruction).
    pub fn is_kernel_os_event(&self) -> bool {
        matches!(
            self,
            Self::Compressed { .. }
                | Self::PageOut { .. }
                | Self::PageIn { .. }
                | Self::CapabilityChanged { .. }
                | Self::ContextRenewed { .. }
                | Self::ContextLifecycleReported { .. }
                | Self::Suspended { .. }
                | Self::Resumed { .. }
                | Self::ToolGated { .. }
                | Self::SignalDeliveryDisposed { .. }
                | Self::BudgetExceeded { .. }
                | Self::BudgetUsageReported { .. }
                | Self::OperationCancelled { .. }
                | Self::CheckpointTaken { .. }
                | Self::EntropySample { .. }
                | Self::EntropyAlert { .. }
                | Self::Rollbacked { .. }
                | Self::WorkflowBatchSpawned { .. }
                | Self::WorkflowCompleted { .. }
                | Self::KernelObservation { .. }
                | Self::AgentProcessChanged { .. }
                | Self::MilestoneAdvanced { .. }
                | Self::MilestoneBlocked { .. }
                | Self::MemoryWritten { .. }
                | Self::MemoryQueried { .. }
                | Self::MemoryValidationFailed { .. }
                | Self::MemoryWriteFailed { .. }
                | Self::MemoryQueryFailed { .. }
                | Self::SemanticArchivePending { .. }
                | Self::SemanticArchiveCompleted { .. }
                | Self::SemanticArchiveFailed { .. }
                | Self::MemoryCandidatePending { .. }
                | Self::MemoryCandidateRejected { .. }
        )
    }
}
