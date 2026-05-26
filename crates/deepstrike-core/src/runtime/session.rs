use serde::{Deserialize, Serialize};

use crate::types::message::{Message, ToolCall, ToolResult};

/// Provider-native replay payload persisted in `llm_completed` for wake/preload recovery.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderReplay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_blocks: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
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
        added: Vec<String>,
        removed: Vec<String>,
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
    },
    /// Execution resumed.
    Resumed {
        turn: u32,
    },
    /// Transaction rollback indicating state was restored to a checkpoint.
    Rollbacked {
        turn: u32,
        checkpoint_history_len: u32,
    },
    /// Host-level resources (temporary workspace trees, MCP child processes) garbage-collected.
    CleanupCompleted {
        run_id: String,
        freed_resources: Vec<String>,
    },

    // ─── 4. Milestone Contracts ───
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
}
