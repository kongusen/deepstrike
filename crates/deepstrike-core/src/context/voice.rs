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

// ── Refusals: why a kernel-tool call was not carried out ────────────────────────────────────────
//
// The reason of a refused call is both an audit fact (`syscall_rejected`) and the result the model
// reads on its next turn. Reasons that only pass on another component's message (a validation
// fault, a host policy's denial) carry that message unchanged.

pub mod refusal {
    pub fn quarantined(caller: &str, family: &str) -> String {
        format!(
            "quarantine: task {caller} is quarantined and may not widen its authority \
             through a {family} syscall"
        )
    }

    pub const EMPTY_WORKFLOW: &str = "an authored workflow with no nodes has nothing to spawn";
    pub const EMPTY_SUBMISSION: &str = "an empty submission appends nothing";
    pub const NO_WORKFLOW_IN_FLIGHT: &str =
        "no workflow is in flight, so there is no graph to append to";

    pub const CHANNEL_EMPTY: &str = "channel_id and subscribers must be non-empty";
    pub const CHANNEL_SUBSCRIBERS_DUPLICATE: &str = "channel subscribers must be unique";
    pub const MAILBOX_LIMIT: &str = "limit must be between 1 and 64";
    pub const IPC_LABELS: &str = "message_id and message_kind must contain 1..=256 bytes";

    pub fn ipc_handle_unreachable(handle_id: &str) -> String {
        format!("payload handle {handle_id} is not reachable by this operation")
    }

    /// One reason per local-IPC failure.
    pub(crate) fn local_ipc(error: &crate::scheduler::tcb::LocalIpcError) -> &'static str {
        use crate::scheduler::tcb::LocalIpcError;
        match error {
            LocalIpcError::UnknownCaller => "unknown caller",
            LocalIpcError::CallerTerminal => "caller is terminal",
            LocalIpcError::UnknownRecipient => "unknown recipient",
            LocalIpcError::ChannelSubscribersMismatch => "channel subscriber set is immutable",
            LocalIpcError::NotSubscriber => "caller is not a channel subscriber",
            LocalIpcError::Full => "IPC capacity is full",
            LocalIpcError::Expired => "message TTL already expired",
            LocalIpcError::ObjectConflict => "object id already names a different descriptor",
        }
    }

    pub fn object_not_registered(object_id: &str) -> String {
        format!("object {object_id} is not registered")
    }

    pub fn no_read_capability(caller: &str, object_id: &str) -> String {
        format!("caller {caller} has no read capability for object {object_id}")
    }

    pub fn no_read_capability_shared(caller: &str, object_id: &str) -> String {
        format!("caller {caller} has no read capability for shared object {object_id}")
    }

    pub const MEMORY_UNBOUND_WRITE: &str =
        "this operation holds no memory binding, so it can author no memory record";
    pub const MEMORY_READ_ONLY: &str = "this operation's memory binding is read-only";
    pub const MEMORY_PROPOSAL_PENDING: &str = "Another memory proposal is already pending review. Propose this one again after its outcome arrives.";
    pub const MEMORY_UNBOUND_READ: &str =
        "this operation holds no memory binding, so it can read no memory record";
    pub const MEMORY_WRITE_ONLY: &str = "this operation's memory binding is write-only";
    pub const MEMORY_QUERY_PENDING: &str =
        "A different memory query is already pending. Retry this query after its result arrives.";
    pub const MEMORY_QUERY_AND_RECORD: &str = "pass either query or record_id, not both";
    pub const MEMORY_QUERY_NEITHER: &str = "pass a query or a record_id";
    pub const MEMORY_WRITE_DENIED: &str = "memory write denied";
    pub const WORKFLOW_AUTHORING_DENIED: &str = "workflow authoring denied";

    pub fn malformed_record_id(message: &str) -> String {
        format!("malformed record_id: {message}")
    }

    pub fn malformed_arguments(error: &str) -> String {
        format!("malformed arguments: {error}")
    }

    pub fn malformed_handle(message: &str) -> String {
        format!("malformed handle: {message}")
    }

    pub fn parent_request_refused(seq: usize, message: &str) -> String {
        format!("request {seq} refused: {message}")
    }

    /// A host admission policy's answer, or `fallback` when it allowed the call.
    pub fn denial(disposition: &crate::syscall::Disposition, fallback: &str) -> String {
        use crate::syscall::Disposition;
        match disposition {
            Disposition::Deny { stage, reason } => format!("{stage}: {reason}"),
            Disposition::RateLimited { retry_after_ms } => {
                format!("rate limited; retry after {retry_after_ms}ms")
            }
            Disposition::Gate { reason, .. } => format!("awaiting approval: {reason}"),
            Disposition::Defer { slot } => format!("deferred at slot {slot}"),
            Disposition::Allow => fallback.to_string(),
        }
    }

    pub fn no_dependency_output(agent_id: &str) -> String {
        format!("this run has no dependency output from {agent_id}")
    }

    pub fn handle_unreachable(handle_id: &str) -> String {
        format!(
            "handle {handle_id} is not reachable in this operation's handle table; a \
             page-in addresses only what the caller already holds"
        )
    }

    pub fn handle_not_paged(handle_id: &str, residency: &str) -> String {
        format!(
            "handle {handle_id} is {residency} — its body is held by this kernel, not by the \
             payload store, so there is nothing to page in"
        )
    }

    pub fn unknown_skill(name: &str) -> String {
        format!(
            "this operation declares no skill named {name:?}; activation is a \
             capability mutation and is refused rather than invented"
        )
    }

    pub fn skill_grants_widen(skill_name: &str, capability_ids: &[&str]) -> String {
        format!(
            "skill {skill_name:?} declares capability grants that would widen the mounting agent's authority: {}",
            capability_ids.join(", ")
        )
    }

    // `note`
    pub const NOTES_DISABLED: &str = "notes are not enabled for this run";
    pub const NOTE_NEEDS_TITLE: &str = "a note needs a title";
    pub const NOTE_NEEDS_BODY: &str = "a note needs a body";

    pub fn note_title_too_long(max: usize) -> String {
        format!("a note title may have at most {max} characters")
    }

    pub fn no_such_note(title: &str, titles: &[&str]) -> String {
        let listing = if titles.is_empty() {
            "there are no notes".to_string()
        } else {
            format!("notes: {titles:?}")
        };
        format!("there is no note titled {title:?}; {listing}")
    }

    pub fn notes_full(used: u32, budget: u32) -> String {
        format!(
            "notes are full ({used} of {budget} tokens with this one): merge notes into \
             fewer, shorten them, or retire ones you no longer need, then write again"
        )
    }

    // Write authority (`context::authority`).
    pub fn item_write(origin: &str, author: &str, writer: &str, op: &str) -> String {
        format!("a {origin} item is written by the {author}; the {writer} may not {op} it")
    }

    pub fn task_fields(writer: &str, refused: &[&str], writable: &[&str]) -> String {
        format!(
            "the {writer} may not set {}; nothing was applied. Fields it may set: {}",
            refused.join(", "),
            if writable.is_empty() { "none".to_string() } else { writable.join(", ") }
        )
    }

    pub fn active_skill_body(key: &str) -> String {
        format!(
            "knowledge {key:?} is the body of an active skill; deactivate the skill \
             instead of removing its body"
        )
    }

    // Memory policy validation.
    pub const MEMORY_NAME_EMPTY: &str = "memory name must not be empty";

    pub fn memory_name_too_long(max: u32) -> String {
        format!("memory name exceeds {max} characters")
    }

    pub fn memory_content_too_large(max: u32) -> String {
        format!("memory content exceeds {max} bytes")
    }
}

