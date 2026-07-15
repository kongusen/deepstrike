use async_trait::async_trait;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::runtime::session::ProviderReplay;
use deepstrike_core::types::message::{Content, ContentPart, Message, Role, ToolCall, ToolSchema};
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{LLMProvider, RuntimePolicy, StreamEvent};
use crate::runtime::provider_replay::assistant_replay_key;
use crate::{Error, Result};

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    model: String,
    max_tokens: u32,
    native_assistant_blocks: Mutex<HashMap<String, Vec<Value>>>,
    stream_native_blocks: Arc<Mutex<HashMap<usize, Value>>>,
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
            native_assistant_blocks: Mutex::new(HashMap::new()),
            stream_native_blocks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn remember_native_blocks(&self, content: &str, tool_calls: &[ToolCall], blocks: Vec<Value>) {
        if blocks.is_empty() {
            return;
        }
        if tool_calls.is_empty()
            && !blocks
                .iter()
                .any(|b| b.get("type").and_then(|v| v.as_str()) == Some("thinking"))
        {
            return;
        }
        self.native_assistant_blocks
            .lock()
            .unwrap()
            .insert(assistant_replay_key(content, tool_calls), blocks);
    }

    fn context_to_anthropic(
        &self,
        context: &RenderedContext,
        strategy: CacheBreakpointStrategy,
    ) -> Result<(Option<Value>, Vec<Value>)> {
        let native = self.native_assistant_blocks.lock().unwrap();
        context_to_anthropic(context, strategy, |content, tool_calls| {
            native
                .get(&assistant_replay_key(content, tool_calls))
                .cloned()
        })
    }
}

fn content_part_to_anthropic(part: &ContentPart) -> Result<Value> {
    match part {
        ContentPart::Text { text } => Ok(json!({ "type": "text", "text": text })),
        ContentPart::Image {
            url: Some(url),
            data: None,
            ..
        } => Ok(json!({ "type": "image", "source": { "type": "url", "url": url } })),
        ContentPart::Image {
            data: Some(data),
            media_type,
            ..
        } => {
            let mt = media_type.as_deref().unwrap_or("image/png");
            Ok(json!({ "type": "image", "source": { "type": "base64", "media_type": mt, "data": data } }))
        }
        ContentPart::Image { .. } => Ok(json!({ "type": "text", "text": "" })),
        ContentPart::Audio { .. } => Err(Error::Provider(
            "UnsupportedModality: audio is not supported by anthropic".into(),
        )),
        ContentPart::ToolResult {
            call_id,
            output,
            is_error,
        } => Ok(
            json!({ "type": "tool_result", "tool_use_id": call_id.as_str(), "content": output, "is_error": is_error }),
        ),
    }
}

fn content_to_anthropic(content: &Content) -> Result<Value> {
    match content {
        Content::Text(s) => Ok(json!(s)),
        Content::Parts(parts) => {
            let blocks: Vec<Value> = parts
                .iter()
                .map(content_part_to_anthropic)
                .collect::<Result<Vec<_>>>()?;
            Ok(json!(blocks))
        }
    }
}

