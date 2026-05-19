use serde::{Deserialize, Serialize};

use crate::types::message::{Message, ToolCall, ToolResult};

/// Append-only session event kinds (Runtime v1 — frozen schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
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
    },
    RunTerminal {
        reason: String,
        turns_used: u32,
        total_tokens: u64,
    },
}
