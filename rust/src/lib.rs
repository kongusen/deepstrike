pub mod agent;
pub mod harness;
pub mod knowledge;
pub mod memory;
pub mod providers;
pub mod safety;
pub mod signals;
pub mod tools;

#[cfg(test)]
mod tests;

pub use agent::{Agent, AgentOptions, EvalLoopHarness, HarnessLoop, RunEvent, SinglePassHarness};
pub use harness::{Harness, HarnessOutcome, HarnessRequest, QualityGate};
pub use knowledge::KnowledgeSource;
pub use memory::{DreamResult, DreamStore, WorkingMemory};
pub use providers::{LLMProvider, StreamEvent, TokenUsage, ProviderToolSpec};
pub use providers::anthropic::AnthropicProvider;
pub use providers::openai::{OpenAIProvider, deepseek, kimi, minimax, ollama, qwen};
pub use safety::{Permission, PermissionDecision, PermissionManager, PermissionMode};
pub use signals::{RuntimeSignal, ScheduledPrompt, SignalSource, SignalGateway, GatewayReceiver};
pub use tools::{RegisteredTool, execute_tools, read_file_tool};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("provider error: {0}")]
    Provider(String),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
