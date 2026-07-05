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
    /// Reactive recovery ladder exhausted on a provider context-overflow (prompt-too-long /
    /// 413): the kernel compacted as hard as it could and the prompt still won't fit. Distinct
    /// from `Timeout` (which the SDK used to fabricate for this case) so embedders can tell an
    /// unrecoverable overflow from a wall-clock deadline.
    ContextOverflow,
    /// Repeat-fuse escalation: the agent kept re-issuing the same tool call (same name AND args)
    /// past the governance `terminate_after` threshold — a stall, not forward motion. Distinct
    /// from `MaxTurns` (which a productive run can also hit) so embedders can tell "looped with
    /// no progress" from "ran out of turns doing real work".
    NoProgress,
}

impl TerminationReason {
    /// Canonical snake_case wire label, kept in lockstep with the serde rename —
    /// the single source for session logs, observations, and FFI bindings.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::MaxTurns => "max_turns",
            Self::TokenBudget => "token_budget",
            Self::Timeout => "timeout",
            Self::UserAbort => "user_abort",
            Self::Error => "error",
            Self::MilestoneExceeded => "milestone_exceeded",
            Self::ContextOverflow => "context_overflow",
            Self::NoProgress => "no_progress",
        }
    }
}

/// What the loop agent proposed to do after this round (`pace` meta-tool verb).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaceAction {
    /// Start the next round immediately.
    Continue,
    /// Sleep `delay_ms`, then start the next round.
    Sleep,
    /// The loop is done (goal met, budget spent, or nothing left to do).
    Stop,
}

impl PaceAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Sleep => "sleep",
            Self::Stop => "stop",
        }
    }
}

/// The kernel-adjudicated outcome of a `pace` proposal: the model PROPOSES, the trap
/// clamps/coerces (delay bounds, max_rounds), and `coerced_from` records any override
/// so the decision stays auditable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaceDecision {
    pub action: PaceAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    pub reason: String,
    /// The model's original proposal when the trap changed it (e.g. "sleep 5" clamped,
    /// or "continue" forced to stop at max_rounds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coerced_from: Option<String>,
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
    /// Loop-agent pacing (③): the adjudicated after-round decision. `None` for every
    /// non-loop run. Additive ABI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pace_decision: Option<PaceDecision>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_matches_serde_rename() {
        for r in [
            TerminationReason::Completed,
            TerminationReason::MaxTurns,
            TerminationReason::TokenBudget,
            TerminationReason::Timeout,
            TerminationReason::UserAbort,
            TerminationReason::Error,
            TerminationReason::MilestoneExceeded,
            TerminationReason::ContextOverflow,
            TerminationReason::NoProgress,
        ] {
            let serde_name = serde_json::to_value(r).unwrap();
            assert_eq!(serde_name.as_str().unwrap(), r.label());
        }
    }
}