/// `note` acknowledgements.
pub fn note_retired(title: &str, left: usize) -> String {
    format!("note {title:?} retired; {left} note(s) left")
}

pub fn note_saved(title: &str, updated: bool, count: usize, used: u32, budget: u32, linked: usize, dead: &[String]) -> String {
    let verb = if updated { "updated" } else { "saved" };
    let mut ack = format!(
        "note {title:?} {verb}; it shows in [TASK STATE] from your next turn ({count} note(s), {used} of \
         {budget} tokens); linked to {linked} result(s)"
    );
    if !dead.is_empty() {
        ack.push_str(&format!("; no note yet for link(s) {dead:?}"));
    }
    ack
}

// ── Kernel tools: what each says it does ───────────────────────────────────────────────────────

pub fn read_result_tool_description() -> String {
    "Re-read the full content of a tool result that was truncated to save \
                          context (marked '[Output truncated: … call the read_result tool …]'), \
                          or of a dependency output (call_id \"dep:<agent_id>\"). Pass that \
                          call_id; use offset/max_bytes to page through large content.".to_string()
}

pub fn read_result_tool_parameters() -> serde_json::Value {
    serde_json::json!({
                "type": "object",
                "properties": {
                    "call_id": { "type": "string" },
                    "offset": { "type": "integer", "description": "Byte offset to start from (default 0)." },
                    "max_bytes": { "type": "integer", "description": "Max bytes to return (default 4000)." }
                },
                "required": ["call_id"]
            })
}

pub fn update_plan_tool_description() -> String {
    "Update your task plan and progress. Call this after completing a step or when the plan changes.".to_string()
}

