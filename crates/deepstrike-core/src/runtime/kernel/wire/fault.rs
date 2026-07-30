//! Fault taxonomy and the closed prepare result (spec §7.13).
//!
//! Two shapes, one property. [`KernelPreparation`] is a **closed** union with no `faults` field on
//! any success arm, so "a step that carries both actions and faults" is not constructible — the
//! historical `KernelStep { faults: Vec<_> }` made a partially-applied transition representable,
//! and one host then ignored the vector entirely while another turned it into a panic.
//!
//! Every rejection is therefore zero-mutation by construction: [`KernelPreparation::Rejected`]
//! carries a fault and nothing else — no record to append, no token to commit, no step to publish.

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use super::effect::{Digest, wire_opaque_ref};
use super::scalar::{SCALAR_ERROR_MARKER, WireScalarError, WireU64};

// ---------------------------------------------------------------------------------------------
// §7.13 · fault codes
// ---------------------------------------------------------------------------------------------

/// Why the kernel refused an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelFaultCode {
    MalformedEnvelope,
    VersionMismatch,
    OperationMismatch,
    ClockRegression,
    InvalidLifecycle,
    InvalidConfig,
    InvalidAuthority,
    ResourceLimitExceeded,
    DuplicateInputConflict,
    /// A host resolved a pending effect with the success payload of another effect kind, or
    /// resolved an effect the kernel is not waiting on.
    UnexpectedEffectOutcome,
    TransactionConflict,
    CheckpointIncompatible,
    CheckpointCorrupted,
    /// A durable **journal record** no longer matches the digest it carries, or the chain it sits
    /// in no longer links up.
    ///
    /// Deliberately not folded into [`Self::CheckpointCorrupted`]: the two have different recovery
    /// ladders. A corrupted checkpoint can be answered by falling back to an older checkpoint and
    /// replaying more tail; a corrupted record is a journal-integrity failure with nothing to fall
    /// back to, because the record chain *is* the operation's history.
    RecordCorrupted,
    /// The **only** retryable code (GAP-2).
    ///
    /// Returned as a `Rejected` preparation with zero mutation — the input was never accepted. The
    /// host takes a checkpoint candidate, installs it and acks it (§12.3), then retries with the
    /// *same* `input_id`. That retry is a brand-new prepare and must not fall into
    /// [`Self::DuplicateInputConflict`].
    ///
    /// This code exists to replace the snapshot-overflow latch, whose double consequence — snapshots
    /// permanently disabled **and** `prepare_step` permanently refused — was a hard failure on the
    /// only durable host and a silent degradation everywhere else.
    CheckpointRequired,
    /// The kernel was about to emit an effect whose kind the operation's `host_effect_support`
    /// declaration does not cover (DEC-8, GAP-6).
    ///
    /// Fail-closed **before emission**: the fault is committed and no effect is published, so the
    /// host is never handed an effect it cannot execute. This is the declaration-time half of the
    /// pair whose runtime half is
    /// [`HostEffectFailureKind::ProtocolError`](super::effect::HostEffectFailureKind::ProtocolError)
    /// (DEC-7).
    UnsupportedEffect,
}

impl KernelFaultCode {
    pub const ALL: [Self; 16] = [
        Self::MalformedEnvelope,
        Self::VersionMismatch,
        Self::OperationMismatch,
        Self::ClockRegression,
        Self::InvalidLifecycle,
        Self::InvalidConfig,
        Self::InvalidAuthority,
        Self::ResourceLimitExceeded,
        Self::DuplicateInputConflict,
        Self::UnexpectedEffectOutcome,
        Self::TransactionConflict,
        Self::CheckpointIncompatible,
        Self::CheckpointCorrupted,
        Self::RecordCorrupted,
        Self::CheckpointRequired,
        Self::UnsupportedEffect,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MalformedEnvelope => "malformed_envelope",
            Self::VersionMismatch => "version_mismatch",
            Self::OperationMismatch => "operation_mismatch",
            Self::ClockRegression => "clock_regression",
            Self::InvalidLifecycle => "invalid_lifecycle",
            Self::InvalidConfig => "invalid_config",
            Self::InvalidAuthority => "invalid_authority",
            Self::ResourceLimitExceeded => "resource_limit_exceeded",
            Self::DuplicateInputConflict => "duplicate_input_conflict",
            Self::UnexpectedEffectOutcome => "unexpected_effect_outcome",
            Self::TransactionConflict => "transaction_conflict",
            Self::CheckpointIncompatible => "checkpoint_incompatible",
            Self::CheckpointCorrupted => "checkpoint_corrupted",
            Self::RecordCorrupted => "record_corrupted",
            Self::CheckpointRequired => "checkpoint_required",
            Self::UnsupportedEffect => "unsupported_effect",
        }
    }

    /// Whether re-submitting the same `input_id` unchanged can succeed. Exactly one code says yes.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::CheckpointRequired)
    }
}

