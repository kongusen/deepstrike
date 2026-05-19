use async_trait::async_trait;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::types::message::{Content, Message, Role, ToolCall, ToolSchema};
use compact_str::CompactString;
use futures::{Stream, StreamExt};

pub mod anthropic;
pub mod openai;

/// Opaque per-run state owned by the provider (e.g. OpenAI Responses continuation).
pub type ProviderRunState = serde_json::Value;

/// Per-model execution policy returned by providers.
/// Three-layer merge in RuntimeRunner: RuntimeOptions > provider > defaults.
#[derive(Debug, Clone, Default)]
pub struct RuntimePolicy {
    pub max_turns: Option<u32>,
    pub timeout_ms: Option<u64>,
}

/// Stream event emitted by providers.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta { delta: String },
    ThinkingDelta { delta: String },
    ToolCall { id: String, name: String, arguments: serde_json::Value },
    /// Token usage from the provider (e.g. OpenAI `stream_options.include_usage`).
    Usage { total_tokens: u32 },
    Done,
}

#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Optional per-run state for protocol-native continuation (e.g. Responses API).
    fn create_run_state(&self) -> Option<ProviderRunState> {
        None
    }

    /// Per-model runtime policy. Overridden by RuntimeOptions fields when set.
    fn runtime_policy(&self) -> RuntimePolicy {
        RuntimePolicy::default()
    }

    /// Non-streaming completion — default collects from `stream`.
    async fn complete(
        &self,
        context: &RenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&serde_json::Value>,
    ) -> crate::Result<Message> {
        let mut stream = self.stream(context, tools, extensions, None).await?;
        collect_message_from_stream(&mut stream).await
    }

    async fn stream(
        &self,
        context: &RenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&serde_json::Value>,
        state: Option<&ProviderRunState>,
    ) -> crate::Result<Box<dyn Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>>;
}

pub async fn collect_message_from_stream(
    stream: &mut (dyn Stream<Item = crate::Result<StreamEvent>> + Send + Unpin),
) -> crate::Result<Message> {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    while let Some(evt) = stream.next().await {
        match evt? {
            StreamEvent::TextDelta { delta } => content.push_str(&delta),
            StreamEvent::ThinkingDelta { .. } => {}
            StreamEvent::ToolCall { id, name, arguments } => {
                tool_calls.push(ToolCall {
                    id: CompactString::new(&id),
                    name: CompactString::new(&name),
                    arguments,
                });
            }
            StreamEvent::Usage { .. } | StreamEvent::Done => {}
        }
    }
    Ok(Message {
        role: Role::Assistant,
        content: Content::Text(content),
        tool_calls,
        token_count: None,
    })
}

/// Token consumption for a single LLM call.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl TokenUsage {
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}

/// A tool specification in provider-facing format (parameters as a parsed JSON value).
#[derive(Debug, Clone)]
pub struct ProviderToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}
