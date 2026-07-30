//! The logical checkpoint and its bounded tail (spec §12).
//!
//! A checkpoint is *not* a snapshot of the kernel's internals. It is a stable, versioned DTO whose
//! shape is a contract in its own right, and every design rule below exists because the historical
//! `KernelSnapshot` broke it:
//!
//! 1. **Nothing here is derived from a private layout.** [`LogicalKernelState`] is built by an
//!    explicit projection — [`LogicalStateProjection`] from the semantic driver, the transition
//!    partition from the transaction — so a field added to `LoopStateMachine` cannot silently
//!    change the checkpoint format, and a field this DTO needs cannot silently disappear. The old
//!    snapshot serialised `last_step` (a whole `KernelStep`, `RenderedContext` and all), which made
//!    the blob a function of the *rendered prompt* rather than of the state.
//! 2. **Every piece of correctness state has exactly one home.** The four partitions of §12.1 —
//!    transition / syscall / scheduler / context_vm — partition the state, they do not overlap it:
//!    pending effects, the input replay ledger and the terminal live in `transition`, task attempts
//!    in `scheduler`, P3 handles in `context_vm`, and the checkpoint header repeats none of them.
//!    `single_ownership_is_structural` proves it by scanning the serialised document.
//! 3. **The bounded tail is exact.** `tail_inputs` covers `(base_step_seq, through_step_seq]` with
//!    no hole, no duplicate and nothing outside the range — checked at construction, so a
//!    checkpoint that would replay a different history than the journal did is not constructible.
//! 4. **Three digests, three questions.** `state_digest` answers "is this the logical state that
//!    was captured", `tail_digest` answers "is this the tail that was captured", and
//!    `checkpoint_digest` answers "is this the whole checkpoint, header included". They all use the
//!    record layer's canonical bytes, so a host validator that already implements §7.1.1 for
//!    records needs no second serialiser.
//!
//! What this module deliberately does **not** do: install, restore, rebase or ack. §12.3's second
//! half is Task 16. What exists here is *generation* — [`KernelTransaction::checkpoint_candidate`]
//! and the shapes it produces — plus the verification a restore will call into.

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use super::KERNEL_CHECKPOINT_VERSION;
use super::config::ResolvedOperationConfig;
use super::effect::{Digest, KernelEffect, LaunchToken, wire_opaque_ref};
use super::envelope::{AbiRevision, OperationLifecycle};
use super::fault::{KernelFault, KernelFaultCode};
use super::record::{NormalizedInput, RecordError, canonical_bytes, canonical_digest};
use super::root::{ExecutionFocus, LogicalAgentSpec, LogicalTask, RootKind};
use super::scalar::{
    AttemptId, BoundedJson, CanonicalBytes, EffectId, InputId, MemoryBindingId, NodeId,
    OperationId, SCALAR_ERROR_MARKER, SignalId, TaskId, WireScalarError, WireU64, WorkflowId,
};
use super::syscall::MemoryKind;
use super::terminal::KernelTerminal;

// ---------------------------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------------------------

/// Prefix of every checkpoint-layer rejection, so all four hosts classify on one marker.
pub const CHECKPOINT_ERROR_MARKER: &str = "kernel checkpoint rejected";

/// Why a checkpoint could not be assembled, decoded or verified.
///
/// The split is the recovery ladder, not the field that failed. `Incompatible` means "this blob is
/// not for this kernel or not for this operation" — a host answers it by looking for a different
/// checkpoint. `Corrupted` means "this blob claims to be ours and does not hold together" — the
/// only answer is an older checkpoint plus more tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    /// Wrong checkpoint revision, wrong ABI revision, wrong operation, or a genesis/head this
    /// operation never had.
    Incompatible(String),
    /// A digest disagrees with the bytes it summarises, or the bounded tail does not cover
    /// `(base_step_seq, through_step_seq]` exactly.
    Corrupted(String),
    /// The value has no canonical byte representation at all.
    NotCanonical(String),
}

impl CheckpointError {
    pub fn message(&self) -> &str {
        match self {
            Self::Incompatible(message)
            | Self::Corrupted(message)
            | Self::NotCanonical(message) => message,
        }
    }

    pub fn code(&self) -> KernelFaultCode {
        match self {
            Self::Incompatible(_) => KernelFaultCode::CheckpointIncompatible,
            Self::Corrupted(_) => KernelFaultCode::CheckpointCorrupted,
            Self::NotCanonical(_) => KernelFaultCode::MalformedEnvelope,
        }
    }

    /// Host-facing projection (§7.13).
    pub fn fault(&self) -> KernelFault {
        KernelFault::new(self.code(), self.to_string())
    }
}

impl fmt::Display for CheckpointError {
    /// The rendered form names its own code.
    ///
    /// Not cosmetic: a checkpoint rejected inside `serde` reaches the caller as a *string*, and
    /// [`from_checkpoint_bytes`](KernelCheckpoint::from_checkpoint_bytes) has to recover the class
    /// from it. Printing the code is what keeps that recovery from being a guess based on which
    /// words happen to be in the message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{CHECKPOINT_ERROR_MARKER} ({}): {}",
            self.code().as_str(),
            self.message()
        )
    }
}

impl std::error::Error for CheckpointError {}

impl From<RecordError> for CheckpointError {
    fn from(error: RecordError) -> Self {
        Self::NotCanonical(error.message().to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// §12.1 · the checkpoint format revision
// ---------------------------------------------------------------------------------------------

/// The checkpoint format revision, fail-closed on the wire.
///
/// A separate axis from [`AbiRevision`] on purpose: the wire contract and the checkpoint layout
/// can move independently, and DEC-6 renamed the field to `checkpoint_version` precisely so this
/// value could start at 1 without colliding with the historical `KERNEL_SNAPSHOT_VERSION` values.
/// Deserialising anything else fails at the boundary, so "an unrecognised checkpoint version" can
/// never reach a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointRevision(u32);

impl CheckpointRevision {
    pub const CURRENT: Self = Self(KERNEL_CHECKPOINT_VERSION);

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for CheckpointRevision {
    fn default() -> Self {
        Self::CURRENT
    }
}

impl fmt::Display for CheckpointRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for CheckpointRevision {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0)
    }
}

impl<'de> Deserialize<'de> for CheckpointRevision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RevisionVisitor;

        impl Visitor<'_> for RevisionVisitor {
            type Value = CheckpointRevision;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "the kernel checkpoint revision {KERNEL_CHECKPOINT_VERSION}"
                )
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                if value == u64::from(KERNEL_CHECKPOINT_VERSION) {
                    Ok(CheckpointRevision::CURRENT)
                } else {
                    Err(E::custom(
                        CheckpointError::Incompatible(format!(
                            "unsupported checkpoint version {value}; this kernel reads only \
                             version {KERNEL_CHECKPOINT_VERSION}"
                        ))
                        .to_string(),
                    ))
                }
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                if value < 0 {
                    return Err(E::custom(
                        CheckpointError::Incompatible(format!(
                            "unsupported checkpoint version {value}"
                        ))
                        .to_string(),
                    ));
                }
                self.visit_u64(value as u64)
            }
        }

        deserializer.deserialize_u32(RevisionVisitor)
    }
}

wire_opaque_ref!(
    /// Handle the host returns to `ack_checkpoint` once the blob is durably installed (§12.3).
    ///
    /// It is **not** a [`KernelInput`](super::envelope::KernelInput) (§12.3 rule 4): acking is
    /// runtime maintenance, it writes no record, and a crash between install and ack is recovered
    /// from the installed checkpoint anyway. Deriving it from the checkpoint's own digest is what
    /// makes "ack a checkpoint that was never handed out" unrepresentable.
    CheckpointAckToken,
    "checkpoint ack token"
);

// ---------------------------------------------------------------------------------------------
// §12.1 · the bounded tail
// ---------------------------------------------------------------------------------------------

/// One accepted input inside a checkpoint's bounded tail.
///
/// It carries the normalised input (so the tail can be **replayed**) and the digest of the record
/// that input produced (so the replay can be **verified** against the journal it came from).
/// Neither alone is enough: digests without inputs make a checkpoint auditable but useless, and
/// inputs without digests make a restore that silently disagrees with the journal possible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalInput {
    pub step_seq: WireU64,
    pub record_digest: Digest,
    pub input: NormalizedInput,
}

