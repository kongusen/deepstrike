pub mod governance;
pub mod harness_loop;
pub mod run_event;
pub mod runtime;
pub mod harness;
pub mod knowledge;
pub mod memory;
pub mod providers;
pub mod safety;
pub mod signals;
pub mod tools;

#[cfg(test)]
mod tests;

pub use run_event::RunEvent;
pub use governance::{Governance, GovernanceVerdict};
pub use harness_loop::{EvalLoopHarness, HarnessLoop, SinglePassHarness};
pub use runtime::{ToolSuspendRequest, ToolSuspendHandler, RunContext};
pub use deepstrike_core::governance::permission::PermissionAction;
pub use runtime::{FileSessionLog, InMemorySessionLog, SessionEntry, SessionLog};
pub use runtime::{CredentialVault, EnvCredentialVault, InMemoryCredentialVault, ChainedCredentialVault};
pub use runtime::{ExecutionPlane, LocalExecutionPlane};
pub use runtime::{ProcessSandboxPlane, SandboxOptions};
pub use runtime::{McpProxyPlane, McpServerConfig};
pub use runtime::{RemoteVpcOptions, RemoteVpcPlane};
pub use runtime::{collect_text, RuntimeOptions, RuntimeRunner};
pub use providers::RuntimePolicy;
pub use harness::{Harness, HarnessOutcome, HarnessRequest, QualityGate};
pub use knowledge::KnowledgeSource;
pub use memory::{DreamResult, DreamStore, WorkingMemory};
pub use providers::{LLMProvider, StreamEvent, TokenUsage, ProviderToolSpec, ProviderRunState};
pub use providers::anthropic::AnthropicProvider;
pub use providers::openai::{OpenAIProvider, deepseek, kimi, minimax, ollama, qwen};
pub use safety::{Permission, PermissionDecision, PermissionManager, PermissionMode};
pub use signals::{RuntimeSignal, ScheduledPrompt, SignalSource, SignalGateway, GatewayReceiver};
pub use tools::{RegisteredTool, ToolChunk, ToolSession, ToolStep, TextToolSession, execute_tools, read_file_tool, validate_tool_arguments};
pub use deepstrike_core::context::renderer::RenderedContext;

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
