//! `KernelJournal` — the durable transaction capability of the Canonical Kernel ABI (spec §9.1).
//!
//! The Rust port of `node/src/runtime/kernel-journal.ts`, which is the authoritative interface
//! shape for all four SDKs. Three rules give this module its shape:
//!
//! 1. **Records are opaque.** core owns canonical serialization and hashing
//!    (`KernelRecord::record_bytes()` / `record_digest()` / `expected_head()`). The host stores the
//!    bytes verbatim and indexes them by the digest core handed it. A journal that re-serialised a
//!    record to recompute its hash would make "the host recomputed and disagreed" a reachable
//!    state; it is not one here.
//! 2. **CAS is a storage-layer primitive, not a read-compare-write sequence.** §9.1 requires a real
//!    atomic operation (file lock / atomic link publish / conditional database update).
//!    [`InMemoryKernelJournal`] is atomic only within one process and says so in its own type;
//!    [`FileKernelJournal`] is atomic across processes (see its type docs).
//! 3. **The journal's sequence space is `step_seq`** — the operation's record-chain position — and
//!    is completely independent of [`super::session_log::SessionLog`]'s business event `seq`.
//!    Pruning a journal prefix can never punch a hole in business event numbering (spec Task 8b,
//!    criterion 4). That is why this is a separate trait rather than more methods on `SessionLog`
//!    (§9.4): a custom business-projection log must never be forced to masquerade as a
//!    transactional journal. One type may implement both.
//!
//! Failures are typed so a caller can tell "retry after rebuild" from "this storage is broken" from
//! "someone handed me a corrupt chain" — a durable-step wrapper must never publish effects on any
//! of them, and must never collapse them into one opaque error. See [`JournalError`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/* ------------------------------------------------------------------ *
 * Errors
 * ------------------------------------------------------------------ */

/// Why a journal operation failed, split into the three classes a caller must treat differently.
///
/// | variant | journal state | response |
/// | --- | --- | --- |
/// | [`JournalError::CasConflict`] | known: someone else won | retryable — `abort(token)` → re-read head → rebuild → replay the input (spec §8.3) |
/// | [`JournalError::Integrity`] | known: contradictory | never retryable — retrying replays the same contradiction |
/// | [`JournalError::Io`] | **unknown** | neither; the append may or may not have landed |
///
/// No variant permits publishing kernel effects (§9.1, "journal failure 不发布 kernel effects").
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// The CAS precondition did not hold: the journal head (or checkpoint pointer) moved.
    #[error("journal CAS conflict: {0}")]
    CasConflict(String),
    /// The journal contents contradict themselves or the caller's claim: a broken digest chain, a
    /// `step_seq` that does not follow its predecessor, a checkpoint whose `covered_head` does not
    /// match the record at its `through_step_seq`.
    #[error("journal integrity fault: {0}")]
    Integrity(String),
    /// The storage layer failed (disk full, permission denied, hard links unsupported). Distinct
    /// from both of the above: the journal state is *unknown*, not known-conflicting and not
    /// known-corrupt.
    #[error("{message}: {source}")]
    Io {
        message: String,
        #[source]
        source: std::io::Error,
    },
}

impl JournalError {
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::CasConflict(message.into())
    }

    pub fn integrity(message: impl Into<String>) -> Self {
        Self::Integrity(message.into())
    }

    pub fn io(message: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            message: message.into(),
            source,
        }
    }

    /// A CAS conflict is the only class the durable-step protocol may retry. `Integrity` is a
    /// permanent contradiction; `Io` leaves the journal in an unknown state, so a retry could
    /// double-append rather than resolve anything — the caller must re-read the head first.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::CasConflict(_))
    }
}

impl From<JournalError> for crate::Error {
    /// `Io` keeps its `ErrorKind` (so callers that already match on [`crate::Error::Io`] keep
    /// working); the two journal-semantic classes become [`crate::Error::Other`] carrying their
    /// full message — the SDK-level error has no variant that could preserve the distinction, and
    /// silently mapping a CAS conflict onto `Io` would tell a caller to retry storage instead of
    /// rebuilding.
    fn from(err: JournalError) -> Self {
        match err {
            JournalError::Io { message, source } => crate::Error::Io(std::io::Error::new(
                source.kind(),
                format!("{message}: {source}"),
            )),
            other => crate::Error::Other(other.to_string()),
        }
    }
}

pub type JournalResult<T> = std::result::Result<T, JournalError>;

/* ------------------------------------------------------------------ *
 * Record shapes
 * ------------------------------------------------------------------ */

/// What a host hands the journal: core's opaque bytes plus the identity core assigned them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecordInput {
    /// Chain position. Genesis (`ConfigureOperation`) is 0; every later record is `head + 1`.
    pub step_seq: u64,
    /// core's `record_digest`. The journal indexes by it and never recomputes it.
    pub record_digest: String,
    /// core's `record_bytes`, stored verbatim.
    pub record_bytes: Vec<u8>,
}

/// A stored record. `previous_record_digest` is the CAS precondition it was appended under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub step_seq: u64,
    pub record_digest: String,
    /// `None` on the genesis record only (spec §8.1: the first append's expected head is empty).
    pub previous_record_digest: Option<String>,
    pub record_bytes: Vec<u8>,
}

/// The journal head: the last record of an operation's chain.
///
/// Carries `step_seq` as well as the digest because CAS needs the chain position to validate that
/// the incoming record actually follows the head (spec Task 8b implementation correction (c)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalHead {
    pub step_seq: u64,
    pub record_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalAppendReceipt {
    pub step_seq: u64,
    pub record_digest: String,
}

/* ------------------------------------------------------------------ *
 * Checkpoint shapes
 * ------------------------------------------------------------------ */

/// The host-persistable part of `kernel.checkpoint_candidate()` (spec §12.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointCandidate {
    /// Stable identity of this checkpoint; the CAS token a later install names as its predecessor.
    pub checkpoint_id: String,
    /// The chain position this checkpoint's logical state covers.
    pub through_step_seq: u64,
    /// core's digest of the logical state. Opaque to the journal.
    pub state_digest: String,
    /// The serialized checkpoint blob, stored verbatim.
    pub checkpoint_bytes: Vec<u8>,
}

/// An installed checkpoint. Flattens node's `InstalledCheckpoint extends CheckpointCandidate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledCheckpoint {
    /// Monotonic install ordinal. `previous_checkpoint_id == None` installs ordinal 0.
    pub ordinal: u64,
    pub checkpoint_id: String,
    pub previous_checkpoint_id: Option<String>,
    /// The record digest at `through_step_seq` — verified at install, not required to still be head.
    pub covered_head: String,
    pub through_step_seq: u64,
    pub state_digest: String,
    pub checkpoint_bytes: Vec<u8>,
    /// Whether [`KernelJournal::ack_checkpoint`] has run. Prefix reclamation is gated on this: a
    /// checkpoint that is installed but not acknowledged is recoverable-from but not
    /// prune-authorising (spec §12.3 rules 5–6).
    pub acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalPruneReceipt {
    /// Highest `step_seq` no longer retained. `None` when nothing has ever been pruned — this is
    /// the Rust rendering of the `-1` sentinel the Node/Python receipts carry.
    pub pruned_through_step_seq: Option<u64>,
    pub pruned_count: u64,
}

/* ------------------------------------------------------------------ *
 * The capability
 * ------------------------------------------------------------------ */

/// The durable transaction capability (spec §9.1). Deliberately *not* part of
/// [`super::session_log::SessionLog`] (§9.4).
///
/// Guarantees an implementation owes (spec §9.1):
/// - strict ordering within an operation;
/// - a failed CAS never overwrites;
/// - record bytes preserved verbatim;
/// - checkpoint pointer advances monotonically;
/// - the checkpoint store verifies `covered_head` against `through_step_seq` but does **not**
///   require it to still be the current transaction head (§22.14);
/// - checkpoint pointer and prefix pruning have an explicit acknowledgement boundary.
#[async_trait]
pub trait KernelJournal: Send + Sync {
    /// Persist the byte-identical canonical input envelope before attempting its record append.
    ///
    /// A wake replays this value rather than constructing a fresh envelope, preserving the
    /// caller's idempotency key and observed clock across the append crash window.
    async fn stage_outbound_envelope(
        &self,
        operation_id: &str,
        envelope_json: &str,
    ) -> JournalResult<()>;

    /// Return the staged outbound envelope, if an append-before crash left one behind.
    async fn read_outbound_envelope(&self, operation_id: &str) -> JournalResult<Option<String>>;

    /// Clear a staged outbound envelope after its input is durably owned or rejected.
    async fn clear_outbound_envelope(&self, operation_id: &str) -> JournalResult<()>;

    /// Atomically append `record` iff the operation's head is exactly `expected_head`.
    ///
    /// `expected_head == None` starts the chain (genesis; `record.step_seq` must be 0).
    ///
    /// Errors: [`JournalError::CasConflict`] when the head moved or a genesis already exists;
    /// [`JournalError::Integrity`] when `step_seq` does not follow the head;
    /// [`JournalError::Io`] on storage failure.
    async fn compare_and_append(
        &self,
        operation_id: &str,
        expected_head: Option<&str>,
        record: JournalRecordInput,
    ) -> JournalResult<JournalAppendReceipt>;

    /// The current head, or `None` when the operation has no records and no pruned anchor.
    async fn head(&self, operation_id: &str) -> JournalResult<Option<JournalHead>>;

