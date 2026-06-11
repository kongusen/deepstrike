use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use super::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    Completed,
    MaxTurns,
    TokenBudget,
    Timeout,
    UserAbort,
    Error,
    /// Milestone phase retry budget exhausted and rollback_policy = Terminate.
    MilestoneExceeded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopResult {
    pub termination: TerminationReason,
    pub final_message: Option<Message>,
    pub turns_used: u32,
    pub total_tokens_used: u64,
    /// Loop-node "until done" signal (A#2 v2): when an iteration of a `NodeKind::Loop` workflow
    /// node reports `Some(false)`, the kernel stops the loop early (before `max_iters`). `None`
    /// (the default, and what every non-loop result carries) = no opinion → run to `max_iters`.
    /// Additive ABI: omitted on the wire when `None`, so existing producers are byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_continue: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub loop_result: LoopResult,
    pub session_id: CompactString,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResult {
    pub agent_id: CompactString,
    pub result: LoopResult,
}