fn context_to_anthropic(
    context: &RenderedContext,
    strategy: CacheBreakpointStrategy,
    native_replay: impl Fn(&str, &[ToolCall]) -> Option<Vec<Value>>,
) -> Result<(Option<Value>, Vec<Value>)> {
    let mut msgs = Vec::new();
    for message in &context.turns {
        if message.role == Role::Tool {
            if let Content::Parts(parts) = &message.content {
                let tool_results = parts
                    .iter()
                    .filter_map(|part| {
                        if let ContentPart::ToolResult {
                            call_id,
                            output,
                            is_error,
                        } = part
                        {
                            Some(json!({
                                "type": "tool_result",
                                "tool_use_id": call_id.as_str(),
                                "content": output,
                                "is_error": is_error,
                            }))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                if !tool_results.is_empty() {
                    msgs.push(json!({ "role": "user", "content": tool_results }));
                }
            }
            continue;
        }

        if message.role == Role::Assistant && !message.tool_calls.is_empty() {
            let content = message.content.as_text().unwrap_or("");
            if let Some(replay) = native_replay(content, &message.tool_calls) {
                msgs.push(json!({ "role": "assistant", "content": replay }));
                continue;
            }
            let mut blocks = Vec::new();
            if !content.is_empty() {
                blocks.push(json!({ "type": "text", "text": content }));
            }
            blocks.extend(message.tool_calls.iter().map(|tc| {
                json!({
                    "type": "tool_use",
                    "id": tc.id.as_str(),
                    "name": tc.name.as_str(),
                    "input": tc.arguments.clone(),
                })
            }));
            msgs.push(json!({ "role": "assistant", "content": blocks }));
            continue;
        }

        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "assistant",
            Role::Tool => unreachable!(),
        };
        msgs.push(json!({ "role": role, "content": content_to_anthropic(&message.content)? }));
    }
    apply_message_cache_control(&mut msgs, strategy);
    // The volatile State turn is rendered AFTER the cache breakpoints, so the
    // history prefix stays cacheable and the state is the cheap uncached tail.
    // (When produced by an un-rebuilt binding, state_turn is None and the state
    // is already inside `turns` — rendered as-is above.)
    if let Some(state) = &context.state_turn {
        let role = if state.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        msgs.push(json!({ "role": role, "content": content_to_anthropic(&state.content)? }));
    }
    Ok((build_system(context, strategy), msgs))
}

/// Anthropic accepts at most this many cache_control breakpoints per request.
const MAX_CACHE_BREAKPOINTS: usize = 4;
/// Rolling cache breakpoints reserved for the message history (system uses ≤2).
const MESSAGE_CACHE_BREAKPOINTS: usize = 2;

/// Cache-control placement strategy. Mirrors the Node SDK's `CacheBreakpointStrategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheBreakpointStrategy {
    Default,
    ToolsOnly,
    SystemOnly,
    FrozenPrefix,
    None,
}

impl CacheBreakpointStrategy {
    fn from_str(raw: &str) -> Self {
        match raw {
            "tools-only" => Self::ToolsOnly,
            "system-only" => Self::SystemOnly,
            "frozen-prefix" => Self::FrozenPrefix,
            "none" => Self::None,
            // "default" and every unrecognised value
            _ => Self::Default,
        }
    }

    fn emit_on_tools(self) -> bool {
        matches!(self, Self::Default | Self::ToolsOnly)
    }
    fn emit_on_system(self) -> bool {
        matches!(self, Self::Default | Self::SystemOnly)
    }
    fn emit_on_messages(self) -> bool {
        matches!(self, Self::Default | Self::FrozenPrefix)
    }
    fn use_rolling_fallback(self) -> bool {
        matches!(self, Self::Default)
    }
}

/// Pull `cacheBreakpointStrategy` from per-call extensions; unrecognised → Default.
fn resolve_cache_breakpoint_strategy(extensions: Option<&Value>) -> CacheBreakpointStrategy {
    extensions
        .and_then(|e| e.get("cacheBreakpointStrategy"))
        .and_then(|v| v.as_str())
        .map(CacheBreakpointStrategy::from_str)
        .unwrap_or(CacheBreakpointStrategy::Default)
}

fn tools_to_anthropic(
    tools: &[ToolSchema],
    anchor_cache: bool,
    strategy: CacheBreakpointStrategy,
) -> Vec<Value> {
    let last = tools.len().saturating_sub(1);
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut def = json!({
                "name": t.name.as_str(),
                "description": t.description,
                "input_schema": t.parameters,
            });
            // Anchor a tool breakpoint only when the system blocks won't carry one;
            // otherwise system_stable already caches the tools prefix (tools render
            // first) and a redundant breakpoint would overrun the 4-slot budget.
            if anchor_cache && strategy.emit_on_tools() && i == last {
                def["cache_control"] = json!({ "type": "ephemeral" });
            }
            def
        })
        .collect()
}

