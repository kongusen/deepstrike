//! §8.2/§12 · the crash-point matrix (0.2.65 S0).
//!
//! Ten places a host process can die between two kernel calls, one test each. Every scenario
//! cuts the durable path at its boundary, rebuilds through the recovery API a real host would
//! call, and asserts the run continues to the *same* head an uninterrupted process reaches —
//! the matrix that turns "durable" from a property of the primitives into a property of the
//! seams between them.
//!
//! Design rulings (same style as `chain_validator`: the contract lives in this one module):
//!
//! 1. **Boundaries, not timers.** A crash is modelled exactly as the API surface allows it:
//!    a candidate dropped, an append without a commit, a commit whose effects were never
//!    handed out. No simulated partial writes — the journal's own CAS append is atomic, and
//!    what can tear is only ever the host's progress *between* kernel calls.
//! 2. **Recovery goes through the public recovery doors.** `restore_operation` (both ladder
//!    arms), `rebuild_from_records` via the same function's no-checkpoint arm, and
//!    `note_append_conflict`. If a scenario can only be written by reaching into transaction
//!    internals, the recovery API is missing something and the test should fail until it isn't.
//! 3. **Equivalence is byte-level.** Envelopes are fully literal (ids, clocks, payloads), so
//!    two runtimes that walked the same inputs must agree on every record digest — the same
//!    differential the restore ladder itself is held to.
//! 4. **The checkpoint-side rows §12.3 Verification 3 already pins in `driver::tests`**
//!    (install-then-crash, appends-after-candidate, moved covered head, ack-then-prune
//!    redelivery) stay where they are; the rows here restate the ones the S0 contract table
//!    names, from this module's seam-first framing, so the matrix is reviewable in one place.
//!
//! Scenario index (S0 contract table, `.local-docs/specs/0.2.65-durable-recovery-closure.md`):
//!
//! | # | seam | test |
//! |---|------|------|
//! | 1 | before journal append | `crash_before_append_leaves_no_residue_and_redelivers_the_same_step` |
//! | 2 | after append, before commit | `crash_after_append_before_commit_reaches_one_head_both_ways` |
//! | 3 | after commit, before effect publish | `crash_after_commit_before_publish_republishes_the_same_effect_ids` |
//! | 4 | after publish, before ResolveEffect | `crash_after_publish_before_resolve_redelivers_to_the_same_record` |
//! | 5 | before checkpoint install | `crash_before_install_a_candidate_is_pure_and_regenerable` |
//! | 6 | after install, before ack | `crash_after_install_before_ack_still_installs_acks_and_restores` |
//! | 7 | after ack, before journal pruning | `crash_after_ack_before_prune_reack_converges` |
//! | 8 | CAS conflict during append | `append_conflict_discards_the_candidate_and_rebuild_replays` |
//! | 9 | checkpoint + incomplete tail | `checkpoint_plus_incomplete_tail_restores_to_head` |
//! | 10 | redelivery below checkpoint base | `redelivery_below_the_base_is_an_ack_not_a_step` |

use serde_json::{Value, json};

use super::checkpoint::KernelCheckpoint;
use super::config::{
    ConfigDefaults, ExecutionPolicy, HostEffectSupport, MemoryPolicy, OperationConfig,
    ResourceQuota, SkillMetadata,
};
use super::driver::{CanonicalOperationDriver, PlannedStep, SYSCALL_TOOL_NAMES};
use super::effect::{
    EffectKindTag, EffectOutcome, EffectSucceeded, EffectSuccess, InlineToolResult,
    MemoryAccessBinding, MemoryCapabilities, ProviderCompleted, ProviderMessage, ProviderOutcome,
    ProviderStopReason, ProviderSuccess, ToolCall as WireToolCall, ToolResult as WireToolResult,
    ToolResultDisposition, ToolResultPayload as WireToolResultPayload, ToolsSuccess,
};
use super::envelope::{
    ConfigureOperation, KernelInput, ResolveEffect, StartOperation, WireEnvelope,
};
use super::fault::{KernelFaultCode, PrepareToken};
use super::record::{KernelRecord, RecordPreparation};
use super::restore::{RestoreCost, restore_operation};
use super::root::{InitialContext, LogicalAgentSpec, LogicalTask, RootAgentEntry, RootEntry};
use super::scalar::{BoundedJson, CallId, EffectId, InputId, MemoryBindingId, WireU64};
use super::transaction::{CommittedTransition, InMemoryRecordIndex, KernelTransaction};

const OPERATION: &str = "op-crash-1";

// -----------------------------------------------------------------------------------------
// §8.2 host loop: prepare → (CAS append) → commit → fold — same harness as `driver::tests`,
// copied rather than shared so this module owns the whole matrix (ruling 2).
// -----------------------------------------------------------------------------------------

