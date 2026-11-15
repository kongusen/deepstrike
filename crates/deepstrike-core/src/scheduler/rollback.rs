//! Pure helpers for turn-transaction rollback messaging.
//!
//! Extracted from `LoopStateMachine` — these functions carry no state machine
//! context, only formatting a [`RollbackReason`] into the text surfaced to the
//! model.

use crate::runtime::session::RollbackReason;

/// Build the concise, model-facing note injected into the conversation after a rollback.
pub(crate) fn build_rollback_note(reason: &RollbackReason) -> String {
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

/// Build model-facing feedback for a request rejected before any transaction began. Keeping this
/// separate from `build_rollback_note` prevents telemetry and prompts from falsely claiming state
/// was reverted when the requested effect never ran.
pub(crate) fn build_control_rejection_note(operation: &str, reason: &str) -> String {
    format!("Action `{operation}` was not allowed ({reason}). Please choose a different approach.")
}