/// Structured system blocks with cache_control when the kernel partitioned the
/// prompt (system_stable / system_knowledge); else the flat system_text string
/// (no breakpoint), or None.
fn build_system(context: &RenderedContext, strategy: CacheBreakpointStrategy) -> Option<Value> {
    if context.system_stable.is_empty() && context.system_knowledge.is_empty() {
        return if context.system_text.is_empty() {
            None
        } else {
            Some(json!(context.system_text))
        };
    }
    let emit = strategy.emit_on_system();
    let mut blocks = Vec::new();
    if !context.system_stable.is_empty() {
        let mut b = json!({ "type": "text", "text": context.system_stable });
        if emit {
            b["cache_control"] = json!({ "type": "ephemeral" });
        }
        blocks.push(b);
    }
    if !context.system_knowledge.is_empty() {
        let mut b = json!({ "type": "text", "text": context.system_knowledge });
        if emit {
            b["cache_control"] = json!({ "type": "ephemeral" });
        }
        blocks.push(b);
    }
    if blocks.is_empty() {
        None
    } else {
        Some(json!(blocks))
    }
}

/// Roll cache breakpoints across the conversation tail so the message-history
/// prefix is written once and re-read on later turns. Without this the cached
/// prefix stops at the end of `system` and the whole tool-result history is
/// re-billed at full input price every turn. Marks the final message plus the
/// nearest preceding user turn (read anchor); a bare string body is promoted to
/// a cache-bearing text block.
fn apply_message_cache_control(msgs: &mut [Value], strategy: CacheBreakpointStrategy) {
    if msgs.is_empty() || !strategy.emit_on_messages() {
        return;
    }
    let last = msgs.len() - 1;
    let mut targets = vec![last];
    // Rust SDK currently has no `frozen_prefix_len` field on RenderedContext; only the rolling
    // fallback applies, and only under Default strategy (FrozenPrefix degrades to last-message only).
    if strategy.use_rolling_fallback() {
        let mut i = last;
        while i > 0 && targets.len() < MESSAGE_CACHE_BREAKPOINTS {
            i -= 1;
            if msgs[i].get("role").and_then(|v| v.as_str()) == Some("user") {
                targets.push(i);
            }
        }
    }
    for idx in targets {
        mark_last_block_cacheable(&mut msgs[idx]);
    }
}

fn mark_last_block_cacheable(msg: &mut Value) {
    let cache_control = json!({ "type": "ephemeral" });
    match msg.get_mut("content") {
        Some(Value::String(s)) => {
            if s.is_empty() {
                return; // don't synthesize an empty (API-rejected) text block
            }
            let text = s.clone();
            msg["content"] =
                json!([{ "type": "text", "text": text, "cache_control": cache_control }]);
        }
        Some(Value::Array(arr)) => {
            if let Some(obj) = arr.last_mut().and_then(|b| b.as_object_mut()) {
                obj.insert("cache_control".to_string(), cache_control);
            }
        }
        _ => {}
    }
}