struct Runtime {
    tx: KernelTransaction<PlannedStep, InMemoryRecordIndex>,
    driver: CanonicalOperationDriver,
    journal: Vec<KernelRecord>,
    restore_cost: Option<RestoreCost>,
}

impl Runtime {
    fn new() -> Self {
        Self {
            tx: KernelTransaction::new(ConfigDefaults::default(), InMemoryRecordIndex::new()),
            driver: CanonicalOperationDriver::new(),
            journal: Vec::new(),
            restore_cost: None,
        }
    }

    fn prepare(&mut self, envelope: &WireEnvelope) -> RecordPreparation<PlannedStep> {
        let Self { tx, driver, .. } = self;
        tx.prepare(envelope, |context| driver.plan(context))
    }

    /// The host's CAS append + commit, as two separate steps so a scenario can die between them.
    fn append_and_commit(
        &mut self,
        preparation: RecordPreparation<PlannedStep>,
    ) -> CommittedTransition<PlannedStep> {
        let token: PrepareToken = preparation
            .token()
            .unwrap_or_else(|| panic!("expected a prepared step, got {:?}", preparation.fault()))
            .clone();
        let head = preparation.record().unwrap().record_digest().clone();
        let committed = self.tx.commit(&token, &head).expect("commit must succeed");
        self.journal.push(committed.record.clone());
        committed
    }

    fn submit(&mut self, envelope: &WireEnvelope) -> CommittedTransition<PlannedStep> {
        let preparation = self.prepare(envelope);
        let committed = self.append_and_commit(preparation);
        self.driver
            .note_committed(committed.step_seq)
            .expect("the driver folds the step it planned");
        committed
    }

    /// The no-checkpoint arm of the ladder: a process rebooted with nothing but the journal.
    fn rebuild(records: &[KernelRecord]) -> Runtime {
        let Restored {
            transaction,
            driver,
        } = {
            let restored = restore_operation(
                None,
                records,
                ConfigDefaults::default(),
                InMemoryRecordIndex::from_records(records),
            )
            .expect("the journal-only ladder runs");
            Restored {
                transaction: restored.transaction,
                driver: restored.driver,
            }
        };
        Runtime {
            tx: transaction,
            driver,
            journal: records.to_vec(),
            restore_cost: None,
        }
    }

    /// The checkpoint arm of the ladder: a process rebooted with a checkpoint blob plus the
    /// journal records above the step it covers.
    fn restore(checkpoint: &KernelCheckpoint, records: &[KernelRecord]) -> Runtime {
        let restored = restore_operation(
            Some(checkpoint),
            records,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(records),
        )
        .expect("the checkpoint ladder runs");
        Runtime {
            tx: restored.transaction,
            driver: restored.driver,
            journal: records.to_vec(),
            restore_cost: Some(restored.cost),
        }
    }

    /// §12.3 · exactly the host's install-point call: generate a candidate over the durable head.
    fn checkpoint(&self) -> super::checkpoint::CheckpointCandidate {
        self.tx
            .checkpoint_candidate(self.driver.project_logical_state())
            .expect("a configured operation has a logical state to checkpoint")
    }
}

struct Restored {
    transaction: KernelTransaction<PlannedStep, InMemoryRecordIndex>,
    driver: CanonicalOperationDriver,
}

fn drive(runtime: &mut Runtime, envelopes: &[WireEnvelope]) {
    for envelope in envelopes {
        runtime.submit(envelope);
    }
}

/// The whole observable surface a crash may disturb, as bytes: where the run stands, what it
/// still waits on, and whether it ended.
fn fingerprint(runtime: &Runtime) -> Value {
    json!({
        "head": runtime.tx.head().map(|head| json!({
            "digest": head.digest.as_str(),
            "step_seq": head.step_seq.to_string(),
        })),
        "lifecycle": format!("{:?}", runtime.tx.lifecycle()),
        "pending_effects": runtime
            .tx
            .pending_effects_in_order()
            .iter()
            .map(|effect| effect.effect_id.as_str())
            .collect::<Vec<_>>(),
        "terminal": runtime.tx.terminal().map(|t| serde_json::to_value(t).unwrap()),
    })
}

// -----------------------------------------------------------------------------------------
// envelopes — literal in every field a record digests (ruling 3)
// -----------------------------------------------------------------------------------------

fn operation() -> super::scalar::OperationId {
    super::scalar::OperationId::new(OPERATION).unwrap()
}

