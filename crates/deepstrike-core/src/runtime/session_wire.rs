//! The cross-SDK wire shape of [`SessionEvent`](super::SessionEvent) fields whose in-memory Rust
//! type differs from the shared registry (`contracts/session-events.json`):
//!
//! - tool calls carry `arguments` as a JSON-encoded string (Node/Python/WASM `ToolCall`);
//! - tool results carry a text `output` plus mandatory portable `content` blocks;
//! - `llm_completed` carries the assistant text as `content` next to `tool_calls`.
//!
//! [`upgrade_legacy`] rewrites events persisted by the Rust SDK before the registry into that
//! shape, so old logs keep replaying.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::types::durable_content::DurableContent;
use crate::types::message::{
    Content, ContentPart, CoreMessage, Role, ToolCall, ToolErrorKind, ToolResult,
};

/// The text projection of a message / tool-result body.
fn content_text(content: &Content) -> String {
    match content {
        Content::Text(text) => text.clone(),
        Content::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ToolResult { output, .. } => Some(output.as_str()),
                _ => None,
            })
            .collect(),
    }
}

#[derive(Serialize, Deserialize)]
struct WireToolCall {
    id: String,
    name: String,
    /// JSON-encoded arguments.
    arguments: String,
}

impl From<&ToolCall> for WireToolCall {
    fn from(call: &ToolCall) -> Self {
        Self {
            id: call.id.to_string(),
            name: call.name.to_string(),
            arguments: call.arguments.to_string(),
        }
    }
}

impl WireToolCall {
    fn into_call(self) -> ToolCall {
        // A provider may hand back arguments that are not valid JSON; keep them as a string
        // rather than failing the whole replay.
        let arguments =
            serde_json::from_str(&self.arguments).unwrap_or(Value::String(self.arguments));
        ToolCall {
            id: self.id.into(),
            name: self.name.into(),
            arguments,
        }
    }
}

pub mod tool_calls {
    use super::*;

    pub fn serialize<S: Serializer>(calls: &[ToolCall], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(calls.iter().map(WireToolCall::from))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<ToolCall>, D::Error> {
        let calls = Vec::<WireToolCall>::deserialize(deserializer)?;
        Ok(calls.into_iter().map(WireToolCall::into_call).collect())
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize, Deserialize)]
struct WireToolResult {
    call_id: String,
    output: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    is_fatal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_kind: Option<ToolErrorKind>,
    /// Advisory token count other SDKs record; Rust keeps none in memory.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    token_count: Option<u64>,
    content: DurableContent,
}

pub mod tool_results {
    use super::*;

    pub fn serialize<S: Serializer>(
        results: &[ToolResult],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(results.iter().map(|result| {
            let output = content_text(&result.output);
            WireToolResult {
                call_id: result.call_id.to_string(),
                content: result
                    .durable_content
                    .clone()
                    .unwrap_or_else(|| DurableContent::text(output.clone())),
                output,
                is_error: result.is_error,
                is_fatal: result.is_fatal,
                error_kind: result.error_kind,
                token_count: None,
            }
        }))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<ToolResult>, D::Error> {
        let results = Vec::<WireToolResult>::deserialize(deserializer)?;
        results
            .into_iter()
            .map(|result| {
                result
                    .content
                    .validate()
                    .map_err(serde::de::Error::custom)?;
                Ok(ToolResult {
                    call_id: result.call_id.into(),
                    output: Content::Text(result.output),
                    durable_content: Some(result.content),
                    is_error: result.is_error,
                    is_fatal: result.is_fatal,
                    error_kind: result.error_kind,
                })
            })
            .collect()
    }
}

/// `llm_completed`'s assistant message, flattened into the event as `content` + `tool_calls`.
pub mod llm_message {
    use super::*;

    #[derive(Serialize, Deserialize)]
    struct WireLlmMessage {
        content: String,
        #[serde(with = "super::tool_calls")]
        tool_calls: Vec<ToolCall>,
    }