    /// Records with `step_seq >= from_step_seq`, in chain order. `0` returns the whole retained
    /// chain (Rust has no default arguments; node's optional parameter defaults to the same).
    async fn read_from(
        &self,
        operation_id: &str,
        from_step_seq: u64,
    ) -> JournalResult<Vec<JournalEntry>>;

    /// Records strictly after the record whose digest is `after_head` — the digest-anchored cursor
    /// §9.1 names `records_after(operation_id, checkpoint_head)`. `None` returns everything
    /// retained.
    ///
    /// Errors with [`JournalError::Integrity`] when `after_head` names no retained record and no
    /// pruned anchor.
    async fn records_after(
        &self,
        operation_id: &str,
        after_head: Option<&str>,
    ) -> JournalResult<Vec<JournalEntry>>;

    /// Atomically install `checkpoint` iff the operation's checkpoint pointer is exactly
    /// `previous_checkpoint_id`, and `covered_head` is the record digest at
    /// `checkpoint.through_step_seq`.
    ///
    /// Per §22.14 this deliberately does **not** require `covered_head` to still be the current
    /// transaction head: transactions appended after the candidate was taken stay as tail.
    ///
    /// Errors: [`JournalError::CasConflict`] when the checkpoint pointer moved;
    /// [`JournalError::Integrity`] when `covered_head`/`through_step_seq` disagree with the chain,
    /// or `through_step_seq` would move the pointer backwards.
    async fn compare_and_install_checkpoint(
        &self,
        operation_id: &str,
        previous_checkpoint_id: Option<&str>,
        covered_head: &str,
        checkpoint: CheckpointCandidate,
    ) -> JournalResult<InstalledCheckpoint>;

    /// The highest-ordinal installed checkpoint, acknowledged or not.
    async fn latest_checkpoint(
        &self,
        operation_id: &str,
    ) -> JournalResult<Option<InstalledCheckpoint>>;

    /// Record the durable acknowledgement that opens the prefix-reclamation boundary. Idempotent.
    ///
    /// Errors with [`JournalError::Integrity`] when no checkpoint with that id is installed.
    async fn ack_checkpoint(
        &self,
        operation_id: &str,
        checkpoint_id: &str,
    ) -> JournalResult<InstalledCheckpoint>;

    /// Reclaim the record prefix covered by the latest **acknowledged** checkpoint. A no-op while
    /// no checkpoint is acknowledged. The pruned boundary is retained as an anchor so [`Self::head`]
    /// and the next CAS still resolve on a fully-pruned chain (Task 8b correction (d)).
    async fn prune_acked_prefix(&self, operation_id: &str) -> JournalResult<JournalPruneReceipt>;
}

/* ------------------------------------------------------------------ *
 * Shared validation
 * ------------------------------------------------------------------ */

/// Zero-padded width of a chain position / install ordinal in a file name.
const SEQ_DIGITS: usize = 12;
/// One past the largest position the 12-digit name space can hold. A larger `step_seq` would pad
/// to 13 characters, no longer match the naming rule, and become an invisible record — so it is
/// refused up front, in shared validation, rather than silently orphaned by one implementation.
const MAX_CHAIN_POSITION: u64 = 1_000_000_000_000;

fn validate_record(record: &JournalRecordInput) -> JournalResult<()> {
    if record.step_seq >= MAX_CHAIN_POSITION {
        return Err(JournalError::integrity(format!(
            "journal record step_seq {} exceeds the {SEQ_DIGITS}-digit chain-position space",
            record.step_seq
        )));
    }
    if record.record_digest.is_empty() {
        return Err(JournalError::integrity(
            "journal record requires a record_digest",
        ));
    }
    Ok(())
}

fn validate_candidate(checkpoint: &CheckpointCandidate) -> JournalResult<()> {
    if checkpoint.checkpoint_id.is_empty() {
        return Err(JournalError::integrity(
            "checkpoint requires a checkpoint_id",
        ));
    }
    if checkpoint.through_step_seq >= MAX_CHAIN_POSITION {
        return Err(JournalError::integrity(format!(
            "checkpoint through_step_seq {} exceeds the {SEQ_DIGITS}-digit chain-position space",
            checkpoint.through_step_seq
        )));
    }
    if checkpoint.state_digest.is_empty() {
        return Err(JournalError::integrity(
            "checkpoint requires a state_digest",
        ));
    }
    Ok(())
}

/// Check the CAS precondition against the observed head, and the chain position that follows from
/// it. Shared by both implementations so their conflict/integrity taxonomy cannot drift apart.
fn check_append_precondition(
    head: Option<&JournalHead>,
    expected_head: Option<&str>,
    record: &JournalRecordInput,
) -> JournalResult<()> {
    let Some(expected_head) = expected_head else {
        if head.is_some() {
            return Err(JournalError::conflict(
                "journal genesis append requires an empty chain, but the operation already has a head",
            ));
        }
        if record.step_seq != 0 {
            return Err(JournalError::integrity(
                "journal genesis record must have step_seq 0",
            ));
        }
        return Ok(());
    };
    let Some(head) = head else {
        return Err(JournalError::conflict(
            "journal compare-and-append expected a head, but the chain is empty",
        ));
    };
    if head.record_digest != expected_head {
        return Err(JournalError::conflict(
            "journal head changed before compare-and-append",
        ));
    }
    if record.step_seq != head.step_seq + 1 {
        return Err(JournalError::integrity(format!(
            "journal record step_seq {} does not follow head step_seq {}",
            record.step_seq, head.step_seq
        )));
    }
    Ok(())
}

fn check_checkpoint_precondition(
    latest: Option<&InstalledCheckpoint>,
    previous_checkpoint_id: Option<&str>,
    checkpoint: &CheckpointCandidate,
) -> JournalResult<()> {
    let Some(previous_checkpoint_id) = previous_checkpoint_id else {
        if latest.is_some() {
            return Err(JournalError::conflict(
                "checkpoint install without a predecessor requires an empty checkpoint pointer",
            ));
        }
        return Ok(());
    };
    let Some(latest) = latest else {
        return Err(JournalError::conflict(
            "checkpoint install named a predecessor, but none is installed",
        ));
    };
    if latest.checkpoint_id != previous_checkpoint_id {
        return Err(JournalError::conflict(
            "checkpoint pointer changed before compare-and-install",
        ));
    }
    if checkpoint.through_step_seq < latest.through_step_seq {
        return Err(JournalError::integrity(
            "checkpoint pointer must advance monotonically",
        ));
    }
    Ok(())
}

/// §9.1: the store verifies that `covered_head` is the record digest at `through_step_seq`. It does
/// NOT check that this is still the current head — §22.14 rejects that, since it would serialise
/// checkpointing against ordinary transitions.
fn verify_covered_head(
    covered: Option<&JournalEntry>,
    pruned: Option<&PrunedAnchor>,
    covered_head: &str,
    through_step_seq: u64,
) -> JournalResult<()> {
    if let Some(covered) = covered {
        if covered.record_digest != covered_head {
            return Err(JournalError::integrity(
                "checkpoint covered_head does not match the record at its through_step_seq",
            ));
        }
        return Ok(());
    }
    if let Some(pruned) = pruned {
        if pruned.through_step_seq == through_step_seq && pruned.covered_head == covered_head {
            return Ok(());
        }
    }
    Err(JournalError::integrity(
        "checkpoint through_step_seq names no retained record",
    ))
}

/// Verify one contiguous run of entries links head-to-tail.
fn verify_chain(entries: &[JournalEntry]) -> JournalResult<()> {
    for window in entries.windows(2) {
        let (previous, entry) = (&window[0], &window[1]);
        if entry.step_seq != previous.step_seq + 1 {
            return Err(JournalError::integrity(format!(
                "journal chain has a gap: step_seq {} follows {}",
                entry.step_seq, previous.step_seq
            )));
        }
        if entry.previous_record_digest.as_deref() != Some(previous.record_digest.as_str()) {
            return Err(JournalError::integrity(
                "journal chain digest linkage is not continuous",
            ));
        }
    }
    Ok(())
}

/// The boundary a prune left behind: the last position that is no longer retained, plus its digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PrunedAnchor {
    through_step_seq: u64,
    covered_head: String,
}

/* ------------------------------------------------------------------ *
 * In-memory implementation
 * ------------------------------------------------------------------ */

#[derive(Default)]
struct OperationState {
    records: Vec<JournalEntry>,
    checkpoints: Vec<InstalledCheckpoint>,
    pruned: Option<PrunedAnchor>,
    outbound_envelope: Option<String>,
}

impl OperationState {
    fn head(&self) -> Option<JournalHead> {
        if let Some(last) = self.records.last() {
            return Some(JournalHead {
                step_seq: last.step_seq,
                record_digest: last.record_digest.clone(),
            });
        }
        self.pruned.as_ref().map(|pruned| JournalHead {
            step_seq: pruned.through_step_seq,
            record_digest: pruned.covered_head.clone(),
        })
    }

    fn read_from(&self, from_step_seq: u64) -> JournalResult<Vec<JournalEntry>> {
        // Retained records are dense and ordered, so the cursor is an index — not a scan. Callers
        // hit this once per durable step to fetch the head; a filter here would make a run
        // quadratic.
        let base = self.records.first().map_or(0, |entry| entry.step_seq);
        let start = (from_step_seq.saturating_sub(base) as usize).min(self.records.len());
        let entries = self.records[start..].to_vec();
        verify_chain(&entries)?;
        Ok(entries)
    }
}

