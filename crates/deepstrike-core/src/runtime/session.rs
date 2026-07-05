use serde::{Deserialize, Serialize};


use crate::types::message::{Message, ToolCall, ToolResult};

/// Provider-native replay payload persisted in `llm_completed` for wake/preload recovery.
///
/// The core is provider-neutral: it persists and round-trips the replay envelope
/// verbatim without interpreting protocol-specific shapes. `native_blocks` and
/// `reasoning_content` are modeled explicitly because the recovery path reads
/// them; every other envelope field (`schema_version`, `provider`, `protocol`,
/// `model`, `reasoning_details`, `native_message`, `tool_calls`, …) is preserved
/// through `extra` so SDK-owned protocol metadata is never dropped.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderReplay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_blocks: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    // ─── 1. Execution & Inference Loop ───
    RunStarted {
        run_id: String,
        goal: String,
        #[serde(default)]
        criteria: Vec<String>,
        agent_id: Option<String>,
        system_prompt: Option<String>,
    },
    LlmCompleted {
        turn: u32,
        message: Message,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_replay: Option<ProviderReplay>,
    },
    ToolRequested {
        turn: u32,
        calls: Vec<ToolCall>,
    },
    ToolCompleted {
        turn: u32,
        results: Vec<ToolResult>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive_ref: Option<String>,
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
        #[serde(default)]
        message_count: u32,
    },
    /// Long-term entries injected into knowledge partition (SDK `page_in`).
    PageIn {
        turn: u32,
        entry_count: u32,
    },
    /// Large tool result spooled to disk by the SDK (kernel Layer 1).
    LargeResultSpooled {
        turn: u32,
        call_id: String,
        tool: String,
        original_size: u32,
        preview_size: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spool_ref: Option<String>,
    },
    RunTerminal {
        reason: String,
        turns_used: u32,
        total_tokens: u64,
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
    /// Context reset and sprint rotated after a context boundary handoff.
    ContextRenewed {
        turn: u32,
        sprint: u32,
        handoff_ref: String,
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
    SignalDisposed {
        turn: u32,
        signal_id: String,
        disposition: String,
        queue_depth: u32,
    },
    /// Scheduler budget axis exhausted.
    BudgetExceeded {
        turn: u32,
        budget: String,
    },
    /// Checkpoint taken at the start of a turn transaction (before LLM call).
    CheckpointTaken {
        turn: u32,
        history_len: u32,
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
        parent_session_id: String,
        role: String,
        isolation: String,
        context_inheritance: String,
        state: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
        memory_id: String,
        memory_kind: String,
        size_bytes: u32,
    },
    /// Memory query request (kernel → SDK; SDK should respond asynchronously).
    MemoryQueried {
        turn: u32,
        query_context: String,
        requested_k: usize,
        requires_async_response: bool,
    },
    /// Memory validation failed (kernel rejected a write request).
    MemoryValidationFailed {
        turn: u32,
        memory_id: String,
        error: String,
    },
    /// Memory retrieval result (SDK → kernel via Resume or other async mechanism).
    MemoryRetrievalResult {
        retrieval: crate::mm::memory::MemoryRetrieval,
    },
}

impl SessionEvent {
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
            Self::LargeResultSpooled { .. } => "large_result_spooled",
            Self::RunTerminal { .. } => "run_terminal",
            Self::ToolArgumentRepaired { .. } => "tool_argument_repaired",
            Self::PermissionRequested { .. } => "permission_requested",
            Self::PermissionResolved { .. } => "permission_resolved",
            Self::ToolDenied { .. } => "tool_denied",
            Self::CapabilityChanged { .. } => "capability_changed",
            Self::ContextRenewed { .. } => "context_renewed",
            Self::Suspended { .. } => "suspended",
            Self::Resumed { .. } => "resumed",
            Self::ToolGated { .. } => "tool_gated",
            Self::SignalDisposed { .. } => "signal_disposed",
            Self::BudgetExceeded { .. } => "budget_exceeded",
            Self::CheckpointTaken { .. } => "checkpoint_taken",
            Self::Rollbacked { .. } => "rollbacked",
            Self::AgentProcessChanged { .. } => "agent_process_changed",
            Self::MilestoneAdvanced { .. } => "milestone_advanced",
            Self::MilestoneBlocked { .. } => "milestone_blocked",
            Self::MemoryWritten { .. } => "memory_written",
            Self::MemoryQueried { .. } => "memory_queried",
            Self::MemoryValidationFailed { .. } => "memory_validation_failed",
            Self::MemoryRetrievalResult { .. } => "memory_retrieval_result",
        }
    }

    /// Whether this event is a kernel OS decision (replay ignores for message reconstruction).
    pub fn is_kernel_os_event(&self) -> bool {
        matches!(
            self,
            Self::Compressed { .. }
                | Self::PageOut { .. }
                | Self::PageIn { .. }
                | Self::LargeResultSpooled { .. }
                | Self::CapabilityChanged { .. }
                | Self::ContextRenewed { .. }
                | Self::Suspended { .. }
                | Self::Resumed { .. }
                | Self::ToolGated { .. }
                | Self::SignalDisposed { .. }
                | Self::BudgetExceeded { .. }
                | Self::CheckpointTaken { .. }
                | Self::Rollbacked { .. }
                | Self::AgentProcessChanged { .. }
                | Self::MilestoneAdvanced { .. }
                | Self::MilestoneBlocked { .. }
                | Self::MemoryWritten { .. }
                | Self::MemoryQueried { .. }
                | Self::MemoryValidationFailed { .. }
        )
    }
}
