use async_trait::async_trait;
use deepstrike_core::context::renderer::InternalRenderedContext;
use deepstrike_core::runtime::session::ProviderReplay;
use deepstrike_core::types::message::{Content, ContentPart, Role, ToolCall, ToolSchema};
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::anthropic_compaction::{
    COMPACT_EDIT, CompactionSend, OnDemandCompactor, THRESHOLD_COMPACTION_BETA, compaction_block,
    resume_after_compaction_pause, without_compaction_pause,
};
pub use super::anthropic_compaction::{AnthropicCompactionPause, AnthropicOnDemandCompaction};
use super::{LLMProvider, NativeContextTiers, RuntimePolicy, StreamEvent};
use crate::runtime::provider_replay::assistant_replay_key;
use crate::{Error, Result};

const CONTEXT_MANAGEMENT_BETA: &str = "context-management-2025-06-27";
const CLEAR_TOOL_USES_EDIT: &str = "clear_tool_uses_20250919";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";

/// Anthropic threshold compaction (`compact_20260112`). Unset fields take the API defaults.
#[derive(Debug, Clone, Default)]
pub struct AnthropicCompaction {
    /// Input tokens at which the API summarizes earlier history (at least 50,000; default 150,000).
    pub trigger_input_tokens: Option<u32>,
    /// A summarization prompt that replaces the API's default one.
    pub instructions: Option<String>,
    /// Stop after the summary is written, then continue with the newest history kept verbatim.
    pub pause_after_compaction: Option<AnthropicCompactionPause>,
}

/// How requests reach the Messages API. The HTTP client in production; a script in tests.
#[async_trait]
pub(crate) trait AnthropicTransport: Send + Sync {
    /// A streamed request: the response body's bytes.
    async fn stream(
        &self,
        body: &Value,
        betas: &[String],
    ) -> Result<futures::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>>;
    /// A non-streaming request: the response JSON.
    async fn create(&self, body: &Value, betas: &[String]) -> Result<Value>;
}

struct HttpTransport {
    client: Client,
    api_key: String,
}

impl HttpTransport {
    async fn send(&self, body: &Value, betas: &[String]) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .post(MESSAGES_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");
        if !betas.is_empty() {
            request = request.header("anthropic-beta", betas.join(","));
        }
        let resp = request.body(body.to_string()).send().await.map_err(|e| {
            Error::from(super::ProviderError::transport("anthropic", e.to_string()))
        })?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(super::ProviderError::from_http("anthropic", status, text).into());
        }
        Ok(resp)
    }
}

#[async_trait]
impl AnthropicTransport for HttpTransport {
    async fn stream(
        &self,
        body: &Value,
        betas: &[String],
    ) -> Result<futures::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>> {
        Ok(self.send(body, betas).await?.bytes_stream().boxed())
    }

    async fn create(&self, body: &Value, betas: &[String]) -> Result<Value> {
        let mut body = body.clone();
        if let Some(object) = body.as_object_mut() {
            object.remove("stream");
        }
        self.send(&body, betas).await?.json::<Value>().await.map_err(|e| {
            Error::from(super::ProviderError::transport("anthropic", e.to_string()))
        })
    }
}

/// Anthropic context editing (`clear_tool_uses_20250919`). Unset fields take the API defaults.
#[derive(Debug, Clone, Default)]
pub struct AnthropicContextEditing {
    pub trigger_input_tokens: Option<u32>,
    pub keep_tool_uses: Option<u32>,
    pub clear_at_least_input_tokens: Option<u32>,
    pub exclude_tools: Vec<String>,
    pub clear_tool_inputs: Option<bool>,
}