/// **Single-process dev/test implementation** (spec Task 8b, criterion 2).
///
/// CAS is genuinely atomic here — every check-then-mutate below happens while the single [`Mutex`]
/// is held, and none of those critical sections contains an `.await`, so the compiler itself
/// guarantees no task interleaves inside one — but that atomicity ends at the process boundary. Two
/// processes sharing "the same" journal do not exist: each has its own map. Production hosts must
/// supply a `KernelJournal` whose CAS is a real storage-layer primitive (spec §9.1);
/// [`FileKernelJournal`] is the reference for that.
pub struct InMemoryKernelJournal {
    operations: Mutex<HashMap<String, OperationState>>,
}

impl Default for InMemoryKernelJournal {
    fn default() -> Self {
        Self {
            operations: Mutex::new(HashMap::new()),
        }
    }
}

impl InMemoryKernelJournal {
    pub fn new() -> Self {
        Self::default()
    }

    /// A poisoned lock means some other task panicked mid-mutation. The invariants this journal
    /// keeps are all re-derived from the vectors on every call (head is `records.last()`, the
    /// pointer is `checkpoints.last()`), and every mutation is a single push/retain, so the
    /// contents are still consistent; refusing every subsequent append would turn one unrelated
    /// panic into a dead operation.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, OperationState>> {
        self.operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl KernelJournal for InMemoryKernelJournal {
    async fn stage_outbound_envelope(
        &self,
        operation_id: &str,
        envelope_json: &str,
    ) -> JournalResult<()> {
        self.lock()
            .entry(operation_id.to_string())
            .or_default()
            .outbound_envelope = Some(envelope_json.to_string());
        Ok(())
    }

    async fn read_outbound_envelope(&self, operation_id: &str) -> JournalResult<Option<String>> {
        Ok(self
            .lock()
            .get(operation_id)
            .and_then(|state| state.outbound_envelope.clone()))
    }

    async fn clear_outbound_envelope(&self, operation_id: &str) -> JournalResult<()> {
        if let Some(state) = self.lock().get_mut(operation_id) {
            state.outbound_envelope = None;
        }
        Ok(())
    }

    async fn compare_and_append(
        &self,
        operation_id: &str,
        expected_head: Option<&str>,
        record: JournalRecordInput,
    ) -> JournalResult<JournalAppendReceipt> {
        validate_record(&record)?;
        // Atomic region: the guard is held across the check and the push, and there is no `.await`
        // inside it.
        let mut operations = self.lock();
        let state = operations.entry(operation_id.to_string()).or_default();
        check_append_precondition(state.head().as_ref(), expected_head, &record)?;
        state.records.push(JournalEntry {
            step_seq: record.step_seq,
            record_digest: record.record_digest.clone(),
            previous_record_digest: expected_head.map(str::to_string),
            record_bytes: record.record_bytes,
        });
        Ok(JournalAppendReceipt {
            step_seq: record.step_seq,
            record_digest: record.record_digest,
        })
    }

    async fn head(&self, operation_id: &str) -> JournalResult<Option<JournalHead>> {
        Ok(self.lock().get(operation_id).and_then(OperationState::head))
    }

    async fn read_from(
        &self,
        operation_id: &str,
        from_step_seq: u64,
    ) -> JournalResult<Vec<JournalEntry>> {
        match self.lock().get(operation_id) {
            Some(state) => state.read_from(from_step_seq),
            None => Ok(Vec::new()),
        }
    }

    async fn records_after(
        &self,
        operation_id: &str,
        after_head: Option<&str>,
    ) -> JournalResult<Vec<JournalEntry>> {
        let Some(after_head) = after_head else {
            return self.read_from(operation_id, 0).await;
        };
        let operations = self.lock();
        let Some(state) = operations.get(operation_id) else {
            return Err(JournalError::integrity(
                "journal cursor digest names no retained record",
            ));
        };
        if let Some(anchor) = state
            .records
            .iter()
            .find(|entry| entry.record_digest == after_head)
        {
            return state.read_from(anchor.step_seq + 1);
        }
        if let Some(pruned) = &state.pruned {
            if pruned.covered_head == after_head {
                return state.read_from(pruned.through_step_seq + 1);
            }
        }
        Err(JournalError::integrity(
            "journal cursor digest names no retained record",
        ))
    }

    async fn compare_and_install_checkpoint(
        &self,
        operation_id: &str,
        previous_checkpoint_id: Option<&str>,
        covered_head: &str,
        checkpoint: CheckpointCandidate,
    ) -> JournalResult<InstalledCheckpoint> {
        validate_candidate(&checkpoint)?;
        // Atomic region: no `.await` between the checks and the push.
        let mut operations = self.lock();
        let state = operations.entry(operation_id.to_string()).or_default();
        let latest = state.checkpoints.last();
        check_checkpoint_precondition(latest, previous_checkpoint_id, &checkpoint)?;
        let ordinal = latest.map_or(0, |latest| latest.ordinal + 1);
        verify_covered_head(
            state
                .records
                .iter()
                .find(|entry| entry.step_seq == checkpoint.through_step_seq),
            state.pruned.as_ref(),
            covered_head,
            checkpoint.through_step_seq,
        )?;
        let installed = InstalledCheckpoint {
            ordinal,
            checkpoint_id: checkpoint.checkpoint_id,
            previous_checkpoint_id: previous_checkpoint_id.map(str::to_string),
            covered_head: covered_head.to_string(),
            through_step_seq: checkpoint.through_step_seq,
            state_digest: checkpoint.state_digest,
            checkpoint_bytes: checkpoint.checkpoint_bytes,
            acknowledged: false,
        };
        state.checkpoints.push(installed.clone());
        Ok(installed)
    }

    async fn latest_checkpoint(
        &self,
        operation_id: &str,
    ) -> JournalResult<Option<InstalledCheckpoint>> {
        Ok(self
            .lock()
            .get(operation_id)
            .and_then(|state| state.checkpoints.last().cloned()))
    }

    async fn ack_checkpoint(
        &self,
        operation_id: &str,
        checkpoint_id: &str,
    ) -> JournalResult<InstalledCheckpoint> {
        let mut operations = self.lock();
        let installed = operations
            .get_mut(operation_id)
            .and_then(|state| {
                state
                    .checkpoints
                    .iter_mut()
                    .find(|entry| entry.checkpoint_id == checkpoint_id)
            })
            .ok_or_else(|| {
                JournalError::integrity("cannot acknowledge an uninstalled checkpoint")
            })?;
        installed.acknowledged = true;
        Ok(installed.clone())
    }

    async fn prune_acked_prefix(&self, operation_id: &str) -> JournalResult<JournalPruneReceipt> {
        let mut operations = self.lock();
        let Some(state) = operations.get_mut(operation_id) else {
            return Ok(JournalPruneReceipt {
                pruned_through_step_seq: None,
                pruned_count: 0,
            });
        };
        let Some(boundary) = state
            .checkpoints
            .iter()
            .rev()
            .find(|entry| entry.acknowledged)
            .cloned()
        else {
            return Ok(JournalPruneReceipt {
                pruned_through_step_seq: state.pruned.as_ref().map(|p| p.through_step_seq),
                pruned_count: 0,
            });
        };
        let before = state.records.len();
        state
            .records
            .retain(|entry| entry.step_seq > boundary.through_step_seq);
        if state
            .pruned
            .as_ref()
            .is_none_or(|pruned| pruned.through_step_seq < boundary.through_step_seq)
        {
            state.pruned = Some(PrunedAnchor {
                through_step_seq: boundary.through_step_seq,
                covered_head: boundary.covered_head.clone(),
            });
        }
        Ok(JournalPruneReceipt {
            pruned_through_step_seq: state.pruned.as_ref().map(|p| p.through_step_seq),
            pruned_count: (before - state.records.len()) as u64,
        })
    }
}

/* ------------------------------------------------------------------ *
 * File implementation
 * ------------------------------------------------------------------ */

const RECORD_SUFFIX: &str = ".rec";
const CHECKPOINT_SUFFIX: &str = ".ckpt";
const ACK_SUFFIX: &str = ".ack";
const OUTBOUND_ENVELOPE_FILE: &str = "outbound-envelope.json";

fn pad(value: u64) -> String {
    format!("{value:0SEQ_DIGITS$}")
}

/// `Some(position)` only for names that are exactly `<12 digits><suffix>`. Everything else —
/// staged temp files, `.rec.partial` residue, scratch notes — is not a committed record.
fn parse_position(name: &str, suffix: &str) -> Option<u64> {
    let stem = name.strip_suffix(suffix)?;
    if stem.len() != SEQ_DIGITS || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

/// Filesystem-safe, injective encoding of an operation id.
///
/// `.` is a passthrough character (so ordinary ids stay readable), which on its own would let the
/// ids `.` and `..` name a parent directory; a segment made only of dots is therefore escaped
/// whole, and the empty id gets its own reserved form. `~` never appears unescaped, so the encoding
/// stays injective.
fn safe_segment(value: &str) -> String {
    if value.is_empty() {
        return "~~".to_string();
    }
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push_str(&format!("~{:x}", ch as u32));
        }
    }
    if out.bytes().all(|byte| byte == b'.') {
        return out.chars().map(|_| "~2e").collect();
    }
    out
}

#[derive(Serialize, Deserialize)]
struct PersistedRecord {
    step_seq: u64,
    record_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_record_digest: Option<String>,
    /// base64, so the on-disk shape matches the Node reference implementation byte for byte.
    record_bytes: String,
}

#[derive(Serialize, Deserialize)]
struct PersistedCheckpoint {
    ordinal: u64,
    checkpoint_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_checkpoint_id: Option<String>,
    covered_head: String,
    through_step_seq: u64,
    state_digest: String,
    checkpoint_bytes: String,
}

