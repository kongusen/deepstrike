//! Pure helpers for turn-transaction rollback messaging.
//!
//! Extracted from `LoopStateMachine` — these functions carry no state machine
//! context, only formatting a [`RollbackReason`] into the text surfaced to the
//! model.

use crate::runtime::session::RollbackReason;

/// Build the concise, model-facing note injected into the conversation after a rollback.
pub(crate) fn build_rollback_note(reason: &RollbackReason) -> String {
    crate::context::voice::rollback_note(reason)
}

/// Build model-facing feedback for a request rejected before any transaction began. Keeping this
/// separate from `build_rollback_note` prevents telemetry and prompts from falsely claiming state
/// was reverted when the requested effect never ran.
pub(crate) fn build_control_rejection_note(operation: &str, reason: &str) -> String {
    crate::context::voice::control_rejection(operation, reason)
}
