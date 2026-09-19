//! P7-S5 · the chain validator, batch 1: rules C1–C4 with C7 degradation marking (P2 §5).
//!
//! Host-ops tooling, not an SDK runtime path: CI gates and incident triage run the same knife,
//! and C3 needs the deterministic transition (re-plan), which only the core can perform. The CLI
//! half is `src/bin/ds-chain-validator.rs`; this module is the verdict logic.
//!
//! Input is a journal prefix — a sequence of opaque record byte blobs. Records are grouped into
//! per-operation chain segments and every segment is judged independently. Nothing here ever
//! re-serializes a record: blobs pass through untouched, so a self-digest verdict is a verdict
//! about the bytes the host durably wrote.
//!
//! The rules, and where each one gets its teeth:
//!
//! - **C1 · chain integrity** — `record[i].previous_record_digest == digest(record[i-1])`,
//!   `step_seq` strictly +1, genesis `previous_record_digest = None`. Complete segments go
//!   through [`verify_record_chain`]; a segment with degraded hops falls back to checking every
//!   link whose digests survived.
//! - **C2 · input idempotency** — one `input_id` never yields two different records: a retry
//!   must reach the same record. Grouped per operation (the idempotency key's namespace).
//! - **C3 · causal closure** — every record's resolved effect must be reproducible by
//!   re-planning the earlier records. This is the §12.2 restore ladder's genesis leg
//!   ([`restore_operation`] with no checkpoint): chain verify + deterministic re-plan + per-step
//!   record-digest comparison. It doubles as the re-plan determinism regression gate — the
//!   direct gate for 0.2.62-class "this binary does not reproduce the history it is resuming"
//!   incidents. If C1 failed, C3 reports degraded rather than re-reporting the same break.
//! - **C4 · task lineage** — the journal-direct half: every `(task_id, attempt_id)` launch pair
//!   appears at most once (the launch token is *derived* from that pair, so a repeated pair is a
//!   reused token), and a spawn resolution names an effect the same operation published at an
//!   earlier step. The parent chain itself is not journaled; it holds structurally under C3's
//!   re-plan because an orphan spawn has no outstanding effect to resolve. The durable
//!   launch-token ledger lives in checkpoints — batch 2 territory. Both limits are named in
//!   [`ValidationReport::deferred`].
//! - **C7 · degradation** — an old-format hop (strict decode fails but the
//!   identity fields survive) degrades the checks that need the missing fields instead of
//!   failing them. A proven digest mismatch fails C1 even when identity fields survive; every degraded hop is marked on its segment's report. A blob that is not a
//!   record at all counts as unparseable input, which is an exit-code-2 condition
//!   ("evidence insufficient"), never a violation.
//!
//! ## Batch 3 · the SessionLog input plane (0.2.64 S4)
//!
//! [`validate_with_session_log`] adds a second plane: SessionLog event streams. SessionLog is
//! Evidence Truth (P6 §S) — never recovery authority, and never kernel input. Where the journal
//! plane is order-independent blobs, a session log is one file's append-ordered events, so the
//! input is a list of **streams** (one per file) whose internal order is preserved.
//!
//! The core has no typed SessionLog vocabulary (P6: the core treats SessionLog as opaque JSON),
//! so events are classified leniently: the `kind` field picks the extraction shape, missing
//! additive fields parse as absent, and both spellings of host-nested fields are accepted
//! (`route.routeId` from node, `route.route_id` from python). Unknown kinds are parseable but
//! ignored — the vocabulary evolves; only C6/C8-relevant kinds are extracted. An event that is
//! not a JSON object at all counts as unparseable (exit-code-2), exactly like the journal plane.
//!
//! C7 carries across planes: old logs simply lack `provider_attempt` / the additive fields —
//! the rules that need them degrade, never fail.
//!
//! ## Batch 2 · the checkpoint input plane (0.2.65 S1)
//!
//! [`validate_with_checkpoint`] adds the third plane: one or more logical checkpoints (§12).
//! A checkpoint is a *claim about the journal* — "this logical state was captured at step N,
//! anchored by these digests" — and C5 is the rule that makes the claim answer to the bytes.
//! Without a checkpoint input, C5 is deferred, not red (the C7 philosophy: a plane nobody
//! handed over cannot fail).
//!
//! - **C5a · checkpoint anchoring** — the checkpoint's `genesis_digest` names the journal's
//!   genesis record, its covered head names the record at `through_step_seq`, and every
//!   bounded-tail entry whose journal record survives carries that record's digest. A present
//!   record with the wrong digest is a proven contradiction (fail); a pruned or missing record
//!   is unverifiable (degrade) — retention is not a crime.
//! - **C5b · the launch-token ledger** — the durable half of C4: within one checkpoint the
//!   same launch token may not name two mints at different steps (reuse across `TaskLaunch`
//!   payloads), every pending `SpawnTasks` effect must carry tokens the ledger registered at
//!   the effect's own step, and no ledger entry may sit beyond the covered boundary. Under
//!   `--strict` the re-plan fold must re-derive the exact ledger.
//! - **`--strict` · the re-plan replay** — the journal is folded from genesis through the
//!   covered step through the same restore path C3 uses, and the re-derived checkpoint must
//!   carry the checkpoint's `state_digest`; the checkpoint+tail restore ladder must also hold
//!   against the journal above the covered step. Cost is one full fold — explicit request only.

use std::collections::HashMap;

use serde::Serialize;

use crate::runtime::kernel::wire::ConfigDefaults;
use crate::runtime::kernel::wire::checkpoint::KernelCheckpoint;
use crate::runtime::kernel::wire::effect::{
    EffectKind, EffectOutcome, EffectSuccess, ProviderOutcome, SpawnTasksEffect,
};
use crate::runtime::kernel::wire::record::{
    KernelRecord, NormalizedPayload, RecordError, verify_record_chain,
};
use crate::runtime::kernel::wire::restore::{RestoredOperation, restore_operation};
use crate::runtime::kernel::wire::transaction::InMemoryRecordIndex;

/// The pseudo-segment for degraded hops whose `operation_id` did not survive. Kept obviously
/// synthetic so a report reader never confuses it with a real operation.
pub const UNATTRIBUTED_SEGMENT: &str = "(unattributed)";

/// Scope limits that hold no matter which evidence planes were handed over, surfaced verbatim
/// on every report so a reader never mistakes a green verdict for a complete §5.
const DEFERRED_ALWAYS: &str = "c4.parent_chain: parent links are not journaled; an orphan spawn \
     cannot resolve (no outstanding effect), which C3's re-plan enforces structurally";

/// The checkpoint-plane deferral: without `--checkpoint`, C5's durable half cannot run. The
/// journal-direct shadow it names is real and checked under C4, so the limit is a scope
/// statement, not a gap.
const DEFERRED_WITHOUT_CHECKPOINT: &str = "c5b.launch_token_ledger: the durable LaunchToken \
     ledger lives in checkpoints; provide --checkpoint to check token reuse across TaskLaunch \
     payloads (the journal-direct shadow — (task_id, attempt_id) pair uniqueness — is checked \
     under C4)";

/// One rule's verdict on one segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleReport {
    /// `C1`…`C4`.
    pub rule: String,
    pub verdict: Verdict,
    /// What was checked, or what broke, or why the check degraded.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    /// C7: the check could not run to completion on this segment's evidence. Never a failure.
    Degraded,
}

/// A hop whose strict record decode failed but whose identity fields survived — the C7 marking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DegradedHop {
    /// Position in the validator's input, for cross-referencing the raw journal.
    pub ordinal: usize,
    pub step_seq: Option<u64>,
    /// Why the strict decode rejected the bytes.
    pub reason: String,
}

/// One operation's chain, judged independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentReport {
    pub operation_id: String,
    pub hops: usize,
    pub degraded_hops: Vec<DegradedHop>,
    pub rules: Vec<RuleReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationReport {
    pub segments: Vec<SegmentReport>,
    /// Blobs that are not records at all (not JSON objects, or carrying no identity fields).
    pub unparseable_records: usize,
    /// Batch 3: SessionLog↔journal cross-verification verdicts (C6/C8). Report-scope, not
    /// per-segment: these rules join two evidence planes.
    #[serde(default)]
    pub cross_checks: Vec<RuleReport>,
    /// SessionLog event blobs that were not JSON objects at all. `0` when no session plane
    /// was handed over.
    #[serde(default)]
    pub unparseable_events: usize,
    /// `Some(count)` when SessionLog streams were handed over — the total of parseable events
    /// across every stream. `None` = journal-only validation (batch-1 mode). An explicitly
    /// provided session plane with zero parseable events is evidence-insufficient (exit 2).
    #[serde(default)]
    pub session_events: Option<usize>,
    /// Batch 2: checkpoint↔journal anchoring verdicts (C5a/C5b). Report-scope, not
    /// per-segment: these rules join the checkpoint plane to the journal plane.
    #[serde(default)]
    pub checkpoint_checks: Vec<RuleReport>,
    /// Checkpoint blobs that did not decode at all. `0` when no checkpoint plane was handed
    /// over.
    #[serde(default)]
    pub unparseable_checkpoints: usize,
    /// `Some(count)` when checkpoint blobs were handed over — how many decoded. `None` = no
    /// checkpoint plane. An explicitly provided checkpoint plane with zero parseable
    /// checkpoints is evidence-insufficient (exit 2), exactly like the other planes.
    #[serde(default)]
    pub checkpoints: Option<usize>,
    /// Batch-scope limits a green verdict does not cover.
    pub deferred: Vec<String>,
}

impl ValidationReport {
    pub fn has_violations(&self) -> bool {
        self.segments
            .iter()
            .flat_map(|segment| segment.rules.iter())
            .chain(self.cross_checks.iter())
            .chain(self.checkpoint_checks.iter())
            .any(|rule| rule.verdict == Verdict::Fail)
    }

    /// Whether a selected operation has a proven rule violation. Cross-plane checks are
    /// report-scoped and therefore still count; segment checks are narrowed to the requested
    /// operation so a multi-operation evidence bundle cannot make an unrelated operation red.
    pub fn has_violations_for(&self, operation_id: &str) -> bool {
        self.segments
            .iter()
            .filter(|segment| segment.operation_id == operation_id)
            .flat_map(|segment| segment.rules.iter())
            .chain(self.cross_checks.iter())
            .chain(self.checkpoint_checks.iter())
            .any(|rule| rule.verdict == Verdict::Fail)
    }

    /// Keep report-scope checks while narrowing the journal segments to one operation for a host
    /// command. The source evidence is still represented by the counts and cross-plane checks.
    pub fn for_operation(&self, operation_id: &str) -> Self {
        let mut report = self.clone();
        report
            .segments
            .retain(|segment| segment.operation_id == operation_id);
        report
    }

    /// Evidence-plane parse failures that make a report insufficient rather than contradictory.
    pub fn has_insufficient_evidence(&self) -> bool {
        self.unparseable_records > 0
            || self.unparseable_events > 0
            || self.unparseable_checkpoints > 0
            || matches!(self.session_events, Some(0))
            || matches!(self.checkpoints, Some(0))
    }

    /// The CLI contract (P7 §3.2): `0` all green, `1` a violation was proven, `2` the evidence
    /// was insufficient. A proven violation outranks insufficient evidence; degraded hops and
    /// deferred scope never move the code. An explicitly provided SessionLog or checkpoint
    /// plane that yields nothing parseable is insufficient evidence of the same kind as
    /// unparseable records.
    pub fn exit_code(&self) -> i32 {
        if self.has_violations() {
            1
        } else if self.has_insufficient_evidence() || self.segments.is_empty() {
            2
        } else {
            0
        }
    }
}

/// One input blob, classified. `Complete` records are self-digest-verified by construction
/// ([`KernelRecord::from_record_bytes`] cannot produce an unverified one).
enum Hop {
    Complete(KernelRecord),
    Degraded(DegradedRecord),
}

struct DegradedRecord {
    ordinal: usize,
    operation_id: Option<String>,
    input_id: Option<String>,
    step_seq: Option<u64>,
    previous_record_digest: Option<String>,
    record_digest: Option<String>,
    reason: String,
    integrity_failure: bool,
}

impl DegradedRecord {
    fn marking(&self) -> DegradedHop {
        DegradedHop {
            ordinal: self.ordinal,
            step_seq: self.step_seq,
            reason: self.reason.clone(),
        }
    }
}

impl Hop {
    fn operation_id(&self) -> Option<&str> {
        match self {
            Self::Complete(record) => Some(record.operation_id().as_str()),
            Self::Degraded(degraded) => degraded.operation_id.as_deref(),
        }
    }

    fn step_seq(&self) -> Option<u64> {
        match self {
            Self::Complete(record) => Some(record.step_seq().get()),
            Self::Degraded(degraded) => degraded.step_seq,
        }
    }
}

/// Validate a journal prefix: a sequence of opaque record byte blobs, in any order. Records
/// group into per-operation segments, each judged independently; blob order never matters
/// because the chain's own `step_seq`/digest links define the order.
pub fn validate_journal<B: AsRef<[u8]>>(blobs: &[B]) -> ValidationReport {
    validate_with_session_log(blobs, &[] as &[Vec<Vec<u8>>])
}

/// Batch 3 entry point: the journal plane plus SessionLog evidence streams. Each inner slice
/// is one session-log file's events **in append order** — unlike journal blobs, event order
/// within a stream is meaningful (a `run_started` delimits the run its following attempts
/// belong to). Streams never cross-join: fingerprint and route-stability checks are per-stream.
pub fn validate_with_session_log<J, S>(
    journal_blobs: &[J],
    session_streams: &[Vec<S>],
) -> ValidationReport
where
    J: AsRef<[u8]>,
    S: AsRef<[u8]>,
{
    validate_with_checkpoint(journal_blobs, session_streams, &[] as &[Vec<u8>], false)
}

/// Batch 2 entry point: the journal plane plus whichever evidence planes the caller holds.
/// An empty `session_streams` means journal-only; an empty `checkpoint_blobs` means C5 is
/// deferred, not run. `strict` arms the re-plan replay (C5's `--strict`): each checkpoint's
/// journal prefix is folded from genesis through the covered step and both the state digest
/// and the launch-token ledger must re-derive exactly. Strict costs one fold per checkpoint
/// and is meaningless without checkpoint blobs.
pub fn validate_with_checkpoint<J, S, C>(
    journal_blobs: &[J],
    session_streams: &[Vec<S>],
    checkpoint_blobs: &[C],
    strict: bool,
) -> ValidationReport
where
    J: AsRef<[u8]>,
    S: AsRef<[u8]>,
    C: AsRef<[u8]>,
{
    let (outcomes, unparseable_records, segment_records) = validate_journal_plane(journal_blobs);

    let mut streams: Vec<SessionStream> = Vec::with_capacity(session_streams.len());
    for stream_blobs in session_streams {
        let mut events = Vec::with_capacity(stream_blobs.len());
        let mut unparseable_events = 0;
        for blob in stream_blobs {
            match classify_session_event(blob.as_ref()) {
                Some(event) => events.push(event),
                None => unparseable_events += 1,
            }
        }
        streams.push(SessionStream {
            events,
            unparseable_events,
        });
    }

    let session_plane_provided = !session_streams.is_empty();
    let session_events =
        session_plane_provided.then(|| streams.iter().map(|stream| stream.events.len()).sum());
    let unparseable_events = streams.iter().map(|stream| stream.unparseable_events).sum();

    // C6/C8 join the planes.
    let cross_checks = if session_plane_provided {
        let mut checks = check_c6(&streams, &outcomes);
        checks.push(check_c8(&streams, &outcomes));
        checks
    } else {
        Vec::new()
    };

    // C5 joins the checkpoint plane to the journal.
    let checkpoint_plane_provided = !checkpoint_blobs.is_empty();
    let (checkpoint_checks, unparseable_checkpoints, checkpoints) = if checkpoint_plane_provided {
        let (checks, unparseable, parsed) = check_c5(&segment_records, checkpoint_blobs, strict);
        (checks, unparseable, Some(parsed))
    } else {
        (Vec::new(), 0, None)
    };

    let mut deferred = vec![DEFERRED_ALWAYS.to_string()];
    if !checkpoint_plane_provided {
        deferred.push(DEFERRED_WITHOUT_CHECKPOINT.to_string());
    }

    ValidationReport {
        segments: outcomes.into_iter().map(|outcome| outcome.report).collect(),
        unparseable_records,
        cross_checks,
        unparseable_events,
        session_events,
        checkpoint_checks,
        unparseable_checkpoints,
        checkpoints,
        deferred,
    }
}

