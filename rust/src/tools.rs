use async_trait::async_trait;
use deepstrike_core::types::message::ToolSchema;
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum ToolChunk {
    Text(String),
    Progress { progress: f64, message: Option<String> },
    Artifact { artifact_id: String, mime_type: Option<String>, label: Option<String> },
    JsonPatch(Value),
    Suspend { suspension_id: String, payload: Option<Value> },
}

impl ToolChunk {
    pub fn text(value: impl Into<String>) -> Self { Self::Text(value.into()) }
    pub fn progress(progress: f64, message: Option<String>) -> Self { Self::Progress { progress, message } }
    pub fn artifact(artifact_id: impl Into<String>, mime_type: Option<String>, label: Option<String>) -> Self {
        Self::Artifact { artifact_id: artifact_id.into(), mime_type, label }
    }
    pub fn json_patch(patch: Value) -> Self { Self::JsonPatch(patch) }
    pub fn suspend(suspension_id: impl Into<String>, payload: Option<Value>) -> Self {
        Self::Suspend { suspension_id: suspension_id.into(), payload }
    }
    pub fn text_projection(&self) -> &str {
        match self { Self::Text(s) => s.as_str(), _ => "" }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStep {
    Chunk(ToolChunk),
    Done(String),
}

#[async_trait]
pub trait ToolSession: Send {
    async fn next(&mut self, resume_input: Option<Value>) -> Result<ToolStep>;
}

pub struct TextToolSession {
    output: Option<String>,
}

impl TextToolSession {
    pub fn new(output: impl Into<String>) -> Self { Self { output: Some(output.into()) } }
}

#[async_trait]
impl ToolSession for TextToolSession {
    async fn next(&mut self, _resume_input: Option<Value>) -> Result<ToolStep> {
        Ok(ToolStep::Done(self.output.take().unwrap_or_default()))
    }
}

pub type ToolFn = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Box<dyn ToolSession>>> + Send + Sync>;

pub struct RegisteredTool {
    pub schema: ToolSchema,
    pub start: ToolFn,
}

impl RegisteredTool {
    pub fn new(
        name: impl Into<compact_str::CompactString>,
        description: impl Into<String>,
        parameters: Value,
        f: impl Fn(Value) -> BoxFuture<'static, Result<Box<dyn ToolSession>>> + Send + Sync + 'static,
    ) -> Self {
        Self { schema: ToolSchema { name: name.into(), description: description.into(), parameters }, start: Arc::new(f) }
    }

    pub fn text(
        name: impl Into<compact_str::CompactString>,
        description: impl Into<String>,
        parameters: Value,
        f: impl Fn(Value) -> BoxFuture<'static, Result<String>> + Send + Sync + 'static,
    ) -> Self {
        Self::new(name, description, parameters, move |args| {
            let fut = f(args);
            Box::pin(async move { Ok(Box::new(TextToolSession::new(fut.await?)) as Box<dyn ToolSession>) })
        })
    }
}

pub fn validate_tool_arguments(schema: &Value, args: &Value) -> std::result::Result<(), String> {
    validate_value(schema, args, "$", true)
}

fn validate_value(schema: &Value, value: &Value, path: &str, is_root: bool) -> std::result::Result<(), String> {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        match expected {
            "object" => {
                let Some(obj) = value.as_object() else { return Err(format!("{path} must be object")); };
                if let Some(required) = schema.get("required").and_then(Value::as_array) {
                    for key in required.iter().filter_map(Value::as_str) {
                        if !obj.contains_key(key) { return Err(format!("{path}.{key} is required")); }
                    }
                }
                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    for (key, child_schema) in properties {
                        if let Some(child_value) = obj.get(key) {
                            validate_value(child_schema, child_value, &format!("{path}.{key}"), false)?;
                        }
                    }
                }
            }
            "array" if !value.is_array() => return Err(format!("{path} must be array")),
            "string" if !value.is_string() => return Err(format!("{path} must be string")),
            "number" if !value.is_number() => return Err(format!("{path} must be number")),
            "integer" if !value.is_i64() && !value.is_u64() => return Err(format!("{path} must be integer")),
            "boolean" if !value.is_boolean() => return Err(format!("{path} must be boolean")),
            _ => {}
        }
    } else if is_root && !value.is_object() {
        return Err(format!("{path} must be object"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.contains(value) { return Err(format!("{path} must be one of enum values")); }
    }
    Ok(())
}

pub async fn execute_tools(
    calls: &[deepstrike_core::types::message::ToolCall],
    registry: &HashMap<String, RegisteredTool>,
) -> Vec<deepstrike_core::types::message::ToolResult> {
    let mut results = Vec::new();
    for call in calls {
        let Some(tool) = registry.get(call.name.as_str()) else {
            results.push(tool_result(call.id.clone(), format!("unknown tool: {}", call.name), true));
            continue;
        };
        if let Err(e) = validate_tool_arguments(&tool.schema.parameters, &call.arguments) {
            results.push(tool_result(call.id.clone(), format!("invalid arguments: {e}"), true));
            continue;
        }
        let mut session = match (tool.start)(call.arguments.clone()).await {
            Ok(session) => session,
            Err(e) => {
                results.push(tool_result(call.id.clone(), e.to_string(), true));
                continue;
            }
        };
        let mut combined = String::new();
        loop {
            match session.next(None).await {
                Ok(ToolStep::Chunk(chunk)) => {
                    if matches!(chunk, ToolChunk::Suspend { .. }) {
                        results.push(tool_result(call.id.clone(), "tool suspended without resume handler".into(), true));
                        break;
                    }
                    combined.push_str(chunk.text_projection());
                }
                Ok(ToolStep::Done(text)) => {
                    combined.push_str(&text);
                    results.push(tool_result(call.id.clone(), combined, false));
                    break;
                }
                Err(e) => {
                    results.push(tool_result(call.id.clone(), e.to_string(), true));
                    break;
                }
            }
        }
    }
    results
}

fn tool_result(call_id: compact_str::CompactString, output: String, is_error: bool) -> deepstrike_core::types::message::ToolResult {
    deepstrike_core::types::message::ToolResult {
        call_id,
        output: deepstrike_core::types::message::Content::Text(output),
        is_error,
        token_count: None,
    }
}

pub fn read_file_tool() -> RegisteredTool {
    RegisteredTool::text(
        "read_file",
        "Read the contents of a file.",
        serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        |args| Box::pin(async move {
            let path = args["path"].as_str().ok_or_else(|| Error::Tool("missing path".into()))?;
            tokio::fs::read_to_string(path).await.map_err(|e| Error::Tool(e.to_string()))
        }),
    )
}