fn envelope(id: &str, observed_at_ms: u64, input: KernelInput) -> WireEnvelope {
    WireEnvelope::new(
        operation(),
        InputId::new(id).unwrap(),
        WireU64::new(observed_at_ms),
        input,
    )
}

fn syscall_tool_catalog() -> Vec<super::effect::ToolSchema> {
    SYSCALL_TOOL_NAMES
        .iter()
        .chain(std::iter::once(&"search"))
        .map(|name| super::effect::ToolSchema {
            name: (*name).to_string(),
            description: String::new(),
            parameters: Default::default(),
        })
        .collect()
}

fn test_agent_spec(goal: &str) -> LogicalAgentSpec {
    LogicalAgentSpec {
        exposure_baseline: Some(
            syscall_tool_catalog()
                .into_iter()
                .map(|tool| tool.name)
                .collect(),
        ),
        ..LogicalAgentSpec::new(goal)
    }
}

fn syscall_config() -> WireEnvelope {
    let config = OperationConfig {
        execution_policy: Some(ExecutionPolicy {
            max_turns: Some(12),
            ..ExecutionPolicy::default()
        }),
        host_effect_support: HostEffectSupport::new([
            EffectKindTag::CallProvider,
            EffectKindTag::ExecuteTools,
            EffectKindTag::LoadPayload,
            EffectKindTag::SpawnTasks,
            EffectKindTag::PreemptTasks,
            EffectKindTag::PersistMemory,
            EffectKindTag::QueryMemory,
        ]),
        tool_catalog: syscall_tool_catalog(),
        skill_catalog: vec![SkillMetadata {
            name: "debug".to_string(),
            description: "debug helper".to_string(),
            when_to_use: None,
            allowed_tools: Vec::new(),
            capability_grants: Vec::new(),
            effort: None,
            estimated_tokens: None,
        }],
        memory_access: Some(MemoryAccessBinding {
            binding_id: MemoryBindingId::new("mem-binding-1").unwrap(),
            capabilities: MemoryCapabilities {
                read: true,
                write: true,
            },
        }),
        memory_policy: Some(MemoryPolicy {
            retrieval_top_k: Some(4),
            ..MemoryPolicy::default()
        }),
        resource_quota: Some(ResourceQuota {
            max_workflow_nodes: Some(3),
            ..ResourceQuota::default()
        }),
        ..OperationConfig::default()
    };
    envelope(
        "in-configure",
        1_700_000_000_000,
        KernelInput::ConfigureOperation(ConfigureOperation { config }),
    )
}

fn agent_start(id: &str, observed_at_ms: u64) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Agent(RootAgentEntry {
                task: LogicalTask::new("write the research brief"),
                run_spec: Some(test_agent_spec("write the research brief")),
            }),
            initial_context: InitialContext::default(),
        }),
    )
}

fn effect_id(step_seq: WireU64) -> EffectId {
    EffectId::new(format!("{OPERATION}:step:{step_seq}:effect:0")).unwrap()
}

fn tool_call(call_id: &str, name: &str, arguments: Value) -> WireToolCall {
    WireToolCall {
        call_id: CallId::new(call_id).unwrap(),
        name: name.to_string(),
        arguments: BoundedJson::new(arguments).unwrap(),
    }
}

fn resolved(id: &str, at: u64, effect: &EffectId, result: EffectSuccess) -> WireEnvelope {
    envelope(
        id,
        at,
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: effect.clone(),
            outcome: EffectOutcome::Succeeded(EffectSucceeded { result }),
        }),
    )
}

fn provider_result(
    id: &str,
    observed_at_ms: u64,
    effect: &EffectId,
    calls: Vec<WireToolCall>,
) -> WireEnvelope {
    resolved(
        id,
        observed_at_ms,
        effect,
        EffectSuccess::Provider(ProviderSuccess {
            outcome: ProviderOutcome::Completed(ProviderCompleted {
                message: ProviderMessage {
                    role: super::root::MessageRole::Assistant,
                    content: String::new(),
                    tool_calls: calls,
                    tool_call_id: None,
                    tokens: None,
                },
                observed_input_tokens: None,
                observed_output_tokens: None,
                stop_reason: None,
            }),
        }),
    )
}

fn tools_resolved(
    id: &str,
    at: u64,
    effect: &EffectId,
    results: &[(&str, &str, bool)],
) -> WireEnvelope {
    resolved(
        id,
        at,
        effect,
        EffectSuccess::Tools(ToolsSuccess {
            results: results
                .iter()
                .map(|(call_id, output, is_error)| {
                    WireToolResultPayload::Inline(InlineToolResult {
                        call_id: CallId::new(*call_id).unwrap(),
                        result: WireToolResult {
                            output: (*output).to_string(),
                            durable_content: None,
                            is_error: *is_error,
                            disposition: ToolResultDisposition::Recoverable,
                            tokens: None,
                        },
                    })
                })
                .collect(),
        }),
    )
}

