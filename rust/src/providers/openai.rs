use async_trait::async_trait;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::types::message::{Content, ContentPart, Role, ToolSchema};
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};

use super::{LLMProvider, RuntimePolicy, StreamEvent};
use crate::{Error, Result};

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, "gpt-4o", "https://api.openai.com/v1")
    }

    pub fn with_base_url(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            model: model.into(),
            base_url: base_url.into(),
        }
    }
}

pub fn qwen(api_key: impl Into<String>) -> OpenAIProvider {
    OpenAIProvider::with_base_url(
        api_key,
        "qwen-max",
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
    )
}

pub fn deepseek(api_key: impl Into<String>) -> OpenAIProvider {
    OpenAIProvider::with_base_url(api_key, "deepseek-chat", "https://api.deepseek.com/v1")
}

pub fn minimax(api_key: impl Into<String>) -> OpenAIProvider {
    OpenAIProvider::with_base_url(api_key, "MiniMax-Text-01", "https://api.minimax.chat/v1")
}

pub fn ollama(model: impl Into<String>) -> OpenAIProvider {
    OpenAIProvider::with_base_url("", model, "http://localhost:11434/v1")
}

pub fn kimi(api_key: impl Into<String>) -> OpenAIProvider {
    OpenAIProvider::with_base_url(api_key, "moonshot-v1-8k", "https://api.moonshot.cn/v1")
}

fn content_part_to_openai(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({ "type": "text", "text": text }),
        ContentPart::Image {
            url: Some(url),
            data: None,
            detail,
            ..
        } => {
            let image_url = match detail.as_deref() {
                Some(d) => json!({ "url": url, "detail": d }),
                None => json!({ "url": url }),
            };
            json!({ "type": "image_url", "image_url": image_url })
        }
        ContentPart::Image {
            data: Some(data),
            media_type,
            detail,
            ..
        } => {
            let mt = media_type.as_deref().unwrap_or("image/png");
            let url = format!("data:{mt};base64,{data}");
            let image_url = match detail.as_deref() {
                Some(d) => json!({ "url": url, "detail": d }),
                None => json!({ "url": url }),
            };
            json!({ "type": "image_url", "image_url": image_url })
        }
        ContentPart::Image { .. } => json!({ "type": "text", "text": "" }),
        ContentPart::Audio { data, media_type } => {
            let fmt = media_type.split('/').nth(1).unwrap_or("wav");
            json!({ "type": "input_audio", "input_audio": { "data": data, "format": fmt } })
        }
        ContentPart::ToolResult { output, .. } => {
            json!({ "type": "text", "text": output })
        }
    }
}

fn content_to_openai(content: &Content) -> Value {
    match content {
        Content::Text(s) => json!(s),
        Content::Parts(parts) => {
            let blocks: Vec<Value> = parts.iter().map(content_part_to_openai).collect();
            json!(blocks)
        }
    }
}

