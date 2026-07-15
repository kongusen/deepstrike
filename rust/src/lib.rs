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
// Session entropy (heartbeat watch source): the kernel emits `entropy_sample` /
// `entropy_alert` observations through the shared JSON ABI; these types are for hosts
// configuring the watch (`set_entropy_watch` / `configure_run.entropy_watch`) or folding
// their own samples when driving the kernel manually.
pub use deepstrike_core::{EntropySample, EntropyTracker, EntropyWatchConfig};
pub use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryPolicy, MemoryProvenance, MemoryQuery, MemoryRecall,
    MemoryRecord, MemoryScope, MemoryTrustLevel,
};
// Workflow surface (DELIBERATE floor, not a gap): the Rust SDK has no `run_workflow` driver — the
// node/python/wasm SDKs own async node execution. These re-exports are for MANUAL driving: build a
// spec with the templates, hold a `WorkflowRun` (a pure state machine), call `ready_batch()` /
// `spawn_info()` / `record_completion()` from your own executor. Everything the drivers do is
// reachable this way; a batteries-included Rust driver lands only when a real consumer needs it.
pub use deepstrike_core::orchestration::workflow::{
    fanout_synthesize, gen_eval, generate_and_filter, verify_rules, WorkflowNode, WorkflowSpec,
};
pub use deepstrike_core::orchestration::workflow::{JudgeMatch, WorkflowRun, WorkflowSpawnInfo};
pub use governance::{Governance, GovernanceVerdict};
pub use harness::{Criterion, CriterionResult, Verdict};
pub use harness_loop::{
    AttemptBody, AttemptBodyContext, AttemptBodyEvent, AttemptBodyStream, AttemptJudge,
    AttemptLoop, AttemptLoopEvent, AttemptLoopStream, AttemptOutcome, AttemptOutcomeKind,
    AttemptRequest, CarryContext, CarryPolicy, ContinueSession, DigestFn, DigestFuture,
    FreshWithDigest, FreshWithFeedback, HybridJudge, JudgeContext, JudgeResult, LlmEvalJudge,
    PassHook, PassHookFuture, PreparedAttempt, RuntimeAttemptBody, StopPolicy, VerdictFn,
    VerdictFnJudge,
};
pub use knowledge::KnowledgeSource;
pub use memory::{DreamStore, InMemoryDreamStore, WorkingMemory};
pub use providers::RuntimePolicy;
pub use providers::anthropic::AnthropicProvider;
pub use providers::openai::{OpenAIProvider, deepseek, kimi, minimax, ollama, qwen};
pub use providers::{LLMProvider, ProviderRunState, ProviderToolSpec, StreamEvent, TokenUsage};
pub use run_event::RunEvent;
pub use runtime::eval::{build_eval_messages, judge, parse_verdict, verdict_output_schema};
pub use runtime::eval::{Criterion as EvalCriterion, Verdict as EvalVerdict};
pub use runtime::replay_provider::{ReplayProvider, ReplayProviderOpts};
pub use runtime::replay_fixture::{extract_recorded_messages, extract_recorded_messages_from_entries};
pub use runtime::{
    assert_native_profile, default_native_governance_policy, os_profile, GovernancePolicy,
    MemoryWriteRateLimit, NativeOsProfile, OsProfile, SchedulerPolicyConfig, SignalPolicy,
    DEFAULT_NATIVE_SIGNAL_POLICY,
};
pub use runtime::{
    collect_text, MilestoneEvaluationContext, MilestoneEvaluationHandler, MilestonePolicy,
    RuntimeOptions, RuntimeRunner,
};
pub use runtime::{
    ChainedCredentialVault, CredentialVault, EnvCredentialVault, InMemoryCredentialVault,
};
pub use runtime::{ExecutionPlane, LocalExecutionPlane};
pub use runtime::{FileSessionLog, InMemorySessionLog, SessionEntry, SessionLog};
pub use runtime::{McpProxyPlane, McpServerConfig};
pub use runtime::{
    PermissionRequest, PermissionRequestHandler, PermissionResponse, RunContext,
    ToolSuspendHandler, ToolSuspendRequest,
};
pub use runtime::{ProcessSandboxPlane, SandboxOptions};
pub use runtime::{RemoteVpcOptions, RemoteVpcPlane};
pub use safety::{Permission, PermissionDecision, PermissionManager, PermissionMode};
pub use signals::{
    GatewayReceiver, RuntimeSignal, ScheduledPrompt, SignalClaim, SignalDeliveryReceipt,
    SignalGateway, SignalSource,
};
pub use tools::{
    RegisteredTool, SafeToolResult, TextToolSession, ToolChunk, ToolEnvelope, ToolEnvelopeFail,
    ToolEnvelopeOk, ToolSession, ToolStep, execute_tools, fail, ok, read_file_tool, safe_tool,
    tool_fail, validate_tool_arguments,
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
    /// Tool author signalled a structured failure with optional machine-readable `code` and a
    /// self-correcting `hint`. Parity with the Node/Python `ToolError` + `safe_tool` envelope:
    /// surfaced to the model as JSON `{message, code?, hint?}` (via `format_tool_error`) so the
    /// agent can branch on `code` instead of pattern-matching a free-form string.
    #[error("{output}")]
    ToolFail {
        output: String,
        code: Option<String>,
        hint: Option<String>,
        is_fatal: bool,
        error_kind: Option<deepstrike_core::types::message::ToolErrorKind>,
    },
    #[error("{0}")]
    Other(String),
}

/// Error-aware serialization for tool-execution error paths. Replaces `e.to_string()` at the
/// sites that hand the model a failure message:
///
/// - `Error::Tool(s)` → `s` (drops the `"tool error: "` prefix from `e.to_string()`).
/// - `Error::ToolExecutionFailed { output, .. }` → `output` (drops the `"tool execution failed: "` prefix).
/// - `Error::ToolFail { output, code:None, hint:None, .. }` → `output`.
/// - `Error::ToolFail { output, code, hint, .. }` (either set) → JSON `{message, code?, hint?}`.
/// - everything else → `e.to_string()` (the `thiserror`-formatted string).
pub fn format_tool_error(e: &Error) -> String {
    match e {
        Error::Tool(s) => s.clone(),
        Error::ToolExecutionFailed { output, .. } => output.clone(),
        Error::ToolFail { output, code, hint, .. } => {
            if code.is_none() && hint.is_none() {
                return output.clone();
            }
            let mut obj = serde_json::Map::with_capacity(3);
            obj.insert("message".to_string(), serde_json::Value::String(output.clone()));
            if let Some(c) = code {
                obj.insert("code".to_string(), serde_json::Value::String(c.clone()));
            }
            if let Some(h) = hint {
                obj.insert("hint".to_string(), serde_json::Value::String(h.clone()));
            }
            serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_else(|_| output.clone())
        }
        _ => e.to_string(),
    }
}

pub type Result<T> = std::result::Result<T, Error>;