fn provider_answer(id: &str, at: u64, effect: &EffectId, text: &str) -> WireEnvelope {
    resolved(
        id,
        at,
        effect,
        EffectSuccess::Provider(ProviderSuccess {
            outcome: ProviderOutcome::Completed(ProviderCompleted {
                message: ProviderMessage {
                    role: super::root::MessageRole::Assistant,
                    content: text.to_string(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tokens: None,
                },
                observed_input_tokens: None,
                observed_output_tokens: None,
                stop_reason: Some(ProviderStopReason::EndTurn),
            }),
        }),
    )
}

/// One full agent turn: configure, start, provider tool calls, tool results, provider again,
/// results again, final answer. Seven durable steps ending in the terminal.
fn turn_envelopes() -> Vec<WireEnvelope> {
    vec![
        syscall_config(),
        agent_start("in-start", 1_700_000_001_000),
        provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(WireU64::new(1)),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ),
        tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(WireU64::new(2)),
            &[("call-1", "three sources found", false)],
        ),
        provider_result(
            "in-acted-2",
            1_700_000_004_000,
            &effect_id(WireU64::new(3)),
            vec![tool_call("call-2", "search", json!({"q": "more"}))],
        ),
        tools_resolved(
            "in-results-2",
            1_700_000_005_000,
            &effect_id(WireU64::new(4)),
            &[("call-2", "two more sources", false)],
        ),
        provider_answer(
            "in-answer",
            1_700_000_006_000,
            &effect_id(WireU64::new(5)),
            "the brief cites five sources",
        ),
    ]
}

fn digests(runtime: &Runtime) -> Vec<String> {
    runtime
        .journal
        .iter()
        .map(|record| record.record_digest().to_string())
        .collect()
}

// -----------------------------------------------------------------------------------------
// scenario 1 · crash before the journal append
// -----------------------------------------------------------------------------------------

/// A candidate that never reached the journal is nothing. The host drops the process (and with
/// it the outstanding candidate, without so much as an `abort`), boots a fresh runtime on the
/// same journal, and redelivers the input: it is **prepared again**, byte-identically — same
/// step, same record digest — because nothing durably happened the first time.
#[test]
fn crash_before_append_leaves_no_residue_and_redelivers_the_same_step() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    runtime.submit(&envelopes[0]);

    let preparation = runtime.prepare(&envelopes[1]);
    let crashed_digest = preparation.record().unwrap().record_digest().clone();
    // ... and the process dies. No append, no commit, no abort — the candidate is simply gone.
    drop(preparation);

    assert!(
        runtime.journal.len() == 1,
        "no durable residue: the journal holds only the configure record"
    );

    let mut recovered = Runtime::rebuild(&runtime.journal);
    let prepared = match recovered.prepare(&envelopes[1]) {
        RecordPreparation::Prepared(prepared) => prepared,
        other => panic!(
            "an input that never reached the journal must be accepted again, got {:?}",
            other.fault().map(|fault| fault.code)
        ),
    };
    assert_eq!(
        prepared.record.record_digest(),
        &crashed_digest,
        "the same input plans the same step — the crash never happened, durably speaking",
    );

    // And the recovered run continues through the durable path to the same history an
    // uninterrupted one reaches.
    let committed = recovered.append_and_commit(RecordPreparation::Prepared(prepared));
    recovered
        .driver
        .note_committed(committed.step_seq)
        .expect("the driver folds the step it planned");
    drive(&mut recovered, &envelopes[2..]);
    let mut uninterrupted = Runtime::new();
    drive(&mut uninterrupted, &envelopes);
    assert_eq!(digests(&recovered), digests(&uninterrupted));
}

// -----------------------------------------------------------------------------------------
// scenario 2 · crash after the append, before the commit
// -----------------------------------------------------------------------------------------