impl CanonicalInput {
    /// Project one durable record into its tail entry.
    pub fn from_record(record: &super::record::KernelRecord) -> Result<Self, CheckpointError> {
        Ok(Self {
            step_seq: record.step_seq(),
            record_digest: record.record_digest().clone(),
            input: record.normalized_input()?,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// §12.1 · the four logical partitions
// ---------------------------------------------------------------------------------------------

/// The versioned logical state of one operation, partitioned as §12.1 requires.
///
/// The four fields are a *partition*: each piece of correctness state appears in exactly one of
/// them, and the checkpoint header above repeats none of it. That is the property the historical
/// snapshot lacked — it stored pending effects at the top level, again inside `last_step`, and a
/// third time in the resumed-outcome vectors, so "which copy is authoritative" was decided by
/// whichever restore path happened to run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalKernelState {
    pub transition: TransitionStateV1,
    pub syscall: SyscallStateV1,
    pub scheduler: SchedulerStateV1,
    pub context_vm: ContextVmStateV1,
}

/// §12.1 · operation lifecycle, execution focus, the effect ledger, input replay, cancellation and
/// the terminal.
///
/// The step sequence is deliberately **not** here: the checkpoint header already states
/// `base_step_seq` and `through_step_seq`, and §12.1's "the header must not duplicate sub-state"
/// cuts both ways.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionStateV1 {
    pub lifecycle: OperationLifecycle,
    /// The configuration the genesis record froze (§8.1, §15.2 item 8).
    ///
    /// Here rather than "read it off the genesis record", because §12.3 rule 6 lets an acked
    /// checkpoint reclaim the journal prefix *including genesis*: after that, the checkpoint is the
    /// only place the resolved configuration still exists, and every later step is planned against
    /// it. `genesis_digest` in the header keeps binding the identity; this carries the content.
    pub resolved_config: ResolvedOperationConfig,
    /// Immutable after the root start commits (§6.1.5).
    #[serde(default)]
    pub root_kind: Option<RootKind>,
    /// Where control is (§7.4). Moves only on a committed transition, so a checkpoint states it
    /// rather than deriving it.
    #[serde(default)]
    pub focus: Option<ExecutionFocus>,
    /// The operation's only clock fact (§11.2): the newest accepted `observed_at_ms`. A restore
    /// must not accept an input that precedes it.
    pub last_observed_at_ms: WireU64,
    /// Effects published by committed records and not yet resolved. **The** home of pending
    /// effects — no other partition, and not the header.
    #[serde(default)]
    pub pending_effects: Vec<KernelEffect>,
    /// Effects already answered, with the digest of the outcome that answered them. This is what
    /// makes a redelivered `ResolveEffect` a `Replayed` instead of a second record (DEC-1).
    #[serde(default)]
    pub resolved_effects: Vec<ResolvedEffectState>,
    /// Launch tokens the kernel minted with a `SpawnTasks` effect. Effect-resolution bookkeeping,
    /// not task state — the task table lives in [`SchedulerStateV1`].
    #[serde(default)]
    pub launch_tokens: Vec<LaunchTokenState>,
    /// §12.3 rule 7 · the input replay/dedupe ledger. An ack must never empty it: it is what turns
    /// a redelivery into an idempotent answer instead of a second durable record.
    #[serde(default)]
    pub accepted_inputs: Vec<AcceptedInputState>,
    /// The cancellation this operation already accepted, so a retry of it is answered rather than
    /// refused by the terminal it created.
    #[serde(default)]
    pub accepted_cancellation: Option<AcceptedCancellationState>,
    /// The committed terminal. **The** home of the terminal.
    #[serde(default)]
    pub terminal: Option<KernelTerminal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedEffectState {
    pub effect_id: EffectId,
    pub outcome_digest: Digest,
    pub input_id: InputId,
    pub step_seq: WireU64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchTokenState {
    pub launch_token: LaunchToken,
    pub step_seq: WireU64,
}

/// One entry of the replay ledger (§12.3 rules 7 and 10).
///
/// The digest is what makes the ledger answerable on its own. Below `base_step_seq` a restored
/// runtime holds no step and, once retention has reclaimed the prefix, no record either — so the
/// guarantee a redelivery gets down there is **idempotent acknowledgement, not step reproduction**:
/// "input X is step N, record D". That is exactly what a caller retrying a lost response needs, and
/// it is all a checkpoint has to remember.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedInputState {
    pub input_id: InputId,
    pub step_seq: WireU64,
    pub record_digest: Digest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedCancellationState {
    /// Digest of the canonical cancel command, so "the same cancellation" is decided by bytes.
    pub command_digest: Digest,
    pub input_id: InputId,
    pub step_seq: WireU64,
}

/// §12.1 · governance revision, the live policy, the rate-limit window, and the provider-tool
/// causation the P1 gate derives a caller from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyscallStateV1 {
    /// §13.2 · the revision two concurrent policy writers race on. `None` before the genesis
    /// record installs the policy.
    #[serde(default)]
    pub policy_revision: Option<WireU64>,
    /// The live-mutable configuration as patched so far. Distinct from the genesis record's
    /// resolved configuration, which is frozen — this is the value the gate reads today.
    #[serde(default)]
    pub live_config: Option<ResolvedOperationConfig>,
    /// §7.6 · the provider calls this operation is waiting on and the tool surface each one
    /// advertised. A tool call naming anything else has no causation to derive from.
    #[serde(default)]
    pub provider_calls: Vec<PendingProviderCallState>,
    /// Tool call ids that already produced a syscall. A causation is spent once.
    #[serde(default)]
    pub consumed_call_ids: Vec<String>,
    /// §22.13 · what the kernel authored for each pending memory write. The resolution reports
    /// these, never what the host echoes back.
    #[serde(default)]
    pub authored_memory_writes: Vec<AuthoredMemoryWriteState>,
    #[serde(default)]
    pub authored_memory_queries: Vec<AuthoredMemoryQueryState>,
    /// Rolling window of accepted memory-write timestamps, in the operation's own clock. The
    /// window is a gate input, so dropping it at a checkpoint would hand the run a fresh quota.
    #[serde(default)]
    pub memory_write_window_ms: Vec<WireU64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingProviderCallState {
    pub effect_id: EffectId,
    /// The task whose turn issued the call — the caller a syscall inherits.
    pub task_id: TaskId,
    pub exposed_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredMemoryWriteState {
    pub effect_id: EffectId,
    pub binding_id: MemoryBindingId,
    pub name: String,
    pub kind: MemoryKind,
    pub size_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredMemoryQueryState {
    pub effect_id: EffectId,
    pub binding_id: MemoryBindingId,
    pub text: String,
    pub requested_k: u32,
}

/// §12.1 · the P2 plane: task control blocks and their attempts, budgets and waits, the workflow
/// graph, queued signals plus dedupe memory, and the milestone cascade.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerStateV1 {
    pub turn: u32,
    pub total_tokens: WireU64,
    pub rounds_completed: u32,
    pub subagents_spawned: u32,
    /// First observed clock of the run, the anchor the wall-budget axis measures against.
    #[serde(default)]
    pub started_at_ms: Option<WireU64>,
    /// §13.2 · the wall-clock budget an `UpdateDeadline` command last projected onto the axis.
    /// Not derivable from the configuration — the command sets a duration measured from
    /// [`Self::started_at_ms`] — so a restore that dropped it would un-bound the run.
    #[serde(default)]
    pub wall_budget_ms: Option<WireU64>,
    /// The task table. **The** home of task lifecycle and, with [`Self::attempts`], of task
    /// identity.
    #[serde(default)]
    pub tasks: Vec<TaskControlState>,
    /// §10.4 · the attempt the kernel minted for each live task. A completion naming an attempt
    /// that is not here has no authority, which is why the mapping is checkpointed rather than
    /// re-derived from task ids.
    #[serde(default)]
    pub attempts: Vec<TaskAttemptState>,
    #[serde(default)]
    pub workflow: Option<WorkflowGraphState>,
    #[serde(default)]
    pub queued_signals: Vec<QueuedSignalState>,
    /// Router dedupe keys in eviction order, including keys for already-dispatched signals.
    #[serde(default)]
    pub signal_dedupe_keys: Vec<String>,
    #[serde(default)]
    pub milestone: Option<MilestoneState>,
    /// Session-disorder measurement and alert-gate state. These values intentionally survive a
    /// canonical crash restore even though they do not participate in a same-process turn
    /// rollback: otherwise the first post-restore sample forgets prior failures and rollbacks.
    #[serde(default)]
    pub entropy: EntropyState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntropyState {
    /// Oldest to newest, bounded by the kernel entropy window.
    #[serde(default)]
    pub window: Vec<EntropyTurnState>,
    /// Rollbacks observed after the newest completed turn.
    pub rollbacks_pending: u32,
    /// Threshold watch hysteresis state.
    pub disarmed: bool,
    /// Most recent alert turn, retained after re-arming for cooldown enforcement.
    #[serde(default)]
    pub last_alert_turn: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntropyTurnState {
    pub errored_results: u32,
    pub total_results: u32,
    pub rollbacks: u32,
}

/// One task control block, projected. The lifecycle travels as its label rather than as the
/// internal enum: `TaskLifecycle::Done(TerminationReason)` is a semantic-kernel shape, and a
/// checkpoint that mirrored it would be a checkpoint of a private layout.
///
/// The label alone is not *invertible*, though, and Task 16 needs it to be: a restore that rebuilt
/// a finished task without the reason it finished for would hand the next transition a different
/// task table than the uninterrupted run had. So the two data-carrying lifecycles travel with their
/// data beside the label — `termination` for `done`, `waiting_on` for a `sub_agent_join` wait —
/// rather than by mirroring the internal enum's shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskControlState {
    pub task_id: TaskId,
    #[serde(default)]
    pub parent_task_id: Option<TaskId>,
    pub lifecycle: String,
    /// Why a `done` task is done. `None` for every other lifecycle.
    #[serde(default)]
    pub termination: Option<String>,
    #[serde(default)]
    pub wait: Option<String>,
    /// The children a `sub_agent_join` wait is blocked on. Empty for every other wait.
    #[serde(default)]
    pub waiting_on: Vec<TaskId>,
    #[serde(default)]
    pub capability_ids: Vec<String>,
    /// Sub-agent process identity and join state. `None` only for the root task.
    #[serde(default)]
    pub process: Option<ChildProcessState>,
    pub tokens_used: WireU64,
    pub turns_used: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildProcessState {
    pub role: String,
    pub isolation: String,
    pub context_inheritance: String,
    /// Present once the child has joined; the task lifecycle alone does not reproduce its output.
    #[serde(default)]
    pub join_result: Option<BoundedJson>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAttemptState {
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowGraphState {
    pub workflow_id: WorkflowId,
    /// The complete, index-ordered DAG and its runtime state. The semantic scheduler rebuilds its
    /// private reverse edges, ready heap and agent lookup from this projection during restore.
    #[serde(default)]
    pub nodes: Vec<WorkflowNodeState>,
}

/// One workflow node as source state rather than a snapshot of `TaskGraph` internals.
///
/// `kind` is explicit even though the canonical ABI currently admits only `spawn`: a checkpoint
/// must fail closed if a future producer writes a control-flow kind this revision cannot rebuild.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowNodeState {
    pub node_id: NodeId,
    pub task: LogicalTask,
    #[serde(default)]
    pub depends_on: Vec<NodeId>,
    #[serde(default)]
    pub run_spec: Option<LogicalAgentSpec>,
    pub kind: String,
    pub status: String,
    /// The deterministic child identity while this node is running.
    #[serde(default)]
    pub active_agent_id: Option<String>,
    #[serde(default)]
    pub iterations_completed: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedSignalState {
    pub signal_id: SignalId,
    pub source: String,
    pub signal_type: String,
    pub urgency: String,
    pub summary: String,
    #[serde(default)]
    pub payload: BoundedJson,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub deadline_ms: Option<WireU64>,
    #[serde(default)]
    pub coalesce_key: Option<String>,
    pub coalesced_count: u32,
    #[serde(default)]
    pub recipient: Option<String>,
    pub timestamp_ms: WireU64,
    pub deadline_escalated: bool,
    /// Every business key represented by this queue entry after coalescing.
    #[serde(default)]
    pub dedupe_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MilestoneState {
    /// The contract whose cascade is installed. `phase_id` is unique only inside it, so the pair
    /// is the host's complete lookup key (§7.9 note 6).
    pub contract_id: String,
    #[serde(default)]
    pub phase_id: Option<String>,
    pub complete: bool,
    /// Consecutive blocks on the current phase — the retry budget. A restore that reset it would
    /// hand a stalled cascade a fresh set of attempts.
    #[serde(default)]
    pub blocked_count: u32,
}

/// §12.1 · the P3 plane: the handle table and its allocator, skills and their leases, the
/// knowledge slots, the signal partition and the compaction/renewal clocks.
///
/// What is here is everything the context VM *cannot re-derive*: identity (handle ids and the
/// allocator that mints them), leases, pin/evict marks, the pending page-in verification targets,
/// the clocks the decay ladders read — and, since Task 16, the **stored messages** themselves.
///
/// The message projection is what makes §12.2 true (adjudication §5q-2). Task 15 carried only token
/// counts and lengths on the theory that a tail replay could rebuild the bodies; it cannot, because
/// a tail that starts above genesis never replays the inputs that produced the older messages. And
/// as long as the bodies were unrecoverable, acking a checkpoint and pruning the journal prefix
/// destroyed the rendering input for good. So they are here — as [`StoredMessageState`], a *source*
/// projection. §15.2's ban is on the derived artefacts, the `KernelStep` and the `RenderedContext`;
/// nothing here is either. A body that is over §7.10's inline threshold is **not** inlined: an
/// `External`/`PagedOut` residency travels as its handle reference and digest, exactly as it does in
/// working context.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextVmStateV1 {
    /// **The** home of P3 handle identity and residency.
    #[serde(default)]
    pub handles: Vec<HandleState>,
    /// The monotonic allocator. Checkpointing it is what stops a restored kernel from re-issuing a
    /// handle id that an outstanding effect still addresses.
    pub next_handle_id: u32,
    /// §7.10 rule 4 · the digest each pending `LoadPayload` will verify its body against. Held
    /// here rather than re-read from the handle table at resolution time, so a residency that moved
    /// in between cannot change what a page-in is checked against.
    #[serde(default)]
    pub pending_payload_loads: Vec<PendingPayloadLoadState>,
    #[serde(default)]
    pub active_skills: Vec<SkillLeaseState>,
    #[serde(default)]
    pub knowledge: Vec<KnowledgeSlotState>,
    #[serde(default)]
    pub signals: Vec<String>,
    /// §5q-2 · the stored messages of the system and history partitions, in render order. **The**
    /// home of message bodies; the knowledge partition carries its own inside
    /// [`KnowledgeSlotState`], because a knowledge entry is an identified slot rather than a
    /// positional message.
    #[serde(default)]
    pub messages: Vec<StoredMessageState>,
    /// The durable task board (goal / plan / progress / directives). It renders into the prompt like
    /// a message does, survives compression by construction, and is the one partition of §12.1's P3
    /// plane that is neither a message list nor a handle.
    pub task_state: LogicalTaskState,
    pub partition_tokens: PartitionTokenState,
    pub history_len: u32,
    /// Message-count boundary projected as `frozen_prefix_len` on future provider effects.
    #[serde(default)]
    pub frozen_history_len: u32,
    pub last_activity_ms: WireU64,
    #[serde(default)]
    pub last_compact_ms: Option<WireU64>,
}

/// Which partition a [`StoredMessageState`] belongs to.
///
/// Only the two positional partitions: knowledge entries are keyed slots with their own lifecycle
/// flags, so they live in [`KnowledgeSlotState`] instead of being a third value here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePartition {
    System,
    History,
}

/// One stored message, projected (§12.1, adjudication §5q-2).
///
/// This is the *source* the renderer reads, not the rendered result: no prompt assembly, no
/// residency projection, no salience footer. A restore rebuilds the partitions from these and then
/// renders exactly what an uninterrupted run would have rendered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMessageState {
    pub partition: MessagePartition,
    /// `system` | `user` | `assistant` | `tool`.
    pub role: String,
    pub body: StoredMessageBody,
    /// The calls an assistant message asked for — the half of "tool association" that points
    /// forward.
    #[serde(default)]
    pub tool_calls: Vec<LogicalToolCall>,
    /// The cached token count the partition counter was built from. Carried rather than recomputed
    /// so a restore reproduces the same budget arithmetic even if the tokenizer moved.
    pub tokens: u32,
}

/// A message body, inline or by reference (§7.10).
///
/// The reference arm is the whole point: a tool result that was over the inline threshold when it
/// was generated (`External`) or that left working context under pressure (`PagedOut`) is already
/// represented in context by a preview plus a handle, and a checkpoint that re-inlined it would put
/// bytes back into the journal that §7.10 spent an effect kind keeping out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum StoredMessageBody {
    /// A body small enough to live in context.
    Inline(InlineMessageBody),
    /// A body that lives with the host. Carries the reference and the digest that verifies a
    /// page-in, never the bytes.
    Reference(ReferencedMessageBody),
    /// A multimodal body the text projection cannot express (image or audio parts), carried as the
    /// canonical JSON of its content.
    ///
    /// It exists so the projection is never *silently* lossy: a body that does not reduce to text
    /// travels whole rather than being flattened to the text parts that happen to be next to it.
    Structured(StructuredMessageBody),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineMessageBody {
    pub text: String,
    /// For a `tool` message: the call this result answers, and whether it failed.
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencedMessageBody {
    /// The P3 handle that addresses the body. Its residency in [`ContextVmStateV1::handles`] is
    /// what a page-in reads.
    pub handle_id: u32,
    /// The digest a page-in must reproduce (§7.10 rule 4).
    pub digest: String,
    /// What is actually resident: the preview the model can see. Never the whole body.
    pub preview: String,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredMessageBody {
    /// Canonical JSON of the message's `content`.
    pub content_json: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalToolCall {
    pub call_id: String,
    pub name: String,
    /// Canonical JSON text of the arguments. A string rather than a `Value` so the checkpoint's
    /// canonical bytes are the arguments' canonical bytes, with no second serialiser in between.
    pub arguments: String,
}

/// §12.1 · the durable task board, projected.
///
/// Explicitly re-declared rather than reusing `crate::context::task_state::TaskState`: that type is
/// semantic-kernel state whose serde shape is free to move, and §12.1's first rule is that a field
/// added there must not silently change the checkpoint format.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalTaskState {
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub criteria: Vec<String>,
    #[serde(default)]
    pub plan: Vec<LogicalPlanStep>,
    #[serde(default)]
    pub current_step: Option<u32>,
    #[serde(default)]
    pub progress: String,
    #[serde(default)]
    pub scratchpad: String,
    #[serde(default)]
    pub blocked_on: Vec<String>,
    #[serde(default)]
    pub directives: Vec<String>,
    #[serde(default)]
    pub preserved_refs: Vec<String>,
    #[serde(default)]
    pub recent_actions: Vec<String>,
    #[serde(default)]
    pub compression_log: Vec<LogicalCompressionEntry>,
    #[serde(default)]
    pub compression_log_dropped: WireU64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalPlanStep {
    pub label: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalCompressionEntry {
    pub action: String,
    pub summary: String,
}

/// One P3 handle. `residency` is the label plus the locator fields that residency carries, so the
/// DTO neither mirrors the internal enum's shape nor loses what a page-in needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandleState {
    pub handle_id: u32,
    pub kind: String,
    pub residency: String,
    #[serde(default)]
    pub payload_ref: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub original_size: Option<WireU64>,
    pub tokens: u32,
    /// Link back to the source object in working context (a tool `call_id` for a tool result).
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingPayloadLoadState {
    pub effect_id: EffectId,
    pub handle_id: String,
    pub digest: String,
    #[serde(default)]
    pub original_size: Option<WireU64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLeaseState {
    pub skill: String,
    /// `None` = permanent; otherwise the turn the lease expires on.
    #[serde(default)]
    pub lease_until_turn: Option<u32>,
}

/// One knowledge slot, body included.
///
/// A knowledge entry has identity (its key) and its own lifecycle flags, so it is not a positional
/// [`StoredMessageState`] — but it renders into the prompt all the same, which is why Task 16 gave it
/// the same body projection. Without it, a restore rebuilt the *shape* of the knowledge partition
/// and none of its content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSlotState {
    /// `None` = an unkeyed append; keyed entries upsert.
    #[serde(default)]
    pub key: Option<String>,
    pub role: String,
    pub body: StoredMessageBody,
    pub tokens: u32,
    pub pinned: bool,
    pub evict_at_boundary: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionTokenState {
    pub system: u32,
    pub knowledge: u32,
    pub history: u32,
}

/// What the semantic driver contributes to a checkpoint.
///
/// Three of the four partitions plus the two transition fields the driver — not the transaction —
/// owns. It is a value, not a borrow of the driver: the checkpoint is built from an explicit
/// projection, never from a live reference into the engine.
#[derive(Debug, Clone, PartialEq)]
pub struct LogicalStateProjection {
    pub root_kind: Option<RootKind>,
    pub focus: Option<ExecutionFocus>,
    pub syscall: SyscallStateV1,
    pub scheduler: SchedulerStateV1,
    pub context_vm: ContextVmStateV1,
}

// ---------------------------------------------------------------------------------------------
// §12.1 · the checkpoint
// ---------------------------------------------------------------------------------------------

/// Everything [`KernelCheckpoint::assemble`] needs. A struct rather than eight positional
/// arguments, because two of them are step sequences and two are digests.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckpointDraft {
    pub operation_id: OperationId,
    pub genesis_digest: Digest,
    pub base_step_seq: WireU64,
    pub base_record_digest: Digest,
    pub through_step_seq: WireU64,
    pub covered_transaction_head_digest: Digest,
    pub logical_state: LogicalKernelState,
    pub tail_inputs: Vec<CanonicalInput>,
}

/// One logical checkpoint (§12.1).
///
/// Fields are private and every digest is computed by [`Self::assemble`], so there is no
/// constructor that takes a digest and "the host recomputed the hash and disagreed" is not a
/// reachable state — the same discipline [`KernelRecord`](super::record::KernelRecord) uses.
/// Decoding goes through the same verification, which is why a tampered blob fails at the boundary
/// rather than half-way through a restore.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KernelCheckpoint {
    checkpoint_version: CheckpointRevision,
    abi_version: AbiRevision,
    operation_id: OperationId,
    /// The digest of the operation's genesis record — its identity. A checkpoint built from
    /// another operation's journal therefore cannot be installed by accident.
    genesis_digest: Digest,
    /// The step the logical state below describes.
    base_step_seq: WireU64,
    /// The record digest at `base_step_seq` — the chain anchor a tail replay starts from.
    ///
    /// The other end of the range the header already states. Without it a rebase could be
    /// *verified* and never *replayed*: the record before the first tail entry is exactly the one an
    /// acked checkpoint is allowed to have pruned, so its digest has to travel with the tail that
    /// depends on it. For a full-state checkpoint it is the covered head, because `base == through`.
    base_record_digest: Digest,
    /// The step the checkpoint covers once its tail is replayed.
    through_step_seq: WireU64,
    /// The record digest at `through_step_seq`. §12.3 rule 2: install checks that this names the
    /// through step, **not** that it is still the current head.
    covered_transaction_head_digest: Digest,
    logical_state: LogicalKernelState,
    tail_inputs: Vec<CanonicalInput>,
    state_digest: Digest,
    tail_digest: Digest,
    checkpoint_digest: Digest,
}

/// The digested body: every field of a checkpoint except the digest that summarises it.
#[derive(Serialize)]
struct CheckpointBody<'a> {
    checkpoint_version: CheckpointRevision,
    abi_version: AbiRevision,
    operation_id: &'a OperationId,
    genesis_digest: &'a Digest,
    base_step_seq: WireU64,
    base_record_digest: &'a Digest,
    through_step_seq: WireU64,
    covered_transaction_head_digest: &'a Digest,
    logical_state: &'a LogicalKernelState,
    tail_inputs: &'a [CanonicalInput],
    state_digest: &'a Digest,
    tail_digest: &'a Digest,
}

impl KernelCheckpoint {
    /// Build a checkpoint, computing all three digests and checking the tail covers
    /// `(base_step_seq, through_step_seq]` exactly.
    pub fn assemble(draft: CheckpointDraft) -> Result<Self, CheckpointError> {
        let CheckpointDraft {
            operation_id,
            genesis_digest,
            base_step_seq,
            base_record_digest,
            through_step_seq,
            covered_transaction_head_digest,
            logical_state,
            tail_inputs,
        } = draft;

        check_tail(
            &operation_id,
            base_step_seq,
            &base_record_digest,
            through_step_seq,
            &covered_transaction_head_digest,
            &tail_inputs,
        )?;

        let state_digest = canonical_digest(canonical_bytes(&logical_state)?.as_slice());
        let tail_digest = canonical_digest(canonical_bytes(&tail_inputs)?.as_slice());
        let checkpoint_digest = Self::body_digest(&CheckpointBody {
            checkpoint_version: CheckpointRevision::CURRENT,
            abi_version: AbiRevision::CURRENT,
            operation_id: &operation_id,
            genesis_digest: &genesis_digest,
            base_step_seq,
            base_record_digest: &base_record_digest,
            through_step_seq,
            covered_transaction_head_digest: &covered_transaction_head_digest,
            logical_state: &logical_state,
            tail_inputs: &tail_inputs,
            state_digest: &state_digest,
            tail_digest: &tail_digest,
        })?;

        Ok(Self {
            checkpoint_version: CheckpointRevision::CURRENT,
            abi_version: AbiRevision::CURRENT,
            operation_id,
            genesis_digest,
            base_step_seq,
            base_record_digest,
            through_step_seq,
            covered_transaction_head_digest,
            logical_state,
            tail_inputs,
            state_digest,
            tail_digest,
            checkpoint_digest,
        })
    }

    fn body_digest(body: &CheckpointBody<'_>) -> Result<Digest, CheckpointError> {
        Ok(canonical_digest(canonical_bytes(body)?.as_slice()))
    }

    // ----- read-only accessors -----

    pub fn checkpoint_version(&self) -> u32 {
        self.checkpoint_version.get()
    }

    pub fn abi_version(&self) -> u32 {
        self.abi_version.get()
    }

    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    pub fn genesis_digest(&self) -> &Digest {
        &self.genesis_digest
    }

    pub fn base_step_seq(&self) -> WireU64 {
        self.base_step_seq
    }

    pub fn base_record_digest(&self) -> &Digest {
        &self.base_record_digest
    }

    pub fn through_step_seq(&self) -> WireU64 {
        self.through_step_seq
    }

    pub fn covered_transaction_head_digest(&self) -> &Digest {
        &self.covered_transaction_head_digest
    }

    pub fn logical_state(&self) -> &LogicalKernelState {
        &self.logical_state
    }

    pub fn tail_inputs(&self) -> &[CanonicalInput] {
        &self.tail_inputs
    }

    pub fn state_digest(&self) -> &Digest {
        &self.state_digest
    }

    pub fn tail_digest(&self) -> &Digest {
        &self.tail_digest
    }

    pub fn checkpoint_digest(&self) -> &Digest {
        &self.checkpoint_digest
    }

    // ----- projection -----

    /// Canonical bytes of the whole checkpoint — the blob a host persists.
    pub fn checkpoint_bytes(&self) -> CanonicalBytes {
        canonical_bytes(self).expect("a checkpoint contains only canonical scalars")
    }

    /// Decode a checkpoint from its stored bytes, verifying every digest and the tail coverage.
    pub fn from_checkpoint_bytes(bytes: &[u8]) -> Result<Self, CheckpointError> {
        let text = std::str::from_utf8(bytes).map_err(|error| {
            CheckpointError::NotCanonical(format!("checkpoint bytes are not UTF-8: {error}"))
        })?;
        serde_json::from_str(text).map_err(|error| decode_error(&error.to_string()))
    }

    /// The prefix an ack of this checkpoint may reclaim (§12.3 rule 6).
    pub fn boundary(&self) -> super::transaction::CheckpointBoundary {
        super::transaction::CheckpointBoundary {
            through_step_seq: self.through_step_seq,
            covered_head: self.covered_transaction_head_digest.clone(),
        }
    }

    /// The §12.3 candidate this checkpoint hands the host.
    pub fn into_candidate(self) -> CheckpointCandidate {
        let ack_token = ack_token_for(
            &self.operation_id,
            self.through_step_seq,
            &self.checkpoint_digest,
        );
        CheckpointCandidate {
            checkpoint_bytes: self.checkpoint_bytes(),
            through_step_seq: self.through_step_seq,
            covered_head: self.covered_transaction_head_digest.clone(),
            state_digest: self.state_digest.clone(),
            ack_token,
        }
    }

    // ----- verification -----

    /// Recompute every digest from the bytes this checkpoint carries and re-check its tail.
    ///
    /// The first two lines of §12.2's ladder. `verify_belongs_to` adds the operation/genesis half;
    /// the version halves are enforced by the decoder, which cannot produce a checkpoint whose
    /// `checkpoint_version` or `abi_version` this kernel does not read.
    pub fn verify(&self) -> Result<(), CheckpointError> {
        check_tail(
            &self.operation_id,
            self.base_step_seq,
            &self.base_record_digest,
            self.through_step_seq,
            &self.covered_transaction_head_digest,
            &self.tail_inputs,
        )?;

        let state_digest = canonical_digest(canonical_bytes(&self.logical_state)?.as_slice());
        if state_digest != self.state_digest {
            return Err(CheckpointError::Corrupted(format!(
                "checkpoint {} through step {}: the logical state hashes to {state_digest}, \
                 but the checkpoint claims {}",
                self.operation_id, self.through_step_seq, self.state_digest
            )));
        }
        let tail_digest = canonical_digest(canonical_bytes(&self.tail_inputs)?.as_slice());
        if tail_digest != self.tail_digest {
            return Err(CheckpointError::Corrupted(format!(
                "checkpoint {} through step {}: the bounded tail hashes to {tail_digest}, \
                 but the checkpoint claims {}",
                self.operation_id, self.through_step_seq, self.tail_digest
            )));
        }
        let checkpoint_digest = Self::body_digest(&CheckpointBody {
            checkpoint_version: self.checkpoint_version,
            abi_version: self.abi_version,
            operation_id: &self.operation_id,
            genesis_digest: &self.genesis_digest,
            base_step_seq: self.base_step_seq,
            base_record_digest: &self.base_record_digest,
            through_step_seq: self.through_step_seq,
            covered_transaction_head_digest: &self.covered_transaction_head_digest,
            logical_state: &self.logical_state,
            tail_inputs: &self.tail_inputs,
            state_digest: &self.state_digest,
            tail_digest: &self.tail_digest,
        })?;
        if checkpoint_digest != self.checkpoint_digest {
            return Err(CheckpointError::Corrupted(format!(
                "checkpoint {} through step {}: the body hashes to {checkpoint_digest}, \
                 but the checkpoint claims {}",
                self.operation_id, self.through_step_seq, self.checkpoint_digest
            )));
        }
        Ok(())
    }

    /// Whether this checkpoint is this operation's (§12.2 line 2).
    pub fn verify_belongs_to(
        &self,
        operation_id: &OperationId,
        genesis_digest: &Digest,
    ) -> Result<(), CheckpointError> {
        if &self.operation_id != operation_id {
            return Err(CheckpointError::Incompatible(format!(
                "checkpoint belongs to operation {}, this runtime to {operation_id}",
                self.operation_id
            )));
        }
        if &self.genesis_digest != genesis_digest {
            return Err(CheckpointError::Incompatible(format!(
                "checkpoint {operation_id} binds genesis {}, this journal's genesis is \
                 {genesis_digest}",
                self.genesis_digest
            )));
        }
        Ok(())
    }
}

/// §12.1 · the bounded tail covers `(base_step_seq, through_step_seq]` exactly.
///
/// One walk catches all four failure modes the spec names: a hole (a gap in the sequence), a
/// duplicate (the same step twice), an out-of-range entry (before `base` or after `through`), and a
/// length that disagrees with the range. It also refuses a tail entry from another operation —
/// the cheapest way to notice a checkpoint assembled from two journals.
fn check_tail(
    operation_id: &OperationId,
    base_step_seq: WireU64,
    base_record_digest: &Digest,
    through_step_seq: WireU64,
    covered_transaction_head_digest: &Digest,
    tail_inputs: &[CanonicalInput],
) -> Result<(), CheckpointError> {
    if base_step_seq > through_step_seq {
        return Err(CheckpointError::Corrupted(format!(
            "checkpoint {operation_id} bases at step {base_step_seq} but covers only through \
             {through_step_seq}"
        )));
    }
    // The two ends of the range meet when the range is empty: a full-state checkpoint's base *is*
    // its covered head, and a header that disagreed with itself about that would hand a restore two
    // different anchors for one record.
    if base_step_seq == through_step_seq && base_record_digest != covered_transaction_head_digest {
        return Err(CheckpointError::Corrupted(format!(
            "checkpoint {operation_id} covers no tail, so its base {base_record_digest} and its \
             covered head {covered_transaction_head_digest} name the same record — but they differ"
        )));
    }
    let expected = through_step_seq.get() - base_step_seq.get();
    if tail_inputs.len() as u64 != expected {
        return Err(CheckpointError::Corrupted(format!(
            "checkpoint {operation_id} covers ({base_step_seq}, {through_step_seq}] — {expected} \
             inputs — but its bounded tail holds {}",
            tail_inputs.len()
        )));
    }
    for (offset, entry) in tail_inputs.iter().enumerate() {
        let want = base_step_seq.get() + offset as u64 + 1;
        if entry.step_seq.get() != want {
            return Err(CheckpointError::Corrupted(format!(
                "checkpoint {operation_id} bounded tail is not the contiguous range \
                 ({base_step_seq}, {through_step_seq}]: position {offset} is step {} where step \
                 {want} was due",
                entry.step_seq
            )));
        }
        if &entry.input.operation_id != operation_id {
            return Err(CheckpointError::Incompatible(format!(
                "checkpoint {operation_id} bounded tail carries an input of operation {} at step \
                 {}",
                entry.input.operation_id, entry.step_seq
            )));
        }
    }
    // The last tail entry *is* the covered head; a tail that ends somewhere else covers a different
    // prefix than the header claims.
    if let Some(last) = tail_inputs.last()
        && &last.record_digest != covered_transaction_head_digest
    {
        return Err(CheckpointError::Corrupted(format!(
            "checkpoint {operation_id} claims covered head {covered_transaction_head_digest}, but \
             its bounded tail ends at {} on step {}",
            last.record_digest, last.step_seq
        )));
    }
    Ok(())
}

fn ack_token_for(
    operation_id: &OperationId,
    through_step_seq: WireU64,
    checkpoint_digest: &Digest,
) -> CheckpointAckToken {
    CheckpointAckToken::new(format!(
        "{operation_id}:checkpoint:{through_step_seq}:{checkpoint_digest}"
    ))
    .expect("an operation-scoped checkpoint ack token is always a legal branded ref")
}

/// §12.3 · what `kernel.checkpoint_candidate()` hands the host.
///
/// Exactly the five values of the spec's arrow, and nothing that would let a host reconstruct the
/// checkpoint itself: `checkpoint_bytes` is opaque storage, `through_step_seq`/`covered_head` are
/// the install precondition, `state_digest` is what an installed blob is audited against, and
/// `ack_token` is the maintenance handle that closes the loop.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckpointCandidate {
    pub checkpoint_bytes: CanonicalBytes,
    pub through_step_seq: WireU64,
    pub covered_head: Digest,
    pub state_digest: Digest,
    pub ack_token: CheckpointAckToken,
}

impl CheckpointCandidate {
    /// The boundary this candidate would let an ack reclaim (§12.3 rule 6).
    pub fn boundary(&self) -> super::transaction::CheckpointBoundary {
        super::transaction::CheckpointBoundary {
            through_step_seq: self.through_step_seq,
            covered_head: self.covered_head.clone(),
        }
    }

    /// Decode the blob back into a verified checkpoint — what an install path does before it
    /// writes anything.
    pub fn decode(&self) -> Result<KernelCheckpoint, CheckpointError> {
        KernelCheckpoint::from_checkpoint_bytes(self.checkpoint_bytes.as_slice())
    }
}

// ---------------------------------------------------------------------------------------------
// decoding
// ---------------------------------------------------------------------------------------------

/// Wire projection of a checkpoint, used only as the decode target, so [`KernelCheckpoint`]'s
/// fields stay private and every decoded checkpoint is verified before it exists.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointProjection {
    checkpoint_version: CheckpointRevision,
    abi_version: AbiRevision,
    operation_id: OperationId,
    genesis_digest: Digest,
    base_step_seq: WireU64,
    base_record_digest: Digest,
    through_step_seq: WireU64,
    covered_transaction_head_digest: Digest,
    logical_state: LogicalKernelState,
    tail_inputs: Vec<CanonicalInput>,
    state_digest: Digest,
    tail_digest: Digest,
    checkpoint_digest: Digest,
}

/// Recover a rejection's class from the string `serde` hands back.
///
/// Three sources reach here: this module's own [`CheckpointError`] rendered by
/// [`fmt::Display`] (which names its code), the scalar layer's ABI-revision refusal, and
/// everything structural — an unknown field, a missing field, a value of the wrong shape.
fn decode_error(message: &str) -> CheckpointError {
    if message.contains(CHECKPOINT_ERROR_MARKER) {
        for code in [
            KernelFaultCode::CheckpointIncompatible,
            KernelFaultCode::CheckpointCorrupted,
        ] {
            if message.contains(&format!("{CHECKPOINT_ERROR_MARKER} ({})", code.as_str())) {
                return match code {
                    KernelFaultCode::CheckpointIncompatible => {
                        CheckpointError::Incompatible(message.to_string())
                    }
                    _ => CheckpointError::Corrupted(message.to_string()),
                };
            }
        }
        return CheckpointError::Corrupted(message.to_string());
    }
    if message.contains(SCALAR_ERROR_MARKER) && message.contains("ABI revision") {
        return CheckpointError::Incompatible(message.to_string());
    }
    CheckpointError::NotCanonical(format!("checkpoint does not decode: {message}"))
}

impl<'de> Deserialize<'de> for KernelCheckpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let projection = CheckpointProjection::deserialize(deserializer)?;
        let checkpoint = Self {
            checkpoint_version: projection.checkpoint_version,
            abi_version: projection.abi_version,
            operation_id: projection.operation_id,
            genesis_digest: projection.genesis_digest,
            base_step_seq: projection.base_step_seq,
            base_record_digest: projection.base_record_digest,
            through_step_seq: projection.through_step_seq,
            covered_transaction_head_digest: projection.covered_transaction_head_digest,
            logical_state: projection.logical_state,
            tail_inputs: projection.tail_inputs,
            state_digest: projection.state_digest,
            tail_digest: projection.tail_digest,
            checkpoint_digest: projection.checkpoint_digest,
        };
        checkpoint
            .verify()
            .map_err(|error| serde::de::Error::custom(error.to_string()))?;
        Ok(checkpoint)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use serde_json::Value;

    use super::super::config::{ConfigDefaults, HostEffectSupport, OperationConfig};
    use super::super::effect::EffectKindTag;
    use super::super::envelope::{ConfigureOperation, KernelInput, WireEnvelope};
    use super::*;

    // -----------------------------------------------------------------------------------------
    // helpers
    // -----------------------------------------------------------------------------------------

    const OPERATION: &str = "op-checkpoint-1";

    fn operation() -> OperationId {
        OperationId::new(OPERATION).unwrap()
    }

    fn digest(label: &str) -> Digest {
        canonical_digest(label.as_bytes())
    }

    fn normalized(input_id: &str, at: u64) -> NormalizedInput {
        let envelope = WireEnvelope::new(
            operation(),
            InputId::new(input_id).unwrap(),
            WireU64::new(at),
            KernelInput::ConfigureOperation(ConfigureOperation {
                config: OperationConfig {
                    host_effect_support: HostEffectSupport {
                        supported: vec![EffectKindTag::CallProvider],
                    },
                    ..OperationConfig::default()
                },
            }),
        );
        NormalizedInput::normalize(&envelope, &ConfigDefaults::default()).expect("normalizes")
    }

    fn tail_entry(step_seq: u64) -> CanonicalInput {
        CanonicalInput {
            step_seq: WireU64::new(step_seq),
            record_digest: digest(&format!("record-{step_seq}")),
            input: normalized(&format!("in-{step_seq}"), 1_700_000_000_000 + step_seq),
        }
    }

    fn resolved_config() -> ResolvedOperationConfig {
        OperationConfig {
            host_effect_support: HostEffectSupport {
                supported: vec![EffectKindTag::CallProvider],
            },
            ..OperationConfig::default()
        }
        .resolve(&ConfigDefaults::default())
        .expect("the default configuration resolves")
    }

    fn logical_state() -> LogicalKernelState {
        LogicalKernelState {
            transition: TransitionStateV1 {
                lifecycle: OperationLifecycle::Running,
                resolved_config: resolved_config(),
                root_kind: Some(RootKind::Agent),
                focus: None,
                last_observed_at_ms: WireU64::new(1_700_000_002_000),
                pending_effects: Vec::new(),
                resolved_effects: Vec::new(),
                launch_tokens: Vec::new(),
                accepted_inputs: vec![AcceptedInputState {
                    input_id: InputId::new("in-configure").unwrap(),
                    step_seq: WireU64::ZERO,
                    record_digest: digest("record-0"),
                }],
                accepted_cancellation: None,
                terminal: None,
            },
            syscall: SyscallStateV1::default(),
            scheduler: SchedulerStateV1::default(),
            context_vm: ContextVmStateV1::default(),
        }
    }

    fn draft(base: u64, through: u64, tail: Vec<CanonicalInput>) -> CheckpointDraft {
        CheckpointDraft {
            operation_id: operation(),
            genesis_digest: digest("genesis"),
            base_step_seq: WireU64::new(base),
            base_record_digest: digest(&format!("record-{base}")),
            through_step_seq: WireU64::new(through),
            covered_transaction_head_digest: digest(&format!("record-{through}")),
            logical_state: logical_state(),
            tail_inputs: tail,
        }
    }

    fn checkpoint() -> KernelCheckpoint {
        KernelCheckpoint::assemble(draft(3, 3, Vec::new())).expect("assembles")
    }

    /// Round-trip a checkpoint through JSON with one field rewritten — the cheapest way to test a
    /// tamper without a constructor that could produce one.
    fn tampered(edit: impl FnOnce(&mut serde_json::Map<String, Value>)) -> CheckpointError {
        let mut document: serde_json::Map<String, Value> =
            serde_json::from_slice(checkpoint().checkpoint_bytes().as_slice()).unwrap();
        edit(&mut document);
        let bytes = serde_json::to_vec(&document).unwrap();
        KernelCheckpoint::from_checkpoint_bytes(&bytes)
            .expect_err("a tampered checkpoint must not decode")
    }

    // -----------------------------------------------------------------------------------------
    // §12.1 · shape
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_checkpoint_states_its_own_versions_from_the_kernels_constants() {
        let checkpoint = checkpoint();
        assert_eq!(checkpoint.checkpoint_version(), KERNEL_CHECKPOINT_VERSION);
        assert_eq!(checkpoint.checkpoint_version(), 1, "§12.1 starts at 1");
        assert_eq!(
            checkpoint.abi_version(),
            super::super::KERNEL_ABI_VERSION,
            "DEC-6 · the ABI revision is read from core's constant, never copied"
        );
    }

    /// The load-bearing invariant of §12.1: each piece of correctness state has exactly one home,
    /// and the header repeats none of it.
    #[test]
    fn single_ownership_is_structural() {
        let checkpoint = checkpoint();
        let document: Value =
            serde_json::from_slice(checkpoint.checkpoint_bytes().as_slice()).unwrap();
        let state = &document["logical_state"];

        // 1. every owned key appears exactly once in the whole logical state document. This is a
        //    total scan, not a spot check: the partitions serialise their whole key set (no
        //    `skip_serializing_if`), so an empty vector is still a visible claim of ownership.
        for (owned, owner) in [
            ("pending_effects", "transition"),
            ("resolved_effects", "transition"),
            ("launch_tokens", "transition"),
            ("accepted_inputs", "transition"),
            ("accepted_cancellation", "transition"),
            ("terminal", "transition"),
            ("attempts", "scheduler"),
            ("tasks", "scheduler"),
            ("handles", "context_vm"),
            ("pending_payload_loads", "context_vm"),
            ("provider_calls", "syscall"),
        ] {
            let mut seen = Vec::new();
            for partition in ["transition", "syscall", "scheduler", "context_vm"] {
                if state[partition]
                    .as_object()
                    .map(|map| map.contains_key(owned))
                    .unwrap_or(false)
                {
                    seen.push(partition);
                }
            }
            assert_eq!(
                seen,
                vec![owner],
                "{owned} must live in exactly one partition"
            );
        }

        // 2. the four partitions share no key at all
        let mut home: BTreeMap<String, &str> = BTreeMap::new();
        for partition in ["transition", "syscall", "scheduler", "context_vm"] {
            for key in state[partition]
                .as_object()
                .expect("a partition object")
                .keys()
            {
                if let Some(previous) = home.insert(key.clone(), partition) {
                    panic!("key {key} lives in both {previous} and {partition}");
                }
            }
        }

        // 3. the header stores no sub-state
        let header: Vec<&String> = document
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| home.contains_key(*key))
            .collect();
        assert!(
            header.is_empty(),
            "the checkpoint header duplicates sub-state: {header:?}"
        );
    }

    /// The DTO must be buildable without touching the semantic engine — the whole point of the
    /// explicit projection. A default projection is a legal (empty) checkpoint.
    #[test]
    fn the_dto_is_constructible_without_any_state_machine() {
        let state = LogicalKernelState {
            transition: TransitionStateV1 {
                lifecycle: OperationLifecycle::Created,
                resolved_config: resolved_config(),
                root_kind: None,
                focus: None,
                last_observed_at_ms: WireU64::ZERO,
                pending_effects: Vec::new(),
                resolved_effects: Vec::new(),
                launch_tokens: Vec::new(),
                accepted_inputs: Vec::new(),
                accepted_cancellation: None,
                terminal: None,
            },
            syscall: SyscallStateV1::default(),
            scheduler: SchedulerStateV1::default(),
            context_vm: ContextVmStateV1::default(),
        };
        let mut draft = draft(0, 0, Vec::new());
        draft.logical_state = state;
        KernelCheckpoint::assemble(draft).expect("an empty logical state is still a checkpoint");
    }

    // -----------------------------------------------------------------------------------------
    // §12.1 · digests
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_three_digests_summarise_three_different_things() {
        let checkpoint = checkpoint();
        assert_eq!(
            checkpoint.state_digest(),
            &canonical_digest(
                canonical_bytes(checkpoint.logical_state())
                    .unwrap()
                    .as_slice()
            ),
        );
        assert_eq!(
            checkpoint.tail_digest(),
            &canonical_digest(
                canonical_bytes(checkpoint.tail_inputs())
                    .unwrap()
                    .as_slice()
            ),
        );
        assert_ne!(checkpoint.state_digest(), checkpoint.checkpoint_digest());
        assert_ne!(checkpoint.tail_digest(), checkpoint.checkpoint_digest());
        checkpoint
            .verify()
            .expect("a freshly built checkpoint verifies");
    }

    /// The checkpoint digest covers the header too: moving `through_step_seq` without moving the
    /// digest is corruption, not a different-but-valid checkpoint.
    #[test]
    fn the_checkpoint_digest_covers_the_header_and_the_bounded_tail() {
        let with_tail = KernelCheckpoint::assemble(draft(3, 4, vec![tail_entry(4)])).unwrap();
        let without_tail = checkpoint();
        assert_eq!(
            with_tail.state_digest(),
            without_tail.state_digest(),
            "the same logical state digests the same either way"
        );
        assert_ne!(
            with_tail.checkpoint_digest(),
            without_tail.checkpoint_digest(),
            "but the checkpoint digest moves with the tail and the header"
        );

        let error = tampered(|document| {
            document.insert("through_step_seq".to_string(), Value::String("9".into()));
        });
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);
    }

    #[test]
    fn a_checkpoint_round_trips_through_its_bytes() {
        let original = KernelCheckpoint::assemble(draft(2, 4, vec![tail_entry(3), tail_entry(4)]))
            .expect("a bounded-tail checkpoint assembles");
        let decoded =
            KernelCheckpoint::from_checkpoint_bytes(original.checkpoint_bytes().as_slice())
                .expect("its own bytes decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.tail_inputs().len(), 2);
    }

    // -----------------------------------------------------------------------------------------
    // corruption and incompatibility
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_digest_that_does_not_match_its_bytes_is_corruption() {
        for field in ["state_digest", "tail_digest", "checkpoint_digest"] {
            let error = tampered(|document| {
                document.insert(
                    field.to_string(),
                    Value::String(digest("bogus").to_string()),
                );
            });
            assert_eq!(
                error.code(),
                KernelFaultCode::CheckpointCorrupted,
                "{field} must fail closed"
            );
            assert!(
                error.to_string().contains(CHECKPOINT_ERROR_MARKER),
                "{field}: every rejection carries the classifier marker"
            );
        }
    }

    #[test]
    fn a_logical_state_edited_after_the_fact_is_corruption() {
        let error = tampered(|document| {
            document["logical_state"]["transition"]["lifecycle"] = Value::String("failed".into());
        });
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);
        assert!(error.message().contains("logical state hashes to"));
    }

    #[test]
    fn an_unrecognised_checkpoint_version_is_incompatible() {
        for version in [0u64, 2, 99] {
            let error = tampered(|document| {
                document.insert("checkpoint_version".to_string(), Value::from(version));
            });
            assert_eq!(
                error.code(),
                KernelFaultCode::CheckpointIncompatible,
                "checkpoint version {version} must be refused, not guessed at"
            );
        }
    }

    #[test]
    fn an_abi_revision_this_kernel_does_not_read_is_incompatible() {
        let error = tampered(|document| {
            document.insert(
                "abi_version".to_string(),
                Value::from(u64::from(super::super::KERNEL_ABI_VERSION) + 1),
            );
        });
        assert_eq!(error.code(), KernelFaultCode::CheckpointIncompatible);
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        let error = tampered(|document| {
            // §12.4 deleted `last_step`; a blob that still carries one is a v2 snapshot.
            document.insert("last_step".to_string(), Value::Null);
        });
        assert_eq!(error.code(), KernelFaultCode::MalformedEnvelope);
    }

    #[test]
    fn a_checkpoint_from_another_operation_or_genesis_is_incompatible() {
        let checkpoint = checkpoint();
        let other = OperationId::new("op-checkpoint-2").unwrap();

        let error = checkpoint
            .verify_belongs_to(&other, &digest("genesis"))
            .expect_err("another operation's checkpoint is not installable");
        assert_eq!(error.code(), KernelFaultCode::CheckpointIncompatible);
        assert!(error.message().contains("belongs to operation"));

        let error = checkpoint
            .verify_belongs_to(&operation(), &digest("another-genesis"))
            .expect_err("a different genesis is a different operation");
        assert_eq!(error.code(), KernelFaultCode::CheckpointIncompatible);
        assert!(error.message().contains("binds genesis"));

        checkpoint
            .verify_belongs_to(&operation(), &digest("genesis"))
            .expect("its own operation and genesis are accepted");
    }

    // -----------------------------------------------------------------------------------------
    // §12.1 · the bounded tail covers (base, through] exactly
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_tail_that_covers_the_range_exactly_is_accepted() {
        KernelCheckpoint::assemble(draft(0, 0, Vec::new())).expect("an empty range needs no tail");
        KernelCheckpoint::assemble(draft(
            2,
            5,
            vec![tail_entry(3), tail_entry(4), tail_entry(5)],
        ))
        .expect("(2, 5] is three contiguous inputs");
    }

    #[test]
    fn a_tail_with_a_hole_is_refused() {
        let error = KernelCheckpoint::assemble(draft(
            2,
            5,
            vec![tail_entry(3), tail_entry(5), tail_entry(6)],
        ))
        .expect_err("step 4 is missing");
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);
        assert!(error.message().contains("step 4 was due"), "{error}");
    }

    #[test]
    fn a_tail_with_a_duplicate_is_refused() {
        let error = KernelCheckpoint::assemble(draft(
            2,
            5,
            vec![tail_entry(3), tail_entry(3), tail_entry(4)],
        ))
        .expect_err("step 3 appears twice");
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);
        assert!(error.message().contains("contiguous range"), "{error}");
    }

    #[test]
    fn a_tail_entry_outside_the_range_is_refused() {
        // before the base
        let error = KernelCheckpoint::assemble(draft(2, 4, vec![tail_entry(2), tail_entry(3)]))
            .expect_err("step 2 is the base, not part of (2, 4]");
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);

        // after the covered head
        let error = KernelCheckpoint::assemble(draft(2, 4, vec![tail_entry(3), tail_entry(9)]))
            .expect_err("step 9 is past the covered head");
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);
    }