/// Regression guard: fail before the API would reject the request for exceeding
/// the cache_control breakpoint limit. Uses the worst-case message count, so it
/// can only fire if a future change adds a system partition or raises the budget.
fn assert_cache_budget(system: Option<&Value>, tool_count: usize) -> Result<()> {
    let system_breakpoints = match system {
        Some(Value::Array(a)) => a.len(),
        _ => 0,
    };
    let is_array = matches!(system, Some(Value::Array(_)));
    let tool_breakpoints = if tool_count > 0 && !is_array { 1 } else { 0 };
    if system_breakpoints + tool_breakpoints + MESSAGE_CACHE_BREAKPOINTS > MAX_CACHE_BREAKPOINTS {
        return Err(Error::Provider(format!(
            "Anthropic cache_control budget exceeded: {system_breakpoints} system + {tool_breakpoints} tool + {MESSAGE_CACHE_BREAKPOINTS} message > {MAX_CACHE_BREAKPOINTS}"
        )));
    }
    Ok(())
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    fn runtime_policy(&self) -> RuntimePolicy {
        match self.model.as_str() {
            "claude-opus-4-7" | "claude-opus-4-6" => RuntimePolicy {
                max_turns: Some(50),
                timeout_ms: None,
            },
            "claude-sonnet-4-6" => RuntimePolicy {
                max_turns: Some(25),
                timeout_ms: None,
            },
            "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => RuntimePolicy {
                max_turns: Some(15),
                timeout_ms: None,
            },
            _ => RuntimePolicy::default(),
        }
    }

    fn peek_provider_replay(
        &self,
        content: &str,
        tool_calls: &[ToolCall],
    ) -> Option<ProviderReplay> {
        let blocks = self
            .native_assistant_blocks
            .lock()
            .unwrap()
            .get(&assistant_replay_key(content, tool_calls))?
            .clone();
        if blocks.is_empty() {
            None
        } else {
            Some(ProviderReplay {
                native_blocks: Some(blocks),
                reasoning_content: None,
                extra: serde_json::Map::new(),
            })
        }
    }

    fn seed_provider_replay(
        &self,
        content: &str,
        tool_calls: &[ToolCall],
        replay: &ProviderReplay,
    ) {
        if let Some(blocks) = &replay.native_blocks {
            if !blocks.is_empty() {
                self.native_assistant_blocks
                    .lock()
                    .unwrap()
                    .insert(assistant_replay_key(content, tool_calls), blocks.clone());
            }
        }
    }

    fn commit_stream_replay(&self, content: &str, tool_calls: &[ToolCall]) {
        let blocks: Vec<Value> = {
            let map = self.stream_native_blocks.lock().unwrap();
            let mut indices: Vec<_> = map.keys().copied().collect();
            indices.sort_unstable();
            indices
                .into_iter()
                .filter_map(|idx| map.get(&idx).cloned())
                .collect()
        };
        self.remember_native_blocks(content, tool_calls, blocks);
    }

    async fn stream(
        &self,
        context: &RenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&Value>,
        _state: Option<&super::ProviderRunState>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>> {
        self.stream_native_blocks.lock().unwrap().clear();
        let strategy = resolve_cache_breakpoint_strategy(extensions);
        let (system, msgs) = self.context_to_anthropic(context, strategy)?;
        // Anchor the tool breakpoint only when system is not structured blocks.
        let tool_anchor = !matches!(&system, Some(Value::Array(_)));
        assert_cache_budget(system.as_ref(), tools.len())?;
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": msgs,
            "stream": true,
        });
        if let Some(s) = system {
            body["system"] = s;
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools_to_anthropic(tools, tool_anchor, strategy));
        }
        if let Some(ext) = extensions {
            if ext
                .get("enable_thinking")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": 8000 });
            }
        }

        let resp = self
            .client
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
        let stream = parse_anthropic_sse(byte_stream, self.stream_native_blocks.clone());
        Ok(Box::new(Box::pin(stream)))
    }
}

/// Extract raw usage components `(uncached_input, cache_read, cache_creation,
/// output)` from an Anthropic usage object; absent fields default to 0. Returns
/// None only when `usage` is not an object. Anthropic pins input + cache counts
/// at message_start and reports cumulative output on message_delta, so callers
/// max-accumulate these across events rather than trusting any single one.
fn anthropic_usage_breakdown(usage: &Value) -> Option<(u32, u32, u32, u32)> {
    if !usage.is_object() {
        return None;
    }
    let field = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    Some((
        field("input_tokens"),
        field("cache_read_input_tokens"),
        field("cache_creation_input_tokens"),
        field("output_tokens"),
    ))
}