    pub fn serialize<S: Serializer>(
        message: &CoreMessage,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        WireLlmMessage {
            content: content_text(&message.content),
            tool_calls: message.tool_calls.clone(),
        }
        .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<CoreMessage, D::Error> {
        let wire = WireLlmMessage::deserialize(deserializer)?;
        Ok(CoreMessage {
            role: Role::Assistant,
            content: Content::Text(wire.content),
            tool_calls: wire.tool_calls,
        })
    }
}

/// Rewrite an event persisted by the pre-registry Rust SDK into the shared wire shape. Events
/// already in that shape pass through unchanged.
///
/// - `llm_completed { message, provider_replay }` → `content` + `tool_calls` +
///   `wire_evidence.replay_state` (no request fingerprint: those logs never recorded one);
/// - `tool_requested` / `llm_completed` tool calls with object arguments → JSON-encoded string;
/// - `tool_completed` results `{ output: Content, durable_content }` → text `output` + `content`;
/// - `workflow_batch_spawned { nodes }` → `node_count` + `node_ids`;
/// - `workflow_completed` without `total_nodes` → counted from `node_outcomes`;
/// - `workflow_spawn_failed` / `workflow_nodes_submitted` (which the Rust SDK has no driver for)
///   → `kernel_observation` envelopes, as the shared projection writes them.
pub fn upgrade_legacy(value: &mut Value) {
    let Some(event) = value.as_object_mut() else {
        return;
    };
    match event.get("kind").and_then(Value::as_str) {
        Some("llm_completed") => upgrade_llm_completed(event),
        Some("tool_requested") => {
            if let Some(calls) = event.get_mut("calls") {
                upgrade_tool_calls(calls);
            }
        }
        Some("tool_completed") => {
            if let Some(Value::Array(results)) = event.get_mut("results") {
                results.iter_mut().for_each(upgrade_tool_result);
            }
        }
        Some("workflow_batch_spawned") => {
            if let Some(Value::Array(nodes)) = event.remove("nodes") {
                let ids = nodes
                    .iter()
                    .map(|node| {
                        node.get("agent_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into()
                    })
                    .collect::<Vec<Value>>();
                event.insert("node_count".into(), ids.len().into());
                event.insert("node_ids".into(), Value::Array(ids));
            }
        }
        Some("workflow_completed") if !event.contains_key("total_nodes") => {
            let total = event
                .get("node_outcomes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            event.insert("total_nodes".into(), total.into());
        }
        Some(kind @ ("workflow_spawn_failed" | "workflow_nodes_submitted")) => {
            let kind = kind.to_string();
            let raw = Value::Object(event.clone());
            let turn = event.get("turn").cloned().unwrap_or(Value::from(0));
            *value = serde_json::json!({
                "kind": "kernel_observation",
                "turn": turn,
                "observation_kind": kind,
                "raw": raw,
            });
        }
        _ => {}
    }
}

fn upgrade_llm_completed(event: &mut Map<String, Value>) {
    if let Some(Value::Object(mut message)) = event.remove("message") {
        let content = match message.remove("content") {
            Some(Value::String(text)) => text,
            Some(other) => serde_json::from_value::<Content>(other)
                .map(|content| content_text(&content))
                .unwrap_or_default(),
            None => String::new(),
        };
        event.insert("content".into(), Value::String(content));
        event.insert(
            "tool_calls".into(),
            message
                .remove("tool_calls")
                .unwrap_or(Value::Array(Vec::new())),
        );
    }
    if let Some(calls) = event.get_mut("tool_calls") {
        upgrade_tool_calls(calls);
    }
    if let Some(replay) = event.remove("provider_replay") {
        if !replay.is_null() && !event.contains_key("wire_evidence") {
            let protocol = replay
                .get("protocol")
                .cloned()
                .unwrap_or(Value::String(String::new()));
            event.insert(
                "wire_evidence".into(),
                serde_json::json!({ "protocol": protocol, "replay_state": replay }),
            );
        }
    }
}

fn upgrade_tool_calls(calls: &mut Value) {
    let Value::Array(calls) = calls else {
        return;
    };
    for call in calls {
        if let Some(arguments) = call.get_mut("arguments") {
            if !arguments.is_string() {
                *arguments = Value::String(arguments.to_string());
            }
        }
    }
}

fn upgrade_tool_result(result: &mut Value) {
    let Some(result) = result.as_object_mut() else {
        return;
    };
    if let Some(output) = result.get_mut("output") {
        if !output.is_string() {
            let text = serde_json::from_value::<Content>(output.take())
                .map(|content| content_text(&content))
                .unwrap_or_default();
            *output = Value::String(text);
        }
    }
    if let Some(durable) = result.remove("durable_content") {
        if !durable.is_null() && !result.contains_key("content") {
            result.insert("content".into(), durable);
        }
    }
    if !result.contains_key("content") {
        let text = result
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        result.insert(
            "content".into(),
            serde_json::to_value(DurableContent::text(text)).unwrap_or(Value::Null),
        );
    }
}
