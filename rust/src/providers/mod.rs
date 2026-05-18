use async_trait::async_trait;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::types::message::ToolSchema;

pub mod anthropic;
pub mod openai;

/// Stream event emitted by providers.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta { delta: String },
    ThinkingDelta { delta: String },
    ToolCall { id: String, name: String, arguments: serde_json::Value },
    Done,
}

#[async_trait]
pub trait LLMProvider: Send + Sync {
    async fn stream(
        &self,
        context: &RenderedContext,
        tools: &[ToolSchema],
        extensions: Option<&serde_json::Value>,
    ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>>;
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
