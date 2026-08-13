//! §12.2 · bounded-tail restore.
//!
//! One entry point, [`restore_operation`], running the spec's ladder in the spec's order:
//!
//! ```text
//! load latest checkpoint
//! → verify checkpoint version/digests/operation/genesis/covered head
//! → restore logical state
//! → replay and verify checkpoint tail
//! → load committed records after through_step_seq
//! → verify record chain and step digests
//! → replay bounded post-checkpoint tail
//! → expose pending effects or terminal
//! ```
//!
//! Three properties are load-bearing, and each is a reaction to how the historical resume path
//! failed:
//!
//! 1. **There is no second state machine.** With a checkpoint or without one, the replay goes
//!    through the same `prepare`/`commit` fold every live transition goes through, and the driver
//!    plans every replayed input exactly as it planned it the first time. The retired recovery path
//!    had a separate replay mode, and the two drifted. Here, "restore from genesis" is literally
//!    `restore_operation` with no
//!    checkpoint — [`KernelTransaction::rebuild_from_records`] — and nothing else.
//! 2. **The restore verifies itself.** After the logical state is installed and before a single
//!    tail input is replayed, the restored runtime is re-projected and the projection is digested.
//!    It must equal the checkpoint's `state_digest`. A hydration that forgets a field therefore
//!    produces a refusal instead of a subtly incomplete runtime.
//! 3. **The cost is the tail, not the run.** [`RestoreCost`] counts what was read, and the count is
//!    bounded by the tail bounds plus whatever the journal holds above `through_step_seq` — never
//!    by how long the operation has been running. That is the property §12 exists for, and
//!    `long_run_restore_cost_is_bounded_by_the_tail` measures it rather than asserting it.

use super::checkpoint::{
    CanonicalInput, KernelCheckpoint, LogicalKernelState, LogicalStateProjection,
};
use super::config::ConfigDefaults;
use super::driver::{CanonicalOperationDriver, PlannedStep};
use super::effect::{Digest, KernelEffect};
use super::fault::{KernelFault, KernelFaultCode};
use super::record::{KernelRecord, NormalizedInput, canonical_bytes, canonical_digest};
use super::terminal::KernelTerminal;
use super::transaction::{InMemoryRecordIndex, KernelTransaction, RecordIndex};

/// A canonical runtime: the transaction and the driver that plans for it.
///
/// The pair is what a restore produces, because neither half is a runtime on its own — the
/// transaction decides *whether* an input is accepted and the driver decides *what* it means, and
/// §12.2's "behaves identically to one that was never interrupted" is a claim about both.
pub struct RestoredOperation<Index = InMemoryRecordIndex> {
    pub transaction: KernelTransaction<PlannedStep, Index>,
    pub driver: CanonicalOperationDriver,
    pub cost: RestoreCost,
}

impl<Index: RecordIndex> std::fmt::Debug for RestoredOperation<Index> {
    /// Names what a restore *is* — where it landed and what it cost — without printing a whole
    /// semantic engine into a test failure.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoredOperation")
            .field("head", &self.transaction.head())
            .field("lifecycle", &self.transaction.lifecycle())
            .field("cost", &self.cost)
            .finish()
    }
}

impl<Index: RecordIndex> RestoredOperation<Index> {
    /// §12.2 line 8 · the effects the restored operation is still waiting on.
    ///
    /// Republished on purpose (adjudication §5g-1): a record that reached the journal is a fact,
    /// and the effects its step planned may never have been handed to a host — the crash could have
    /// landed between the append and the publish. Effects are idempotent by `effect_id`, so
    /// re-exposing one the host already ran costs a duplicate resolution that DEC-1 answers with a
    /// `Replayed`; *not* re-exposing one that was never run strands the operation forever.
    pub fn pending_effects(&self) -> Vec<KernelEffect> {
        self.transaction.pending_effects().cloned().collect()
    }

    /// §12.2 line 8 · the terminal, if this operation already ended.
    pub fn terminal(&self) -> Option<&KernelTerminal> {
        self.transaction.terminal()
    }
}

