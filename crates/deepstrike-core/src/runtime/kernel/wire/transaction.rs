//! The one durable transition protocol: prepare → CAS append → commit (spec §8.2, §8.3, §15.2).
//!
//! This module is the state machine that sits between a [`WireEnvelope`] and a
//! [`KernelRecord`]. It owns exactly the decisions that must not be re-implemented per host:
//! whether an input is a replay, whether it may be accepted at all, which record it becomes, and
//! when the effects that record's step planned become visible.
//!
//! Four properties shape the API, and each is meant to be unrepresentable-if-violated rather than
//! merely documented:
//!
//! 1. **Abort's boundary is strictly before the append** (§8.3). There is no state in which a
//!    record is durable *and* still sits in the candidate slot, because [`KernelTransaction::commit`]
//!    is the call a host makes **after** its CAS append succeeded, and it consumes the candidate.
//!    A host that gets an error out of `commit`, or crashes inside it, therefore has no `abort`
//!    to reach for — the transaction poisons itself and the only way forward is
//!    [`KernelTransaction::rebuild_from_records`]. "Append succeeded, commit failed, so we rolled
//!    back" is not a control flow this type can express.
//! 2. **Every rejection is byte-for-byte zero mutation** (§15.2). `prepare` mutates nothing until
//!    the very last statement, which is the one that fills the candidate slot; the tests assert
//!    this by cloning the whole transaction and comparing it after a rejected prepare.
//! 3. **Idempotency is anchored on `input_id` + the durable journal** (DEC-2), never on a bounded
//!    in-memory window. The lookup goes through [`RecordIndex`], whose production implementation
//!    reads the journal; the historical 256-entry FIFO turned "the same delivery arrived twice"
//!    into a fail-closed rejection as soon as the run got long enough.
//! 4. **An already-resolved effect resolves to `Replayed`, never to a second record** (DEC-1).
//!    Reporting that case as `Prepared` while returning the *old* `step_seq` is the live dead end
//!    this protocol exists to remove.
//!
//! What this layer deliberately does not do: it never plans a step itself (the caller passes a
//! planner, and Phase 3/4 supplies the real one), and it never rebuilds itself after a conflict —
//! rebuild/retry is the host-side loop of Task 7b, and this module only offers it a verified
//! entry point.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use serde::Serialize;

use super::checkpoint::{
    AcceptedCancellationState, AcceptedInputState, CanonicalInput, CheckpointCandidate,
    CheckpointDraft, KernelCheckpoint, LaunchTokenState, LogicalKernelState,
    LogicalStateProjection, ResolvedEffectState, TransitionStateV1,
};
use super::command::{CancelCommand, HostCommand};
use super::config::{ConfigDefaults, ResolvedOperationConfig, TailBounds};
use super::effect::{Digest, EffectKind, EffectKindTag, EffectOutcome, KernelEffect, LaunchToken};
use super::envelope::{OperationLifecycle, WireEnvelope, WireRejection, WireRejectionKind};
use super::fault::{
    KernelFault, KernelFaultCode, KernelPreparation, PrepareToken, PreparedTransition,
    RejectedTransition, ReplayedTransition,
};
use super::record::{
    ChainAnchor, KernelRecord, NormalizedInput, NormalizedPayload, RecordError, RecordPreparation,
    canonical_bytes, canonical_digest, verify_record_chain,
};
use super::root::{ExecutionFocus, RootKind};
use super::scalar::{EffectId, InputId, OperationId, WireU64};
use super::terminal::{KernelTerminal, StepDisposition, TerminalSlot};

// ---------------------------------------------------------------------------------------------
// what the transaction needs from a planned step
// ---------------------------------------------------------------------------------------------

/// The one thing the transaction layer needs to know about a planned step.
///
/// It is not "a step is a struct with these fields": the step type belongs to the semantic kernel
/// (Phase 3/4) and travels through here as a generic. What the transaction must see is only what
/// a *commit* publishes, and §7.12 already fixed that shape as [`StepDisposition`] — effects or a
/// terminal, never both.
pub trait TransitionStep: Serialize + Clone {
    fn disposition(&self) -> &StepDisposition;

    fn effects(&self) -> &[KernelEffect] {
        self.disposition().effects()
    }

    fn terminal(&self) -> Option<&KernelTerminal> {
        self.disposition().terminal()
    }
}

// ---------------------------------------------------------------------------------------------
// the idempotency anchor (DEC-2)
// ---------------------------------------------------------------------------------------------

/// Lookup from an `input_id` to the record it already produced.
///
/// This is the **durable** idempotency anchor of §15.2: a production implementation answers from
/// the journal, so a retry stays idempotent no matter how long the operation has been running.
/// The trait exists so the transaction never holds its own replay window — the moment that window
/// is the authority, a long run turns a legitimate retry into `DuplicateInputConflict`.
pub trait RecordIndex {
    /// The record this `input_id` already produced in this operation, if any.
    fn record_for_input(
        &self,
        operation_id: &OperationId,
        input_id: &InputId,
    ) -> Option<KernelRecord>;

    /// Note a record that just became durable.
    ///
    /// A journal-backed index that reads through to storage implements this as a no-op; an index
    /// that caches needs it to stay complete. It is called **after** the host reported a
    /// successful CAS append, never before.
    fn note_committed(&mut self, record: &KernelRecord) {
        let _ = record;
    }
}

/// In-memory [`RecordIndex`], for tests and for hosts whose journal is itself in memory (§8.4).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InMemoryRecordIndex {
    records: BTreeMap<(OperationId, InputId), KernelRecord>,
}

impl InMemoryRecordIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the index from a journal's records — the shape a rebuild starts from.
    pub fn from_records(records: &[KernelRecord]) -> Self {
        let mut index = Self::new();
        for record in records {
            index.note_committed(record);
        }
        index
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl RecordIndex for InMemoryRecordIndex {
    fn record_for_input(
        &self,
        operation_id: &OperationId,
        input_id: &InputId,
    ) -> Option<KernelRecord> {
        self.records
            .get(&(operation_id.clone(), input_id.clone()))
            .cloned()
    }

    fn note_committed(&mut self, record: &KernelRecord) {
        self.records.insert(
            (record.operation_id().clone(), record.input_id().clone()),
            record.clone(),
        );
    }
}

// ---------------------------------------------------------------------------------------------
// §12.3 · the bounded tail
// ---------------------------------------------------------------------------------------------

/// How much tail the operation is carrying since its last acked checkpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TailUsage {
    pub records: u64,
    pub bytes: u64,
}

/// Where the tail sits against its bounds. `Full` is not a latch — an acked checkpoint moves it
/// straight back to `Nominal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailPressure {
    Nominal,
    /// Past the soft watermark: the host should take a checkpoint candidate soon.
    Watermark,
    /// At the hard limit: the next prepare is rejected with a retryable `CheckpointRequired`.
    Full,
}

/// One journal record still inside the tail: what a checkpoint candidate needs to carry it, and
/// what an ack needs to reclaim it.
///
/// The normalised input is kept because §12.1's `tail_inputs` is exactly this sequence — a
/// checkpoint that stored only digests could be *verified* but never *replayed*, which is the half
/// of §12.2 that makes a bounded-tail restore cheaper than a full journal fold.
#[derive(Debug, Clone, PartialEq)]
struct TailEntry {
    step_seq: WireU64,
    record_digest: Digest,
    bytes: u64,
    input: NormalizedInput,
}

// ---------------------------------------------------------------------------------------------
// observable transaction shapes
// ---------------------------------------------------------------------------------------------

/// The journal head this runtime believes in: the CAS precondition of the next append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableHead {
    pub digest: Digest,
    pub step_seq: WireU64,
}

/// The prefix a checkpoint would cover (§12.3).
///
/// Derived from the **durable head only**: an outstanding transaction candidate neither moves this
/// boundary nor is blocked by it, which is §22.14's rejection of "checkpoint install requires the
/// candidate head to still be the current head" expressed as a data dependency instead of a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointBoundary {
    pub through_step_seq: WireU64,
    pub covered_head: Digest,
}

/// What the planner is given. Everything it needs to decide, and nothing that would let it depend
/// on the host's wall clock or on a default that may drift between binaries: `config` is the
/// configuration this operation *froze in its genesis record*.
#[derive(Debug)]
pub struct PlanContext<'a> {
    pub input: &'a NormalizedInput,
    pub step_seq: WireU64,
    pub previous_head: Option<&'a Digest>,
    pub config: &'a ResolvedOperationConfig,
    /// The pending effect a `ResolveEffect` input answers — the very effect this kernel published,
    /// already checked against §15.3 (still pending, kind matches the outcome, not a conflicting
    /// duplicate). `None` for every other input class.
    ///
    /// The planner needs it because a **failure** outcome carries no kind: §7.9's cross-effect
    /// `HostEffectFailure` is deliberately kind-agnostic, so the one policy decision DEC-5 allows
    /// has to be looked up from what the kernel asked for, never from what the host echoed back.
    pub resolving: Option<&'a KernelEffect>,
}

/// One durable transition, after the host's append and this runtime's commit.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedTransition<Step> {
    pub record: KernelRecord,
    pub step: Step,
    pub step_seq: WireU64,
    /// §12.3 · set on exactly the commit that carries the tail past its soft watermark.
    ///
    /// **Edge-triggered, not level-triggered.** A level-triggered flag would be set on every commit
    /// between the watermark and the hard limit, which is precisely the window in which the host is
    /// already taking a checkpoint — so it would arrive as noise at the moment it stopped being
    /// news. Fired once per crossing, it is a fact: "the tail just went over".
    ///
    /// The host projects it into a §7.11 observation (Phase 6). The kernel does not push it,
    /// because a transition publishes effects or a terminal (§7.12) and advice is neither.
    pub checkpoint_advice: Option<CheckpointAdvice>,
}

/// The soft-watermark crossing of §12.3, with the numbers that justify it.
///
/// It is advice and nothing more: no state moves, no input is refused, and ignoring it costs
/// exactly one `CheckpointRequired` rejection later — the retryable one. That is the difference
/// between this and the overflow latch it replaces, which turned "the tail got long" into a
/// permanent refusal with no way back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointAdvice {
    pub through_step_seq: WireU64,
    pub usage: TailUsage,
    pub bounds: TailBounds,
}

impl<Step: TransitionStep> CommittedTransition<Step> {
    /// The effects this commit made visible. Before the commit there were none — §15.2's "an
    /// effect is not visible before its record is durable" is why they are published from here
    /// rather than from `prepare`.
    pub fn published_effects(&self) -> &[KernelEffect] {
        self.step.effects()
    }

