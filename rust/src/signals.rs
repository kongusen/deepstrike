use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSignal {
    pub source: String,
    pub signal_type: String,
    pub urgency: String,
    pub payload: serde_json::Value,
    pub dedupe_key: Option<String>,
    pub recipient: Option<String>,
    pub deadline_ms: Option<u64>,
    pub coalesce_key: Option<String>,
    pub coalesced_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalDeliveryReceipt {
    pub delivery_id: String,
    pub lease_token: String,
}

#[derive(Debug, Clone)]
pub struct SignalClaim {
    pub delivery_id: String,
    pub lease_token: String,
    pub signal_id: String,
    pub delivery_attempt: u32,
    pub signal: RuntimeSignal,
}

/// Feed signals from any external source (cron, webhook, queue).
#[async_trait]
pub trait SignalSource: Send + Sync {
    async fn claim_signal(&self) -> crate::Result<Option<SignalClaim>>;
    async fn ack_signal(&self, receipt: &SignalDeliveryReceipt) -> crate::Result<bool>;
    async fn nack_signal(&self, receipt: &SignalDeliveryReceipt) -> crate::Result<bool>;
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
            source: "cron".into(),
            signal_type: "job".into(),
            urgency: "normal".into(),
            payload: serde_json::json!({
                "goal": self.goal,
                "criteria": self.criteria,
                "run_at_ms": self.run_at_ms,
            }),
            dedupe_key: Some(format!("cron:{}:{}", self.goal, self.run_at_ms)),
            recipient: None,
            deadline_ms: None,
            coalesce_key: None,
            coalesced_count: 1,
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
            state: tokio::sync::Mutex::new(GatewayReceiverState {
                rx: self.tx.subscribe(),
                pending: None,
            }),
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
    state: tokio::sync::Mutex<GatewayReceiverState>,
}

struct PendingDelivery {
    delivery_id: String,
    signal_id: String,
    delivery_attempt: u32,
    lease_token: Option<String>,
    signal: RuntimeSignal,
}

struct GatewayReceiverState {
    rx: broadcast::Receiver<RuntimeSignal>,
    pending: Option<PendingDelivery>,
}

#[async_trait]
impl SignalSource for GatewayReceiver {
    async fn claim_signal(&self) -> crate::Result<Option<SignalClaim>> {
        let mut state = self.state.lock().await;
        if state.pending.is_none() {
            let signal = match state.rx.recv().await {
                Ok(signal) => signal,
                Err(broadcast::error::RecvError::Lagged(_))
                | Err(broadcast::error::RecvError::Closed) => return Ok(None),
            };
            state.pending = Some(PendingDelivery {
                delivery_id: uuid::Uuid::new_v4().to_string(),
                signal_id: uuid::Uuid::new_v4().to_string(),
                delivery_attempt: 0,
                lease_token: None,
                signal,
            });
        }
        let pending = state
            .pending
            .as_mut()
            .expect("pending delivery initialized");
        if pending.lease_token.is_some() {
            return Ok(None);
        }
        pending.delivery_attempt = pending.delivery_attempt.saturating_add(1);
        let lease_token = uuid::Uuid::new_v4().to_string();
        pending.lease_token = Some(lease_token.clone());
        Ok(Some(SignalClaim {
            delivery_id: pending.delivery_id.clone(),
            lease_token,
            signal_id: pending.signal_id.clone(),
            delivery_attempt: pending.delivery_attempt,
            signal: pending.signal.clone(),
        }))
    }

    async fn ack_signal(&self, receipt: &SignalDeliveryReceipt) -> crate::Result<bool> {
        let mut state = self.state.lock().await;
        let matches = state.pending.as_ref().is_some_and(|pending| {
            pending.delivery_id == receipt.delivery_id
                && pending.lease_token.as_deref() == Some(receipt.lease_token.as_str())
        });
        if matches {
            state.pending = None;
        }
        Ok(matches)
    }

    async fn nack_signal(&self, receipt: &SignalDeliveryReceipt) -> crate::Result<bool> {
        let mut state = self.state.lock().await;
        let Some(pending) = state.pending.as_mut() else {
            return Ok(false);
        };
        if pending.delivery_id != receipt.delivery_id
            || pending.lease_token.as_deref() != Some(receipt.lease_token.as_str())
        {
            return Ok(false);
        }
        pending.lease_token = None;
        Ok(true)
    }
}
