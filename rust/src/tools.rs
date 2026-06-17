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
    Progress {
        progress: f64,
        message: Option<String>,
    },
    Artifact {
        artifact_id: String,
        mime_type: Option<String>,
        label: Option<String>,
    },
    JsonPatch(Value),
    Suspend {
        suspension_id: String,
        payload: Option<Value>,
    },
}

impl ToolChunk {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }
    pub fn progress(progress: f64, message: Option<String>) -> Self {
        Self::Progress { progress, message }
    }
    pub fn artifact(
        artifact_id: impl Into<String>,
        mime_type: Option<String>,
        label: Option<String>,
    ) -> Self {
        Self::Artifact {
            artifact_id: artifact_id.into(),
            mime_type,
            label,
        }
    }
    pub fn json_patch(patch: Value) -> Self {
        Self::JsonPatch(patch)
    }
    pub fn suspend(suspension_id: impl Into<String>, payload: Option<Value>) -> Self {
        Self::Suspend {
            suspension_id: suspension_id.into(),
            payload,
        }
    }
    pub fn text_projection(&self) -> &str {
        match self {
            Self::Text(s) => s.as_str(),
            _ => "",
        }
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
    pub fn new(output: impl Into<String>) -> Self {
        Self {
            output: Some(output.into()),
        }
    }
}

#[async_trait]
impl ToolSession for TextToolSession {
    async fn next(&mut self, _resume_input: Option<Value>) -> Result<ToolStep> {
        Ok(ToolStep::Done(self.output.take().unwrap_or_default()))
    }
}

pub type ToolFn =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Box<dyn ToolSession>>> + Send + Sync>;

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
        Self {
            schema: ToolSchema {
                name: name.into(),
                description: description.into(),
                parameters,
            },
            start: Arc::new(f),
        }
    }

    pub fn text(
        name: impl Into<compact_str::CompactString>,
        description: impl Into<String>,
        parameters: Value,
        f: impl Fn(Value) -> BoxFuture<'static, Result<String>> + Send + Sync + 'static,
    ) -> Self {
        Self::new(name, description, parameters, move |args| {
            let fut = f(args);
            Box::pin(async move {
                Ok(Box::new(TextToolSession::new(fut.await?)) as Box<dyn ToolSession>)
            })
        })
    }
}

pub fn validate_tool_arguments(
    schema: &Value,
    args: &mut Value,
) -> std::result::Result<bool, String> {
    let mut repaired = false;
    validate_value(schema, args, "$", true, &mut repaired)?;
    Ok(repaired)
}

