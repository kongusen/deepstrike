use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeepStrikeError {
    #[error("context pressure exceeded: rho={pressure:.2}")]
    ContextOverflow { pressure: f64 },

    #[error("tool '{name}' denied by governance: {reason}")]
    GovernanceDenied { name: String, reason: String },

    #[error("signal routing failed: {0}")]
    SignalError(String),

    #[error("orchestration cycle detected in task graph")]
    OrchestrationCycle,

    #[error("token budget exhausted: used={used}, limit={limit}")]
    TokenBudgetExhausted { used: u64, limit: u64 },

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, DeepStrikeError>;
