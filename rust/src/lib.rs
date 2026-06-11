pub mod governance;
pub mod harness;
pub mod harness_loop;
pub mod knowledge;
pub mod memory;
pub mod providers;
pub mod run_event;
pub mod runtime;
pub mod safety;
pub mod signals;
pub mod tools;

#[cfg(test)]
mod tests;

pub use deepstrike_core::context::renderer::RenderedContext;
pub use deepstrike_core::governance::permission::PermissionAction;
pub use deepstrike_core::governance::quota::ResourceQuota;
pub use deepstrike_core::mm::memory::{
    MemoryKind, MemoryMetadata, MemoryPolicy, MemoryQuery, MemoryRetrieval, MemoryWriteRequest,
};
pub use deepstrike_core::orchestration::workflow::{
    ClassifyAndAct, WorkflowNode, WorkflowSpec, classify_and_act, fanout_synthesize,
    generate_and_filter,
};
pub use deepstrike_core::orchestration::workflow::{JudgeMatch, WorkflowRun, WorkflowSpawnInfo};
pub use governance::{Governance, GovernanceVerdict};
pub use harness::{Harness, HarnessOutcome, HarnessRequest, QualityGate};
pub use harness_loop::{EvalLoopHarness, HarnessLoop, SinglePassHarness};
pub use knowledge::KnowledgeSource;
pub use memory::{DreamResult, DreamStore, WorkingMemory};
pub use providers::RuntimePolicy;
pub use providers::anthropic::AnthropicProvider;
pub use providers::openai::{OpenAIProvider, deepseek, kimi, minimax, ollama, qwen};
pub use providers::{LLMProvider, ProviderRunState, ProviderToolSpec, StreamEvent, TokenUsage};
pub use run_event::RunEvent;
pub use runtime::{
    ChainedCredentialVault, CredentialVault, EnvCredentialVault, InMemoryCredentialVault,
};
pub use runtime::{ExecutionPlane, LocalExecutionPlane};
pub use runtime::{FileSessionLog, InMemorySessionLog, SessionEntry, SessionLog};
pub use runtime::{McpProxyPlane, McpServerConfig};
pub use runtime::{
    AttentionPolicy, GovernancePolicy, MemoryWriteRateLimit, NativeOsProfile, OsProfile,
    SchedulerBudget, assert_native_profile, default_native_governance_policy, os_profile,
    DEFAULT_NATIVE_ATTENTION_POLICY,
};
pub use runtime::{
    MilestoneEvaluationContext, MilestoneEvaluationHandler, MilestonePolicy, RuntimeOptions,
    RuntimeRunner, collect_text,
};
pub use runtime::{
    PermissionRequest, PermissionRequestHandler, PermissionResponse, RunContext,
    ToolSuspendHandler, ToolSuspendRequest,
};
pub use runtime::{ProcessSandboxPlane, SandboxOptions};
pub use runtime::{RemoteVpcOptions, RemoteVpcPlane};
pub use safety::{Permission, PermissionDecision, PermissionManager, PermissionMode};
pub use signals::{GatewayReceiver, RuntimeSignal, ScheduledPrompt, SignalGateway, SignalSource};
pub use tools::{
    RegisteredTool, TextToolSession, ToolChunk, ToolSession, ToolStep, execute_tools,
    read_file_tool, validate_tool_arguments,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("provider error: {0}")]
    Provider(String),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tool execution failed: {output}")]
    ToolExecutionFailed {
        output: String,
        is_fatal: bool,
        error_kind: Option<deepstrike_core::types::message::ToolErrorKind>,
    },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