/// The record is in the journal; the transaction died before `commit`. Two hosts recover from
/// the same on-disk state and must land on **one** head:
///
/// - **arm A** — the process actually survived (the crash was a pause): the outstanding token
///   still commits, because the append it waited for happened.
/// - **arm B** — the process died: a fresh runtime rebuilds from the journal, and the appended
///   record is folded in like any other.
#[test]
fn crash_after_append_before_commit_reaches_one_head_both_ways() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    runtime.submit(&envelopes[0]);

    let preparation = runtime.prepare(&envelopes[1]);
    let token: PrepareToken = preparation.token().unwrap().clone();
    let appended_head = preparation.record().unwrap().record_digest().clone();
    // The CAS append succeeded. The process dies before `commit`.
    runtime.journal.push(preparation.record().unwrap().clone());

    // arm A · the pause, not a crash: the same runtime commits the token it still holds.
    runtime
        .tx
        .commit(&token, &appended_head)
        .expect("the append happened, so the commit lands");
    runtime
        .driver
        .note_committed(runtime.tx.head().unwrap().step_seq)
        .expect("the fold catches up");
    let journal_snapshot = runtime.journal.clone();

    // arm B · the reboot: rebuild purely from the same journal bytes.
    let rebuilt = Runtime::rebuild(&journal_snapshot);

    assert_eq!(
        runtime.tx.head().map(|head| head.digest),
        rebuilt.tx.head().map(|head| head.digest),
        "commit-after-the-fact and rebuild agree on the head",
    );
    assert_eq!(
        runtime.tx.head().unwrap().digest,
        appended_head,
        "and that head is exactly the appended record",
    );

    // Both arms continue where an uninterrupted run would be.
    drive(&mut runtime, &envelopes[2..]);
    let mut rebuilt = rebuilt;
    drive(&mut rebuilt, &envelopes[2..]);
    let mut uninterrupted = Runtime::new();
    drive(&mut uninterrupted, &envelopes);
    assert_eq!(digests(&runtime), digests(&uninterrupted));
    assert_eq!(digests(&rebuilt), digests(&uninterrupted));
}

// -----------------------------------------------------------------------------------------
// scenario 3 · crash after the commit, before the effect publish
// -----------------------------------------------------------------------------------------

/// A record that reached the journal is a fact, but its effects may never have been handed to
/// the world — the crash landed between `commit` and the host's dispatch. On rebuild the
/// pending set is republished **with the same effect ids**: identity is durable, so a host that
/// re-publishes cannot fork the effect, and a host that already ran one costs only a
/// `Replayed` resolution (§5g-1, DEC-1).
#[test]
fn crash_after_commit_before_publish_republishes_the_same_effect_ids() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    runtime.submit(&envelopes[0]);
    let started = runtime.submit(&envelopes[1]);

    // The host crashed here: the step is durable, its effect never reached the provider.
    let published: Vec<String> = started
        .published_effects()
        .iter()
        .map(|effect| effect.effect_id.as_str().to_string())
        .collect();
    assert_eq!(
        published.len(),
        1,
        "the fixture publishes exactly one effect"
    );
    assert_eq!(
        started.published_effects()[0].tag(),
        EffectKindTag::CallProvider
    );

    let rebuilt = Runtime::rebuild(&runtime.journal);
    let republished: Vec<String> = rebuilt
        .tx
        .pending_effects_in_order()
        .iter()
        .map(|effect| effect.effect_id.as_str().to_string())
        .collect();
    assert_eq!(
        republished, published,
        "§5g-1 · the recovery hands back the same effect identities to (re-)execute",
    );
    assert_eq!(fingerprint(&rebuilt), fingerprint(&runtime));
    assert_eq!(
        rebuilt.tx.head().unwrap().digest,
        started.record.record_digest().clone(),
        "the rebuild stands on the committed record, not before it",
    );
}

// -----------------------------------------------------------------------------------------
// scenario 4 · crash after the publish, before the ResolveEffect
// -----------------------------------------------------------------------------------------

/// The effect reached the world and no resolution ever came back. The reboot replays the
/// journal, finds the same pending effect, and the redelivered resolution commits the **same
/// record** it would have committed without the crash.
#[test]
fn crash_after_publish_before_resolve_redelivers_to_the_same_record() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    runtime.submit(&envelopes[0]);
    runtime.submit(&envelopes[1]);

    let rebuilt = Runtime::rebuild(&runtime.journal);
    let mut rebuilt = rebuilt;
    rebuilt.submit(&envelopes[2]);

    let mut uninterrupted = Runtime::new();
    drive(&mut uninterrupted, &envelopes[..3]);

    assert_eq!(
        digests(&rebuilt),
        digests(&uninterrupted),
        "the post-crash resolution writes the same record the uninterrupted run wrote",
    );
    assert_eq!(fingerprint(&rebuilt), fingerprint(&uninterrupted));
}

// -----------------------------------------------------------------------------------------
// scenario 5 · crash before the checkpoint install
// -----------------------------------------------------------------------------------------