/// What a restore actually read.
///
/// A deterministic counter rather than a timer: the claim "restore is bounded by the tail" is about
/// how much *history* is touched, and a wall-clock benchmark would measure the machine instead. The
/// three counters are the three sources a restore can read from, and `records_before_checkpoint` is
/// the one that must stay zero whenever a checkpoint exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestoreCost {
    /// Journal records read from below the checkpoint's base. Zero with a checkpoint; the whole
    /// journal without one.
    pub records_before_checkpoint: u64,
    /// Canonical inputs replayed out of the checkpoint's own bounded tail — `(base, through]`.
    pub tail_inputs_replayed: u64,
    /// Journal records replayed from above `through_step_seq`.
    pub records_after_checkpoint: u64,
    /// Bytes read across all three, for the byte axis of the tail bound.
    pub bytes_read: u64,
}

impl RestoreCost {
    pub fn total_transitions(&self) -> u64 {
        self.records_before_checkpoint + self.tail_inputs_replayed + self.records_after_checkpoint
    }
}

/// §12.2 · restore one operation from its latest checkpoint plus the records above it.
///
/// `checkpoint = None` is the no-checkpoint arm of §12.2's last line: the fold starts at genesis and
/// runs the *same* path. `records` are the committed records **after** `through_step_seq` when a
/// checkpoint is supplied, and the whole journal when one is not.
pub fn restore_operation<Index: RecordIndex>(
    checkpoint: Option<&KernelCheckpoint>,
    records: &[KernelRecord],
    defaults: ConfigDefaults,
    index: Index,
) -> Result<RestoredOperation<Index>, KernelFault> {
    let Some(checkpoint) = checkpoint else {
        return restore_from_genesis(records, defaults, index);
    };

    // ----- line 2 · verify version/digests/operation/genesis/covered head -----
    //
    // The version halves are enforced by the decoder, which cannot even construct a checkpoint of a
    // revision this kernel does not read; the digest and coverage halves are re-run here because a
    // checkpoint handed over as a value (rather than decoded from bytes) has not passed them yet.
    checkpoint.verify().map_err(|error| error.fault())?;

    // ----- line 3 · restore logical state -----
    let state = checkpoint.logical_state();
    let mut driver =
        CanonicalOperationDriver::restore_logical_state(&state.transition.resolved_config, state)?;
    let mut transaction = KernelTransaction::restore_from_checkpoint(checkpoint, defaults, index)?;

    // The restore verifies itself before it replays anything: if the state that came back does not
    // hash to the state that was captured, the tail would be replayed onto a *different* history
    // and every record it produced would be wrong for a reason no later check could localise.
    verify_restored_state(&transaction, &driver, checkpoint)?;

    let mut cost = RestoreCost::default();

    // ----- line 4 · replay and verify the checkpoint tail -----
    for entry in checkpoint.tail_inputs() {
        cost.tail_inputs_replayed += 1;
        cost.bytes_read += canonical_bytes(&entry.input)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(0);
        replay_tail_entry(&mut transaction, &mut driver, entry)?;
    }

    // ----- lines 5–7 · the post-checkpoint records -----
    replay_records(&mut transaction, &mut driver, records, &mut cost, false)?;

    Ok(RestoredOperation {
        transaction,
        driver,
        cost,
    })
}

/// §12.2 last line · no checkpoint, so the fold starts at genesis — through the same code.
fn restore_from_genesis<Index: RecordIndex>(
    records: &[KernelRecord],
    defaults: ConfigDefaults,
    index: Index,
) -> Result<RestoredOperation<Index>, KernelFault> {
    let mut driver = CanonicalOperationDriver::new();
    let transaction =
        KernelTransaction::rebuild_from_records(records, defaults, index, |context| {
            driver.fold(context)
        })?;
    let cost = RestoreCost {
        records_before_checkpoint: records.len() as u64,
        tail_inputs_replayed: 0,
        records_after_checkpoint: 0,
        bytes_read: records
            .iter()
            .map(|record| record.record_bytes().len() as u64)
            .sum(),
    };
    Ok(RestoredOperation {
        transaction,
        driver,
        cost,
    })
}

