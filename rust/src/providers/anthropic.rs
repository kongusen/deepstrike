use async_trait::async_trait;
use compact_str::CompactString;
use deepstrike_core::types::message::{Content, Message, Role, ToolCall, ToolSchema};
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use std::pin::Pin;

use super::{LLMProvider, StreamEvent};
use crate::{Error, Result};

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, "claude-sonnet-4-6")
    }

    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: 8096,
        }
    }
}

fn messages_to_anthropic(messages: &[Message]) -> (Option<String>, Vec<Value>) {
    let system = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| match &m.content { Content::Text(s) => s.clone(), _ => String::new() })
        .collect::<Vec<_>>()
        .join("\n\n");
    let msgs = messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| {
            let role = match m.role { Role::User => "user", _ => "assistant" };
            let content = match &m.content { Content::Text(s) => s.clone(), _ => String::new() };
            json!({ "role": role, "content": content })
        })
        .collect();
    (if system.is_empty() { None } else { Some(system) }, msgs)
}

fn tools_to_anthropic(tools: &[ToolSchema]) -> Vec<Value> {
    tools.iter().map(|t| json!({
        "name": t.name.as_str(),
        "description": t.description,
        "input_schema": t.parameters,
    })).collect()
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        extensions: Option<&Value>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>> {
        let (system, msgs) = messages_to_anthropic(messages);
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": msgs,
            "stream": true,
        });
        if let Some(s) = system { body["system"] = json!(s); }
        if !tools.is_empty() { body["tools"] = json!(tools_to_anthropic(tools)); }
        if let Some(ext) = extensions {
            if ext.get("enable_thinking").and_then(|v| v.as_bool()).unwrap_or(false) {
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": 8000 });
            }
        }

        let resp = self.client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| Error::Provider(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!("Anthropic {status}: {text}")));
        }

        let byte_stream = resp.bytes_stream();
        let stream = parse_anthropic_sse(byte_stream);
        Ok(Box::new(Box::pin(stream)))
    }
}

fn parse_anthropic_sse(
    byte_stream: impl Stream<Item = reqwest::Result<bytes::Bytes>> + Send + 'static,
) -> impl Stream<Item = Result<StreamEvent>> + Send {
    let mut buf = String::new();
    let mut tool_blocks: std::collections::HashMap<usize, (String, String, String)> = std::collections::HashMap::new();

    futures::stream::unfold(
        (Box::pin(byte_stream), buf, tool_blocks),
        |(mut stream, mut buf, mut tool_blocks)| async move {
            loop {
                // Try to parse buffered lines first
                if let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf = buf[pos + 1..].to_string();

                    if !line.starts_with("data: ") { continue; }
                    let data = &line[6..];
                    if data == "[DONE]" { return None; }

                    let Ok(evt) = serde_json::from_str::<Value>(data) else { continue };
                    let kind = evt["type"].as_str().unwrap_or("");

                    if kind == "content_block_start" {
                        let cb = &evt["content_block"];
                        if cb["type"] == "tool_use" {
                            let idx = evt["index"].as_u64().unwrap_or(0) as usize;
                            tool_blocks.insert(idx, (
                                cb["id"].as_str().unwrap_or("").to_string(),
                                cb["name"].as_str().unwrap_or("").to_string(),
                                String::new(),
                            ));
                        }
                    } else if kind == "content_block_delta" {
                        let d = &evt["delta"];
                        let idx = evt["index"].as_u64().unwrap_or(0) as usize;
                        if d["type"] == "text_delta" {
                            let delta = d["text"].as_str().unwrap_or("").to_string();
                            if !delta.is_empty() {
                                return Some((Ok(StreamEvent::TextDelta { delta }), (stream, buf, tool_blocks)));
                            }
                        } else if d["type"] == "thinking_delta" {
                            let delta = d["thinking"].as_str().unwrap_or("").to_string();
                            if !delta.is_empty() {
                                return Some((Ok(StreamEvent::ThinkingDelta { delta }), (stream, buf, tool_blocks)));
                            }
                        } else if d["type"] == "input_json_delta" {
                            if let Some(tb) = tool_blocks.get_mut(&idx) {
                                tb.2.push_str(d["partial_json"].as_str().unwrap_or(""));
                            }
                        }
                    } else if kind == "content_block_stop" {
                        let idx = evt["index"].as_u64().unwrap_or(0) as usize;
                        if let Some((id, name, args_buf)) = tool_blocks.remove(&idx) {
                            let arguments: Value = serde_json::from_str(&args_buf).unwrap_or(Value::Object(Default::default()));
                            return Some((Ok(StreamEvent::ToolCall { id, name, arguments }), (stream, buf, tool_blocks)));
                        }
                    }
                    continue;
                }

                // Need more data
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    Some(Err(e)) => {
                        return Some((Err(Error::Provider(e.to_string())), (stream, buf, tool_blocks)));
                    }
                    None => return None,
                }
            }
        },
    )
}