/// The journal plane on its own: classify blobs into hops, group into segments, judge each.
/// The complete records also come back grouped per operation — C5 anchors checkpoints against
/// them without re-decoding a single blob.
fn validate_journal_plane<B: AsRef<[u8]>>(
    blobs: &[B],
) -> (
    Vec<SegmentOutcome>,
    usize,
    HashMap<String, Vec<KernelRecord>>,
) {
    let mut hops: Vec<Hop> = Vec::with_capacity(blobs.len());
    let mut unparseable_records = 0;
    for (ordinal, blob) in blobs.iter().enumerate() {
        match classify(ordinal, blob.as_ref()) {
            Some(hop) => hops.push(hop),
            None => unparseable_records += 1,
        }
    }

    let mut segments: HashMap<String, Vec<Hop>> = HashMap::new();
    let mut complete: HashMap<String, Vec<KernelRecord>> = HashMap::new();
    for hop in hops {
        let key = hop
            .operation_id()
            .map(str::to_string)
            .unwrap_or_else(|| UNATTRIBUTED_SEGMENT.to_string());
        if let Hop::Complete(record) = &hop {
            complete
                .entry(key.clone())
                .or_default()
                .push(record.clone());
        }
        segments.entry(key).or_default().push(hop);
    }

    let mut keys: Vec<String> = segments.keys().cloned().collect();
    keys.sort();
    let reports = keys
        .iter()
        .map(|key| validate_segment(key, segments.remove(key).unwrap_or_default()))
        .collect();
    (reports, unparseable_records, complete)
}

// ---------------------------------------------------------------------------------------------
// batch 3 · the SessionLog evidence plane
// ---------------------------------------------------------------------------------------------

/// One session-log file, classified: its events in append order plus the count of blobs that
/// were not JSON objects at all.
pub struct SessionStream {
    pub events: Vec<EvidenceEvent>,
    pub unparseable_events: usize,
}

/// A SessionLog event, leniently classified. Only the kinds C6/C8 read are extracted; every
/// other kind — known or future — is `Other`. Field absence is data (C7 degrades), not error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceEvent {
    /// `run_started` — delimits a run; its `route` is the batch-3 route-stability baseline
    /// (Q3). Absent route = old log = degraded, never failed.
    RunStarted { route_id: Option<String> },
    /// `provider_attempt` — one effect's physical execution (P4 §1.2).
    ProviderAttempt {
        effect_id: Option<String>,
        request_fingerprint: Option<String>,
        route_id: Option<String>,
        status: Option<String>,
    },
    /// `prompt_measured` — the durable measurement fact; `request_fingerprint` joins a
    /// `provider_attempt` to the request plan it executed (G2, C6).
    PromptMeasured {
        effect_id: Option<String>,
        request_fingerprint: Option<String>,
    },
    /// `llm_completed` — the invocation's terminal projection: `invocation_id` derives as the
    /// chain's first effect (P4 §1.1), `effect_id` is the selected outcome effect.
    LlmCompleted {
        effect_id: Option<String>,
        invocation_id: Option<String>,
    },
    /// Any other kind — parseable, ignored by batch-3 rules.
    Other,
}

/// Lenient event classification: a JSON object with an extractable `kind` classifies; anything
/// else is unparseable input (exit-code-2, never a violation).
fn classify_session_event(bytes: &[u8]) -> Option<EvidenceEvent> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let string = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let kind = string("kind");
    let event = match kind.as_deref() {
        Some("run_started") => EvidenceEvent::RunStarted {
            route_id: route_id_of(&value),
        },
        Some("provider_attempt") => EvidenceEvent::ProviderAttempt {
            effect_id: string("effect_id"),
            request_fingerprint: string("request_fingerprint"),
            route_id: route_id_of(&value),
            status: string("status"),
        },
        Some("prompt_measured") => EvidenceEvent::PromptMeasured {
            effect_id: string("effect_id"),
            // The nested measurement keeps its host-native shape: node serializes camelCase
            // (`requestFingerprint`), python snake_case (`request_fingerprint`).
            request_fingerprint: object
                .get("measurement")
                .and_then(|measurement| {
                    measurement
                        .get("requestFingerprint")
                        .or_else(|| measurement.get("request_fingerprint"))
                })
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        },
        Some("llm_completed") => EvidenceEvent::LlmCompleted {
            effect_id: string("effect_id"),
            invocation_id: string("invocation_id"),
        },
        _ => EvidenceEvent::Other,
    };
    Some(event)
}