fn context_to_openai(context: &RenderedContext) -> Vec<Value> {
    let mut messages = Vec::new();
    if !context.system_text.is_empty() {
        messages.push(json!({ "role": "system", "content": context.system_text }));
    }
    for message in &context.turns {
        if message.role == Role::Tool {
            if let Content::Parts(parts) = &message.content {
                for part in parts {
                    if let ContentPart::ToolResult {
                        call_id, output, ..
                    } = part
                    {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id.as_str(),
                            "content": output,
                        }));
                    }
                }
            }
            continue;
        }

        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Tool => "tool",
            Role::Assistant => "assistant",
        };
        let mut next = json!({
            "role": role,
            "content": content_to_openai(&message.content),
        });
        if message.role == Role::Assistant && !message.tool_calls.is_empty() {
            next["tool_calls"] = json!(
                message
                    .tool_calls
                    .iter()
                    .map(|tc| json!({
                        "id": tc.id.as_str(),
                        "type": "function",
                        "function": {
                            "name": tc.name.as_str(),
                            "arguments": tc.arguments.to_string(),
                        }
                    }))
                    .collect::<Vec<_>>()
            );
        }
        messages.push(next);
    }
    messages
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    fn runtime_policy(&self) -> RuntimePolicy {
        match self.model.as_str() {
            // OpenAI
            "gpt-4o" => RuntimePolicy {
                max_turns: Some(25),
                timeout_ms: None,
            },
            "gpt-4o-mini" => RuntimePolicy {
                max_turns: Some(15),
                timeout_ms: None,
            },
            "gpt-4.1" => RuntimePolicy {
                max_turns: Some(35),
                timeout_ms: None,
            },
            "gpt-4.1-mini" => RuntimePolicy {
                max_turns: Some(20),
                timeout_ms: None,
            },
            "gpt-4.1-nano" => RuntimePolicy {
                max_turns: Some(15),
                timeout_ms: None,
            },
            "gpt-5" => RuntimePolicy {
                max_turns: Some(50),
                timeout_ms: None,
            },
            "gpt-5-mini" => RuntimePolicy {
                max_turns: Some(25),
                timeout_ms: None,
            },
            "o3" | "o3-mini" | "o4-mini" => RuntimePolicy {
                max_turns: Some(50),
                timeout_ms: None,
            },
            // DeepSeek
            "deepseek-chat" | "deepseek-v4-flash" => RuntimePolicy {
                max_turns: Some(25),
                timeout_ms: None,
            },
            "deepseek-reasoner" | "deepseek-r1" => RuntimePolicy {
                max_turns: Some(50),
                timeout_ms: None,
            },
            "deepseek-v4-pro" => RuntimePolicy {
                max_turns: Some(35),
                timeout_ms: None,
            },
            // Qwen
            "qwen-max" => RuntimePolicy {
                max_turns: Some(25),
                timeout_ms: None,
            },
            "qwen-plus" => RuntimePolicy {
                max_turns: Some(20),
                timeout_ms: None,
            },
            "qwq-plus" | "qwq-32b" => RuntimePolicy {
                max_turns: Some(40),
                timeout_ms: None,
            },
            "qwen3-235b-a22b" => RuntimePolicy {
                max_turns: Some(35),
                timeout_ms: None,
            },
            "qwen3-72b" => RuntimePolicy {
                max_turns: Some(25),
                timeout_ms: None,
            },
            "qwen3-32b" | "qwen3-14b" | "qwen3-8b" => RuntimePolicy {
                max_turns: Some(20),
                timeout_ms: None,
            },
            // Kimi (Moonshot)
            "moonshot-v1-8k" => RuntimePolicy {
                max_turns: Some(15),
                timeout_ms: None,
            },
            "moonshot-v1-32k" => RuntimePolicy {
                max_turns: Some(20),
                timeout_ms: None,
            },
            "moonshot-v1-128k" | "kimi-k2.5" => RuntimePolicy {
                max_turns: Some(30),
                timeout_ms: None,
            },
            "kimi-k2.6" => RuntimePolicy {
                max_turns: Some(35),
                timeout_ms: None,
            },
            // MiniMax
            "MiniMax-M2.7" => RuntimePolicy {
                max_turns: Some(35),
                timeout_ms: None,
            },
            "MiniMax-M2.5" | "MiniMax-M1" => RuntimePolicy {
                max_turns: Some(25),
                timeout_ms: None,
            },
            "MiniMax-Text-01" => RuntimePolicy {
                max_turns: Some(20),
                timeout_ms: None,
            },
            // Ollama prefix matching
            m if m.starts_with("deepseek-r1") => RuntimePolicy {
                max_turns: Some(40),
                timeout_ms: None,
            },
            m if m.starts_with("qwq") => RuntimePolicy {
                max_turns: Some(35),
                timeout_ms: None,
            },
            m if m.starts_with("llama3") => RuntimePolicy {
                max_turns: Some(20),
                timeout_ms: None,
            },
            m if m.starts_with("mistral") || m.starts_with("gemma") || m.starts_with("phi") => {
                RuntimePolicy {
                    max_turns: Some(20),
                    timeout_ms: None,
                }
            }
            _ => RuntimePolicy {
                max_turns: Some(20),
                timeout_ms: None,
            },
        }
    }

    async fn stream(
        &self,
        context: &RenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&Value>,
        _state: Option<&super::ProviderRunState>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>> {
        let mut body = json!({
            "model": self.model,
            "messages": context_to_openai(context),
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools.iter().map(|t| json!({
                "type": "function",
                "function": { "name": t.name.as_str(), "description": t.description, "parameters": t.parameters }
            })).collect::<Vec<_>>());
        }
        let mut expose_reasoning = false;
        if let Some(ext) = extensions {
            if let Some(obj) = ext.as_object() {
                for (k, v) in obj {
                    if k == "expose_reasoning" {
                        expose_reasoning = v.as_bool().unwrap_or(false);
                    } else {
                        body[k] = v.clone();
                    }
                }
            }
        }

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| Error::Provider(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!("OpenAI {status}: {text}")));
        }

        let byte_stream = resp.bytes_stream();
        let stream = parse_openai_sse(byte_stream, expose_reasoning);
        Ok(Box::new(Box::pin(stream)))
    }
}

