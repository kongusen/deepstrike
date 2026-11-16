//! What the kernel says to the model, in one place (kernel-distillation plan task 1.7).
//!
//! Every notice, refusal and acknowledgement the kernel itself writes into a model's context is
//! built here: the signals of the state turn, the results the kernel answers a call with, and the
//! acknowledgements of its own tools. The words are an experimental variable — they can be read,
//! counted (`kernel_text_tokens` in the lifecycle report) and swapped as a whole from this file.
//! Each function returns exactly the bytes that used to be written at its call site.

use crate::runtime::session::RollbackReason;

// ── Signals: notices rendered once in the state turn ─────────────────────────────────────────────

/// Knowledge entries a boundary removed, and how to get them back.
pub fn knowledge_removed(keys: &[String]) -> String {
    format!(
        "[KNOWLEDGE] entries removed at this boundary: {} — re-fetch via the memory tool if still needed.",
        keys.join(", ")
    )
}

/// A skill whose activation lease ran out.
pub fn skill_lease_expired(name: &str) -> String {
    format!("[SKILL] lease expired: {name} unloaded; the full toolset is restored.")
}

/// The milestone phase that begins now.
pub fn next_milestone_phase(id: &str, criteria: &[String]) -> String {
    if criteria.is_empty() {
        format!("[NEXT MILESTONE PHASE: {id}]")
    } else {
        format!("[NEXT MILESTONE PHASE: {id} — Criteria: {}]", criteria.join("; "))
    }
}

/// A milestone the verifier blocked.
pub fn milestone_blocked(phase_id: &str, reason: &str) -> String {
    format!("[MILESTONE BLOCKED: {phase_id} — {reason}. Address the criteria and try again.]")
}

/// A host signal that interrupts the current work.
pub fn interrupt(summary: &str) -> String {
    format!("[INTERRUPT] {summary}")
}

/// A host signal read at the next turn.
pub fn signal(summary: &str) -> String {
    format!("[SIGNAL] {summary}")
}

/// A sub-agent's result returned to its parent.
pub fn sub_agent_result(quarantined: bool, agent_id: &str, summary: &str) -> String {
    let marker = if quarantined { "quarantined sub-agent" } else { "sub-agent" };
    format!("[{marker} {agent_id}] {summary}")
}

/// The repeat fuse ends the run after one final report turn.
pub fn no_progress(signature: &str, count: u32) -> String {
    format!(
        "[NO-PROGRESS] `{signature}` was re-issued {count}x consecutively with no new outcome. \
         The run is terminating. Report what was accomplished and what remains, in plain text."
    )
}

/// A response cut off at the output cap, resumed.
pub const OUTPUT_TRUNCATION_NUDGE: &str = "Output token limit hit. Resume directly — no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.";

/// The criteria self-check before a run finishes.
pub fn criteria_check_finish(criteria: &[String]) -> String {
    format!(
        "[CRITERIA CHECK] You are about to finish. Verify each acceptance \
         criterion first: {}. If any is NOT met, continue working on it now. \
         If all are met, give the final answer.",
        criteria.join(" | ")
    )
}

/// The criteria self-check before a loop round stops.
pub fn criteria_check_pace(criteria: &[String]) -> String {
    format!(
        "[CRITERIA CHECK] You proposed stopping the loop. Verify each acceptance \
         criterion first: {}. If any is NOT met, continue working (or pace(continue)). \
         If all are met, call pace(stop) again.",
        criteria.join(" | ")
    )
}

/// Why a step was rolled back.
pub fn rollback_note(reason: &RollbackReason) -> String {
    match reason {
        RollbackReason::FatalToolError { tool_name, error } => {
            format!(
                "The previous step failed (`{tool_name}`: {error}). Please try a different approach."
            )
        }
        RollbackReason::GovernanceDenied { tool_name, reason } => {
            format!(
                "Action `{tool_name}` was not allowed ({reason}). Please choose a different approach."
            )
        }
        RollbackReason::ProviderFailure { .. } => {
            "The previous attempt failed. Please try again.".to_string()
        }
        RollbackReason::Timeout => {
            "The previous step timed out. Please try a faster approach.".to_string()
        }
        RollbackReason::UserInterrupt => "Interrupted. Please continue.".to_string(),
        RollbackReason::MalformedReplay { .. } => {
            "Context inconsistency detected. Please continue.".to_string()
        }
    }
}