/// **Cross-process atomic reference implementation** of [`KernelJournal`] (spec Task 8b).
///
/// The atomicity primitive is POSIX `link(2)` (`std::fs::hard_link`, reached through
/// `tokio::fs::hard_link` so the blocking call stays off the reactor): content is written to a
/// private temp file and fsynced, then hard-linked into its final name. `link` fails with
/// `AlreadyExists` if the name is taken, and it publishes already-complete content — so a crash can
/// never leave a half-written `.rec`, only an orphan temp file that the naming rule ignores. The
/// journal root must therefore live on a filesystem that supports hard links; that requirement is
/// the price of real CAS.
///
/// **Why the record filename is `<step_seq>.rec` and contains no digest.** The filename *is* the
/// collision domain. Two writers racing on the same head both compute the same next `step_seq`, so
/// they contend for one name and exactly one wins. Folding a per-writer value (the new record's
/// digest) into the name would give the racers *different* names — both links would succeed and the
/// chain would fork. Only the predecessor-determined part of the identity may appear in the name
/// (spec Task 8b correction (a)).
///
/// The pre-`link` head check is not a TOCTOU hole: it can only *reject* an append that `link` would
/// have accepted (a stale `expected_head` whose `step_seq` slot happens to be free), never accept
/// one `link` would have rejected. Every acceptance is still decided by the atomic link — the
/// publish half is factored into [`FileKernelJournal::publish_record`] /
/// [`FileKernelJournal::publish_checkpoint`] so that claim is directly testable without the
/// pre-check in front of it.
///
/// Checkpoint installs use the same primitive on a separate ordinal space (`<ordinal>.ckpt`), so
/// two processes installing on the same predecessor also contend for one name.
pub struct FileKernelJournal {
    root: PathBuf,
}

impl FileKernelJournal {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn operation_dir(&self, operation_id: &str) -> PathBuf {
        self.root.join(safe_segment(operation_id))
    }

    fn records_dir(&self, operation_id: &str) -> PathBuf {
        self.operation_dir(operation_id).join("records")
    }

    fn checkpoints_dir(&self, operation_id: &str) -> PathBuf {
        self.operation_dir(operation_id).join("checkpoints")
    }

    fn tmp_dir(&self, operation_id: &str) -> PathBuf {
        self.operation_dir(operation_id).join("tmp")
    }

    fn pruned_path(&self, operation_id: &str) -> PathBuf {
        self.operation_dir(operation_id).join("pruned.json")
    }

    fn outbound_envelope_path(&self, operation_id: &str) -> PathBuf {
        self.operation_dir(operation_id)
            .join(OUTBOUND_ENVELOPE_FILE)
    }

    /// Write `payload` to a temp file, fsync it, then atomically claim `target` by hard link.
    ///
    /// Returns `false` when the name was already taken — i.e. a lost CAS race.
    async fn publish(
        &self,
        operation_id: &str,
        target: &Path,
        payload: &str,
    ) -> JournalResult<bool> {
        let tmp_dir = self.tmp_dir(operation_id);
        fs::create_dir_all(&tmp_dir)
            .await
            .map_err(|err| JournalError::io("journal could not stage a durable record", err))?;
        let tmp_path = tmp_dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        if let Err(err) = stage(&tmp_path, payload).await {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(err);
        }
        let outcome = match fs::hard_link(&tmp_path, target).await {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(err) => Err(JournalError::io(
                "journal could not publish a durable record",
                err,
            )),
        };
        let _ = fs::remove_file(&tmp_path).await;
        if matches!(outcome, Ok(true)) {
            if let Some(parent) = target.parent() {
                sync_dir(parent).await;
            }
        }
        outcome
    }

    /// Sorted chain positions of the retained records. Anything not matching the naming rule is
    /// residue.
    async fn record_positions(&self, operation_id: &str) -> JournalResult<Vec<u64>> {
        list_positions(&self.records_dir(operation_id), RECORD_SUFFIX, "records").await
    }

    async fn read_record(
        &self,
        operation_id: &str,
        step_seq: u64,
    ) -> JournalResult<Option<JournalEntry>> {
        let name = format!("{}{RECORD_SUFFIX}", pad(step_seq));
        let Some(raw) =
            read_optional(&self.records_dir(operation_id).join(&name), "a record").await?
        else {
            return Ok(None);
        };
        let persisted: PersistedRecord = serde_json::from_str(&raw).map_err(|err| {
            JournalError::integrity(format!("journal record {name} is not readable: {err}"))
        })?;
        if persisted.step_seq != step_seq {
            return Err(JournalError::integrity(format!(
                "journal record {name} disagrees with its own step_seq"
            )));
        }
        Ok(Some(JournalEntry {
            step_seq: persisted.step_seq,
            record_digest: persisted.record_digest,
            previous_record_digest: persisted.previous_record_digest,
            record_bytes: base64_decode(&persisted.record_bytes).ok_or_else(|| {
                JournalError::integrity(format!("journal record {name} has unreadable bytes"))
            })?,
        }))
    }

    async fn pruned_anchor(&self, operation_id: &str) -> JournalResult<Option<PrunedAnchor>> {
        let Some(raw) = read_optional(&self.pruned_path(operation_id), "its pruned anchor").await?
        else {
            return Ok(None);
        };
        serde_json::from_str(&raw).map(Some).map_err(|err| {
            JournalError::integrity(format!("journal pruned anchor is not readable: {err}"))
        })
    }

    /// The anchor only ever moves forward, so an atomic overwriting `rename` is the right
    /// primitive — unlike record and checkpoint names, this one is *meant* to be replaced.
    async fn write_anchor(&self, operation_id: &str, anchor: &PrunedAnchor) -> JournalResult<()> {
        let tmp_dir = self.tmp_dir(operation_id);
        fs::create_dir_all(&tmp_dir)
            .await
            .map_err(|err| JournalError::io("journal could not record its pruned anchor", err))?;
        let tmp_path = tmp_dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let payload = serde_json::to_string(anchor).unwrap_or_default();
        let result = async {
            stage(&tmp_path, &payload).await?;
            fs::rename(&tmp_path, self.pruned_path(operation_id))
                .await
                .map_err(|err| JournalError::io("journal could not record its pruned anchor", err))
        }
        .await;
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path).await;
        }
        result
    }

    /// The publish half of [`KernelJournal::compare_and_append`]: everything after the pre-`link`
    /// head read. Private on purpose — it exists so the tests can prove that the atomic claim, not
    /// the pre-check, is what decides an append (a caller cannot reach it and skip validation).
    async fn publish_record(
        &self,
        operation_id: &str,
        expected_head: Option<&str>,
        record: &JournalRecordInput,
    ) -> JournalResult<JournalAppendReceipt> {
        let records_dir = self.records_dir(operation_id);
        fs::create_dir_all(&records_dir).await.map_err(|err| {
            JournalError::io("journal could not create its record directory", err)
        })?;
        let persisted = PersistedRecord {
            step_seq: record.step_seq,
            record_digest: record.record_digest.clone(),
            previous_record_digest: expected_head.map(str::to_string),
            record_bytes: base64_encode(&record.record_bytes),
        };
        let target = records_dir.join(format!("{}{RECORD_SUFFIX}", pad(record.step_seq)));
        let payload = serde_json::to_string(&persisted).map_err(|err| {
            JournalError::integrity(format!("journal record is not encodable: {err}"))
        })?;
        if !self.publish(operation_id, &target, &payload).await? {
            return Err(JournalError::conflict(format!(
                "journal step_seq {} was claimed by a concurrent writer",
                record.step_seq
            )));
        }
        Ok(JournalAppendReceipt {
            step_seq: record.step_seq,
            record_digest: record.record_digest.clone(),
        })
    }

    /// The publish half of [`KernelJournal::compare_and_install_checkpoint`]. Private for the same
    /// reason as [`Self::publish_record`].
    async fn publish_checkpoint(
        &self,
        operation_id: &str,
        ordinal: u64,
        previous_checkpoint_id: Option<&str>,
        covered_head: &str,
        checkpoint: &CheckpointCandidate,
    ) -> JournalResult<InstalledCheckpoint> {
        let checkpoints_dir = self.checkpoints_dir(operation_id);
        fs::create_dir_all(&checkpoints_dir).await.map_err(|err| {
            JournalError::io("journal could not create its checkpoint directory", err)
        })?;
        let persisted = PersistedCheckpoint {
            ordinal,
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            previous_checkpoint_id: previous_checkpoint_id.map(str::to_string),
            covered_head: covered_head.to_string(),
            through_step_seq: checkpoint.through_step_seq,
            state_digest: checkpoint.state_digest.clone(),
            checkpoint_bytes: base64_encode(&checkpoint.checkpoint_bytes),
        };
        let target = checkpoints_dir.join(format!("{}{CHECKPOINT_SUFFIX}", pad(ordinal)));
        let payload = serde_json::to_string(&persisted).map_err(|err| {
            JournalError::integrity(format!("checkpoint is not encodable: {err}"))
        })?;
        if !self.publish(operation_id, &target, &payload).await? {
            return Err(JournalError::conflict(format!(
                "checkpoint ordinal {ordinal} was claimed by a concurrent installer"
            )));
        }
        Ok(InstalledCheckpoint {
            ordinal,
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            previous_checkpoint_id: previous_checkpoint_id.map(str::to_string),
            covered_head: covered_head.to_string(),
            through_step_seq: checkpoint.through_step_seq,
            state_digest: checkpoint.state_digest.clone(),
            checkpoint_bytes: checkpoint.checkpoint_bytes.clone(),
            acknowledged: false,
        })
    }

    async fn checkpoint_ordinals(&self, operation_id: &str) -> JournalResult<Vec<u64>> {
        list_positions(
            &self.checkpoints_dir(operation_id),
            CHECKPOINT_SUFFIX,
            "checkpoints",
        )
        .await
    }

    async fn acked_ordinals(&self, operation_id: &str) -> JournalResult<HashSet<u64>> {
        Ok(list_positions(
            &self.checkpoints_dir(operation_id),
            ACK_SUFFIX,
            "checkpoints",
        )
        .await?
        .into_iter()
        .collect())
    }

    async fn read_checkpoint(
        &self,
        operation_id: &str,
        ordinal: u64,
        acked: Option<&HashSet<u64>>,
    ) -> JournalResult<Option<InstalledCheckpoint>> {
        let name = format!("{}{CHECKPOINT_SUFFIX}", pad(ordinal));
        let Some(raw) = read_optional(
            &self.checkpoints_dir(operation_id).join(&name),
            "a checkpoint",
        )
        .await?
        else {
            return Ok(None);
        };
        let persisted: PersistedCheckpoint = serde_json::from_str(&raw).map_err(|err| {
            JournalError::integrity(format!("checkpoint {name} is not readable: {err}"))
        })?;
        let acknowledged = match acked {
            Some(acked) => acked.contains(&ordinal),
            None => self.acked_ordinals(operation_id).await?.contains(&ordinal),
        };
        Ok(Some(InstalledCheckpoint {
            ordinal: persisted.ordinal,
            checkpoint_id: persisted.checkpoint_id,
            previous_checkpoint_id: persisted.previous_checkpoint_id,
            covered_head: persisted.covered_head,
            through_step_seq: persisted.through_step_seq,
            state_digest: persisted.state_digest,
            checkpoint_bytes: base64_decode(&persisted.checkpoint_bytes).ok_or_else(|| {
                JournalError::integrity(format!("checkpoint {name} has unreadable bytes"))
            })?,
            acknowledged,
        }))
    }
}

