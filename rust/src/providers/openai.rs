use async_trait::async_trait;
use deepstrike_core::context::renderer::InternalRenderedContext;
use deepstrike_core::types::message::{Content, ContentPart, Role, ToolSchema};
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};

use super::{LLMProvider, RuntimePolicy, StreamEvent};
use crate::{Error, Result};

/// Cached-prompt-token count from an OpenAI-compatible usage object: the standard
/// `prompt_tokens_details.cached_tokens` (OpenAI, Qwen, MiniMax, GLM, Kimi) and
/// DeepSeek's `prompt_cache_hit_tokens`. These caches bill reads only (no
/// cache-creation count); the figure is a subset of `prompt_tokens`.
fn openai_cached_prompt_tokens(usage: &Value) -> u32 {
    let standard = usage["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    let deepseek = usage["prompt_cache_hit_tokens"].as_u64().unwrap_or(0);
    standard.max(deepseek) as u32
}

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
    /// Plan task 9.0 · DeepSeek models may write tool calls as DSML markup in the text.
    text_tool_call_markup: bool,
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
        let model = model.into();
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            text_tool_call_markup: model.starts_with("deepseek"),
            model,
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

/// Map an audio MIME type to OpenAI's `input_audio.format` (accepts "mp3" | "wav").
/// `audio/mpeg` must become "mp3", not the raw "mpeg" subtype.
fn openai_audio_format(media_type: &str) -> &str {
    match media_type.split('/').nth(1).unwrap_or("wav") {
        "mpeg" | "mp3" => "mp3",
        "wav" | "wave" | "x-wav" => "wav",
        other => other,
    }
}

fn content_part_to_openai(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({ "type": "text", "text": text }),
        ContentPart::Image {
            source: deepstrike_core::types::durable_content::DurableSource::Url { url },
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
            source: deepstrike_core::types::durable_content::DurableSource::Base64 { data },
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
        ContentPart::Audio {
            source: deepstrike_core::types::durable_content::DurableSource::Base64 { data },
            media_type,
        } => {
            json!({ "type": "input_audio", "input_audio": { "data": data, "format": openai_audio_format(media_type) } })
        }
        ContentPart::Audio { .. } => json!({ "type": "text", "text": "" }),
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

fn context_to_openai(context: &InternalRenderedContext) -> Vec<Value> {
    let mut messages = Vec::new();
    if !context.system_text.is_empty() {
        messages.push(json!({ "role": "system", "content": context.system_text }));
    }
    // OpenAI auto-caches by prefix; the volatile State turn is appended as the
    // latest turn so the history stays a stable cacheable prefix. `state_turn` is
    // None on un-rebuilt bindings, where the state is still inside `turns`.
    for message in context.turns.iter().chain(context.state_turn.iter()) {
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
    fn context_route(&self) -> Value {
        let endpoint = reqwest::Url::parse(&self.base_url).ok().map(|mut url| {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        });
        json!({ "protocol": "openai-chat", "model": self.model, "endpoint": endpoint })
    }

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

    fn prepare_context_request(
        &self,
        context: &InternalRenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&Value>,
        _state: Option<&super::ProviderRunState>,
    ) -> Result<Value> {
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

        Ok(json!({ "scope": "encoded_body", "body": body, "expose_reasoning": expose_reasoning }))
    }

    async fn stream(
        &self,
        context: &InternalRenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&Value>,
        state: Option<&super::ProviderRunState>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>> {
        let prepared = self.prepare_context_request(context, tools, extensions, state)?;
        self.stream_prepared(&prepared, context, tools, extensions, state)
            .await
    }

    async fn stream_prepared(
        &self,
        prepared: &Value,
        _context: &InternalRenderedContext,
        _tools: &[ToolSchema],
        _extensions: Option<&Value>,
        _state: Option<&super::ProviderRunState>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>> {
        let body = prepared
            .get("body")
            .filter(|body| body.is_object())
            .ok_or_else(|| {
                Error::Other("prepared provider request must contain an encoded body".into())
            })?;
        let expose_reasoning = prepared["expose_reasoning"].as_bool().unwrap_or(false);

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| Error::from(super::ProviderError::transport("openai", e.to_string())))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(super::ProviderError::from_http("openai", status, text).into());
        }

        let byte_stream = resp.bytes_stream();
        let stream = parse_openai_sse(byte_stream, expose_reasoning, self.text_tool_call_markup);
        Ok(Box::new(Box::pin(stream)))
    }
}

fn parse_openai_sse(
    byte_stream: impl Stream<Item = reqwest::Result<bytes::Bytes>> + Send + 'static,
    expose_reasoning: bool,
    text_tool_call_markup: bool,
) -> impl Stream<Item = Result<StreamEvent>> + Send {
    async_stream::stream! {
        let mut byte_stream = Box::pin(byte_stream);
        // Raw bytes until a full line arrives: a multi-byte character (`｜`, CJK text) split across
        // network chunks must not be decoded half at a time.
        let mut pending: Vec<u8> = Vec::new();
        // Ordered by index so parallel calls come out in the order the model made them.
        let mut tool_accum: std::collections::BTreeMap<usize, (String, String, String)> =
            std::collections::BTreeMap::new();
        // The last finish_reason seen — "length" flags an output-cap truncation; it arrives on a
        // choices frame before the trailing usage frame and is attached to the Usage event.
        let mut finish_reason: Option<String> = None;
        let mut usage: Option<Value> = None;
        let mut text = super::text_tool_calls::TextToolCallFilter::new(text_tool_call_markup);
        'read: loop {
            while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                let line_bytes: Vec<u8> = pending.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                if data == "[DONE]" {
                    break 'read;
                }
                let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(fr) = chunk["choices"][0]["finish_reason"].as_str() {
                    finish_reason = Some(fr.to_string());
                }
                if chunk["usage"]["total_tokens"].as_u64().is_some() {
                    usage = Some(chunk["usage"].clone());
                    continue;
                }
                let delta = &chunk["choices"][0]["delta"];
                if expose_reasoning {
                    if let Some(reasoning) = delta["reasoning_content"].as_str() {
                        if !reasoning.is_empty() {
                            yield Ok(StreamEvent::ThinkingDelta { delta: reasoning.to_string() });
                        }
                    }
                }
                if let Some(content) = delta["content"].as_str() {
                    let visible = text.feed(content);
                    if !visible.is_empty() {
                        yield Ok(StreamEvent::TextDelta { delta: visible });
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
                        entry.2.push_str(tc["function"]["arguments"].as_str().unwrap_or(""));
                    }
                }
            }
            match byte_stream.next().await {
                Some(Ok(chunk)) => pending.extend_from_slice(&chunk),
                Some(Err(e)) => {
                    yield Err(super::ProviderError::transport("openai", e.to_string()).into());
                    return;
                }
                None => break 'read,
            }
        }
        // Plan task 9.0 · markup written as text becomes tool calls, or the call fails.
        match text.finish() {
            Ok((held, calls)) => {
                if !held.is_empty() {
                    yield Ok(StreamEvent::TextDelta { delta: held });
                }
                if !calls.is_empty() {
                    let mut index = tool_accum.keys().next_back().map_or(0, |last| last + 1);
                    for call in calls {
                        let args = Value::Object(call.arguments).to_string();
                        tool_accum.insert(index, (super::text_tool_calls::text_tool_call_id(), call.name, args));
                        index += 1;
                    }
                    finish_reason = Some("tool_calls".to_string());
                }
            }
            Err(()) => {
                yield Err(super::text_tool_calls::textual_tool_call_error("openai").into());
                return;
            }
        }
        for (_, (id, name, args_buf)) in tool_accum {
            let arguments = crate::runtime::tool_arguments::from_model_text(&args_buf);
            yield Ok(StreamEvent::ToolCall { id, name, arguments });
        }
        if let Some(usage) = usage {
            yield Ok(StreamEvent::Usage {
                total_tokens: usage["total_tokens"].as_u64().unwrap_or(0) as u32,
                input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
                cache_read_input_tokens: openai_cached_prompt_tokens(&usage),
                cache_creation_input_tokens: 0,
                // I1: OpenAI-family providers auto-cache; no per-slot attribution.
                cache_read_input_tokens_by_slot: None,
                // finish_reason="length" flags an output-cap truncation and drives the kernel's
                // recovery.
                stop_reason: finish_reason.clone(),
            });
        }
    }
}

#[cfg(test)]
mod text_tool_call_tests {
    //! Plan task 9.0 · DeepSeek tool-call markup written as text, against the fixture every SDK
    //! shares. Mirrors node/tests/text-tool-calls.test.ts.
    use super::*;

    fn fixture() -> Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/provider-textual-tool-call/openai-chat.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn sse(chunks: &[Value]) -> Vec<u8> {
        let mut out = String::new();
        for content in chunks {
            out.push_str(&format!("data: {}\n\n", json!({ "choices": [{ "index": 0, "delta": { "content": content } }] })));
        }
        out.push_str(&format!("data: {}\n\n", json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] })));
        out.push_str(&format!("data: {}\n\n", json!({ "choices": [], "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 } })));
        out.push_str("data: [DONE]\n\n");
        out.into_bytes()
    }

    /// Feeds `bytes` in 7-byte pieces, so multi-byte characters are split across network chunks.
    async fn run(bytes: Vec<u8>, markup: bool) -> Vec<Result<StreamEvent>> {
        let pieces: Vec<reqwest::Result<bytes::Bytes>> =
            bytes.chunks(7).map(|piece| Ok(bytes::Bytes::copy_from_slice(piece))).collect();
        parse_openai_sse(futures::stream::iter(pieces), false, markup).collect().await
    }

    fn text(events: &[Result<StreamEvent>]) -> String {
        events.iter().filter_map(|event| match event {
            Ok(StreamEvent::TextDelta { delta }) => Some(delta.as_str()),
            _ => None,
        }).collect()
    }

    fn calls(events: &[Result<StreamEvent>]) -> Vec<(String, String, Value)> {
        events.iter().filter_map(|event| match event {
            Ok(StreamEvent::ToolCall { id, name, arguments }) => Some((id.clone(), name.clone(), serde_json::to_value(arguments).unwrap())),
            _ => None,
        }).collect()
    }

    #[tokio::test]
    async fn markup_becomes_tool_calls_and_never_reaches_the_text() {
        for case in fixture()["cases"].as_array().unwrap().iter().filter(|c| c.get("error").is_none()) {
            let events = run(sse(case["chunks"].as_array().unwrap()), true).await;
            assert_eq!(text(&events), case["text"].as_str().unwrap(), "{}", case["name"]);
            let got = calls(&events);
            let expected: Vec<(String, Value)> = case["calls"].as_array().unwrap().iter()
                .map(|c| (c["name"].as_str().unwrap().to_string(), c["arguments"].clone())).collect();
            assert_eq!(got.iter().map(|(_, n, a)| (n.clone(), a.clone())).collect::<Vec<_>>(), expected, "{}", case["name"]);
            assert!(got.iter().all(|(id, _, _)| id.starts_with("call_text_")));
            let stop = events.iter().find_map(|event| match event {
                Ok(StreamEvent::Usage { stop_reason, .. }) => Some(stop_reason.clone()),
                _ => None,
            }).flatten();
            let want = if expected.is_empty() { "stop" } else { "tool_calls" };
            assert_eq!(stop.as_deref(), Some(want), "{}", case["name"]);
        }
    }

    #[tokio::test]
    async fn unparsed_markup_fails_as_a_retryable_protocol_error() {
        for case in fixture()["cases"].as_array().unwrap().iter().filter(|c| c.get("error").is_some()) {
            let events = run(sse(case["chunks"].as_array().unwrap()), true).await;
            let error = events.iter().find_map(|event| event.as_ref().err()).expect("an error");
            let Error::ProviderFailure(error) = error else { panic!("not a provider failure: {error}") };
            assert_eq!(error.kind, super::super::provider_error::ProviderErrorKind::Protocol);
            assert!(error.retryable);
            assert_eq!(error.provider_code.as_deref(), case["error"].as_str());
            assert!(!error.message.contains("logs/a.log"));
        }
    }

    #[tokio::test]
    async fn other_models_pass_the_text_through_unchanged() {
        let case = &fixture()["cases"][0];
        let chunks = case["chunks"].as_array().unwrap();
        let events = run(sse(chunks), false).await;
        let all: String = chunks.iter().map(|c| c.as_str().unwrap()).collect();
        assert_eq!(text(&events), all);
        assert!(calls(&events).is_empty());
    }

    #[tokio::test]
    async fn parallel_native_tool_calls_all_survive_done_in_order() {
        let frames = [
            json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [
                { "index": 0, "id": "call_a", "function": { "name": "list_files", "arguments": "{}" } },
                { "index": 1, "id": "call_b", "function": { "name": "read_file", "arguments": "{\"path\":\"a\"}" } },
            ] } }] }),
            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }] }),
        ];
        let mut body: String = frames.iter().map(|frame| format!("data: {frame}\n\n")).collect();
        body.push_str("data: [DONE]\n\n");
        let events = run(body.into_bytes(), true).await;
        let ids: Vec<String> = calls(&events).into_iter().map(|(id, _, _)| id).collect();
        assert_eq!(ids, vec!["call_a", "call_b"]);
    }

    #[test]
    fn deepseek_models_watch_for_markup() {
        assert!(deepseek("k").text_tool_call_markup);
        assert!(!OpenAIProvider::new("k").text_tool_call_markup);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;
    use deepstrike_core::types::message::{ContentPart, CoreMessage, ToolCall};

    #[test]
    fn context_replays_tool_calls_and_results_natively() {
        let context = InternalRenderedContext {
            system_text: "system rules".into(),
            system_stable: "system rules".into(),
            system_knowledge: String::new(),
            budget_overflow: None,
            turn_items: Vec::new(),
            estimated_tokens: 0,
            turn_replays: Vec::new(),
            turns: vec![
                CoreMessage::user("What is the weather?"),
                CoreMessage {
                    role: Role::Assistant,
                    content: Content::Text("I'll check.".into()),
                    tool_calls: vec![ToolCall {
                        id: CompactString::new("call_1"),
                        name: CompactString::new("get_weather"),
                        arguments: json!({ "city": "Shanghai" }),
                    }],
                },
                CoreMessage::tool(vec![ContentPart::ToolResult {
                    call_id: CompactString::new("call_1"),
                    output: "sunny".into(),
                    is_error: false,
                    durable_content: None,
                }]),
            ],
            state_turn: None,
            frozen_prefix_len: None,
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

    #[test]
    fn state_turn_appended_as_latest_turn() {
        let context = InternalRenderedContext {
            system_text: "sys".into(),
            system_stable: "sys".into(),
            system_knowledge: String::new(),
            turns: vec![CoreMessage::user("history msg")],
            state_turn: Some(CoreMessage::user("[TASK STATE] goal: g\n\nProceed.")),
            frozen_prefix_len: None,
            budget_overflow: None,
            turn_items: Vec::new(),
            estimated_tokens: 0,
            turn_replays: Vec::new(),
        };
        let msgs = context_to_openai(&context);
        // [system][history][state] — history is the stable cacheable prefix, state last.
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["content"], "history msg");
        assert_eq!(msgs[2]["role"], "user");
        assert!(
            msgs[2]["content"]
                .as_str()
                .unwrap()
                .contains("[TASK STATE]")
        );
    }
}