/// `checkpoint_candidate` is `&self` generation: it installs nothing, so a crash before the
/// install loses nothing and leaves no residue. Regenerating the candidate produces the same
/// checkpoint bytes, and the transaction it observed is untouched by having been observed.
#[test]
fn crash_before_install_a_candidate_is_pure_and_regenerable() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);

    let head_before = runtime.tx.head();
    let tail_before = runtime.tx.tail_usage();
    let boundary_before = runtime.tx.checkpoint_boundary();

    let first = runtime
        .checkpoint()
        .decode()
        .expect("the host decoded the blob");
    let first_digest = first.checkpoint_digest().clone();
    // ... and the process dies before the CAS install. Drop everything, regenerate.
    drop(first);

    let regenerated = runtime
        .checkpoint()
        .decode()
        .expect("regenerates just as well");

    assert_eq!(
        runtime.tx.head(),
        head_before,
        "generation is a read: the durable head did not move",
    );
    assert_eq!(runtime.tx.tail_usage(), tail_before);
    assert_eq!(runtime.tx.checkpoint_boundary(), boundary_before);
    assert_eq!(
        regenerated.checkpoint_digest(),
        &first_digest,
        "the regenerated candidate is byte-identical — nothing was in flight to lose",
    );
}

// -----------------------------------------------------------------------------------------
// scenario 6 · crash after the install, before the ack
// -----------------------------------------------------------------------------------------

/// The checkpoint blob is in the CAS store; the kernel was never acked. The ack is a
/// *retention* signal, not a durability one (§12.3 rule 5), so:
///
/// - the surviving process acks normally afterwards and the tail is reclaimed;
/// - a process that *did* die restores from the installed blob and is republished the effects
///   it is waiting on (§5g-1).
#[test]
fn crash_after_install_before_ack_still_installs_acks_and_restores() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);

    let candidate = runtime.checkpoint();
    let installed = candidate.decode().expect("the host installed this blob");
    let installed_digest = installed.checkpoint_digest().clone();
    // ... and the process dies. `note_checkpoint_acked` was never called.

    // The install was observed twice (CAS read-back) — the blob is the same blob.
    let reread = candidate.decode().expect("re-read from the store");
    assert_eq!(reread.checkpoint_digest(), &installed_digest);

    // arm A · the surviving process acks after the fact; the covered tail is reclaimed.
    let usage_before = runtime.tx.tail_usage();
    assert!(usage_before.records > 0, "the fixture carries a live tail");
    let usage = runtime
        .tx
        .note_checkpoint_acked(&installed.boundary())
        .expect("the ack is a retention signal and lands whenever it is delivered");
    assert_eq!(
        usage.records, 0,
        "a full-state candidate at the head covers the whole tail",
    );
    assert_eq!(
        runtime.tx.head().map(|head| head.digest),
        Some(installed.covered_transaction_head_digest().clone()),
        "the ack reclaims accounting, never history",
    );

    // arm B · the reboot: the installed blob restores, and the wait is re-exposed.
    let above: Vec<KernelRecord> = runtime
        .journal
        .iter()
        .filter(|record| record.step_seq().get() > installed.through_step_seq().get())
        .cloned()
        .collect();
    let restored = Runtime::restore(&installed, &above);
    assert_eq!(restored.restore_cost.unwrap().records_before_checkpoint, 0);
    assert_eq!(
        restored
            .tx
            .pending_effects_in_order()
            .iter()
            .map(|effect| effect.effect_id.as_str().to_string())
            .collect::<Vec<_>>(),
        vec![effect_id(WireU64::new(3)).to_string()],
        "§5g-1 · the effect the operation is waiting on is exposed again",
    );
}

// -----------------------------------------------------------------------------------------
// scenario 7 · crash after the ack, before the journal pruning
// -----------------------------------------------------------------------------------------

/// The kernel reclaimed its tail accounting; the host died before pruning the physical
/// journal. On reboot the journal still holds records the kernel considers reclaimed — and a
/// rebuild over the *whole* journal must still work, the re-ack must still succeed (the prune
/// is idempotent when replayed), and a redelivery from the reclaimed prefix must still be a
/// replay, never a second acceptance (§12.3 rule 7: an ack never empties the idempotence
/// window).
#[test]
fn crash_after_ack_before_prune_reack_converges() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    let boundary = checkpoint.boundary();
    runtime
        .tx
        .note_checkpoint_acked(&boundary)
        .expect("the boundary names a prefix of this journal");
    let head_after_ack = runtime.tx.head();
    // ... and the host dies. The journal was never pruned: all four records are on disk.

    // Rebuild over the un-pruned journal — records the kernel had already reclaimed included.
    let mut rebuilt = Runtime::rebuild(&runtime.journal);
    assert_eq!(
        rebuilt.tx.head(),
        head_after_ack,
        "the un-pruned prefix replays to the same head",
    );
    assert!(
        rebuilt.tx.tail_usage().records > 0,
        "the rebuild re-materialises the tail the ack had reclaimed — physical state is honest again",
    );

    // The prune is replayed: the same boundary acks again and converges to the same accounting.
    let usage = rebuilt
        .tx
        .note_checkpoint_acked(&boundary)
        .expect("the replayed prune hits the same boundary");
    assert_eq!(
        usage.records, 0,
        "the reclaim lands exactly as the first one did"
    );

    // The idempotence window survived the reconstruction: a redelivery from below the
    // re-acked boundary is still a replay of the durable record — the un-pruned journal
    // answers for it — and never a new acceptance.
    let redelivered = rebuilt.prepare(&envelopes[1]);
    assert!(
        redelivered.token().is_none(),
        "a replay offers nothing to commit",
    );
    let RecordPreparation::Replayed(replay) = redelivered else {
        panic!("an acked input must not be accepted a second time");
    };
    assert_eq!(
        replay.record_digest,
        *runtime.journal[1].record_digest(),
        "the answer is still the original record digest",
    );
    assert!(
        replay.record.is_some() && replay.committed_step.is_some(),
        "the un-pruned journal reproduces the step from disk rather than re-executing it",
    );
}

