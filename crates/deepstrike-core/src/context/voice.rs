//! What the kernel says to the model, in one place (kernel-distillation plan task 1.7).
//!
//! Every notice, refusal and acknowledgement the kernel itself writes into a model's context is
//! built here: the signals of the state turn, the results the kernel answers a call with, and the
//! acknowledgements of its own tools. The words are an experimental variable — they can be read,
//! counted (`kernel_text_tokens` in the lifecycle report) and swapped as a whole from this file.
//! Each function returns exactly the bytes that used to be written at its call site.

use super::task_state::{Note, TaskState};
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

// ── Kernel notices written into history ─────────────────────────────────────────────────────────

/// The index line one recalled record becomes. The header names the record, its name, kind and
/// trust; the body is its one-line description (its name when it has none). The model reads the
/// record in full with `memory(record_id)`.
pub fn recall_index_line(record_id: &str, kind: &str, trust: &str, description: &str, name: &str) -> String {
    let summary = if description.trim().is_empty() { name.trim() } else { description.trim() };
    let trust = if trust.is_empty() { String::new() } else { format!(" trust={trust}") };
    let name = memory_name_field(name);
    format!("[MEMORY record_id={record_id}{name} kind={kind}{trust}] {summary}")
}

/// ` name=<slug>` for a memory header, or nothing for a record without a name. The name is what
/// `propose_memory` asks the model to reuse to update a record (plan task 4.4).
pub fn memory_name_field(name: &str) -> String {
    if name.trim().is_empty() { String::new() } else { format!(" name={}", name.trim()) }
}

/// A record a search or a `memory(record_id)` read returned, in full.
pub fn memory_hit(record_ref: &str, name: &str, kind: &str, content: &str) -> String {
    format!("[MEMORY record_ref={record_ref}{} kind={kind}] {content}", memory_name_field(name))
}

/// A `memory(record_id)` read that found nothing.
pub fn memory_not_found(record_id: &str) -> String {
    format!(
        "[MEMORY record_ref={record_id}] not found: it was deleted, expired, or is \
         outside this memory scope."
    )
}

/// The outcome of the model's own `propose_memory`.
pub fn memory_proposal_outcome(name: &str, outcome: &str) -> String {
    format!("[MEMORY PROPOSAL name={name}] {outcome}")
}

/// A proposal the host stored.
pub fn proposal_stored(record_ref: &str) -> String {
    format!("stored as record {record_ref}.")
}

/// A proposal the host could not store.
pub fn proposal_not_stored(error: &str) -> String {
    format!("not stored: {error}")
}

/// A proposal waiting for review.
pub fn proposal_pending(candidate_id: &str) -> String {
    format!("pending review as candidate {candidate_id}; it is not stored until the host accepts it.")
}

/// A body `read_result` paged back in.
pub fn payload(handle_id: impl std::fmt::Display, content: &str) -> String {
    format!("[PAYLOAD handle_id={handle_id}]\n{content}")
}

/// What a dependent workflow node's goal is followed by, per dependency.
pub fn dependency_references(ids: &[&str]) -> String {
    format!("[dependency references]\n{}", ids.join(", "))
}

/// A dependency output clipped to its preview.
pub fn dependency_output_clipped(id: &str, chars: usize, preview: &str) -> String {
    format!(
        "[dependency {id} output, {chars} characters]\n{preview}\n…[read_result(call_id=\"dep:{id}\") reads it in full]"
    )
}

/// A dependency output shown whole.
pub fn dependency_output(id: &str, output: &str) -> String {
    format!("[dependency {id} output]\n{output}")
}

// ── The rendered request ────────────────────────────────────────────────────────────────────────

/// The first user turn when a rendered history would otherwise start mid-exchange.
pub const CONTEXT_RESUMED: &str = "[context resumed]";

/// The state turn's closing anchor.
pub fn state_turn(body: &str) -> String {
    format!("{body}\n\nProceed.")
}

/// The marker that ends a tool result's preview, and says how to read the rest.
pub fn read_result_hint(call_id: &str) -> String {
    format!("Call the read_result tool with call_id \"{call_id}\" to re-read the full result.]")
}

/// A tool result shown as its preview.
pub fn truncated_output(preview: &str, dropped: usize, call_id: &str) -> String {
    format!(
        "{preview}…\n[Output truncated: {dropped} bytes omitted. Call the read_result tool with call_id \"{call_id}\" to re-read the full result.]"
    )
}

/// The heading of the notes block in the state turn.
pub const NOTES_HEADING: &str =
    "notes (yours, kept verbatim through compaction; rewrite a note by its title as it changes):";

/// One note's line in the state turn.
pub fn note_line(note: &Note) -> String {
    format!("  - [{}] {}: {}", note.kind.label(), note.title, note.body)
}

/// The notes block; empty without notes.
pub fn notes_block(notes: &[Note]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut lines = vec![NOTES_HEADING.to_string()];
    lines.extend(notes.iter().map(note_line));
    lines.join("\n")
}

/// The `[TASK STATE]` block of the state turn; empty before a task is set.
pub fn task_state_block(state: &TaskState) -> String {
    if state.goal.is_empty() && state.plan.is_empty() && state.progress.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    lines.push(format!("[TASK STATE] goal: {}", state.goal));
    if !state.criteria.is_empty() {
        lines.push(format!("criteria: {}", state.criteria.join(" | ")));
    }
    // Directives right after the goal: a recent user command keeps its imperative force across
    // compaction and renewal.
    if !state.directives.is_empty() {
        lines.push("active_directives (most recent last):".to_string());
        for d in &state.directives {
            lines.push(format!("  - {d}"));
        }
    }
    if !state.plan.is_empty() {
        lines.push("plan:".to_string());
        for (i, step) in state.plan.iter().enumerate() {
            let marker = if step.done {
                "done"
            } else if Some(i) == state.current_step {
                "active"
            } else {
                "todo"
            };
            lines.push(format!("  [{}] {}. {}", marker, i + 1, step.label));
        }
    }
    if !state.progress.is_empty() {
        lines.push(format!("progress: {}", state.progress));
    }
    if !state.blocked_on.is_empty() {
        lines.push(format!("blocked_on: {}", state.blocked_on.join(", ")));
    }
    let notes = notes_block(&state.notes);
    if !notes.is_empty() {
        lines.push(notes);
    }
    if !state.scratchpad.is_empty() {
        lines.push(format!("scratchpad: {}", state.scratchpad));
    }
    // Only the latest three compaction records; older ones stay in `compression_log`.
    if !state.compression_log.is_empty() {
        lines.push("compression_history:".to_string());
        let start = state.compression_log.len().saturating_sub(3);
        for entry in &state.compression_log[start..] {
            if entry.summary.is_empty() {
                lines.push(format!("  [{}]", entry.action));
            } else {
                lines.push(format!("  [{}] {}", entry.action, entry.summary));
            }
        }
    }
    lines.join("\n")
}
