use async_trait::async_trait;
use deepstrike_core::types::message::ToolSchema;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::{Error, Result};

pub type ToolFn = Arc<dyn Fn(Value) -> futures::future::BoxFuture<'static, Result<String>> + Send + Sync>;

pub struct RegisteredTool {
    pub schema: ToolSchema,
    pub execute: ToolFn,
}

impl RegisteredTool {
    pub fn new(
        name: impl Into<compact_str::CompactString>,
        description: impl Into<String>,
        parameters: Value,
        f: impl Fn(Value) -> futures::future::BoxFuture<'static, Result<String>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            schema: ToolSchema { name: name.into(), description: description.into(), parameters },
            execute: Arc::new(f),
        }
    }
}

pub async fn execute_tools(
    calls: &[deepstrike_core::types::message::ToolCall],
    registry: &HashMap<String, RegisteredTool>,
) -> Vec<deepstrike_core::types::message::ToolResult> {
    let futs = calls.iter().map(|c| {
        let tool = registry.get(c.name.as_str());
        let call_id = c.id.clone();
        let args = c.arguments.clone();
        async move {
            let (output, is_error) = match tool {
                None => (format!("unknown tool: {}", call_id), true),
                Some(t) => match (t.execute)(args).await {
                    Ok(s) => (s, false),
                    Err(e) => (e.to_string(), true),
                },
            };
            deepstrike_core::types::message::ToolResult {
                call_id,
                output: deepstrike_core::types::message::Content::Text(output),
                is_error,
                token_count: None,
            }
        }
    });
    futures::future::join_all(futs).await
}

/// Built-in: read a file from disk.
pub fn read_file_tool() -> RegisteredTool {
    RegisteredTool::new(
        "read_file",
        "Read the contents of a file.",
        serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        |args| Box::pin(async move {
            let path = args["path"].as_str().ok_or_else(|| Error::Tool("missing path".into()))?;
            tokio::fs::read_to_string(path).await.map_err(|e| Error::Tool(e.to_string()))
        }),
    )
}
