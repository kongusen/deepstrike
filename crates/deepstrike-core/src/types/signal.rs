use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSignal {
    pub id: Uuid,
    pub source: SignalSource,
    pub signal_type: SignalType,
    pub urgency: Urgency,
    pub summary: CompactString,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<CompactString>,
    /// Target a specific agent/session loop. `None` ⇒ broadcast (drained by any puller).
    /// The canonical key is the recipient's `sessionId` (see R1 / L0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<CompactString>,
    /// Optional topic for pub/sub-style fan-out. Carried through for subscribers;
    /// multi-subscriber routing is deferred (field-only this phase).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<CompactString>,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSource {
    Cron,
    Gateway,
    Heartbeat,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalType {
    Event,
    Job,
    Alert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

impl RuntimeSignal {
    pub fn new(
        source: SignalSource,
        signal_type: SignalType,
        urgency: Urgency,
        summary: impl Into<CompactString>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            source,
            signal_type,
            urgency,
            summary: summary.into(),
            payload: serde_json::Value::Null,
            dedupe_key: None,
            recipient: None,
            topic: None,
            timestamp_ms: 0,
        }
    }

    pub fn with_dedupe(mut self, key: impl Into<CompactString>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    pub fn with_recipient(mut self, recipient: impl Into<CompactString>) -> Self {
        self.recipient = Some(recipient.into());
        self
    }

    pub fn with_topic(mut self, topic: impl Into<CompactString>) -> Self {
        self.topic = Some(topic.into());
        self
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }

    pub fn with_timestamp(mut self, ts: u64) -> Self {
        self.timestamp_ms = ts;
        self
    }
}
