//! Framework-first 0.2.70 verifiable-runtime contracts.
//!
//! The types in this module are deliberately storage-neutral.  They accept evidence bytes that
//! have already been obtained by a host, SDK, or durable-store adapter; they never open a path,
//! call a provider, or mutate runtime state.  `deepstrike` is one adapter over this API.

use serde::{Deserialize, Serialize};

use super::chain_validator::{SegmentReport, ValidationReport, validate_with_checkpoint};
use super::kernel::wire::record::KernelRecord;

pub const REPORT_SCHEMA: &str = "verifiable-report/v2";
pub const FORK_SCHEMA: &str = "verifiable-fork/v2";

#[derive(Debug, Default, Deserialize)]
struct JsonEvidence {
    #[serde(default)]
    journal: Vec<Vec<u8>>,
    #[serde(default)]
    session_logs: Vec<Vec<Vec<u8>>>,
    #[serde(default)]
    checkpoints: Vec<Vec<u8>>,
}

#[derive(Debug, Deserialize)]
struct JsonOperationRequest {
    operation_id: String,
    command: String,
    #[serde(default)]
    evidence: JsonEvidence,
    #[serde(default)]
    strict: bool,
    #[serde(default)]
    require_complete: bool,
    at_step: Option<u64>,
}

/// Execute one framework operation from a JSON request for language bindings.
///
/// The JSON bridge is intentionally a transport adapter, not a second implementation. It decodes
/// byte arrays into [`EvidenceBundle`], invokes [`VerifiableOperation`], and serializes the typed
/// result. Hosts still own storage access and may use the typed Rust API directly.
pub fn operation_json(request: &str) -> Result<String, String> {
    let request: JsonOperationRequest = serde_json::from_str(request)
        .map_err(|error| format!("invalid verifiable request: {error}"))?;
    let operation = VerifiableOperation::new(
        request.operation_id,
        EvidenceBundle::new(
            request.evidence.journal,
            request.evidence.session_logs,
            request.evidence.checkpoints,
        ),
    );
    let value = match request.command.as_str() {
        "inspect" => serde_json::to_value(operation.inspect(request.strict)),
        "verify" => serde_json::to_value(operation.verify(VerifyOptions {
            strict: request.strict,
            require_complete: request.require_complete,
        })),
        "replay" => serde_json::to_value(operation.replay(ReplayOptions {
            strict: request.strict,
            at_step: request.at_step,
        })),
        "fork" => operation
            .prepare_fork(
                request
                    .at_step
                    .ok_or_else(|| "fork requires at_step".to_string())?,
                request.strict,
            )
            .map(|plan| serde_json::to_value(plan.manifest()))
            .map_err(|error| error.to_string())?,
        other => return Err(format!("unknown verifiable command: {other}")),
    }
    .map_err(|error| format!("could not serialize verifiable result: {error}"))?;
    serde_json::to_string(&value)
        .map_err(|error| format!("could not encode verifiable result: {error}"))
}

/// Evidence planes supplied by a host or SDK adapter.
///
/// The bundle owns its bytes so a `VerifiableOperation` can be passed across an FFI boundary
/// without borrowing a filesystem reader or a process-local store.  The byte layout remains the
/// canonical wire layout validated by the runtime chain validator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidenceBundle {
    journal: Vec<Vec<u8>>,
    session_logs: Vec<Vec<Vec<u8>>>,
    checkpoints: Vec<Vec<u8>>,
}

impl EvidenceBundle {
    pub fn new(
        journal: Vec<Vec<u8>>,
        session_logs: Vec<Vec<Vec<u8>>>,
        checkpoints: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            journal,
            session_logs,
            checkpoints,
        }
    }

    pub fn journal(&self) -> &[Vec<u8>] {
        &self.journal
    }

    pub fn session_logs(&self) -> &[Vec<Vec<u8>>] {
        &self.session_logs
    }

    pub fn checkpoints(&self) -> &[Vec<u8>] {
        &self.checkpoints
    }
}

/// Options for a framework verification operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerifyOptions {
    pub strict: bool,
    pub require_complete: bool,
}

/// Options for an offline replay operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayOptions {
    pub strict: bool,
    pub at_step: Option<u64>,
}