fn parse_openai_sse(
    byte_stream: impl Stream<Item = reqwest::Result<bytes::Bytes>> + Send + 'static,
    expose_reasoning: bool,
) -> impl Stream<Item = Result<StreamEvent>> + Send {
    let tool_accum: std::collections::HashMap<usize, (String, String, String)> =
        std::collections::HashMap::new();

    futures::stream::unfold(
        (Box::pin(byte_stream), String::new(), tool_accum, false),
        move |(mut stream, mut buf, mut tool_accum, mut flushed)| async move {
            if flushed {
                return None;
            }
            loop {
                if let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf = buf[pos + 1..].to_string();

                    if !line.starts_with("data: ") {
                        continue;
                    }
                    let data = &line[6..];
                    if data == "[DONE]" {
                        // flush accumulated tool calls
                        if let Some((_, (id, name, args_buf))) = tool_accum.iter().next() {
                            let arguments: Value = serde_json::from_str(args_buf)
                                .unwrap_or(Value::Object(Default::default()));
                            let evt = StreamEvent::ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                arguments,
                            };
                            flushed = true;
                            return Some((Ok(evt), (stream, buf, tool_accum, flushed)));
                        }
                        return None;
                    }

                    let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };
                    if let Some(total) = chunk["usage"]["total_tokens"].as_u64() {
                        return Some((
                            Ok(StreamEvent::Usage {
                                total_tokens: total as u32,
                            }),
                            (stream, buf, tool_accum, flushed),
                        ));
                    }
                    let delta = &chunk["choices"][0]["delta"];
                    if expose_reasoning {
                        if let Some(reasoning) = delta["reasoning_content"].as_str() {
                            if !reasoning.is_empty() {
                                return Some((
                                    Ok(StreamEvent::ThinkingDelta {
                                        delta: reasoning.to_string(),
                                    }),
                                    (stream, buf, tool_accum, flushed),
                                ));
                            }
                        }
                    }
                    if let Some(content) = delta["content"].as_str() {
                        if !content.is_empty() {
                            return Some((
                                Ok(StreamEvent::TextDelta {
                                    delta: content.to_string(),
                                }),
                                (stream, buf, tool_accum, flushed),
                            ));
                        }
                    }
                    if let Some(tcs) = delta["tool_calls"].as_array() {
                        for tc in tcs {
                            let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                            let entry = tool_accum.entry(idx).or_insert_with(|| {
                                (
                                    tc["id"].as_str().unwrap_or("").to_string(),
                                    tc["function"]["name"].as_str().unwrap_or("").to_string(),
                                    String::new(),
                                )
                            });
                            entry
                                .2
                                .push_str(tc["function"]["arguments"].as_str().unwrap_or(""));
                        }
                    }
                    continue;
                }

                match stream.next().await {
                    Some(Ok(chunk)) => buf.push_str(&String::from_utf8_lossy(&chunk)),
                    Some(Err(e)) => {
                        return Some((
                            Err(Error::Provider(e.to_string())),
                            (stream, buf, tool_accum, flushed),
                        ));
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
    fn context_replays_tool_calls_and_results_natively() {
        let context = RenderedContext {
            system_text: "system rules".into(),
            system_stable: "system rules".into(),
            system_knowledge: String::new(),
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

        assert_eq!(
            context_to_openai(&context),
            vec![
                json!({ "role": "system", "content": "system rules" }),
                json!({ "role": "user", "content": "What is the weather?" }),
                json!({
                    "role": "assistant",
                    "content": "I'll check.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Shanghai\"}",
                        }
                    }],
                }),
                json!({ "role": "tool", "tool_call_id": "call_1", "content": "sunny" }),
            ]
        );
    }
}
