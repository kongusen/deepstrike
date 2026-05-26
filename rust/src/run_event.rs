use crate::tools::ToolChunk;

/// Streaming events from `RuntimeRunner` and `ExecutionPlane`.
#[derive(Debug, Clone)]
pub enum RunEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCall {
        id: String,
        name: String,
    },
    ToolDelta {
        call_id: String,
        name: String,
        chunk: ToolChunk,
    },
    ToolSuspend {
        call_id: String,
        name: String,
        suspension_id: String,
        payload: Option<serde_json::Value>,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    ToolArgumentRepaired {
        call_id: String,
        name: String,
        original_arguments: String,
        repaired_arguments: String,
    },
    /// Governance pipeline denied a tool call before execution.
    /// Emitted alongside `ToolResult { is_error: true }` so callers can
    /// distinguish policy denials from tool-side errors and write the correct
    /// `SessionEvent::ToolDenied` audit record.
    ToolDenied {
        call_id: String,
        tool_name: String,
        reason: String,
    },
    /// Governance pipeline requires user approval (ask_user verdict).
    /// Emitted before `ToolResult { is_error: true }` so runners can write
    /// `SessionEvent::PermissionRequested` + `SessionEvent::PermissionResolved`.
    PermissionRequest {
        call_id: String,
        tool_name: String,
        arguments: String,
        reason: String,
    },
    Done {
        iterations: u32,
        total_tokens: u64,
        status: String,
    },
    Error(String),
}