impl fmt::Display for KernelFaultCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured rejection. Malformed JSON, unknown fields/variants and revision mismatches all
/// arrive here too — the same shape in all four languages, rather than one language's exception.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelFault {
    pub code: KernelFaultCode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

impl KernelFault {
    pub fn new(code: KernelFaultCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn is_retryable(&self) -> bool {
        self.code.is_retryable()
    }
}

impl fmt::Display for KernelFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            f.write_str(self.code.as_str())
        } else {
            write!(f, "{}: {}", self.code.as_str(), self.message)
        }
    }
}

impl std::error::Error for KernelFault {}

wire_opaque_ref!(
    /// Handle for a prepared-but-uncommitted transition. Handed out only by
    /// [`KernelPreparation::Prepared`]: a replay has nothing to commit and a rejection has nothing
    /// to abort.
    PrepareToken,
    "prepare token"
);

// ---------------------------------------------------------------------------------------------
// §7.13 · the closed prepare result
// ---------------------------------------------------------------------------------------------

/// The result of preparing one input.
///
/// Generic over the durable record and the planned step: Task 6 owns those two contracts, and this
/// task fixes only the **shape** of the result — which arms exist, and what each may carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum KernelPreparation<Record, Step> {
    /// A new record was built and is waiting for the host to append it and then commit the token.
    Prepared(PreparedTransition<Record, Step>),
    /// This input maps onto a record that **already exists**. No new record is produced and
    /// `step_seq` points at the existing one.
    ///
    /// Two triggers, one shape:
    ///
    /// 1. **input-level replay** — the same `input_id` with the same canonical payload;
    /// 2. **effect-level dedup** (DEC-1) — a *new* `input_id` resolving an already-completed
    ///    effect with the same payload; the cancellation dedup branch behaves identically.
    ///
    /// The second trigger is the one that used to be reported as `Prepared` while returning the
    /// old `step_seq`. A host then built a transaction whose `step_seq` did not increase, its CAS
    /// successor check rejected it as an integrity error, and the run died — a live dead end on
    /// the only durable host.
    Replayed(ReplayedTransition<Record, Step>),
    /// Nothing was accepted, nothing was staged, no state moved.
    Rejected(RejectedTransition),
}

impl<Record, Step> KernelPreparation<Record, Step> {
    pub fn record(&self) -> Option<&Record> {
        match self {
            Self::Prepared(prepared) => Some(&prepared.record),
            Self::Replayed(replayed) => replayed.record.as_ref(),
            Self::Rejected(_) => None,
        }
    }

    /// Only a `Prepared` transition has something to commit.
    pub fn token(&self) -> Option<&PrepareToken> {
        match self {
            Self::Prepared(prepared) => Some(&prepared.token),
            Self::Replayed(_) | Self::Rejected(_) => None,
        }
    }

    pub fn step(&self) -> Option<&Step> {
        match self {
            Self::Prepared(prepared) => Some(&prepared.planned_step),
            Self::Replayed(replayed) => replayed.committed_step.as_ref(),
            Self::Rejected(_) => None,
        }
    }

    /// The sequence of the record this preparation refers to. `Prepared` does not have one yet —
    /// its record is not in the journal until the host appends it.
    pub fn step_seq(&self) -> Option<WireU64> {
        match self {
            Self::Replayed(replayed) => Some(replayed.step_seq),
            Self::Prepared(_) | Self::Rejected(_) => None,
        }
    }