#[async_trait]
impl KernelJournal for FileKernelJournal {
    async fn stage_outbound_envelope(
        &self,
        operation_id: &str,
        envelope_json: &str,
    ) -> JournalResult<()> {
        let tmp_dir = self.tmp_dir(operation_id);
        fs::create_dir_all(&tmp_dir)
            .await
            .map_err(|err| JournalError::io("journal could not stage an outbound envelope", err))?;
        let tmp_path = tmp_dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = async {
            stage(&tmp_path, envelope_json).await?;
            fs::rename(&tmp_path, self.outbound_envelope_path(operation_id))
                .await
                .map_err(|err| {
                    JournalError::io("journal could not publish an outbound envelope", err)
                })?;
            sync_dir(&self.operation_dir(operation_id)).await;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path).await;
        }
        result
    }

    async fn read_outbound_envelope(&self, operation_id: &str) -> JournalResult<Option<String>> {
        read_optional(
            &self.outbound_envelope_path(operation_id),
            "a staged outbound envelope",
        )
        .await
    }

    async fn clear_outbound_envelope(&self, operation_id: &str) -> JournalResult<()> {
        match fs::remove_file(self.outbound_envelope_path(operation_id)).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(JournalError::io(
                "journal could not clear a staged outbound envelope",
                err,
            )),
        }
    }

    async fn compare_and_append(
        &self,
        operation_id: &str,
        expected_head: Option<&str>,
        record: JournalRecordInput,
    ) -> JournalResult<JournalAppendReceipt> {
        validate_record(&record)?;
        check_append_precondition(
            self.head(operation_id).await?.as_ref(),
            expected_head,
            &record,
        )?;
        self.publish_record(operation_id, expected_head, &record)
            .await
    }

    async fn head(&self, operation_id: &str) -> JournalResult<Option<JournalHead>> {
        if let Some(last) = self.record_positions(operation_id).await?.last().copied() {
            if let Some(entry) = self.read_record(operation_id, last).await? {
                return Ok(Some(JournalHead {
                    step_seq: entry.step_seq,
                    record_digest: entry.record_digest,
                }));
            }
        }
        Ok(self
            .pruned_anchor(operation_id)
            .await?
            .map(|pruned| JournalHead {
                step_seq: pruned.through_step_seq,
                record_digest: pruned.covered_head,
            }))
    }

    async fn read_from(
        &self,
        operation_id: &str,
        from_step_seq: u64,
    ) -> JournalResult<Vec<JournalEntry>> {
        let mut entries = Vec::new();
        for position in self.record_positions(operation_id).await? {
            if position < from_step_seq {
                continue;
            }
            if let Some(entry) = self.read_record(operation_id, position).await? {
                entries.push(entry);
            }
        }
        verify_chain(&entries)?;
        Ok(entries)
    }

    async fn records_after(
        &self,
        operation_id: &str,
        after_head: Option<&str>,
    ) -> JournalResult<Vec<JournalEntry>> {
        let Some(after_head) = after_head else {
            return self.read_from(operation_id, 0).await;
        };
        for position in self.record_positions(operation_id).await? {
            if let Some(entry) = self.read_record(operation_id, position).await? {
                if entry.record_digest == after_head {
                    return self.read_from(operation_id, position + 1).await;
                }
            }
        }
        if let Some(pruned) = self.pruned_anchor(operation_id).await? {
            if pruned.covered_head == after_head {
                return self
                    .read_from(operation_id, pruned.through_step_seq + 1)
                    .await;
            }
        }
        Err(JournalError::integrity(
            "journal cursor digest names no retained record",
        ))
    }

    async fn compare_and_install_checkpoint(
        &self,
        operation_id: &str,
        previous_checkpoint_id: Option<&str>,
        covered_head: &str,
        checkpoint: CheckpointCandidate,
    ) -> JournalResult<InstalledCheckpoint> {
        validate_candidate(&checkpoint)?;
        let latest = self.latest_checkpoint(operation_id).await?;
        check_checkpoint_precondition(latest.as_ref(), previous_checkpoint_id, &checkpoint)?;
        verify_covered_head(
            self.read_record(operation_id, checkpoint.through_step_seq)
                .await?
                .as_ref(),
            self.pruned_anchor(operation_id).await?.as_ref(),
            covered_head,
            checkpoint.through_step_seq,
        )?;
        let ordinal = latest.map_or(0, |latest| latest.ordinal + 1);
        self.publish_checkpoint(
            operation_id,
            ordinal,
            previous_checkpoint_id,
            covered_head,
            &checkpoint,
        )
        .await
    }

    async fn latest_checkpoint(
        &self,
        operation_id: &str,
    ) -> JournalResult<Option<InstalledCheckpoint>> {
        match self
            .checkpoint_ordinals(operation_id)
            .await?
            .last()
            .copied()
        {
            Some(ordinal) => self.read_checkpoint(operation_id, ordinal, None).await,
            None => Ok(None),
        }
    }

    async fn ack_checkpoint(
        &self,
        operation_id: &str,
        checkpoint_id: &str,
    ) -> JournalResult<InstalledCheckpoint> {
        let mut ordinals = self.checkpoint_ordinals(operation_id).await?;
        ordinals.reverse();
        for ordinal in ordinals {
            let Some(installed) = self.read_checkpoint(operation_id, ordinal, None).await? else {
                continue;
            };
            if installed.checkpoint_id != checkpoint_id {
                continue;
            }
            if !installed.acknowledged {
                // `publish` returning false means another process already acknowledged it — the
                // acknowledgement is idempotent, so a lost race is still a success.
                let target = self
                    .checkpoints_dir(operation_id)
                    .join(format!("{}{ACK_SUFFIX}", pad(ordinal)));
                let payload =
                    serde_json::json!({ "ordinal": ordinal, "checkpoint_id": checkpoint_id })
                        .to_string();
                self.publish(operation_id, &target, &payload).await?;
            }
            return Ok(InstalledCheckpoint {
                acknowledged: true,
                ..installed
            });
        }
        Err(JournalError::integrity(
            "cannot acknowledge an uninstalled checkpoint",
        ))
    }

    async fn prune_acked_prefix(&self, operation_id: &str) -> JournalResult<JournalPruneReceipt> {
        let acked = self.acked_ordinals(operation_id).await?;
        let existing = self.pruned_anchor(operation_id).await?;
        let mut ordinals = self.checkpoint_ordinals(operation_id).await?;
        ordinals.reverse();
        let mut boundary = None;
        for ordinal in ordinals {
            if !acked.contains(&ordinal) {
                continue;
            }
            boundary = self
                .read_checkpoint(operation_id, ordinal, Some(&acked))
                .await?;
            break;
        }
        let Some(boundary) = boundary else {
            return Ok(JournalPruneReceipt {
                pruned_through_step_seq: existing.map(|anchor| anchor.through_step_seq),
                pruned_count: 0,
            });
        };
        // Anchor first, delete second: a crash mid-prune leaves a resolvable head either way.
        if existing
            .as_ref()
            .is_none_or(|anchor| anchor.through_step_seq < boundary.through_step_seq)
        {
            self.write_anchor(
                operation_id,
                &PrunedAnchor {
                    through_step_seq: boundary.through_step_seq,
                    covered_head: boundary.covered_head.clone(),
                },
            )
            .await?;
        }
        let mut pruned_count = 0;
        for position in self.record_positions(operation_id).await? {
            if position > boundary.through_step_seq {
                break;
            }
            let _ = fs::remove_file(
                self.records_dir(operation_id)
                    .join(format!("{}{RECORD_SUFFIX}", pad(position))),
            )
            .await;
            pruned_count += 1;
        }
        Ok(JournalPruneReceipt {
            pruned_through_step_seq: Some(
                boundary.through_step_seq.max(
                    existing
                        .as_ref()
                        .map_or(boundary.through_step_seq, |anchor| anchor.through_step_seq),
                ),
            ),
            pruned_count,
        })
    }
}