fn parse_anthropic_sse(
    byte_stream: impl Stream<Item = reqwest::Result<bytes::Bytes>> + Send + 'static,
    native_blocks: Arc<Mutex<HashMap<usize, Value>>>,
) -> impl Stream<Item = Result<StreamEvent>> + Send {
    let mut buf = String::new();
    let mut tool_blocks: std::collections::HashMap<usize, (String, String, String)> =
        std::collections::HashMap::new();

    futures::stream::unfold(
        (
            Box::pin(byte_stream),
            buf,
            tool_blocks,
            native_blocks,
            (0u32, 0u32, 0u32, 0u32),
        ),
        |(mut stream, mut buf, mut tool_blocks, native_blocks, mut acc)| async move {
            loop {
                if let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf = buf[pos + 1..].to_string();

                    if !line.starts_with("data: ") {
                        continue;
                    }
                    let data = &line[6..];
                    if data == "[DONE]" {
                        return None;
                    }

                    let Ok(evt) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };
                    let kind = evt["type"].as_str().unwrap_or("");

                    if kind == "content_block_start" {
                        let idx = evt["index"].as_u64().unwrap_or(0) as usize;
                        let cb = &evt["content_block"];
                        native_blocks.lock().unwrap().insert(idx, cb.clone());
                        if cb["type"] == "tool_use" {
                            tool_blocks.insert(
                                idx,
                                (
                                    cb["id"].as_str().unwrap_or("").to_string(),
                                    cb["name"].as_str().unwrap_or("").to_string(),
                                    String::new(),
                                ),
                            );
                        }
                    } else if kind == "content_block_delta" {
                        let d = &evt["delta"];
                        let idx = evt["index"].as_u64().unwrap_or(0) as usize;
                        if d["type"] == "text_delta" {
                            let delta = d["text"].as_str().unwrap_or("").to_string();
                            if let Some(block) = native_blocks.lock().unwrap().get_mut(&idx) {
                                let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                block["text"] = json!(format!("{text}{delta}"));
                            }
                            if !delta.is_empty() {
                                return Some((
                                    Ok(StreamEvent::TextDelta { delta }),
                                    (stream, buf, tool_blocks, native_blocks, acc),
                                ));
                            }
                        } else if d["type"] == "thinking_delta" {
                            let delta = d["thinking"].as_str().unwrap_or("").to_string();
                            if let Some(block) = native_blocks.lock().unwrap().get_mut(&idx) {
                                let text =
                                    block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                                block["thinking"] = json!(format!("{text}{delta}"));
                            }
                            if !delta.is_empty() {
                                return Some((
                                    Ok(StreamEvent::ThinkingDelta { delta }),
                                    (stream, buf, tool_blocks, native_blocks, acc),
                                ));
                            }
                        } else if d["type"] == "signature_delta" {
                            if let Some(block) = native_blocks.lock().unwrap().get_mut(&idx) {
                                let sig = block
                                    .get("signature")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let delta = d["signature"].as_str().unwrap_or("");
                                block["signature"] = json!(format!("{sig}{delta}"));
                            }
                        } else if d["type"] == "input_json_delta" {
                            if let Some(tb) = tool_blocks.get_mut(&idx) {
                                tb.2.push_str(d["partial_json"].as_str().unwrap_or(""));
                            }
                        }
                    } else if kind == "content_block_stop" {
                        let idx = evt["index"].as_u64().unwrap_or(0) as usize;
                        if let Some((id, name, args_buf)) = tool_blocks.remove(&idx) {
                            let arguments: Value = serde_json::from_str(&args_buf)
                                .unwrap_or(Value::Object(Default::default()));
                            if let Some(block) = native_blocks.lock().unwrap().get_mut(&idx) {
                                block["input"] = arguments.clone();
                            }
                            return Some((
                                Ok(StreamEvent::ToolCall {
                                    id,
                                    name,
                                    arguments,
                                }),
                                (stream, buf, tool_blocks, native_blocks, acc),
                            ));
                        }
                    } else if kind == "message_start" || kind == "message_delta" {
                        let usage = evt
                            .get("usage")
                            .or_else(|| evt.get("message").and_then(|m| m.get("usage")));
                        if let Some((uncached, cache_read, cache_creation, output)) =
                            usage.and_then(anthropic_usage_breakdown)
                        {
                            // acc = (uncached, cache_read, cache_creation, output).
                            // A message_delta omits input/cache (read as 0); max()
                            // keeps the totals pinned at message_start while letting
                            // the final cumulative output through.
                            acc.0 = acc.0.max(uncached);
                            acc.1 = acc.1.max(cache_read);
                            acc.2 = acc.2.max(cache_creation);
                            acc.3 = acc.3.max(output);
                            let full_input = acc.0 + acc.1 + acc.2;
                            // stop_reason rides on message_delta (the closing frame); `max_tokens`
                            // drives the kernel's output-cap recovery.
                            let stop_reason = evt
                                .get("delta")
                                .and_then(|d| d.get("stop_reason"))
                                .and_then(|s| s.as_str())
                                .map(|s| s.to_string());
                            return Some((
                                Ok(StreamEvent::Usage {
                                    total_tokens: full_input + acc.3,
                                    input_tokens: full_input,
                                    output_tokens: acc.3,
                                    cache_read_input_tokens: acc.1,
                                    cache_creation_input_tokens: acc.2,
                                    // I1: per-slot attribution not yet wired in Rust Anthropic
                                    // provider; field reserved. Mirrors the field-presence
                                    // contract across SDKs; non-Anthropic Rust providers also
                                    // emit None. Wiring is left for a focused Rust iteration.
                                    cache_read_input_tokens_by_slot: None,
                                    stop_reason,
                                }),
                                (stream, buf, tool_blocks, native_blocks, acc),
                            ));
                        }
                    }
                    continue;
                }

                match stream.next().await {
                    Some(Ok(chunk)) => {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    Some(Err(e)) => {
                        return Some((
                            Err(Error::Provider(e.to_string())),
                            (stream, buf, tool_blocks, native_blocks, acc),
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
    fn anthropic_usage_breakdown_extracts_raw_components() {
        let usage = json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 900,
            "cache_creation_input_tokens": 10,
        });
        // (uncached, cache_read, cache_creation, output)
        assert_eq!(anthropic_usage_breakdown(&usage), Some((100, 900, 10, 50)));
    }

    #[test]
    fn anthropic_usage_breakdown_defaults_absent_fields_to_zero() {
        // A message_delta carries only the cumulative output; the rest read as 0
        // so the caller's max-accumulator keeps the message_start input/cache.
        let usage = json!({ "output_tokens": 50 });
        assert_eq!(anthropic_usage_breakdown(&usage), Some((0, 0, 0, 50)));
        // A non-object usage yields None.
        assert_eq!(anthropic_usage_breakdown(&json!("nope")), None);
    }

    #[test]
    fn context_replays_tool_calls_and_results_as_blocks() {
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
            state_turn: None,
            frozen_prefix_len: None,
            budget_overflow: None,
        };

        let (system, messages) =
            context_to_anthropic(&context, CacheBreakpointStrategy::Default, |_, _| None).unwrap();
        // system_stable present -> structured cache block, not a bare string.
        assert_eq!(
            system,
            Some(json!([
                { "type": "text", "text": "system rules", "cache_control": { "type": "ephemeral" } }
            ]))
        );
        // The first user turn and the trailing tool-result turn carry rolling
        // cache breakpoints; the assistant tool-use turn is untouched.
        assert_eq!(
            messages,
            vec![
                json!({ "role": "user", "content": [
                    { "type": "text", "text": "What is the weather?", "cache_control": { "type": "ephemeral" } }
                ] }),
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
                        "cache_control": { "type": "ephemeral" },
                    }],
                }),
            ]
        );
    }

    #[test]
    fn budget_guard_passes_for_partitioned_system_with_tools() {
        let context = RenderedContext {
            system_text: "rules\nknowledge".into(),
            system_stable: "rules".into(),
            system_knowledge: "knowledge".into(),
            turns: vec![Message::user("hi")],
            state_turn: None,
            frozen_prefix_len: None,
            budget_overflow: None,
        };
        let (system, _msgs) =
            context_to_anthropic(&context, CacheBreakpointStrategy::Default, |_, _| None).unwrap();
        // 2 system + 2 message = 4 (tool breakpoint dropped) — at the limit, ok.
        assert!(assert_cache_budget(system.as_ref(), 3).is_ok());
    }

    #[test]
    fn state_turn_rendered_after_history_without_cache_control() {
        // History is the cacheable prefix; the volatile state turn is the tail.
        let context = RenderedContext {
            system_text: String::new(),
            system_stable: String::new(),
            system_knowledge: String::new(),
            turns: vec![
                Message::user("earlier question"),
                Message::assistant("earlier answer"),
            ],
            state_turn: Some(Message::user("[TASK STATE] goal: g\n\nProceed.")),
            frozen_prefix_len: None,
            budget_overflow: None,
        };
        let (_system, messages) =
            context_to_anthropic(&context, CacheBreakpointStrategy::Default, |_, _| None).unwrap();
        // history (2) + state (1) appended last
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "user");
        assert!(messages[2]["content"]
            .as_str()
            .unwrap()
            .contains("[TASK STATE]"));
        // the state turn carries NO cache breakpoint (it is the uncached tail)
        assert!(messages[2].get("cache_control").is_none());
        // the last history turn DID get a breakpoint (read anchor) — it became a block array
        assert!(messages[1]["content"].is_array() || messages[0]["content"].is_array());
    }
}
