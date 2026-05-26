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
    Done {
        iterations: u32,
        total_tokens: u64,
        status: String,
    },
    Error(String),
}