/// `route.route_id`, accepting both host spellings (node camelCase, python snake_case).
fn route_id_of(event: &serde_json::Value) -> Option<String> {
    let route = event.get("route")?;
    route
        .get("routeId")
        .or_else(|| route.get("route_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

// ---------------------------------------------------------------------------------------------
// C6 · SessionLog↔journal cross-verification (batch 3, 0.2.64 S4b)
// ---------------------------------------------------------------------------------------------

/// C6 joins the two planes: host-side SessionLog evidence against the journal's authority.
/// Three clauses, three reports — their degradation conditions differ, so one merged verdict
/// would hide which clause actually ran:
///
/// - **C6.1 · attempt↔journal effect correspondence** — every `provider_attempt.effect_id`
///   must name an effect its operation's segment actually published. Membership comes from
///   the deterministic re-plan (resolved effect ids read journal-directly ∪ pending effects
///   after restore): a complete segment either published the effect or it did not, which makes
///   a mismatched attempt provably forged rather than merely unverifiable. Journal prefixes
///   degrade honestly: an attempt naming a step past the journal's tip, a missing segment
///   (the journal may cover a subset of the session's operations), or an unrestorable segment
///   all degrade instead of failing.
/// - **C6.2 · fingerprint join** — every `provider_attempt.request_fingerprint` must appear on
///   a `prompt_measured` in the same stream (G2: the fingerprint binds the evidence to the
///   request plan; P4 §5). Streams never cross-join.
/// - **C6.3 · route stability** (裁决 Q3) — within one run (delimited by `run_started`), every
///   attempt's routeId equals the pinning `run_started.route.route_id`; an in-run mismatch is
///   a violation. A *new* `run_started` naming a different route is a legal cross-resume change
///   (adapter upgrades happen) and is degraded-marked, never failed.
///
/// C7 spans all three: a stream with no `provider_attempt` events at all (a pre-0.2.63 log)
/// degrades every clause. A `provider_attempt` missing its primary key (`effect_id`) or its
/// `request_fingerprint` fails — the event kind itself is new, so no old log can produce one,
/// and the conformant writers require both fields; a keyless attempt is forged evidence.
fn check_c6(streams: &[SessionStream], outcomes: &[SegmentOutcome]) -> Vec<RuleReport> {
    let segments: HashMap<&str, &SegmentOutcome> = outcomes
        .iter()
        .filter(|outcome| outcome.report.operation_id != UNATTRIBUTED_SEGMENT)
        .map(|outcome| (outcome.report.operation_id.as_str(), outcome))
        .collect();

    let mut correspondence = ClauseAccumulator::default();
    let mut fingerprints = ClauseAccumulator::default();
    let mut routes = ClauseAccumulator::default();
    let mut total_attempts = 0usize;

    for (index, stream) in streams.iter().enumerate() {
        let measured: std::collections::HashSet<&str> = stream
            .events
            .iter()
            .filter_map(|event| match event {
                EvidenceEvent::PromptMeasured {
                    request_fingerprint,
                    ..
                } => request_fingerprint.as_deref(),
                _ => None,
            })
            .collect();

        // C6.3 per-stream walk state: the current run's pinned route, and the previous run's
        // for the cross-resume comparison.
        let mut baseline: Option<&str> = None;
        let mut last_pinned: Option<&str> = None;

        for event in &stream.events {
            match event {
                EvidenceEvent::RunStarted { route_id } => {
                    if let Some(new_route) = route_id.as_deref() {
                        if let Some(previous) = last_pinned
                            && previous != new_route
                        {
                            routes.degraded(format!(
                                "stream #{index}: run resumed on route {new_route} (was \
                                 {previous}) — a cross-resume change, degraded per Q3"
                            ));
                        }
                        baseline = Some(new_route);
                        last_pinned = Some(new_route);
                    } else {
                        // A routeless run_started is old-format: attempts under it cannot be
                        // route-checked.
                        baseline = None;
                    }
                }
                EvidenceEvent::ProviderAttempt {
                    effect_id,
                    request_fingerprint,
                    route_id,
                    ..
                } => {
                    total_attempts += 1;
                    let label = effect_id.as_deref().unwrap_or("(no effect_id)");

                    // C6.1
                    match effect_id.as_deref() {
                        None => correspondence.violation(format!(
                            "stream #{index}: provider_attempt without effect_id — the writers \
                             mint it from the kernel effect, so a keyless attempt is forged \
                             evidence"
                        )),
                        Some(effect) => match parse_effect_step(effect) {
                            None => correspondence.violation(format!(
                                "stream #{index}: attempt names {effect}, which is not in the \
                                 `operation:step:N:effect:M` vocabulary"
                            )),
                            Some((operation, step)) => match segments.get(operation) {
                                None => correspondence.degraded(format!(
                                    "stream #{index}: attempt names {effect}, but operation \
                                     {operation} has no journal segment (the journal may cover \
                                     a subset of the session)"
                                )),
                                Some(outcome) => match &outcome.effects {
                                    None => correspondence.degraded(format!(
                                        "stream #{index}: segment {operation} could not be \
                                         re-planned, so {effect}'s publication is unverifiable"
                                    )),
                                    Some(effects) if effects.published.contains(effect) => {
                                        correspondence.checked += 1;
                                    }
                                    Some(effects) if step > effects.max_step => {
                                        correspondence.degraded(format!(
                                            "stream #{index}: attempt names {effect} at step \
                                             {step}, past the journal's tip (step {}) — a \
                                             prefix cannot disprove it",
                                            effects.max_step
                                        ));
                                    }
                                    Some(_) => correspondence.violation(format!(
                                        "stream #{index}: attempt names {effect}, but the \
                                         deterministic re-plan of {operation} never published \
                                         it — the attempt is unmoored from the journal"
                                    )),
                                },
                            },
                        },
                    }

                    // C6.2
                    match request_fingerprint.as_deref() {
                        None => fingerprints.violation(format!(
                            "stream #{index}: provider_attempt {label} without \
                             request_fingerprint — the writers require it (G2)"
                        )),
                        Some(fingerprint) if measured.contains(fingerprint) => {
                            fingerprints.checked += 1;
                        }
                        Some(fingerprint) => fingerprints.violation(format!(
                            "stream #{index}: attempt {label} carries fingerprint \
                             {fingerprint}, but no prompt_measured in this session carries it"
                        )),
                    }

                    // C6.3
                    match (route_id.as_deref(), baseline) {
                        (Some(route), Some(pinned)) if route != pinned => {
                            routes.violation(format!(
                                "stream #{index}: attempt {label} ran on route {route} inside \
                                 a run pinned to {pinned} — an in-run route change is a \
                                 violation (Q3)"
                            ))
                        }
                        (Some(_), Some(_)) => routes.checked += 1,
                        // No pinned run, or the attempt lacks a route: unverifiable.
                        _ => routes.unverifiable += 1,
                    }
                }
                _ => {}
            }
        }
    }

    vec![
        correspondence.report(
            "C6.1",
            total_attempts,
            "attempt↔journal effect correspondence",
        ),
        fingerprints.report(
            "C6.2",
            total_attempts,
            "attempt fingerprint↔prompt_measured join",
        ),
        routes.report("C6.3", total_attempts, "in-run route stability"),
    ]
}

/// One C6 clause's tally across every stream. Fail outranks degrade; a clause that found no
/// attempts at all degrades (a pre-0.2.63 log carries none — C7).
#[derive(Default)]
struct ClauseAccumulator {
    checked: usize,
    unverifiable: usize,
    violations: Vec<String>,
    degraded_notes: Vec<String>,
}

impl ClauseAccumulator {
    fn violation(&mut self, detail: String) {
        self.violations.push(detail);
    }

    fn degraded(&mut self, note: String) {
        self.degraded_notes.push(note);
    }

    fn report(self, rule: &str, total_attempts: usize, what: &str) -> RuleReport {
        let rule = rule.to_string();
        if !self.violations.is_empty() {
            return RuleReport {
                rule,
                verdict: Verdict::Fail,
                detail: self.violations.join("; "),
            };
        }
        if total_attempts == 0 {
            return RuleReport {
                rule,
                verdict: Verdict::Degraded,
                detail: format!(
                    "no provider_attempt events in any stream — a pre-0.2.63 log carries none \
                     (C7); {what} unchecked"
                ),
            };
        }
        if !self.degraded_notes.is_empty() || self.unverifiable > 0 {
            let mut detail = self.degraded_notes.join("; ");
            if self.unverifiable > 0 {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                detail.push_str(&format!(
                    "{} attempt(s) unverifiable (no pinned run route)",
                    self.unverifiable
                ));
            }
            return RuleReport {
                rule,
                verdict: Verdict::Degraded,
                detail: format!("{} attempt(s) verified for {what}; {detail}", self.checked),
            };
        }
        RuleReport {
            rule,
            verdict: Verdict::Pass,
            detail: format!(
                "{} attempt(s) verified — {what} holds across every stream",
                self.checked
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// C8 · invocation chain adjacency (batch 3, 0.2.64 S4c)
// ---------------------------------------------------------------------------------------------

/// C8 · a retried invocation's chain must be journal-real. The SessionLog's falsifiable claim
/// is the endpoint pair: `llm_completed.invocation_id` (the chain's first effect — the derived
/// identity, P4 §1.1) and `llm_completed.effect_id` (the effect the kernel adopted). When they
/// differ, the journal must show that the head did NOT close the invocation:
///
/// - both ids parse in the `{operation}:step:N:effect:M` vocabulary, same operation (causation
///   cannot cross operations — C4's spirit), and the selected effect's step strictly follows
///   the head's;
/// - the head's resolution is **chain-advancing** — `Overflow` (the compaction ladder
///   republishes call_provider) or `Failed` (accepted for forward-compat: today's kernel
///   answers a CallProvider failure with a terminal per DEC-5, and a restored segment has
///   already proven the kernel itself walked whatever followed). A `Completed` head with a
///   *different* selected effect is the "merge two invocations into one" forgery: it fails.
///
/// Two deliberate deviations from P4 §5's letter, both forced by the wire reality:
/// 1. §5 says the hop between adjacent effects is a *Failed* resolution. Today's kernel never
///    re-emits after a CallProvider failure (DEC-5: `plan_effect_failure` → terminal), so real
///    chains advance through **Succeeded/ContextOverflow** resolutions. C8 checks
///    chain-advancing, not Failed, or every honest 0.2.63 overflow-retry log would read forged.
/// 2. §5's per-adjacent-pair walk needs published-effect causation, which the record format
///    deliberately omits (the step payload stays out of records — only step_digest). C8
///    verifies the chain's endpoints journal-directly and delegates the middle to the C3
///    re-plan's determinism: a segment that restored cleanly contains only steps the kernel's
///    own rules produced.
///
/// Plane lag degrades, never fails: the journal may trail the SessionLog, so a head whose
/// resolution hasn't landed yet (pending) or an effect claiming a step past the journal's tip
/// is unverifiable, not forged. A first-try chain (`invocation_id == effect_id`) has no
/// adjacency to prove. Old logs without `invocation_id` degrade per C7.
fn check_c8(streams: &[SessionStream], outcomes: &[SegmentOutcome]) -> RuleReport {
    let rule = "C8".to_string();
    let segments: HashMap<&str, &SegmentOutcome> = outcomes
        .iter()
        .filter(|outcome| outcome.report.operation_id != UNATTRIBUTED_SEGMENT)
        .map(|outcome| (outcome.report.operation_id.as_str(), outcome))
        .collect();

    let mut llm_completed_events = 0usize;
    let mut checked = 0usize;
    let mut trivial = 0usize;
    let mut unverifiable = 0usize;
    let mut violations: Vec<String> = Vec::new();
    let mut degraded_notes: Vec<String> = Vec::new();

    for (index, stream) in streams.iter().enumerate() {
        for event in &stream.events {
            let EvidenceEvent::LlmCompleted {
                effect_id,
                invocation_id,
            } = event
            else {
                continue;
            };
            llm_completed_events += 1;
            let (Some(head), Some(selected)) = (invocation_id.as_deref(), effect_id.as_deref())
            else {
                // A 0.2.62 log's llm_completed lacks the additive fields. C7: unverifiable,
                // never failed.
                unverifiable += 1;
                continue;
            };
            if head == selected {
                trivial += 1;
                continue;
            }

            let (Some((head_op, head_step)), Some((selected_op, selected_step))) =
                (parse_effect_step(head), parse_effect_step(selected))
            else {
                violations.push(format!(
                    "stream #{index}: llm_completed claims invocation {head} → {selected}, but \
                     one of the pair is not in the `operation:step:N:effect:M` vocabulary"
                ));
                continue;
            };
            if head_op != selected_op {
                violations.push(format!(
                    "stream #{index}: llm_completed claims invocation {head} → {selected} — an \
                     invocation chain cannot cross operations"
                ));
                continue;
            }
            if selected_step <= head_step {
                violations.push(format!(
                    "stream #{index}: llm_completed claims invocation {head} → {selected}, but \
                     the selected effect does not follow the chain head"
                ));
                continue;
            }
            let Some(outcome) = segments.get(head_op) else {
                degraded_notes.push(format!(
                    "stream #{index}: operation {head_op} has no journal segment (the journal \
                     may cover a subset of the session)"
                ));
                continue;
            };
            let Some(effects) = &outcome.effects else {
                degraded_notes.push(format!(
                    "stream #{index}: segment {head_op} could not be re-planned, so the \
                     invocation {head} → {selected} is unverifiable"
                ));
                continue;
            };

            // The head must be chain-advancing.
            match effects.resolutions.get(head) {
                Some(ResolutionFact::Completed) | Some(ResolutionFact::Other) => {
                    violations.push(format!(
                        "stream #{index}: llm_completed claims invocation {head} → {selected}, \
                         but {head} resolved to completion — a completed effect closes its \
                         invocation; nothing chains from it"
                    ));
                    continue;
                }
                Some(ResolutionFact::Overflow) | Some(ResolutionFact::Failed) => {}
                None if effects.published.contains(head) => degraded_notes.push(format!(
                    "stream #{index}: chain head {head} is published but its resolution has \
                     not landed in the journal (the planes are not synchronised)"
                )),
                None if head_step > effects.max_step => degraded_notes.push(format!(
                    "stream #{index}: chain head {head} claims step {head_step}, past the \
                     journal's tip (step {})",
                    effects.max_step
                )),
                None => {
                    violations.push(format!(
                        "stream #{index}: llm_completed claims invocation head {head}, but the \
                         deterministic re-plan of {head_op} never published it"
                    ));
                    continue;
                }
            }

            // The selected effect must exist on the chain.
            if effects.resolutions.contains_key(selected) {
                checked += 1;
            } else if effects.published.contains(selected) || selected_step > effects.max_step {
                degraded_notes.push(format!(
                    "stream #{index}: selected effect {selected} is not resolved in the \
                     journal (the planes are not synchronised)"
                ));
            } else {
                violations.push(format!(
                    "stream #{index}: llm_completed selects {selected}, but the deterministic \
                     re-plan of {selected_op} never published it"
                ));
            }
        }
    }

    if !violations.is_empty() {
        return RuleReport {
            rule,
            verdict: Verdict::Fail,
            detail: violations.join("; "),
        };
    }
    if llm_completed_events == 0 {
        return RuleReport {
            rule,
            verdict: Verdict::Degraded,
            detail: "no llm_completed events in any stream — invocation adjacency unchecked"
                .to_string(),
        };
    }
    if !degraded_notes.is_empty() || unverifiable > 0 {
        let mut detail = degraded_notes.join("; ");
        if unverifiable > 0 {
            if !detail.is_empty() {
                detail.push_str("; ");
            }
            detail.push_str(&format!(
                "{unverifiable} llm_completed event(s) without invocation_id/effect_id \
                 (pre-0.2.63 fields — C7)"
            ));
        }
        if !detail.is_empty() {
            return RuleReport {
                rule,
                verdict: Verdict::Degraded,
                detail: format!(
                    "{checked} retried invocation(s) verified, {trivial} first-try chain(s) \
                     closed; {detail}"
                ),
            };
        }
    }
    RuleReport {
        rule,
        verdict: Verdict::Pass,
        detail: format!(
            "{checked} retried invocation(s) verified end-to-end, {trivial} first-try chain(s) \
             closed"
        ),
    }
}

/// Strict first, lenient second: a record that fails the strict decode but still shows its
/// identity fields retains its context for C7 reporting. Proven digest corruption still fails
/// C1; only unavailable evidence degrades. Anything else is not a record.
fn classify(ordinal: usize, bytes: &[u8]) -> Option<Hop> {
    let error = match KernelRecord::from_record_bytes(bytes) {
        Ok(record) => return Some(Hop::Complete(record)),
        Err(error) => error,
    };
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let string = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    // `step_seq` rides the wire as a branded decimal string (scalar.rs), but an old-format or
    // foreign record may carry a bare number — accept both.
    let step_seq = object.get("step_seq").and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    });
    let degraded = DegradedRecord {
        ordinal,
        operation_id: string("operation_id"),
        input_id: string("input_id"),
        step_seq,
        previous_record_digest: string("previous_record_digest"),
        record_digest: string("record_digest"),
        reason: format!("{}: {}", error.code().as_str(), error.message()),
        integrity_failure: matches!(error, RecordError::DigestMismatch(_)),
    };
    // An old-format record must still answer "which chain, which hop" to count as evidence;
    // without either it is unparseable input.
    if degraded.operation_id.is_some() || degraded.step_seq.is_some() {
        Some(Hop::Degraded(degraded))
    } else {
        None
    }
}

fn validate_segment(operation_id: &str, mut hops: Vec<Hop>) -> SegmentOutcome {
    // The chain's own fields define the order; the input order is a storage detail. Hops that
    // cannot say where they sit sort last, in input order.
    hops.sort_by_key(|hop| {
        (
            hop.step_seq().unwrap_or(u64::MAX),
            match hop {
                Hop::Complete(_) => 0usize,
                Hop::Degraded(degraded) => degraded.ordinal,
            },
        )
    });

    let degraded_hops: Vec<DegradedHop> = hops
        .iter()
        .filter_map(|hop| match hop {
            Hop::Degraded(degraded) => Some(degraded.marking()),
            Hop::Complete(_) => None,
        })
        .collect();
    let hop_count = hops.len();

    let c1 = check_c1(&hops);
    let replan = replan_segment(&hops, &c1);
    let c2 = check_c2(&hops);
    let c3 = render_c3(&replan);
    let c4 = check_c4(&hops, operation_id);

    // C6.1's membership evidence: when the re-plan ran, the operation's published effects are
    // exactly (journal-resolved effect ids) ∪ (still-pending effects after the re-plan).
    let effects = match &replan {
        Replan::Restored(restored) => {
            let mut published: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut resolutions: HashMap<String, ResolutionFact> = HashMap::new();
            for hop in &hops {
                if let Some((effect_id, fact)) = resolution_of(hop) {
                    published.insert(effect_id.clone());
                    resolutions.insert(effect_id, fact);
                }
            }
            published.extend(
                restored
                    .transaction
                    .pending_effects()
                    .map(|effect| effect.effect_id.as_str().to_string()),
            );
            let max_step = hops
                .iter()
                .filter_map(|hop| hop.step_seq())
                .max()
                .unwrap_or(0);
            Some(SegmentEffects {
                max_step,
                published,
                resolutions,
            })
        }
        _ => None,
    };

    SegmentOutcome {
        report: SegmentReport {
            operation_id: operation_id.to_string(),
            hops: hop_count,
            degraded_hops,
            rules: vec![c1, c2, c3, c4],
        },
        effects,
    }
}

/// A segment's verdict plus the cross-plane evidence C6 needs from it.
struct SegmentOutcome {
    report: SegmentReport,
    /// `Some` iff the deterministic re-plan ran (the same condition under which C3 passes).
    effects: Option<SegmentEffects>,
}

struct SegmentEffects {
    /// The highest step the journal reaches — an attempt naming a later step is unverifiable
    /// (prefix), not forged.
    max_step: u64,
    /// Every effect the operation published through the journal's tip.
    published: std::collections::HashSet<String>,
    /// The journal-direct resolution fact per resolved effect — C8's adjacency evidence.
    resolutions: HashMap<String, ResolutionFact>,
}

/// How a resolved effect's outcome bears on an invocation chain (C8). The two planes are not
/// synchronised, so this is read only on fully restored segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionFact {
    /// Succeeded with `ProviderOutcome::Completed` — closes the invocation; nothing chains.
    Completed,
    /// Succeeded with `ProviderOutcome::ContextOverflow` — the compaction ladder republishes
    /// call_provider: the one chain-advancing resolution in today's kernel (see check_c8).
    Overflow,
    /// `EffectOutcome::Failed` — terminal for CallProvider under DEC-5 today, but
    /// chain-advancing under any future kernel with a failure retry ladder; a restored
    /// segment has already proven the kernel itself walked whatever follows.
    Failed,
    /// Any other resolution (tools/spawn/syscall/...) — never invocation-chain-advancing.
    Other,
}

/// The effect a record's ResolveEffect input settles and how, if it is one — the
/// journal-direct resolution facts (the same read C4 makes, with the outcome kept).
fn resolution_of(hop: &Hop) -> Option<(String, ResolutionFact)> {
    let Hop::Complete(record) = hop else {
        return None;
    };
    let input = record.normalized_input().ok()?;
    let NormalizedPayload::ResolveEffect(resolve) = &input.input else {
        return None;
    };
    let fact = match &resolve.outcome {
        EffectOutcome::Failed(_) => ResolutionFact::Failed,
        EffectOutcome::Succeeded(success) => match &success.result {
            EffectSuccess::Provider(provider) => match &provider.outcome {
                ProviderOutcome::Completed(_) => ResolutionFact::Completed,
                ProviderOutcome::ContextOverflow(_) => ResolutionFact::Overflow,
            },
            _ => ResolutionFact::Other,
        },
    };
    Some((resolve.effect_id.as_str().to_string(), fact))
}

/// C1 · chain integrity.
fn check_c1(hops: &[Hop]) -> RuleReport {
    let rule = "C1".to_string();
    if hops.is_empty() {
        return RuleReport {
            rule,
            verdict: Verdict::Degraded,
            detail: "no records in this segment".to_string(),
        };
    }
    let all_complete = hops.iter().all(|hop| matches!(hop, Hop::Complete(_)));
    if all_complete {
        let records: Vec<KernelRecord> = hops
            .iter()
            .filter_map(|hop| match hop {
                Hop::Complete(record) => Some(record.clone()),
                Hop::Degraded(_) => None,
            })
            .collect();
        return match verify_record_chain(&records) {
            Ok(genesis_digest) => RuleReport {
                rule,
                verdict: Verdict::Pass,
                detail: format!(
                    "{} record(s), genesis {genesis_digest}, every link verified",
                    records.len()
                ),
            },
            Err(error) => RuleReport {
                rule,
                verdict: Verdict::Fail,
                detail: format!("{}: {}", error.code().as_str(), error.message()),
            },
        };
    }

    // Mixed segment: check every link whose digests survived, and the genesis claim when the
    // first hop can make one. Degraded hops verify nothing themselves.
    let mut broken: Vec<String> = hops
        .iter()
        .filter_map(|hop| match hop {
            Hop::Degraded(record) if record.integrity_failure => Some(record.reason.clone()),
            _ => None,
        })
        .collect();
    let mut unverifiable_links = 0usize;
    let mut previous: Option<(&Hop, Option<&KernelRecord>)> = None;
    for hop in hops {
        let step = hop.step_seq();
        let (prev_digest, _) = digests_of(hop);
        if let Some((previous_hop, previous_complete)) = previous {
            let previous_step = previous_hop.step_seq();
            let previous_digest = digests_of(previous_hop).1;
            match (prev_digest, previous_digest) {
                (Some(expected), Some(actual)) if expected != actual => broken.push(format!(
                    "hop at step {} expects head {expected}, but its predecessor's digest is \
                     {actual}",
                    step.map_or("?".to_string(), |seq| seq.to_string()),
                )),
                (None, _) => unverifiable_links += 1,
                (_, None) => unverifiable_links += 1,
                _ => {}
            }
            match (step, previous_step) {
                (Some(step), Some(previous_step)) if step != previous_step + 1 => broken.push(
                    format!("hop is step {step}, but its predecessor is step {previous_step}"),
                ),
                (Some(_), Some(_)) => {}
                _ => unverifiable_links += 1,
            }
            // `verify_follows` is only meaningful across an unbroken run of complete records:
            // a degraded hop in between severs the chain of custody for the +1/digest pair.
            if let (Hop::Complete(record), Some(previous_record)) = (hop, previous_complete)
                && let Err(error) = record.verify_follows(Some(previous_record))
            {
                broken.push(format!("{}: {}", error.code().as_str(), error.message()));
            }
        } else if let Hop::Complete(record) = hop
            && let Err(error) = record.verify_follows(None)
        {
            broken.push(format!("{}: {}", error.code().as_str(), error.message()));
        }
        previous = Some((
            hop,
            match hop {
                Hop::Complete(record) => Some(record),
                Hop::Degraded(_) => None,
            },
        ));
    }

    if !broken.is_empty() {
        return RuleReport {
            rule,
            verdict: Verdict::Fail,
            detail: broken.join("; "),
        };
    }
    RuleReport {
        rule,
        verdict: Verdict::Degraded,
        detail: format!(
            "partial chain: every surviving link verified, {unverifiable_links} link(s) \
             unverifiable across degraded hop(s)"
        ),
    }
}

/// C2 · input idempotency: one input_id, one record.
fn check_c2(hops: &[Hop]) -> RuleReport {
    let rule = "C2".to_string();
    let mut by_input: HashMap<&str, &str> = HashMap::new();
    let mut conflicts: Vec<String> = Vec::new();
    let mut retries = 0usize;
    let mut unverifiable = 0usize;
    for hop in hops {
        let (input_id, record_digest) = match hop {
            Hop::Complete(record) => (
                Some(record.input_id().as_str()),
                Some(record.record_digest().as_str()),
            ),
            Hop::Degraded(degraded) => (
                degraded.input_id.as_deref(),
                degraded.record_digest.as_deref(),
            ),
        };
        let Some(input_id) = input_id else { continue };
        let Some(digest) = record_digest else {
            unverifiable += 1;
            continue;
        };
        match by_input.get(input_id) {
            Some(existing) if *existing != digest => conflicts.push(format!(
                "input {input_id} produced two different records ({existing} and {digest}); a \
                 retry must reach the same record"
            )),
            Some(_) => retries += 1,
            None => {
                by_input.insert(input_id, digest);
            }
        }
    }
    if !conflicts.is_empty() {
        return RuleReport {
            rule,
            verdict: Verdict::Fail,
            detail: conflicts.join("; "),
        };
    }
    if unverifiable > 0 {
        return RuleReport {
            rule,
            verdict: Verdict::Degraded,
            detail: format!(
                "{} unique input(s), {retries} idempotent retry hit(s); {unverifiable} degraded \
                 hop(s) could not be compared",
                by_input.len(),
            ),
        };
    }
    RuleReport {
        rule,
        verdict: Verdict::Pass,
        detail: format!(
            "{} unique input(s), {retries} idempotent retry hit(s), no divergent duplicates",
            by_input.len(),
        ),
    }
}

/// C3 · causal closure runs on the §12.2 genesis-leg restore: the deterministic re-plan of
/// every transition. Batch 3 shares that one restore with C6.1 — the restored transaction
/// answers "did this operation ever publish effect X" — so the restore happens once per
/// segment, here, and C3's report only renders the outcome.
enum Replan {
    /// C3/C6.1 degrade: the re-plan never ran.
    Unavailable(&'static str),
    /// The restore itself faulted — C3 fails.
    Failed(String),
    Restored(RestoredOperation),
}

fn replan_segment(hops: &[Hop], c1: &RuleReport) -> Replan {
    if hops.iter().any(|hop| matches!(hop, Hop::Degraded(_))) {
        return Replan::Unavailable(
            "re-plan requires complete records; this segment has degraded hops",
        );
    }
    if c1.verdict == Verdict::Fail {
        return Replan::Unavailable(
            "C1 failed; a re-plan over a broken chain would only re-report that break",
        );
    }
    let records: Vec<KernelRecord> = hops
        .iter()
        .filter_map(|hop| match hop {
            Hop::Complete(record) => Some(record.clone()),
            Hop::Degraded(_) => None,
        })
        .collect();
    if records.is_empty() {
        return Replan::Unavailable("no records in this segment");
    }
    match restore_operation(
        None,
        &records,
        ConfigDefaults::default(),
        InMemoryRecordIndex::from_records(&records),
    ) {
        Ok(restored) => Replan::Restored(restored),
        Err(fault) => Replan::Failed(format!("{}: {}", fault.code.as_str(), fault.message)),
    }
}

fn render_c3(replan: &Replan) -> RuleReport {
    let rule = "C3".to_string();
    match replan {
        Replan::Unavailable(reason) => RuleReport {
            rule,
            verdict: Verdict::Degraded,
            detail: (*reason).to_string(),
        },
        Replan::Failed(fault) => RuleReport {
            rule,
            verdict: Verdict::Fail,
            detail: fault.clone(),
        },
        Replan::Restored(restored) => RuleReport {
            rule,
            verdict: Verdict::Pass,
            detail: format!(
                "re-planned {} record(s) from genesis; every durable record digest reproduced",
                restored.cost.records_before_checkpoint
            ),
        },
    }
}

/// C4 · task lineage, the journal-direct half.
fn check_c4(hops: &[Hop], operation_id: &str) -> RuleReport {
    let rule = "C4".to_string();
    struct LaunchFact {
        task_id: String,
        attempt_id: String,
        step_seq: u64,
        effect_id: String,
    }

    let mut launches: Vec<LaunchFact> = Vec::new();
    let mut unreadable_inputs = 0usize;
    for hop in hops {
        let Hop::Complete(record) = hop else { continue };
        let input = match record.normalized_input() {
            Ok(input) => input,
            Err(_) => {
                unreadable_inputs += 1;
                continue;
            }
        };
        let NormalizedPayload::ResolveEffect(resolve) = &input.input else {
            continue;
        };
        let EffectOutcome::Succeeded(success) = &resolve.outcome else {
            continue;
        };
        let EffectSuccess::TasksSpawned(spawned) = &success.result else {
            continue;
        };
        for attempt in &spawned.attempts {
            launches.push(LaunchFact {
                task_id: attempt.task_id.as_str().to_string(),
                attempt_id: attempt.attempt_id.as_str().to_string(),
                step_seq: record.step_seq().get(),
                effect_id: resolve.effect_id.as_str().to_string(),
            });
        }
    }

    let mut violations: Vec<String> = Vec::new();
    let mut seen: HashMap<(&str, &str), u64> = HashMap::new();
    for fact in &launches {
        let pair = (fact.task_id.as_str(), fact.attempt_id.as_str());
        if let Some(first_step) = seen.insert(pair, fact.step_seq) {
            violations.push(format!(
                "task {} attempt {} launched at steps {first_step} and {}; the launch token is \
                 derived from that pair, so a repeated pair is a reused LaunchToken",
                fact.task_id, fact.attempt_id, fact.step_seq,
            ));
        }
        match parse_effect_step(&fact.effect_id) {
            Some((effect_operation, effect_step)) => {
                if effect_operation != operation_id {
                    violations.push(format!(
                        "task {} launch at step {} resolves effect {} of another operation — \
                         causation cannot cross operations",
                        fact.task_id, fact.step_seq, fact.effect_id,
                    ));
                } else if effect_step >= fact.step_seq {
                    violations.push(format!(
                        "task {} launch resolved at step {} names an effect published at step \
                         {effect_step} — the resolution precedes the publication",
                        fact.task_id, fact.step_seq,
                    ));
                }
            }
            None => violations.push(format!(
                "task {} launch at step {} names effect {}, which is not in the \
                 `operation:step:N:effect:M` vocabulary",
                fact.task_id, fact.step_seq, fact.effect_id,
            )),
        }
    }

    if !violations.is_empty() {
        return RuleReport {
            rule,
            verdict: Verdict::Fail,
            detail: violations.join("; "),
        };
    }
    let degraded_hops = hops
        .iter()
        .filter(|hop| matches!(hop, Hop::Degraded(_)))
        .count();
    if degraded_hops > 0 || unreadable_inputs > 0 {
        return RuleReport {
            rule,
            verdict: Verdict::Degraded,
            detail: format!(
                "{} launch(es) checked; {degraded_hops} degraded hop(s) and \
                 {unreadable_inputs} unreadable input(s) could hide further launches",
                launches.len(),
            ),
        };
    }
    RuleReport {
        rule,
        verdict: Verdict::Pass,
        detail: format!(
            "{} launch(es), every (task_id, attempt_id) pair unique, every spawn resolution \
             names an earlier step of this operation",
            launches.len(),
        ),
    }
}

/// The kernel's effect-id vocabulary is `{operation}:step:{N}:effect:{M}` (driver minting).
/// Operation ids may themselves contain colons, so parse from the right.
fn parse_effect_step(effect_id: &str) -> Option<(&str, u64)> {
    let (before_effect, _) = effect_id.rsplit_once(":effect:")?;
    let (operation, step) = before_effect.rsplit_once(":step:")?;
    Some((operation, step.parse().ok()?))
}

fn digests_of(hop: &Hop) -> (Option<&str>, Option<&str>) {
    match hop {
        Hop::Complete(record) => (
            record
                .previous_record_digest()
                .map(|digest| digest.as_str()),
            Some(record.record_digest().as_str()),
        ),
        Hop::Degraded(degraded) => (
            degraded.previous_record_digest.as_deref(),
            degraded.record_digest.as_deref(),
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// batch 2 · the checkpoint evidence plane (C5, 0.2.65 S1)
// ---------------------------------------------------------------------------------------------

/// Verdict accumulation for one C5 rule — the C7 ordering every other rule uses: any proven
/// contradiction fails, else any unverifiable clause degrades, else pass.
struct C5Clauses {
    violations: Vec<String>,
    degradations: Vec<String>,
    confirmations: Vec<String>,
}

impl C5Clauses {
    fn new() -> Self {
        Self {
            violations: Vec::new(),
            degradations: Vec::new(),
            confirmations: Vec::new(),
        }
    }

    fn violation(&mut self, detail: String) {
        self.violations.push(detail);
    }

    fn degraded(&mut self, detail: String) {
        self.degradations.push(detail);
    }

    fn confirms(&mut self, detail: String) {
        self.confirmations.push(detail);
    }

    fn report(self, rule: &str) -> RuleReport {
        let rule = rule.to_string();
        if !self.violations.is_empty() {
            return RuleReport {
                rule,
                verdict: Verdict::Fail,
                detail: self.violations.join("; "),
            };
        }
        if !self.degradations.is_empty() {
            let mut detail = self.degradations.join("; ");
            if !self.confirmations.is_empty() {
                detail.push_str("; ");
                detail.push_str(&self.confirmations.join("; "));
            }
            return RuleReport {
                rule,
                verdict: Verdict::Degraded,
                detail,
            };
        }
        RuleReport {
            rule,
            verdict: Verdict::Pass,
            detail: if self.confirmations.is_empty() {
                "nothing to anchor".to_string()
            } else {
                self.confirmations.join("; ")
            },
        }
    }
}

/// Decode and judge every checkpoint blob against the journal's per-operation records.
/// Returns the C5 verdicts, the blob count that did not decode, and the count that did.
fn check_c5<C: AsRef<[u8]>>(
    segments: &HashMap<String, Vec<KernelRecord>>,
    blobs: &[C],
    strict: bool,
) -> (Vec<RuleReport>, usize, usize) {
    let mut checks = Vec::new();
    let mut unparseable = 0usize;
    let mut parsed = 0usize;
    for blob in blobs {
        match KernelCheckpoint::from_checkpoint_bytes(blob.as_ref()) {
            Ok(checkpoint) => {
                parsed += 1;
                let records = segments.get(checkpoint.operation_id().as_str());
                // The strict fold feeds both rules, so it runs once per checkpoint.
                let replay = strict.then(|| strict_replay(&checkpoint, records.map(Vec::as_slice)));
                checks.push(check_c5a(
                    &checkpoint,
                    records.map(Vec::as_slice),
                    replay.as_ref(),
                ));
                checks.push(check_c5b(&checkpoint, replay.as_ref()));
            }
            Err(error) => {
                unparseable += 1;
                checks.push(RuleReport {
                    rule: "C5a".to_string(),
                    verdict: Verdict::Degraded,
                    detail: format!(
                        "checkpoint blob did not decode — its claims are unverifiable (C7): {}",
                        error.message()
                    ),
                });
            }
        }
    }
    (checks, unparseable, parsed)
}

/// What `--strict` produced for one checkpoint. The re-plan replay folds the journal from
/// genesis through the checkpoint's own anchor steps through the same restore path C3 uses,
/// re-derives the checkpoint the fold would have written, and independently drives the
/// checkpoint+tail restore ladder against the records above the covered step. A windowed
/// checkpoint (base < through) captures its logical state **at the base** and bridges to
/// `through` with its bounded tail, so the fold lands on two steps: base, and covered.
enum StrictReplay {
    /// The replay could not run on this evidence — unverifiable, never a failure.
    Skipped(String),
    /// The fold or the ladder itself refused the records — a proven inconsistency.
    Faulted(String),
    Done {
        /// The re-derived checkpoint at the checkpoint's base step — the state its
        /// `state_digest` and launch-token ledger claim. A windowed checkpoint (base <
        /// through) captures its logical state **at the base** and bridges to `through` with
        /// its bounded tail, so this is the fold the checkpoint's claims answer to; the tail's
        /// landing is proven by the ladder arm plus C5a's digest reconciliation.
        at_base: KernelCheckpoint,
        ladder: Result<(), String>,
        above_records: usize,
    },
}

fn strict_replay(checkpoint: &KernelCheckpoint, records: Option<&[KernelRecord]>) -> StrictReplay {
    let through = checkpoint.through_step_seq().get();
    let base = checkpoint.base_step_seq().get();
    let Some(records) = records else {
        return StrictReplay::Skipped(
            "the journal holds no segment for this operation".to_string(),
        );
    };

    // The fold must start at the real genesis and run unbroken to the covered step; anything
    // less would re-derive a *different* history and every mismatch it reported would be an
    // artifact of the gap, not of the checkpoint.
    let mut steps: Vec<u64> = records
        .iter()
        .map(|record| record.step_seq().get())
        .filter(|step| *step <= through)
        .collect();
    steps.sort_unstable();
    steps.dedup();
    if steps != (0..=through).collect::<Vec<u64>>() {
        return StrictReplay::Skipped(format!(
            "the journal does not hold an unbroken record run from step 0 through {through} \
             (pruned prefix or partial copy); the re-plan replay cannot start at genesis"
        ));
    }

    // The fold to the base answers the checkpoint's own claims (state digest, ledger); the
    // tail's landing is proven by the ladder arm plus C5a's digest reconciliation, so one
    // fold suffices for both full-state and windowed checkpoints.
    let fold_to_base = || -> Result<KernelCheckpoint, String> {
        let mut prefix: Vec<&KernelRecord> = records
            .iter()
            .filter(|record| record.step_seq().get() <= base)
            .collect();
        prefix.sort_by_key(|record| record.step_seq().get());
        let prefix: Vec<KernelRecord> = prefix.into_iter().cloned().collect();
        let folded = restore_operation(
            None,
            &prefix,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(&prefix),
        )
        .map_err(|fault| format!("{}: {}", fault.code.as_str(), fault.message))?;
        folded
            .transaction
            .checkpoint_candidate(folded.driver.project_logical_state())
            .map_err(|fault| format!("{}: {}", fault.code.as_str(), fault.message))?
            .decode()
            .map_err(|error| {
                format!(
                    "the re-derived checkpoint does not decode: {}",
                    error.message()
                )
            })
    };
    let at_base = match fold_to_base() {
        Ok(checkpoint) => checkpoint,
        Err(fault) => return StrictReplay::Faulted(fault),
    };

    // The other half of §12.2: the checkpoint plus the journal above it must drive the real
    // restore. An empty tail (checkpoint at the journal head) still proves the ladder's first
    // three lines.
    let mut above: Vec<&KernelRecord> = records
        .iter()
        .filter(|record| record.step_seq().get() > through)
        .collect();
    above.sort_by_key(|record| record.step_seq().get());
    let above_records = above.len();
    let above: Vec<KernelRecord> = above.into_iter().cloned().collect();
    let ladder = restore_operation(
        Some(checkpoint),
        &above,
        ConfigDefaults::default(),
        InMemoryRecordIndex::from_records(&above),
    )
    .map(|_: RestoredOperation<InMemoryRecordIndex>| ())
    .map_err(|fault| format!("{}: {}", fault.code.as_str(), fault.message));

    StrictReplay::Done {
        at_base,
        ladder,
        above_records,
    }
}

/// C5a · every digest the checkpoint claims about the journal must anchor. A present record
/// with the wrong digest is a proven contradiction; a pruned or missing record is unverifiable.
/// Under `--strict`, the re-plan must also reproduce the captured state.
fn check_c5a(
    checkpoint: &KernelCheckpoint,
    records: Option<&[KernelRecord]>,
    replay: Option<&StrictReplay>,
) -> RuleReport {
    let rule = "C5a";
    let mut clauses = C5Clauses::new();
    let Some(records) = records else {
        return RuleReport {
            rule: rule.to_string(),
            verdict: Verdict::Degraded,
            detail: format!(
                "checkpoint names operation {}; the journal holds no segment for it, so no \
                 anchor can be checked",
                checkpoint.operation_id()
            ),
        };
    };
    let through = checkpoint.through_step_seq().get();

    // The genesis anchor: a checkpoint binds itself to the operation's identity record.
    match record_at(records, 0) {
        Some(genesis)
            if genesis.record_digest().as_str() != checkpoint.genesis_digest().as_str() =>
        {
            clauses.violation(format!(
                "the journal's genesis record hashes to {}, but the checkpoint binds genesis {} — \
                 this checkpoint was captured on another chain",
                genesis.record_digest(),
                checkpoint.genesis_digest()
            ));
        }
        Some(_) => clauses.confirms("genesis digest anchored".to_string()),
        None => clauses.degraded(
            "the genesis record is not in the journal (pruned prefix); the identity anchor is \
             unverifiable"
                .to_string(),
        ),
    }

    // The covered-head anchor: §12.3 rule 2 — the covered head names the through step, not the
    // journal's current tip.
    match record_at(records, through) {
        Some(record)
            if record.record_digest().as_str()
                != checkpoint.covered_transaction_head_digest().as_str() =>
        {
            clauses.violation(format!(
                "the journal record at the covered step {through} hashes to {}, but the \
                 checkpoint's covered head is {}",
                record.record_digest(),
                checkpoint.covered_transaction_head_digest()
            ));
        }
        Some(_) => clauses.confirms(format!("covered head anchored at step {through}")),
        None => clauses.degraded(format!(
            "no journal record at the covered step {through}; the covered head is unverifiable"
        )),
    }

    // The base anchor and the bounded-tail reconciliation matter only when the checkpoint
    // covers a window (base < through); a full-state checkpoint's base is its covered head.
    let base = checkpoint.base_step_seq().get();
    if base != through {
        match record_at(records, base) {
            Some(record)
                if record.record_digest().as_str() != checkpoint.base_record_digest().as_str() =>
            {
                clauses.violation(format!(
                    "the journal record at the tail base step {base} hashes to {}, but the \
                     checkpoint anchors its tail on {}",
                    record.record_digest(),
                    checkpoint.base_record_digest()
                ));
            }
            _ => {}
        }
    }
    let mut reconciled = 0usize;
    let mut pruned = 0usize;
    for entry in checkpoint.tail_inputs() {
        match record_at(records, entry.step_seq.get()) {
            Some(record) if record.record_digest().as_str() != entry.record_digest.as_str() => {
                clauses.violation(format!(
                    "the journal record at step {} disagrees with the checkpoint's bounded tail \
                     (journal {}, checkpoint {})",
                    entry.step_seq.get(),
                    record.record_digest(),
                    entry.record_digest
                ));
            }
            Some(_) => reconciled += 1,
            None => pruned += 1,
        }
    }
    if reconciled > 0 {
        clauses.confirms(format!(
            "{reconciled} bounded-tail entries reconcile with the journal"
        ));
    }
    if pruned > 0 {
        clauses.degraded(format!(
            "{pruned} bounded-tail entries have no journal record (pruned interval)"
        ));
    }

    match replay {
        Some(StrictReplay::Skipped(reason)) => {
            clauses.degraded(format!("strict replay skipped: {reason}"));
        }
        Some(StrictReplay::Faulted(fault)) => {
            clauses.violation(format!("strict replay faulted: {fault}"));
        }
        Some(StrictReplay::Done {
            at_base,
            ladder,
            above_records,
        }) => {
            let base = checkpoint.base_step_seq().get();
            if at_base.state_digest() != checkpoint.state_digest() {
                clauses.violation(format!(
                    "strict replay folds the journal to state digest {} at the checkpoint's \
                     base step {base}, but the checkpoint captured {} there",
                    at_base.state_digest(),
                    checkpoint.state_digest()
                ));
            } else {
                clauses.confirms(format!(
                    "strict replay reproduces the captured state digest at step {base}"
                ));
            }
            match ladder {
                Ok(()) => clauses.confirms(format!(
                    "the checkpoint+tail restore ladder holds against the {above_records} \
                     journal record(s) above the covered step"
                )),
                Err(fault) => clauses.violation(format!(
                    "the checkpoint+tail restore ladder faults against this journal: {fault}"
                )),
            }
        }
        None => {}
    }

    clauses.report(rule)
}

/// C5b · the durable launch-token ledger — the batch-2 half C4 defers to this plane. Within
/// one checkpoint, no token may name two mints at different steps (reuse across `TaskLaunch`
/// payloads), every pending `SpawnTasks` effect must carry a token the ledger registered at
/// the effect's own step, and no entry may sit beyond the covered boundary. Under `--strict`
/// the re-plan must re-derive the exact ledger.
fn check_c5b(checkpoint: &KernelCheckpoint, replay: Option<&StrictReplay>) -> RuleReport {
    let rule = "C5b";
    let mut clauses = C5Clauses::new();
    let transition = &checkpoint.logical_state().transition;
    let through = checkpoint.through_step_seq().get();

    let mut mints: HashMap<&str, u64> = HashMap::new();
    for entry in &transition.launch_tokens {
        let token = entry.launch_token.as_str();
        let step = entry.step_seq.get();
        match mints.get(token) {
            Some(previous) if *previous != step => clauses.violation(format!(
                "launch token {token} is minted at step {step} and step {previous} — reuse \
                 across TaskLaunch payloads"
            )),
            Some(_) => clauses.violation(format!(
                "launch token {token} is registered twice at step {step} — a duplicated ledger \
                 entry"
            )),
            None => {
                mints.insert(token, step);
            }
        }
        if step > through {
            clauses.violation(format!(
                "launch token {token} is minted at step {step}, beyond the covered boundary \
                 {through}"
            ));
        }
    }

    for effect in &transition.pending_effects {
        let EffectKind::SpawnTasks(spawn) = &effect.effect else {
            continue;
        };
        let SpawnTasksEffect { tasks, .. } = spawn;
        let effect_step = parse_effect_step(effect.effect_id.as_str()).map(|(_, step)| step);
        for launch in tasks {
            let token = launch.launch_token.as_str();
            match (mints.get(token), effect_step) {
                (None, _) => clauses.violation(format!(
                    "pending effect {} carries launch token {token} the ledger never registered",
                    effect.effect_id
                )),
                (Some(&minted), Some(step)) if minted != step => clauses.violation(format!(
                    "pending effect {} carries launch token {token} minted at step {minted}, not \
                     at the effect's own step {step}",
                    effect.effect_id
                )),
                _ => {}
            }
        }
    }

    if !transition.launch_tokens.is_empty() {
        clauses.confirms(format!(
            "{} launch token(s) anchored; no reuse across TaskLaunch payloads",
            transition.launch_tokens.len()
        ));
    }

    match replay {
        Some(StrictReplay::Skipped(reason)) => {
            clauses.degraded(format!("strict replay skipped: {reason}"));
        }
        Some(StrictReplay::Faulted(fault)) => {
            clauses.violation(format!("strict replay faulted: {fault}"));
        }
        Some(StrictReplay::Done { at_base, .. }) => {
            let replayed_ledger = &at_base.logical_state().transition.launch_tokens;
            if replayed_ledger != &transition.launch_tokens {
                clauses.violation(format!(
                    "strict replay mints a different launch-token ledger: the journal fold \
                     registers {} token(s), the checkpoint carries {}",
                    replayed_ledger.len(),
                    transition.launch_tokens.len()
                ));
            } else {
                clauses.confirms(
                    "strict replay reproduces the launch-token ledger exactly".to_string(),
                );
            }
        }
        None => {}
    }

    clauses.report(rule)
}

/// The segment's complete record at one step, if the journal holds it.
fn record_at(records: &[KernelRecord], step_seq: u64) -> Option<&KernelRecord> {
    records
        .iter()
        .find(|record| record.step_seq().get() == step_seq)
}

// ---------------------------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::runtime::kernel::wire::config::{
        ConfigDefaults, ExecutionPolicy, HostEffectSupport, OperationConfig,
    };
    use crate::runtime::kernel::wire::driver::CanonicalOperationDriver;
    use crate::runtime::kernel::wire::effect::{
        EffectKindTag,
        EffectSucceeded,
        ProviderCompleted,
        ProviderContextOverflow,
        ProviderMessage,
        ProviderOutcome,
        ProviderSuccess,
        TaskLaunchOutcome,
        TaskLaunchStarted,
        TaskLaunchStatus,
        TasksSpawnedSuccess,
        // F5 alias discipline (0.2.66): wire-side imports of dual-family types name the
        // authority direction — the wire version is the ABI authority.
        ToolCall as WireToolCall,
    };
    use crate::runtime::kernel::wire::envelope::{
        ConfigureOperation, KernelInput, ResolveEffect, StartOperation, WireEnvelope,
    };
    use crate::runtime::kernel::wire::record::{KernelRecord, NormalizedInput};
    use crate::runtime::kernel::wire::root::{
        InitialContext, LogicalAgentSpec, LogicalMessage, LogicalTask, MessageRole, RootAgentEntry,
        RootEntry, RootWorkflowEntry, WorkflowNode as WireWorkflowNode,
        WorkflowSpec as WireWorkflowSpec,
    };
    use crate::runtime::kernel::wire::scalar::{
        AttemptId, BoundedJson, CallId, EffectId, InputId, NodeId, OperationId, TaskId, WireU64,
    };
    use crate::runtime::kernel::wire::transaction::{InMemoryRecordIndex, KernelTransaction};

    // -----------------------------------------------------------------------------------------
    // envelopes
    // -----------------------------------------------------------------------------------------

    fn operation(id: &str) -> OperationId {
        OperationId::new(id).unwrap()
    }

    fn envelope(op: &OperationId, id: &str, at: u64, input: KernelInput) -> WireEnvelope {
        WireEnvelope::new(
            op.clone(),
            InputId::new(id).unwrap(),
            WireU64::new(at),
            input,
        )
    }

    fn configure_envelope(op: &OperationId) -> WireEnvelope {
        envelope(
            op,
            "in-configure",
            1_700_000_000_000,
            KernelInput::ConfigureOperation(ConfigureOperation {
                config: OperationConfig {
                    execution_policy: Some(ExecutionPolicy {
                        max_turns: Some(12),
                        ..ExecutionPolicy::default()
                    }),
                    host_effect_support: HostEffectSupport::new([
                        EffectKindTag::CallProvider,
                        EffectKindTag::SpawnTasks,
                    ]),
                    ..OperationConfig::default()
                },
            }),
        )
    }

    fn agent_start_envelope(op: &OperationId) -> WireEnvelope {
        envelope(
            op,
            "in-start",
            1_700_000_001_000,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("write the brief"),
                    run_spec: Some(LogicalAgentSpec::new("write the brief")),
                }),
                initial_context: InitialContext::default(),
            }),
        )
    }

    /// An agent start carrying `messages` history items — the compaction ladder needs real
    /// history to reclaim, or the first context overflow exhausts recovery and terminates the
    /// operation instead of republishing a call_provider effect.
    fn agent_start_with_history_envelope(op: &OperationId, messages: usize) -> WireEnvelope {
        envelope(
            op,
            "in-start",
            1_700_000_001_000,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("write the brief"),
                    run_spec: Some(LogicalAgentSpec::new("write the brief")),
                }),
                initial_context: InitialContext {
                    messages: (0..messages)
                        .map(|index| LogicalMessage {
                            role: if index % 2 == 0 {
                                MessageRole::User
                            } else {
                                MessageRole::Assistant
                            },
                            content: format!(
                                "turn {index}: a long enough body that compaction has \
                                 something to reclaim when the prompt stops fitting"
                            ),
                            tokens: Some(64),
                            tool_call_id: None,
                        })
                        .collect(),
                    ..InitialContext::default()
                },
            }),
        )
    }

    fn workflow_start_envelope(op: &OperationId) -> WireEnvelope {
        envelope(
            op,
            "in-start",
            1_700_000_001_000,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Workflow(RootWorkflowEntry {
                    spec: WireWorkflowSpec {
                        name: "brief".to_string(),
                        nodes: vec![
                            WireWorkflowNode {
                                node_id: NodeId::new("collect").unwrap(),
                                task: LogicalTask::new("collect the sources"),
                                depends_on: vec![],
                                run_spec: Some(LogicalAgentSpec::new("collect the sources")),
                            },
                            WireWorkflowNode {
                                node_id: NodeId::new("write").unwrap(),
                                task: LogicalTask::new("write the brief"),
                                depends_on: vec![NodeId::new("collect").unwrap()],
                                run_spec: Some(LogicalAgentSpec::new("write the brief")),
                            },
                        ],
                    },
                }),
                initial_context: InitialContext::default(),
            }),
        )
    }

    fn resolve_overflow_envelope(op: &OperationId, effect_step: u64) -> WireEnvelope {
        envelope(
            op,
            "in-resolve",
            1_700_000_002_000,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: EffectId::new(format!("{op}:step:{effect_step}:effect:0")).unwrap(),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::Provider(ProviderSuccess {
                        outcome: ProviderOutcome::ContextOverflow(
                            ProviderContextOverflow::default(),
                        ),
                    }),
                }),
            }),
        )
    }

    /// A provider completion. `with_tool_call` makes the completion request a tool, so the
    /// next step publishes an ExecuteTools effect instead of terminating the operation.
    fn resolve_completed_envelope(
        op: &OperationId,
        id: &str,
        at: u64,
        effect_step: u64,
        with_tool_call: bool,
    ) -> WireEnvelope {
        envelope(
            op,
            id,
            at,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: EffectId::new(format!("{op}:step:{effect_step}:effect:0")).unwrap(),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::Provider(ProviderSuccess {
                        outcome: ProviderOutcome::Completed(ProviderCompleted {
                            message: ProviderMessage {
                                role: MessageRole::Assistant,
                                content: "done".to_string(),
                                tool_calls: if with_tool_call {
                                    vec![WireToolCall {
                                        call_id: CallId::new("call-1").unwrap(),
                                        name: "read_file".to_string(),
                                        arguments: BoundedJson::new(json!({})).unwrap(),
                                    }]
                                } else {
                                    Vec::new()
                                },
                                tool_call_id: None,
                                tokens: None,
                            },
                            observed_input_tokens: None,
                            observed_output_tokens: None,
                            stop_reason: None,
                        }),
                    }),
                }),
            }),
        )
    }

    fn resolve_spawn_envelope(
        op: &OperationId,
        id: &str,
        at: u64,
        effect_id: &str,
        tasks: &[(&str, &str)],
    ) -> WireEnvelope {
        envelope(
            op,
            id,
            at,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: EffectId::new(effect_id).unwrap(),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::TasksSpawned(TasksSpawnedSuccess {
                        attempts: tasks
                            .iter()
                            .map(|(task, attempt)| TaskLaunchOutcome {
                                task_id: TaskId::new(*task).unwrap(),
                                attempt_id: AttemptId::new(*attempt).unwrap(),
                                outcome: TaskLaunchStatus::Started(TaskLaunchStarted {}),
                            })
                            .collect(),
                    }),
                }),
            }),
        )
    }

    // -----------------------------------------------------------------------------------------
    // chain builders
    // -----------------------------------------------------------------------------------------

    /// The honest path: a live transaction driven by the real driver, so every record's step is
    /// exactly what a re-plan reproduces. This is what a host's journal prefix looks like.
    fn live_chain(envelopes: &[WireEnvelope]) -> Vec<KernelRecord> {
        let mut tx = KernelTransaction::new(ConfigDefaults::default(), InMemoryRecordIndex::new());
        let mut driver = CanonicalOperationDriver::new();
        let mut journal = Vec::new();
        for envelope in envelopes {
            let preparation = tx.prepare(envelope, |context| driver.plan(context));
            let token = preparation
                .token()
                .unwrap_or_else(|| {
                    panic!("expected a prepared step, got {:?}", preparation.fault())
                })
                .clone();
            let head = preparation.record().unwrap().record_digest().clone();
            let committed = tx.commit(&token, &head).expect("commit must succeed");
            journal.push(committed.record.clone());
            driver
                .note_committed(committed.step_seq)
                .expect("the driver folds the step it planned");
        }
        journal
    }

    /// A structurally sound chain whose steps are hand-pinned JSON — **not** the driver's plans.
    /// C1/C2/C4 read only the records, so they judge these chains; C3 necessarily fails on them
    /// (the re-plan cannot reproduce a hand-pinned step) and is simply not asserted there.
    fn hand_chain(envelopes: &[WireEnvelope]) -> Vec<KernelRecord> {
        let mut records: Vec<KernelRecord> = Vec::new();
        for (index, envelope) in envelopes.iter().enumerate() {
            let input = NormalizedInput::normalize(envelope, &ConfigDefaults::default())
                .expect("the envelope normalises");
            let step = json!({ "planned": format!("step-{index}"), "effects": [] });
            let record =
                KernelRecord::chain(records.last(), &input, &step).expect("the record chains");
            records.push(record);
        }
        records
    }

    fn blobs(records: &[KernelRecord]) -> Vec<Vec<u8>> {
        records
            .iter()
            .map(|record| record.record_bytes().into_vec())
            .collect()
    }

    fn rule<'a>(report: &'a ValidationReport, segment: usize, id: &str) -> &'a RuleReport {
        report.segments[segment]
            .rules
            .iter()
            .find(|rule| rule.rule == id)
            .unwrap_or_else(|| panic!("segment {segment} has no {id} verdict"))
    }

    fn cross<'a>(report: &'a ValidationReport, id: &str) -> &'a RuleReport {
        report
            .cross_checks
            .iter()
            .find(|rule| rule.rule == id)
            .unwrap_or_else(|| panic!("the report has no {id} cross-check"))
    }

    // -----------------------------------------------------------------------------------------
    // green paths
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_green_agent_chain_passes_every_rule() {
        let op = operation("op-green-agent");
        let chain = live_chain(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let report = validate_journal(&blobs(&chain));
        assert_eq!(report.segments.len(), 1);
        for id in ["C1", "C2", "C3", "C4"] {
            assert_eq!(
                rule(&report, 0, id).verdict,
                Verdict::Pass,
                "{id}: {}",
                rule(&report, 0, id).detail
            );
        }
        assert!(
            rule(&report, 0, "C3")
                .detail
                .contains("every durable record digest reproduced"),
            "C3 proves the re-plan: {}",
            rule(&report, 0, "C3").detail
        );
        assert_eq!(report.exit_code(), 0);
        assert_eq!(report.unparseable_records, 0);
    }

    #[test]
    fn a_green_workflow_chain_passes_c4_with_real_launches() {
        let op = operation("op-green-workflow");
        let chain = live_chain(&[
            configure_envelope(&op),
            workflow_start_envelope(&op),
            resolve_spawn_envelope(
                &op,
                "in-ack-1",
                1_700_000_002_000,
                "op-green-workflow:step:1:effect:0",
                &[("wf-node0", "wf-node0:attempt:1")],
            ),
        ]);
        let report = validate_journal(&blobs(&chain));
        assert_eq!(report.segments.len(), 1);
        for id in ["C1", "C2", "C3", "C4"] {
            assert_eq!(
                rule(&report, 0, id).verdict,
                Verdict::Pass,
                "{id}: {}",
                rule(&report, 0, id).detail
            );
        }
        assert!(
            rule(&report, 0, "C4").detail.contains("1 launch(es)"),
            "{}",
            rule(&report, 0, "C4").detail
        );
        assert_eq!(report.deferred.len(), 2, "batch-1 scope limits are named");
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn input_order_is_a_storage_detail() {
        let op = operation("op-shuffled");
        let chain = live_chain(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let mut shuffled = blobs(&chain);
        shuffled.reverse();
        let report = validate_journal(&shuffled);
        assert_eq!(
            report.exit_code(),
            0,
            "the chain's own links define the order"
        );
    }

    #[test]
    fn two_operations_validate_as_independent_segments() {
        let op_a = operation("op-seg-a");
        let op_b = operation("op-seg-b");
        let chain_a = live_chain(&[configure_envelope(&op_a), agent_start_envelope(&op_a)]);
        let chain_b = live_chain(&[configure_envelope(&op_b), agent_start_envelope(&op_b)]);
        // Interleaved and sharing input ids — idempotency is namespaced per operation.
        let mut mixed = Vec::new();
        for index in 0..2 {
            mixed.push(chain_a[index].record_bytes().into_vec());
            mixed.push(chain_b[index].record_bytes().into_vec());
        }
        let report = validate_journal(&mixed);
        assert_eq!(report.segments.len(), 2);
        assert_eq!(report.exit_code(), 0);
    }

    // -----------------------------------------------------------------------------------------
    // C1 · chain integrity
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_gap_in_the_chain_fails_c1_and_degrades_c3() {
        let op = operation("op-gapped");
        let chain = live_chain(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let gapped = blobs(&[chain[0].clone(), chain[2].clone()]);
        let report = validate_journal(&gapped);
        assert_eq!(rule(&report, 0, "C1").verdict, Verdict::Fail);
        assert_eq!(
            rule(&report, 0, "C3").verdict,
            Verdict::Degraded,
            "a re-plan over a broken chain would only re-report the C1 break"
        );
        assert_eq!(report.exit_code(), 1);
    }

    // -----------------------------------------------------------------------------------------
    // C2 · input idempotency
    // -----------------------------------------------------------------------------------------

    #[test]
    fn two_different_records_for_one_input_fail_c2() {
        let op = operation("op-dup-input");
        // Two chains over the same operation id whose `in-start` envelopes differ only in the
        // observed clock — same input id, different canonical input, different records.
        let chain_a = hand_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let mut later_start = agent_start_envelope(&op);
        later_start.observed_at_ms = WireU64::new(1_700_000_001_500);
        let chain_b = hand_chain(&[configure_envelope(&op), later_start]);
        assert_ne!(
            chain_a[1].record_digest(),
            chain_b[1].record_digest(),
            "the fixture must produce two different records for one input id"
        );
        let report = validate_journal(&blobs(&[
            chain_a[0].clone(),
            chain_a[1].clone(),
            chain_b[1].clone(),
        ]));
        assert_eq!(rule(&report, 0, "C2").verdict, Verdict::Fail);
        assert!(
            rule(&report, 0, "C2").detail.contains("in-start"),
            "{}",
            rule(&report, 0, "C2").detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    // -----------------------------------------------------------------------------------------
    // C4 · task lineage
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_repeated_attempt_pair_is_a_reused_launch_token() {
        let op = operation("op-dup-launch");
        let chain = hand_chain(&[
            configure_envelope(&op),
            resolve_spawn_envelope(
                &op,
                "in-ack-1",
                1_700_000_001_000,
                "op-dup-launch:step:0:effect:0",
                &[("writer", "writer:attempt:1")],
            ),
            resolve_spawn_envelope(
                &op,
                "in-ack-2",
                1_700_000_002_000,
                "op-dup-launch:step:0:effect:0",
                &[("writer", "writer:attempt:1")],
            ),
        ]);
        let report = validate_journal(&blobs(&chain));
        assert_eq!(rule(&report, 0, "C1").verdict, Verdict::Pass);
        assert_eq!(rule(&report, 0, "C4").verdict, Verdict::Fail);
        assert!(
            rule(&report, 0, "C4").detail.contains("LaunchToken"),
            "the verdict names the token reuse: {}",
            rule(&report, 0, "C4").detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_resolution_naming_a_future_step_fails_c4() {
        let op = operation("op-future-effect");
        let chain = hand_chain(&[
            configure_envelope(&op),
            resolve_spawn_envelope(
                &op,
                "in-ack-1",
                1_700_000_001_000,
                "op-future-effect:step:5:effect:0",
                &[("writer", "writer:attempt:1")],
            ),
        ]);
        let report = validate_journal(&blobs(&chain));
        assert_eq!(rule(&report, 0, "C4").verdict, Verdict::Fail);
        assert!(
            rule(&report, 0, "C4")
                .detail
                .contains("precedes the publication"),
            "{}",
            rule(&report, 0, "C4").detail
        );
    }

    #[test]
    fn a_resolution_naming_another_operation_fails_c4() {
        let op = operation("op-foreign-effect");
        let chain = hand_chain(&[
            configure_envelope(&op),
            resolve_spawn_envelope(
                &op,
                "in-ack-1",
                1_700_000_001_000,
                "op-somewhere-else:step:0:effect:0",
                &[("writer", "writer:attempt:1")],
            ),
        ]);
        let report = validate_journal(&blobs(&chain));
        assert_eq!(rule(&report, 0, "C4").verdict, Verdict::Fail);
        assert!(
            rule(&report, 0, "C4").detail.contains("another operation"),
            "{}",
            rule(&report, 0, "C4").detail
        );
    }

    // -----------------------------------------------------------------------------------------
    // C7 · degradation
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_tampered_hop_fails_integrity_validation() {
        let op = operation("op-tampered");
        let chain = live_chain(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let mut input = blobs(&chain);
        // Corrupt the middle record's step_digest: the strict decode now fails the self-digest
        // check. Surviving identity fields must not hide proven corruption.
        let mut forged: serde_json::Value = serde_json::from_slice(&input[1]).unwrap();
        forged["step_digest"] = serde_json::Value::String(chain[0].record_digest().to_string());
        input[1] = serde_json::to_vec(&forged).unwrap();

        let report = validate_journal(&input);
        assert_eq!(report.segments.len(), 1);
        assert_eq!(report.segments[0].degraded_hops.len(), 1);
        assert_eq!(
            rule(&report, 0, "C1").verdict,
            Verdict::Fail,
            "a digest mismatch must fail C1: {}",
            rule(&report, 0, "C1").detail
        );
        assert_eq!(rule(&report, 0, "C3").verdict, Verdict::Degraded);
        assert_eq!(rule(&report, 0, "C4").verdict, Verdict::Degraded);
        assert_eq!(
            report.exit_code(),
            1,
            "proven digest corruption must fail the validator"
        );
    }

    #[test]
    fn missing_legacy_digest_degrades_without_claiming_corruption() {
        let op = operation("op-legacy");
        let chain = live_chain(&[configure_envelope(&op)]);
        let mut legacy: serde_json::Value = serde_json::from_slice(&blobs(&chain)[0]).unwrap();
        legacy.as_object_mut().unwrap().remove("step_digest");
        let report = validate_journal(&[serde_json::to_vec(&legacy).unwrap()]);
        assert_eq!(rule(&report, 0, "C1").verdict, Verdict::Degraded);
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn unparseable_input_is_evidence_insufficient_not_guilty() {
        let report = validate_journal(&[b"this is not a record".to_vec()]);
        assert!(report.segments.is_empty());
        assert_eq!(report.unparseable_records, 1);
        assert_eq!(report.exit_code(), 2);
    }

    #[test]
    fn garbage_beside_a_green_chain_stays_exit_2_without_a_violation() {
        let op = operation("op-plus-garbage");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let mut input = blobs(&chain);
        input.push(b"this is not a record".to_vec());
        let report = validate_journal(&input);
        assert_eq!(report.segments.len(), 1);
        assert_eq!(rule(&report, 0, "C1").verdict, Verdict::Pass);
        assert_eq!(report.unparseable_records, 1);
        assert_eq!(
            report.exit_code(),
            2,
            "no violation was proven, but the evidence was partially unreadable"
        );
    }

    #[test]
    fn an_empty_journal_is_evidence_insufficient() {
        let report = validate_journal::<Vec<u8>>(&[]);
        assert_eq!(report.exit_code(), 2);
    }

    // -----------------------------------------------------------------------------------------
    // batch 3 · SessionLog input plane (C6/C8 land in S4b/S4c)
    // -----------------------------------------------------------------------------------------

    fn session_event(value: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn session_events_classify_leniently_across_host_spellings() {
        let node_attempt = session_event(json!({
            "kind": "provider_attempt",
            "effect_id": "op:step:1:effect:0",
            "request_fingerprint": "fp-1",
            "route": { "routeId": "route-a", "provider": "p" },
            "status": "success"
        }));
        let py_attempt = session_event(json!({
            "kind": "provider_attempt",
            "effect_id": "op:step:2:effect:0",
            "route": { "route_id": "route-b" }
        }));
        let run_started = session_event(json!({
            "kind": "run_started",
            "run_id": "run-1",
            "route": { "routeId": "route-a" }
        }));
        let node_measured = session_event(json!({
            "kind": "prompt_measured",
            "turn": 1,
            "effect_id": "op:step:1:effect:0",
            "measurement": { "requestFingerprint": "fp-1", "inputTokens": 10 }
        }));
        let py_measured = session_event(json!({
            "kind": "prompt_measured",
            "measurement": { "request_fingerprint": "fp-2" }
        }));
        let llm_completed = session_event(json!({
            "kind": "llm_completed",
            "effect_id": "op:step:2:effect:0",
            "invocation_id": "op:step:1:effect:0"
        }));
        let unknown_kind = session_event(json!({ "kind": "compressed", "turn": 3 }));
        let kindless = session_event(json!({ "turn": 3 }));

        assert_eq!(
            classify_session_event(&node_attempt),
            Some(EvidenceEvent::ProviderAttempt {
                effect_id: Some("op:step:1:effect:0".to_string()),
                request_fingerprint: Some("fp-1".to_string()),
                route_id: Some("route-a".to_string()),
                status: Some("success".to_string()),
            })
        );
        assert_eq!(
            classify_session_event(&py_attempt),
            Some(EvidenceEvent::ProviderAttempt {
                effect_id: Some("op:step:2:effect:0".to_string()),
                request_fingerprint: None,
                route_id: Some("route-b".to_string()),
                status: None,
            })
        );
        assert_eq!(
            classify_session_event(&run_started),
            Some(EvidenceEvent::RunStarted {
                route_id: Some("route-a".to_string())
            })
        );
        assert_eq!(
            classify_session_event(&node_measured),
            Some(EvidenceEvent::PromptMeasured {
                effect_id: Some("op:step:1:effect:0".to_string()),
                request_fingerprint: Some("fp-1".to_string()),
            })
        );
        assert_eq!(
            classify_session_event(&py_measured),
            Some(EvidenceEvent::PromptMeasured {
                effect_id: None,
                request_fingerprint: Some("fp-2".to_string()),
            })
        );
        assert_eq!(
            classify_session_event(&llm_completed),
            Some(EvidenceEvent::LlmCompleted {
                effect_id: Some("op:step:2:effect:0".to_string()),
                invocation_id: Some("op:step:1:effect:0".to_string()),
            })
        );
        assert_eq!(
            classify_session_event(&unknown_kind),
            Some(EvidenceEvent::Other),
            "unknown kinds are parseable but ignored — the vocabulary evolves"
        );
        assert_eq!(
            classify_session_event(&kindless),
            Some(EvidenceEvent::Other)
        );
        assert_eq!(
            classify_session_event(b"not json"),
            None,
            "a non-object event blob is unparseable input, never a violation"
        );
    }

    #[test]
    fn dual_input_with_a_green_journal_and_real_events_stays_green() {
        let op = operation("op-dual-green");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let stream = vec![
            session_event(json!({
                "kind": "run_started",
                "run_id": "r1",
                "route": { "routeId": "route-a" }
            })),
            session_event(json!({
                "kind": "prompt_measured",
                "turn": 1,
                "effect_id": "op-dual-green:step:1:effect:0",
                "measurement": { "requestFingerprint": "fp-1", "inputTokens": 10 }
            })),
            session_event(json!({
                "kind": "provider_attempt",
                "effect_id": "op-dual-green:step:1:effect:0",
                "request_fingerprint": "fp-1",
                "route": { "routeId": "route-a" },
                "status": "success"
            })),
            session_event(json!({
                "kind": "llm_completed",
                "turn": 1,
                "effect_id": "op-dual-green:step:1:effect:0",
                "invocation_id": "op-dual-green:step:1:effect:0"
            })),
        ];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(report.session_events, Some(4));
        assert_eq!(report.unparseable_events, 0);
        for id in ["C6.1", "C6.2", "C6.3", "C8"] {
            assert_eq!(
                cross(&report, id).verdict,
                Verdict::Pass,
                "{id}: {}",
                cross(&report, id).detail
            );
        }
        assert!(
            !report
                .deferred
                .iter()
                .any(|line| line.starts_with("c6.") || line.starts_with("c8.")),
            "C6/C8 are implemented — the interim scope notes are gone"
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn journal_only_validation_carries_no_session_plane() {
        let op = operation("op-journal-only");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let report = validate_journal(&blobs(&chain));
        assert_eq!(report.session_events, None);
        assert_eq!(report.unparseable_events, 0);
        assert_eq!(
            report.deferred.len(),
            2,
            "batch-1 deferred scope is unchanged"
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn an_empty_session_plane_is_evidence_insufficient() {
        let op = operation("op-empty-session");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let report = validate_with_session_log(&blobs(&chain), &[Vec::<Vec<u8>>::new()]);
        assert_eq!(report.session_events, Some(0));
        assert!(
            !report.has_violations(),
            "an empty log proves nothing either way"
        );
        assert_eq!(report.exit_code(), 2);
    }

    #[test]
    fn garbage_session_events_are_evidence_insufficient_not_guilty() {
        let op = operation("op-garbage-session");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let stream = vec![
            session_event(json!({ "kind": "run_started", "run_id": "r1" })),
            b"this is not an event".to_vec(),
        ];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(report.session_events, Some(1));
        assert_eq!(report.unparseable_events, 1);
        assert!(!report.has_violations());
        assert_eq!(report.exit_code(), 2);
    }

    // -----------------------------------------------------------------------------------------
    // C6 · SessionLog↔journal cross-verification
    // -----------------------------------------------------------------------------------------

    /// A green chain whose step-1 call_provider effect was resolved, plus a matching honest
    /// session stream: run pinned to route-a, the measurement, the attempt.
    fn honest_dual_input(op_name: &str) -> (Vec<KernelRecord>, Vec<Vec<u8>>) {
        let op = operation(op_name);
        let chain = live_chain(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let effect = format!("{op_name}:step:1:effect:0");
        let stream = vec![
            session_event(json!({
                "kind": "run_started",
                "run_id": "r1",
                "route": { "routeId": "route-a" }
            })),
            session_event(json!({
                "kind": "prompt_measured",
                "turn": 1,
                "effect_id": effect,
                "measurement": { "requestFingerprint": "fp-1", "inputTokens": 10 }
            })),
            session_event(json!({
                "kind": "provider_attempt",
                "effect_id": effect,
                "request_fingerprint": "fp-1",
                "route": { "routeId": "route-a" },
                "status": "success"
            })),
        ];
        (chain, stream)
    }

    #[test]
    fn an_attempt_naming_an_effect_the_replan_never_published_fails_c6() {
        let (chain, mut stream) = honest_dual_input("op-forged-effect");
        stream[2] = session_event(json!({
            "kind": "provider_attempt",
            "effect_id": "op-forged-effect:step:1:effect:7",
            "request_fingerprint": "fp-1",
            "route": { "routeId": "route-a" },
            "status": "success"
        }));
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C6.1").verdict, Verdict::Fail);
        assert!(
            cross(&report, "C6.1").detail.contains("never published"),
            "{}",
            cross(&report, "C6.1").detail
        );
        assert_eq!(
            report.exit_code(),
            1,
            "the forged attempt turns the run red"
        );
    }

    #[test]
    fn an_attempt_without_an_effect_id_fails_c6_as_forged_evidence() {
        let (chain, mut stream) = honest_dual_input("op-keyless-attempt");
        stream[2] = session_event(json!({
            "kind": "provider_attempt",
            "request_fingerprint": "fp-1",
            "route": { "routeId": "route-a" },
            "status": "success"
        }));
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C6.1").verdict, Verdict::Fail);
        assert!(
            cross(&report, "C6.1").detail.contains("without effect_id"),
            "{}",
            cross(&report, "C6.1").detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn an_attempt_past_the_journal_tip_degrades_c6_instead_of_failing() {
        let (chain, mut stream) = honest_dual_input("op-prefix-attempt");
        stream[2] = session_event(json!({
            "kind": "provider_attempt",
            "effect_id": "op-prefix-attempt:step:9:effect:0",
            "request_fingerprint": "fp-1",
            "route": { "routeId": "route-a" },
            "status": "success"
        }));
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(
            cross(&report, "C6.1").verdict,
            Verdict::Degraded,
            "a journal prefix cannot disprove an effect past its tip: {}",
            cross(&report, "C6.1").detail
        );
        assert_eq!(report.exit_code(), 0, "degradation never turns the run red");
    }

    #[test]
    fn an_attempt_on_an_operation_without_a_segment_degrades_c6() {
        let (chain, mut stream) = honest_dual_input("op-subset-journal");
        stream[2] = session_event(json!({
            "kind": "provider_attempt",
            "effect_id": "op-elsewhere:step:1:effect:0",
            "request_fingerprint": "fp-1",
            "route": { "routeId": "route-a" },
            "status": "success"
        }));
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C6.1").verdict, Verdict::Degraded);
        assert!(
            cross(&report, "C6.1").detail.contains("no journal segment"),
            "{}",
            cross(&report, "C6.1").detail
        );
    }

    #[test]
    fn an_orphan_fingerprint_fails_c6() {
        let (chain, mut stream) = honest_dual_input("op-orphan-fp");
        stream.remove(1); // drop the prompt_measured — the attempt's fingerprint is orphaned
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C6.2").verdict, Verdict::Fail);
        assert!(
            cross(&report, "C6.2").detail.contains("fp-1"),
            "{}",
            cross(&report, "C6.2").detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn an_attempt_without_a_fingerprint_fails_c6() {
        let (chain, mut stream) = honest_dual_input("op-fpless-attempt");
        stream[2] = session_event(json!({
            "kind": "provider_attempt",
            "effect_id": "op-fpless-attempt:step:1:effect:0",
            "route": { "routeId": "route-a" },
            "status": "success"
        }));
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C6.2").verdict, Verdict::Fail);
        assert!(
            cross(&report, "C6.2")
                .detail
                .contains("without request_fingerprint"),
            "{}",
            cross(&report, "C6.2").detail
        );
    }

    #[test]
    fn an_in_run_route_change_fails_c6() {
        let (chain, mut stream) = honest_dual_input("op-route-flip");
        stream[2] = session_event(json!({
            "kind": "provider_attempt",
            "effect_id": "op-route-flip:step:1:effect:0",
            "request_fingerprint": "fp-1",
            "route": { "routeId": "route-b" },
            "status": "success"
        }));
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C6.3").verdict, Verdict::Fail);
        assert!(
            cross(&report, "C6.3")
                .detail
                .contains("in-run route change"),
            "{}",
            cross(&report, "C6.3").detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_cross_resume_route_change_degrades_c6_per_q3() {
        let (chain, mut stream) = honest_dual_input("op-route-resume");
        // A new run_started pins route-b; its attempt follows honestly. The route CHANGE
        // across the resume is degraded-marked (adapter upgrades are legal), never failed.
        stream.push(session_event(json!({
            "kind": "run_started",
            "run_id": "r1",
            "route": { "routeId": "route-b" }
        })));
        stream.push(session_event(json!({
            "kind": "prompt_measured",
            "turn": 2,
            "effect_id": "op-route-resume:step:1:effect:0",
            "measurement": { "requestFingerprint": "fp-2", "inputTokens": 11 }
        })));
        stream.push(session_event(json!({
            "kind": "provider_attempt",
            "effect_id": "op-route-resume:step:1:effect:0",
            "request_fingerprint": "fp-2",
            "route": { "routeId": "route-b" },
            "status": "success"
        })));
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(
            cross(&report, "C6.3").verdict,
            Verdict::Degraded,
            "cross-resume route changes mark, they do not fail: {}",
            cross(&report, "C6.3").detail
        );
        assert!(
            cross(&report, "C6.3").detail.contains("cross-resume"),
            "{}",
            cross(&report, "C6.3").detail
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_pre_0_2_63_log_without_attempts_degrades_every_c6_clause() {
        let op = operation("op-old-log");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let stream = vec![
            session_event(json!({ "kind": "run_started", "run_id": "r1" })),
            session_event(json!({ "kind": "llm_completed", "turn": 1, "content": "done" })),
        ];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        for id in ["C6.1", "C6.2", "C6.3", "C8"] {
            assert_eq!(
                cross(&report, id).verdict,
                Verdict::Degraded,
                "{id}: old logs degrade (C7), never fail — {}",
                cross(&report, id).detail
            );
        }
        assert_eq!(report.exit_code(), 0);
    }

    // -----------------------------------------------------------------------------------------
    // C8 · invocation chain adjacency
    // -----------------------------------------------------------------------------------------

    /// A live chain whose first provider call overflows and whose retry completes:
    /// step 1 publishes `step:1:effect:0` (overflowed), its resolution's step publishes the
    /// retry `step:2:effect:0` (completed). The honest llm_completed for this invocation is
    /// `invocation_id = step:1:effect:0`, `effect_id = step:2:effect:0`. The operation starts
    /// with history so the compaction ladder can actually recover from the overflow.
    fn overflow_retry_chain(op_name: &str) -> Vec<KernelRecord> {
        let op = operation(op_name);
        live_chain(&[
            configure_envelope(&op),
            agent_start_with_history_envelope(&op, 14),
            resolve_overflow_envelope(&op, 1),
            resolve_completed_envelope(&op, "in-resolve-2", 1_700_000_003_000, 2, false),
        ])
    }

    #[test]
    fn an_honest_overflow_retry_invocation_passes_c8() {
        let chain = overflow_retry_chain("op-c8-green");
        let stream = vec![
            session_event(json!({ "kind": "run_started", "run_id": "r1" })),
            session_event(json!({
                "kind": "llm_completed",
                "turn": 1,
                "effect_id": "op-c8-green:step:2:effect:0",
                "invocation_id": "op-c8-green:step:1:effect:0"
            })),
        ];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(
            cross(&report, "C8").verdict,
            Verdict::Pass,
            "{}",
            cross(&report, "C8").detail
        );
        assert!(
            cross(&report, "C8")
                .detail
                .contains("1 retried invocation(s)"),
            "{}",
            cross(&report, "C8").detail
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_completed_chain_head_is_the_merge_forgery() {
        // The provider call completes with a tool call, so step 2 publishes an ExecuteTools
        // effect. Claiming invocation step:1:effect:0 → step:2:effect:0 merges the tool
        // execution into the provider invocation — the head COMPLETED, so nothing chains.
        let op = operation("op-c8-merged");
        let chain = live_chain(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_completed_envelope(&op, "in-resolve-1", 1_700_000_002_000, 1, true),
        ]);
        let stream = vec![session_event(json!({
            "kind": "llm_completed",
            "turn": 1,
            "effect_id": "op-c8-merged:step:2:effect:0",
            "invocation_id": "op-c8-merged:step:1:effect:0"
        }))];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C8").verdict, Verdict::Fail);
        assert!(
            cross(&report, "C8")
                .detail
                .contains("closes its invocation"),
            "{}",
            cross(&report, "C8").detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_selected_effect_preceding_the_chain_head_fails_c8() {
        let chain = overflow_retry_chain("op-c8-backwards");
        let stream = vec![session_event(json!({
            "kind": "llm_completed",
            "turn": 1,
            "effect_id": "op-c8-backwards:step:1:effect:0",
            "invocation_id": "op-c8-backwards:step:2:effect:0"
        }))];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C8").verdict, Verdict::Fail);
        assert!(
            cross(&report, "C8")
                .detail
                .contains("does not follow the chain head"),
            "{}",
            cross(&report, "C8").detail
        );
    }

    #[test]
    fn a_selected_effect_the_replan_never_published_fails_c8() {
        let chain = overflow_retry_chain("op-c8-phantom");
        let stream = vec![session_event(json!({
            "kind": "llm_completed",
            "turn": 1,
            "effect_id": "op-c8-phantom:step:2:effect:9",
            "invocation_id": "op-c8-phantom:step:1:effect:0"
        }))];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(cross(&report, "C8").verdict, Verdict::Fail);
        assert!(
            cross(&report, "C8").detail.contains("never published"),
            "{}",
            cross(&report, "C8").detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_first_try_invocation_has_no_adjacency_to_prove() {
        let op = operation("op-c8-first-try");
        let chain = live_chain(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_completed_envelope(&op, "in-resolve-1", 1_700_000_002_000, 1, false),
        ]);
        let stream = vec![session_event(json!({
            "kind": "llm_completed",
            "turn": 1,
            "effect_id": "op-c8-first-try:step:1:effect:0",
            "invocation_id": "op-c8-first-try:step:1:effect:0"
        }))];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(
            cross(&report, "C8").verdict,
            Verdict::Pass,
            "{}",
            cross(&report, "C8").detail
        );
        assert!(
            cross(&report, "C8").detail.contains("1 first-try"),
            "{}",
            cross(&report, "C8").detail
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn an_invocation_past_the_journal_tip_degrades_c8() {
        // The journal is a prefix cut before the overflow resolution lands; the SessionLog
        // already tells the whole story. Plane lag degrades, never fails.
        let op = operation("op-c8-lag");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let stream = vec![session_event(json!({
            "kind": "llm_completed",
            "turn": 1,
            "effect_id": "op-c8-lag:step:2:effect:0",
            "invocation_id": "op-c8-lag:step:1:effect:0"
        }))];
        let report = validate_with_session_log(&blobs(&chain), &[stream]);
        assert_eq!(
            cross(&report, "C8").verdict,
            Verdict::Degraded,
            "{}",
            cross(&report, "C8").detail
        );
        assert_eq!(report.exit_code(), 0);
    }

    // -----------------------------------------------------------------------------------------
    // batch 2 · C5 — the checkpoint evidence plane
    // -----------------------------------------------------------------------------------------

    use crate::runtime::kernel::wire::checkpoint::{
        CheckpointDraft, KernelCheckpoint, LaunchTokenState,
    };
    use crate::runtime::kernel::wire::driver::PlannedStep;
    use crate::runtime::kernel::wire::transaction::CheckpointBoundary;
    use std::path::PathBuf;

    /// A live run whose runtime stays alive, so a test can take checkpoint candidates the way
    /// the kernel does — and keep driving the same runtime afterwards.
    fn live_runtime(
        envelopes: &[WireEnvelope],
    ) -> (
        Vec<KernelRecord>,
        KernelTransaction<PlannedStep, InMemoryRecordIndex>,
        CanonicalOperationDriver,
    ) {
        let mut tx = KernelTransaction::new(ConfigDefaults::default(), InMemoryRecordIndex::new());
        let mut driver = CanonicalOperationDriver::new();
        let mut journal = Vec::new();
        for envelope in envelopes {
            let preparation = tx.prepare(envelope, |context| driver.plan(context));
            let token = preparation
                .token()
                .unwrap_or_else(|| {
                    panic!("expected a prepared step, got {:?}", preparation.fault())
                })
                .clone();
            let head = preparation.record().unwrap().record_digest().clone();
            let committed = tx.commit(&token, &head).expect("commit must succeed");
            journal.push(committed.record.clone());
            driver
                .note_committed(committed.step_seq)
                .expect("the driver folds the step it planned");
        }
        (journal, tx, driver)
    }

    fn checkpoint_at_head(
        tx: &KernelTransaction<PlannedStep, InMemoryRecordIndex>,
        driver: &CanonicalOperationDriver,
    ) -> KernelCheckpoint {
        tx.checkpoint_candidate(driver.project_logical_state())
            .expect("the head checkpoints")
            .decode()
            .expect("the candidate decodes")
    }

    fn checkpoint_blob(checkpoint: &KernelCheckpoint) -> Vec<u8> {
        checkpoint.checkpoint_bytes().into_vec()
    }

    fn checkpoint_check<'a>(report: &'a ValidationReport, id: &str) -> &'a RuleReport {
        report
            .checkpoint_checks
            .iter()
            .find(|rule| rule.rule == id)
            .unwrap_or_else(|| panic!("the report has no {id} checkpoint-check"))
    }

    fn no_streams() -> Vec<Vec<Vec<u8>>> {
        Vec::new()
    }

    #[test]
    fn without_a_checkpoint_plane_c5_is_deferred_not_red() {
        let op = operation("op-c5-deferred");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let report = validate_journal(&blobs(&chain));
        assert!(report.checkpoint_checks.is_empty());
        assert_eq!(report.checkpoints, None);
        assert_eq!(report.unparseable_checkpoints, 0);
        assert_eq!(
            report.deferred.len(),
            2,
            "the checkpoint plane was never offered"
        );
        assert!(
            report.deferred[1].contains("c5b.launch_token_ledger"),
            "{}",
            report.deferred[1]
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_real_checkpoint_anchors_its_journal() {
        let op = operation("op-c5-green");
        let (chain, tx, driver) = live_runtime(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let checkpoint = checkpoint_at_head(&tx, &driver);
        let report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[checkpoint_blob(&checkpoint)],
            false,
        );
        assert_eq!(report.checkpoints, Some(1));
        assert_eq!(report.unparseable_checkpoints, 0);
        for id in ["C5a", "C5b"] {
            assert_eq!(
                checkpoint_check(&report, id).verdict,
                Verdict::Pass,
                "{id}: {}",
                checkpoint_check(&report, id).detail
            );
        }
        assert!(
            checkpoint_check(&report, "C5a")
                .detail
                .contains("covered head anchored at step"),
            "{}",
            checkpoint_check(&report, "C5a").detail
        );
        assert_eq!(
            report.deferred.len(),
            1,
            "the checkpoint plane retires the c5b deferral"
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn strict_replay_reproduces_the_covered_state_and_the_ladder_holds() {
        let op = operation("op-c5-strict");
        let (chain, tx, driver) = live_runtime(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let checkpoint = checkpoint_at_head(&tx, &driver);
        let report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[checkpoint_blob(&checkpoint)],
            true,
        );
        let c5a = checkpoint_check(&report, "C5a");
        assert_eq!(c5a.verdict, Verdict::Pass, "{}", c5a.detail);
        assert!(
            c5a.detail
                .contains("strict replay reproduces the captured state digest"),
            "{}",
            c5a.detail
        );
        assert!(
            c5a.detail.contains("restore ladder holds"),
            "{}",
            c5a.detail
        );
        let c5b = checkpoint_check(&report, "C5b");
        assert_eq!(c5b.verdict, Verdict::Pass, "{}", c5b.detail);
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn strict_replay_with_tail_records_drives_the_whole_ladder() {
        let op = operation("op-c5-strict-tail");
        let (mut chain, mut tx, mut driver) =
            live_runtime(&[configure_envelope(&op), agent_start_envelope(&op)]);
        // The checkpoint is taken at step 1; the record that lands afterwards is the tail the
        // ladder has to replay.
        let checkpoint = checkpoint_at_head(&tx, &driver);
        let preparation = tx.prepare(&resolve_overflow_envelope(&op, 1), |context| {
            driver.plan(context)
        });
        let token = preparation.token().expect("the tail step prepares").clone();
        let head = preparation.record().unwrap().record_digest().clone();
        let committed = tx.commit(&token, &head).expect("the tail step commits");
        chain.push(committed.record.clone());
        driver
            .note_committed(committed.step_seq)
            .expect("the driver folds the tail step");

        let report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[checkpoint_blob(&checkpoint)],
            true,
        );
        let c5a = checkpoint_check(&report, "C5a");
        assert_eq!(c5a.verdict, Verdict::Pass, "{}", c5a.detail);
        assert!(
            c5a.detail.contains("against the 1 journal record(s) above"),
            "{}",
            c5a.detail
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_bounded_tail_checkpoint_reconciles_with_the_journal() {
        let op = operation("op-c5-bounded");
        let (mut chain, mut tx, mut driver) =
            live_runtime(&[configure_envelope(&op), agent_start_envelope(&op)]);
        // Base the window at step 1, then let the run grow past it and rebase: the checkpoint
        // then covers (1, 2] as a bounded tail instead of a full state.
        let candidate = tx
            .checkpoint_candidate(driver.project_logical_state())
            .expect("the base checkpoints");
        let boundary: CheckpointBoundary = candidate.boundary();
        let base_state = candidate
            .decode()
            .expect("the base decodes")
            .logical_state()
            .clone();
        let preparation = tx.prepare(&resolve_overflow_envelope(&op, 1), |context| {
            driver.plan(context)
        });
        let token = preparation.token().expect("the tail step prepares").clone();
        let head = preparation.record().unwrap().record_digest().clone();
        let committed = tx.commit(&token, &head).expect("the tail step commits");
        chain.push(committed.record.clone());
        driver
            .note_committed(committed.step_seq)
            .expect("the driver folds the tail step");

        let rebased = tx
            .checkpoint_rebase(&boundary, base_state)
            .expect("the window rebases")
            .decode()
            .expect("the rebase decodes");
        assert_eq!(rebased.base_step_seq().get(), 1);
        assert_eq!(rebased.through_step_seq().get(), 2);
        assert_eq!(rebased.tail_inputs().len(), 1);

        // Strict, to prove the re-plan replay agrees with a windowed checkpoint too: the
        // windowed checkpoint captures its state at the base step, so the fold lands there,
        // and the ladder replays the tail onto it.
        let report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[checkpoint_blob(&rebased)],
            true,
        );
        let c5a = checkpoint_check(&report, "C5a");
        assert_eq!(c5a.verdict, Verdict::Pass, "{}", c5a.detail);
        assert!(
            c5a.detail.contains("1 bounded-tail entries reconcile"),
            "{}",
            c5a.detail
        );
        assert!(
            c5a.detail.contains("the captured state digest at step 1"),
            "{}",
            c5a.detail
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_checkpoint_from_another_chain_fails_c5a() {
        let op = operation("op-c5-foreign");
        // Same operation id, different genesis: the journal's configure froze max_turns 12,
        // the checkpoint's chain froze 24. Identity digests are the only witness.
        let (chain, _tx, _driver) =
            live_runtime(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let foreign_config = |max_turns: u32| {
            envelope(
                &op,
                "in-configure",
                1_700_000_000_000,
                KernelInput::ConfigureOperation(ConfigureOperation {
                    config: OperationConfig {
                        execution_policy: Some(ExecutionPolicy {
                            max_turns: Some(max_turns),
                            ..ExecutionPolicy::default()
                        }),
                        host_effect_support: HostEffectSupport::new([
                            EffectKindTag::CallProvider,
                            EffectKindTag::SpawnTasks,
                        ]),
                        ..OperationConfig::default()
                    },
                }),
            )
        };
        let (_chain_other, other_tx, other_driver) =
            live_runtime(&[foreign_config(24), agent_start_envelope(&op)]);
        let foreign = checkpoint_at_head(&other_tx, &other_driver);

        let report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[checkpoint_blob(&foreign)],
            false,
        );
        let c5a = checkpoint_check(&report, "C5a");
        assert_eq!(c5a.verdict, Verdict::Fail, "{}", c5a.detail);
        assert!(
            c5a.detail.contains("captured on another chain"),
            "{}",
            c5a.detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_reused_launch_token_fails_c5b() {
        let op = operation("op-c5-tokens");
        let (chain, tx, driver) = live_runtime(&[
            configure_envelope(&op),
            workflow_start_envelope(&op),
            resolve_spawn_envelope(
                &op,
                "in-ack-1",
                1_700_000_002_000,
                "op-c5-tokens:step:1:effect:0",
                &[("wf-node0", "wf-node0:attempt:1")],
            ),
        ]);
        let checkpoint = checkpoint_at_head(&tx, &driver);
        assert_eq!(
            checkpoint.logical_state().transition.launch_tokens.len(),
            1,
            "the workflow start minted one launch token"
        );

        // Forge the reuse honestly: re-assemble with the same token registered at a second
        // step. The digests are computed, so the blob decodes — the content is what lies.
        let mut state = checkpoint.logical_state().clone();
        let minted = state.transition.launch_tokens[0].clone();
        state.transition.launch_tokens.push(LaunchTokenState {
            launch_token: minted.launch_token,
            step_seq: WireU64::new(2),
        });
        let forged = KernelCheckpoint::assemble(CheckpointDraft {
            operation_id: checkpoint.operation_id().clone(),
            genesis_digest: checkpoint.genesis_digest().clone(),
            base_step_seq: checkpoint.base_step_seq(),
            base_record_digest: checkpoint.base_record_digest().clone(),
            through_step_seq: checkpoint.through_step_seq(),
            covered_transaction_head_digest: checkpoint.covered_transaction_head_digest().clone(),
            logical_state: state,
            tail_inputs: checkpoint.tail_inputs().to_vec(),
        })
        .expect("the forged draft assembles");

        let report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[checkpoint_blob(&forged)],
            false,
        );
        let c5b = checkpoint_check(&report, "C5b");
        assert_eq!(c5b.verdict, Verdict::Fail, "{}", c5b.detail);
        assert!(
            c5b.detail.contains("reuse across TaskLaunch payloads"),
            "{}",
            c5b.detail
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn strict_replay_catches_a_ledger_the_journal_never_minted() {
        let op = operation("op-c5-moved-mint");
        let (chain, tx, driver) = live_runtime(&[
            configure_envelope(&op),
            workflow_start_envelope(&op),
            resolve_spawn_envelope(
                &op,
                "in-ack-1",
                1_700_000_002_000,
                "op-c5-moved-mint:step:1:effect:0",
                &[("wf-node0", "wf-node0:attempt:1")],
            ),
        ]);
        let checkpoint = checkpoint_at_head(&tx, &driver);

        // Move the mint from step 1 to step 2. No duplicate, nothing beyond the boundary, no
        // pending effect to contradict — the default anchors cannot see the lie; the re-plan
        // is the only witness.
        let mut state = checkpoint.logical_state().clone();
        state.transition.launch_tokens[0].step_seq = WireU64::new(2);
        let forged = KernelCheckpoint::assemble(CheckpointDraft {
            operation_id: checkpoint.operation_id().clone(),
            genesis_digest: checkpoint.genesis_digest().clone(),
            base_step_seq: checkpoint.base_step_seq(),
            base_record_digest: checkpoint.base_record_digest().clone(),
            through_step_seq: checkpoint.through_step_seq(),
            covered_transaction_head_digest: checkpoint.covered_transaction_head_digest().clone(),
            logical_state: state,
            tail_inputs: checkpoint.tail_inputs().to_vec(),
        })
        .expect("the forged draft assembles");

        let default_report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[checkpoint_blob(&forged)],
            false,
        );
        assert_eq!(
            checkpoint_check(&default_report, "C5b").verdict,
            Verdict::Pass,
            "the default plane cannot see a moved mint: {}",
            checkpoint_check(&default_report, "C5b").detail
        );

        let strict_report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[checkpoint_blob(&forged)],
            true,
        );
        let c5b = checkpoint_check(&strict_report, "C5b");
        assert_eq!(c5b.verdict, Verdict::Fail, "{}", c5b.detail);
        assert!(
            c5b.detail.contains("different launch-token ledger"),
            "{}",
            c5b.detail
        );
        assert_eq!(
            checkpoint_check(&strict_report, "C5a").verdict,
            Verdict::Fail,
            "the ledger is part of the state, so the state digest moves too"
        );
        assert_eq!(strict_report.exit_code(), 1);
    }

    #[test]
    fn a_tail_disconnected_from_the_journal_fails_c5a() {
        let op = operation("op-c5-spliced");
        let (chain_a, tx, driver) = live_runtime(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let checkpoint = checkpoint_at_head(&tx, &driver);

        // A second run sharing the first two envelopes (deterministic, so byte-identical) but
        // resolving step 1 through a different input id. Its record chains cleanly onto a's
        // step 1 — C1 cannot see the splice; only the checkpoint's covered head can.
        let other_resolution = envelope(
            &op,
            "in-resolve-other",
            1_700_000_002_000,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: EffectId::new("op-c5-spliced:step:1:effect:0").unwrap(),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::Provider(ProviderSuccess {
                        outcome: ProviderOutcome::ContextOverflow(
                            ProviderContextOverflow::default(),
                        ),
                    }),
                }),
            }),
        );
        let (chain_b, _tx_b, _driver_b) = live_runtime(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            other_resolution,
        ]);
        let mut spliced = chain_a[..2].to_vec();
        spliced.push(chain_b[2].clone());

        let report = validate_with_checkpoint(
            &blobs(&spliced),
            &no_streams(),
            &[checkpoint_blob(&checkpoint)],
            false,
        );
        assert_eq!(
            rule(&report, 0, "C1").verdict,
            Verdict::Pass,
            "the splice chains cleanly — C1 is not the witness here"
        );
        let c5a = checkpoint_check(&report, "C5a");
        assert_eq!(c5a.verdict, Verdict::Fail, "{}", c5a.detail);
        assert!(c5a.detail.contains("covered step 2"), "{}", c5a.detail);
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_pruned_journal_degrades_the_c5_anchors_and_c1_keeps_its_verdict() {
        let op = operation("op-c5-pruned");
        let (chain, tx, driver) = live_runtime(&[
            configure_envelope(&op),
            agent_start_envelope(&op),
            resolve_overflow_envelope(&op, 1),
        ]);
        let checkpoint = checkpoint_at_head(&tx, &driver);
        // Retention reclaimed the prefix: the genesis and the start record are gone.
        let pruned = chain[2..].to_vec();

        let report = validate_with_checkpoint(
            &blobs(&pruned),
            &no_streams(),
            &[checkpoint_blob(&checkpoint)],
            true,
        );
        // C5 judges honestly: the identity anchor is unverifiable, the covered head anchors
        // fine, and the strict replay skips rather than folding a foreign history.
        let c5a = checkpoint_check(&report, "C5a");
        assert_eq!(c5a.verdict, Verdict::Degraded, "{}", c5a.detail);
        assert!(
            c5a.detail.contains("identity anchor is unverifiable"),
            "{}",
            c5a.detail
        );
        assert!(
            c5a.detail.contains("covered head anchored at step 2"),
            "{}",
            c5a.detail
        );
        assert!(
            c5a.detail.contains("strict replay skipped"),
            "{}",
            c5a.detail
        );
        // C1's batch-1 stance is unchanged by the checkpoint plane: a segment whose first
        // record is not genesis is a broken chain until a rule is taught about acked
        // reclamation — deliberately out of C5's scope (S1 adds C5a/C5b, nothing else).
        assert_eq!(rule(&report, 0, "C1").verdict, Verdict::Fail);
        assert!(report.has_violations());
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn an_unparseable_checkpoint_leaves_evidence_insufficient() {
        let op = operation("op-c5-junk");
        let chain = live_chain(&[configure_envelope(&op), agent_start_envelope(&op)]);
        let report = validate_with_checkpoint(
            &blobs(&chain),
            &no_streams(),
            &[b"{not a checkpoint".to_vec()],
            false,
        );
        assert_eq!(report.unparseable_checkpoints, 1);
        assert_eq!(report.checkpoints, Some(0));
        assert_eq!(
            checkpoint_check(&report, "C5a").verdict,
            Verdict::Degraded,
            "{}",
            checkpoint_check(&report, "C5a").detail
        );
        assert_eq!(report.exit_code(), 2);
    }

    #[test]
    fn the_published_golden_checkpoints_decode_through_the_checkpoint_plane() {
        let fixture_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kernel-wire");
        let mut blobs = Vec::new();
        for name in [
            "golden_checkpoint_agent_turn",
            "golden_checkpoint_bounded_tail",
        ] {
            let wrapper: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(fixture_dir.join(format!("{name}.json")))
                    .expect("fixture readable"),
            )
            .expect("fixture json");
            blobs.push(
                serde_json::to_vec(&wrapper["checkpoint"]).expect("the nested checkpoint writes"),
            );
        }
        // No journal segment names these operations, so C5a degrades; C5b is
        // checkpoint-internal and still runs — it must never fail on a real published
        // checkpoint.
        let report = validate_with_checkpoint(&[] as &[Vec<u8>], &no_streams(), &blobs, false);
        assert_eq!(report.checkpoints, Some(2));
        assert_eq!(report.unparseable_checkpoints, 0);
        let c5a = checkpoint_check(&report, "C5a");
        assert_eq!(c5a.verdict, Verdict::Degraded, "{}", c5a.detail);
        assert!(c5a.detail.contains("holds no segment"), "{}", c5a.detail);
        let c5b = checkpoint_check(&report, "C5b");
        assert_ne!(c5b.verdict, Verdict::Fail, "{}", c5b.detail);
        assert!(!report.has_violations());
    }
}
