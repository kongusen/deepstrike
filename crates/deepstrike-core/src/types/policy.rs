use super::agent::AgentIdentity;

/// Caller identity passed through the governance pipeline.
/// Wraps AgentIdentity for unified agent identification.
pub type CallerContext = AgentIdentity;

/// Attention disposition — what to do when a signal arrives.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalDisposition {
    Ignore,
    Observe,
    Queue,
    Run,
    Interrupt,
    InterruptNow,
    /// Router rejected the signal because the bounded queue could not admit or displace an entry.
    /// Its dedupe key is not committed, so the SDK may safely retry after applying backpressure.
    Dropped,
}

impl SignalDisposition {
    /// Canonical snake_case wire label — the single source for kernel events,
    /// session logs, and all FFI bindings.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Observe => "observe",
            Self::Queue => "queue",
            Self::Run => "run",
            Self::Interrupt => "interrupt",
            Self::InterruptNow => "interrupt_now",
            Self::Dropped => "dropped",
        }
    }
}

/// Governance verdict for a tool call.
#[derive(Debug, Clone)]
pub enum GovernanceVerdict {
    Allow,
    Deny { stage: &'static str, reason: String },
    RateLimited { retry_after_ms: u64 },
    AskUser { reason: String },
}