/// A typed, storage-neutral operation view.
///
/// This is the framework entry point.  Adapters construct it from their own evidence source and
/// use the same methods regardless of whether that source is a file, database, object store, or
/// an in-memory test fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiableOperation {
    operation_id: String,
    evidence: EvidenceBundle,
}

impl VerifiableOperation {
    pub fn new(operation_id: impl Into<String>, evidence: EvidenceBundle) -> Self {
        Self {
            operation_id: operation_id.into(),
            evidence,
        }
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn evidence(&self) -> &EvidenceBundle {
        &self.evidence
    }

    pub fn inspect(&self, strict: bool) -> InspectReport {
        inspect_operation(
            &self.operation_id,
            self.evidence.journal(),
            self.evidence.session_logs(),
            self.evidence.checkpoints(),
            strict,
        )
    }

    pub fn verify(&self, options: VerifyOptions) -> VerifyReport {
        verify_operation(
            &self.operation_id,
            self.evidence.journal(),
            self.evidence.session_logs(),
            self.evidence.checkpoints(),
            options.strict,
            options.require_complete,
        )
    }

    pub fn replay(&self, options: ReplayOptions) -> ReplayReport {
        replay_operation(
            &self.operation_id,
            self.evidence.journal(),
            self.evidence.session_logs(),
            self.evidence.checkpoints(),
            options.strict,
            options.at_step,
        )
    }

    pub fn prepare_fork(&self, at_step: u64, strict: bool) -> Result<ForkPlan, String> {
        prepare_fork(
            &self.operation_id,
            self.evidence.journal(),
            self.evidence.session_logs(),
            self.evidence.checkpoints(),
            strict,
            at_step,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayVerdict {
    Pass,
    Fail,
    Unavailable,
}

impl ReplayVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckVerdict {
    Pass,
    Degraded,
    Fail,
    Unavailable,
}

impl CheckVerdict {
    pub fn exit_code(self, require_complete: bool) -> i32 {
        match self {
            Self::Fail => 1,
            Self::Unavailable => 2,
            Self::Degraded if require_complete => 2,
            Self::Pass | Self::Degraded => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkManifest {
    pub schema: String,
    pub operation_id: String,
    pub at_step: String,
    pub parent_record_digest: String,
    pub parent_input_id: String,
    pub source_records: usize,
}

/// A verified, read-only fork boundary in framework-native types.
///
/// A plan is data returned to a host for a later orchestration decision.  It does not append a
/// kernel input, mutate a checkpoint, or become a recovery authority.  `manifest()` is the stable
/// cross-process serialization projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPlan {
    pub operation_id: String,
    pub at_step: u64,
    pub parent_record_digest: String,
    pub parent_input_id: String,
    pub source_records: usize,
}

impl ForkPlan {
    pub fn manifest(&self) -> ForkManifest {
        ForkManifest {
            schema: FORK_SCHEMA.to_string(),
            operation_id: self.operation_id.clone(),
            at_step: self.at_step.to_string(),
            parent_record_digest: self.parent_record_digest.clone(),
            parent_input_id: self.parent_input_id.clone(),
            source_records: self.source_records,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordSummary {
    pub step_seq: String,
    pub input_id: String,
    pub input_kind: String,
    pub previous_record_digest: Option<String>,
    pub record_digest: String,
    pub step_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceSummary {
    pub journal_records: usize,
    pub session_events: Option<usize>,
    pub checkpoints: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectReport {
    pub schema: String,
    pub command: String,
    pub operation_id: String,
    pub evidence: EvidenceSummary,
    pub records: Vec<RecordSummary>,
    pub validation: ValidationReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayReport {
    pub schema: String,
    pub command: String,
    pub operation_id: String,
    pub at_step: Option<String>,
    pub verdict: ReplayVerdict,
    pub compared_steps: usize,
    pub first_divergence: Option<String>,
    pub validation: ValidationReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    pub schema: String,
    pub command: String,
    pub operation_id: String,
    pub require_complete: bool,
    pub verdict: CheckVerdict,
    pub validation: ValidationReport,
}

fn verify_operation<J, S, C>(
    operation_id: &str,
    journal_blobs: &[J],
    session_streams: &[Vec<S>],
    checkpoint_blobs: &[C],
    strict: bool,
    require_complete: bool,
) -> VerifyReport
where
    J: AsRef<[u8]>,
    S: AsRef<[u8]>,
    C: AsRef<[u8]>,
{
    let full_validation =
        validate_with_checkpoint(journal_blobs, session_streams, checkpoint_blobs, strict);
    let segment = full_validation
        .segments
        .iter()
        .find(|segment| segment.operation_id == operation_id);
    let missing_required_plane = require_complete
        && (full_validation.session_events.is_none() || full_validation.checkpoints.is_none());
    let verdict = if segment.is_none() || missing_required_plane {
        CheckVerdict::Unavailable
    } else if full_validation.has_violations_for(operation_id) {
        CheckVerdict::Fail
    } else if full_validation.has_insufficient_evidence() {
        CheckVerdict::Unavailable
    } else if segment.is_some_and(|segment| {
        segment
            .rules
            .iter()
            .any(|rule| rule.verdict == super::chain_validator::Verdict::Degraded)
    }) {
        CheckVerdict::Degraded
    } else {
        CheckVerdict::Pass
    };
    VerifyReport {
        schema: REPORT_SCHEMA.to_string(),
        command: "verify".to_string(),
        operation_id: operation_id.to_string(),
        require_complete,
        validation: full_validation.for_operation(operation_id),
        verdict,
    }
}

fn inspect_operation<J, S, C>(
    operation_id: &str,
    journal_blobs: &[J],
    session_streams: &[Vec<S>],
    checkpoint_blobs: &[C],
    strict: bool,
) -> InspectReport
where
    J: AsRef<[u8]>,
    S: AsRef<[u8]>,
    C: AsRef<[u8]>,
{
    let validation =
        validate_with_checkpoint(journal_blobs, session_streams, checkpoint_blobs, strict)
            .for_operation(operation_id);
    let records = operation_records(operation_id, journal_blobs)
        .into_iter()
        .map(record_summary)
        .collect();
    InspectReport {
        schema: REPORT_SCHEMA.to_string(),
        command: "inspect".to_string(),
        operation_id: operation_id.to_string(),
        evidence: EvidenceSummary {
            journal_records: journal_blobs.len(),
            session_events: validation.session_events,
            checkpoints: validation.checkpoints,
        },
        records,
        validation,
    }
}

fn replay_operation<J, S, C>(
    operation_id: &str,
    journal_blobs: &[J],
    session_streams: &[Vec<S>],
    checkpoint_blobs: &[C],
    strict: bool,
    at_step: Option<u64>,
) -> ReplayReport
where
    J: AsRef<[u8]>,
    S: AsRef<[u8]>,
    C: AsRef<[u8]>,
{
    let records = operation_records(operation_id, journal_blobs);
    let selected: Vec<Vec<u8>> = records
        .iter()
        .filter(|record| at_step.is_none_or(|at| record.step_seq().get() <= at))
        .map(|record| record.record_bytes().into_vec())
        .collect();
    let mut validation = if at_step.is_none() {
        validate_with_checkpoint(journal_blobs, session_streams, checkpoint_blobs, strict)
    } else {
        validate_with_checkpoint(&selected, session_streams, checkpoint_blobs, strict)
    };
    // Prefix replay uses canonical complete records so the order is deterministic. Preserve the
    // source-plane fact that a blob could not decode; silently dropping a malformed hop would turn
    // incomplete evidence into a false green replay.
    if at_step.is_some()
        && journal_blobs
            .iter()
            .any(|blob| KernelRecord::from_record_bytes(blob.as_ref()).is_err())
    {
        validation.unparseable_records = validation.unparseable_records.saturating_add(1);
    }
    let segment = validation
        .segments
        .iter()
        .find(|segment| segment.operation_id == operation_id);
    let output_validation = validation.for_operation(operation_id);
    let compared_steps = selected.len();
    let (verdict, first_divergence) = if validation.has_insufficient_evidence() {
        (
            ReplayVerdict::Unavailable,
            Some("journal evidence is incomplete or unparseable".to_string()),
        )
    } else if at_step.is_some_and(|at| !records.iter().any(|record| record.step_seq().get() == at))
    {
        (
            ReplayVerdict::Unavailable,
            at_step.map(|at| format!("operation {operation_id} has no step {at}")),
        )
    } else {
        match segment {
            None => (
                ReplayVerdict::Unavailable,
                Some("operation has no complete records".to_string()),
            ),
            Some(segment) => c3_verdict(segment),
        }
    };
    ReplayReport {
        schema: REPORT_SCHEMA.to_string(),
        command: "replay".to_string(),
        operation_id: operation_id.to_string(),
        at_step: at_step.map(|step| step.to_string()),
        verdict,
        compared_steps,
        first_divergence,
        validation: output_validation,
    }
}

fn prepare_fork<J, S, C>(
    operation_id: &str,
    journal_blobs: &[J],
    session_streams: &[Vec<S>],
    checkpoint_blobs: &[C],
    strict: bool,
    at_step: u64,
) -> Result<ForkPlan, String>
where
    J: AsRef<[u8]>,
    S: AsRef<[u8]>,
    C: AsRef<[u8]>,
{
    let replay = replay_operation(
        operation_id,
        journal_blobs,
        session_streams,
        checkpoint_blobs,
        strict,
        Some(at_step),
    );
    if replay.verdict != ReplayVerdict::Pass {
        return Err(replay
            .first_divergence
            .unwrap_or_else(|| "fork boundary is not verifiable".to_string()));
    }
    let records = operation_records(operation_id, journal_blobs);
    let parent = records
        .iter()
        .find(|record| record.step_seq().get() == at_step)
        .ok_or_else(|| format!("operation {operation_id} has no step {at_step}"))?;
    Ok(ForkPlan {
        operation_id: operation_id.to_string(),
        at_step,
        parent_record_digest: parent.record_digest().to_string(),
        parent_input_id: parent.input_id().to_string(),
        source_records: records
            .iter()
            .filter(|record| record.step_seq().get() <= at_step)
            .count(),
    })
}

fn c3_verdict(segment: &SegmentReport) -> (ReplayVerdict, Option<String>) {
    let rule = segment.rules.iter().find(|rule| rule.rule == "C3");
    match rule.map(|rule| rule.verdict) {
        Some(super::chain_validator::Verdict::Pass) => (ReplayVerdict::Pass, None),
        Some(super::chain_validator::Verdict::Fail) => {
            (ReplayVerdict::Fail, rule.map(|rule| rule.detail.clone()))
        }
        _ => (
            ReplayVerdict::Unavailable,
            rule.map(|rule| rule.detail.clone()),
        ),
    }
}

fn operation_records<B: AsRef<[u8]>>(operation_id: &str, blobs: &[B]) -> Vec<KernelRecord> {
    let mut records: Vec<_> = blobs
        .iter()
        .filter_map(|blob| KernelRecord::from_record_bytes(blob.as_ref()).ok())
        .filter(|record| record.operation_id().as_str() == operation_id)
        .collect();
    records.sort_by_key(|record| record.step_seq().get());
    records
}

fn record_summary(record: KernelRecord) -> RecordSummary {
    let input_kind = record
        .normalized_input()
        .map(|input| input.input.kind().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    RecordSummary {
        step_seq: record.step_seq().to_string(),
        input_id: record.input_id().to_string(),
        input_kind,
        previous_record_digest: record.previous_record_digest().map(ToString::to_string),
        record_digest: record.record_digest().to_string(),
        step_digest: record.step_digest().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CheckVerdict, EvidenceBundle, ForkManifest, REPORT_SCHEMA, ReplayOptions, ReplayVerdict,
        VerifiableOperation, VerifyOptions, inspect_operation, operation_json, replay_operation,
        verify_operation,
    };

    fn record_fixture() -> Vec<Vec<u8>> {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/kernel-wire/golden_record_chain.json"
        ))
        .unwrap();
        fixture["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|link| serde_json::to_vec(&link["record"]).unwrap())
            .collect()
    }

    #[test]
    fn report_schema_is_frozen_for_the_minor() {
        assert_eq!(REPORT_SCHEMA, "verifiable-report/v2");
    }

    #[test]
    fn json_bridge_delegates_to_the_framework_operation() {
        let request = serde_json::json!({
            "operation_id": "missing",
            "command": "verify",
            "evidence": {"journal": []},
            "require_complete": true,
        });
        let result: serde_json::Value =
            serde_json::from_str(&operation_json(&request.to_string()).unwrap()).unwrap();
        assert_eq!(result["schema"], REPORT_SCHEMA);
        assert_eq!(result["operation_id"], "missing");
        assert_eq!(result["verdict"], "unavailable");
    }

    #[test]
    fn a_fork_manifest_round_trips_without_new_authority() {
        let manifest = ForkManifest {
            schema: "verifiable-fork/v2".to_string(),
            operation_id: "op-1".to_string(),
            at_step: "3".to_string(),
            parent_record_digest: "sha256:parent".to_string(),
            parent_input_id: "in-3".to_string(),
            source_records: 4,
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let decoded: ForkManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, manifest);
        assert_eq!(ReplayVerdict::Pass.as_str(), "pass");
    }

    #[test]
    fn inspect_selects_one_operation_and_keeps_the_validator_report() {
        let records = record_fixture();
        let report = inspect_operation(
            "op-record-1",
            &records,
            &[] as &[Vec<Vec<u8>>],
            &[] as &[Vec<u8>],
            false,
        );
        assert_eq!(report.schema, REPORT_SCHEMA);
        assert_eq!(report.records.len(), 3);
        assert_eq!(report.validation.segments.len(), 1);
    }

    #[test]
    fn framework_operation_owns_evidence_and_delegates_all_views() {
        let records = record_fixture();
        let operation = VerifiableOperation::new(
            "op-record-1",
            EvidenceBundle::new(records, Vec::new(), Vec::new()),
        );
        assert_eq!(operation.operation_id(), "op-record-1");
        assert_eq!(operation.evidence().journal().len(), 3);
        assert_eq!(operation.inspect(false).records.len(), 3);
        assert_eq!(
            operation
                .verify(VerifyOptions {
                    strict: false,
                    require_complete: false,
                })
                .operation_id,
            "op-record-1"
        );
        assert_eq!(
            operation
                .replay(ReplayOptions {
                    strict: false,
                    at_step: Some(99),
                })
                .verdict,
            ReplayVerdict::Unavailable
        );
    }

    #[test]
    fn verify_missing_operation_is_unavailable() {
        let records = record_fixture();
        let report = verify_operation(
            "missing",
            &records,
            &[] as &[Vec<Vec<u8>>],
            &[] as &[Vec<u8>],
            false,
            true,
        );
        assert_eq!(report.verdict, CheckVerdict::Unavailable);
        assert_eq!(report.verdict.exit_code(true), 2);
    }

    #[test]
    fn verify_require_complete_rejects_a_journal_only_bundle() {
        let records = record_fixture();
        let report = verify_operation(
            "op-record-1",
            &records,
            &[] as &[Vec<Vec<u8>>],
            &[] as &[Vec<u8>],
            false,
            true,
        );
        assert_eq!(report.verdict, CheckVerdict::Unavailable);
        assert_eq!(report.verdict.exit_code(true), 2);
    }

    #[test]
    fn replay_rejects_a_missing_boundary_without_treating_it_as_a_divergence() {
        let records = record_fixture();
        let report = replay_operation(
            "op-record-1",
            &records,
            &[] as &[Vec<Vec<u8>>],
            &[] as &[Vec<u8>],
            false,
            Some(99),
        );
        assert_eq!(report.verdict, ReplayVerdict::Unavailable);
        assert_eq!(report.compared_steps, 3);
        assert!(report.first_divergence.unwrap().contains("no step 99"));
    }

    #[test]
    fn replay_marks_a_malformed_prefix_as_unavailable() {
        let mut records = record_fixture();
        records.push(b"{malformed".to_vec());
        let report = replay_operation(
            "op-record-1",
            &records,
            &[] as &[Vec<Vec<u8>>],
            &[] as &[Vec<u8>],
            false,
            Some(2),
        );
        assert_eq!(report.verdict, ReplayVerdict::Unavailable);
        assert!(report.first_divergence.unwrap().contains("incomplete"));
    }
}