/* ------------------------------------------------------------------ *
 * File helpers
 * ------------------------------------------------------------------ */

/// Write `payload` into a fresh private file and fsync it, so whatever a later `link` publishes is
/// already complete.
async fn stage(tmp_path: &Path, payload: &str) -> JournalResult<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(tmp_path)
        .await
        .map_err(|err| JournalError::io("journal could not stage a durable record", err))?;
    file.write_all(payload.as_bytes())
        .await
        .map_err(|err| JournalError::io("journal could not stage a durable record", err))?;
    file.sync_all()
        .await
        .map_err(|err| JournalError::io("journal could not stage a durable record", err))?;
    Ok(())
}

/// Directory fsync is a durability nicety; some platforms refuse it. Never fail the append on it.
async fn sync_dir(dir: &Path) {
    if let Ok(handle) = fs::File::open(dir).await {
        let _ = handle.sync_all().await;
    }
}

async fn list_positions(dir: &Path, suffix: &str, what: &str) -> JournalResult<Vec<u64>> {
    let mut read_dir = match fs::read_dir(dir).await {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(JournalError::io(
                format!("journal could not list its {what}"),
                err,
            ));
        }
    };
    let mut positions = Vec::new();
    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|err| JournalError::io(format!("journal could not list its {what}"), err))?
    {
        if let Some(position) = parse_position(&entry.file_name().to_string_lossy(), suffix) {
            positions.push(position);
        }
    }
    positions.sort_unstable();
    Ok(positions)
}

