use super::agent::AgentIdentity;
use super::message::ToolCall;
use super::signal::RuntimeSignal;

/// Caller identity passed through the governance pipeline.
/// Wraps AgentIdentity for unified agent identification.
pub type CallerContext = AgentIdentity;

/// Attention disposition — what to do when a signal arrives.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalDisposition {
    Ignore,
    Observe,
    Queue,
    Run {
        priority: u8,
    },
    Interrupt,
    InterruptNow,
    /// Router accepted the signal but the queue is full; signal was dropped.
    /// SDK should surface this for backpressure handling.
    Dropped,
}

/// Trait for attention policies — SDK layer provides concrete implementations.
pub trait AttentionPolicy: Send + Sync {
    fn evaluate(&self, signal: &RuntimeSignal, is_running: bool) -> SignalDisposition;
}

/// Governance verdict for a tool call.
#[derive(Debug, Clone)]
pub enum GovernanceVerdict {
    Allow,
    Deny { stage: &'static str, reason: String },
    RateLimited { retry_after_ms: u64 },
    AskUser { reason: String },
}

/// Trait for custom permission checks injected from SDK layer.
pub trait PermissionCheck: Send + Sync {
    fn check(&self, call: &ToolCall, caller: &CallerContext) -> Option<GovernanceVerdict>;
}

/// Trait for custom veto checks injected from SDK layer.
/// Returns `Some(reason)` to veto the call, `None` to pass.
/// FFI-friendly: SDK implements this trait directly rather than passing closures.
pub trait VetoCheck: Send + Sync {
    fn check(&self, call: &ToolCall, caller: &CallerContext) -> Option<String>;
}

/// Blanket impl so plain closures still work in the in-process Rust API
/// without the SDK having to define a struct for each check.
impl<F> VetoCheck for F
where
    F: Fn(&ToolCall, &CallerContext) -> Option<String> + Send + Sync,
{
    fn check(&self, call: &ToolCall, caller: &CallerContext) -> Option<String> {
        (self)(call, caller)
    }
}