fn validate_value(
    schema: &Value,
    value: &mut Value,
    path: &str,
    is_root: bool,
    repaired: &mut bool,
) -> std::result::Result<(), String> {
    // 1. 类型自动规整 (Auto-cast / Repair)
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        match expected {
            "boolean" => {
                if let Some(s) = value.as_str() {
                    if s == "true" {
                        *value = Value::Bool(true);
                        *repaired = true;
                    } else if s == "false" {
                        *value = Value::Bool(false);
                        *repaired = true;
                    }
                }
            }
            "number" | "integer" => {
                if let Some(s) = value.as_str() {
                    if let Ok(num) = s.parse::<f64>() {
                        if expected == "integer" {
                            if num == num.round() {
                                *value = Value::Number(serde_json::Number::from(num as i64));
                                *repaired = true;
                            }
                        } else {
                            if let Some(n) = serde_json::Number::from_f64(num) {
                                *value = Value::Number(n);
                                *repaired = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // 2. 补默认值 (Default Injection)
    if let Some(obj) = value.as_object_mut() {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (key, child_schema) in properties {
                if !obj.contains_key(key) {
                    if let Some(default_val) = child_schema.get("default") {
                        obj.insert(key.clone(), default_val.clone());
                        *repaired = true;
                    }
                }
            }
        }
    }

    // 3. 校验并递归
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        match expected {
            "object" => {
                let Some(obj) = value.as_object_mut() else {
                    return Err(format!("{path} must be object"));
                };

                // 3a. 裁剪 schema 未声明的多余字段
                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    let allowed_keys: std::collections::HashSet<&str> =
                        properties.keys().map(|s| s.as_str()).collect();
                    let keys_to_remove: Vec<String> = obj
                        .keys()
                        .filter(|k| !allowed_keys.contains(k.as_str()))
                        .cloned()
                        .collect();
                    if !keys_to_remove.is_empty() {
                        for k in keys_to_remove {
                            obj.remove(&k);
                        }
                        *repaired = true;
                    }
                }

                if let Some(required) = schema.get("required").and_then(Value::as_array) {
                    for key in required.iter().filter_map(Value::as_str) {
                        if !obj.contains_key(key) {
                            return Err(format!("{path}.{key} is required"));
                        }
                    }
                }
                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    for (key, child_schema) in properties {
                        if let Some(child_value) = obj.get_mut(key) {
                            validate_value(
                                child_schema,
                                child_value,
                                &format!("{path}.{key}"),
                                false,
                                repaired,
                            )?;
                        }
                    }
                }
            }
            "array" => {
                let Some(arr) = value.as_array_mut() else {
                    return Err(format!("{path} must be array"));
                };
                if let Some(items_schema) = schema.get("items") {
                    for (i, child_value) in arr.iter_mut().enumerate() {
                        validate_value(
                            items_schema,
                            child_value,
                            &format!("{path}[{i}]"),
                            false,
                            repaired,
                        )?;
                    }
                }
            }
            "string" if !value.is_string() => return Err(format!("{path} must be string")),
            "number" if !value.is_number() => return Err(format!("{path} must be number")),
            "integer" if !value.is_i64() && !value.is_u64() => {
                return Err(format!("{path} must be integer"));
            }
            "boolean" if !value.is_boolean() => return Err(format!("{path} must be boolean")),
            _ => {}
        }
    } else if is_root && !value.is_object() {
        return Err(format!("{path} must be object"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.contains(value) {
            return Err(format!("{path} must be one of enum values"));
        }
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
            results.push(tool_result(
                call.id.clone(),
                format!("unknown tool: {}", call.name),
                true,
            ));
            continue;
        };
        let mut call_args = call.arguments.clone();
        if let Err(e) = validate_tool_arguments(&tool.schema.parameters, &mut call_args) {
            results.push(tool_result(
                call.id.clone(),
                format!("invalid arguments: {e}"),
                true,
            ));
            continue;
        }
        let mut session = match (tool.start)(call_args).await {
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
                        results.push(tool_result(
                            call.id.clone(),
                            "tool suspended without resume handler".into(),
                            true,
                        ));
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

fn tool_result(
    call_id: compact_str::CompactString,
    output: String,
    is_error: bool,
) -> deepstrike_core::types::message::ToolResult {
    deepstrike_core::types::message::ToolResult {
        call_id,
        output: deepstrike_core::types::message::Content::Text(output),
        is_error,
        is_fatal: false,
        error_kind: None,
        token_count: None,
    }
}

// ── Structured tool envelope (safe_tool / ok / fail / tool_fail) ──────────────────────────────
//
// Opt-in: same shape as the Node/Python `safe_tool` + `ok()` / `fail()` envelope so a tool
// authored once produces consistent `{success, code?, error?, hint?}` JSON for the model across
// runtimes. The classic `RegisteredTool::text()` factory is unchanged.

#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum ToolEnvelope {
    Ok(ToolEnvelopeOk),
    Fail(ToolEnvelopeFail),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolEnvelopeOk {
    pub success: bool, // always true
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolEnvelopeFail {
    pub success: bool, // always false
    pub code: String,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub fn ok(data: impl Into<Option<Value>>) -> ToolEnvelope {
    ToolEnvelope::Ok(ToolEnvelopeOk { success: true, data: data.into() })
}

pub fn fail(code: impl Into<String>, error: impl Into<String>, hint: Option<String>) -> ToolEnvelope {
    ToolEnvelope::Fail(ToolEnvelopeFail {
        success: false,
        code: code.into(),
        error: error.into(),
        hint,
    })
}

/// Build a coded tool-failure `Error` (parity with Node `new ToolError(message, {code, hint})`).
/// Throwing this from a `safe_tool` body produces `{success:false, code, error, hint?}`; thrown
/// from a classic `tool()` body, the catch site formats it via `format_tool_error` as JSON.
pub fn tool_fail(message: impl Into<String>, code: Option<String>, hint: Option<String>) -> crate::Error {
    crate::Error::ToolFail {
        output: message.into(),
        code,
        hint,
        is_fatal: false,
        error_kind: None,
    }
}

/// `RegisteredTool::text` equivalent that wraps the body in a structured envelope. The body
/// returns either:
/// - `Ok(envelope)` produced by `ok(data)` / `fail(code, msg, hint)` — passed through
/// - `Ok(value)` of any other `Value` — auto-wrapped as `ok(value)`
/// - `Err(crate::Error::ToolFail{..})` — converted to `fail` envelope with the carried code/hint
/// - `Err(other)` — converted to `{success:false, code:"internal", error: format_tool_error(...)}`
pub fn safe_tool<F, Fut>(
    name: impl Into<compact_str::CompactString>,
    description: impl Into<String>,
    parameters: Value,
    f: F,
) -> RegisteredTool
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<SafeToolResult>> + Send + 'static,
{
    let f = Arc::new(f);
    RegisteredTool::text(name, description, parameters, move |args| {
        let f = Arc::clone(&f);
        Box::pin(async move {
            let envelope = match f(args).await {
                Ok(SafeToolResult::Envelope(env)) => env,
                Ok(SafeToolResult::Data(v)) => ok(Some(v)),
                Err(e) => {
                    let env = match &e {
                        crate::Error::ToolFail { output, code, hint, .. } => fail(
                            code.clone().unwrap_or_else(|| "internal".to_string()),
                            output.clone(),
                            hint.clone(),
                        ),
                        _ => fail("internal", crate::format_tool_error(&e), None),
                    };
                    env
                }
            };
            Ok(serde_json::to_string(&envelope).unwrap_or_else(|_| String::from(r#"{"success":false,"code":"internal","error":"envelope serialization failed"}"#)))
        })
    })
}

/// Return type for `safe_tool` bodies. Use `ok(...)` / `fail(...)` to build an explicit envelope,
/// or return `Data(Value)` to auto-wrap as `{success:true, data:value}`. `From<ToolEnvelope>` and
/// `From<Value>` impls let bodies just `return Ok(env.into())` / `return Ok(value.into())`.
#[derive(Debug, Clone)]
pub enum SafeToolResult {
    Envelope(ToolEnvelope),
    Data(Value),
}

impl From<ToolEnvelope> for SafeToolResult {
    fn from(e: ToolEnvelope) -> Self { Self::Envelope(e) }
}
impl From<Value> for SafeToolResult {
    fn from(v: Value) -> Self { Self::Data(v) }
}

pub fn read_file_tool() -> RegisteredTool {
    RegisteredTool::text(
        "read_file",
        "Read the contents of a file.",
        serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        |args| {
            Box::pin(async move {
                let path = args["path"]
                    .as_str()
                    .ok_or_else(|| Error::Tool("missing path".into()))?;
                tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| Error::Tool(e.to_string()))
            })
        },
    )
}