// -----------------------------------------------------------------------------------------
// scenario 8 · CAS conflict during the append
// -----------------------------------------------------------------------------------------

/// Two writers, one journal. The loser's CAS precondition fails; `note_append_conflict`
/// discards its candidate and fails closed. Recovery is the host-side loop the fault names:
/// rebuild from the journal the winner wrote — where the winner's record already answers the
/// disputed input — and the redelivery is a replay by reference, never a second acceptance.
/// Both writers then stand on the chain a conflict-free run reaches.
#[test]
fn append_conflict_discards_the_candidate_and_rebuild_replays() {
    let envelopes = turn_envelopes();

    // A shared base: both writers stand on the same configured, started journal.
    let mut base = Runtime::new();
    drive(&mut base, &envelopes[..2]);
    let shared = base.journal.clone();

    let mut winner = Runtime::rebuild(&shared);
    let mut loser = Runtime::rebuild(&shared);

    // Both plan the same next resolution against the same expected head.
    let preparation = loser.prepare(&envelopes[2]);
    let token: PrepareToken = preparation.token().unwrap().clone();
    let loser_head = preparation.record().unwrap().record_digest().clone();

    // The winner's append lands first.
    winner.submit(&envelopes[2]);
    let actual_head = winner.tx.head().unwrap().digest.clone();

    // The loser's CAS fails; the conflict poisons the runtime and drops the candidate.
    let conflict = loser.tx.note_append_conflict(&token, Some(&actual_head));
    assert_eq!(conflict.code, KernelFaultCode::TransactionConflict);
    assert!(
        conflict.message.contains(&loser_head.to_string()),
        "the fault names the head the loser expected",
    );
    assert!(
        conflict.message.contains(&actual_head.to_string()),
        "and the head the journal actually holds",
    );

    // Fail closed: the poisoned runtime refuses to prepare anything further.
    let refused = loser.prepare(&envelopes[3]);
    assert_eq!(
        refused.fault().map(|fault| fault.code),
        Some(KernelFaultCode::TransactionConflict),
        "the poison propagates until the host rebuilds",
    );

    // The host-side closure: rebuild from the journal the winner wrote. The disputed input is
    // already durable there — the redelivery is answered by the winner's record.
    let mut recovered = Runtime::rebuild(&winner.journal);
    let redelivered = recovered.prepare(&envelopes[2]);
    assert!(
        redelivered.token().is_none(),
        "the replay offers nothing to commit",
    );
    let RecordPreparation::Replayed(replay) = redelivered else {
        panic!("the winner's record answers the redelivery");
    };
    assert_eq!(
        replay.record_digest,
        *winner.journal[2].record_digest(),
        "the disputed input resolves to the winner's record, not a competing one",
    );

    // Forward progress: both writers and a conflict-free control converge on one chain.
    recovered.submit(&envelopes[3]);
    winner.submit(&envelopes[3]);
    let mut control = Runtime::new();
    drive(&mut control, &envelopes[..4]);
    assert_eq!(digests(&winner), digests(&control));
    assert_eq!(digests(&recovered), digests(&control));
    assert_eq!(fingerprint(&recovered), fingerprint(&control));
}

// -----------------------------------------------------------------------------------------
// scenario 9 · checkpoint + incomplete tail
// -----------------------------------------------------------------------------------------

