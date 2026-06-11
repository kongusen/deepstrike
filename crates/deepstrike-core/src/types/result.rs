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
    /// Classify-node branch selection (A#2): a `NodeKind::Classify` node's agent reports the chosen
    /// branch label here; the kernel runs that branch and prunes the others. Additive ABI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classify_branch: Option<String>,
    /// Tournament-node judge verdict (A#2): a judge sub-agent of a `NodeKind::Tournament` node
    /// reports the winning entrant's agent id here (one of the match's two entrants). The kernel
    /// advances the bracket with it; the controller node's final result carries the champion's id
    /// in this same field. Additive ABI: omitted on the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tournament_winner: Option<String>,
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
