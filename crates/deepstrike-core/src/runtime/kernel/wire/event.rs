//! External events — facts the host observed (spec §7.7).

use serde::{Deserialize, Serialize};

use super::scalar::{AttemptId, BoundedJson, DeliveryId, FiniteF64, SignalId, TaskId, WireU64};
use super::syscall::SyscallRequest;

/// An external event is a **fact**, not an effect result: a signal can arrive with no pending
/// effect, and a child completes long after its spawn was acknowledged.
///
/// No variant carries a host wall clock. The envelope's `observed_at_ms` is the only clock the
/// kernel admits — a result that carries its own `Date.now()` produces different bytes for the
/// same intent, which turns idempotent replay into a conflict fault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalEvent {
    DeliverSignal(DeliverSignal),
    ChildCompleted(ChildCompleted),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverSignal {
    /// Host delivery identity — distinct from the logical signal identity, so a redelivery is
    /// recognisable as the same signal.
    pub delivery_id: DeliveryId,
    pub attempt: u32,
    pub signal: LogicalSignal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalSignal {
    pub signal_id: SignalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SignalSourceKind>,
    #[serde(default)]
    pub target: SignalTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<SignalUrgency>,
    #[serde(default, skip_serializing_if = "BoundedJson::is_null")]
    pub payload: BoundedJson,
    /// Metadata only. TTL, deadlines and admission all use the envelope's accepted time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timestamp_ms: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    /// How long this signal may wait before its urgency is raised one tier — a **duration**, not
    /// an instant.
    ///
    /// A duration is the only shape that can be a canonical input. The deadline it implies is
    /// anchored to the envelope's accepted time, which the kernel already owns, so a redelivery of
    /// the same bytes anchors identically; an absolute `deadline_ms` would be a second host clock
    /// on the wire (DEC-2, §11.2) and would make the same intent decode to a different deadline on
    /// every retry.
    ///
    /// Inert unless `signal_policy.deadline_escalation` is on — that switch is the operation's
    /// statement that it wants waiting to change priority at all. `Some(0)` is a legitimate value:
    /// escalate on admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_after_ms: Option<WireU64>,
}

impl LogicalSignal {
    pub fn new(signal_id: SignalId) -> Self {
        Self {
            signal_id,
            source: None,
            target: SignalTarget::default(),
            urgency: None,
            payload: BoundedJson::null(),
            source_timestamp_ms: None,
            dedupe_key: None,
            escalate_after_ms: None,
        }
    }

    /// The urgency this delivery reaches at admission, once a due `escalate_after_ms` is applied.
    ///
    /// The kernel has to know this *before* the router moves: the one effect a delivery can
    /// publish is `PreemptTasks`, and DEC-8 says an undeclared effect is refused with nothing
    /// mutated. Reading the wire urgency alone would let an escalated-to-critical signal reach the
    /// router and only then discover the host cannot stop its children.
    pub fn effective_urgency(&self, escalation_enabled: bool) -> SignalUrgency {
        let urgency = self.urgency.unwrap_or(SignalUrgency::Normal);
        let due_on_admission = self.escalate_after_ms.is_some_and(|after| after.get() == 0);
        if escalation_enabled && due_on_admission {
            urgency.escalated()
        } else {
            urgency
        }
    }
}

/// A signal targets the operation or one logical task. Host session ids are not a target.
///
/// Both variants are newtypes over their own struct rather than inline struct variants: serde
/// cannot apply `deny_unknown_fields` to an inline variant of an internally tagged enum, so an
/// inline shape would silently accept `{"kind":"task","task_id":"t","session_id":"s"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalTarget {
    Operation(OperationTarget),
    Task(TaskTarget),
}

impl Default for SignalTarget {
    fn default() -> Self {
        Self::Operation(OperationTarget {})
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationTarget {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTarget {
    pub task_id: TaskId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSourceKind {
    Cron,
    Gateway,
    Heartbeat,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalUrgency {
    Low,
    Normal,
    High,
    Critical,
}

impl SignalUrgency {
    /// One tier up, saturating at `Critical`. Mirrors the router's own escalation so the
    /// pre-admission check and the router cannot disagree about what a due deadline produces.
    pub fn escalated(self) -> Self {
        match self {
            Self::Low => Self::Normal,
            Self::Normal => Self::High,
            Self::High | Self::Critical => Self::Critical,
        }
    }
}

/// A child task attempt finished.
///
/// `parent_requests` is the **only** legal child→parent request channel: the requests enter P1
/// with `ChildAttempt` causation inside this same transition. GAP-4: the completion itself is a
/// fact and commits unconditionally, while each request is adjudicated independently — a denied
/// request produces a structured rejection observation and changes neither the completion nor
/// its siblings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildCompleted {
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub result: ChildResult,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_requests: Vec<SyscallRequest>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildResult {
    #[serde(default)]
    pub status: ChildStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageFacts>,
    /// Observation-only quality score (verifier/judge). Finite by construction, and never a
    /// branch input — thresholds that gate kernel decisions use fixed-point `Ppm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<FiniteF64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    #[default]
    Completed,
    Failed,
    Cancelled,
}

/// Host-observed resource facts for one child attempt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
}
