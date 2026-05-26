use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

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
        Self {
            goal: goal.into(),
            run_at_ms,
            criteria: Vec::new(),
        }
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

/// Entry point for all external signals into the agent.
///
/// - Cron scheduling: fires [`ScheduledPrompt`]s at the right wall-clock time
/// - Webhook ingestion: push any [`RuntimeSignal`] directly via [`ingest`](SignalGateway::ingest)
/// - Subscribe pattern: callers receive signals via [`subscribe`](SignalGateway::subscribe)
pub struct SignalGateway {
    tx: broadcast::Sender<RuntimeSignal>,
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl SignalGateway {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            tx,
            tasks: Mutex::new(HashMap::new()),
        }
    }

    /// Subscribe to all signals emitted by this gateway.
    /// Returns a [`GatewayReceiver`] that implements [`SignalSource`].
    pub fn subscribe(&self) -> GatewayReceiver {
        GatewayReceiver {
            rx: tokio::sync::Mutex::new(self.tx.subscribe()),
        }
    }

    /// Schedule a [`ScheduledPrompt`] to fire at its `run_at_ms`. Idempotent by goal+time.
    pub fn schedule(&self, prompt: ScheduledPrompt) {
        let key = format!("cron:{}:{}", prompt.goal, prompt.run_at_ms);
        let mut guard = self.tasks.lock().unwrap();
        if guard.contains_key(&key) {
            return;
        }

        let tx = self.tx.clone();
        let signal = prompt.to_signal();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let delay_ms = prompt.run_at_ms.saturating_sub(now_ms);

        let handle = tokio::spawn(async move {
            if delay_ms > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            }
            let _ = tx.send(signal);
        });
        guard.insert(key, handle);
    }

    /// Cancel a previously scheduled prompt.
    pub fn cancel(&self, goal: &str, run_at_ms: u64) {
        let key = format!("cron:{goal}:{run_at_ms}");
        if let Some(h) = self.tasks.lock().unwrap().remove(&key) {
            h.abort();
        }
    }

    /// Ingest a raw external signal (e.g. from a webhook handler).
    pub fn ingest(&self, signal: RuntimeSignal) {
        let _ = self.tx.send(signal);
    }

    /// Abort all pending scheduled tasks.
    pub fn destroy(&self) {
        for (_, h) in self.tasks.lock().unwrap().drain() {
            h.abort();
        }
    }
}

impl Default for SignalGateway {
    fn default() -> Self {
        Self::new()
    }
}

/// A broadcast receiver from a [`SignalGateway`] that implements [`SignalSource`].
pub struct GatewayReceiver {
    rx: tokio::sync::Mutex<broadcast::Receiver<RuntimeSignal>>,
}

#[async_trait]
impl SignalSource for GatewayReceiver {
    async fn next_signal(&self) -> crate::Result<Option<RuntimeSignal>> {
        match self.rx.lock().await.recv().await {
            Ok(sig) => Ok(Some(sig)),
            Err(broadcast::error::RecvError::Lagged(_)) => Ok(None),
            Err(broadcast::error::RecvError::Closed) => Ok(None),
        }
    }
}
