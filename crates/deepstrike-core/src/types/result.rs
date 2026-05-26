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
