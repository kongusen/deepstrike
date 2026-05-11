use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSignal {
    pub kind: String, // "interrupt" | "scheduled" | "external"
    pub payload: serde_json::Value,
    pub priority: u8,
}

/// Feed signals from any external source (cron, webhook, queue).
#[async_trait]
pub trait SignalSource: Send + Sync {
    async fn next_signal(&self) -> crate::Result<Option<RuntimeSignal>>;
}

#[derive(Debug, Clone)]
pub struct ScheduledPrompt {
    pub goal: String,
    pub run_at_ms: u64,
    pub criteria: Vec<String>,
}

impl ScheduledPrompt {
    pub fn new(goal: impl Into<String>, run_at_ms: u64) -> Self {
        Self { goal: goal.into(), run_at_ms, criteria: Vec::new() }
    }

    pub fn to_signal(&self) -> RuntimeSignal {
        RuntimeSignal {
            kind: "scheduled".into(),
            payload: serde_json::json!({
                "goal": self.goal,
                "criteria": self.criteria,
                "run_at_ms": self.run_at_ms,
            }),
            priority: 0,
        }
    }
}
