//! Pure helpers for turn-transaction rollback messaging.
//!
//! Extracted from `LoopStateMachine` — these functions carry no state machine
//! context, only formatting a [`RollbackReason`] into the text surfaced to the
//! model (concise) or telemetry (verbose) and extracting tool-result output.

use crate::runtime::session::RollbackReason;
use crate::types::message::{Content, ToolResult};

/// Flatten a tool result's output into plain text.
/// `Content::Parts` are serialised to JSON so the text can be carried faithfully.
pub(crate) fn tool_result_output_text(result: &ToolResult) -> String {
    match &result.output {
        Content::Text(s) => s.clone(),
        Content::Parts(parts) => serde_json::to_string(parts).unwrap_or_default(),
    }
}

/// Internal, telemetry-oriented description of a rollback reason.
pub(crate) fn rollback_reason_message(reason: &RollbackReason) -> String {
    match reason {
        RollbackReason::FatalToolError { tool_name, error } => {
            format!("fatal tool error in {tool_name}: {error}")
        }
        RollbackReason::GovernanceDenied { tool_name, reason } => {
            format!("governance denied {tool_name}: {reason}")
        }
        RollbackReason::ProviderFailure { error } => {
            format!("provider failure: {error}")
        }
        RollbackReason::Timeout => "timeout".to_string(),
        RollbackReason::UserInterrupt => "user interrupt".to_string(),
        RollbackReason::MalformedReplay { reason } => {
            format!("malformed replay: {reason}")
        }
    }
}

/// Build the note injected into the conversation after a rollback.
/// `verbose` uses the internal `[SYSTEM] Transaction rollback: …` format;
/// otherwise a concise, model-facing instruction is produced.
pub(crate) fn build_rollback_note(reason: &RollbackReason, verbose: bool) -> String {
    if verbose {
        return format!(
            "[SYSTEM] Transaction rollback: {}",
            rollback_reason_message(reason)
        );
    }
    match reason {
        RollbackReason::FatalToolError { tool_name, error } => {
            format!("The previous step failed (`{tool_name}`: {error}). Please try a different approach.")
        }
        RollbackReason::GovernanceDenied { tool_name, reason } => {
            format!("Action `{tool_name}` was not allowed ({reason}). Please choose a different approach.")
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