pub fn update_plan_tool_parameters() -> serde_json::Value {
    serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": { "type": "array", "items": { "type": "string" } },
                    "current_step": { "type": "integer" },
                    "progress": { "type": "string" },
                    "blocked_on": { "type": "array", "items": { "type": "string" } }
                }
            })
}

pub fn memory_tool_description() -> String {
    "Read your long-term memory. Pass `query` to search it for relevant past \
experiences and knowledge, or `record_id` to read one record in full. Recalled memories arrive as \
index lines `[MEMORY record_id=… name=… kind=… trust=…] <description>`: when one looks relevant to the \
task, read it by its record_id before relying on it.".to_string()
}

pub fn memory_tool_parameters() -> serde_json::Value {
    serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to search for. Omit when passing record_id." },
                    "record_id": { "type": "string", "description": "Read this one record in full, as named by a recalled index line. Omit when passing query." },
                    "top_k": { "type": "integer" }
                }
            })
}

pub fn propose_memory_tool_description() -> String {
    "Propose one fact for long-term memory, so it is available in future sessions. \
Propose when you learn something that will still matter later and cannot be recovered from the code, files, or this conversation's history: \
who the user is and how they like to work, a correction or confirmed approach they gave you (with why), \
a project goal, constraint, or decision, or a pointer to an external resource. \
Do not propose what the workspace already records, temporary task state, or guesses. \
Search memory first and reuse an existing name to update a fact rather than duplicating it. \
The proposal is reviewed before it is stored; its outcome arrives in this conversation before your next turn. \
Propose one fact per call and wait for the outcome before proposing another. \
Scope, identity, provenance, and timestamps are derived for you.".to_string()
}

pub fn propose_memory_tool_parameters() -> serde_json::Value {
    serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string", "description": "Short stable kebab-case slug for this fact; reuse an existing name to update it." },
                    "kind": { "type": "string", "enum": ["user", "feedback", "project", "reference"], "description": "user: who the user is and their preferences. feedback: guidance on how to work. project: goals, constraints, decisions. reference: pointers to external resources." },
                    "content": { "type": "string", "description": "The fact itself, self-contained and stated so a future session can act on it without this conversation." },
                    "description": { "type": "string", "description": "One-line summary used to decide relevance at recall time." },
                    "evidence_refs": { "type": "array", "items": { "type": "string" }, "description": "Optional references (call ids, record ids, paths) supporting the fact." }
                },
                "required": ["name", "kind", "content"]
            })
}

pub fn knowledge_tool_description() -> String {
    "Search the external knowledge base for facts, documentation, or reference data.".to_string()
}

pub fn knowledge_tool_parameters() -> serde_json::Value {
    serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "top_k": { "type": "integer" }
                },
                "required": ["query"]
            })
}

pub fn note_tool_description(propose_memory_enabled: bool) -> String {
    let mut description = "Keep notes that survive compaction. As the conversation grows, older \
tool results and turns are archived; your notes stay in [TASK STATE] word for word. Write down \
what you work out and could not simply read again: running counts and totals, lists of what is \
done and what is left, conclusions, rules you learned. When it changes, write the same title \
again to update it. `evidence` names what it came from (call ids, file paths or arguments); \
without it the note is linked to the results you received since your last note. Retire a note \
you no longer need."
        .to_string();
    // Plan task 9.4 · notes end with the session; what outlives it is the model's call, made
    // through the memory proposal it already has. No promotion happens behind its back.
    if propose_memory_enabled {
        description.push_str(
            " Notes end with this task: a rule or index worth keeping for later tasks can be \
proposed for long-term memory with `propose_memory`.",
        );
    }
    description
}

pub fn note_tool_parameters() -> serde_json::Value {
    serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["write", "retire"], "description": "Default: write." },
                    "title": { "type": "string", "description": "The note's name; writing an existing title updates it." },
                    "kind": { "type": "string", "enum": ["state", "rule", "question", "index"], "description": "state: progress of this task (counts, done-lists); rule: a lesson or rule; question: something still open; index: where things are." },
                    "body": { "type": "string" },
                    "evidence": { "type": "array", "items": { "type": "string" } },
                    "links": { "type": "array", "items": { "type": "string" }, "description": "Titles of related notes." },
                    "replaced_by": { "type": "string", "description": "On retire: the note that replaces this one." }
                },
                "required": ["title"]
            })
}

pub fn skill_tool_description(available_skills: &str) -> String {
    let xml = available_skills;
    format!(
                "Load a skill into your context to access specialized instructions for a task.\n\n{xml}"
            )
}

pub fn skill_tool_parameters() -> serde_json::Value {
    serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the skill to load."
                    }
                },
                "required": ["name"]
            })
}