    #[test]
    fn a_tail_whose_length_disagrees_with_the_range_is_refused() {
        let error = KernelCheckpoint::assemble(draft(2, 5, vec![tail_entry(3)]))
            .expect_err("(2, 5] is three inputs, not one");
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);
        assert!(error.message().contains("bounded tail holds 1"), "{error}");

        let error = KernelCheckpoint::assemble(draft(4, 2, Vec::new()))
            .expect_err("a base past the covered head is not a range at all");
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);
    }

    #[test]
    fn a_tail_input_from_another_operation_is_incompatible() {
        let mut foreign = tail_entry(3);
        foreign.input.operation_id = OperationId::new("op-checkpoint-2").unwrap();
        let error = KernelCheckpoint::assemble(draft(2, 3, vec![foreign]))
            .expect_err("a tail assembled from two journals is not a checkpoint");
        assert_eq!(error.code(), KernelFaultCode::CheckpointIncompatible);
    }

    /// Decoding re-runs the coverage check, so a hole punched into a stored blob is caught at the
    /// boundary rather than half-way through a replay.
    #[test]
    fn a_tail_edited_in_storage_is_refused_at_decode() {
        let original = KernelCheckpoint::assemble(draft(2, 4, vec![tail_entry(3), tail_entry(4)]))
            .expect("assembles");
        let mut document: serde_json::Map<String, Value> =
            serde_json::from_slice(original.checkpoint_bytes().as_slice()).unwrap();
        let tail = document["tail_inputs"].as_array_mut().unwrap();
        tail.remove(0);
        let bytes = serde_json::to_vec(&document).unwrap();
        let error = KernelCheckpoint::from_checkpoint_bytes(&bytes)
            .expect_err("a truncated tail no longer covers its range");
        assert_eq!(error.code(), KernelFaultCode::CheckpointCorrupted);
    }

    // -----------------------------------------------------------------------------------------
    // §12.3 · the candidate handle
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_candidate_carries_the_five_values_of_the_spec_arrow() {
        let checkpoint = KernelCheckpoint::assemble(draft(3, 4, vec![tail_entry(4)])).unwrap();
        let expected_digest = checkpoint.checkpoint_digest().clone();
        let candidate = checkpoint.into_candidate();

        assert_eq!(candidate.through_step_seq, WireU64::new(4));
        assert_eq!(candidate.covered_head, digest("record-4"));
        assert!(
            candidate.ack_token.as_str().contains(OPERATION)
                && candidate
                    .ack_token
                    .as_str()
                    .contains(expected_digest.as_str()),
            "the ack token names the checkpoint it acknowledges: {}",
            candidate.ack_token
        );

        let decoded = candidate.decode().expect("the blob decodes and verifies");
        assert_eq!(decoded.checkpoint_digest(), &expected_digest);
        assert_eq!(decoded.state_digest(), &candidate.state_digest);
        assert_eq!(
            candidate.boundary().through_step_seq,
            candidate.through_step_seq
        );
    }

    // -----------------------------------------------------------------------------------------
    // rejection fixtures
    // -----------------------------------------------------------------------------------------

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kernel-wire")
    }

    /// Regenerate every `reject_checkpoint_*` blob from this module's own constructors.
    ///
    /// The fixtures used to be hand-written, which made them a second, drifting copy of the
    /// checkpoint shape: adding one field to the DTO invalidated all eight, and each had to be
    /// edited by hand into a document that still failed for the *declared* reason rather than for
    /// "missing field". Deriving them means a shape change costs one `BLESS_KERNEL_RECORD_FIXTURES=1`
    /// run, and — more importantly — a fixture can never claim to test a corruption while actually
    /// testing a stale schema.
    #[test]
    fn bless_checkpoint_rejection_fixtures() {
        if std::env::var("BLESS_KERNEL_RECORD_FIXTURES").as_deref() != Ok("1") {
            return;
        }
        let dir = fixture_dir();
        for (name, expect, description, mutate) in rejection_cases() {
            let mut document: serde_json::Map<String, Value> =
                serde_json::from_slice(mutate.0.checkpoint_bytes().as_slice()).unwrap();
            (mutate.1)(&mut document);
            let fixture = serde_json::json!({
                "expect": expect,
                "description": description,
                "checkpoint": Value::Object(document),
            });
            let mut text = serde_json::to_string_pretty(&fixture).unwrap();
            text.push('\n');
            fs::write(dir.join(name), text).unwrap_or_else(|e| panic!("cannot bless {name}: {e}"));
        }
    }

    #[allow(clippy::type_complexity)]
    fn rejection_cases() -> Vec<(
        &'static str,
        &'static str,
        &'static str,
        (
            KernelCheckpoint,
            Box<dyn Fn(&mut serde_json::Map<String, Value>)>,
        ),
    )> {
        let full = || checkpoint();
        let with_tail =
            || KernelCheckpoint::assemble(draft(2, 4, vec![tail_entry(3), tail_entry(4)])).unwrap();
        vec![
            (
                "reject_checkpoint_unknown_checkpoint_version.json",
                "checkpoint_incompatible",
                "A checkpoint of a revision this kernel does not read is refused at the decode \
                 boundary, never guessed at (spec 12.1, DEC-6).",
                (
                    full(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d.insert("checkpoint_version".into(), Value::from(99u64));
                    }) as Box<dyn Fn(&mut serde_json::Map<String, Value>)>,
                ),
            ),
            (
                "reject_checkpoint_abi_revision_future.json",
                "checkpoint_incompatible",
                "A checkpoint written against a newer wire revision is incompatible, not corrupt: \
                 the answer is a different checkpoint, not more tail (spec 16.2).",
                (
                    full(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d.insert(
                            "abi_version".into(),
                            Value::from(u64::from(super::super::KERNEL_ABI_VERSION) + 1),
                        );
                    }),
                ),
            ),
            (
                "reject_checkpoint_state_digest_mismatch.json",
                "checkpoint_corrupted",
                "The logical state does not hash to the digest the checkpoint claims (spec 12.1).",
                (
                    full(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d.insert(
                            "state_digest".into(),
                            Value::String(digest("bogus").to_string()),
                        );
                    }),
                ),
            ),
            (
                "reject_checkpoint_missing_field_checkpoint_digest.json",
                "malformed_envelope",
                "A structural refusal that names the field: a checkpoint without its own digest is \
                 not a checkpoint with an unverified digest.",
                (
                    full(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d.remove("checkpoint_digest");
                    }),
                ),
            ),
            (
                "reject_checkpoint_unknown_field_last_step.json",
                "malformed_envelope",
                "Spec 12.4 deleted `last_step`; a blob that still carries one is an ABI-v2 \
                 snapshot and is refused rather than partially read.",
                (
                    full(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d.insert("last_step".into(), Value::Null);
                    }),
                ),
            ),
            (
                "reject_checkpoint_base_disagrees_with_covered_head.json",
                "checkpoint_corrupted",
                "A full-state checkpoint covers no tail, so its base and its covered head name the \
                 same record; a header that disagrees with itself would hand a restore two \
                 different chain anchors (spec 12.1, Task 16).",
                (
                    full(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d.insert(
                            "base_record_digest".into(),
                            Value::String(digest("another-record").to_string()),
                        );
                    }),
                ),
            ),
            (
                "reject_checkpoint_tail_hole.json",
                "checkpoint_corrupted",
                "The bounded tail must cover (base, through] with no hole (spec 12.1).",
                (
                    with_tail(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d["tail_inputs"].as_array_mut().unwrap().remove(0);
                    }),
                ),
            ),
            (
                "reject_checkpoint_tail_duplicate.json",
                "checkpoint_corrupted",
                "The bounded tail must cover (base, through] with no duplicate (spec 12.1).",
                (
                    with_tail(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        let tail = d["tail_inputs"].as_array_mut().unwrap();
                        tail[1] = tail[0].clone();
                    }),
                ),
            ),
            (
                "reject_checkpoint_tail_foreign_operation.json",
                "checkpoint_incompatible",
                "A bounded tail assembled from two journals is not a checkpoint (spec 12.1).",
                (
                    with_tail(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d["tail_inputs"][0]["input"]["operation_id"] =
                            Value::String("op-checkpoint-2".into());
                    }),
                ),
            ),
            (
                "reject_checkpoint_tail_ends_off_the_covered_head.json",
                "checkpoint_corrupted",
                "The last bounded-tail entry *is* the covered head; a tail that ends somewhere \
                 else covers a different prefix than the header claims (spec 12.1, Task 16).",
                (
                    with_tail(),
                    Box::new(|d: &mut serde_json::Map<String, Value>| {
                        d["tail_inputs"][1]["record_digest"] =
                            Value::String(digest("some-other-record").to_string());
                    }),
                ),
            ),
        ]
    }

    #[test]
    fn checkpoint_rejection_fixtures_fail_closed_with_the_declared_kind() {
        let dir = fixture_dir();
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .filter(|name| name.starts_with("reject_checkpoint_") && name.ends_with(".json"))
            .collect();
        names.sort();
        assert!(
            names.len() >= 5,
            "too few checkpoint rejection fixtures: {names:?}"
        );

        for name in names {
            let raw = fs::read_to_string(dir.join(&name)).expect("fixture reads");
            let fixture: Value = serde_json::from_str(&raw).expect("fixture is JSON");
            let expected = fixture["expect"]
                .as_str()
                .expect("every fixture declares `expect`");
            let bytes = serde_json::to_vec(&fixture["checkpoint"]).unwrap();
            let error = KernelCheckpoint::from_checkpoint_bytes(&bytes)
                .expect_err(&format!("{name}: expected a rejection"));
            assert_eq!(
                error.code().as_str(),
                expected,
                "{name}: {} (message: {})",
                error.code().as_str(),
                error.message()
            );
            // The `missing_field` / `unknown_field` naming convention has to mean something: both
            // are structural refusals, and both must name the field so a host can act on them.
            for (marker, needle) in [
                ("_missing_field_", "missing field"),
                ("_unknown_field_", "unknown field"),
            ] {
                if name.contains(marker) {
                    assert_eq!(
                        error.code(),
                        KernelFaultCode::MalformedEnvelope,
                        "{name}: a structural refusal is malformed_envelope"
                    );
                    assert!(
                        error.message().contains(needle),
                        "{name}: the rejection must say which field ({})",
                        error.message()
                    );
                }
            }
        }
    }
}