/// The self-check of §12.2 line 3: re-project the restored runtime and compare digests.
fn verify_restored_state<Index: RecordIndex>(
    transaction: &KernelTransaction<PlannedStep, Index>,
    driver: &CanonicalOperationDriver,
    checkpoint: &KernelCheckpoint,
) -> Result<(), KernelFault> {
    let reprojected = project(transaction, driver)?;
    let digest = state_digest(&reprojected)?;
    if &digest != checkpoint.state_digest() {
        return Err(KernelFault::new(
            KernelFaultCode::CheckpointCorrupted,
            format!(
                "the restored logical state hashes to {digest}, but the checkpoint at step {} \
                 captured {}; the restore would replay its tail onto a different history",
                checkpoint.base_step_seq(),
                checkpoint.state_digest()
            ),
        ));
    }
    Ok(())
}

/// Project the pair back into the DTO the checkpoint stores. The mirror of a candidate, minus the
/// header — which is what makes the digest comparable.
fn project<Index: RecordIndex>(
    transaction: &KernelTransaction<PlannedStep, Index>,
    driver: &CanonicalOperationDriver,
) -> Result<LogicalKernelState, KernelFault> {
    let LogicalStateProjection {
        root_kind,
        focus,
        syscall,
        scheduler,
        context_vm,
    } = driver.project_logical_state();
    Ok(LogicalKernelState {
        transition: transaction.transition_state_for_restore(root_kind, focus)?,
        syscall,
        scheduler,
        context_vm,
    })
}

fn state_digest(state: &LogicalKernelState) -> Result<Digest, KernelFault> {
    canonical_bytes(state)
        .map(|bytes| canonical_digest(bytes.as_slice()))
        .map_err(|error| {
            KernelFault::new(
                KernelFaultCode::CheckpointCorrupted,
                error.message().to_string(),
            )
        })
}

/// §12.2 line 4 · replay one bounded-tail input and verify it reproduces the record it produced.
fn replay_tail_entry<Index: RecordIndex>(
    transaction: &mut KernelTransaction<PlannedStep, Index>,
    driver: &mut CanonicalOperationDriver,
    entry: &CanonicalInput,
) -> Result<(), KernelFault> {
    replay_one(transaction, driver, &entry.input, &entry.record_digest)
}

/// §12.2 lines 5–7 · fold the committed records above the checkpoint, verifying the chain.
fn replay_records<Index: RecordIndex>(
    transaction: &mut KernelTransaction<PlannedStep, Index>,
    driver: &mut CanonicalOperationDriver,
    records: &[KernelRecord],
    cost: &mut RestoreCost,
    below_checkpoint: bool,
) -> Result<(), KernelFault> {
    for record in records {
        if below_checkpoint {
            cost.records_before_checkpoint += 1;
        } else {
            cost.records_after_checkpoint += 1;
        }
        cost.bytes_read += record.record_bytes().len() as u64;

        let input = record.normalized_input().map_err(|error| {
            KernelFault::new(
                KernelFaultCode::RecordCorrupted,
                error.message().to_string(),
            )
        })?;
        replay_one(transaction, driver, &input, record.record_digest())?;
    }
    Ok(())
}

/// One replayed transition, through the transaction's own replay primitive.
///
/// Both halves of the ladder land here, which is what makes the tail and the journal the *same*
/// fold: the only difference between them is where the expected record digest came from.
fn replay_one<Index: RecordIndex>(
    transaction: &mut KernelTransaction<PlannedStep, Index>,
    driver: &mut CanonicalOperationDriver,
    input: &NormalizedInput,
    expected: &Digest,
) -> Result<(), KernelFault> {
    transaction.replay_committed(input, expected, &mut |context| driver.fold(context))?;
    Ok(())
}
