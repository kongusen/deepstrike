//! ReplayProvider — an LLMProvider that emits previously-recorded assistant messages
//! instead of calling a real LLM API.
//!
//! Rust port of node/src/runtime/replay-provider.ts. See that file for the full design
//! rationale. Distinct from `provider_replay` (the session-repair reasoning-content cache
//! that does NOT skip LLM calls).
//!
//! Cost-accounting under replay:
//! - `input_tokens` is ESTIMATED from the rendered context (NOT a recorded value).
//! - `output_tokens` is estimated from the replayed message body with the provider tokenizer.
//! - `cache_read_input_tokens` / `cache_creation_input_tokens` emitted as 0.

use std::sync::Mutex;

use async_trait::async_trait;
use deepstrike_core::context::renderer::InternalRenderedContext;
use deepstrike_core::runtime::session::ProviderReplay;
use deepstrike_core::types::message::{Content, ContentPart, CoreMessage, ToolCall, ToolSchema};
use futures::Stream;

use crate::Result;
use crate::providers::{LLMProvider, ProviderRunState, RuntimePolicy, StreamEvent};

/// Options for `ReplayProvider`.
pub struct ReplayProviderOpts {
    /// Maps a rendered text payload to a token count. Defaults to `chars / 4`.
    pub tokenizer: Option<Box<dyn Fn(&str) -> u32 + Send + Sync>>,
    /// When true, `stream()` wraps to the start once the fixture is exhausted instead of erroring.
    pub wrap: bool,
}

impl Default for ReplayProviderOpts {
    fn default() -> Self {
        Self {
            tokenizer: None,
            wrap: false,
        }
    }
}

fn default_tokenizer(text: &str) -> u32 {
    let len = text.chars().count() as u32;
    (len + 3) / 4
}

/// LLMProvider that dequeues recorded assistant messages instead of calling an API.
pub struct ReplayProvider {
    messages: Vec<CoreMessage>,
    cursor: Mutex<usize>,
    tokenizer: Box<dyn Fn(&str) -> u32 + Send + Sync>,
    wrap: bool,
}

impl ReplayProvider {
    pub fn new(messages: Vec<CoreMessage>) -> Self {
        Self::with_opts(messages, ReplayProviderOpts::default())
    }

    pub fn with_opts(messages: Vec<CoreMessage>, opts: ReplayProviderOpts) -> Self {
        Self {
            messages,
            cursor: Mutex::new(0),
            tokenizer: opts
                .tokenizer
                .unwrap_or_else(|| Box::new(default_tokenizer)),
            wrap: opts.wrap,
        }
    }

    pub fn consumed(&self) -> usize {
        *self.cursor.lock().unwrap()
    }

    pub fn remaining(&self) -> usize {
        let c = *self.cursor.lock().unwrap();
        self.messages.len().saturating_sub(c)
    }

    pub fn reset(&self) {
        *self.cursor.lock().unwrap() = 0;
    }

    fn pull(&self) -> Result<CoreMessage> {
        let mut c = self.cursor.lock().unwrap();
        if *c >= self.messages.len() {
            if self.wrap && !self.messages.is_empty() {
                *c = 0;
            } else {
                return Err(crate::Error::Other(format!(
                    "ReplayProvider: fixture exhausted (consumed={}, total={})",
                    *c,
                    self.messages.len()
                )));
            }
        }
        let msg = self.messages[*c].clone();
        *c += 1;
        Ok(msg)
    }

    fn estimate_input_tokens(
        &self,
        context: &InternalRenderedContext,
        tools: &[ToolSchema],
    ) -> u32 {
        (self.tokenizer)(&render_context_to_text(context, tools))
    }
}

fn render_context_to_text(context: &InternalRenderedContext, tools: &[ToolSchema]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !context.system_text.is_empty() {
        parts.push(context.system_text.clone());
    }
    if !context.system_stable.is_empty() {
        parts.push(context.system_stable.clone());
    }
    if !context.system_knowledge.is_empty() {
        parts.push(context.system_knowledge.clone());
    }
    if let Some(turn) = &context.state_turn {
        if let Some(t) = message_text(turn) {
            parts.push(t);
        }
    }
    for turn in &context.turns {
        if let Some(t) = message_text(turn) {
            parts.push(t);
        }
        for tc in &turn.tool_calls {
            parts.push(format!("{} {}", tc.name, tc.arguments.to_string()));
        }
    }
    for tool in tools {
        parts.push(format!(
            "{} {} {}",
            tool.name, tool.description, tool.parameters
        ));
    }
    parts.join("\n")
}

fn message_text(m: &CoreMessage) -> Option<String> {
    match &m.content {
        Content::Text(s) if !s.is_empty() => Some(s.clone()),
        Content::Parts(parts) => {
            let joined: String = parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    ContentPart::ToolResult { output, .. } => Some(output.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

#[async_trait]
impl LLMProvider for ReplayProvider {
    fn runtime_policy(&self) -> RuntimePolicy {
        RuntimePolicy::default()
    }

    fn peek_provider_replay(
        &self,
        _content: &str,
        _tool_calls: &[ToolCall],
    ) -> Option<ProviderReplay> {
        None
    }

    fn seed_provider_replay(
        &self,
        _content: &str,
        _tool_calls: &[ToolCall],
        _replay: &ProviderReplay,
    ) {
    }

    async fn stream(
        &self,
        context: &InternalRenderedContext,
        tools: &[ToolSchema],
        _extensions: Option<&serde_json::Value>,
        _state: Option<&ProviderRunState>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent>> + Send + Unpin>> {
        let msg = self.pull()?;
        let input_tokens = self.estimate_input_tokens(context, tools);
        let content = message_text(&msg).unwrap_or_default();
        let output_tokens = (self.tokenizer)(&content);

        let mut events: Vec<Result<StreamEvent>> = Vec::new();
        events.push(Ok(StreamEvent::Usage {
            total_tokens: input_tokens + output_tokens,
            input_tokens,
            output_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            // I1: replay provider does not model the cache layer.
            cache_read_input_tokens_by_slot: None,
            // Replay never truncates — a recorded transcript is already complete.
            stop_reason: None,
        }));
        if let Some(text) = message_text(&msg) {
            if !text.is_empty() {
                events.push(Ok(StreamEvent::TextDelta { delta: text }));
            }
        }
        for tc in &msg.tool_calls {
            events.push(Ok(StreamEvent::ToolCall {
                id: tc.id.to_string(),
                name: tc.name.to_string(),
                arguments: tc.arguments.clone(),
            }));
        }
        Ok(Box::new(futures::stream::iter(events)))
    }
}