    pub fn terminal(&self) -> Option<&KernelTerminal> {
        self.step.terminal()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Candidate<Step> {
    token: PrepareToken,
    record: KernelRecord,
    step: Step,
    input: NormalizedInput,
    record_bytes: u64,
}

/// The envelope facts a prepare reads, once the payload has been normalised.
///
/// Not a second envelope type: the three scalars plus the lifecycle table are everything the guards
/// below `prepare_normalized_inner` consult, and naming them keeps the live path and the restore
/// path running the *same* guards instead of two similar ones.
struct InputFacts {
    operation_id: OperationId,
    input_id: InputId,
    observed_at_ms: WireU64,
    admissible_lifecycles: &'static [OperationLifecycle],
}

#[derive(Debug, Clone, PartialEq)]
struct ResolvedEffectRecord {
    outcome_digest: Digest,
    input_id: InputId,
    step_seq: WireU64,
}

// ---------------------------------------------------------------------------------------------
// the transaction
// ---------------------------------------------------------------------------------------------

/// The prepare/commit/abort state machine of §8.2.
///
/// One operation, one transaction, one candidate slot. Everything durable lives in the journal
/// behind [`RecordIndex`]; what is held here is the fold of that journal that the next transition
/// needs — head, lifecycle, pending effects, launch tokens and the ephemeral steps the records
/// only carry a digest of.
#[derive(Debug, Clone, PartialEq)]
pub struct KernelTransaction<Step, Index> {
    defaults: ConfigDefaults,
    bounds: TailBounds,
    index: Index,
    operation_id: Option<OperationId>,
    /// §12.1 · the operation's identity. The genesis record's digest, kept because a checkpoint
    /// binds itself to it and the genesis record itself may be pruned once a checkpoint covers it.
    genesis_digest: Option<Digest>,
    config: Option<ResolvedOperationConfig>,
    /// The chain anchor of the durable head — *not* the head record.
    ///
    /// §12.2: a runtime restored onto an acked checkpoint may have no head record to hold, because
    /// the record the next append chains onto is exactly the one retention was allowed to reclaim.
    /// What survives is the anchor, and the anchor is all a successor ever read.
    head: Option<ChainAnchor>,
    lifecycle: OperationLifecycle,
    terminal: TerminalSlot,
    candidate: Option<Candidate<Step>>,
    prepare_epoch: u64,
    last_observed_at_ms: WireU64,
    /// `input_id` → the step that record committed.
    ///
    /// Steps are never durable (§22.12), so this is the only place a committed step exists. It is
    /// repopulated wholesale by [`KernelTransaction::rebuild_from_records`], which is what keeps
    /// `Replayed` answerable after a crash without the journal storing a derived planned step.
    ///
    /// Deliberately **not** a bounded window: the bounded window is the thing DEC-2 removed, and a
    /// miss here cannot be answered with a fabricated replay — it is reported as
    /// [`KernelFaultCode::RecordCorrupted`] ("rebuild first"). Bounding it is Phase 5's problem
    /// and needs a spec decision first, because §12.3 rule 7 forbids clearing the replay window at
    /// checkpoint ack while §7.13 requires `Replayed` to carry the committed step.
    steps: BTreeMap<InputId, Step>,
    /// §12.3 rule 7 · the replay ledger: every `input_id` this operation ever accepted, with the
    /// step and record digest it produced.
    ///
    /// Separate from `steps` because the two answer different questions and survive differently. A
    /// step is a process-local artefact; a ledger entry is a *fact* the checkpoint carries, so it
    /// survives both a restore and — once the prefix is reclaimed — the disappearance of the record
    /// itself. An ack must never empty it (rule 7); nothing here does.
    accepted: BTreeMap<InputId, AcceptedInputState>,
    /// §12.3 rule 10 · the step below which this runtime answers a replay by reference.
    ///
    /// `None` for a runtime that folded its whole history. `Some(base)` after a restore: at or
    /// below `base` the steps were never durable and are not reproducible, so a redelivery is
    /// acknowledged rather than re-answered with a step this process would have to invent.
    replay_floor: Option<WireU64>,
    pending_effects: BTreeMap<EffectId, KernelEffect>,
    resolved_effects: BTreeMap<EffectId, ResolvedEffectRecord>,
    launch_tokens: BTreeMap<LaunchToken, WireU64>,
    /// §18.3 · the cancellation this operation already accepted, if any.
    ///
    /// Cancel is the one *command* with an effect-level dedup branch, and it needs one for the
    /// same reason a resolution does: a caller that does not hear the answer retries, and a retry
    /// that mints a fresh `input_id` would otherwise land on the terminal latch this very command
    /// created and come back as `InvalidLifecycle` — telling the caller its cancel failed when it
    /// is exactly what succeeded.
    accepted_cancellation: Option<AcceptedCancellation>,
    tail: VecDeque<TailEntry>,
    poison: Option<KernelFault>,
}

/// What an accepted cancellation has to remember to answer a redelivery (§18.3).
#[derive(Debug, Clone, PartialEq)]
struct AcceptedCancellation {
    /// Digest of the canonical `CancelCommand`, so "the same cancellation" is decided by bytes
    /// rather than by field-by-field comparison that a new field could silently fall out of.
    command_digest: Digest,
    input_id: InputId,
    step_seq: WireU64,
}

impl<Step, Index> KernelTransaction<Step, Index>
where
    Step: TransitionStep,
    Index: RecordIndex,
{
    /// A transaction with no journal yet.
    ///
    /// It starts on the *bootstrap* tail bound — `defaults.baseline.recovery_policy.tail_bounds` —
    /// because the genesis append has to be bounded by something and the operation's own
    /// configuration is not resolved until that very record. The moment genesis commits, the
    /// frozen value replaces it (§5e-5).
    pub fn new(defaults: ConfigDefaults, index: Index) -> Self {
        let bounds = defaults.baseline.recovery_policy.tail_bounds;
        Self {
            defaults,
            bounds,
            index,
            operation_id: None,
            genesis_digest: None,
            config: None,
            head: None,
            lifecycle: OperationLifecycle::Created,
            terminal: TerminalSlot::empty(),
            candidate: None,
            prepare_epoch: 0,
            last_observed_at_ms: WireU64::ZERO,
            steps: BTreeMap::new(),
            accepted: BTreeMap::new(),
            replay_floor: None,
            pending_effects: BTreeMap::new(),
            resolved_effects: BTreeMap::new(),
            launch_tokens: BTreeMap::new(),
            accepted_cancellation: None,
            tail: VecDeque::new(),
            poison: None,
        }
    }

    // ----- §8.2 line 3–4 · prepare -----

    /// Normalise, validate and plan one input (§8.2 lines 3–4).
    ///
    /// Returns the closed three-arm result of §7.13. Nothing in this call touches the journal: a
    /// `Prepared` result means a record was *built*, and it becomes durable only when the host
    /// appends it and calls [`Self::commit`].
    ///
    /// Every path that does not return `Prepared` leaves this transaction byte-for-byte
    /// unchanged, including the ones that ran the planner.
    pub fn prepare<F>(&mut self, envelope: &WireEnvelope, plan: F) -> RecordPreparation<Step>
    where
        F: FnOnce(&PlanContext<'_>) -> Result<Step, KernelFault>,
    {
        match self.prepare_inner(envelope, plan) {
            Ok(preparation) => preparation,
            Err(fault) => KernelPreparation::Rejected(RejectedTransition { fault }),
        }
    }

    fn prepare_inner<F>(
        &mut self,
        envelope: &WireEnvelope,
        plan: F,
    ) -> Result<RecordPreparation<Step>, KernelFault>
    where
        F: FnOnce(&PlanContext<'_>) -> Result<Step, KernelFault>,
    {
        self.check_preparable(&envelope.operation_id)?;
        let input =
            NormalizedInput::normalize(envelope, &self.defaults).map_err(rejection_fault)?;
        self.prepare_normalized_inner(input, plan)
    }

    /// The guards every prepare runs before it looks at the input at all.
    fn check_preparable(&self, operation_id: &OperationId) -> Result<(), KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }

        // One candidate at a time (§8.2 is a linear protocol). A second concurrent prepare is not
        // queued and not silently allowed to displace the first: displacing it would strand a
        // record the host may already be appending.
        if let Some(candidate) = &self.candidate {
            return Err(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!(
                    "transaction candidate {} is still outstanding at step {}; commit or abort it \
                     before preparing another input",
                    candidate.token,
                    candidate.record.step_seq()
                ),
            ));
        }

        if let Some(bound) = &self.operation_id
            && bound != operation_id
        {
            return Err(KernelFault::new(
                KernelFaultCode::OperationMismatch,
                format!(
                    "input belongs to operation {operation_id}, but this runtime is bound to \
                     {bound}"
                ),
            ));
        }
        Ok(())
    }

    fn prepare_normalized_inner<F>(
        &mut self,
        input: NormalizedInput,
        plan: F,
    ) -> Result<RecordPreparation<Step>, KernelFault>
    where
        F: FnOnce(&PlanContext<'_>) -> Result<Step, KernelFault>,
    {
        self.check_preparable(&input.operation_id)?;
        let envelope = InputFacts {
            operation_id: input.operation_id.clone(),
            input_id: input.input_id.clone(),
            observed_at_ms: input.observed_at_ms,
            admissible_lifecycles: input.input.admissible_lifecycles(),
        };
        let envelope = &envelope;
        let canonical_input = canonical_bytes(&input).map_err(record_fault)?;

        // Exact replay is answered **before** the clock and lifecycle checks: a retry of an input
        // the journal already holds is not a new decision, so it cannot become a new refusal.
        if let Some(existing) = self
            .index
            .record_for_input(&envelope.operation_id, &envelope.input_id)
        {
            if existing.canonical_input() != &canonical_input {
                return Err(KernelFault::new(
                    KernelFaultCode::DuplicateInputConflict,
                    format!(
                        "input {} was already accepted at step {} with a different payload",
                        envelope.input_id,
                        existing.step_seq()
                    ),
                ));
            }
            return self.replay_of(existing);
        }

        // §12.3 rules 6–7 and 10 · the ledger outlives the record. Once an acked checkpoint's
        // prefix has been reclaimed the journal can no longer answer for those inputs, and without
        // this branch a redelivery down there would be accepted a *second* time — the exact failure
        // "an ack must not empty the replay window" names, arriving through retention instead of
        // through a clear.
        if let Some(entry) = self.accepted.get(&envelope.input_id)
            && self.below_replay_floor(entry.step_seq)
        {
            return Ok(self.replay_by_reference(entry));
        }

        if let Some(config) = &self.config
            && canonical_input.len() > config.kernel_limits.max_input_bytes as usize
        {
            return Err(KernelFault::new(
                KernelFaultCode::ResourceLimitExceeded,
                format!(
                    "canonical input carries {} bytes; the operation limit is {}",
                    canonical_input.len(),
                    config.kernel_limits.max_input_bytes
                ),
            ));
        }

        if envelope.observed_at_ms.get() < self.last_observed_at_ms.get() {
            return Err(KernelFault::new(
                KernelFaultCode::ClockRegression,
                format!(
                    "input observed at {} precedes the last accepted input at {}",
                    envelope.observed_at_ms, self.last_observed_at_ms
                ),
            ));
        }

        // §18.3 · the cancel dedup branch, isomorphic with effect-level dedup and decided **before**
        // the lifecycle check on purpose: the terminal it would be refused by is the one this very
        // cancellation committed.
        if let NormalizedPayload::HostControl(control) = &input.input
            && let HostCommand::Cancel(cancel) = &control.command
            && let Some(preparation) = self.cancel_guard(cancel)?
        {
            return Ok(preparation);
        }

        if !envelope.admissible_lifecycles.contains(&self.lifecycle) {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                format!(
                    "a {} input is not admissible while the operation is {:?}",
                    input.input.kind(),
                    self.lifecycle
                ),
            ));
        }

        // DEC-1 · effect-level dedup and the fail-closed resolution rules of §15.3.
        if let NormalizedPayload::ResolveEffect(resolve) = &input.input
            && let Some(preparation) = self.resolve_effect_guard(resolve)?
        {
            return Ok(preparation);
        }

        if self.tail_records() + 1 > self.bounds.hard_records.get() {
            return Err(self.checkpoint_required(format!(
                "the journal tail already holds {} records; its hard limit is {}",
                self.tail_records(),
                self.bounds.hard_records
            )));
        }

        let step_seq = self.next_step_seq()?;
        let genesis_config = input.resolved_config().cloned();
        let config = match (&genesis_config, &self.config) {
            (Some(config), _) => config,
            (None, Some(config)) => config,
            (None, None) => {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidLifecycle,
                    "the operation has no genesis record, so it has no configuration to plan \
                     against"
                        .to_string(),
                ));
            }
        };

        let settled = match &input.input {
            NormalizedPayload::ResolveEffect(resolve) => Some(&resolve.effect_id),
            _ => None,
        };
        let resolving = settled.and_then(|effect_id| self.pending_effects.get(effect_id));
        let step = plan(&PlanContext {
            input: &input,
            step_seq,
            previous_head: self.head.as_ref().map(|anchor| &anchor.record_digest),
            config,
            resolving,
        })?;

        self.screen_planned_effects(&step, config, settled)?;

        let record =
            KernelRecord::chain_after(self.head.as_ref(), &input, &step).map_err(record_fault)?;
        let record_bytes = record.record_bytes().len() as u64;
        if self.tail_bytes() + record_bytes > self.bounds.hard_bytes.get() {
            return Err(self.checkpoint_required(format!(
                "the journal tail holds {} bytes and this record adds {record_bytes}; \
                 the hard limit is {}",
                self.tail_bytes(),
                self.bounds.hard_bytes
            )));
        }

        // ----- the first and only mutation of a successful prepare -----
        self.prepare_epoch += 1;
        let token = PrepareToken::new(format!(
            "{}:prepare:{step_seq}:{}",
            envelope.operation_id, self.prepare_epoch
        ))
        .expect("an operation-scoped prepare token is always a legal branded ref");
        self.candidate = Some(Candidate {
            token: token.clone(),
            record: record.clone(),
            step: step.clone(),
            input,
            record_bytes,
        });

        Ok(KernelPreparation::Prepared(PreparedTransition {
            token,
            record,
            planned_step: step,
        }))
    }

    // ----- §8.2 line 6 · commit -----

    /// Commit the candidate the host **has already appended** (§8.2 lines 5–6).
    ///
    /// `appended_head` is the journal's head after the CAS append; it must be this candidate's own
    /// record digest. There is no separate "the append succeeded" call, and that is the point:
    /// between the append and this call the kernel holds no state that could be rolled back, so
    /// §8.3's "append succeeded, commit failed ⇒ discard the runtime and rebuild" is the only
    /// expressible outcome. Any failure here poisons the transaction — a poisoned transaction
    /// refuses every later call and must be replaced via [`Self::rebuild_from_records`].
    pub fn commit(
        &mut self,
        token: &PrepareToken,
        appended_head: &Digest,
    ) -> Result<CommittedTransition<Step>, KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }

        let Some(candidate) = self.candidate.take() else {
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!(
                    "commit reports a durable append for token {token}, but no candidate is \
                     outstanding; a committed record is never re-committed and never aborted"
                ),
            )));
        };

        if &candidate.token != token {
            let outstanding = candidate.token.clone();
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!(
                    "commit names token {token}, but the outstanding candidate is {outstanding}; \
                     the runtime no longer describes what the journal holds"
                ),
            )));
        }

        if appended_head != candidate.record.record_digest() {
            let expected = candidate.record.record_digest().clone();
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!(
                    "the journal head after the append is {appended_head}, but this candidate is \
                     {expected}; the append did not place this record, so the runtime must be \
                     rebuilt from the journal"
                ),
            )));
        }

        let Candidate {
            record,
            step,
            input,
            record_bytes,
            ..
        } = candidate;
        match self.integrate(record, step, &input, record_bytes) {
            Ok(committed) => Ok(committed),
            Err(fault) => Err(self.poison_with(fault)),
        }
    }

    // ----- §8.3 line 3–4 · abort, strictly before the append -----

    /// Discard a candidate the host has **not** appended (§8.3 line 3).
    ///
    /// The only legal abort window. It is not reachable after a successful append because
    /// [`Self::commit`] consumes the candidate, so "we appended, then something threw, so we
    /// aborted" cannot be written against this API.
    pub fn abort(&mut self, token: &PrepareToken) -> Result<KernelRecord, KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }

        let Some(candidate) = &self.candidate else {
            return Err(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!(
                    "no candidate is outstanding for token {token}; a record that reached the \
                     journal is never abortable"
                ),
            ));
        };
        if &candidate.token != token {
            return Err(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!(
                    "token {token} does not name the outstanding candidate {}",
                    candidate.token
                ),
            ));
        }

        let candidate = self.candidate.take().expect("checked just above");
        Ok(candidate.record)
    }

    /// The host's CAS append failed its precondition (§8.3 line 4).
    ///
    /// Discards the candidate — legal, because a failed CAS wrote nothing — and then fails closed:
    /// the journal moved under this runtime, so every later call is refused until the host runs
    /// the rebuild/retry loop. This layer never rebuilds itself; deciding to re-read the head and
    /// replay the input is the host-side closure of Task 7b.
    pub fn note_append_conflict(
        &mut self,
        token: &PrepareToken,
        observed_head: Option<&Digest>,
    ) -> KernelFault {
        let expected = self
            .candidate
            .as_ref()
            .and_then(|candidate| candidate.record.expected_head().cloned());
        self.candidate = None;
        let observed =
            observed_head.map_or_else(|| "an empty journal".to_string(), Digest::to_string);
        let expected =
            expected.map_or_else(|| "an empty journal".to_string(), |head| head.to_string());
        let fault = KernelFault::new(
            KernelFaultCode::TransactionConflict,
            format!(
                "the CAS append for token {token} expected head {expected} but the journal holds \
                 {observed}; the candidate is discarded and this runtime must be rebuilt from the \
                 journal before the input is replayed"
            ),
        );
        self.poison_with(fault)
    }

    // ----- §8.3 lines 5–6 · rebuild -----

    /// Rebuild a transaction from a journal's records (§8.3 lines 5–6, §12.2).
    ///
    /// Every record is re-planned through `plan` and the resulting record is rebuilt and compared
    /// with the stored one, so a rebuild proves three things at once: the chain links up, the
    /// planner is still deterministic, and the step digest the journal froze is the step this
    /// binary produces. Anything else is [`KernelFaultCode::RecordCorrupted`] — fail closed rather
    /// than resume on a history this binary cannot reproduce.
    pub fn rebuild_from_records<F>(
        records: &[KernelRecord],
        defaults: ConfigDefaults,
        index: Index,
        mut plan: F,
    ) -> Result<Self, KernelFault>
    where
        F: FnMut(&PlanContext<'_>) -> Result<Step, KernelFault>,
    {
        let mut transaction = Self::new(defaults, index);
        if records.is_empty() {
            return Ok(transaction);
        }
        verify_record_chain(records).map_err(corrupt_chain_fault)?;

        for record in records {
            let input = record.normalized_input().map_err(record_fault)?;
            transaction.replay_committed(&input, record.record_digest(), &mut plan)?;
        }
        Ok(transaction)
    }

    /// Replay one already-committed transition onto this runtime (§8.3 lines 5–6, §12.2 lines 4–7).
    ///
    /// **The** replay primitive: `rebuild_from_records` is a loop over it, and so is §12.2's tail
    /// replay. That is what makes "there is no second resume state machine" a structural fact rather
    /// than a claim — a checkpoint's `tail_inputs` and a journal's records reach the fold through the
    /// same function, differing only in where the expected digest came from.
    ///
    /// It deliberately does **not** go through `prepare`. A record that is already durable is not a
    /// new decision: `prepare`'s first move is to ask the index whether this `input_id` was already
    /// accepted, and during a replay the honest answer is "yes, by the very record we are replaying"
    /// — which would turn the fold into a `Replayed` and quietly skip it. Re-planning and comparing
    /// digests is the stronger check anyway: it proves the chain links up, the planner is still
    /// deterministic, and the step digest the journal froze is the step this binary produces.
    pub fn replay_committed<F>(
        &mut self,
        input: &NormalizedInput,
        expected_record_digest: &Digest,
        plan: &mut F,
    ) -> Result<KernelRecord, KernelFault>
    where
        F: FnMut(&PlanContext<'_>) -> Result<Step, KernelFault>,
    {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        let step_seq = self.next_step_seq()?;
        let genesis_config = input.resolved_config().cloned();
        let config = match (&genesis_config, &self.config) {
            (Some(config), _) => config,
            (None, Some(config)) => config,
            (None, None) => {
                return Err(KernelFault::new(
                    KernelFaultCode::RecordCorrupted,
                    format!(
                        "the transition at step {step_seq} has no genesis configuration before it"
                    ),
                ));
            }
        };

        // A replay re-runs the same effects in the same order, so the pending set here is the set
        // the original prepare saw — the planner reads its resolution target from the same place
        // either way.
        let resolving = match &input.input {
            NormalizedPayload::ResolveEffect(resolve) => {
                self.pending_effects.get(&resolve.effect_id)
            }
            _ => None,
        };
        let step = plan(&PlanContext {
            input,
            step_seq,
            previous_head: self.head.as_ref().map(|anchor| &anchor.record_digest),
            config,
            resolving,
        })?;

        let rebuilt = KernelRecord::chain_after(self.head.as_ref(), input, &step)
            .map_err(corrupt_chain_fault)?;
        if rebuilt.record_digest() != expected_record_digest {
            return Err(KernelFault::new(
                KernelFaultCode::RecordCorrupted,
                format!(
                    "replaying the transition at step {step_seq} produced record digest {} against \
                     the durable {expected_record_digest}; this binary does not reproduce the \
                     history it is resuming",
                    rebuilt.record_digest(),
                ),
            ));
        }

        let bytes = rebuilt.record_bytes().len() as u64;
        self.integrate(rebuilt.clone(), step, input, bytes)?;
        Ok(rebuilt)
    }

    // ----- §12.2 · restore -----

    /// Rebuild a transaction from a checkpoint's logical state (§12.2 line 3).
    ///
    /// This is the transaction half of the §12.2 ladder; the driver half is
    /// [`CanonicalOperationDriver::restore_logical_state`](super::driver::CanonicalOperationDriver::restore_logical_state)
    /// and the two are composed by [`restore_operation`](super::restore::restore_operation), which
    /// is also what verifies the result. Nothing is replayed here: the returned transaction sits at
    /// `base_step_seq`, and the caller replays the bounded tail and then the post-checkpoint
    /// records onto it through the ordinary `prepare`/`commit` fold.
    ///
    /// The cost is `O(1)` in the length of the run — that is the whole of §12's claim. What makes
    /// it sound is that everything a transaction decides with is *in* the checkpoint: the frozen
    /// configuration, the effect ledger, the replay ledger, the cancellation and the terminal.
    pub fn restore_from_checkpoint(
        checkpoint: &KernelCheckpoint,
        defaults: ConfigDefaults,
        index: Index,
    ) -> Result<Self, KernelFault> {
        let state = checkpoint.logical_state();
        let transition = &state.transition;
        let mut transaction = Self::new(defaults, index);

        transaction.operation_id = Some(checkpoint.operation_id().clone());
        transaction.genesis_digest = Some(checkpoint.genesis_digest().clone());
        transaction.bounds = transition.resolved_config.recovery_policy.tail_bounds;
        transaction.config = Some(transition.resolved_config.clone());
        transaction.head = Some(ChainAnchor {
            operation_id: checkpoint.operation_id().clone(),
            step_seq: checkpoint.base_step_seq(),
            record_digest: checkpoint.base_record_digest().clone(),
        });
        transaction.lifecycle = transition.lifecycle;
        transaction.last_observed_at_ms = transition.last_observed_at_ms;
        transaction.replay_floor = Some(checkpoint.base_step_seq());

        if let Some(terminal) = &transition.terminal {
            transaction
                .terminal
                .commit(terminal.clone())
                .map_err(|error| {
                    KernelFault::new(KernelFaultCode::CheckpointCorrupted, error.to_string())
                })?;
        }
        transaction.pending_effects = transition
            .pending_effects
            .iter()
            .map(|effect| (effect.effect_id.clone(), effect.clone()))
            .collect();
        transaction.resolved_effects = transition
            .resolved_effects
            .iter()
            .map(|resolved| {
                (
                    resolved.effect_id.clone(),
                    ResolvedEffectRecord {
                        outcome_digest: resolved.outcome_digest.clone(),
                        input_id: resolved.input_id.clone(),
                        step_seq: resolved.step_seq,
                    },
                )
            })
            .collect();
        transaction.launch_tokens = transition
            .launch_tokens
            .iter()
            .map(|token| (token.launch_token.clone(), token.step_seq))
            .collect();
        transaction.accepted = transition
            .accepted_inputs
            .iter()
            .map(|entry| (entry.input_id.clone(), entry.clone()))
            .collect();
        transaction.accepted_cancellation =
            transition
                .accepted_cancellation
                .as_ref()
                .map(|cancellation| AcceptedCancellation {
                    command_digest: cancellation.command_digest.clone(),
                    input_id: cancellation.input_id.clone(),
                    step_seq: cancellation.step_seq,
                });
        Ok(transaction)
    }

    // ----- §12.3 · checkpoint boundary -----

    /// The prefix a checkpoint candidate taken right now would cover.
    ///
    /// Independent of the transaction candidate slot in both directions (§22.14): taking this does
    /// not require an empty slot, and an outstanding candidate does not move the boundary.
    pub fn checkpoint_boundary(&self) -> Option<CheckpointBoundary> {
        self.head.as_ref().map(|head| CheckpointBoundary {
            through_step_seq: head.step_seq,
            covered_head: head.record_digest.clone(),
        })
    }

    /// The canonical inputs the tail still carries, in step order (§12.1 `tail_inputs`).
    ///
    /// Exactly the range `(last acked checkpoint, head]`. A checkpoint whose `base_step_seq` sits
    /// further back needs the host to have kept the older logical state — that is the rebase half
    /// of §12.3, which Task 16 owns.
    pub fn tail_inputs(&self) -> Vec<CanonicalInput> {
        self.tail
            .iter()
            .map(|entry| CanonicalInput {
                step_seq: entry.step_seq,
                record_digest: entry.record_digest.clone(),
                input: entry.input.clone(),
            })
            .collect()
    }

    /// Assemble a checkpoint candidate over the current durable head (§12.3, first half).
    ///
    /// Generation only. It installs nothing, acks nothing and reclaims nothing — and it leaves the
    /// transaction untouched, which is what makes §12.3 rule 1 ("appends may continue after a
    /// candidate") true by construction rather than by discipline: `&self`, no candidate slot, no
    /// tail mutation.
    ///
    /// The candidate is a **full-state** checkpoint: `base_step_seq == through_step_seq`, so its
    /// bounded tail is empty and a restore needs no replay before the post-checkpoint records.
    /// [`Self::checkpoint_rebase`] produces the incremental form.
    pub fn checkpoint_candidate(
        &self,
        projection: LogicalStateProjection,
    ) -> Result<CheckpointCandidate, KernelFault> {
        let head = self.require_head()?.clone();
        self.assemble_checkpoint(
            projection,
            head.step_seq,
            head.record_digest.clone(),
            head.step_seq,
            head.record_digest,
            Vec::new(),
        )
    }

    /// The **rebase** form of §12.3 rule 11: an older logical state plus the canonical inputs that
    /// carry it forward to the current head.
    ///
    /// `base` is a checkpoint boundary this runtime already produced — in practice the last one the
    /// host installed. The tail is [`Self::tail_inputs`] restricted to `(base, head]`, taken from
    /// the transaction's own accounting rather than re-harvested from the journal, so a rebase can
    /// be built after the prefix it rebases onto has been reclaimed.
    ///
    /// Why it exists at all: a full-state candidate re-serialises the whole logical state every
    /// time, and a long run with a big context pays that cost per checkpoint. A rebase pays it once
    /// and then appends bounded tails. The contract that makes the two interchangeable is that they
    /// produce the **same `state_digest`** for the same logical state — the header and the tail
    /// move, the state does not.
    pub fn checkpoint_rebase(
        &self,
        base: &CheckpointBoundary,
        base_state: LogicalKernelState,
    ) -> Result<CheckpointCandidate, KernelFault> {
        let head = self.require_head()?.clone();
        if base.through_step_seq > head.step_seq {
            return Err(KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!(
                    "a rebase bases at step {} but this journal's head is step {}",
                    base.through_step_seq, head.step_seq
                ),
            ));
        }
        let tail: Vec<CanonicalInput> = self
            .tail_inputs()
            .into_iter()
            .filter(|entry| entry.step_seq.get() > base.through_step_seq.get())
            .collect();
        if tail.len() as u64 != head.step_seq.get() - base.through_step_seq.get() {
            return Err(KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!(
                    "a rebase over ({}, {}] needs {} tail inputs, but this runtime's tail holds \
                     {} of them — the prefix it would rebase onto was already reclaimed",
                    base.through_step_seq,
                    head.step_seq,
                    head.step_seq.get() - base.through_step_seq.get(),
                    tail.len()
                ),
            ));
        }

        let operation_id = self.require_operation()?;
        let genesis_digest = self.require_genesis()?;
        let checkpoint = KernelCheckpoint::assemble(CheckpointDraft {
            operation_id: operation_id.clone(),
            genesis_digest: genesis_digest.clone(),
            base_step_seq: base.through_step_seq,
            base_record_digest: base.covered_head.clone(),
            through_step_seq: head.step_seq,
            covered_transaction_head_digest: head.record_digest,
            logical_state: base_state,
            tail_inputs: tail,
        })
        .map_err(|error| error.fault())?;
        Ok(checkpoint.into_candidate())
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble_checkpoint(
        &self,
        projection: LogicalStateProjection,
        base_step_seq: WireU64,
        base_record_digest: Digest,
        through_step_seq: WireU64,
        covered_transaction_head_digest: Digest,
        tail_inputs: Vec<CanonicalInput>,
    ) -> Result<CheckpointCandidate, KernelFault> {
        let operation_id = self.require_operation()?.clone();
        let genesis_digest = self.require_genesis()?.clone();
        let LogicalStateProjection {
            root_kind,
            focus,
            syscall,
            scheduler,
            context_vm,
        } = projection;
        let logical_state = LogicalKernelState {
            transition: self.transition_state(root_kind, focus)?,
            syscall,
            scheduler,
            context_vm,
        };
        let checkpoint = KernelCheckpoint::assemble(CheckpointDraft {
            operation_id,
            genesis_digest,
            base_step_seq,
            base_record_digest,
            through_step_seq,
            covered_transaction_head_digest,
            logical_state,
            tail_inputs,
        })
        .map_err(|error| error.fault())?;
        Ok(checkpoint.into_candidate())
    }

    fn require_head(&self) -> Result<&ChainAnchor, KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        self.head.as_ref().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "an operation with no genesis record has no logical state to checkpoint"
                    .to_string(),
            )
        })
    }

    fn require_operation(&self) -> Result<&OperationId, KernelFault> {
        self.operation_id.as_ref().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "an unbound operation has no logical state to checkpoint".to_string(),
            )
        })
    }

    fn require_genesis(&self) -> Result<&Digest, KernelFault> {
        self.genesis_digest.as_ref().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "an operation with no genesis record has no identity to bind a checkpoint to"
                    .to_string(),
            )
        })
    }

    /// §12.2 · the transition partition, for a restore's own re-projection.
    ///
    /// Public because the restore has to be able to ask "what does this runtime say its transition
    /// state is" without going through `checkpoint_candidate`, which would build a whole checkpoint
    /// header it is about to throw away.
    pub fn transition_state_for_restore(
        &self,
        root_kind: Option<RootKind>,
        focus: Option<ExecutionFocus>,
    ) -> Result<TransitionStateV1, KernelFault> {
        self.transition_state(root_kind, focus)
    }

    /// §12.1 · the transition partition, as of the durable head.
    ///
    /// Everything here is transaction-owned except the two focus fields, which the driver supplies
    /// — a checkpoint states the focus rather than re-deriving it, because §7.4 lets it move only
    /// on a committed transition and a restore has no transition to move it on.
    fn transition_state(
        &self,
        root_kind: Option<RootKind>,
        focus: Option<ExecutionFocus>,
    ) -> Result<TransitionStateV1, KernelFault> {
        let config = self.config.as_ref().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "an operation with no genesis record has no resolved configuration to checkpoint"
                    .to_string(),
            )
        })?;

        Ok(TransitionStateV1 {
            lifecycle: self.lifecycle,
            resolved_config: config.clone(),
            root_kind,
            focus,
            last_observed_at_ms: self.last_observed_at_ms,
            pending_effects: self.pending_effects.values().cloned().collect(),
            resolved_effects: self
                .resolved_effects
                .iter()
                .map(|(effect_id, resolved)| ResolvedEffectState {
                    effect_id: effect_id.clone(),
                    outcome_digest: resolved.outcome_digest.clone(),
                    input_id: resolved.input_id.clone(),
                    step_seq: resolved.step_seq,
                })
                .collect(),
            launch_tokens: self
                .launch_tokens
                .iter()
                .map(|(launch_token, step_seq)| LaunchTokenState {
                    launch_token: launch_token.clone(),
                    step_seq: *step_seq,
                })
                .collect(),
            accepted_inputs: self.accepted.values().cloned().collect(),
            accepted_cancellation: self.accepted_cancellation.as_ref().map(|cancellation| {
                AcceptedCancellationState {
                    command_digest: cancellation.command_digest.clone(),
                    input_id: cancellation.input_id.clone(),
                    step_seq: cancellation.step_seq,
                }
            }),
            terminal: self.terminal.get().cloned(),
        })
    }

    /// Reclaim the tail prefix an installed checkpoint covers — only after the host acked it
    /// (§12.3 rule 6).
    ///
    /// The boundary is verified against the tail's own digests, so a checkpoint from another
    /// operation, or one claiming a step this journal never had, is refused. It does **not** have
    /// to name the current head (§12.3 rule 2): records appended after the candidate stay as tail.
    ///
    /// What it reclaims is the tail *accounting*, never the replay/dedup ledgers — §12.3 rule 7
    /// is explicit that an ack must not empty the window that makes a redelivery idempotent.
    pub fn note_checkpoint_acked(
        &mut self,
        boundary: &CheckpointBoundary,
    ) -> Result<TailUsage, KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        let matches = self.tail.iter().any(|entry| {
            entry.step_seq == boundary.through_step_seq
                && entry.record_digest == boundary.covered_head
        });
        if !matches {
            return Err(KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!(
                    "no tail record at step {} has digest {}; this checkpoint does not cover a \
                     prefix of this journal",
                    boundary.through_step_seq, boundary.covered_head
                ),
            ));
        }
        while let Some(entry) = self.tail.front() {
            if entry.step_seq.get() <= boundary.through_step_seq.get() {
                self.tail.pop_front();
            } else {
                break;
            }
        }
        Ok(self.tail_usage())
    }

    // ----- observers -----

    pub fn operation_id(&self) -> Option<&OperationId> {
        self.operation_id.as_ref()
    }

    pub fn config(&self) -> Option<&ResolvedOperationConfig> {
        self.config.as_ref()
    }

    pub fn head(&self) -> Option<DurableHead> {
        self.head.as_ref().map(|anchor| DurableHead {
            digest: anchor.record_digest.clone(),
            step_seq: anchor.step_seq,
        })
    }

    pub fn lifecycle(&self) -> OperationLifecycle {
        self.lifecycle
    }

    pub fn terminal(&self) -> Option<&KernelTerminal> {
        self.terminal.get()
    }

    /// Effects published by committed records and not yet resolved. A prepared-but-uncommitted
    /// step's effects are **not** here (§15.2).
    pub fn pending_effects(&self) -> impl Iterator<Item = &KernelEffect> {
        self.pending_effects.values()
    }

    pub fn is_effect_resolved(&self, effect_id: &EffectId) -> bool {
        self.resolved_effects.contains_key(effect_id)
    }

    pub fn knows_launch_token(&self, token: &LaunchToken) -> bool {
        self.launch_tokens.contains_key(token)
    }

    pub fn committed_step(&self, input_id: &InputId) -> Option<&Step> {
        self.steps.get(input_id)
    }

    pub fn outstanding_token(&self) -> Option<&PrepareToken> {
        self.candidate.as_ref().map(|candidate| &candidate.token)
    }

    pub fn has_candidate(&self) -> bool {
        self.candidate.is_some()
    }

    /// The fault that poisoned this transaction, if any. A poisoned transaction is not recoverable
    /// in place — the host discards it and rebuilds from the journal (§8.3).
    pub fn poison(&self) -> Option<&KernelFault> {
        self.poison.as_ref()
    }

    pub fn is_poisoned(&self) -> bool {
        self.poison.is_some()
    }

    pub fn bounds(&self) -> TailBounds {
        self.bounds
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn tail_usage(&self) -> TailUsage {
        TailUsage {
            records: self.tail_records(),
            bytes: self.tail_bytes(),
        }
    }

    pub fn tail_pressure(&self) -> TailPressure {
        let usage = self.tail_usage();
        if usage.records >= self.bounds.hard_records.get()
            || usage.bytes >= self.bounds.hard_bytes.get()
        {
            TailPressure::Full
        } else if usage.records >= self.bounds.soft_records.get()
            || usage.bytes >= self.bounds.soft_bytes.get()
        {
            TailPressure::Watermark
        } else {
            TailPressure::Nominal
        }
    }

    // ----- internals -----

    fn tail_records(&self) -> u64 {
        self.tail.len() as u64
    }

    fn tail_bytes(&self) -> u64 {
        self.tail.iter().map(|entry| entry.bytes).sum()
    }

    fn next_step_seq(&self) -> Result<WireU64, KernelFault> {
        match &self.head {
            None => Ok(WireU64::ZERO),
            Some(head) => head
                .step_seq
                .get()
                .checked_add(1)
                .map(WireU64::new)
                .ok_or_else(|| {
                    KernelFault::new(
                        KernelFaultCode::ResourceLimitExceeded,
                        "step sequence overflowed u64".to_string(),
                    )
                }),
        }
    }

    fn checkpoint_required(&self, detail: String) -> KernelFault {
        KernelFault::new(
            KernelFaultCode::CheckpointRequired,
            format!(
                "{detail}; take a checkpoint candidate, install and ack it, then retry this input \
                 unchanged — it was never accepted"
            ),
        )
    }

    fn poison_with(&mut self, fault: KernelFault) -> KernelFault {
        self.candidate = None;
        self.poison.get_or_insert(fault).clone()
    }

    /// Build the `Replayed` arm for a record the journal already holds.
    ///
    /// §12.3 rule 10 decides how strong the answer is. Above the replay floor the step is still
    /// held and travels with the record. At or below it — a restored runtime's checkpointed prefix
    /// — the step was never durable and this process never replayed it, so the answer is the
    /// ledger's own reference. Missing above the floor is a genuine disagreement with the journal
    /// and stays fail-closed.
    fn replay_of(&self, existing: KernelRecord) -> Result<RecordPreparation<Step>, KernelFault> {
        let step_seq = existing.step_seq();
        let record_digest = existing.record_digest().clone();
        match self.steps.get(existing.input_id()) {
            Some(step) => {
                existing.verify_step(step).map_err(record_fault)?;
                Ok(KernelPreparation::Replayed(ReplayedTransition {
                    record: Some(existing),
                    record_digest,
                    committed_step: Some(step.clone()),
                    step_seq,
                }))
            }
            None if self.below_replay_floor(step_seq) => {
                Ok(KernelPreparation::Replayed(ReplayedTransition {
                    record: Some(existing),
                    record_digest,
                    committed_step: None,
                    step_seq,
                }))
            }
            None => Err(KernelFault::new(
                KernelFaultCode::RecordCorrupted,
                format!(
                    "the journal holds record {record_digest} at step {step_seq} for input {}, but \
                     this runtime has not replayed it and cannot reproduce its step; rebuild from \
                     the journal first",
                    existing.input_id()
                ),
            )),
        }
    }

    /// §12.3 rule 10 · the ledger's own answer, for an input whose record retention already
    /// reclaimed.
    fn replay_by_reference(&self, entry: &AcceptedInputState) -> RecordPreparation<Step> {
        KernelPreparation::Replayed(ReplayedTransition {
            record: None,
            record_digest: entry.record_digest.clone(),
            committed_step: None,
            step_seq: entry.step_seq,
        })
    }

    fn below_replay_floor(&self, step_seq: WireU64) -> bool {
        self.replay_floor
            .is_some_and(|floor| step_seq.get() <= floor.get())
    }

    /// DEC-1 + §15.3: dedup an already-resolved effect, and fail closed on everything a pending
    /// effect cannot legally be answered with.
    ///
    /// `Ok(Some(_))` is the dedup replay; `Ok(None)` means "this resolution is new and legal".
    fn resolve_effect_guard(
        &self,
        resolve: &super::envelope::ResolveEffect,
        // (the wire type, not a second shape — a resolution is one input class)
    ) -> Result<Option<RecordPreparation<Step>>, KernelFault> {
        let outcome_digest = outcome_digest(&resolve.outcome)?;
        if let Some(resolved) = self.resolved_effects.get(&resolve.effect_id) {
            if resolved.outcome_digest != outcome_digest {
                return Err(KernelFault::new(
                    KernelFaultCode::UnexpectedEffectOutcome,
                    format!(
                        "effect {} was already resolved at step {} with a different outcome",
                        resolve.effect_id, resolved.step_seq
                    ),
                ));
            }
            // A *new* input_id resolving an already-completed effect with the same payload is a
            // replay of the existing record, never a second record (DEC-1).
            let Some(existing) = self.index.record_for_input(
                self.operation_id.as_ref().expect("bound by the genesis"),
                &resolved.input_id,
            ) else {
                return Err(KernelFault::new(
                    KernelFaultCode::RecordCorrupted,
                    format!(
                        "effect {} is resolved by input {} at step {}, but the journal has no such \
                         record",
                        resolve.effect_id, resolved.input_id, resolved.step_seq
                    ),
                ));
            };
            return self.replay_of(existing).map(Some);
        }

        let Some(pending) = self.pending_effects.get(&resolve.effect_id) else {
            return Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                format!(
                    "effect {} is not pending; the kernel is not waiting on it",
                    resolve.effect_id
                ),
            ));
        };
        pending.accept_outcome(&resolve.outcome).map_err(|error| {
            KernelFault::new(KernelFaultCode::UnexpectedEffectOutcome, error.to_string())
        })?;
        Ok(None)
    }

    /// §18.3 · dedup a cancellation this operation already accepted.
    ///
    /// `Ok(Some(_))` is the dedup replay; `Ok(None)` means "no cancellation has been accepted yet",
    /// which is the only state in which a cancel is a new decision. A *different* cancellation —
    /// another reason, or a different set of abandoned calls — is a conflict rather than a silent
    /// overwrite: the operation already ended for the first reason, and the second cannot re-decide
    /// that.
    fn cancel_guard(
        &self,
        cancel: &CancelCommand,
    ) -> Result<Option<RecordPreparation<Step>>, KernelFault> {
        let Some(accepted) = &self.accepted_cancellation else {
            return Ok(None);
        };
        if accepted.command_digest != cancel_digest(cancel)? {
            return Err(KernelFault::new(
                KernelFaultCode::DuplicateInputConflict,
                format!(
                    "this operation committed a different cancellation at step {}; a cancel is not \
                     re-decided once the terminal it produced exists",
                    accepted.step_seq
                ),
            ));
        }
        let Some(existing) = self.index.record_for_input(
            self.operation_id.as_ref().expect("bound by the genesis"),
            &accepted.input_id,
        ) else {
            return Err(KernelFault::new(
                KernelFaultCode::RecordCorrupted,
                format!(
                    "this operation was cancelled by input {} at step {}, but the journal has no \
                     such record",
                    accepted.input_id, accepted.step_seq
                ),
            ));
        };
        self.replay_of(existing).map(Some)
    }

    /// Screen a planned step's effects **before** the record is built, so a refusal is still a
    /// zero-mutation rejection rather than a durable transition the host cannot execute.
    fn screen_planned_effects(
        &self,
        step: &Step,
        config: &ResolvedOperationConfig,
        settled: Option<&EffectId>,
    ) -> Result<(), KernelFault> {
        let mut kinds_in_step: Vec<EffectKindTag> = Vec::new();
        let mut tokens_in_step: Vec<&LaunchToken> = Vec::new();
        // The effect this very input resolves stops being pending in the same committed step, so
        // it must not block its own successor: a provider turn that answers one call and asks the
        // next question is the ordinary shape of a run, not a DEC-3 violation.
        let still_pending = |effect_id: &EffectId| {
            self.pending_effects.contains_key(effect_id) && Some(effect_id) != settled
        };

        for effect in step.effects() {
            let tag = effect.tag();

            // DEC-8 · never publish an effect the host declared it cannot execute.
            if !config.host_effect_support.supports(tag) {
                return Err(KernelFault::new(
                    KernelFaultCode::UnsupportedEffect,
                    format!(
                        "this operation's host does not declare support for {tag} effects, so \
                         effect {} is refused before emission",
                        effect.effect_id
                    ),
                ));
            }

            // Identity is minted once. A re-minted effect id would make the host's
            // effect-id-keyed idempotency answer the wrong effect.
            if self.pending_effects.contains_key(&effect.effect_id)
                || self.resolved_effects.contains_key(&effect.effect_id)
                || Some(&effect.effect_id) == settled
            {
                return Err(KernelFault::new(
                    KernelFaultCode::TransactionConflict,
                    format!(
                        "effect id {} was already published; effect identity is minted once",
                        effect.effect_id
                    ),
                ));
            }

            // DEC-3 · at most one pending effect per kind.
            if kinds_in_step.contains(&tag)
                || self
                    .pending_effects
                    .iter()
                    .any(|(id, pending)| pending.tag() == tag && still_pending(id))
            {
                return Err(KernelFault::new(
                    KernelFaultCode::ResourceLimitExceeded,
                    format!(
                        "a {tag} effect is already pending; resolve it before emitting another \
                         (§15.3 admits at most one pending effect per kind)"
                    ),
                ));
            }
            kinds_in_step.push(tag);

            if let EffectKind::SpawnTasks(spawn) = &effect.effect {
                for launch in &spawn.tasks {
                    if self.launch_tokens.contains_key(&launch.launch_token)
                        || tokens_in_step.contains(&&launch.launch_token)
                    {
                        return Err(KernelFault::new(
                            KernelFaultCode::TransactionConflict,
                            format!(
                                "launch token {} was already published; a re-launch reuses the \
                                 committed token so the host's launch dedup stays exact",
                                launch.launch_token
                            ),
                        ));
                    }
                    tokens_in_step.push(&launch.launch_token);
                }
            }
        }
        Ok(())
    }

    /// Fold one durable record into the runtime state. Shared by [`Self::commit`] and
    /// [`Self::rebuild_from_records`] so a rebuilt runtime is the same runtime, not a similar one.
    fn integrate(
        &mut self,
        record: KernelRecord,
        step: Step,
        input: &NormalizedInput,
        record_bytes: u64,
    ) -> Result<CommittedTransition<Step>, KernelFault> {
        let step_seq = record.step_seq();
        let was_nominal = self.tail_pressure() == TailPressure::Nominal;

        if self.operation_id.is_none() {
            self.operation_id = Some(record.operation_id().clone());
        }
        if record.is_genesis() {
            self.genesis_digest = Some(record.record_digest().clone());
        }
        if let Some(config) = input.resolved_config() {
            // §5e-5 · the genesis record is what freezes the tail bound. Until this line the
            // transaction runs on the binary's bootstrap baseline (something has to bound the
            // genesis append itself); from here on it runs on the value this operation's own
            // configuration resolved to, so a later binary's different default cannot move it.
            self.bounds = config.recovery_policy.tail_bounds;
            self.config = Some(config.clone());
            self.lifecycle = OperationLifecycle::Configured;
        } else if matches!(input.input, NormalizedPayload::StartOperation(_)) {
            self.lifecycle = OperationLifecycle::Running;
        }

        if let NormalizedPayload::HostControl(control) = &input.input
            && let HostCommand::Cancel(cancel) = &control.command
        {
            self.accepted_cancellation = Some(AcceptedCancellation {
                command_digest: cancel_digest(cancel)?,
                input_id: record.input_id().clone(),
                step_seq,
            });
        }

        if let NormalizedPayload::ResolveEffect(resolve) = &input.input {
            self.pending_effects.remove(&resolve.effect_id);
            self.resolved_effects.insert(
                resolve.effect_id.clone(),
                ResolvedEffectRecord {
                    outcome_digest: outcome_digest(&resolve.outcome)?,
                    input_id: record.input_id().clone(),
                    step_seq,
                },
            );
        }

        for effect in step.effects() {
            self.pending_effects
                .insert(effect.effect_id.clone(), effect.clone());
            if let EffectKind::SpawnTasks(spawn) = &effect.effect {
                for launch in &spawn.tasks {
                    self.launch_tokens
                        .insert(launch.launch_token.clone(), step_seq);
                }
            }
        }

        if let Some(terminal) = step.terminal() {
            self.terminal.commit(terminal.clone()).map_err(|error| {
                KernelFault::new(KernelFaultCode::InvalidLifecycle, error.to_string())
            })?;
            self.lifecycle = terminal_lifecycle(terminal);
            // A terminal ends the operation; nothing is left waiting on the host.
            self.pending_effects.clear();
        }

        self.last_observed_at_ms = input.observed_at_ms;
        self.steps.insert(record.input_id().clone(), step.clone());
        self.accepted.insert(
            record.input_id().clone(),
            AcceptedInputState {
                input_id: record.input_id().clone(),
                step_seq,
                record_digest: record.record_digest().clone(),
            },
        );
        self.tail.push_back(TailEntry {
            step_seq,
            record_digest: record.record_digest().clone(),
            bytes: record_bytes,
            input: input.clone(),
        });
        self.index.note_committed(&record);
        self.head = Some(record.anchor());

        // The crossing is read *after* the tail grew and compared with what it was before, so the
        // advice fires on the transition that caused it and on no other.
        let checkpoint_advice = (was_nominal && self.tail_pressure() != TailPressure::Nominal)
            .then(|| CheckpointAdvice {
                through_step_seq: step_seq,
                usage: self.tail_usage(),
                bounds: self.bounds,
            });

        Ok(CommittedTransition {
            record,
            step,
            step_seq,
            checkpoint_advice,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// error projections
// ---------------------------------------------------------------------------------------------

fn outcome_digest(outcome: &EffectOutcome) -> Result<Digest, KernelFault> {
    canonical_bytes(outcome)
        .map(|bytes| canonical_digest(bytes.as_slice()))
        .map_err(record_fault)
}

fn cancel_digest(cancel: &CancelCommand) -> Result<Digest, KernelFault> {
    canonical_bytes(cancel)
        .map(|bytes| canonical_digest(bytes.as_slice()))
        .map_err(record_fault)
}

fn record_fault(error: RecordError) -> KernelFault {
    KernelFault::new(error.code(), error.message().to_string())
}

/// A chain that does not verify is journal corruption, not a misplaced input: the records are
/// already durable, so nobody can be told "place it somewhere else".
fn corrupt_chain_fault(error: RecordError) -> KernelFault {
    match error {
        RecordError::ChainBroken(message) => {
            KernelFault::new(KernelFaultCode::RecordCorrupted, message)
        }
        other => record_fault(other),
    }
}

fn rejection_fault(rejection: WireRejection) -> KernelFault {
    let code = match rejection.kind {
        WireRejectionKind::PolicyViolation => KernelFaultCode::InvalidConfig,
        WireRejectionKind::VersionMismatch => KernelFaultCode::VersionMismatch,
        _ => KernelFaultCode::MalformedEnvelope,
    };
    KernelFault::new(code, rejection.message)
}

fn terminal_lifecycle(terminal: &KernelTerminal) -> OperationLifecycle {
    match terminal {
        KernelTerminal::Agent(_) | KernelTerminal::Workflow(_) => OperationLifecycle::Completed,
        KernelTerminal::Cancelled(_) => OperationLifecycle::Cancelled,
        KernelTerminal::Failed(_) => OperationLifecycle::Failed,
    }
}

impl fmt::Display for CheckpointBoundary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "through step {} at head {}",
            self.through_step_seq, self.covered_head
        )
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::super::*;

    // -----------------------------------------------------------------------------------------
    // fixtures
    // -----------------------------------------------------------------------------------------

    const OPERATION: &str = "op-tx-1";

    fn operation() -> OperationId {
        OperationId::new(OPERATION).unwrap()
    }

    fn input_id(name: &str) -> InputId {
        InputId::new(name).unwrap()
    }

    fn boot_config(supported: impl IntoIterator<Item = EffectKindTag>) -> OperationConfig {
        OperationConfig {
            execution_policy: Some(ExecutionPolicy {
                max_turns: Some(12),
                ..ExecutionPolicy::default()
            }),
            host_effect_support: HostEffectSupport::new(supported),
            ..OperationConfig::default()
        }
    }

    fn envelope(id: &str, observed_at_ms: u64, input: KernelInput) -> WireEnvelope {
        WireEnvelope::new(
            operation(),
            input_id(id),
            WireU64::new(observed_at_ms),
            input,
        )
    }

    fn configure_at(id: &str, supported: impl IntoIterator<Item = EffectKindTag>) -> WireEnvelope {
        envelope(
            id,
            1_700_000_000_000,
            KernelInput::ConfigureOperation(ConfigureOperation {
                config: boot_config(supported),
            }),
        )
    }

    fn configure() -> WireEnvelope {
        configure_at(
            "in-configure",
            [EffectKindTag::CallProvider, EffectKindTag::SpawnTasks],
        )
    }

    fn start_at(id: &str, observed_at_ms: u64) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("write the brief"),
                    run_spec: None,
                }),
                initial_context: InitialContext::default(),
            }),
        )
    }

    fn start() -> WireEnvelope {
        start_at("in-start", 1_700_000_001_000)
    }

    fn provider_outcome() -> EffectOutcome {
        EffectOutcome::Succeeded(EffectSucceeded {
            result: EffectSuccess::Provider(ProviderSuccess {
                outcome: ProviderOutcome::ContextOverflow(ProviderContextOverflow::default()),
            }),
        })
    }

    fn failure_outcome() -> EffectOutcome {
        EffectOutcome::Failed(EffectFailed {
            failure: HostEffectFailure {
                kind: HostEffectFailureKind::TransportExhausted,
                message: "the vendor gave up".to_string(),
                retryable: Some(false),
            },
        })
    }

    fn resolve_at(
        id: &str,
        observed_at_ms: u64,
        effect_id: &EffectId,
        outcome: EffectOutcome,
    ) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect_id.clone(),
                outcome,
            }),
        )
    }

    fn cancel_at(id: &str, observed_at_ms: u64) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::HostControl(HostControl {
                command: HostCommand::Cancel(CancelCommand {
                    reason: CancellationReason::User,
                    pending_call_ids: vec![],
                }),
            }),
        )
    }

    fn signal_at(id: &str, observed_at_ms: u64) -> WireEnvelope {
        use super::super::event::{DeliverSignal, ExternalEvent, LogicalSignal};
        use super::super::scalar::{DeliveryId, SignalId};

        envelope(
            id,
            observed_at_ms,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::DeliverSignal(DeliverSignal {
                    delivery_id: DeliveryId::new(format!("delivery-{id}")).unwrap(),
                    attempt: 1,
                    signal: LogicalSignal::new(SignalId::new("sig-late").unwrap()),
                }),
            }),
        )
    }

    // ----- the step a test planner produces -----

    #[derive(Debug, Clone, PartialEq, Serialize)]
    struct TestStep {
        plan: String,
        disposition: StepDisposition,
    }

    impl TransitionStep for TestStep {
        fn disposition(&self) -> &StepDisposition {
            &self.disposition
        }
    }

    fn nothing(plan: &str) -> TestStep {
        TestStep {
            plan: plan.to_string(),
            disposition: StepDisposition::Effects(EffectsDisposition::default()),
        }
    }

    fn publishing(plan: &str, effects: Vec<KernelEffect>) -> TestStep {
        TestStep {
            plan: plan.to_string(),
            disposition: StepDisposition::Effects(EffectsDisposition { effects }),
        }
    }

    fn effect(id: &str, causation: &InputId, kind: EffectKind) -> KernelEffect {
        KernelEffect {
            effect_id: EffectId::new(id).unwrap(),
            causation_input_id: causation.clone(),
            effect: kind,
        }
    }

    fn provider_effect_id(step_seq: WireU64) -> EffectId {
        EffectId::new(format!("{OPERATION}:step:{step_seq}:effect:0")).unwrap()
    }

    /// The one deterministic planner every test shares: `configure` plans nothing, `start`
    /// publishes a provider call, a resolution plans nothing, and a host command terminates.
    ///
    /// Deterministic in the strict sense — a pure function of the canonical input and the step
    /// position — which is exactly what a rebuild re-runs.
    fn plan(context: &PlanContext<'_>) -> Result<TestStep, KernelFault> {
        let label = format!("{}@{}", context.input.input.kind(), context.step_seq);
        Ok(match &context.input.input {
            NormalizedPayload::StartOperation(_) => publishing(
                &label,
                vec![effect(
                    provider_effect_id(context.step_seq).as_str(),
                    &context.input.input_id,
                    EffectKind::CallProvider(CallProviderEffect::default()),
                )],
            ),
            NormalizedPayload::HostControl(_) => TestStep {
                plan: label,
                disposition: StepDisposition::Terminal(TerminalDisposition {
                    terminal: KernelTerminal::Cancelled(CancelledTerminal {
                        reason: CancellationReason::User,
                        usage: UsageReport::default(),
                    }),
                }),
            },
            _ => nothing(&label),
        })
    }

    type Tx = KernelTransaction<TestStep, InMemoryRecordIndex>;

    fn transaction() -> Tx {
        KernelTransaction::new(ConfigDefaults::default(), InMemoryRecordIndex::new())
    }

    /// A transaction whose *baseline* carries a tighter tail bound.
    ///
    /// Deliberately routed through [`ConfigDefaults`] rather than through a constructor argument:
    /// §5e-5 put `TailBounds` in the resolved configuration, so the only ways an operation can end
    /// up with a non-default bound are "the binary's baseline says so" and "the genesis record
    /// resolved one". A test that could inject a bound past both would be testing a path no host
    /// has.
    fn bounded(bounds: TailBounds) -> Tx {
        let mut defaults = ConfigDefaults::default();
        defaults.baseline.recovery_policy.tail_bounds = bounds;
        KernelTransaction::new(defaults, InMemoryRecordIndex::new())
    }

    /// One full §8.2 round trip: prepare → (host CAS append) → commit.
    fn run(tx: &mut Tx, envelope: &WireEnvelope) -> CommittedTransition<TestStep> {
        run_with(tx, envelope, plan)
    }

    fn run_with<F>(
        tx: &mut Tx,
        envelope: &WireEnvelope,
        planner: F,
    ) -> CommittedTransition<TestStep>
    where
        F: FnOnce(&PlanContext<'_>) -> Result<TestStep, KernelFault>,
    {
        let preparation = tx.prepare(envelope, planner);
        let token = preparation
            .token()
            .unwrap_or_else(|| {
                panic!(
                    "expected a prepared transition, got {:?}",
                    preparation.fault()
                )
            })
            .clone();
        let head = preparation.record().unwrap().record_digest().clone();
        tx.commit(&token, &head).expect("the commit must succeed")
    }

    fn fault_of(preparation: &RecordPreparation<TestStep>) -> KernelFaultCode {
        preparation
            .fault()
            .unwrap_or_else(|| panic!("expected a rejection, got a success"))
            .code
    }

    /// Everything a host can observe about a transaction, for before/after comparisons where the
    /// internal prepare epoch legitimately moves.
    fn observable(
        tx: &Tx,
    ) -> (
        Option<DurableHead>,
        OperationLifecycle,
        Vec<String>,
        TailUsage,
        bool,
    ) {
        (
            tx.head(),
            tx.lifecycle(),
            tx.pending_effects()
                .map(|effect| effect.effect_id.to_string())
                .collect(),
            tx.tail_usage(),
            tx.has_candidate(),
        )
    }

    fn started() -> (Tx, Vec<KernelRecord>, EffectId) {
        let mut tx = transaction();
        let genesis = run(&mut tx, &configure());
        let started = run(&mut tx, &start());
        let effect_id = provider_effect_id(started.step_seq);
        (tx, vec![genesis.record, started.record], effect_id)
    }

    // -----------------------------------------------------------------------------------------
    // §8.3 failure matrix, row by row
    // -----------------------------------------------------------------------------------------

    /// Row 1 — before a prepare there is no candidate and no journal mutation.
    #[test]
    fn matrix_row1_before_a_prepare_nothing_exists() {
        let tx = transaction();
        assert!(!tx.has_candidate());
        assert_eq!(tx.head(), None);
        assert_eq!(tx.lifecycle(), OperationLifecycle::Created);
        assert_eq!(tx.pending_effects().count(), 0);
        assert_eq!(tx.tail_usage(), TailUsage::default());
        assert!(tx.index().is_empty(), "no record exists yet");
        assert_eq!(tx.checkpoint_boundary(), None);
        assert!(tx.outstanding_token().is_none());
    }

    /// Row 2 — a rejected prepare hands out no token and moves nothing.
    ///
    /// Asserted structurally: the whole transaction is cloned and compared, so "nothing moved"
    /// covers the ledgers, the tail and the head, not just the fields a test remembered to check.
    #[test]
    fn matrix_row2_a_rejected_prepare_hands_out_no_token_and_moves_nothing() {
        let (mut tx, _, effect_id) = started();

        let rejections: Vec<(&str, RecordPreparation<TestStep>)> = vec![
            // wrong lifecycle
            (
                "second configure",
                tx.prepare(
                    &configure_at("in-again", [EffectKindTag::CallProvider]),
                    plan,
                ),
            ),
            // unknown effect
            (
                "unknown effect",
                tx.prepare(
                    &resolve_at(
                        "in-unknown",
                        1_700_000_002_000,
                        &EffectId::new("op-tx-1:step:9:effect:0").unwrap(),
                        failure_outcome(),
                    ),
                    plan,
                ),
            ),
            // a fault raised by the planner itself, i.e. a rejection *after* planning ran
            (
                "planner fault",
                tx.prepare(
                    &resolve_at(
                        "in-planned",
                        1_700_000_002_000,
                        &effect_id,
                        failure_outcome(),
                    ),
                    |_| {
                        Err(KernelFault::new(
                            KernelFaultCode::ResourceLimitExceeded,
                            "the planner refused",
                        ))
                    },
                ),
            ),
        ];

        let before = tx.clone();
        for (label, preparation) in rejections {
            assert!(preparation.is_zero_mutation(), "{label}");
            assert!(preparation.token().is_none(), "{label}");
            assert!(preparation.record().is_none(), "{label}");
            assert!(preparation.step().is_none(), "{label}");
            assert!(preparation.step_seq().is_none(), "{label}");
        }
        assert_eq!(tx, before, "a rejected prepare must not move one byte");
    }

    /// Row 3 — a crash between prepare and append is undone by `abort`.
    #[test]
    fn matrix_row3_a_crash_before_the_append_is_undone_by_abort() {
        let (mut tx, records, _) = started();
        let before = observable(&tx);

        let preparation = tx.prepare(&cancel_at("in-cancel", 1_700_000_003_000), plan);
        let token = preparation.token().expect("prepared").clone();
        assert!(tx.has_candidate());
        assert_eq!(
            tx.head().map(|head| head.digest),
            Some(records[1].record_digest().clone()),
            "a candidate does not move the durable head"
        );

        let discarded = tx.abort(&token).expect("the candidate was never appended");
        assert_eq!(discarded.step_seq(), WireU64::new(2));
        assert_eq!(
            observable(&tx),
            before,
            "an aborted candidate leaves no trace"
        );

        // and the discarded token is spent: it can neither be aborted nor committed again
        assert_eq!(
            tx.abort(&token).unwrap_err().code,
            KernelFaultCode::TransactionConflict
        );
        assert!(
            !tx.is_poisoned(),
            "an abort is a normal, non-poisoning path"
        );
    }

    /// Row 4 — a CAS conflict discards the candidate and demands a rebuild.
    #[test]
    fn matrix_row4_a_cas_conflict_discards_the_candidate_and_demands_a_rebuild() {
        let (mut tx, records, _) = started();
        let preparation = tx.prepare(&cancel_at("in-cancel", 1_700_000_003_000), plan);
        let token = preparation.token().expect("prepared").clone();

        // another writer moved the head under us
        let forked = canonical_digest(b"another writer's record");
        let fault = tx.note_append_conflict(&token, Some(&forked));

        assert_eq!(fault.code, KernelFaultCode::TransactionConflict);
        assert!(!fault.is_retryable(), "a conflict is not a bare retry");
        assert!(
            !tx.has_candidate(),
            "the candidate is discarded, not appended"
        );
        assert!(tx.is_poisoned(), "this layer never rebuilds itself");

        // fail closed: nothing works until the host rebuilds
        assert_eq!(
            fault_of(&tx.prepare(&cancel_at("in-cancel-2", 1_700_000_004_000), plan)),
            KernelFaultCode::TransactionConflict
        );
        assert_eq!(
            tx.abort(&token).unwrap_err().code,
            KernelFaultCode::TransactionConflict
        );
        assert_eq!(
            tx.commit(&token, &forked).unwrap_err().code,
            KernelFaultCode::TransactionConflict
        );

        // the journal itself is untouched — the rebuild entry point is the whole recovery
        let rebuilt = Tx::rebuild_from_records(
            &records,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(&records),
            plan,
        )
        .expect("the journal still verifies");
        assert_eq!(
            rebuilt.head().map(|head| head.digest),
            Some(records[1].record_digest().clone())
        );
        assert!(!rebuilt.is_poisoned());
    }

    /// Row 5 — a crash between a successful append and the commit: the new process rebuilds from
    /// the journal, and the effect the step planned is published exactly once, by the rebuild.
    #[test]
    fn matrix_row5_a_crash_between_append_and_commit_rebuilds_and_publishes_once() {
        let mut tx = transaction();
        let genesis = run(&mut tx, &configure());
        let mut journal = vec![genesis.record];

        let preparation = tx.prepare(&start(), plan);
        let appended = preparation.record().expect("prepared").clone();
        // the host appends...
        journal.push(appended.clone());
        // ...and the process dies before commit. The effect was never published.
        assert_eq!(
            tx.pending_effects().count(),
            0,
            "a record that has not been committed publishes no effect (§15.2)"
        );
        drop(tx);

        let rebuilt = Tx::rebuild_from_records(
            &journal,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(&journal),
            plan,
        )
        .expect("the journal rebuilds");

        assert_eq!(
            rebuilt.head().map(|head| head.digest),
            Some(appended.record_digest().clone())
        );
        assert_eq!(rebuilt.lifecycle(), OperationLifecycle::Running);
        let pending: Vec<&EffectId> = rebuilt
            .pending_effects()
            .map(|effect| &effect.effect_id)
            .collect();
        assert_eq!(
            pending,
            vec![&provider_effect_id(WireU64::new(1))],
            "the rebuilt runtime re-exposes the one pending effect, with the same identity"
        );
    }

    /// Row 6 — a commit that fails after a successful append poisons the runtime, never aborts,
    /// and never revokes the durable record.
    #[test]
    fn matrix_row6_a_failed_commit_never_becomes_an_abort() {
        let mut tx = transaction();
        let genesis = run(&mut tx, &configure());
        let journal = vec![genesis.record];

        let preparation = tx.prepare(&start(), plan);
        let token = preparation.token().expect("prepared").clone();
        // The append succeeded, but the head the journal reports is not this record — the commit
        // cannot be honoured.
        let wrong_head = canonical_digest(b"some other record");
        let fault = tx
            .commit(&token, &wrong_head)
            .expect_err("a commit that cannot be attributed must fail");
        assert_eq!(fault.code, KernelFaultCode::TransactionConflict);

        // The abort branch is not reachable: there is no candidate left to abort, and the
        // transaction is poisoned.
        assert!(tx.is_poisoned());
        assert!(!tx.has_candidate());
        assert_eq!(
            tx.abort(&token).unwrap_err().code,
            KernelFaultCode::TransactionConflict,
            "append-then-abort is not expressible"
        );

        // and the durable prefix is untouched — recovery is a rebuild, not a rollback
        let rebuilt = Tx::rebuild_from_records(
            &journal,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(&journal),
            plan,
        )
        .expect("the durable record survives a failed commit");
        assert_eq!(rebuilt.lifecycle(), OperationLifecycle::Configured);
    }

    /// Row 7 — a lost commit response replays to the same record and the same step.
    #[test]
    fn matrix_row7_a_lost_commit_response_replays_the_same_record_and_step() {
        let mut tx = transaction();
        run(&mut tx, &configure());
        let committed = run(&mut tx, &start());
        let before = observable(&tx);

        // the caller never saw the response and retries the identical envelope
        let replay = tx.prepare(&start(), plan);

        assert!(replay.token().is_none(), "a replay has nothing to commit");
        assert_eq!(replay.step_seq(), Some(committed.step_seq));
        assert_eq!(replay.record(), Some(&committed.record));
        assert_eq!(replay.step(), Some(&committed.step));
        assert_eq!(
            observable(&tx),
            before,
            "a replay creates no record and re-publishes no effect"
        );
        assert_eq!(tx.index().len(), 2, "no second record was minted");
    }

    /// Row 8 — a lost resolution response replays idempotently, both by input id and, per DEC-1,
    /// under a brand-new input id.
    #[test]
    fn matrix_row8_a_lost_resolution_response_replays_idempotently() {
        let (mut tx, _, effect_id) = started();
        let resolved = run(
            &mut tx,
            &resolve_at(
                "in-resolve",
                1_700_000_002_000,
                &effect_id,
                provider_outcome(),
            ),
        );
        assert_eq!(tx.pending_effects().count(), 0, "the effect is settled");
        let before = observable(&tx);

        // 1. the host redelivers the same envelope
        let same_input = tx.prepare(
            &resolve_at(
                "in-resolve",
                1_700_000_002_000,
                &effect_id,
                provider_outcome(),
            ),
            plan,
        );
        assert_eq!(same_input.step_seq(), Some(resolved.step_seq));
        assert_eq!(same_input.record(), Some(&resolved.record));

        // 1b. re-stamping the clock on a retry is a *different* input, not a replay: the envelope
        //     time is part of the canonical input the record froze (§11.2), so a retry must
        //     replay the original envelope rather than rebuild it.
        assert_eq!(
            fault_of(&tx.prepare(
                &resolve_at(
                    "in-resolve",
                    1_700_000_002_500,
                    &effect_id,
                    provider_outcome()
                ),
                plan
            )),
            KernelFaultCode::DuplicateInputConflict
        );

        // 2. DEC-1: a *new* input id resolving the same effect with the same payload is the same
        //    transition, not a second record.
        let new_input = tx.prepare(
            &resolve_at(
                "in-resolve-again",
                1_700_000_003_000,
                &effect_id,
                provider_outcome(),
            ),
            plan,
        );
        assert_eq!(
            new_input.step_seq(),
            Some(resolved.step_seq),
            "effect-level dedup points at the existing record's step_seq"
        );
        assert_eq!(new_input.record(), Some(&resolved.record));
        assert!(
            new_input.token().is_none(),
            "reporting this as Prepared with an old step_seq is the dead end this replaces"
        );
        assert_eq!(observable(&tx), before);
        assert_eq!(tx.index().len(), 3, "still three records");
    }

    // -----------------------------------------------------------------------------------------
    // §15.2 transaction invariants
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_committed_record_can_never_be_aborted() {
        let mut tx = transaction();
        let preparation = tx.prepare(&configure(), plan);
        let token = preparation.token().expect("prepared").clone();
        let head = preparation.record().unwrap().record_digest().clone();
        tx.commit(&token, &head).expect("commit");

        let error = tx
            .abort(&token)
            .expect_err("a committed record has no candidate to abort");
        assert_eq!(error.code, KernelFaultCode::TransactionConflict);
        assert!(!tx.is_poisoned(), "asking is not itself a corruption");
        assert_eq!(
            tx.head().map(|head| head.step_seq),
            Some(WireU64::ZERO),
            "the committed record stands"
        );
    }

    #[test]
    fn a_second_prepare_is_refused_while_a_candidate_is_outstanding() {
        let mut tx = transaction();
        let first = tx.prepare(&configure(), plan);
        let token = first.token().expect("prepared").clone();

        let second = tx.prepare(&start(), plan);
        assert_eq!(fault_of(&second), KernelFaultCode::TransactionConflict);
        assert_eq!(
            tx.outstanding_token(),
            Some(&token),
            "the first candidate is not displaced by the second attempt"
        );

        // the first candidate still commits
        let head = first.record().unwrap().record_digest().clone();
        tx.commit(&token, &head).expect("the first candidate wins");
    }

    #[test]
    fn effects_become_visible_only_when_their_record_is_durable() {
        let mut tx = transaction();
        run(&mut tx, &configure());

        let preparation = tx.prepare(&start(), plan);
        assert_eq!(
            preparation.step().unwrap().effects().len(),
            1,
            "the planned step carries the effect"
        );
        assert_eq!(
            tx.pending_effects().count(),
            0,
            "but nothing is pending until the record is durable"
        );

        let token = preparation.token().unwrap().clone();
        let head = preparation.record().unwrap().record_digest().clone();
        let committed = tx.commit(&token, &head).unwrap();
        assert_eq!(committed.published_effects().len(), 1);
        assert_eq!(tx.pending_effects().count(), 1);
    }

    #[test]
    fn an_aborted_candidate_publishes_nothing() {
        let mut tx = transaction();
        run(&mut tx, &configure());
        let preparation = tx.prepare(&start(), plan);
        let token = preparation.token().unwrap().clone();
        tx.abort(&token).unwrap();
        assert_eq!(tx.pending_effects().count(), 0);
        assert_eq!(tx.lifecycle(), OperationLifecycle::Configured);
    }

    #[test]
    fn the_operation_id_is_bound_by_the_genesis_record() {
        let mut tx = transaction();
        run(&mut tx, &configure());
        assert_eq!(tx.operation_id(), Some(&operation()));

        let foreign = WireEnvelope::new(
            OperationId::new("op-other").unwrap(),
            input_id("in-foreign"),
            WireU64::new(1_700_000_002_000),
            KernelInput::HostControl(HostControl {
                command: HostCommand::Cancel(CancelCommand {
                    reason: CancellationReason::User,
                    pending_call_ids: vec![],
                }),
            }),
        );
        assert_eq!(
            fault_of(&tx.prepare(&foreign, plan)),
            KernelFaultCode::OperationMismatch
        );
    }

    #[test]
    fn a_duplicate_input_id_with_a_different_payload_is_a_conflict() {
        let mut tx = transaction();
        run(&mut tx, &configure());
        run(&mut tx, &start());

        // same input_id, different canonical payload
        let mut divergent = start();
        divergent.input = KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Agent(RootAgentEntry {
                task: LogicalTask::new("write something else"),
                run_spec: None,
            }),
            initial_context: InitialContext::default(),
        });
        assert_eq!(
            fault_of(&tx.prepare(&divergent, plan)),
            KernelFaultCode::DuplicateInputConflict
        );
    }

    #[test]
    fn an_exact_replay_is_answered_before_the_clock_check() {
        let mut tx = transaction();
        run(&mut tx, &configure());
        let started = run(&mut tx, &start_at("in-start", 1_700_000_005_000));

        // the retry carries the original, now-stale, observation time
        let replay = tx.prepare(&start_at("in-start", 1_700_000_005_000), plan);
        assert_eq!(replay.step_seq(), Some(started.step_seq));

        // a genuinely new input from the past is still refused
        assert_eq!(
            fault_of(&tx.prepare(&cancel_at("in-past", 1_700_000_004_000), plan)),
            KernelFaultCode::ClockRegression
        );
    }

    #[test]
    fn a_terminal_closes_the_operation_to_every_later_input() {
        let (mut tx, _, effect_id) = started();
        assert_eq!(tx.pending_effects().count(), 1);

        let terminal = run(&mut tx, &cancel_at("in-cancel", 1_700_000_003_000));
        assert!(matches!(
            terminal.terminal(),
            Some(KernelTerminal::Cancelled(_))
        ));
        assert_eq!(tx.lifecycle(), OperationLifecycle::Cancelled);
        assert!(tx.lifecycle().is_terminal());
        assert_eq!(
            tx.pending_effects().count(),
            0,
            "a terminal leaves nothing waiting on the host"
        );

        // DEC-4: after a terminal every state-changing input is refused, resolutions and signal
        // deliveries included — a refused signal never reaches a queue, a journal or a step seq.
        let before = tx.clone();
        for envelope in [
            resolve_at("in-late", 1_700_000_004_000, &effect_id, provider_outcome()),
            start_at("in-restart", 1_700_000_004_000),
            signal_at("in-late-signal", 1_700_000_004_000),
        ] {
            assert_eq!(
                fault_of(&tx.prepare(&envelope, plan)),
                KernelFaultCode::InvalidLifecycle,
                "{} must be refused after a terminal",
                envelope.input_id
            );
        }
        assert_eq!(tx, before, "a refused input leaves the transaction alone");
    }

    // fixture: cancel-is-idempotent
    #[test]
    fn a_re_issued_cancellation_replays_the_terminal_it_already_committed() {
        let (mut tx, _, _) = started();
        let cancelled = run(&mut tx, &cancel_at("in-cancel", 1_700_000_003_000));
        let before = tx.clone();

        // §18.3 · a new input id carrying the same cancellation is the dedup branch, not a new
        // decision: the caller that did not hear the first answer hears the same one.
        let replay = tx.prepare(&cancel_at("in-cancel-again", 1_700_000_004_000), plan);
        assert!(
            matches!(replay, KernelPreparation::Replayed(_)),
            "a re-issued cancellation must replay, not be refused by the latch it created"
        );
        assert_eq!(replay.step_seq(), Some(cancelled.step_seq));
        assert_eq!(
            replay.record().unwrap().record_digest(),
            cancelled.record.record_digest(),
            "the replay points at the existing record, so no second record exists"
        );
        assert_eq!(tx, before, "a replay moves nothing");

        // an exact retry of the original input id is the other, isomorphic replay source
        let exact = tx.prepare(&cancel_at("in-cancel", 1_700_000_003_000), plan);
        assert_eq!(exact.step_seq(), Some(cancelled.step_seq));
        assert_eq!(tx, before);
    }

    #[test]
    fn configured_input_byte_limit_rejects_later_inputs_without_mutation() {
        let mut tx = transaction();
        let mut config = boot_config([EffectKindTag::CallProvider]);
        config.kernel_limits = Some(KernelLimits {
            max_input_bytes: Some(1_024),
            ..KernelLimits::default()
        });
        let configure = envelope(
            "in-configure-limited",
            1_700_000_000_000,
            KernelInput::ConfigureOperation(ConfigureOperation { config }),
        );
        run(&mut tx, &configure);

        let oversized = envelope(
            "in-oversized",
            1_700_000_001_000,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("x".repeat(4_096)),
                    run_spec: None,
                }),
                initial_context: InitialContext::default(),
            }),
        );
        let before = tx.clone();
        let rejected = tx.prepare(&oversized, plan);

        assert_eq!(fault_of(&rejected), KernelFaultCode::ResourceLimitExceeded);
        assert!(rejected.is_zero_mutation());
        assert_eq!(tx, before);
    }

    #[test]
    fn a_differing_cancellation_after_the_terminal_is_a_conflict_not_an_overwrite() {
        let (mut tx, _, _) = started();
        run(&mut tx, &cancel_at("in-cancel", 1_700_000_003_000));
        let before = tx.clone();

        let divergent = envelope(
            "in-cancel-other",
            1_700_000_004_000,
            KernelInput::HostControl(HostControl {
                command: HostCommand::Cancel(CancelCommand {
                    reason: CancellationReason::HostShutdown,
                    pending_call_ids: vec![],
                }),
            }),
        );
        assert_eq!(
            fault_of(&tx.prepare(&divergent, plan)),
            KernelFaultCode::DuplicateInputConflict,
            "the operation already ended for the first reason; the second cannot re-decide it"
        );
        assert_eq!(tx, before);
    }

    // -----------------------------------------------------------------------------------------
    // §15.3 effect rules (DEC-1, DEC-3, DEC-8)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_conflicting_resolution_of_a_settled_effect_fails_closed() {
        let (mut tx, _, effect_id) = started();
        run(
            &mut tx,
            &resolve_at(
                "in-resolve",
                1_700_000_002_000,
                &effect_id,
                provider_outcome(),
            ),
        );
        let before = tx.clone();

        let conflicting = tx.prepare(
            &resolve_at(
                "in-resolve-conflict",
                1_700_000_003_000,
                &effect_id,
                failure_outcome(),
            ),
            plan,
        );
        assert_eq!(
            fault_of(&conflicting),
            KernelFaultCode::UnexpectedEffectOutcome
        );
        assert_eq!(tx, before);
    }

    #[test]
    fn an_unknown_effect_id_fails_closed() {
        let (mut tx, _, _) = started();
        let unknown = EffectId::new("op-tx-1:step:99:effect:0").unwrap();
        assert_eq!(
            fault_of(&tx.prepare(
                &resolve_at(
                    "in-unknown",
                    1_700_000_002_000,
                    &unknown,
                    provider_outcome()
                ),
                plan
            )),
            KernelFaultCode::UnexpectedEffectOutcome
        );
    }

    #[test]
    fn a_resolution_carrying_another_effect_kinds_payload_fails_closed() {
        let (mut tx, _, effect_id) = started();
        let wrong_shape = EffectOutcome::Succeeded(EffectSucceeded {
            result: EffectSuccess::Tools(ToolsSuccess::default()),
        });
        assert_eq!(
            fault_of(&tx.prepare(
                &resolve_at("in-wrong", 1_700_000_002_000, &effect_id, wrong_shape),
                plan
            )),
            KernelFaultCode::UnexpectedEffectOutcome
        );
    }

    #[test]
    fn at_most_one_pending_effect_per_kind() {
        let (mut tx, _, _) = started();
        let before = tx.clone();

        // a planner that tries to publish a second provider call while one is pending
        let greedy = tx.prepare(&cancel_at("in-second", 1_700_000_002_000), |context| {
            Ok(publishing(
                "greedy",
                vec![effect(
                    "op-tx-1:step:2:effect:0",
                    &context.input.input_id,
                    EffectKind::CallProvider(CallProviderEffect::default()),
                )],
            ))
        });
        assert_eq!(fault_of(&greedy), KernelFaultCode::ResourceLimitExceeded);
        assert_eq!(tx, before, "the refusal is zero mutation");

        // and the same rule applies within a single step
        let doubled = tx.prepare(&cancel_at("in-double", 1_700_000_002_000), |context| {
            Ok(publishing(
                "doubled",
                vec![
                    effect(
                        "op-tx-1:step:2:effect:0",
                        &context.input.input_id,
                        EffectKind::SpawnTasks(SpawnTasksEffect::default()),
                    ),
                    effect(
                        "op-tx-1:step:2:effect:1",
                        &context.input.input_id,
                        EffectKind::SpawnTasks(SpawnTasksEffect::default()),
                    ),
                ],
            ))
        });
        assert_eq!(fault_of(&doubled), KernelFaultCode::ResourceLimitExceeded);
    }

    #[test]
    fn an_effect_kind_the_host_did_not_declare_is_refused_before_emission() {
        let mut tx = transaction();
        // this operation declares provider calls only
        run(
            &mut tx,
            &configure_at("in-configure", [EffectKindTag::CallProvider]),
        );
        let before = tx.clone();

        let refused = tx.prepare(&start(), |context| {
            Ok(publishing(
                "tools",
                vec![effect(
                    "op-tx-1:step:1:effect:0",
                    &context.input.input_id,
                    EffectKind::ExecuteTools(ExecuteToolsEffect::default()),
                )],
            ))
        });
        assert_eq!(fault_of(&refused), KernelFaultCode::UnsupportedEffect);
        assert_eq!(tx, before, "no record, no effect, no state change");
    }

    #[test]
    fn a_launch_token_is_never_minted_twice() {
        let (mut tx, _, effect_id) = started();
        let spawn = |id: &str, token: &str| {
            let effect_id = id.to_string();
            let launch_token = token.to_string();
            move |context: &PlanContext<'_>| {
                Ok(publishing(
                    "spawn",
                    vec![effect(
                        &effect_id,
                        &context.input.input_id,
                        EffectKind::SpawnTasks(SpawnTasksEffect {
                            tasks: vec![TaskLaunch {
                                task_id: TaskId::new("task-1").unwrap(),
                                attempt_id: AttemptId::new("task-1:attempt:1").unwrap(),
                                launch_token: LaunchToken::new(launch_token.clone()).unwrap(),
                                node_id: NodeId::new("node-1").unwrap(),
                                spec: LogicalAgentSpec::new("do the thing"),
                            }],
                            budget: None,
                        }),
                    )],
                ))
            }
        };

        let spawn_effect_id = EffectId::new("op-tx-1:step:2:effect:0").unwrap();
        run_with(
            &mut tx,
            &resolve_at(
                "in-resolve",
                1_700_000_002_000,
                &effect_id,
                provider_outcome(),
            ),
            spawn(spawn_effect_id.as_str(), "op-tx-1:launch:1"),
        );
        assert!(tx.knows_launch_token(&LaunchToken::new("op-tx-1:launch:1").unwrap()));

        // settle the spawn so the per-kind pending rule is not what refuses the relaunch
        run(
            &mut tx,
            &resolve_at(
                "in-spawned",
                1_700_000_003_000,
                &spawn_effect_id,
                EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::TasksSpawned(TasksSpawnedSuccess::default()),
                }),
            ),
        );

        // a second step re-using the same launch token would make the host's launch dedup answer
        // for two different launches
        let reused = tx.prepare(
            &cancel_at("in-relaunch", 1_700_000_004_000),
            spawn("op-tx-1:step:4:effect:0", "op-tx-1:launch:1"),
        );
        assert_eq!(fault_of(&reused), KernelFaultCode::TransactionConflict);

        // ...while a fresh token for a fresh launch is accepted
        let fresh = tx.prepare(
            &cancel_at("in-relaunch", 1_700_000_004_000),
            spawn("op-tx-1:step:4:effect:0", "op-tx-1:launch:2"),
        );
        assert!(fresh.token().is_some(), "{:?}", fresh.fault());
    }

    #[test]
    fn an_effect_id_is_never_minted_twice() {
        let (mut tx, _, effect_id) = started();
        let collision = tx.prepare(
            &resolve_at(
                "in-resolve",
                1_700_000_002_000,
                &effect_id,
                provider_outcome(),
            ),
            move |context| {
                Ok(publishing(
                    "collide",
                    vec![effect(
                        // the id the *start* step already minted
                        "op-tx-1:step:1:effect:0",
                        &context.input.input_id,
                        EffectKind::SpawnTasks(SpawnTasksEffect::default()),
                    )],
                ))
            },
        );
        assert_eq!(fault_of(&collision), KernelFaultCode::TransactionConflict);
    }

    // -----------------------------------------------------------------------------------------
    // §12.3 bounded tail and the retryable CheckpointRequired (GAP-2)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_full_tail_asks_for_a_checkpoint_and_the_retry_is_a_fresh_prepare() {
        let mut tx = bounded(TailBounds::new(1, 2, 1024, 1024 * 1024).unwrap());
        run(&mut tx, &configure());
        run(&mut tx, &start());
        assert_eq!(tx.tail_pressure(), TailPressure::Full);
        let before = tx.clone();

        let retry_envelope = cancel_at("in-cancel", 1_700_000_003_000);
        let refused = tx.prepare(&retry_envelope, plan);
        let fault = refused.fault().expect("rejected").clone();
        assert_eq!(fault.code, KernelFaultCode::CheckpointRequired);
        assert!(fault.is_retryable(), "the one retryable code (GAP-2)");
        assert!(refused.is_zero_mutation());
        assert_eq!(tx, before, "the input was never accepted");

        // the host checkpoints through the current head and acks it
        let boundary = tx.checkpoint_boundary().expect("a head exists");
        assert_eq!(boundary.through_step_seq, WireU64::new(1));
        let usage = tx.note_checkpoint_acked(&boundary).expect("ack");
        assert_eq!(
            usage,
            TailUsage::default(),
            "the covered prefix is reclaimed"
        );
        assert_eq!(tx.tail_pressure(), TailPressure::Nominal, "not a latch");

        // the same input_id retries as a brand-new prepare, not a DuplicateInputConflict
        let retried = tx.prepare(&retry_envelope, plan);
        assert!(retried.token().is_some(), "{:?}", retried.fault());
        assert_eq!(retried.record().unwrap().step_seq(), WireU64::new(2));
    }

    #[test]
    fn the_tail_bounds_the_byte_axis_too() {
        let mut tx = bounded(TailBounds::new(64, 128, 128, 512).unwrap());
        // the genesis record alone is far past a 512-byte tail
        let refused = tx.prepare(&configure(), plan);
        assert_eq!(fault_of(&refused), KernelFaultCode::CheckpointRequired);
        assert!(refused.fault().unwrap().is_retryable());
    }

    #[test]
    fn tail_bounds_refuse_an_incoherent_watermark() {
        assert_eq!(
            TailBounds::new(10, 4, 100, 100).unwrap_err().code,
            KernelFaultCode::InvalidConfig
        );
        assert_eq!(
            TailBounds::new(1, 4, 0, 0).unwrap_err().code,
            KernelFaultCode::InvalidConfig
        );
        assert_eq!(TailBounds::default(), TailBounds::DEFAULT);
    }

    #[test]
    fn the_tail_reports_its_soft_watermark_before_its_hard_limit() {
        let mut tx = bounded(TailBounds::new(2, 8, 1024 * 1024, 4 * 1024 * 1024).unwrap());
        assert_eq!(tx.tail_pressure(), TailPressure::Nominal);
        run(&mut tx, &configure());
        assert_eq!(tx.tail_pressure(), TailPressure::Nominal);
        run(&mut tx, &start());
        assert_eq!(tx.tail_pressure(), TailPressure::Watermark);
    }

    /// §22.14 — the checkpoint boundary and the transaction candidate share no slot: neither
    /// blocks the other.
    #[test]
    fn a_checkpoint_boundary_neither_blocks_nor_is_blocked_by_a_candidate() {
        let (mut tx, records, _) = started();

        let preparation = tx.prepare(&cancel_at("in-cancel", 1_700_000_003_000), plan);
        let token = preparation.token().expect("prepared").clone();
        let candidate_digest = preparation.record().unwrap().record_digest().clone();

        let boundary = tx.checkpoint_boundary().expect("a head exists");
        assert_eq!(
            boundary.covered_head,
            *records[1].record_digest(),
            "the boundary follows the durable head, not the outstanding candidate"
        );
        assert_ne!(boundary.covered_head, candidate_digest);

        // installing/acking a checkpoint while a transaction is in flight is legal
        tx.note_checkpoint_acked(&boundary).expect("ack");
        assert_eq!(tx.tail_usage(), TailUsage::default());
        assert!(tx.has_candidate(), "the candidate survived the checkpoint");

        // ...and the in-flight transaction still commits, staying as tail after the boundary
        let committed = tx.commit(&token, &candidate_digest).expect("commit");
        assert_eq!(committed.step_seq, WireU64::new(2));
        assert_eq!(
            tx.tail_usage().records,
            1,
            "records after the candidate are tail"
        );
    }

    #[test]
    fn a_checkpoint_that_covers_no_prefix_of_this_journal_is_refused() {
        let (mut tx, _, _) = started();
        let bogus = CheckpointBoundary {
            through_step_seq: WireU64::new(1),
            covered_head: canonical_digest(b"another operation's head"),
        };
        assert_eq!(
            tx.note_checkpoint_acked(&bogus).unwrap_err().code,
            KernelFaultCode::CheckpointIncompatible
        );
        assert_eq!(tx.tail_usage().records, 2, "nothing was reclaimed");
    }

    // -----------------------------------------------------------------------------------------
    // rebuild (§8.3 lines 5–6, §12.2)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_rebuild_reproduces_every_step_digest_and_the_next_transition() {
        let mut live = transaction();
        let mut journal = Vec::new();
        journal.push(run(&mut live, &configure()).record);
        journal.push(run(&mut live, &start()).record);
        let effect_id = provider_effect_id(WireU64::new(1));
        journal.push(
            run(
                &mut live,
                &resolve_at(
                    "in-resolve",
                    1_700_000_002_000,
                    &effect_id,
                    provider_outcome(),
                ),
            )
            .record,
        );

        let mut rebuilt = Tx::rebuild_from_records(
            &journal,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(&journal),
            plan,
        )
        .expect("the journal rebuilds");

        assert_eq!(rebuilt.head(), live.head());
        assert_eq!(rebuilt.lifecycle(), live.lifecycle());
        assert_eq!(rebuilt.config(), live.config());
        assert_eq!(rebuilt.tail_usage(), live.tail_usage());
        for record in &journal {
            let step = rebuilt
                .committed_step(record.input_id())
                .expect("every replayed step is recoverable");
            record
                .verify_step(step)
                .expect("the rebuilt step matches the frozen digest");
            assert_eq!(Some(step), live.committed_step(record.input_id()));
        }

        // the uninterrupted path and the rebuilt path produce the same next record
        let next = cancel_at("in-cancel", 1_700_000_003_000);
        let uninterrupted = live.prepare(&next, plan);
        let after_rebuild = rebuilt.prepare(&next, plan);
        assert_eq!(
            after_rebuild.record().unwrap().step_digest(),
            uninterrupted.record().unwrap().step_digest()
        );
        assert_eq!(
            after_rebuild.record().unwrap().record_digest(),
            uninterrupted.record().unwrap().record_digest()
        );
    }

    #[test]
    fn a_rebuild_refuses_a_broken_chain() {
        let mut live = transaction();
        let genesis = run(&mut live, &configure()).record;
        let started = run(&mut live, &start()).record;
        let resolved = run(
            &mut live,
            &resolve_at(
                "in-resolve",
                1_700_000_002_000,
                &provider_effect_id(WireU64::new(1)),
                provider_outcome(),
            ),
        )
        .record;

        // a gap in the chain
        let gapped = vec![genesis.clone(), resolved.clone()];
        let error = Tx::rebuild_from_records(
            &gapped,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(&gapped),
            plan,
        )
        .expect_err("a chain with a hole is not a journal");
        assert_eq!(error.code, KernelFaultCode::RecordCorrupted);

        // a record whose step this binary no longer reproduces
        let intact = vec![genesis, started, resolved];
        let error = Tx::rebuild_from_records(
            &intact,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(&intact),
            |context| {
                let mut step = plan(context)?;
                step.plan.push_str(" (drifted)");
                Ok(step)
            },
        )
        .expect_err("a drifted planner must not silently resume");
        assert_eq!(error.code, KernelFaultCode::RecordCorrupted);
        assert!(
            error.message.contains("step"),
            "the fault names the digest that disagreed: {}",
            error.message
        );
    }

    #[test]
    fn a_rebuild_of_an_empty_journal_is_a_fresh_operation() {
        let rebuilt = Tx::rebuild_from_records(
            &[],
            ConfigDefaults::default(),
            InMemoryRecordIndex::new(),
            plan,
        )
        .expect("an empty journal is a legal starting point");
        assert_eq!(rebuilt.lifecycle(), OperationLifecycle::Created);
        assert_eq!(rebuilt.head(), None);
    }

    /// A journal the runtime has not replayed cannot be answered from memory: the step is not
    /// durable, so the honest answer is "rebuild first", not a fabricated replay.
    #[test]
    fn a_replay_of_a_record_this_runtime_never_saw_demands_a_rebuild() {
        let mut source = transaction();
        let genesis = run(&mut source, &configure()).record;

        let mut cold = KernelTransaction::<TestStep, _>::new(
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(&[genesis]),
        );
        assert_eq!(
            fault_of(&cold.prepare(&configure(), plan)),
            KernelFaultCode::RecordCorrupted
        );
    }

    #[test]
    fn a_poisoned_transaction_refuses_every_call() {
        let (mut tx, _, _) = started();
        let preparation = tx.prepare(&cancel_at("in-cancel", 1_700_000_003_000), plan);
        let token = preparation.token().unwrap().clone();
        tx.note_append_conflict(&token, None);

        assert!(tx.is_poisoned());
        assert_eq!(
            tx.poison().map(|fault| fault.code),
            Some(KernelFaultCode::TransactionConflict)
        );
        assert_eq!(
            fault_of(&tx.prepare(&start_at("in-any", 1_700_000_009_000), plan)),
            KernelFaultCode::TransactionConflict
        );
        let boundary = CheckpointBoundary {
            through_step_seq: WireU64::new(1),
            covered_head: canonical_digest(b"whatever"),
        };
        assert_eq!(
            tx.note_checkpoint_acked(&boundary).unwrap_err().code,
            KernelFaultCode::TransactionConflict
        );
    }
}