/// A control request refused before any transaction began.
pub fn control_rejection(operation: &str, reason: &str) -> String {
    format!("Action `{operation}` was not allowed ({reason}). Please choose a different approach.")
}

// ── Results the kernel answers a call with ───────────────────────────────────────────────────────

/// A batch the executor could not run.
pub fn executor_failed(kind: &str) -> String {
    format!(
        "not executed: the executor could not run this batch ({kind}). The call \
         did not take effect — try a different approach or a smaller step."
    )
}

/// A call batched with `pace`, never executed.
pub const SUPERSEDED_BY_PACE: &str = "not executed: superseded by pace — the round is ending";

/// A call that timed out.
pub fn tool_timed_out(name: &str) -> String {
    format!(
        "Tool call `{name}` timed out before completing. The operation \
         may or may not have taken effect — verify before assuming, \
         then retry with a smaller step or a faster approach."
    )
}

/// A malformed `pace` proposal.
pub const PACE_REJECTED: &str = "pace rejected: next must be continue|sleep|stop";

/// `pace(stop)` with standing criteria.
pub const PACE_STOP_VERIFY: &str =
    "pace(stop) noted — verify the acceptance criteria first, then pace again.";

/// An accepted `pace` decision.
pub fn pace_acknowledged(action: &str, delay_ms: Option<u64>) -> String {
    format!(
        "pace acknowledged: {}{} — wrap up with a brief round report.",
        action,
        delay_ms.map(|d| format!(" {d}ms")).unwrap_or_default()
    )
}

/// A call governance denied.
pub fn permission_denied(reason: &str) -> String {
    format!("permission denied: {reason}")
}

/// A host tool this run never offered.
pub fn tool_not_in_toolset(name: &str) -> String {
    format!(
        "Tool '{name}' is not part of this run's toolset. Choose from the tools listed in \
         your tools schema."
    )
}

/// A kernel tool this turn did not offer.
pub fn tool_not_available_this_turn(name: &str) -> String {
    format!(
        "Tool '{name}' is not available on this turn. Choose from the tools listed in \
         your tools schema."
    )
}

/// The repeat fuse's refusal of a repeated call.
pub fn repeat_denied(count: u32) -> String {
    format!(
        "not executed: this exact call (same tool, same arguments) has been issued \
         {count}x consecutively with no new outcome — do something DIFFERENT: change \
         the arguments, use another tool, or report the task state as it stands"
    )
}

/// The acknowledgement of a kernel tool the kernel carried out.
pub fn syscall_ack(name: &str) -> &'static str {
    match name {
        "start_workflow" => {
            "workflow accepted: its ready nodes are scheduled; each result arrives as that node \
             completes"
        }
        "submit_workflow_nodes" => {
            "nodes appended to the running workflow; each result arrives as that node completes"
        }
        "skill" => "skill activated: its guidance and tools are in this turn's context",
        "update_plan" => "plan updated: the new state renders in [TASK STATE] from here on",
        super::notes::NOTE_TOOL_NAME => "note saved: it renders in [TASK STATE] from here on",
        super::manager::MEMORY_TOOL_NAME => {
            "memory search issued: matching records are added to this conversation before your \
             next turn"
        }
        super::manager::PROPOSE_MEMORY_TOOL_NAME => {
            "memory proposal submitted for review: it is not stored until the host accepts it, \
             and its outcome is added to this conversation before your next turn"
        }
        super::manager::READ_RESULT_TOOL_NAME => "page-in requested",
        "send_message" | "publish_channel" => "local handle routed",
        "receive_mailbox" | "receive_channel" | "read_object" => "local state returned",
        _ => "accepted",
    }
}
