use super::agent::AgentIdentity;
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

