use async_trait::async_trait;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::types::message::{Content, ContentPart, Role, ToolSchema};
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};

use super::{LLMProvider, RuntimePolicy, StreamEvent};
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

fn content_part_to_anthropic(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({ "type": "text", "text": text }),
        ContentPart::Image { url: Some(url), data: None, .. } => {
            json!({ "type": "image", "source": { "type": "url", "url": url } })
        }
        ContentPart::Image { data: Some(data), media_type, .. } => {
            let mt = media_type.as_deref().unwrap_or("image/png");
            json!({ "type": "image", "source": { "type": "base64", "media_type": mt, "data": data } })
        }
        ContentPart::Image { .. } => json!({ "type": "text", "text": "" }),
        ContentPart::Audio { media_type, .. } => {
            // Anthropic messages API doesn't accept audio natively; surface as placeholder
            json!({ "type": "text", "text": format!("[audio: {media_type}]") })
        }
        ContentPart::ToolResult { call_id, output, is_error } => {
            json!({ "type": "tool_result", "tool_use_id": call_id.as_str(), "content": output, "is_error": is_error })
        }
    }
}

fn content_to_anthropic(content: &Content) -> Value {
    match content {
        Content::Text(s) => json!(s),
        Content::Parts(parts) => {
            let blocks: Vec<Value> = parts.iter().map(content_part_to_anthropic).collect();
            json!(blocks)
        }
    }
}

fn context_to_anthropic(context: &RenderedContext) -> (Option<String>, Vec<Value>) {
    let mut msgs = Vec::new();
    for message in &context.turns {
        if message.role == Role::Tool {
            if let Content::Parts(parts) = &message.content {
                let tool_results = parts.iter().filter_map(|part| {
                    if let ContentPart::ToolResult { call_id, output, is_error } = part {
                        Some(json!({
                            "type": "tool_result",
                            "tool_use_id": call_id.as_str(),
                            "content": output,
                            "is_error": is_error,
                        }))
                    } else {
                        None
                    }
                }).collect::<Vec<_>>();
                if !tool_results.is_empty() {
                    msgs.push(json!({ "role": "user", "content": tool_results }));
                }
            }
            continue;
        }

        if message.role == Role::Assistant && !message.tool_calls.is_empty() {
            let mut blocks = Vec::new();
            if let Some(text) = message.content.as_text() {
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
            }
            blocks.extend(message.tool_calls.iter().map(|tc| json!({
                "type": "tool_use",
                "id": tc.id.as_str(),
                "name": tc.name.as_str(),
                "input": tc.arguments.clone(),
            })));
            msgs.push(json!({ "role": "assistant", "content": blocks }));
            continue;
        }

        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "assistant",
            Role::Tool => unreachable!(),
        };
        msgs.push(json!({ "role": role, "content": content_to_anthropic(&message.content) }));
    }
    (
        if context.system_text.is_empty() { None } else { Some(context.system_text.clone()) },
        msgs,
    )
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
    fn runtime_policy(&self) -> RuntimePolicy {
        match self.model.as_str() {
            "claude-opus-4-7" | "claude-opus-4-6" => RuntimePolicy { max_turns: Some(50), timeout_ms: None },
            "claude-sonnet-4-6" => RuntimePolicy { max_turns: Some(25), timeout_ms: None },
            "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => RuntimePolicy { max_turns: Some(15), timeout_ms: None },
            _ => RuntimePolicy::default(),
        }
    }

    async fn stream(
        &self,
        context: &RenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&Value>,
        _state: Option<&super::ProviderRunState>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>> {
        let (system, msgs) = context_to_anthropic(context);
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

fn anthropic_usage_total(usage: &Value) -> Option<u32> {
    let input = usage.get("input_tokens")?.as_u64()?;
    let output = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Some((input + output) as u32)
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
                    } else if kind == "message_start" || kind == "message_delta" {
                        if let Some(total) = evt
                            .get("usage")
                            .and_then(anthropic_usage_total)
                            .or_else(|| {
                                evt.get("message")
                                    .and_then(|m| m.get("usage"))
                                    .and_then(anthropic_usage_total)
                            })
                        {
                            return Some((
                                Ok(StreamEvent::Usage { total_tokens: total }),
                                (stream, buf, tool_blocks),
                            ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;
    use deepstrike_core::types::message::{ContentPart, Message, ToolCall};

    #[test]
    fn anthropic_usage_total_sums_input_and_output() {
        let usage = json!({ "input_tokens": 100, "output_tokens": 50 });
        assert_eq!(anthropic_usage_total(&usage), Some(150));
    }

    #[test]
    fn context_replays_tool_calls_and_results_as_blocks() {
        let context = RenderedContext {
            system_text: "system rules".into(),
            turns: vec![
                Message::user("What is the weather?"),
                Message {
                    role: Role::Assistant,
                    content: Content::Text("I'll check.".into()),
                    tool_calls: vec![ToolCall {
                        id: CompactString::new("call_1"),
                        name: CompactString::new("get_weather"),
                        arguments: json!({ "city": "Shanghai" }),
                    }],
                    token_count: None,
                },
                Message::tool(vec![ContentPart::ToolResult {
                    call_id: CompactString::new("call_1"),
                    output: "sunny".into(),
                    is_error: false,
                }]),
            ],
        };

        let (system, messages) = context_to_anthropic(&context);
        assert_eq!(system.as_deref(), Some("system rules"));
        assert_eq!(
            messages,
            vec![
                json!({ "role": "user", "content": "What is the weather?" }),
                json!({
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "I'll check." },
                        {
                            "type": "tool_use",
                            "id": "call_1",
                            "name": "get_weather",
                            "input": { "city": "Shanghai" },
                        },
                    ],
                }),
                json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call_1",
                        "content": "sunny",
                        "is_error": false,
                    }],
                }),
            ]
        );
    }
}