pub struct AnthropicProvider {
    transport: Arc<dyn AnthropicTransport>,
    model: String,
    max_tokens: u32,
    context_editing: Option<Value>,
    compaction: Option<Value>,
    /// Threshold compaction with a pause: how many history messages stay verbatim behind the block.
    compaction_pause_keep: Option<usize>,
    on_demand: Option<Arc<OnDemandCompactor>>,
    native_assistant_blocks: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    stream_native_blocks: Arc<Mutex<HashMap<usize, Value>>>,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_model(api_key, "claude-sonnet-4-6")
    }

    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::with_context_editing(api_key, model, None)
    }

    pub fn with_context_editing(
        api_key: impl Into<String>,
        model: impl Into<String>,
        config: Option<AnthropicContextEditing>,
    ) -> Self {
        let context_editing = config.map(|c| {
            let mut edit = json!({"type": CLEAR_TOOL_USES_EDIT});
            if let Some(value) = c.trigger_input_tokens { edit["trigger"] = json!({"type": "input_tokens", "value": value}); }
            if let Some(value) = c.keep_tool_uses { edit["keep"] = json!({"type": "tool_uses", "value": value}); }
            if let Some(value) = c.clear_at_least_input_tokens { edit["clear_at_least"] = json!({"type": "input_tokens", "value": value}); }
            if !c.exclude_tools.is_empty() { edit["exclude_tools"] = json!(c.exclude_tools); }
            if let Some(value) = c.clear_tool_inputs { edit["clear_tool_inputs"] = json!(value); }
            edit
        });
        Self {
            transport: Arc::new(HttpTransport { client: Client::new(), api_key: api_key.into() }),
            model: model.into(),
            max_tokens: 8096,
            context_editing,
            compaction: None,
            compaction_pause_keep: None,
            on_demand: None,
            native_assistant_blocks: Arc::new(Mutex::new(HashMap::new())),
            stream_native_blocks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_transport(mut self, transport: Arc<dyn AnthropicTransport>) -> Self {
        self.transport = transport;
        self
    }

    /// Summarize earlier history server-side once input reaches a threshold. The provider then
    /// declares compaction native and the kernel stops compacting itself; its renewal stays the
    /// safety net, so set the trigger below the kernel's renewal threshold. Replaces any on-demand
    /// compaction set before.
    pub fn with_compaction(mut self, config: AnthropicCompaction) -> Self {
        let mut edit = json!({"type": COMPACT_EDIT});
        if let Some(value) = config.trigger_input_tokens {
            edit["trigger"] = json!({"type": "input_tokens", "value": value});
        }
        if config.pause_after_compaction.is_some() {
            edit["pause_after_compaction"] = json!(true);
        }
        if let Some(instructions) = config.instructions {
            edit["instructions"] = json!(instructions);
        }
        self.compaction = Some(edit);
        self.compaction_pause_keep = config.pause_after_compaction.map(|pause| pause.keep_recent_messages);
        self.on_demand = None;
        self
    }

    /// Summarize earlier history on demand (`compact-2026-09-04`): once a turn's input plus output
    /// reaches `compact_at_input_tokens`, the provider asks for a summary of that turn's history in
    /// the background and swaps it in for those messages on later requests. Compaction is then
    /// declared native, as with the threshold form, which this replaces.
    pub fn with_on_demand_compaction(mut self, config: AnthropicOnDemandCompaction) -> Result<Self> {
        if config.compact_at_input_tokens == 0 {
            return Err(Error::Other("on-demand compaction needs a positive compact_at_input_tokens".into()));
        }
        if config.max_tokens == Some(0) {
            return Err(Error::Other("on-demand compaction max_tokens must be positive".into()));
        }
        self.on_demand = Some(Arc::new(OnDemandCompactor::new(config)));
        self.compaction = None;
        self.compaction_pause_keep = None;
        Ok(self)
    }

    /// On-demand compaction in flight settles; for tests and orderly shutdown.
    pub async fn compaction_idle(&self) {
        if let Some(compactor) = &self.on_demand {
            compactor.idle().await;
        }
    }

    fn remember_native_blocks(&self, content: &str, tool_calls: &[ToolCall], blocks: Vec<Value>) {
        if blocks.is_empty() {
            return;
        }
        if tool_calls.is_empty()
            && !blocks
                .iter()
                .any(|b| matches!(b.get("type").and_then(|v| v.as_str()), Some("thinking" | "compaction")))
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
        context: &InternalRenderedContext,
        strategy: CacheBreakpointStrategy,
    ) -> Result<RequestParts> {
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
            source, media_type, ..
        } => match source {
            deepstrike_core::types::durable_content::DurableSource::Url { url } => {
                Ok(json!({ "type": "image", "source": { "type": "url", "url": url } }))
            }
            deepstrike_core::types::durable_content::DurableSource::Base64 { data } => {
                let mt = media_type.as_deref().unwrap_or("image/png");
                Ok(
                    json!({ "type": "image", "source": { "type": "base64", "media_type": mt, "data": data } }),
                )
            }
            _ => Ok(json!({ "type": "text", "text": "" })),
        },
        ContentPart::Audio { .. } => Err(Error::Provider(
            "UnsupportedModality: audio is not supported by anthropic".into(),
        )),
        ContentPart::ToolResult {
            call_id,
            output,
            is_error,
            ..
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

/// The request's system, its messages, and for each message the index of the turn it came from
/// (`None` for the state turn, which always comes last).
type RequestParts = (Option<Value>, Vec<Value>, Vec<Option<usize>>);

fn context_to_anthropic(
    context: &InternalRenderedContext,
    strategy: CacheBreakpointStrategy,
    native_replay: impl Fn(&str, &[ToolCall]) -> Option<Vec<Value>>,
) -> Result<RequestParts> {
    let mut msgs = Vec::new();
    let mut wire_turns = Vec::new();
    for (turn_index, message) in context.turns.iter().enumerate() {
        // Messages pushed so far, and not yet attributed, came from the previous turn.
        if let Some(previous) = turn_index.checked_sub(1) {
            wire_turns.resize(msgs.len(), Some(previous));
        }
        if message.role == Role::Tool {
            if let Content::Parts(parts) = &message.content {
                let tool_results = parts
                    .iter()
                    .filter_map(|part| {
                        if let ContentPart::ToolResult {
                            call_id,
                            output,
                            is_error,
                            ..
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

        if message.role == Role::Assistant && message.tool_calls.is_empty() {
            // A turn that carries a server compaction block goes back as the API sent it: the
            // block is what tells the API to drop everything before it.
            let content = message.content.as_text().unwrap_or("");
            if let Some(replay) = native_replay(content, &message.tool_calls)
                && replay.iter().any(|block| block["type"] == "compaction")
            {
                msgs.push(json!({ "role": "assistant", "content": replay }));
                continue;
            }
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
    wire_turns.resize(msgs.len(), context.turns.len().checked_sub(1));
    // The API ignores everything before a compaction block, so it is not sent.
    let head = msgs.iter().rposition(|message| {
        message["content"]
            .as_array()
            .is_some_and(|content| content.iter().any(|block| block["type"] == "compaction"))
    });
    if let Some(head) = head.filter(|head| *head > 0) {
        msgs.drain(..head);
        wire_turns.drain(..head);
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
        wire_turns.push(None);
    }
    Ok((build_system(context, strategy), msgs, wire_turns))
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
fn build_system(
    context: &InternalRenderedContext,
    strategy: CacheBreakpointStrategy,
) -> Option<Value> {
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
    // Rust SDK currently has no `frozen_prefix_len` field on InternalRenderedContext; only the rolling
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
    fn context_route(&self) -> Value {
        json!({ "provider": "anthropic", "protocol": "anthropic-messages", "model": self.model,
            "endpoint": "https://api.anthropic.com/v1/messages" })
    }

    fn native_context_tiers(&self) -> NativeContextTiers {
        NativeContextTiers {
            tool_result_clearing: self.context_editing.is_some(),
            compaction: self.compaction.is_some() || self.on_demand.is_some(),
        }
    }

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
                protocol: "anthropic-messages".into(),
                provider: Some("anthropic".into()),
                model: Some(self.model.clone()),
                native_blocks: Some(blocks),
                reasoning_content: None,
                reasoning_details: None,
                native_message: None,
                tool_calls: None,
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

    fn prepare_context_request(
        &self,
        context: &InternalRenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&Value>,
        _state: Option<&super::ProviderRunState>,
    ) -> Result<Value> {
        let strategy = resolve_cache_breakpoint_strategy(extensions);
        let (system, msgs, _) = self.context_to_anthropic(context, strategy)?;
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
            if let Some(value) = ext.get("context_management") {
                body["context_management"] = value.clone();
            }
        }
        // Betas are transport metadata (the `anthropic-beta` header), not body fields; they sit
        // beside the body so sending never has to copy the body to strip them.
        let mut betas: Vec<String> = extensions
            .and_then(|ext| ext.get("betas"))
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        // The native edits ride on every request, beside any the caller passed: the kernel stopped
        // running those tiers on the strength of them. Clearing runs first so the compaction
        // trigger reads the input after tool results are cleared.
        let native = [
            (CONTEXT_MANAGEMENT_BETA, CLEAR_TOOL_USES_EDIT, &self.context_editing),
            (THRESHOLD_COMPACTION_BETA, COMPACT_EDIT, &self.compaction),
        ];
        for (beta, kind, configured) in native {
            let Some(configured) = configured else { continue };
            if !betas.iter().any(|held| held == beta) {
                betas.push(beta.to_owned());
            }
            let management = body
                .as_object_mut()
                .expect("request body is an object")
                .entry("context_management")
                .or_insert_with(|| json!({}));
            if !management.is_object() {
                *management = json!({});
            }
            let edits = management
                .as_object_mut()
                .expect("context_management is an object")
                .entry("edits")
                .or_insert_with(|| json!([]));
            if !edits.is_array() {
                *edits = json!([]);
            }
            let edits = edits.as_array_mut().expect("edits is an array");
            if !edits.iter().any(|edit| edit.get("type").and_then(Value::as_str) == Some(kind)) {
                edits.push(configured.clone());
            }
        }

        if let Some(compactor) = &self.on_demand {
            compactor.apply(&mut body, &mut betas);
        }

        let mut prepared = json!({ "scope": "encoded_body", "body": body });
        if !betas.is_empty() {
            prepared["betas"] = json!(betas);
        }
        Ok(prepared)
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
        context: &InternalRenderedContext,
        _tools: &[ToolSchema],
        extensions: Option<&Value>,
        _state: Option<&super::ProviderRunState>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>> {
        self.stream_native_blocks.lock().unwrap().clear();
        let body = prepared
            .get("body")
            .filter(|body| body.is_object())
            .ok_or_else(|| {
                Error::Other("prepared provider request must contain an encoded body".into())
            })?
            .clone();
        let betas: Vec<String> = prepared
            .get("betas")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();

        if self.compaction_pause_keep.is_none() && self.on_demand.is_none() {
            let bytes = self.transport.stream(&body, &betas).await?;
            return Ok(Box::new(Box::pin(parse_anthropic_sse(bytes, self.stream_native_blocks.clone()))));
        }

        // The replay key of the turn behind each wire message, for a pause that re-anchors one.
        let strategy = resolve_cache_breakpoint_strategy(extensions);
        let (_, _, wire_turns) = self.context_to_anthropic(context, strategy)?;
        let wire_keys: Vec<Option<String>> = wire_turns
            .iter()
            .map(|turn| {
                turn.and_then(|index| context.turns.get(index)).map(|message| {
                    assistant_replay_key(message.content.as_text().unwrap_or(""), &message.tool_calls)
                })
            })
            .collect();
        let turn = CompactingTurn {
            transport: self.transport.clone(),
            body,
            betas,
            wire_keys,
            keep: self.compaction_pause_keep,
            on_demand: self.on_demand.clone(),
            native_assistant_blocks: self.native_assistant_blocks.clone(),
            stream_native_blocks: self.stream_native_blocks.clone(),
        };
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            if let Err(error) = turn.run(&tx).await {
                let _ = tx.send(Err(error)).await;
            }
        });
        Ok(Box::new(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        }))))
    }
}

/// One turn under a compaction feature, driven off the caller's task so its events stream as they
/// arrive: with a pause configured, events are held until the response shows it is not a pause
/// (any non-usage event) or ends; a pause is continued with the kept history behind the block;
/// and the turn's usage tells on-demand compaction whether to ask for a summary.
struct CompactingTurn {
    transport: Arc<dyn AnthropicTransport>,
    body: Value,
    betas: Vec<String>,
    wire_keys: Vec<Option<String>>,
    keep: Option<usize>,
    on_demand: Option<Arc<OnDemandCompactor>>,
    native_assistant_blocks: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    stream_native_blocks: Arc<Mutex<HashMap<usize, Value>>>,
}

type EventSender = tokio::sync::mpsc::Sender<Result<StreamEvent>>;

impl CompactingTurn {
    /// Forward (or hold) one response's events; returns its stop reason and token total, or
    /// `None` once the consumer has gone.
    async fn relay(
        &self,
        body: &Value,
        tx: &EventSender,
        mut held: Option<&mut Vec<StreamEvent>>,
    ) -> Result<Option<(Option<String>, u32)>> {
        let bytes = self.transport.stream(body, &self.betas).await?;
        let mut events = Box::pin(parse_anthropic_sse(bytes, self.stream_native_blocks.clone()));
        let (mut stop, mut total) = (None, 0);
        while let Some(event) = events.next().await {
            let event = event?;
            if let StreamEvent::Usage { stop_reason, total_tokens, .. } = &event {
                if stop_reason.is_some() {
                    stop = stop_reason.clone();
                }
                total = *total_tokens;
            } else if let Some(buffer) = held.take() {
                for earlier in buffer.drain(..) {
                    if tx.send(Ok(earlier)).await.is_err() {
                        return Ok(None);
                    }
                }
            }
            match held.as_deref_mut() {
                Some(buffer) => buffer.push(event),
                None => {
                    if tx.send(Ok(event)).await.is_err() {
                        return Ok(None);
                    }
                }
            }
        }
        Ok(Some((stop, total)))
    }

    fn streamed_blocks(&self) -> Vec<Value> {
        let map = self.stream_native_blocks.lock().unwrap();
        let mut indices: Vec<_> = map.keys().copied().collect();
        indices.sort_unstable();
        indices.into_iter().filter_map(|index| map.get(&index).cloned()).collect()
    }

    async fn run(self, tx: &EventSender) -> Result<()> {
        let mut held = self.keep.map(|_| Vec::new());
        let Some((stop, mut total)) = self.relay(&self.body, tx, held.as_mut()).await? else {
            return Ok(());
        };
        let history_count = self.wire_keys.iter().filter(|key| key.is_some()).count();
        let paused = match (&held, stop.as_deref()) {
            (Some(_), Some("compaction")) => compaction_block(&self.streamed_blocks()),
            _ => None,
        };
        if let Some(block) = paused {
            let messages = self.body["messages"].as_array().cloned().unwrap_or_default();
            let (resumed, anchor) = resume_after_compaction_pause(
                &messages,
                history_count,
                &block,
                self.keep.unwrap_or(0),
            );
            let anchored = match anchor {
                Some((wire_index, content)) => match self.wire_keys.get(wire_index).cloned().flatten() {
                    Some(key) => {
                        self.native_assistant_blocks.lock().unwrap().insert(key, content);
                        true
                    }
                    None => false,
                },
                None => false,
            };
            let mut continuation = without_compaction_pause(&self.body);
            continuation["messages"] = Value::Array(resumed);
            self.stream_native_blocks.lock().unwrap().clear();
            let Some((_, continued)) = self.relay(&continuation, tx, None).await? else {
                return Ok(());
            };
            total = continued;
            if !anchored {
                // The block heads the reply's own replay, ahead of its streamed blocks.
                let mut blocks = vec![block];
                blocks.extend(self.streamed_blocks());
                let mut map = self.stream_native_blocks.lock().unwrap();
                map.clear();
                map.extend(blocks.into_iter().enumerate());
            }
        } else if let Some(buffer) = held {
            for event in buffer {
                if tx.send(Ok(event)).await.is_err() {
                    return Ok(());
                }
            }
        }
        if let Some(compactor) = &self.on_demand {
            let transport = self.transport.clone();
            let send: CompactionSend = Arc::new(move |body, betas| {
                let transport = transport.clone();
                Box::pin(async move { transport.create(&body, &betas).await })
            });
            compactor.observe(&self.body, &self.betas, history_count, total, send);
        }
        Ok(())
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
                        } else if d["type"] == "compaction_delta" {
                            // The summary arrives whole in one delta; it is replayed, never shown.
                            if let Some(block) = native_blocks.lock().unwrap().get_mut(&idx) {
                                let held = block.get("content").and_then(|v| v.as_str()).unwrap_or("");
                                let delta = d["content"].as_str().unwrap_or("");
                                block["content"] = json!(format!("{held}{delta}"));
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
                            let arguments =
                                crate::runtime::tool_arguments::from_model_text(&args_buf);
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
                            Err(super::ProviderError::transport("anthropic", e.to_string()).into()),
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
    use deepstrike_core::types::message::{ContentPart, CoreMessage, ToolCall};

    fn question_context() -> InternalRenderedContext {
        InternalRenderedContext {
            system_text: String::new(),
            system_stable: String::new(),
            system_knowledge: String::new(),
            turns: vec![CoreMessage::user("question")],
            state_turn: None,
            frozen_prefix_len: None,
            budget_overflow: None,
            turn_items: Vec::new(),
            estimated_tokens: 0,
            turn_replays: Vec::new(),
        }
    }

    #[test]
    fn context_editing_is_off_unless_configured() {
        let provider = AnthropicProvider::new("test-key");
        assert_eq!(provider.native_context_tiers(), NativeContextTiers::default());
        let prepared = provider.prepare_context_request(&question_context(), &[], None, None).unwrap();
        assert!(prepared.get("betas").is_none());
        assert!(prepared["body"].get("betas").is_none());
        assert!(prepared["body"].get("context_management").is_none());
    }

    #[test]
    fn context_editing_sends_clear_tool_uses_and_declares_the_tier_native() {
        let provider = AnthropicProvider::with_context_editing(
            "test-key",
            "claude-opus-5-5",
            Some(AnthropicContextEditing { trigger_input_tokens: Some(60_000), keep_tool_uses: Some(4), ..Default::default() }),
        );
        assert_eq!(
            provider.native_context_tiers(),
            NativeContextTiers { tool_result_clearing: true, compaction: false }
        );
        let prepared = provider.prepare_context_request(&question_context(), &[], None, None).unwrap();
        assert_eq!(prepared["betas"], json!(["context-management-2025-06-27"]));
        assert!(prepared["body"].get("betas").is_none());
        assert_eq!(
            prepared["body"]["context_management"],
            json!({"edits": [{
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "input_tokens", "value": 60_000},
                "keep": {"type": "tool_uses", "value": 4},
            }]})
        );
    }

    #[test]
    fn context_editing_keeps_caller_betas_and_edits() {
        let provider = AnthropicProvider::with_context_editing("test-key", "claude-opus-5-5", Some(Default::default()));
        let ext = json!({
            "betas": ["other-beta", "context-management-2025-06-27"],
            "context_management": {"edits": [{"type": "clear_thinking_20251015"}]},
        });
        let prepared = provider.prepare_context_request(&question_context(), &[], Some(&ext), None).unwrap();
        assert_eq!(prepared["betas"], json!(["other-beta", "context-management-2025-06-27"]));
        assert_eq!(
            prepared["body"]["context_management"]["edits"],
            json!([{"type": "clear_thinking_20251015"}, {"type": "clear_tool_uses_20250919"}])
        );
    }

    #[test]
    fn compaction_sends_compact_beside_clear_tool_uses_and_declares_both_tiers() {
        let provider = AnthropicProvider::with_context_editing("test-key", "claude-opus-5-5", Some(Default::default()))
            .with_compaction(AnthropicCompaction { trigger_input_tokens: Some(120_000), ..Default::default() });
        assert_eq!(
            provider.native_context_tiers(),
            NativeContextTiers { tool_result_clearing: true, compaction: true }
        );
        let prepared = provider.prepare_context_request(&question_context(), &[], None, None).unwrap();
        assert_eq!(prepared["betas"], json!(["context-management-2025-06-27", "compact-2026-01-12"]));
        assert_eq!(
            prepared["body"]["context_management"]["edits"],
            json!([
                {"type": "clear_tool_uses_20250919"},
                {"type": "compact_20260112", "trigger": {"type": "input_tokens", "value": 120_000}},
            ])
        );
    }

    #[tokio::test]
    async fn a_streamed_compaction_block_is_replayed_on_the_next_request() {
        use futures::StreamExt;
        let provider = AnthropicProvider::with_model("test-key", "claude-opus-5-5")
            .with_compaction(AnthropicCompaction::default());
        let events = [
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "compaction", "content": null}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "compaction_delta", "content": "Summary: the user wants a scraper."}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Here it is."}}),
            json!({"type": "content_block_stop", "index": 1}),
        ];
        let sse: String = events.iter().map(|event| format!("data: {event}\n\n")).collect();
        let bytes = futures::stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from(sse))]);
        let streamed: Vec<_> = parse_anthropic_sse(bytes, provider.stream_native_blocks.clone()).collect().await;
        assert!(streamed.iter().all(Result::is_ok));
        provider.commit_stream_replay("Here it is.", &[]);

        let context = InternalRenderedContext {
            turns: vec![CoreMessage::user("question"), CoreMessage::assistant("Here it is."), CoreMessage::user("add retries")],
            ..question_context()
        };
        let prepared = provider.prepare_context_request(&context, &[], None, None).unwrap();
        // The API ignores everything before the block, so the turn that carries it is sent first.
        assert_eq!(prepared["body"]["messages"].as_array().unwrap().len(), 2);
        let assistant = &prepared["body"]["messages"][0];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"][0], json!({"type": "compaction", "content": "Summary: the user wants a scraper."}));
        assert_eq!(assistant["content"][1]["text"], "Here it is.");
    }

    #[test]
    fn prepared_request_freezes_native_replay_blocks() {
        let provider = AnthropicProvider::new("test-key");
        let tool_calls = vec![ToolCall {
            id: "call".into(),
            name: "lookup".into(),
            arguments: json!({}),
        }];
        let mut assistant = CoreMessage::assistant("answer");
        assistant.tool_calls = tool_calls.clone();
        let context = InternalRenderedContext {
            system_text: String::new(),
            system_stable: String::new(),
            system_knowledge: String::new(),
            turns: vec![CoreMessage::user("question"), assistant],
            state_turn: None,
            frozen_prefix_len: None,
            budget_overflow: None,
            turn_items: Vec::new(),
            estimated_tokens: 0,
            turn_replays: Vec::new(),
        };
        let before = provider
            .prepare_context_request(&context, &[], None, None)
            .unwrap();
        provider.remember_native_blocks(
            "answer",
            &tool_calls,
            vec![
                json!({ "type": "thinking", "thinking": "reasoning", "signature": "signed" }),
                json!({ "type": "text", "text": "answer" }),
            ],
        );
        let frozen = provider
            .prepare_context_request(&context, &[], None, None)
            .unwrap();
        assert_ne!(before, frozen);
        assert_eq!(
            frozen["body"]["messages"][1]["content"][0]["type"],
            "thinking"
        );
        provider.native_assistant_blocks.lock().unwrap().clear();
        assert_eq!(
            before,
            provider
                .prepare_context_request(&context, &[], None, None)
                .unwrap()
        );
        // stream_prepared consumes this owned body without consulting the replay cache again.
        assert_eq!(
            frozen["body"]["messages"][1]["content"][0]["signature"],
            "signed"
        );
    }

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
        let context = InternalRenderedContext {
            system_text: "system rules".into(),
            system_stable: "system rules".into(),
            system_knowledge: String::new(),
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
            budget_overflow: None,
            turn_items: Vec::new(),
            estimated_tokens: 0,
            turn_replays: Vec::new(),
        };

        let (system, messages, _) =
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
        let context = InternalRenderedContext {
            system_text: "rules\nknowledge".into(),
            system_stable: "rules".into(),
            system_knowledge: "knowledge".into(),
            turns: vec![CoreMessage::user("hi")],
            state_turn: None,
            frozen_prefix_len: None,
            budget_overflow: None,
            turn_items: Vec::new(),
            estimated_tokens: 0,
            turn_replays: Vec::new(),
        };
        let (system, _msgs, _) =
            context_to_anthropic(&context, CacheBreakpointStrategy::Default, |_, _| None).unwrap();
        // 2 system + 2 message = 4 (tool breakpoint dropped) — at the limit, ok.
        assert!(assert_cache_budget(system.as_ref(), 3).is_ok());
    }

    #[test]
    fn state_turn_rendered_after_history_without_cache_control() {
        // History is the cacheable prefix; the volatile state turn is the tail.
        let context = InternalRenderedContext {
            system_text: String::new(),
            system_stable: String::new(),
            system_knowledge: String::new(),
            turns: vec![
                CoreMessage::user("earlier question"),
                CoreMessage::assistant("earlier answer"),
            ],
            state_turn: Some(CoreMessage::user("[TASK STATE] goal: g\n\nProceed.")),
            frozen_prefix_len: None,
            budget_overflow: None,
            turn_items: Vec::new(),
            estimated_tokens: 0,
            turn_replays: Vec::new(),
        };
        let (_system, messages, _) =
            context_to_anthropic(&context, CacheBreakpointStrategy::Default, |_, _| None).unwrap();
        // history (2) + state (1) appended last
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "user");
        assert!(
            messages[2]["content"]
                .as_str()
                .unwrap()
                .contains("[TASK STATE]")
        );
        // the state turn carries NO cache breakpoint (it is the uncached tail)
        assert!(messages[2].get("cache_control").is_none());
        // the last history turn DID get a breakpoint (read anchor) — it became a block array
        assert!(messages[1]["content"].is_array() || messages[0]["content"].is_array());
    }

    /// Scripted Messages API: one SSE body per streamed request, one JSON per create, in order.
    #[derive(Default)]
    struct ScriptedTransport {
        streams: Mutex<std::collections::VecDeque<String>>,
        creates: Mutex<std::collections::VecDeque<Value>>,
        streamed: Mutex<Vec<(Value, Vec<String>)>>,
        created: Mutex<Vec<(Value, Vec<String>)>>,
    }

    impl ScriptedTransport {
        fn new(streams: Vec<String>, creates: Vec<Value>) -> Arc<Self> {
            Arc::new(Self {
                streams: Mutex::new(streams.into()),
                creates: Mutex::new(creates.into()),
                ..Default::default()
            })
        }
        fn push_stream(&self, sse: String) {
            self.streams.lock().unwrap().push_back(sse);
        }
    }

    #[async_trait]
    impl AnthropicTransport for ScriptedTransport {
        async fn stream(
            &self,
            body: &Value,
            betas: &[String],
        ) -> Result<futures::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>> {
            self.streamed.lock().unwrap().push((body.clone(), betas.to_vec()));
            let sse = self.streams.lock().unwrap().pop_front().unwrap_or_default();
            Ok(futures::stream::iter(vec![Ok(bytes::Bytes::from(sse))]).boxed())
        }
        async fn create(&self, body: &Value, betas: &[String]) -> Result<Value> {
            self.created.lock().unwrap().push((body.clone(), betas.to_vec()));
            self.creates.lock().unwrap().pop_front().ok_or_else(|| Error::Other("no scripted response".into()))
        }
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|event| format!("data: {event}\n\n")).collect()
    }

    fn text_sse(text: &str, input: u32, output: u32) -> String {
        sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": input, "output_tokens": 0}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": output}}),
        ])
    }

    fn pause_sse() -> String {
        sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 0, "output_tokens": 0}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "compaction", "content": null}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "compaction_delta", "content": "Summary so far."}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "compaction"}, "usage": {"output_tokens": 0}}),
        ])
    }

    async fn drain(provider: &AnthropicProvider, context: &InternalRenderedContext) -> String {
        use futures::StreamExt;
        let mut stream = provider.stream(context, &[], None, None).await.unwrap();
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta { delta } = event.unwrap() {
                text.push_str(&delta);
            }
        }
        text
    }

    fn turns(turns: Vec<CoreMessage>) -> InternalRenderedContext {
        InternalRenderedContext { turns, ..question_context() }
    }

    #[tokio::test]
    async fn a_paused_compaction_continues_with_the_block_appended_then_replays_it_first() {
        let transport = ScriptedTransport::new(vec![pause_sse(), text_sse("Continued.", 3, 1)], vec![]);
        let provider = AnthropicProvider::with_model("k", "claude-opus-5-5")
            .with_compaction(AnthropicCompaction {
                pause_after_compaction: Some(AnthropicCompactionPause { keep_recent_messages: 0 }),
                ..Default::default()
            })
            .with_transport(transport.clone());
        let first = vec![CoreMessage::user("u1")];
        assert_eq!(drain(&provider, &turns(first.clone())).await, "Continued.");
        provider.commit_stream_replay("Continued.", &[]);
        {
            let streamed = transport.streamed.lock().unwrap();
            assert_eq!(streamed[0].0["context_management"]["edits"], json!([{"type": "compact_20260112", "pause_after_compaction": true}]));
            assert_eq!(streamed[1].0["context_management"]["edits"], json!([{"type": "compact_20260112"}]));
            assert_eq!(
                streamed[1].0["messages"].as_array().unwrap().last().unwrap(),
                &json!({"role": "assistant", "content": [{"type": "compaction", "content": "Summary so far."}]})
            );
        }
        let mut next = first;
        next.extend([CoreMessage::assistant("Continued."), CoreMessage::user("u2")]);
        let prepared = provider.prepare_context_request(&turns(next), &[], None, None).unwrap();
        let messages = prepared["body"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"][0], json!({"type": "compaction", "content": "Summary so far."}));
        assert_eq!(messages[0]["content"][1]["text"], "Continued.");
    }

    #[tokio::test]
    async fn a_paused_compaction_keeps_the_newest_history_behind_the_block() {
        let transport = ScriptedTransport::new(vec![pause_sse(), text_sse("Done.", 3, 1)], vec![]);
        let provider = AnthropicProvider::with_model("k", "claude-opus-5-5")
            .with_compaction(AnthropicCompaction {
                pause_after_compaction: Some(AnthropicCompactionPause { keep_recent_messages: 1 }),
                ..Default::default()
            })
            .with_transport(transport.clone());
        let calls = vec![ToolCall { id: "t1".into(), name: "look".into(), arguments: json!({}) }];
        let mut tool_turn = CoreMessage::assistant("");
        tool_turn.tool_calls = calls.clone();
        provider.remember_native_blocks("", &calls, vec![
            json!({"type": "thinking", "thinking": "", "signature": "sig"}),
            json!({"type": "tool_use", "id": "t1", "name": "look", "input": {}}),
        ]);
        let history = vec![
            CoreMessage::user("u1"),
            CoreMessage::assistant("a1"),
            CoreMessage::user("u2"),
            tool_turn,
            CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: CompactString::new("t1"),
                output: "seen".into(),
                is_error: false,
                durable_content: None,
            }]),
        ];
        assert_eq!(drain(&provider, &turns(history.clone())).await, "Done.");
        let anchor = json!({"role": "assistant", "content": [
            {"type": "compaction", "content": "Summary so far."},
            {"type": "tool_use", "id": "t1", "name": "look", "input": {}},
        ]});
        let continuation = transport.streamed.lock().unwrap()[1].0.clone();
        assert_eq!(continuation["messages"][0], anchor);
        assert_eq!(continuation["messages"].as_array().unwrap().len(), 2);

        provider.commit_stream_replay("Done.", &[]);
        let mut next = history;
        next.extend([CoreMessage::assistant("Done."), CoreMessage::user("u3")]);
        let prepared = provider.prepare_context_request(&turns(next), &[], None, None).unwrap();
        assert_eq!(prepared["body"]["messages"][0], anchor);
        assert_eq!(prepared["body"]["messages"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn on_demand_compaction_summarizes_a_turn_in_the_background_then_swaps_the_block_in() {
        let block = json!({"type": "compaction", "content": "Summary of u1..a1.", "signature": "sig-1"});
        let transport = ScriptedTransport::new(
            vec![text_sse("a2", 90, 20)],
            vec![json!({"stop_reason": "compaction", "content": [block.clone()], "usage": {}})],
        );
        let provider = AnthropicProvider::with_model("k", "claude-opus-5-5")
            .with_on_demand_compaction(AnthropicOnDemandCompaction {
                compact_at_input_tokens: 100,
                instructions: Some("Keep the open questions.".into()),
                max_tokens: None,
            })
            .unwrap()
            .with_transport(transport.clone());
        assert_eq!(provider.native_context_tiers(), NativeContextTiers { tool_result_clearing: false, compaction: true });
        assert!(AnthropicProvider::new("k")
            .with_on_demand_compaction(AnthropicOnDemandCompaction::default())
            .is_err());
        let history = vec![CoreMessage::user("u1"), CoreMessage::assistant("a1"), CoreMessage::user("u2")];
        let context = InternalRenderedContext { state_turn: Some(CoreMessage::user("[TASK STATE] t")), ..turns(history.clone()) };
        drain(&provider, &context).await;
        provider.compaction_idle().await;
        {
            let created = transport.created.lock().unwrap();
            let (request, betas) = &created[0];
            assert_eq!(betas, &vec!["compact-2026-09-04".to_string()]);
            assert_eq!(request["compaction"], json!({"type": "summarize", "instructions": "Keep the open questions."}));
            assert!(request.get("stream").is_none());
            let roles: Vec<_> = request["messages"].as_array().unwrap().iter().map(|m| m["role"].clone()).collect();
            assert_eq!(roles, vec![json!("user"), json!("assistant"), json!("user")]);
        }

        let mut next = history;
        next.extend([CoreMessage::assistant("a2"), CoreMessage::user("u3")]);
        let context = InternalRenderedContext { state_turn: Some(CoreMessage::user("[TASK STATE] t")), ..turns(next) };
        let prepared = provider.prepare_context_request(&context, &[], None, None).unwrap();
        assert_eq!(prepared["betas"], json!(["compact-2026-09-04"]));
        assert_eq!(prepared["body"]["messages"][0], json!({"role": "assistant", "content": [block]}));
        assert_eq!(prepared["body"]["messages"].as_array().unwrap().len(), 4);

        // A renewal rebuilt the history: the summary no longer stands for its front.
        transport.push_stream(text_sse("ok", 1, 1));
        let renewed = turns(vec![CoreMessage::user("renewed goal")]);
        let prepared = provider.prepare_context_request(&renewed, &[], None, None).unwrap();
        assert!(prepared.get("betas").is_none());
        assert!(!prepared["body"]["messages"].to_string().contains("compaction"));
    }
}