    pub fn fault(&self) -> Option<&KernelFault> {
        match self {
            Self::Rejected(rejected) => Some(&rejected.fault),
            Self::Prepared(_) | Self::Replayed(_) => None,
        }
    }

    /// Whether this preparation left the operation byte-identical. True for exactly the rejected
    /// arm — the type-level statement of the zero-mutation rule.
    pub fn is_zero_mutation(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }

    /// Whether the host may re-submit the same `input_id` unchanged.
    pub fn is_retryable(&self) -> bool {
        self.fault().is_some_and(KernelFault::is_retryable)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedTransition<Record, Step> {
    pub token: PrepareToken,
    pub record: Record,
    /// Ephemeral. The planned step is returned to the caller but never enters the durable record —
    /// a rebuild re-derives it from the canonical input and checks its digest, which is what keeps
    /// rendered provider contexts out of the journal.
    pub planned_step: Step,
}

/// The answer to an input this operation already accepted.
///
/// Two strengths of answer, and §12.3 rule 10 is what decides which one a caller gets:
///
/// * **reproduction** — above a restored checkpoint's `base_step_seq` the runtime still holds the
///   record and the step it committed, so a redelivery is answered with both;
/// * **acknowledgement** — below it, the step was never durable (§22.12) and the record may already
///   have been reclaimed under an acked checkpoint. What survives is the ledger entry, and that is
///   what answers: this input is step N, record D. A caller retrying a lost response learns exactly
///   what it needed to; nothing is fabricated to fill the other two fields.
///
/// `record_digest` is therefore the one field that is always present — it is the identity of the
/// transition, where `record` and `committed_step` are the (possibly reclaimed) *contents* of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayedTransition<Record, Step> {
    #[serde(default = "Option::default")]
    pub record: Option<Record>,
    pub record_digest: Digest,
    #[serde(default = "Option::default")]
    pub committed_step: Option<Step>,
    pub step_seq: WireU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectedTransition {
    pub fault: KernelFault,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::super::*;

    /// Stand-ins for the Task 6 record/step types. [`KernelPreparation`] is generic over them
    /// precisely so this task can fix the *shape* of the prepare result without squatting on the
    /// durable-record contract.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StubRecord {
        step_seq: WireU64,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StubStep {
        effects: u32,
    }

    type Preparation = KernelPreparation<StubRecord, StubStep>;

    fn prepared() -> Preparation {
        KernelPreparation::Prepared(PreparedTransition {
            token: PrepareToken::new("prepare-1").unwrap(),
            record: StubRecord {
                step_seq: WireU64::new(4),
            },
            planned_step: StubStep { effects: 1 },
        })
    }

    fn replayed() -> Preparation {
        KernelPreparation::Replayed(ReplayedTransition {
            record: Some(StubRecord {
                step_seq: WireU64::new(2),
            }),
            record_digest: Digest::new("sha256:replayed").unwrap(),
            committed_step: Some(StubStep { effects: 1 }),
            step_seq: WireU64::new(2),
        })
    }

    fn rejected(code: KernelFaultCode) -> Preparation {
        KernelPreparation::Rejected(RejectedTransition {
            fault: KernelFault::new(code, "rejected"),
        })
    }

    // -----------------------------------------------------------------------------------------
    // fault codes (§7.13, GAP-2 / GAP-6)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_fault_taxonomy_is_the_sixteen_declared_codes() {
        let labels: BTreeSet<&str> = KernelFaultCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            labels,
            BTreeSet::from([
                "malformed_envelope",
                "version_mismatch",
                "operation_mismatch",
                "clock_regression",
                "invalid_lifecycle",
                "invalid_config",
                "invalid_authority",
                "resource_limit_exceeded",
                "duplicate_input_conflict",
                "unexpected_effect_outcome",
                "transaction_conflict",
                "checkpoint_incompatible",
                "checkpoint_corrupted",
                "record_corrupted",
                "checkpoint_required",
                "unsupported_effect",
            ])
        );
        assert_eq!(KernelFaultCode::ALL.len(), 16);

        for code in KernelFaultCode::ALL {
            let text = serde_json::to_string(&code).unwrap();
            assert_eq!(text, format!("\"{}\"", code.as_str()));
            let back: KernelFaultCode = serde_json::from_str(&text).unwrap();
            assert_eq!(back, code);
        }
    }