async fn read_optional(path: &Path, what: &str) -> JournalResult<Option<String>> {
    match fs::read_to_string(path).await {
        Ok(raw) => Ok(Some(raw)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(JournalError::io(
            format!("journal could not read {what}"),
            err,
        )),
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Opaque record bytes are stored base64-encoded, matching the Node reference implementation's
/// on-disk shape so a journal directory stays readable by any host.
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let packed = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(BASE64_ALPHABET[(packed >> 18) as usize & 63] as char);
        out.push(BASE64_ALPHABET[(packed >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(packed >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[packed as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' => continue,
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((accumulator >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/* ------------------------------------------------------------------ *
 * Tests
 * ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    const OP: &str = "op-journal";

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("ds-journal-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A record shaped like core's output: opaque bytes + the digest core assigned them.
    fn record(step_seq: u64, digest: &str) -> JournalRecordInput {
        payload_record(step_seq, digest, &format!("payload-{step_seq}"))
    }

    fn payload_record(step_seq: u64, digest: &str, payload: &str) -> JournalRecordInput {
        JournalRecordInput {
            step_seq,
            record_digest: digest.to_string(),
            record_bytes: payload.as_bytes().to_vec(),
        }
    }

    fn candidate(id: &str, through_step_seq: u64) -> CheckpointCandidate {
        CheckpointCandidate {
            checkpoint_id: id.to_string(),
            through_step_seq,
            state_digest: format!("state-{id}"),
            checkpoint_bytes: format!("checkpoint-{id}").into_bytes(),
        }
    }

    fn head(step_seq: u64, digest: &str) -> Option<JournalHead> {
        Some(JournalHead {
            step_seq,
            record_digest: digest.to_string(),
        })
    }

    fn positions(entries: &[JournalEntry]) -> Vec<u64> {
        entries.iter().map(|entry| entry.step_seq).collect()
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).expect("utf8")
    }

    /// Append `count` linked records after genesis; returns every digest in chain order.
    async fn seed_chain(
        journal: &dyn KernelJournal,
        count: u64,
        operation_id: &str,
    ) -> Vec<String> {
        let mut digests = vec!["d0".to_string()];
        journal
            .compare_and_append(operation_id, None, record(0, "d0"))
            .await
            .expect("genesis");
        for step_seq in 1..=count {
            let digest = format!("d{step_seq}");
            journal
                .compare_and_append(
                    operation_id,
                    Some(&digests[(step_seq - 1) as usize]),
                    record(step_seq, &digest),
                )
                .await
                .expect("append");
            digests.push(digest);
        }
        digests
    }

    fn assert_conflict<T: std::fmt::Debug>(result: JournalResult<T>) {
        match result {
            Err(err @ JournalError::CasConflict(_)) => assert!(err.is_retryable()),
            other => panic!("expected a CAS conflict, got {other:?}"),
        }
    }

    fn assert_integrity<T: std::fmt::Debug>(result: JournalResult<T>) {
        match result {
            Err(err @ JournalError::Integrity(_)) => assert!(!err.is_retryable()),
            other => panic!("expected an integrity fault, got {other:?}"),
        }
    }

    /* ---------- the contract, run against both implementations ---------- */

    async fn genesis_append_advances_the_head(journal: &dyn KernelJournal) {
        assert_eq!(journal.head(OP).await.unwrap(), None);

        let receipt = journal
            .compare_and_append(OP, None, record(0, "d0"))
            .await
            .unwrap();
        assert_eq!(
            receipt,
            JournalAppendReceipt {
                step_seq: 0,
                record_digest: "d0".into()
            }
        );
        assert_eq!(journal.head(OP).await.unwrap(), head(0, "d0"));

        journal
            .compare_and_append(OP, Some("d0"), record(1, "d1"))
            .await
            .unwrap();
        assert_eq!(journal.head(OP).await.unwrap(), head(1, "d1"));
    }

    async fn stores_bytes_verbatim_and_links_each_record(journal: &dyn KernelJournal) {
        seed_chain(journal, 2, OP).await;
        let entries = journal.read_from(OP, 0).await.unwrap();

        assert_eq!(positions(&entries), vec![0, 1, 2]);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.previous_record_digest.clone())
                .collect::<Vec<_>>(),
            vec![None, Some("d0".into()), Some("d1".into())]
        );
        assert_eq!(text(&entries[2].record_bytes), "payload-2");
    }

    async fn rejects_a_stale_expected_head_without_overwriting(journal: &dyn KernelJournal) {
        seed_chain(journal, 1, OP).await;

        assert_conflict(
            journal
                .compare_and_append(OP, Some("d0"), record(1, "other"))
                .await,
        );
        // The winner is untouched: same head, same bytes, no fork.
        assert_eq!(journal.head(OP).await.unwrap(), head(1, "d1"));
        assert_eq!(journal.read_from(OP, 0).await.unwrap().len(), 2);
    }

    async fn rejects_a_second_genesis_on_a_non_empty_chain(journal: &dyn KernelJournal) {
        journal
            .compare_and_append(OP, None, record(0, "d0"))
            .await
            .unwrap();
        assert_conflict(
            journal
                .compare_and_append(OP, None, record(0, "other"))
                .await,
        );
    }

    async fn separates_a_cas_conflict_from_an_integrity_violation(journal: &dyn KernelJournal) {
        seed_chain(journal, 1, OP).await;

        // Head matches, but the record claims a position that does not follow it.
        assert_integrity(
            journal
                .compare_and_append(OP, Some("d1"), record(5, "d5"))
                .await,
        );
        // A genesis record on a chain that already has one is a conflict, not an integrity fault.
        assert_conflict(journal.compare_and_append(OP, None, record(0, "d0")).await);
    }

    async fn reads_by_step_cursor_and_by_digest_cursor(journal: &dyn KernelJournal) {
        let digests = seed_chain(journal, 3, OP).await;

        assert_eq!(
            positions(&journal.read_from(OP, 2).await.unwrap()),
            vec![2, 3]
        );
        assert_eq!(
            positions(&journal.records_after(OP, Some(&digests[1])).await.unwrap()),
            vec![2, 3]
        );
        assert_eq!(
            positions(&journal.records_after(OP, None).await.unwrap()),
            vec![0, 1, 2, 3]
        );
        assert_integrity(journal.records_after(OP, Some("not-a-record")).await);
    }

    async fn keeps_operations_isolated(journal: &dyn KernelJournal) {
        seed_chain(journal, 1, "op-a").await;
        seed_chain(journal, 2, "op-b").await;

        assert_eq!(journal.head("op-a").await.unwrap(), head(1, "d1"));
        assert_eq!(journal.head("op-b").await.unwrap(), head(2, "d2"));
    }

    /// §22.14: the covered head does not have to still be the current head.
    async fn installs_a_checkpoint_covering_a_non_current_head(journal: &dyn KernelJournal) {
        let digests = seed_chain(journal, 3, OP).await;

        // Candidate covers step 1; steps 2 and 3 were appended after it was taken.
        let installed = journal
            .compare_and_install_checkpoint(OP, None, &digests[1], candidate("ck-1", 1))
            .await
            .unwrap();

        assert_eq!(installed.ordinal, 0);
        assert_eq!(installed.covered_head, "d1");
        assert!(!installed.acknowledged);
        assert_eq!(journal.head(OP).await.unwrap(), head(3, "d3"));
        assert_eq!(
            journal
                .latest_checkpoint(OP)
                .await
                .unwrap()
                .map(|installed| installed.checkpoint_id),
            Some("ck-1".to_string())
        );
        // The tail survives the install.
        assert_eq!(
            positions(&journal.read_from(OP, 0).await.unwrap()),
            vec![0, 1, 2, 3]
        );
    }

    async fn rejects_a_checkpoint_whose_covered_head_disagrees(journal: &dyn KernelJournal) {
        let digests = seed_chain(journal, 2, OP).await;

        assert_integrity(
            journal
                .compare_and_install_checkpoint(OP, None, &digests[2], candidate("ck-1", 1))
                .await,
        );
        assert_integrity(
            journal
                .compare_and_install_checkpoint(OP, None, "d9", candidate("ck-1", 9))
                .await,
        );
        assert!(journal.latest_checkpoint(OP).await.unwrap().is_none());
    }

    async fn advances_the_checkpoint_pointer_monotonically(journal: &dyn KernelJournal) {
        let digests = seed_chain(journal, 3, OP).await;
        journal
            .compare_and_install_checkpoint(OP, None, &digests[1], candidate("ck-1", 1))
            .await
            .unwrap();

        // Installing again without naming the predecessor is a conflict.
        assert_conflict(
            journal
                .compare_and_install_checkpoint(OP, None, &digests[2], candidate("ck-2", 2))
                .await,
        );
        // Naming a stale predecessor is a conflict.
        assert_conflict(
            journal
                .compare_and_install_checkpoint(OP, Some("ck-0"), &digests[2], candidate("ck-2", 2))
                .await,
        );
        // Naming the current predecessor but moving backwards is an integrity fault.
        assert_integrity(
            journal
                .compare_and_install_checkpoint(OP, Some("ck-1"), &digests[0], candidate("ck-2", 0))
                .await,
        );

        let second = journal
            .compare_and_install_checkpoint(OP, Some("ck-1"), &digests[2], candidate("ck-2", 2))
            .await
            .unwrap();
        assert_eq!(second.ordinal, 1);
        assert_eq!(second.previous_checkpoint_id.as_deref(), Some("ck-1"));
        assert_eq!(
            journal
                .latest_checkpoint(OP)
                .await
                .unwrap()
                .map(|installed| installed.checkpoint_id),
            Some("ck-2".to_string())
        );
    }

    async fn gates_prefix_reclamation_on_the_acknowledgement(journal: &dyn KernelJournal) {
        let digests = seed_chain(journal, 3, OP).await;
        journal
            .compare_and_install_checkpoint(OP, None, &digests[2], candidate("ck-1", 2))
            .await
            .unwrap();

        // Installed but unacknowledged: nothing is reclaimed.
        assert_eq!(
            journal.prune_acked_prefix(OP).await.unwrap(),
            JournalPruneReceipt {
                pruned_through_step_seq: None,
                pruned_count: 0
            }
        );
        assert_eq!(journal.read_from(OP, 0).await.unwrap().len(), 4);

        let acked = journal.ack_checkpoint(OP, "ck-1").await.unwrap();
        assert!(acked.acknowledged);
        assert!(
            journal
                .latest_checkpoint(OP)
                .await
                .unwrap()
                .unwrap()
                .acknowledged
        );

        assert_eq!(
            journal.prune_acked_prefix(OP).await.unwrap(),
            JournalPruneReceipt {
                pruned_through_step_seq: Some(2),
                pruned_count: 3
            }
        );
        assert_eq!(positions(&journal.read_from(OP, 0).await.unwrap()), vec![3]);
        // The pruned boundary is retained as an anchor, so head and the digest cursor still resolve.
        assert_eq!(journal.head(OP).await.unwrap(), head(3, "d3"));
        assert_eq!(
            positions(&journal.records_after(OP, Some(&digests[2])).await.unwrap()),
            vec![3]
        );
        // And the chain keeps growing from the surviving head.
        journal
            .compare_and_append(OP, Some("d3"), record(4, "d4"))
            .await
            .unwrap();
        assert_eq!(journal.head(OP).await.unwrap(), head(4, "d4"));
    }

    async fn refuses_to_acknowledge_an_uninstalled_checkpoint(journal: &dyn KernelJournal) {
        seed_chain(journal, 1, OP).await;
        assert_integrity(journal.ack_checkpoint(OP, "ck-missing").await);
    }

    async fn stages_reads_and_clears_an_outbound_envelope(journal: &dyn KernelJournal) {
        assert_eq!(journal.read_outbound_envelope(OP).await.unwrap(), None);
        journal
            .stage_outbound_envelope(OP, r#"{"input_id":"stable","kind":"configure_operation"}"#)
            .await
            .unwrap();
        assert_eq!(
            journal.read_outbound_envelope(OP).await.unwrap().as_deref(),
            Some(r#"{"input_id":"stable","kind":"configure_operation"}"#)
        );
        // Staging replaces the prior envelope atomically: only the most recently attempted,
        // not-yet-owned input can be replayed after wake.
        journal
            .stage_outbound_envelope(OP, r#"{"input_id":"replacement"}"#)
            .await
            .unwrap();
        assert_eq!(
            journal.read_outbound_envelope(OP).await.unwrap().as_deref(),
            Some(r#"{"input_id":"replacement"}"#)
        );
        journal.clear_outbound_envelope(OP).await.unwrap();
        journal.clear_outbound_envelope(OP).await.unwrap();
        assert_eq!(journal.read_outbound_envelope(OP).await.unwrap(), None);
    }

    /// Every contract case above runs against both implementations — the taxonomy they promise is
    /// the capability's contract, not one implementation's behaviour.
    macro_rules! contract_suite {
        ($($case:ident),+ $(,)?) => {
            mod in_memory {
                use super::*;
                $(
                    #[tokio::test]
                    async fn $case() {
                        super::$case(&InMemoryKernelJournal::new()).await;
                    }
                )+
            }

            mod file {
                use super::*;
                $(
                    #[tokio::test]
                    async fn $case() {
                        let dir = TempDir::new();
                        super::$case(&FileKernelJournal::new(dir.path())).await;
                    }
                )+
            }
        };
    }

    contract_suite!(
        genesis_append_advances_the_head,
        stores_bytes_verbatim_and_links_each_record,
        rejects_a_stale_expected_head_without_overwriting,
        rejects_a_second_genesis_on_a_non_empty_chain,
        separates_a_cas_conflict_from_an_integrity_violation,
        reads_by_step_cursor_and_by_digest_cursor,
        keeps_operations_isolated,
        installs_a_checkpoint_covering_a_non_current_head,
        rejects_a_checkpoint_whose_covered_head_disagrees,
        advances_the_checkpoint_pointer_monotonically,
        gates_prefix_reclamation_on_the_acknowledgement,
        refuses_to_acknowledge_an_uninstalled_checkpoint,
        stages_reads_and_clears_an_outbound_envelope,
    );

    /* ---------- FileKernelJournal: cross-process atomicity ---------- */

    /// Run `body` on `count` OS threads that all leave the barrier together — real parallelism, so
    /// only the storage layer's atomic claim can decide the race.
    fn race<T, F>(count: usize, body: F) -> Vec<T>
    where
        T: Send + 'static,
        F: Fn(usize) -> T + Send + Sync + 'static,
    {
        let barrier = Arc::new(Barrier::new(count));
        let body = Arc::new(body);
        let handles: Vec<_> = (0..count)
            .map(|index| {
                let barrier = Arc::clone(&barrier);
                let body = Arc::clone(&body);
                std::thread::spawn(move || {
                    barrier.wait();
                    body(index)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("worker thread"))
            .collect()
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    /// One `Ok`, everything else a CAS conflict.
    fn assert_single_winner<T: std::fmt::Debug>(results: &[JournalResult<T>]) -> &T {
        let winners: Vec<&T> = results
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .collect();
        assert_eq!(winners.len(), 1, "expected exactly one winner: {results:?}");
        for result in results {
            if let Err(err) = result {
                assert!(
                    matches!(err, JournalError::CasConflict(_)),
                    "loser must lose with a CAS conflict, got {err:?}"
                );
            }
        }
        winners[0]
    }

    #[tokio::test]
    async fn two_concurrent_writers_contend_for_one_chain_position() {
        let dir = TempDir::new();
        let root = dir.path().to_path_buf();
        FileKernelJournal::new(&root)
            .compare_and_append(OP, None, record(0, "d0"))
            .await
            .unwrap();

        let raced = root.clone();
        let results = race(2, move |index| {
            let journal = FileKernelJournal::new(&raced);
            block_on(journal.compare_and_append(
                OP,
                Some("d0"),
                payload_record(1, &format!("from-{index}"), &format!("w{index}")),
            ))
        });

        let winner = assert_single_winner(&results);
        // The journal did not fork: one record at step 1, and every reader agrees on it.
        let entries = FileKernelJournal::new(&root)
            .read_from(OP, 0)
            .await
            .unwrap();
        assert_eq!(positions(&entries), vec![0, 1]);
        assert_eq!(entries[1].record_digest, winner.record_digest);
    }

    #[tokio::test]
    async fn a_wide_append_storm_still_has_a_single_winner_per_position() {
        let dir = TempDir::new();
        let root = dir.path().to_path_buf();
        FileKernelJournal::new(&root)
            .compare_and_append(OP, None, record(0, "d0"))
            .await
            .unwrap();

        let raced = root.clone();
        let results = race(8, move |index| {
            let journal = FileKernelJournal::new(&raced);
            block_on(journal.compare_and_append(
                OP,
                Some("d0"),
                payload_record(1, &format!("d1-{index}"), &format!("w{index}")),
            ))
        });

        assert_single_winner(&results);
        assert_eq!(
            FileKernelJournal::new(&root)
                .read_from(OP, 0)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn two_concurrent_installers_contend_for_one_checkpoint_ordinal() {
        let dir = TempDir::new();
        let root = dir.path().to_path_buf();
        let digests = seed_chain(&FileKernelJournal::new(&root), 2, OP).await;
        assert_eq!(digests[2], "d2");

        let raced = root.clone();
        let results = race(2, move |index| {
            let journal = FileKernelJournal::new(&raced);
            // Both installers name the same predecessor (none) and the same covered head, so the
            // only thing that can separate them is the ordinal name they both try to claim.
            block_on(journal.compare_and_install_checkpoint(
                OP,
                None,
                "d2",
                candidate(if index == 0 { "ck-a" } else { "ck-b" }, 2),
            ))
        });

        assert_single_winner(&results);
        let installed = FileKernelJournal::new(&root)
            .latest_checkpoint(OP)
            .await
            .unwrap()
            .unwrap();
        assert!(["ck-a", "ck-b"].contains(&installed.checkpoint_id.as_str()));
        assert_eq!(installed.ordinal, 0);
        let names: Vec<String> = std::fs::read_dir(root.join(OP).join("checkpoints"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["000000000000.ckpt".to_string()]);
    }

    #[tokio::test]
    async fn the_atomic_claim_not_the_pre_check_decides_the_append() {
        // The pre-`link` head read cannot be the fence: another process may commit between it and
        // the publish. `publish_record` is exactly the code that runs after the pre-check, so
        // calling it with an already-taken position simulates that interleaving. If acceptance were
        // decided in userspace this append would succeed and fork the chain; it must lose to the
        // storage layer instead.
        let dir = TempDir::new();
        let committed = FileKernelJournal::new(dir.path());
        committed
            .compare_and_append(OP, None, record(0, "d0"))
            .await
            .unwrap();
        committed
            .compare_and_append(OP, Some("d0"), payload_record(1, "winner", "winner"))
            .await
            .unwrap();

        let loser = FileKernelJournal::new(dir.path());
        assert_conflict(
            loser
                .publish_record(OP, Some("d0"), &payload_record(1, "loser", "loser"))
                .await,
        );

        let entries = committed.read_from(OP, 0).await.unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.record_digest.clone())
                .collect::<Vec<_>>(),
            vec!["d0".to_string(), "winner".to_string()]
        );
        assert_eq!(text(&entries[1].record_bytes), "winner");
    }

    #[tokio::test]
    async fn the_atomic_claim_decides_the_checkpoint_install_too() {
        let dir = TempDir::new();
        let committed = FileKernelJournal::new(dir.path());
        let digests = seed_chain(&committed, 2, OP).await;
        committed
            .compare_and_install_checkpoint(OP, None, &digests[2], candidate("ck-winner", 2))
            .await
            .unwrap();

        let loser = FileKernelJournal::new(dir.path());
        assert_conflict(
            loser
                .publish_checkpoint(OP, 0, None, &digests[2], &candidate("ck-loser", 2))
                .await,
        );

        assert_eq!(
            committed
                .latest_checkpoint(OP)
                .await
                .unwrap()
                .unwrap()
                .checkpoint_id,
            "ck-winner"
        );
    }

    #[tokio::test]
    async fn reopens_and_verifies_the_chain_ignoring_crash_residue() {
        let dir = TempDir::new();
        let journal = FileKernelJournal::new(dir.path());
        let digests = seed_chain(&journal, 3, OP).await;

        // Residue a crash can actually leave: a staged-but-unlinked temp file, plus anything that
        // does not match the record naming rule. Neither may be mistaken for a committed record.
        let operation_dir = dir.path().join(OP);
        std::fs::create_dir_all(operation_dir.join("tmp")).unwrap();
        std::fs::write(
            operation_dir.join("tmp").join("half-written.tmp"),
            r#"{"step_seq":4,"record_dig"#,
        )
        .unwrap();
        std::fs::write(
            operation_dir
                .join("records")
                .join("000000000004.rec.partial"),
            r#"{"step_seq":4"#,
        )
        .unwrap();
        std::fs::write(operation_dir.join("records").join("notes.txt"), "scratch").unwrap();

        let reopened = FileKernelJournal::new(dir.path());
        let entries = reopened.read_from(OP, 0).await.unwrap();
        assert_eq!(positions(&entries), vec![0, 1, 2, 3]);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.record_digest.clone())
                .collect::<Vec<_>>(),
            digests
        );
        assert_eq!(reopened.head(OP).await.unwrap(), head(3, "d3"));
        // The chain still accepts its next record, so residue did not poison the CAS position.
        reopened
            .compare_and_append(OP, Some("d3"), record(4, "d4"))
            .await
            .unwrap();
        assert_eq!(reopened.head(OP).await.unwrap(), head(4, "d4"));
    }

    #[tokio::test]
    async fn reopens_installed_and_acknowledged_checkpoints() {
        let dir = TempDir::new();
        let journal = FileKernelJournal::new(dir.path());
        let digests = seed_chain(&journal, 2, OP).await;
        journal
            .compare_and_install_checkpoint(OP, None, &digests[1], candidate("ck-1", 1))
            .await
            .unwrap();
        journal.ack_checkpoint(OP, "ck-1").await.unwrap();

        let reopened = FileKernelJournal::new(dir.path());
        let latest = reopened.latest_checkpoint(OP).await.unwrap().unwrap();
        assert_eq!(latest.checkpoint_id, "ck-1");
        assert!(latest.acknowledged);
        assert_eq!(latest.covered_head, "d1");
        assert_eq!(latest.through_step_seq, 1);
        assert_eq!(text(&latest.checkpoint_bytes), "checkpoint-ck-1");
    }

    #[tokio::test]
    async fn raises_an_integrity_fault_when_a_record_contradicts_its_own_name() {
        let dir = TempDir::new();
        let journal = FileKernelJournal::new(dir.path());
        seed_chain(&journal, 1, OP).await;
        let records_dir = dir.path().join(OP).join("records");

        // A committed name holding a record that claims another position.
        std::fs::write(
            records_dir.join("000000000002.rec"),
            r#"{"step_seq":7,"record_digest":"d7","record_bytes":""}"#,
        )
        .unwrap();
        assert_integrity(FileKernelJournal::new(dir.path()).read_from(OP, 0).await);

        // A committed name holding bytes that are not a record at all.
        std::fs::write(records_dir.join("000000000002.rec"), r#"{"step_seq":2"#).unwrap();
        assert_integrity(FileKernelJournal::new(dir.path()).read_from(OP, 0).await);
    }

    /* ---------- naming and encoding rules ---------- */

    #[test]
    fn only_exactly_padded_names_are_committed_records() {
        assert_eq!(parse_position("000000000004.rec", RECORD_SUFFIX), Some(4));
        assert_eq!(
            parse_position("000000000004.rec.partial", RECORD_SUFFIX),
            None
        );
        assert_eq!(parse_position("4.rec", RECORD_SUFFIX), None);
        assert_eq!(parse_position("0000000000004.rec", RECORD_SUFFIX), None);
        assert_eq!(parse_position("notes.txt", RECORD_SUFFIX), None);
        assert_eq!(parse_position("000000000004.ckpt", RECORD_SUFFIX), None);
        assert_eq!(pad(4), "000000000004");
    }

    #[test]
    fn an_operation_id_can_never_name_a_directory_outside_the_journal() {
        assert_eq!(safe_segment("op-journal"), "op-journal");
        assert_eq!(safe_segment("session:1/op-1"), "session~3a1~2fop-1");
        assert_eq!(safe_segment(".."), "~2e~2e");
        assert_eq!(safe_segment("."), "~2e");
        assert_eq!(safe_segment(""), "~~");
        // Injective: distinct ids never collide on one directory.
        assert_ne!(safe_segment("a/b"), safe_segment("a-b"));
    }

    #[test]
    fn record_bytes_survive_a_base64_round_trip() {
        for payload in [
            vec![],
            vec![0u8],
            vec![0u8, 255, 128],
            b"payload-2".to_vec(),
            (0..=255u8).collect::<Vec<u8>>(),
        ] {
            assert_eq!(base64_decode(&base64_encode(&payload)), Some(payload));
        }
        assert_eq!(
            base64_encode(b"any carnal pleasure."),
            "YW55IGNhcm5hbCBwbGVhc3VyZS4="
        );
        assert_eq!(base64_decode("not base64!"), None);
    }

    #[tokio::test]
    async fn a_chain_position_past_the_name_space_is_refused_by_both_implementations() {
        let dir = TempDir::new();
        let file = FileKernelJournal::new(dir.path());
        let memory = InMemoryKernelJournal::new();
        for journal in [&file as &dyn KernelJournal, &memory as &dyn KernelJournal] {
            assert_integrity(
                journal
                    .compare_and_append(OP, None, record(MAX_CHAIN_POSITION, "d-overflow"))
                    .await,
            );
        }
    }

    #[test]
    fn the_three_error_classes_map_onto_the_sdk_error_without_collapsing() {
        let conflict: crate::Error = JournalError::conflict("head moved").into();
        assert!(
            matches!(conflict, crate::Error::Other(ref message) if message.contains("head moved"))
        );

        let integrity: crate::Error = JournalError::integrity("chain broken").into();
        assert!(
            matches!(integrity, crate::Error::Other(ref message) if message.contains("chain broken"))
        );

        let io: crate::Error = JournalError::io(
            "journal could not publish a durable record",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        )
        .into();
        match io {
            crate::Error::Io(err) => {
                assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
                assert!(err.to_string().contains("could not publish"));
            }
            other => panic!("io must stay io, got {other:?}"),
        }
    }
}
