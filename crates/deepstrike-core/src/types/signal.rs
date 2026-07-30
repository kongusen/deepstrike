use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSignal {
    /// The *business* signal identity, verbatim as its author wrote it.
    ///
    /// A free string rather than a UUID on purpose (§7.7): the canonical wire's `SignalId` is a
    /// branded ref the caller owns, and a kernel that could only hold UUIDs would have to mint a
    /// second identity for every non-UUID signal — which is exactly the fabricated identity the
    /// disposition/expiry/displacement audit facts must never report. [`RuntimeSignal::new`]
    /// still mints a UUID when no author supplied one, so a self-issued signal is still unique.
    pub id: CompactString,
    pub source: SignalSource,
    pub signal_type: SignalType,
    pub urgency: Urgency,
    pub summary: CompactString,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<CompactString>,
    /// Absolute journal-clock deadline. Reaching it may promote urgency when enabled by policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    /// Merge only with an unconsumed queued signal carrying the same key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_key: Option<CompactString>,
    /// Number of logical signals represented by this delivery.
    #[serde(default = "default_coalesced_count")]
    pub coalesced_count: u32,
    /// Host-side routing key for a targeted delivery; `None` ⇒ broadcast (drained by any puller).
    ///
    /// § Task 11 · opaque to the kernel, which never reads it — the SDK's signal gateway matches it
    /// while deciding *whose* queue a signal lands in, before the delivery reaches the ABI. The
    /// kernel's own addressing is `SignalTarget` (operation or logical task), which has no session
    /// slot (§22.6). Do not re-document this as a session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<CompactString>,
    pub timestamp_ms: u64,
}

const fn default_coalesced_count() -> u32 {
    1
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
            id: CompactString::new(Uuid::new_v4().to_string()),
            source,
            signal_type,
            urgency,
            summary: summary.into(),
            payload: serde_json::Value::Null,
            dedupe_key: None,
            deadline_ms: None,
            coalesce_key: None,
            coalesced_count: 1,
            recipient: None,
            timestamp_ms: 0,
        }
    }

    /// Adopt the author's own signal identity instead of the minted one.
    pub fn with_id(mut self, id: impl Into<CompactString>) -> Self {
        self.id = id.into();
        self
    }

    pub fn with_dedupe(mut self, key: impl Into<CompactString>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    pub fn with_recipient(mut self, recipient: impl Into<CompactString>) -> Self {
        self.recipient = Some(recipient.into());
        self
    }

    pub fn with_deadline(mut self, deadline_ms: u64) -> Self {
        self.deadline_ms = Some(deadline_ms);
        self
    }

    pub fn with_coalesce(mut self, key: impl Into<CompactString>) -> Self {
        self.coalesce_key = Some(key.into());
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