    #[test]
    fn checkpoint_required_is_the_only_retryable_fault_code() {
        for code in KernelFaultCode::ALL {
            assert_eq!(
                code.is_retryable(),
                code == KernelFaultCode::CheckpointRequired,
                "{} must{} be retryable",
                code.as_str(),
                if code == KernelFaultCode::CheckpointRequired {
                    ""
                } else {
                    " not"
                }
            );
        }
        assert!(KernelFault::new(KernelFaultCode::CheckpointRequired, "").is_retryable());
        assert!(!KernelFault::new(KernelFaultCode::DuplicateInputConflict, "").is_retryable());
    }

    #[test]
    fn unknown_fault_codes_are_rejected() {
        for raw in ["\"snapshot_overflow\"", "\"ok\"", "3", "null"] {
            assert!(
                serde_json::from_str::<KernelFaultCode>(raw).is_err(),
                "{raw} must not decode as a fault code"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // zero mutation (§7.13)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_rejected_preparation_carries_no_record_no_token_and_no_step() {
        for code in KernelFaultCode::ALL {
            let preparation = rejected(code);
            assert!(preparation.record().is_none(), "{}", code.as_str());
            assert!(preparation.token().is_none(), "{}", code.as_str());
            assert!(preparation.step().is_none(), "{}", code.as_str());
            assert!(preparation.step_seq().is_none(), "{}", code.as_str());
            assert_eq!(preparation.fault().map(|f| f.code), Some(code));
            assert!(preparation.is_zero_mutation());
        }
    }

    #[test]
    fn a_successful_preparation_can_never_carry_a_fault() {
        for preparation in [prepared(), replayed()] {
            assert!(preparation.fault().is_none());
            assert!(!preparation.is_zero_mutation());

            let mut all = BTreeSet::new();
            let value = serde_json::to_value(&preparation).unwrap();
            if let serde_json::Value::Object(map) = &value {
                for key in map.keys() {
                    all.insert(key.clone());
                }
            }
            assert!(
                !all.contains("fault") && !all.contains("faults"),
                "a fault-bearing success step must not be constructible: {value}"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // the closed prepare result (§7.13, DEC-1)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn preparation_has_exactly_three_shapes() {
        let statuses: BTreeSet<String> = [
            prepared(),
            replayed(),
            rejected(KernelFaultCode::InvalidLifecycle),
        ]
        .iter()
        .map(|preparation| {
            serde_json::to_value(preparation).unwrap()["status"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        assert_eq!(
            statuses,
            BTreeSet::from([
                "prepared".to_string(),
                "replayed".to_string(),
                "rejected".to_string(),
            ])
        );

        for shape in ["accepted", "deferred", "prepared_with_faults"] {
            let raw = json!({ "status": shape });
            assert!(
                serde_json::from_value::<Preparation>(raw).is_err(),
                "{shape} is not a preparation shape"
            );
        }
    }

    #[test]
    fn replayed_points_at_the_existing_record_step_seq() {
        let preparation = replayed();
        assert_eq!(preparation.step_seq(), Some(WireU64::new(2)));
        assert_eq!(
            preparation.record().map(|record| record.step_seq),
            Some(WireU64::new(2)),
            "a replay must point at the record that already exists, not mint a new one"
        );
        assert!(
            preparation.token().is_none(),
            "a replay has nothing to commit, so it hands out no prepare token"
        );
    }

    #[test]
    fn preparation_round_trips_and_rejects_unknown_fields() {
        for preparation in [
            prepared(),
            replayed(),
            rejected(KernelFaultCode::CheckpointRequired),
        ] {
            let value = serde_json::to_value(&preparation).unwrap();
            let back: Preparation = serde_json::from_value(value).unwrap();
            assert_eq!(back, preparation);
        }

        let extra = json!({
            "status": "rejected",
            "fault": { "code": "invalid_lifecycle", "message": "terminal already committed" },
            "retry_after_ms": 500,
        });
        assert!(serde_json::from_value::<Preparation>(extra).is_err());
    }
}
