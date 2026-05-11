use async_trait::async_trait;
use deepstrike_core::types::message::{Message, ToolSchema};

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
        messages: &[Message],
        tools: &[ToolSchema],
        extensions: Option<&serde_json::Value>,
    ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>>;
}