/// The checkpoint was taken and acked mid-run, the host pruned the covered prefix, more records
/// accumulated, and *then* the process died. What a reboot holds is the blob plus a tail that
/// starts above the checkpoint — the ladder's ordinary shape, and it must land exactly on the
/// head the uninterrupted run reached, reading nothing from below the base.
#[test]
fn checkpoint_plus_incomplete_tail_restores_to_head() {
    let envelopes = turn_envelopes();

    // Checkpoint after step 1; two more records accumulate; the covered prefix is pruned.
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..2]);
    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    runtime
        .tx
        .note_checkpoint_acked(&checkpoint.boundary())
        .expect("the host acked the install");
    drive(&mut runtime, &envelopes[2..4]);
    let pruned: Vec<KernelRecord> = runtime
        .journal
        .iter()
        .filter(|record| record.step_seq().get() > checkpoint.through_step_seq().get())
        .cloned()
        .collect();
    assert_eq!(pruned.len(), 2, "the host pruned the covered prefix");

    // The reboot: blob + incomplete tail.
    let restored = Runtime::restore(&checkpoint, &pruned);
    let cost = restored.restore_cost.unwrap();
    assert_eq!(
        cost.records_before_checkpoint, 0,
        "nothing below the base is read"
    );
    assert_eq!(
        cost.tail_inputs_replayed, 0,
        "a full-state candidate carries no tail of its own"
    );
    assert_eq!(
        cost.records_after_checkpoint, 2,
        "the incomplete tail is what is replayed"
    );

    // And it lands exactly where the uninterrupted run stands.
    let mut uninterrupted = Runtime::new();
    drive(&mut uninterrupted, &envelopes[..4]);
    assert_eq!(fingerprint(&restored), fingerprint(&uninterrupted));

    // Forward progress from the restored seam is the history that would have been written.
    // The restored journal view starts above the checkpoint, so the comparison runs against
    // the uninterrupted history's corresponding suffix.
    let mut restored = restored;
    drive(&mut restored, &envelopes[4..]);
    drive(&mut uninterrupted, &envelopes[4..]);
    let offset = checkpoint.through_step_seq().get() as usize + 1;
    assert_eq!(
        digests(&restored),
        digests(&uninterrupted)[offset..],
        "the records above the checkpoint are written identically",
    );
}

// -----------------------------------------------------------------------------------------
// scenario 10 · redelivery below the checkpoint base
// -----------------------------------------------------------------------------------------

/// After the ack the covered prefix is gone — journal pruned, tail reclaimed — yet a redelivery
/// of an input from down there must still be answered: by reference, with the original record
/// digest, never as a new step (§12.3 rule 10). The load-bearing half of this row is *where*
/// the answer comes from: the replay/dedupe ledger rides inside the checkpoint's logical
/// state, so a runtime restored from the blob alone — no records at all — still knows every
/// input it ever accepted. An ack never empties the idempotence window (K5).
#[test]
fn redelivery_below_the_base_is_an_ack_not_a_step() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    runtime
        .tx
        .note_checkpoint_acked(&checkpoint.boundary())
        .expect("the host acked the install");

    // The reboot holds the blob and nothing else: the covered prefix was pruned and no
    // post-checkpoint records exist yet.
    let mut restored = Runtime::restore(&checkpoint, &[]);
    assert_eq!(
        restored.restore_cost.unwrap().records_before_checkpoint,
        0,
        "the covered prefix is gone from every surface",
    );
    let head_before = restored.tx.head();

    // Redeliver inputs the reclaimed prefix carried — the genesis configure and the start
    // that drove the operation's first turn.
    for (index, envelope) in envelopes.iter().take(2).enumerate() {
        let redelivered = restored.prepare(envelope);
        let RecordPreparation::Replayed(replay) = redelivered else {
            panic!("a redelivery below the checkpoint base must not be accepted again");
        };
        assert_eq!(
            replay.record_digest,
            *runtime.journal[index].record_digest(),
            "§12.3 rule 10 · the answer is the original step and record digest",
        );
        assert!(
            replay.committed_step.is_none() && replay.record.is_none(),
            "idempotent acknowledgement, not step reproduction",
        );
        assert!(replay.step_seq == runtime.journal[index].step_seq());
    }

    assert_eq!(
        restored.tx.head(),
        head_before,
        "an acknowledgement writes nothing",
    );

    // And the operation still runs forward from where it stands — every below-base input is
    // answered by reference, and only post-checkpoint inputs are new steps.
    let offset = checkpoint.through_step_seq().get() as usize + 1;
    drive(&mut restored, &envelopes[offset..]);
    let mut uninterrupted = Runtime::new();
    drive(&mut uninterrupted, &envelopes);
    assert_eq!(
        digests(&restored),
        digests(&uninterrupted)[offset..],
        "the post-checkpoint history is written identically",
    );
    assert_eq!(fingerprint(&restored), fingerprint(&uninterrupted));
}
