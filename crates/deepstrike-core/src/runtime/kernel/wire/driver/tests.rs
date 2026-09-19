use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;
use crate::runtime::kernel::wire::checkpoint::{
    CanonicalInput, CheckpointCandidate, CheckpointDraft, KernelCheckpoint,
};
use crate::runtime::kernel::wire::config::ConfigDefaults;
use crate::runtime::kernel::wire::config::TailBounds;
use crate::runtime::kernel::wire::config::{
    EntropyWatchPolicy, ExecutionPolicy, HostEffectSupport, MilestonePhase as WireMilestonePhase,
    OperationConfig, VerificationContract as WireVerificationContract,
};
use crate::runtime::kernel::wire::effect::{
    EffectSucceeded, TaskLaunchOutcome, TaskLaunchStarted, TaskLaunchStatus, TasksSpawnedSuccess,
};
use crate::runtime::kernel::wire::envelope::{
    ConfigureOperation, DeliverExternalEvent, KernelInput, ResolveEffect, StartOperation,
    WireEnvelope,
};
use crate::runtime::kernel::wire::event::{ChildResult, DeliverSignal, LogicalSignal};
use crate::runtime::kernel::wire::fault::PrepareToken;
use crate::runtime::kernel::wire::record::{KernelRecord, RecordPreparation, verify_record_chain};
use crate::runtime::kernel::wire::restore::{RestoreCost, RestoredOperation, restore_operation};
use crate::runtime::kernel::wire::root::{
    RootAgentEntry, RootWorkflowEntry, WorkflowNode as WireNode,
};
use crate::runtime::kernel::wire::scalar::{
    AttemptId as WireAttemptId, DeliveryId, InputId, Ppm, SignalId,
};
use crate::runtime::kernel::wire::transaction::{
    CheckpointBoundary, CommittedTransition, InMemoryRecordIndex, KernelTransaction, TailPressure,
};
use crate::scheduler::tcb::TaskLifecycle;

const OPERATION: &str = "op-driver-1";

// -----------------------------------------------------------------------------------------
// §8.2 host loop: prepare → (CAS append) → commit → fold
// -----------------------------------------------------------------------------------------

/// The whole durable path, exactly as a host runs it. Nothing here reaches into the driver
/// behind the transaction's back: a step is planned by the driver, appended by the "host"
/// (this in-memory journal), committed by the transaction, and only then folded into the
/// driver's root-kind/focus state.
struct Runtime {
    tx: KernelTransaction<PlannedStep, InMemoryRecordIndex>,
    driver: CanonicalOperationDriver,
    journal: Vec<KernelRecord>,
    last_observations: Vec<KernelObservation>,
    /// What the restore that produced this runtime read, when it came from one.
    restore_cost: Option<RestoreCost>,
}

impl Runtime {
    fn new() -> Self {
        Self {
            tx: KernelTransaction::new(ConfigDefaults::default(), InMemoryRecordIndex::new()),
            driver: CanonicalOperationDriver::new(),
            journal: Vec::new(),
            last_observations: Vec::new(),
            restore_cost: None,
        }
    }

    fn prepare(&mut self, envelope: &WireEnvelope) -> RecordPreparation<PlannedStep> {
        let Self { tx, driver, .. } = self;
        tx.prepare(envelope, |context| driver.plan(context))
    }

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
        self.last_observations = committed.step.observations.clone();
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

    /// Same durable path, but the step comes from a planner the *test* supplies. Used for the
    /// two transitions Task 9 deliberately leaves to Task 10/12 — the P1 syscall that starts a
    /// nested workflow, and the provider resolution that frees the pending provider effect.
    fn submit_planned<F>(
        &mut self,
        envelope: &WireEnvelope,
        plan: F,
    ) -> CommittedTransition<PlannedStep>
    where
        F: FnOnce(
            &mut CanonicalOperationDriver,
            &PlanContext<'_>,
        ) -> Result<PlannedStep, KernelFault>,
    {
        let preparation = {
            let Self { tx, driver, .. } = self;
            tx.prepare(envelope, |context| plan(driver, context))
        };
        self.append_and_commit(preparation)
    }

    fn reject(&mut self, envelope: &WireEnvelope) -> KernelFault {
        let preparation = self.prepare(envelope);
        preparation
            .fault()
            .unwrap_or_else(|| panic!("expected a rejection, got a prepared step"))
            .clone()
    }

    /// §12.2 · the host's own restore call: a checkpoint blob plus the journal records above the
    /// step it covers. Nothing else — in particular, **not** the records the checkpoint covers,
    /// which is what makes the cost assertions meaningful.
    fn restore(&self, checkpoint: &KernelCheckpoint) -> Runtime {
        let after: Vec<KernelRecord> = self
            .journal
            .iter()
            .filter(|record| record.step_seq().get() > checkpoint.through_step_seq().get())
            .cloned()
            .collect();
        Self::restore_with(Some(checkpoint), &after)
    }

    fn restore_with(checkpoint: Option<&KernelCheckpoint>, records: &[KernelRecord]) -> Runtime {
        let RestoredOperation {
            transaction,
            driver,
            cost,
        } = restore_operation(
            checkpoint,
            records,
            ConfigDefaults::default(),
            InMemoryRecordIndex::from_records(records),
        )
        .expect("the restore ladder runs to completion");
        Runtime {
            tx: transaction,
            driver,
            journal: Vec::new(),
            last_observations: Vec::new(),
            restore_cost: Some(cost),
        }
    }

    /// The whole journal, as the host holds it.
    fn journal_from(&self, checkpoint: &KernelCheckpoint) -> Vec<KernelRecord> {
        self.journal
            .iter()
            .filter(|record| record.step_seq().get() > checkpoint.through_step_seq().get())
            .cloned()
            .collect()
    }

    fn pending_effect_kinds(&self) -> Vec<EffectKindTag> {
        self.tx.pending_effects().map(|e| e.tag()).collect()
    }

    fn observations(&self) -> &[KernelObservation] {
        &self.last_observations
    }

    /// §12.3 · exactly the host's call: project the driver's three partitions, hand them to
    /// the transaction, get a candidate. Nothing here reaches around either layer.
    fn checkpoint(&self) -> CheckpointCandidate {
        self.tx
            .checkpoint_candidate(self.driver.project_logical_state())
            .expect("a configured operation has a logical state to checkpoint")
    }
}

// -----------------------------------------------------------------------------------------
// envelopes
// -----------------------------------------------------------------------------------------

fn operation() -> OperationId {
    OperationId::new(OPERATION).unwrap()
}

fn envelope(id: &str, observed_at_ms: u64, input: KernelInput) -> WireEnvelope {
    WireEnvelope::new(
        operation(),
        InputId::new(id).unwrap(),
        WireU64::new(observed_at_ms),
        input,
    )
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

fn configure() -> WireEnvelope {
    configure_supporting([EffectKindTag::CallProvider, EffectKindTag::SpawnTasks])
}

fn configure_supporting(supported: impl IntoIterator<Item = EffectKindTag>) -> WireEnvelope {
    envelope(
        "in-configure",
        1_700_000_000_000,
        KernelInput::ConfigureOperation(ConfigureOperation {
            config: boot_config(supported),
        }),
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

/// Most driver fixtures exercise the complete configured tool surface. Keep that authority
/// explicit so the tests do not depend on a permissive missing-baseline fallback.
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

fn agent_start_with_capabilities(
    id: &str,
    observed_at_ms: u64,
    requested_capabilities: Vec<crate::types::capability::Capability>,
) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Agent(RootAgentEntry {
                task: LogicalTask::new("write the research brief"),
                run_spec: Some(test_agent_spec("write the research brief")),
            }),
            initial_context: InitialContext {
                requested_capabilities,
                ..InitialContext::default()
            },
        }),
    )
}

fn wire_node(node_id: &str, goal: &str, depends_on: &[&str]) -> WireNode {
    WireNode {
        node_id: NodeId::new(node_id).unwrap(),
        task: LogicalTask::new(goal),
        depends_on: depends_on
            .iter()
            .map(|id| NodeId::new(*id).unwrap())
            .collect(),
        run_spec: Some(test_agent_spec(goal)),
    }
}

fn two_node_spec() -> WireSpec {
    WireSpec {
        name: "brief".to_string(),
        nodes: vec![
            wire_node("collect", "collect the sources", &[]),
            wire_node("write", "write the brief", &["collect"]),
        ],
    }
}

fn workflow_start(id: &str, observed_at_ms: u64, spec: WireSpec) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Workflow(RootWorkflowEntry { spec }),
            initial_context: InitialContext::default(),
        }),
    )
}

fn spawned(id: &str, observed_at_ms: u64, effect_id: &EffectId, tasks: &[&str]) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: effect_id.clone(),
            outcome: EffectOutcome::Succeeded(EffectSucceeded {
                result: EffectSuccess::TasksSpawned(TasksSpawnedSuccess {
                    attempts: tasks
                        .iter()
                        .map(|task| TaskLaunchOutcome {
                            task_id: TaskId::new(*task).unwrap(),
                            attempt_id: WireAttemptId::new(format!("{task}:attempt:1")).unwrap(),
                            outcome: TaskLaunchStatus::Started(TaskLaunchStarted {}),
                        })
                        .collect(),
                }),
            }),
        }),
    )
}

fn spawned_attempt(
    id: &str,
    observed_at_ms: u64,
    effect_id: &EffectId,
    task: &str,
    attempt: u32,
) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: effect_id.clone(),
            outcome: EffectOutcome::Succeeded(EffectSucceeded {
                result: EffectSuccess::TasksSpawned(TasksSpawnedSuccess {
                    attempts: vec![TaskLaunchOutcome {
                        task_id: TaskId::new(task).unwrap(),
                        attempt_id: WireAttemptId::new(format!("{task}:attempt:{attempt}"))
                            .unwrap(),
                        outcome: TaskLaunchStatus::Started(TaskLaunchStarted {}),
                    }],
                }),
            }),
        }),
    )
}

fn child_done(id: &str, observed_at_ms: u64, task: &str, output: &str) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::ChildCompleted(ChildCompleted {
                task_id: TaskId::new(task).unwrap(),
                attempt_id: WireAttemptId::new(format!("{task}:attempt:1")).unwrap(),
                result: ChildResult {
                    status: ChildStatus::Completed,
                    output: Some(output.to_string()),
                    ..ChildResult::default()
                },
                parent_requests: Vec::new(),
            }),
        }),
    )
}

/// Drives [`CanonicalOperationDriver::begin_nested_workflow`] as a direct API call, on an
/// envelope that carries no request of its own.
///
/// The real P1 carrier is now the provider resolution (see `provider_result` and the §7.6
/// tests); these Task 9 tests keep using the direct entry point because what they assert is the
/// focus/authority arc, which both paths share — `plan_provider_syscalls` reduces
/// `SubmitWorkflow` onto exactly this code.
fn syscall_carrier(id: &str, observed_at_ms: u64) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::DeliverSignal(DeliverSignal {
                delivery_id: DeliveryId::new(format!("delivery-{id}")).unwrap(),
                attempt: 1,
                signal: LogicalSignal::new(SignalId::new(format!("sig-{id}")).unwrap()),
            }),
        }),
    )
}

fn effect_id(step_seq: WireU64) -> EffectId {
    EffectId::new(format!("{OPERATION}:step:{step_seq}:effect:0")).unwrap()
}

fn effect_id_at(step_seq: WireU64, index: u32) -> EffectId {
    EffectId::new(format!("{OPERATION}:step:{step_seq}:effect:{index}")).unwrap()
}

// -----------------------------------------------------------------------------------------
// §7.6 · syscall envelopes
// -----------------------------------------------------------------------------------------

fn syscall_tool_catalog() -> Vec<WireToolSchema> {
    SYSCALL_TOOL_NAMES
        .iter()
        .chain(std::iter::once(&"search"))
        .map(|name| WireToolSchema {
            name: (*name).to_string(),
            description: String::new(),
            parameters: Default::default(),
        })
        .collect()
}

/// A configuration whose operation can actually reach every P1 syscall: the meta-tool surface
/// is in the catalog, memory is bound read+write, one skill is declared, and the host supports
/// the effects the memory syscalls publish.
fn syscall_config() -> WireEnvelope {
    syscall_config_with(|_| {})
}

/// The same configuration with one edit applied — how the effect-resolution tests declare the
/// extra host support (approval, page-out, milestone) or a governance policy their arc needs.
fn syscall_config_with(edit: impl FnOnce(&mut OperationConfig)) -> WireEnvelope {
    use crate::runtime::kernel::wire::config::{
        MemoryPolicy, ResourceQuota, SkillMetadata as WireSkill,
    };
    use crate::runtime::kernel::wire::effect::{MemoryAccessBinding, MemoryCapabilities};
    use crate::runtime::kernel::wire::scalar::MemoryBindingId;

    let mut config = {
        OperationConfig {
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
            skill_catalog: vec![WireSkill {
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
        }
    };
    edit(&mut config);
    envelope(
        "in-configure",
        1_700_000_000_000,
        KernelInput::ConfigureOperation(ConfigureOperation { config }),
    )
}

fn tool_call(call_id: &str, name: &str, arguments: Value) -> WireToolCall {
    WireToolCall {
        call_id: super::super::scalar::CallId::new(call_id).unwrap(),
        name: name.to_string(),
        arguments: super::super::scalar::BoundedJson::new(arguments).unwrap(),
    }
}

/// A provider result carrying tool calls — the only wire shape from which a `ProviderTool`
/// causation can be derived.
fn provider_result(
    id: &str,
    observed_at_ms: u64,
    effect: &EffectId,
    calls: Vec<WireToolCall>,
) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: effect.clone(),
            outcome: EffectOutcome::Succeeded(EffectSucceeded {
                result: EffectSuccess::Provider(super::super::effect::ProviderSuccess {
                    outcome: super::super::effect::ProviderOutcome::Completed(
                        super::super::effect::ProviderCompleted {
                            message: ProviderMessage {
                                role: MessageRole::Assistant,
                                content: String::new(),
                                tool_calls: calls,
                                tool_call_id: None,
                                tokens: None,
                            },
                            observed_input_tokens: None,
                            observed_output_tokens: None,
                            stop_reason: None,
                        },
                    ),
                }),
            }),
        }),
    )
}

fn child_done_with(
    id: &str,
    observed_at_ms: u64,
    task: &str,
    attempt: &str,
    requests: Vec<SyscallRequest>,
) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::ChildCompleted(ChildCompleted {
                task_id: TaskId::new(task).unwrap(),
                attempt_id: WireAttemptId::new(attempt).unwrap(),
                result: ChildResult {
                    status: ChildStatus::Completed,
                    output: Some("done".to_string()),
                    ..ChildResult::default()
                },
                parent_requests: requests,
            }),
        }),
    )
}

fn child_failed(
    id: &str,
    observed_at_ms: u64,
    task: &str,
    attempt: u32,
    reason: &str,
) -> WireEnvelope {
    envelope(
        id,
        observed_at_ms,
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::ChildCompleted(ChildCompleted {
                task_id: TaskId::new(task).unwrap(),
                attempt_id: WireAttemptId::new(format!("{task}:attempt:{attempt}")).unwrap(),
                result: ChildResult {
                    status: ChildStatus::Failed,
                    error: Some(reason.to_string()),
                    ..ChildResult::default()
                },
                parent_requests: Vec::new(),
            }),
        }),
    )
}

fn node_args(nodes: &[WireNode]) -> Value {
    json!({ "nodes": serde_json::to_value(nodes).unwrap() })
}

/// `(operation, subject, reason)` of every structured rejection the last transition recorded.
fn rejections(runtime: &Runtime) -> Vec<(String, Option<String>, String)> {
    runtime
        .observations()
        .iter()
        .filter_map(|observation| match observation {
            KernelObservation::ControlRequestRejected {
                operation,
                subject,
                reason,
                ..
            } => Some((operation.clone(), subject.clone(), reason.clone())),
            _ => None,
        })
        .collect()
}

fn sole_effect(committed: &CommittedTransition<PlannedStep>) -> &KernelEffect {
    let effects = committed.published_effects();
    assert_eq!(effects.len(), 1, "expected exactly one published effect");
    &effects[0]
}

// -----------------------------------------------------------------------------------------
// fixture: agent-configure-is-genesis
// -----------------------------------------------------------------------------------------

#[test]
fn the_first_accepted_input_must_be_the_configuration() {
    let mut runtime = Runtime::new();

    // a business input before any configuration
    let fault = runtime.reject(&agent_start("in-early-start", 1_700_000_000_100));
    assert_eq!(fault.code, KernelFaultCode::InvalidLifecycle);
    assert_eq!(runtime.tx.head(), None, "a rejection moves nothing");
    assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Created);
    assert!(
        runtime.driver.engine().is_none(),
        "no semantic kernel exists"
    );

    let genesis = runtime.submit(&configure());
    assert_eq!(genesis.step_seq, WireU64::ZERO);
    assert_eq!(
        genesis.record.previous_record_digest(),
        None,
        "the genesis record has no predecessor (§8.1)"
    );
    assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Configured);
    assert!(runtime.driver.engine().is_some());
    assert_eq!(
        runtime.driver.root_kind(),
        None,
        "configuring is not starting"
    );

    // the genesis record stores the *resolved* configuration, not the sparse input
    let stored = genesis.record.normalized_input().unwrap();
    let resolved = stored
        .resolved_config()
        .expect("genesis carries the config");
    assert_eq!(resolved.execution_policy.max_turns, 12);
    assert_eq!(
        resolved.execution_policy.max_context_tokens,
        ConfigDefaults::default()
            .baseline
            .execution_policy
            .max_context_tokens,
        "every default this operation runs on is frozen in its first record"
    );
}

// -----------------------------------------------------------------------------------------
// fixture: agent-root-start-is-atomic + agent-start-issues-model-turn
// -----------------------------------------------------------------------------------------

#[test]
fn an_agent_root_start_is_one_atomic_input_that_issues_the_model_turn() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    // root kind and initial context are fixed by this one accepted input
    assert_eq!(started.step.root_kind, Some(RootKind::Agent));
    assert_eq!(runtime.driver.root_kind(), Some(RootKind::Agent));
    assert_eq!(
        runtime.driver.focus(),
        Some(&ExecutionFocus::agent_turn(root_task_id())),
    );
    assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Running);

    // exactly one pending effect, and it is the provider call
    let effect = sole_effect(&started);
    assert_eq!(effect.tag(), EffectKindTag::CallProvider);
    assert_eq!(
        effect.effect_id,
        effect_id(started.step_seq),
        "the effect id is minted by the kernel from the step it belongs to"
    );
    assert_eq!(
        effect.causation_input_id.as_str(),
        "in-start",
        "every effect names the accepted input that produced it"
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::CallProvider]
    );

    // the rendered context is real: the goal reached the P3 context VM
    let EffectKind::CallProvider(call) = &effect.effect else {
        panic!("expected a provider call");
    };
    let candidate = &call.context_candidate;
    candidate.state.verify_digest().unwrap();
    assert_eq!(candidate.operation_id, OPERATION);
    assert_eq!(candidate.input_sequence, started.step_seq.get());
    assert_eq!(
        candidate.step_id,
        format!("{OPERATION}:step:{}", started.step_seq)
    );
    assert_eq!(
        candidate.state,
        runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .context_state()
            .unwrap()
    );
    let projection =
        crate::runtime::kernel::wire::record::canonical_bytes(&(&call.context, &call.tools))
            .unwrap();
    assert_eq!(
        candidate.rendered_snapshot,
        crate::evolution::ContentDigest::from_bytes(projection.as_slice())
    );
    let (plan, input) = candidate
        .bind(
            crate::evolution::ContentDigest::from_bytes(b"host-measurement"),
            crate::evolution::ContentDigest::from_bytes(b"host-route"),
        )
        .unwrap();
    plan.verify(&candidate.state).unwrap();
    input.verify(&plan).unwrap();
    let mut changed_context = call.context.clone();
    changed_context.system_stable.push_str("tampered");
    let changed_projection =
        crate::runtime::kernel::wire::record::canonical_bytes(&(&changed_context, &call.tools))
            .unwrap();
    assert_ne!(
        candidate.rendered_snapshot,
        crate::evolution::ContentDigest::from_bytes(changed_projection.as_slice())
    );
    let rendered = format!(
        "{}{}",
        call.context.system_stable, call.context.system_knowledge
    );
    let state_turn = call
        .context
        .state_turn
        .as_ref()
        .map(|turn| turn.content.clone())
        .unwrap_or_default();
    assert!(
        rendered.contains("research brief") || state_turn.contains("research brief"),
        "the root task's goal must be in the rendered context, not merely stored"
    );
    assert!(
        !call.context.turns.is_empty(),
        "the start seeded a first turn"
    );
}

#[test]
fn a_second_root_start_is_refused_with_zero_mutation() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    let before = (
        runtime.tx.head(),
        runtime.tx.lifecycle(),
        runtime.pending_effect_kinds(),
        runtime.driver.root_kind(),
        runtime.driver.focus().cloned(),
    );

    let fault = runtime.reject(&agent_start("in-start-again", 1_700_000_002_000));
    assert_eq!(fault.code, KernelFaultCode::InvalidLifecycle);

    let after = (
        runtime.tx.head(),
        runtime.tx.lifecycle(),
        runtime.pending_effect_kinds(),
        runtime.driver.root_kind(),
        runtime.driver.focus().cloned(),
    );
    assert_eq!(before, after, "a refused root start moves nothing");
    assert_eq!(runtime.journal.len(), 2, "no third record exists");
    assert_eq!(started.step.root_kind, Some(RootKind::Agent));
}

#[test]
fn a_workflow_root_start_after_an_agent_root_cannot_re_root_the_operation() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    let fault = runtime.reject(&workflow_start(
        "in-reroot",
        1_700_000_002_000,
        two_node_spec(),
    ));
    assert_eq!(fault.code, KernelFaultCode::InvalidLifecycle);
    assert_eq!(
        runtime.driver.root_kind(),
        Some(RootKind::Agent),
        "the root kind is immutable for the operation's lifetime (§6.1.5)"
    );
}

/// DEC-8 fail-closed, at the root start rather than at the first emission. `call_provider`
/// support is already mandatory at configuration time (every operation reaches a provider
/// call), so the reachable half of the rule is a workflow root on a host that cannot spawn.
#[test]
fn a_workflow_root_is_refused_when_the_host_cannot_spawn_tasks() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure_supporting([EffectKindTag::CallProvider]));
    let fault = runtime.reject(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    assert_eq!(fault.code, KernelFaultCode::UnsupportedEffect);
    assert_eq!(runtime.driver.root_kind(), None);
    assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Configured);
    assert_eq!(runtime.journal.len(), 1, "only the genesis record exists");
}

// -----------------------------------------------------------------------------------------
// fixture: workflow-root-entry-is-atomic
// -----------------------------------------------------------------------------------------

#[test]
fn a_workflow_root_start_spawns_tasks_and_never_calls_the_provider() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));

    assert_eq!(started.step.root_kind, Some(RootKind::Workflow));
    let effect = sole_effect(&started);
    assert_eq!(
        effect.tag(),
        EffectKindTag::SpawnTasks,
        "a workflow root's first effect is a task spawn (§10.1)"
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::SpawnTasks],
        "no provider effect is published, so none can be overwritten by a workflow load"
    );

    // the DAG's identity is kernel-minted and its first ready node is the only launch
    let EffectKind::SpawnTasks(spawn) = &effect.effect else {
        panic!("expected a task spawn");
    };
    assert_eq!(spawn.tasks.len(), 1, "only `collect` is ready");
    assert_eq!(spawn.tasks[0].node_id.as_str(), "collect");
    assert_eq!(spawn.tasks[0].task_id.as_str(), "wf-node0");
    assert!(
        !spawn.tasks[0].launch_token.as_str().is_empty(),
        "the launch token exists as a committed fact before the host launches anything"
    );

    // focus is the root controller, with no parent to restore
    assert_eq!(
        runtime.driver.focus(),
        Some(&ExecutionFocus::workflow_controller(
            runtime.driver.workflow_id().unwrap().clone(),
            None
        ))
    );
    assert!(
        !runtime.driver.focus().unwrap().is_nested_in_agent(),
        "the root workflow is not nested in an agent"
    );
}

#[test]
fn a_workflow_launch_preserves_logical_context_inheritance() {
    let mut spec = two_node_spec();
    spec.nodes[0].run_spec = Some(LogicalAgentSpec {
        context_inheritance: Some(WireContextInheritance::SystemOnly),
        ..LogicalAgentSpec::new("collect the sources")
    });

    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&workflow_start("in-start", 1_700_000_001_000, spec));

    let EffectKind::SpawnTasks(spawn) = &sole_effect(&started).effect else {
        panic!("expected a task spawn");
    };
    assert_eq!(
        spawn.tasks[0].spec.context_inheritance,
        Some(WireContextInheritance::SystemOnly),
    );
}

#[test]
fn a_workflow_root_with_no_nodes_has_no_first_effect_and_is_refused() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let fault = runtime.reject(&workflow_start(
        "in-start",
        1_700_000_001_000,
        WireSpec::default(),
    ));
    assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
    assert_eq!(runtime.driver.root_kind(), None);
}

#[test]
fn a_workflow_spec_whose_dependency_names_no_declared_node_is_refused() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let spec = WireSpec {
        name: "broken".to_string(),
        nodes: vec![wire_node("write", "write", &["collect"])],
    };
    let fault = runtime.reject(&workflow_start("in-start", 1_700_000_001_000, spec));
    assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
    assert_eq!(runtime.driver.root_kind(), None);
    assert_eq!(runtime.journal.len(), 1, "only the genesis record exists");
}

// -----------------------------------------------------------------------------------------
// fixture: workflow-root-completion-commits-terminal
// -----------------------------------------------------------------------------------------

/// The full §10.1 path: configure → root start → spawn → ack → completions → workflow terminal,
/// with no `LoadWorkflow`, no placeholder agent run and no host-privileged `CompleteRun`.
fn drive_workflow_root_to_terminal() -> Runtime {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));

    runtime.submit(&spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    let advanced = runtime.submit(&child_done(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "sources collected",
    ));
    let second = sole_effect(&advanced);
    assert_eq!(second.tag(), EffectKindTag::SpawnTasks);
    let EffectKind::SpawnTasks(spawn) = &second.effect else {
        panic!("expected a task spawn");
    };
    assert_eq!(spawn.tasks[0].node_id.as_str(), "write");

    runtime.submit(&spawned(
        "in-ack-2",
        1_700_000_004_000,
        &effect_id(advanced.step_seq),
        &["wf-node1"],
    ));
    runtime.submit(&child_done(
        "in-done-2",
        1_700_000_005_000,
        "wf-node1",
        "brief written",
    ));
    runtime
}

#[test]
fn a_root_workflow_completion_commits_the_workflow_terminal_itself() {
    let runtime = drive_workflow_root_to_terminal();

    let terminal = runtime.tx.terminal().expect("the run terminated");
    let KernelTerminal::Workflow(workflow) = terminal else {
        panic!("a workflow root terminates with a workflow terminal, got {terminal:?}");
    };
    assert_eq!(workflow.outcome.status, WorkflowStatus::Completed);
    assert_eq!(
        workflow
            .outcome
            .completed_nodes
            .iter()
            .map(|node| node.as_str())
            .collect::<Vec<_>>(),
        vec!["collect", "write"],
        "the terminal names the wire node ids the host declared"
    );
    assert!(workflow.outcome.failed_nodes.is_empty());
    assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Completed);
    assert_eq!(
        runtime.pending_effect_kinds(),
        Vec::<EffectKindTag>::new(),
        "a root workflow issues no provider call after it completes"
    );
}

#[test]
fn spc_019_10_restart_publishes_a_distinct_bounded_attempt() {
    use crate::scheduler::tcb::ChildFailurePolicy;

    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    runtime.submit(&spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    let root = runtime
        .driver
        .engine
        .as_mut()
        .unwrap()
        .task_table_mut()
        .get_mut("root")
        .unwrap();
    root.supervision.child_failure = ChildFailurePolicy::Restart;
    root.supervision.max_restarts = Some(1);
    let child = runtime
        .driver
        .engine
        .as_mut()
        .unwrap()
        .task_table_mut()
        .get_mut("wf-node0")
        .unwrap();
    child.budget.turns = 4;
    child.budget.total_tokens = 80;

    let failed = envelope(
        "in-failed-1",
        1_700_000_003_000,
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::ChildCompleted(ChildCompleted {
                task_id: TaskId::new("wf-node0").unwrap(),
                attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                result: ChildResult {
                    status: ChildStatus::Failed,
                    error: Some("worker crashed".to_string()),
                    ..ChildResult::default()
                },
                parent_requests: Vec::new(),
            }),
        }),
    );
    let restarted = runtime.submit(&failed);
    let EffectKind::SpawnTasks(spawn) = &sole_effect(&restarted).effect else {
        panic!("restart must publish an explicit spawn effect");
    };
    assert_eq!(spawn.tasks[0].attempt_id.as_str(), "wf-node0:attempt:2");
    let restart_effect = sole_effect(&restarted).effect_id.clone();
    let child = runtime
        .driver
        .engine
        .as_ref()
        .unwrap()
        .task_table()
        .get("wf-node0")
        .unwrap();
    assert_eq!((child.budget.turns, child.budget.total_tokens), (0, 0));
    assert_eq!(
        child.supervision_events[0].reason.as_str(),
        "worker crashed"
    );
    assert!(child.supervision_events[0].terminal);
    assert!(child.supervision_events[0].relaunched);
    assert_eq!(
        runtime
            .driver
            .engine
            .as_ref()
            .unwrap()
            .task_lifecycle("wf-node0"),
        Some(crate::scheduler::tcb::TaskLifecycle::Starting)
    );

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    let restored = Runtime::restore_with(Some(&checkpoint), &[]);
    assert_eq!(surface(&restored), surface(&runtime));

    runtime.submit(&spawned_attempt(
        "in-ack-2",
        1_700_000_004_000,
        &restart_effect,
        "wf-node0",
        2,
    ));
    let failed_again = envelope(
        "in-failed-2",
        1_700_000_005_000,
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::ChildCompleted(ChildCompleted {
                task_id: TaskId::new("wf-node0").unwrap(),
                attempt_id: WireAttemptId::new("wf-node0:attempt:2").unwrap(),
                result: ChildResult {
                    status: ChildStatus::Failed,
                    error: Some("crashed again".to_string()),
                    ..ChildResult::default()
                },
                parent_requests: Vec::new(),
            }),
        }),
    );
    let terminal = runtime.submit(&failed_again);
    assert!(matches!(
        terminal.step.disposition,
        StepDisposition::Terminal(_)
    ));
    let events = &runtime
        .driver
        .engine
        .as_ref()
        .unwrap()
        .task_table()
        .get("wf-node0")
        .unwrap()
        .supervision_events;
    assert_eq!(events.len(), 2);
    assert!(!events[1].relaunched, "the explicit limit stops attempt 3");
}

#[test]
fn spc_019_10_retry_preserves_usage_while_ignore_accepts_the_terminal_attempt() {
    use crate::scheduler::tcb::ChildFailurePolicy;

    let mut retry = Runtime::new();
    retry.submit(&syscall_config());
    let started = retry.submit(&workflow_start(
        "retry-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    retry.submit(&spawned(
        "retry-ack",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    {
        let table = retry.driver.engine.as_mut().unwrap().task_table_mut();
        table.get_mut("root").unwrap().supervision = crate::scheduler::tcb::SupervisionPolicy {
            child_failure: ChildFailurePolicy::Retry,
            max_restarts: Some(1),
            cancel_children_on_exit: true,
        };
        table.get_mut("wf-node0").unwrap().budget.turns = 3;
        table.get_mut("wf-node0").unwrap().budget.total_tokens = 55;
    }
    let retried = retry.submit(&child_failed(
        "retry-failed",
        1_700_000_003_000,
        "wf-node0",
        1,
        "transient",
    ));
    let EffectKind::SpawnTasks(spawn) = &sole_effect(&retried).effect else {
        panic!("retry must be an explicit spawn");
    };
    assert_eq!(spawn.tasks[0].attempt_id.as_str(), "wf-node0:attempt:2");
    let retry_child = retry
        .driver
        .engine
        .as_ref()
        .unwrap()
        .task_table()
        .get("wf-node0")
        .unwrap();
    assert_eq!(
        (retry_child.budget.turns, retry_child.budget.total_tokens),
        (3, 55),
        "retry preserves logical-task usage"
    );

    let mut ignore = Runtime::new();
    ignore.submit(&syscall_config());
    let started = ignore.submit(&workflow_start(
        "ignore-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    ignore.submit(&spawned(
        "ignore-ack",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    ignore
        .driver
        .engine
        .as_mut()
        .unwrap()
        .task_table_mut()
        .get_mut("root")
        .unwrap()
        .supervision
        .child_failure = ChildFailurePolicy::Ignore;
    let advanced = ignore.submit(&child_failed(
        "ignore-failed",
        1_700_000_003_000,
        "wf-node0",
        1,
        "non-critical",
    ));
    let EffectKind::SpawnTasks(spawn) = &sole_effect(&advanced).effect else {
        panic!("ignored failure should advance the dependent node");
    };
    assert_eq!(spawn.tasks[0].task_id.as_str(), "wf-node1");
    let event = &ignore
        .driver
        .engine
        .as_ref()
        .unwrap()
        .task_table()
        .get("wf-node0")
        .unwrap()
        .supervision_events[0];
    assert_eq!(event.strategy, ChildFailurePolicy::Ignore);
    assert!(event.terminal);
    assert!(!event.relaunched);
}

#[test]
fn spc_019_10_propagate_and_isolate_keep_distinct_terminal_audit_strategies() {
    use crate::scheduler::tcb::ChildFailurePolicy;

    for (index, strategy) in [ChildFailurePolicy::Propagate, ChildFailurePolicy::Isolate]
        .into_iter()
        .enumerate()
    {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&workflow_start(
            &format!("terminal-start-{index}"),
            1_700_000_001_000,
            two_node_spec(),
        ));
        runtime.submit(&spawned(
            &format!("terminal-ack-{index}"),
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        runtime
            .driver
            .engine
            .as_mut()
            .unwrap()
            .task_table_mut()
            .get_mut("root")
            .unwrap()
            .supervision
            .child_failure = strategy;
        let terminal = runtime.submit(&child_failed(
            &format!("terminal-failed-{index}"),
            1_700_000_003_000,
            "wf-node0",
            1,
            "terminal failure",
        ));
        assert!(matches!(
            terminal.step.disposition,
            StepDisposition::Terminal(_)
        ));
        let event = &runtime
            .driver
            .engine
            .as_ref()
            .unwrap()
            .task_table()
            .get("wf-node0")
            .unwrap()
            .supervision_events[0];
        assert_eq!(event.strategy, strategy);
        assert!(event.terminal);
        assert!(!event.relaunched);
    }
}

#[test]
fn a_completion_naming_an_attempt_the_kernel_never_minted_is_refused() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    runtime.submit(&spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));

    let forged = envelope(
        "in-forged",
        1_700_000_003_000,
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::ChildCompleted(ChildCompleted {
                task_id: TaskId::new("wf-node0").unwrap(),
                attempt_id: WireAttemptId::new("wf-node0:attempt:7").unwrap(),
                result: ChildResult::default(),
                parent_requests: Vec::new(),
            }),
        }),
    );
    let before = runtime.pending_effect_kinds();
    let fault = runtime.reject(&forged);
    assert_eq!(
        fault.code,
        KernelFaultCode::InvalidAuthority,
        "a host does not mint or rewrite child identity (§10.4)"
    );
    assert_eq!(runtime.pending_effect_kinds(), before);
    assert!(runtime.tx.terminal().is_none());
}

#[test]
fn a_terminated_root_workflow_refuses_every_later_state_changing_input() {
    let mut runtime = drive_workflow_root_to_terminal();
    let fault = runtime.reject(&child_done(
        "in-done-late",
        1_700_000_006_000,
        "wf-node1",
        "again",
    ));
    assert_eq!(fault.code, KernelFaultCode::InvalidLifecycle);
}

// -----------------------------------------------------------------------------------------
// fixture: workflow-no-stack-and-root-kind-immutable
// -----------------------------------------------------------------------------------------

/// Free the pending provider effect so the nested-workflow arc has a clean `call_provider`
/// slot. The real provider reduction has its own tests below; this planner only clears the
/// registration so the focus assertions read against a quiet step.
fn provider_settled(
    driver: &mut CanonicalOperationDriver,
    _context: &PlanContext<'_>,
) -> Result<PlannedStep, KernelFault> {
    Ok(PlannedStep::quiet(
        driver.root_kind(),
        driver.focus().cloned(),
    ))
}

fn agent_root_with_nested_workflow() -> (Runtime, TaskId) {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    // settle the provider call the agent root issued
    let settle = envelope(
        "in-provider",
        1_700_000_002_000,
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: effect_id(started.step_seq),
            outcome: EffectOutcome::Succeeded(EffectSucceeded {
                result: EffectSuccess::Provider(super::super::effect::ProviderSuccess {
                    outcome: super::super::effect::ProviderOutcome::ContextOverflow(
                        super::super::effect::ProviderContextOverflow::default(),
                    ),
                }),
            }),
        }),
    );
    runtime.submit_planned(&settle, provider_settled);

    let spec = two_node_spec();
    let carrier = syscall_carrier("in-submit-workflow", 1_700_000_003_000);
    let entered = runtime.submit_planned(&carrier, |driver, context| {
        driver.begin_nested_workflow(context, &spec)
    });
    runtime
        .driver
        .note_committed(entered.step_seq)
        .expect("the nested start folds like any other planned step");
    assert_eq!(sole_effect(&entered).tag(), EffectKindTag::SpawnTasks);
    (runtime, root_task_id())
}

/// The historical bootstrap replaced a live provider registration with a workflow spawn, so a
/// pending provider call could disappear without ever being resolved. Here the two effects are
/// separate registrations and neither evicts the other.
#[test]
fn starting_a_workflow_never_overwrites_a_live_provider_effect() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let provider_effect = sole_effect(&started).effect_id.clone();

    let spec = two_node_spec();
    let carrier = syscall_carrier("in-submit-workflow", 1_700_000_002_000);
    let entered = runtime.submit_planned(&carrier, |driver, context| {
        driver.begin_nested_workflow(context, &spec)
    });
    runtime.driver.note_committed(entered.step_seq).unwrap();

    let mut pending = runtime.pending_effect_kinds();
    pending.sort();
    assert_eq!(
        pending,
        vec![EffectKindTag::CallProvider, EffectKindTag::SpawnTasks],
        "the provider call is still outstanding; the spawn is a second registration"
    );
    assert!(
        runtime
            .tx
            .pending_effects()
            .any(|effect| effect.effect_id == provider_effect),
        "the provider effect kept its identity, so the host can still resolve it"
    );
}

#[test]
fn an_agent_authored_workflow_moves_the_focus_but_never_the_root_kind() {
    let (runtime, parent) = agent_root_with_nested_workflow();

    assert_eq!(
        runtime.driver.root_kind(),
        Some(RootKind::Agent),
        "a syscall never re-roots an operation (§10.2)"
    );
    let focus = runtime.driver.focus().expect("a focus exists");
    assert!(
        focus.is_nested_in_agent(),
        "the focus records the parent agent task it must restore"
    );
    assert_eq!(
        focus,
        &ExecutionFocus::workflow_controller(
            runtime.driver.workflow_id().unwrap().clone(),
            Some(parent),
        )
    );
}

#[test]
fn a_second_workflow_inside_a_workflow_focus_is_an_authority_refusal_with_no_spawn() {
    let (mut runtime, _) = agent_root_with_nested_workflow();

    let before = (
        runtime.tx.head(),
        runtime.pending_effect_kinds(),
        runtime.driver.root_kind(),
        runtime.driver.focus().cloned(),
    );

    let spec = two_node_spec();
    let carrier = syscall_carrier("in-submit-again", 1_700_000_004_000);
    let preparation = {
        let Runtime { tx, driver, .. } = &mut runtime;
        tx.prepare(&carrier, |context| {
            driver.begin_nested_workflow(context, &spec)
        })
    };
    let fault = preparation.fault().expect("expected a refusal").clone();
    assert_eq!(
        fault.code,
        KernelFaultCode::InvalidAuthority,
        "workflows do not stack — depth is at most 1 (§7.4)"
    );

    let after = (
        runtime.tx.head(),
        runtime.pending_effect_kinds(),
        runtime.driver.root_kind(),
        runtime.driver.focus().cloned(),
    );
    assert_eq!(before, after, "the refusal produced no derived action");
}

#[test]
fn a_nested_workflow_completion_restores_the_parent_agent_and_resumes_its_turn() {
    let (mut runtime, parent) = agent_root_with_nested_workflow();
    let spawn_step = runtime.journal.last().unwrap().step_seq();

    runtime.submit(&spawned(
        "in-ack-1",
        1_700_000_004_000,
        &effect_id(spawn_step),
        &["wf-node0"],
    ));
    let advanced = runtime.submit(&child_done(
        "in-done-1",
        1_700_000_005_000,
        "wf-node0",
        "sources collected",
    ));
    runtime.submit(&spawned(
        "in-ack-2",
        1_700_000_006_000,
        &effect_id(advanced.step_seq),
        &["wf-node1"],
    ));
    let finished = runtime.submit(&child_done(
        "in-done-2",
        1_700_000_007_000,
        "wf-node1",
        "brief written",
    ));

    assert!(
        runtime.tx.terminal().is_none(),
        "a nested workflow's completion is not the operation's terminal (§6.1.7)"
    );
    assert_eq!(
        runtime.driver.focus(),
        Some(&ExecutionFocus::agent_turn(parent)),
        "the focus returns to the agent turn it left"
    );
    assert_eq!(
        sole_effect(&finished).tag(),
        EffectKindTag::CallProvider,
        "the parent agent's turn resumes with a provider call"
    );
    assert_eq!(runtime.driver.root_kind(), Some(RootKind::Agent));
}

#[test]
fn a_workflow_root_admits_no_nested_workflow_at_all() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));

    let spec = two_node_spec();
    let carrier = syscall_carrier("in-submit", 1_700_000_002_000);
    let preparation = {
        let Runtime { tx, driver, .. } = &mut runtime;
        tx.prepare(&carrier, |context| {
            driver.begin_nested_workflow(context, &spec)
        })
    };
    assert_eq!(
        preparation.fault().unwrap().code,
        KernelFaultCode::InvalidAuthority
    );
}

#[test]
fn a_workflow_roots_focus_never_moves_while_its_dag_runs() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    let focus = runtime.driver.focus().cloned();

    runtime.submit(&spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    assert_eq!(
        runtime.driver.focus().cloned(),
        focus,
        "an ack moves nothing"
    );

    let advanced = runtime.submit(&child_done(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "done",
    ));
    assert_eq!(
        runtime.driver.focus().cloned(),
        focus,
        "a DAG node's agent execution is a child attempt, not a focus change"
    );
    assert_eq!(sole_effect(&advanced).tag(), EffectKindTag::SpawnTasks);
}

// -----------------------------------------------------------------------------------------
// fixture: agent-syscall-caller-is-derived
// -----------------------------------------------------------------------------------------

/// Configure → agent root → provider result carrying one meta-tool call. Returns the runtime
/// and the provider effect the start published.
fn agent_awaiting_provider() -> (Runtime, EffectId) {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let effect = sole_effect(&started);
    assert_eq!(effect.tag(), EffectKindTag::CallProvider);
    let EffectKind::CallProvider(call) = &effect.effect else {
        panic!("expected a provider call");
    };
    assert!(
        call.tools.iter().any(|tool| tool.name == "start_workflow"),
        "the turn must actually expose the meta-tool the model is about to call"
    );
    (runtime, effect.effect_id.clone())
}

#[test]
fn a_provider_tool_call_derives_its_caller_and_enters_p1() {
    let (mut runtime, provider) = agent_awaiting_provider();

    let spec = serde_json::to_value(two_node_spec()).unwrap();
    let entered = runtime.submit(&provider_result(
        "in-authored",
        1_700_000_002_000,
        &provider,
        vec![tool_call("call-1", "start_workflow", spec)],
    ));

    // the request became a real transition: a DAG was bootstrapped and its first node launched
    let effect = sole_effect(&entered);
    assert_eq!(effect.tag(), EffectKindTag::SpawnTasks);
    let EffectKind::SpawnTasks(spawn) = &effect.effect else {
        panic!("expected a task spawn");
    };
    for task in &spawn.tasks {
        assert_eq!(
            runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(task.task_id.as_str())
                .and_then(|tcb| tcb.parent.as_ref())
                .map(|parent| parent.as_str()),
            Some(ROOT_TASK_ID),
            "the provider-tool causation projects the suspended agent turn as caller"
        );
    }
    assert_eq!(
        runtime.driver.root_kind(),
        Some(RootKind::Agent),
        "a syscall never re-roots an operation (§10.2)"
    );
    assert!(
        runtime.driver.focus().unwrap().is_nested_in_agent(),
        "the focus moved to the workflow controller, under the agent turn it suspended"
    );

    // and the host declared nobody: the accepted input carries no caller field at all
    let stored = entered.record.normalized_input().unwrap();
    let json = serde_json::to_value(&stored).unwrap();
    let text = json.to_string();
    for forbidden in ["submitter_agent_id", "actor_id", "parent_session_id"] {
        assert!(
            !text.contains(forbidden),
            "the canonical input still carries {forbidden}"
        );
    }
}

#[test]
fn a_successful_effect_resolution_notifies_the_durable_effect_wait() {
    use crate::scheduler::tcb::{WaitCondition, WaitMode, WaitSet};

    let (mut runtime, provider) = agent_awaiting_provider();
    runtime
        .driver
        .engine_mut()
        .unwrap()
        .task_table_mut()
        .register_wait_set(
            ROOT_TASK_ID,
            WaitSet {
                mode: WaitMode::Any,
                conditions: vec![WaitCondition::Effect(provider.clone())],
            },
        );

    runtime.submit(&provider_answer(
        "in-effect-wake",
        1_700_000_002_000,
        &provider,
        "done",
    ));
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(ROOT_TASK_ID)
            .unwrap()
            .wait_set
            .is_none(),
        "the ResolveEffect transition is the sole effect-wait producer"
    );
}

#[test]
fn a_tool_the_turn_never_exposed_has_no_caller_to_derive() {
    let (mut runtime, provider) = agent_awaiting_provider();

    // An unexposed *task* tool is not an authority claim — it is a call the model made up, and
    // the kernel's fail-closed dispatch gate answers it the way the model is trained to read:
    // a visible error result, no host dispatch, and the turn continues.
    let committed = runtime.submit(&provider_result(
        "in-forged",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-1",
            "escalate_privileges",
            json!({"nodes": []}),
        )],
    ));
    assert_eq!(
        committed
            .published_effects()
            .iter()
            .map(|effect| effect.tag())
            .collect::<Vec<_>>(),
        vec![EffectKindTag::CallProvider],
        "a phantom tool is never dispatched; the turn is answered and re-asked"
    );

    // The authority half: a *syscall* name the turn did not advertise. Narrow the recorded
    // surface to nothing and ask again — the request is well-formed and still has no caller.
    let next = effect_id(committed.step_seq);
    runtime
        .driver
        .provider_calls
        .get_mut(&next)
        .expect("the driver recorded the turn it published")
        .exposed_tools
        .clear();
    let before = (
        runtime.tx.head(),
        runtime.pending_effect_kinds(),
        runtime.driver.focus().cloned(),
    );
    let fault = runtime
        .driver
        .derive_provider_syscalls(&next, &[tool_call("call-2", "start_workflow", json!({}))])
        .expect_err("a tool the turn never exposed has no causation");
    assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
    assert!(fault.message.contains("exposed no tool"));

    let after = (
        runtime.tx.head(),
        runtime.pending_effect_kinds(),
        runtime.driver.focus().cloned(),
    );
    assert_eq!(before, after, "a forged causation moves nothing");
}

#[test]
fn a_provider_effect_this_kernel_never_published_has_no_causation() {
    let (runtime, _) = agent_awaiting_provider();
    let unknown = EffectId::new("op-driver-1:step:99:effect:0").unwrap();
    let fault = runtime
        .driver
        .derive_provider_syscalls(
            &unknown,
            &[tool_call("call-1", "start_workflow", json!({}))],
        )
        .expect_err("an unpublished effect names no turn");
    assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
    assert!(fault.message.contains("not a provider call"));
}

#[test]
fn a_call_id_that_already_produced_a_syscall_cannot_produce_a_second() {
    let (mut runtime, provider) = agent_awaiting_provider();

    // two calls sharing one id inside a single result: the second has no causation left
    let fault = runtime.reject(&provider_result(
        "in-double",
        1_700_000_002_000,
        &provider,
        vec![
            tool_call("call-1", "skill", json!({"name": "debug"})),
            tool_call("call-1", "skill", json!({"name": "debug"})),
        ],
    ));
    assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
    assert!(fault.message.contains("consumed once"));

    // and a causation that *was* spent stays spent for the operation's lifetime
    runtime.submit(&provider_result(
        "in-skill",
        1_700_000_002_000,
        &provider,
        vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
    ));
    assert!(
        runtime.driver.consumed_calls.contains("call-1"),
        "the spent causation is remembered, so a redelivery under a fresh input id buys nothing"
    );
    assert!(
        !runtime.driver.provider_calls.contains_key(&provider),
        "a resolved provider call is no longer a surface anything can be attributed to"
    );
}

#[test]
fn a_skill_the_operation_never_declared_cannot_be_activated() {
    let (mut runtime, provider) = agent_awaiting_provider();
    runtime.submit(&provider_result(
        "in-skill",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-1",
            "skill",
            json!({"name": "not-declared"}),
        )],
    ));
    let rejected = rejections(&runtime);
    assert_eq!(rejected.len(), 1, "the refusal is an audit fact");
    assert_eq!(rejected[0].0, "skill");
    assert_eq!(
        rejected[0].1.as_deref(),
        Some(ROOT_TASK_ID),
        "the audit fact names the caller the kernel derived, not one a host supplied"
    );
    assert!(rejected[0].2.contains("declares no skill"));
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::CallProvider],
        "a rejected capability mutation publishes no effect of its own; the turn still \
             continues (the §5k syscall-only continuation)"
    );
}

#[test]
fn a_skill_without_capability_grants_keeps_name_only_activation_semantics() {
    let (mut runtime, provider) = agent_awaiting_provider();
    runtime.submit(&provider_result(
        "in-skill",
        1_700_000_002_000,
        &provider,
        vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
    ));

    let engine = runtime.driver.engine().unwrap();
    assert!(engine.ctx.active_skills.contains_key("debug"));
    assert!(engine.ctx.active_skill_capabilities().is_empty());
}

#[test]
fn an_append_with_no_graph_to_append_to_is_an_audit_fact_not_a_derived_action() {
    let (mut runtime, provider) = agent_awaiting_provider();
    runtime.submit(&provider_result(
        "in-append",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-1",
            "submit_workflow_nodes",
            node_args(&[wire_node("stray", "stray", &[])]),
        )],
    ));

    let rejected = rejections(&runtime);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, "submit_workflow_nodes");
    assert_eq!(
        rejected[0].1.as_deref(),
        Some(ROOT_TASK_ID),
        "a provider-tool causation names the task whose turn issued the call"
    );
    assert!(rejected[0].2.contains("no workflow is in flight"));
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::CallProvider],
        "a refused append spawns nothing; the turn continues with the next provider call"
    );
}

// -----------------------------------------------------------------------------------------
// fixture: workflow-dynamic-append-preserves-authority (+ §7.7 GAP-4)
// -----------------------------------------------------------------------------------------

/// Workflow root, first node launched and acknowledged.
fn workflow_root_awaiting_first_child() -> Runtime {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    runtime.submit(&spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    runtime
}

#[test]
fn parent_requests_are_adjudicated_independently_and_never_undo_the_completion() {
    let mut runtime = workflow_root_awaiting_first_child();
    assert_eq!(
        runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(ROOT_TASK_ID)
            .unwrap()
            .wait_set
            .as_ref()
            .unwrap()
            .conditions,
        vec![WaitCondition::Child("wf-node0".into())],
        "workflow join is a durable child wait before the completion arrives"
    );

    let good =
        SyscallRequest::AppendWorkflowNodes(super::super::syscall::AppendWorkflowNodesRequest {
            nodes: vec![wire_node("verify", "verify the sources", &[])],
        });
    // batch-relative dependency that names nothing in its own batch — refused on its own merits
    let bad =
        SyscallRequest::AppendWorkflowNodes(super::super::syscall::AppendWorkflowNodesRequest {
            nodes: vec![wire_node("orphan", "orphan", &["nowhere"])],
        });
    let another_good = SyscallRequest::UpdateTask(super::super::syscall::UpdateTaskRequest {
        update: WireTaskUpdate {
            progress: Some("sources collected".to_string()),
            ..WireTaskUpdate::default()
        },
    });

    let advanced = runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![good, bad, another_good],
    ));

    // the completion committed unconditionally and drained into the next ready batch
    assert_eq!(
        runtime.tx.lifecycle(),
        OperationLifecycle::Running,
        "a denied parent request does not undo the child's execution (GAP-4)"
    );
    let effect = sole_effect(&advanced);
    assert_eq!(effect.tag(), EffectKindTag::SpawnTasks);
    let EffectKind::SpawnTasks(spawn) = &effect.effect else {
        panic!("expected a task spawn");
    };
    assert!(
        !runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .wait_index()
            .lookup(&WaitKey::Child("wf-node0".into()))
            .contains(&crate::scheduler::tcb::TaskId::from(ROOT_TASK_ID)),
        "ChildCompleted consumed the completed child's wait before installing the next join"
    );
    let launched: Vec<&str> = spawn
        .tasks
        .iter()
        .map(|task| task.node_id.as_str())
        .collect();
    assert!(
        launched.contains(&"write") && launched.contains(&"verify"),
        "the admitted append reached the next ready batch alongside the original DAG, got \
             {launched:?}"
    );
    assert!(
        !launched.contains(&"orphan"),
        "the refused append produced no derived action"
    );

    // The next runnable batch was released by `wf-node0`'s committed child attempt. Its
    // caller is therefore that attempt, not the workflow table's structural root.
    for task in &spawn.tasks {
        let parent = runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(task.task_id.as_str())
            .and_then(|tcb| tcb.parent.as_ref())
            .map(|parent| parent.as_str());
        assert_eq!(
            parent,
            Some("wf-node0"),
            "{} must retain the child-attempt caller that caused its launch",
            task.task_id
        );
    }

    // exactly one structured rejection, and the third request was unaffected by the second
    let rejected = rejections(&runtime);
    assert_eq!(rejected.len(), 1, "each request is adjudicated on its own");
    assert_eq!(rejected[0].0, "submit_workflow_nodes");
    assert_eq!(
        rejected[0].1.as_deref(),
        Some("wf-node0"),
        "the refusal names the child attempt it was derived from"
    );
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .partitions
            .task_state
            .progress
            .contains("sources collected"),
        "a sibling's refusal does not stop the requests after it"
    );

    let replay = Runtime::restore_with(None, &runtime.journal);
    for task in &spawn.tasks {
        let live_parent = runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(task.task_id.as_str())
            .and_then(|tcb| tcb.parent.clone());
        let replay_parent = replay
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(task.task_id.as_str())
            .and_then(|tcb| tcb.parent.clone());
        assert_eq!(
            replay_parent, live_parent,
            "replay preserves caller lineage"
        );
        assert_eq!(
            replay.driver.attempt_id(task.task_id.as_str()),
            runtime.driver.attempt_id(task.task_id.as_str()),
            "replay preserves the kernel-minted child attempt"
        );
    }
}

#[test]
fn spc_019_08_child_parent_requests_route_handles_through_durable_local_ipc() {
    use crate::scheduler::tcb::ChannelId;

    let mut runtime = workflow_root_awaiting_first_child();
    runtime
        .driver
        .engine_mut()
        .unwrap()
        .ctx
        .handles
        .insert(Handle::resident_for(
            77,
            HandleKind::ToolResult,
            1,
            "ipc-handle",
        ));
    let send = SyscallRequest::SendMessage(super::super::syscall::SendMessageRequest {
        message_id: "message-1".to_string(),
        to: TaskId::new(ROOT_TASK_ID).unwrap(),
        message_kind: "child_result".to_string(),
        payload_handle: HandleId::new("ipc-handle").unwrap(),
        ttl_turns: Some(4),
    });
    let publish = SyscallRequest::PublishChannel(super::super::syscall::PublishChannelRequest {
        channel_id: "results".to_string(),
        message_id: "channel-message-1".to_string(),
        subscribers: vec![TaskId::new(ROOT_TASK_ID).unwrap()],
        message_kind: "child_result".to_string(),
        payload_handle: HandleId::new("ipc-handle").unwrap(),
        ttl_turns: Some(4),
    });
    runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![send.clone(), send, publish],
    ));

    let table = runtime.driver.engine_mut().unwrap().task_table_mut();
    let messages = table
        .receive_mailbox(ROOT_TASK_ID, crate::scheduler::mailbox::LogicalTime(0), 8)
        .unwrap();
    assert_eq!(messages.len(), 1, "duplicate message id is enqueued once");
    assert_eq!(messages[0].from.as_str(), "wf-node0");
    assert_eq!(messages[0].payload_handle, 77);
    assert_eq!(
        table
            .receive_channel(
                ROOT_TASK_ID,
                &ChannelId("results".into()),
                crate::scheduler::mailbox::LogicalTime(0),
            )
            .unwrap()
            .len(),
        1
    );

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    let restored = Runtime::restore_with(Some(&checkpoint), &[]);
    assert_eq!(surface(&restored), surface(&runtime));
}

#[test]
fn an_append_beyond_the_workflow_node_quota_is_denied_without_touching_the_graph() {
    let mut runtime = workflow_root_awaiting_first_child();
    let nodes_before = runtime.driver.engine().unwrap().workflow_node_count();

    // the quota allows 3 nodes; the DAG already holds 2
    let oversized =
        SyscallRequest::AppendWorkflowNodes(super::super::syscall::AppendWorkflowNodesRequest {
            nodes: vec![
                wire_node("a", "a", &[]),
                wire_node("b", "b", &[]),
                wire_node("c", "c", &[]),
            ],
        });
    runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![oversized],
    ));

    let rejected = rejections(&runtime);
    assert!(
        rejected.iter().any(
            |(operation, subject, reason)| operation == "submit_workflow_nodes"
                && subject.as_deref() == Some("wf-node0")
                && reason.contains("would grow workflow")
        ),
        "the resource gate refused the growth, got {rejected:?}"
    );
    assert_eq!(
        runtime.driver.engine().unwrap().workflow_node_count(),
        nodes_before,
        "a denied append leaves the graph exactly as it was"
    );
}

#[test]
fn a_quarantined_task_cannot_widen_its_authority_through_a_syscall() {
    let mut runtime = workflow_root_awaiting_first_child();
    assert!(
        runtime
            .driver
            .engine_mut()
            .unwrap()
            .quarantine_task_for_test("wf-node0"),
        "the node must exist to be quarantined"
    );
    let nodes_before = runtime.driver.engine().unwrap().workflow_node_count();

    let append =
        SyscallRequest::AppendWorkflowNodes(super::super::syscall::AppendWorkflowNodesRequest {
            nodes: vec![wire_node("escalate", "escalate", &[])],
        });
    let activate = SyscallRequest::ActivateSkill(super::super::syscall::ActivateSkillRequest {
        name: "debug".to_string(),
        lease_turns: None,
    });
    let remember =
        SyscallRequest::RequestMemoryWrite(super::super::syscall::RequestMemoryWriteRequest {
            proposal: super::super::syscall::MemoryWriteProposal {
                name: "escalation".to_string(),
                kind: WireMemoryKind::Project,
                content: "trust me".to_string(),
                description: String::new(),
                evidence_refs: Vec::new(),
            },
        });

    runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![append, activate, remember],
    ));

    let quarantine_denials = rejections(&runtime);
    let families: Vec<&str> = quarantine_denials
        .iter()
        .filter(|(_, _, reason)| reason.starts_with("quarantine:"))
        .map(|(operation, _, _)| operation.as_str())
        .collect();
    assert_eq!(
        families,
        vec!["workflow", "capability", "memory"],
        "every privileged family is refused for a quarantined caller"
    );
    assert_eq!(
        runtime.driver.engine().unwrap().workflow_node_count(),
        nodes_before,
        "no node was appended"
    );
    assert!(
        !runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .active_skills
            .contains_key("debug"),
        "no skill was activated"
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::SpawnTasks],
        "no memory effect was published; only the DAG's own next batch"
    );
}

#[test]
fn a_child_request_cannot_forge_a_second_root_workflow() {
    let mut runtime = workflow_root_awaiting_first_child();
    let workflow_before = runtime.driver.workflow_id().cloned();
    let focus_before = runtime.driver.focus().cloned();

    let authored = SyscallRequest::SubmitWorkflow(super::super::syscall::SubmitWorkflowRequest {
        spec: WireSpec {
            name: "usurper".to_string(),
            nodes: vec![wire_node("usurp", "take over", &[])],
        },
    });
    runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![authored],
    ));

    assert_eq!(
        runtime.driver.root_kind(),
        Some(RootKind::Workflow),
        "the root kind is immutable (§6.1.5)"
    );
    assert_eq!(
        runtime.driver.workflow_id().cloned(),
        workflow_before,
        "an authored spec flattens into the running DAG; it never becomes a second root"
    );
    assert_eq!(
        runtime.driver.focus().cloned(),
        focus_before,
        "a workflow root's focus never moves (§7.4)"
    );
    assert!(
        rejections(&runtime).is_empty(),
        "flattening is the admitted path, not a refusal"
    );
}

// -----------------------------------------------------------------------------------------
// fixture: workflow-child-identity-is-kernel-issued (TCB launch arc)
// -----------------------------------------------------------------------------------------

#[test]
fn a_child_moves_pending_launch_then_starting_then_running_on_the_acknowledgement() {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());

    // the spawn effect is planned but not yet committed: the identity exists, the launch does not
    let preparation = runtime.prepare(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    assert_eq!(
        runtime.driver.engine().unwrap().task_lifecycle("wf-node0"),
        Some(TaskLifecycle::Starting),
        "the launch effect is planned, so the task left PendingLaunch and awaits the host"
    );
    let started = runtime.append_and_commit(preparation);
    runtime.driver.note_committed(started.step_seq).unwrap();
    assert_eq!(
        runtime.driver.engine().unwrap().task_lifecycle("wf-node0"),
        Some(TaskLifecycle::Starting),
        "a published launch is not a running task"
    );

    runtime.submit(&spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    assert_eq!(
        runtime.driver.engine().unwrap().task_lifecycle("wf-node0"),
        Some(TaskLifecycle::Running),
        "only the acknowledgement makes a task Running (§10.4, §15.3)"
    );
}

/// The launch arc at its own layer, where all three states are separately observable. Through
/// the driver `PendingLaunch` and `Starting` both happen inside one `plan` call, because minting
/// identity and building the launch effect are two moments of the same transition.
#[test]
fn an_ack_gated_spawn_mints_identity_in_pending_launch_before_the_effect_is_published() {
    let mut engine = LoopStateMachine::new(SchedulerBudget::default());
    let action = engine.load_workflow(
        build_core_spec(&WireSpec {
            name: String::new(),
            nodes: vec![wire_node("only", "only", &[])],
        })
        .unwrap(),
    );
    assert!(matches!(action, LoopAction::SpawnWorkflow { .. }));
    assert_eq!(
        engine.task_lifecycle("wf-node0"),
        Some(TaskLifecycle::PendingLaunch),
        "identity is minted, the launch is not published yet"
    );

    engine.mark_tasks_starting(&["wf-node0".to_string()]);
    assert_eq!(
        engine.task_lifecycle("wf-node0"),
        Some(TaskLifecycle::Starting),
        "the launch effect is published; the host has not answered"
    );

    engine.resolve_workflow_spawn(vec!["wf-node0".to_string()], Vec::new());
    assert_eq!(
        engine.task_lifecycle("wf-node0"),
        Some(TaskLifecycle::Running),
        "only the acknowledgement makes it Running"
    );
}

#[test]
fn a_failed_launch_ends_the_attempt_and_refuses_any_later_completion_for_it() {
    use crate::runtime::kernel::wire::effect::{TaskLaunchFailed, TaskLaunchStatus};

    // two independent nodes, so failing one leaves the DAG running rather than terminating it
    let parallel = WireSpec {
        name: "parallel".to_string(),
        nodes: vec![
            wire_node("left", "left", &[]),
            wire_node("right", "right", &[]),
        ],
    };
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&workflow_start("in-start", 1_700_000_001_000, parallel));

    let failure = envelope(
            "in-ack-fail",
            1_700_000_002_000,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect_id(started.step_seq),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::TasksSpawned(TasksSpawnedSuccess {
                        attempts: vec![
                            TaskLaunchOutcome {
                                task_id: TaskId::new("wf-node0").unwrap(),
                                attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                                outcome: TaskLaunchStatus::Failed(TaskLaunchFailed {
                                    failure: super::super::effect::TaskLaunchFailure {
                                        kind:
                                            super::super::effect::HostEffectFailureKind::StorageUnavailable,
                                        message: "no worker".to_string(),
                                    },
                                }),
                            },
                            TaskLaunchOutcome {
                                task_id: TaskId::new("wf-node1").unwrap(),
                                attempt_id: WireAttemptId::new("wf-node1:attempt:1").unwrap(),
                                outcome: TaskLaunchStatus::Started(TaskLaunchStarted {}),
                            },
                        ],
                    }),
                }),
            }),
        );
    runtime.submit(&failure);
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .task_lifecycle("wf-node0")
            .is_some_and(|state| state.is_terminal()),
        "a failed launch terminates the attempt"
    );

    let fault = runtime.reject(&child_done_with(
        "in-late",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        Vec::new(),
    ));
    assert_eq!(
        fault.code,
        KernelFaultCode::InvalidAuthority,
        "a terminated attempt is a stale causation"
    );
}

#[test]
fn a_second_completion_for_a_spent_attempt_carries_no_authority() {
    let mut runtime = workflow_root_awaiting_first_child();
    runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        Vec::new(),
    ));
    let nodes_before = runtime.driver.engine().unwrap().workflow_node_count();

    let replayed = child_done_with(
        "in-done-1-again",
        1_700_000_004_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![SyscallRequest::AppendWorkflowNodes(
            super::super::syscall::AppendWorkflowNodesRequest {
                nodes: vec![wire_node("smuggled", "smuggled", &[])],
            },
        )],
    );
    let fault = runtime.reject(&replayed);
    assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
    assert_eq!(
        runtime.driver.engine().unwrap().workflow_node_count(),
        nodes_before,
        "a refused completion appends nothing"
    );
}

// -----------------------------------------------------------------------------------------
// §7.6 · memory proposals
// -----------------------------------------------------------------------------------------

#[test]
fn a_memory_proposal_becomes_a_kernel_authored_write_with_derived_provenance() {
    let mut runtime = workflow_root_awaiting_first_child();
    let write =
        SyscallRequest::RequestMemoryWrite(super::super::syscall::RequestMemoryWriteRequest {
            proposal: super::super::syscall::MemoryWriteProposal {
                name: "source-set".to_string(),
                kind: WireMemoryKind::Project,
                content: "12 primary sources".to_string(),
                description: String::new(),
                evidence_refs: Vec::new(),
            },
        });
    let advanced = runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![write],
    ));

    let persisted = advanced
        .published_effects()
        .iter()
        .find(|effect| effect.tag() == EffectKindTag::PersistMemory)
        .expect("the proposal published a memory write");
    assert_eq!(
        persisted.effect_id,
        effect_id_at(advanced.step_seq, 0),
        "syscall effects mint their own identity from the step they belong to"
    );
    let EffectKind::PersistMemory(effect) = &persisted.effect else {
        panic!("expected a memory write");
    };
    assert_eq!(effect.binding.binding_id.as_str(), "mem-binding-1");
    assert_eq!(
        effect.memory.accepted_at_ms,
        WireU64::new(1_700_000_003_000),
        "provenance time is the envelope's accepted time, never a host clock (DEC-2)"
    );
    match &effect.memory.causation {
        SyscallCausation::ChildAttempt(child) => {
            assert_eq!(child.task_id.as_str(), "wf-node0");
            assert_eq!(child.attempt_id.as_str(), "wf-node0:attempt:1");
            assert_eq!(child.request_seq, 0, "the seq is the list's own order");
        }
        other => panic!("expected a child-attempt causation, got {other:?}"),
    }
    // the proposal contributed no security field, and the record does not grow one
    let json = serde_json::to_value(&effect.memory).unwrap().to_string();
    for forbidden in ["tenant", "author", "trust_level", "record_id", "session"] {
        assert!(
            !json.contains(forbidden),
            "the kernel-authored write leaked {forbidden}"
        );
    }
}

#[test]
fn a_memory_query_is_clamped_to_the_operations_retrieval_policy() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let resolved = runtime.submit(&provider_result(
        "in-recall",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-1",
            crate::context::manager::MEMORY_TOOL_NAME,
            json!({"query": "past briefs", "top_k": 999}),
        )],
    ));
    let queried = sole_effect(&resolved);
    let EffectKind::QueryMemory(effect) = &queried.effect else {
        panic!("expected a memory query, got {:?}", queried.tag());
    };
    assert_eq!(
        effect.requested_k, 4,
        "the model cannot widen the operation's retrieval policy by asking for more"
    );
    assert!(matches!(
        effect.query.causation,
        SyscallCausation::ProviderTool(_)
    ));
    // the query the kernel authored is binding + causation + accepted time, nothing else
    let json = serde_json::to_value(&effect.query).unwrap().to_string();
    for forbidden in ["session", "tenant", "author", "trust", "agent_id"] {
        assert!(
            !json.contains(forbidden),
            "the kernel-authored query leaked {forbidden}"
        );
    }
    assert_eq!(effect.binding.binding_id.as_str(), "mem-binding-1");
}

/// §7.6 + §15.3 · a provider turn that mixes an effect-publishing syscall (`memory`) with host
/// tool calls publishes BOTH effects in one step — different kinds, so DEC-3 admits them
/// together — and the host consumes them one pending effect at a time. Resolving the syscall's
/// effect must not re-emit the dispatched tool batch: it is already pending, §15.3 admits at
/// most one pending effect per kind, and the calls would dispatch twice.
#[test]
fn a_mixed_syscall_and_host_tool_batch_resolves_without_re_emitting_the_tool_batch() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let mixed = runtime.submit(&provider_result(
        "in-mixed",
        1_700_000_002_000,
        &provider,
        vec![
            tool_call(
                "call-1",
                crate::context::manager::MEMORY_TOOL_NAME,
                json!({"query": "past briefs"}),
            ),
            tool_call("call-2", "search", json!({"q": "kpi benchmarks"})),
        ],
    ));

    // (1) the step publishes only the syscall's effect — one effect per step is the host's
    //     consumption contract, and the tool batch stays re-derivable rather than pending
    let query_effect = sole_effect(&mixed);
    assert_eq!(
        query_effect.tag(),
        EffectKindTag::QueryMemory,
        "the syscall effect is the step's only publication"
    );

    // (2) resolving the syscall effect re-derives the batch: the calls were dispatched but not
    //     published, so the kind slot is free and the resume rebuilds it from history
    let queried = runtime.submit(&resolved(
        "in-recalls",
        1_700_000_003_000,
        &query_effect.effect_id,
        EffectSuccess::MemoryQueried(MemoryQueriedSuccess {
            recalls: vec![MemoryRecall {
                record_ref: MemoryRecordRef::new("rec-1").unwrap(),
                name: "brief-style".to_string(),
                kind: SyscallMemoryKind::Project,
                content: "prefers numbered sections".to_string(),
                score: None,
            }],
        }),
    ));
    let tools_effect = sole_effect(&queried);
    assert_eq!(
        tools_effect.tag(),
        EffectKindTag::ExecuteTools,
        "with the syscall effect settled, the resume rebuilds the tool batch"
    );
    let tools_effect = tools_effect.clone();
    assert!(
        history_text(&runtime)
            .iter()
            .any(|line| line.contains("prefers numbered sections")),
        "the recall is in the context the resumed turn renders"
    );

    // (3) resolving the tool batch resumes the loop; the next provider call renders a context
    //     that carries both the recall and the tool result
    let resumed = runtime.submit(&payloads_resolved(
        "in-tools",
        1_700_000_004_000,
        &tools_effect.effect_id,
        vec![WireToolResultPayload::Inline(InlineToolResult {
            call_id: CallId::new("call-2").unwrap(),
            result: WireToolResult {
                output: "kpi: 12%".into(),
                durable_content: None,
                is_error: false,
                disposition: ToolResultDisposition::Recoverable,
            },
        })],
    ));
    assert_eq!(
        kinds(&resumed),
        vec![EffectKindTag::CallProvider],
        "results plus recalls are in, so the loop calls the provider again"
    );
    let text = history_text(&runtime).join("\n");
    assert!(
        text.contains("prefers numbered sections") && text.contains("kpi: 12%"),
        "the resumed context carries the recall and the tool result"
    );
}

// -----------------------------------------------------------------------------------------
// fixture: no-host-session-identity (§22.6 · Task 11)
// -----------------------------------------------------------------------------------------

/// Every JSON key anywhere in `value`.
fn all_keys(value: &Value, into: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                into.insert(key.clone());
                all_keys(child, into);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| all_keys(item, into)),
        _ => {}
    }
}

/// One full canonical arc — configure → workflow root start → spawn effect → spawn
/// acknowledgement → child completion carrying a memory proposal — as a host runs it.
///
/// Returns everything the arc made durable or published: the record chain (bytes and digests),
/// the effects, and the observations. Nothing here is host-timed or host-named beyond the
/// opaque ids §5.3 admits.
fn canonical_arc() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let mut runtime = Runtime::new();
    let mut effects = Vec::new();
    let mut observations = Vec::new();
    let collect = |_runtime: &Runtime,
                   committed: &CommittedTransition<PlannedStep>,
                   effects: &mut Vec<Value>,
                   observations: &mut Vec<Value>| {
        for effect in committed.published_effects() {
            effects.push(serde_json::to_value(effect).unwrap());
        }
        for observation in &committed.step.observations {
            observations.push(serde_json::to_value(observation).unwrap());
        }
    };

    let configured = runtime.submit(&syscall_config());
    collect(&runtime, &configured, &mut effects, &mut observations);

    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    collect(&runtime, &started, &mut effects, &mut observations);

    let acked = runtime.submit(&spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    collect(&runtime, &acked, &mut effects, &mut observations);

    let write =
        SyscallRequest::RequestMemoryWrite(super::super::syscall::RequestMemoryWriteRequest {
            proposal: super::super::syscall::MemoryWriteProposal {
                name: "source-set".to_string(),
                kind: WireMemoryKind::Project,
                content: "12 primary sources".to_string(),
                description: String::new(),
                evidence_refs: Vec::new(),
            },
        });
    let advanced = runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![write],
    ));
    collect(&runtime, &advanced, &mut effects, &mut observations);

    let records = runtime
        .journal
        .iter()
        .map(|record| {
            json!({
                "bytes": String::from_utf8_lossy(record.record_bytes().as_slice()).into_owned(),
                "digest": record.record_digest().as_str(),
            })
        })
        .collect();
    (records, effects, observations)
}

/// §22.6 · "host mapping does not affect kernel deterministic output", proved forwards: the arc
/// is a pure function of its logical inputs, so two hosts — whatever they call their sessions —
/// build the same record chain, publish the same effect bytes and read the same observations.
#[test]
fn the_canonical_arc_is_byte_identical_for_any_host() {
    let (records_a, effects_a, observations_a) = canonical_arc();
    let (records_b, effects_b, observations_b) = canonical_arc();

    assert!(!records_a.is_empty() && !effects_a.is_empty() && !observations_a.is_empty());
    assert_eq!(records_a, records_b, "the record chain is not reproducible");
    assert_eq!(
        effects_a, effects_b,
        "published effects are not reproducible"
    );
    assert_eq!(
        observations_a, observations_b,
        "observations are not reproducible"
    );
}

/// The scan half of Task 11: no session key, and no session *value*, exists anywhere on the
/// canonical output surface — the durable records, the effects the host executes, or the
/// observations it projects. A host may keep its own session mapping; the kernel holds none.
#[test]
fn no_canonical_record_effect_or_observation_names_a_session() {
    const BANNED_KEYS: [&str; 5] = [
        "session_id",
        "parent_session_id",
        "session",
        "submitter_agent_id",
        "actor_id",
    ];

    let (records, effects, observations) = canonical_arc();
    for (surface, values) in [
        ("record", &records),
        ("effect", &effects),
        ("observation", &observations),
    ] {
        for value in values {
            let mut keys = BTreeSet::new();
            all_keys(value, &mut keys);
            for banned in BANNED_KEYS {
                assert!(
                    !keys.contains(banned),
                    "canonical {surface} carries the host-owned key {banned:?}: {value}"
                );
            }
            assert!(
                !value.to_string().contains("session"),
                "canonical {surface} mentions a session: {value}"
            );
        }
    }

    // and the child the arc launched is correlated by logical identity alone
    let launch = effects
        .iter()
        .find(|effect| effect["effect"]["kind"] == "spawn_tasks")
        .expect("the arc launched a child");
    let task = &launch["effect"]["tasks"][0];
    assert_eq!(task["task_id"], "wf-node0");
    assert_eq!(task["attempt_id"], "wf-node0:attempt:1");
    assert!(
        task["launch_token"].as_str().is_some_and(|t| !t.is_empty()),
        "a child is named by task/attempt/launch token, never by a session"
    );
}

/// The process observation states the logical **parent task**, and the child spec carries an
/// empty session because canonical kernel input contains no host session identity.
#[test]
fn a_spawned_process_is_reported_by_its_logical_parent_task() {
    let (_, _, observations) = canonical_arc();
    let process = observations
        .iter()
        .find(|observation| observation["kind"] == "agent_process_changed")
        .expect("the arc published a process observation");
    assert_eq!(process["agent_id"], "wf-node0");
    assert_eq!(process["parent_task_id"], "root");

    let spec = agent_run_spec(&LogicalAgentSpec::new("write the brief"));
    assert_eq!(spec.identity.session_id.as_str(), NO_HOST_SESSION);
    assert!(spec.identity.parent_session_id.is_none());
}

// -----------------------------------------------------------------------------------------
// fixture: removed-self-declared-caller
// -----------------------------------------------------------------------------------------

/// Every P1 request shape, serialized. None of them has a field through which a host could say
/// who is asking — §22.10's whole point, and the reason omitting an id can no longer mean
/// "skip the trust downgrade".
#[test]
fn no_syscall_request_shape_carries_a_self_declared_caller() {
    use super::super::syscall::*;

    let requests = vec![
        SyscallRequest::SubmitWorkflow(SubmitWorkflowRequest {
            spec: two_node_spec(),
        }),
        SyscallRequest::AppendWorkflowNodes(AppendWorkflowNodesRequest {
            nodes: vec![wire_node("n", "n", &[])],
        }),
        SyscallRequest::ActivateSkill(ActivateSkillRequest {
            name: "debug".to_string(),
            lease_turns: Some(3),
        }),
        SyscallRequest::UpdateTask(UpdateTaskRequest {
            update: WireTaskUpdate::default(),
        }),
        SyscallRequest::RequestMemoryWrite(RequestMemoryWriteRequest {
            proposal: MemoryWriteProposal {
                name: "n".to_string(),
                kind: WireMemoryKind::Project,
                content: "c".to_string(),
                description: String::new(),
                evidence_refs: Vec::new(),
            },
        }),
        SyscallRequest::RequestMemoryQuery(RequestMemoryQueryRequest {
            query: MemoryQueryProposal::default(),
        }),
        SyscallRequest::PageIn(PageInRequest {
            handle_id: super::super::scalar::HandleId::new("h-1").unwrap(),
        }),
    ];

    for request in &requests {
        let text = serde_json::to_value(request).unwrap().to_string();
        for forbidden in [
            "submitter_agent_id",
            "actor_id",
            "agent_id",
            "session_id",
            "parent_session_id",
            "caller",
            "author",
            "trust",
        ] {
            assert!(
                !text.contains(forbidden),
                "{request:?} still exposes {forbidden}"
            );
        }
    }

    // a host cannot smuggle one in either: the shapes deny unknown fields
    let smuggled = r#"{"kind":"append_workflow_nodes","nodes":[],"submitter_agent_id":"root"}"#;
    assert!(
        serde_json::from_str::<SyscallRequest>(smuggled).is_err(),
        "a self-declared submitter must not decode"
    );
}

/// The three authority families a quarantined caller is refused, kept exhaustive against the
/// request union so a new syscall cannot be added without a decision about it.
#[test]
fn every_syscall_is_classified_against_the_quarantine_rule() {
    use super::super::syscall::*;

    let classified = [
        (
            SyscallRequest::SubmitWorkflow(SubmitWorkflowRequest {
                spec: WireSpec::default(),
            }),
            Some("workflow"),
        ),
        (
            SyscallRequest::AppendWorkflowNodes(AppendWorkflowNodesRequest { nodes: Vec::new() }),
            Some("workflow"),
        ),
        (
            SyscallRequest::ActivateSkill(ActivateSkillRequest {
                name: String::new(),
                lease_turns: None,
            }),
            Some("capability"),
        ),
        (
            SyscallRequest::RequestMemoryWrite(RequestMemoryWriteRequest {
                proposal: MemoryWriteProposal {
                    name: String::new(),
                    kind: WireMemoryKind::Project,
                    content: String::new(),
                    description: String::new(),
                    evidence_refs: Vec::new(),
                },
            }),
            Some("memory"),
        ),
        (
            SyscallRequest::RequestMemoryQuery(RequestMemoryQueryRequest {
                query: MemoryQueryProposal::default(),
            }),
            Some("memory"),
        ),
        (
            SyscallRequest::UpdateTask(UpdateTaskRequest {
                update: WireTaskUpdate::default(),
            }),
            None,
        ),
        (
            SyscallRequest::PageIn(PageInRequest {
                handle_id: super::super::scalar::HandleId::new("h-1").unwrap(),
            }),
            None,
        ),
    ];
    for (request, family) in &classified {
        assert_eq!(&privileged_family(request), family, "{request:?}");
    }
}

#[test]
fn a_page_in_of_a_handle_the_caller_does_not_hold_is_refused() {
    let (mut runtime, provider) = agent_awaiting_provider();
    runtime.submit(&provider_result(
        "in-read",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-1",
            crate::context::manager::READ_RESULT_TOOL_NAME,
            json!({"call_id": "never-existed"}),
        )],
    ));
    let rejected = rejections(&runtime);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, "read_result");
    assert!(
        rejected[0].2.contains("not reachable"),
        "got {:?}",
        rejected[0].2
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::CallProvider],
        "an address the caller does not hold produces no page-in effect; the turn continues"
    );
}

// -----------------------------------------------------------------------------------------
// §7.10 · external payload (Task 13)
// -----------------------------------------------------------------------------------------

/// The body a host persists before it ever submits a result. Long enough to be over the test
/// threshold, so both directions of the partition are exercised by real sizes.
const BODY: &str = "the full report body, far larger than this operation keeps resident, \
                        repeated so it clears the inline threshold by a comfortable margin";

fn body_digest() -> Digest {
    super::super::record::canonical_digest(BODY.as_bytes())
}

/// A payload policy small enough to test with real strings: results reaching 64 bytes must be
/// externalised, and at most 32 bytes of preview stay resident.
fn payload_config() -> WireEnvelope {
    use crate::runtime::kernel::wire::config::PayloadPolicy;
    syscall_config_with(|config| {
        config.host_effect_support = support_with([EffectKindTag::ArchivePageOut]);
        config.payload_policy = Some(PayloadPolicy {
            inline_threshold_bytes: Some(64),
            preview_bytes: Some(32),
        });
    })
}

fn external_payload(
    call_id: &str,
    digest: Digest,
    original_size: u64,
    preview: &str,
) -> WireToolResultPayload {
    external_payload_with(
        call_id,
        digest,
        original_size,
        preview,
        false,
        ToolResultDisposition::Recoverable,
    )
}

/// The same, with the two §7.10 rule 9 failure facts stated.
fn external_payload_with(
    call_id: &str,
    digest: Digest,
    original_size: u64,
    preview: &str,
    is_error: bool,
    disposition: ToolResultDisposition,
) -> WireToolResultPayload {
    WireToolResultPayload::External(super::super::effect::ExternalToolResult {
        call_id: CallId::new(call_id).unwrap(),
        payload_ref: PayloadRef::new("payload:01J8Y2QK7C4N0V").unwrap(),
        digest,
        original_size: WireU64::new(original_size),
        preview: preview.to_string(),
        is_error,
        disposition,
    })
}

fn payloads_resolved(
    id: &str,
    at: u64,
    effect: &EffectId,
    results: Vec<WireToolResultPayload>,
) -> WireEnvelope {
    resolved(
        id,
        at,
        effect,
        EffectSuccess::Tools(ToolsSuccess {
            results,
            measurements: Vec::new(),
        }),
    )
}

/// An agent that called one host tool and is waiting for its results, under `payload_config`.
fn agent_awaiting_tool_results() -> (Runtime, EffectId) {
    let mut runtime = Runtime::new();
    runtime.submit(&payload_config());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    ));
    let tools = sole_effect(&acted);
    assert_eq!(tools.tag(), EffectKindTag::ExecuteTools);
    (runtime, tools.effect_id.clone())
}

/// Structured durable content is measured as part of the inline result, so this fixture uses
/// a threshold large enough for the mixed text/image/file envelope while keeping the
/// external-payload tests on their intentionally small threshold above.
fn agent_awaiting_structured_tool_results() -> (Runtime, EffectId) {
    use crate::runtime::kernel::wire::config::PayloadPolicy;
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.payload_policy = Some(PayloadPolicy {
            inline_threshold_bytes: Some(1024),
            preview_bytes: Some(256),
        });
    }));
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    ));
    let tools = sole_effect(&acted);
    assert_eq!(tools.tag(), EffectKindTag::ExecuteTools);
    (runtime, tools.effect_id.clone())
}

/// The whole point of the contract: the *preview* enters context and the handle says where the
/// body is. Nothing about the body itself is inside this kernel.
#[test]
fn an_external_tool_result_lands_as_a_preview_and_an_external_handle() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let committed = runtime.submit(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![external_payload(
            "call-1",
            body_digest(),
            BODY.len() as u64,
            "the full report body, far la…",
        )],
    ));
    assert_eq!(
        kinds(&committed),
        vec![EffectKindTag::CallProvider],
        "an external result resumes the turn exactly like an inline one"
    );
    let EffectKind::CallProvider(provider) = &sole_effect(&committed).effect else {
        panic!("external residency must resume through the provider");
    };
    assert!(
        provider
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == READ_RESULT_TOOL_NAME),
        "the refreshed provider projection must advertise the newly reachable payload"
    );

    let engine = runtime.driver.engine().expect("the arc built an engine");
    assert_eq!(
        engine.ctx.payload_residency("call-1"),
        Some(&Residency::External {
            payload_ref: "payload:01J8Y2QK7C4N0V".to_string(),
            digest: body_digest().as_str().to_string(),
            original_size: BODY.len() as u64,
        }),
        "§7.10 rule 3 · the P3 handle is where the reference lives"
    );

    let rendered = serde_json::to_string(&engine.ctx.partitions.history.messages).unwrap();
    assert!(
        rendered.contains("the full report body, far la"),
        "the preview is what occupies working context"
    );
    assert!(
        !rendered.contains("clears the inline threshold"),
        "the body must not be in context: {rendered}"
    );
    assert!(observation_kinds(&runtime).contains(&"payload_residency_changed"));
    assert_eq!(
        runtime
            .observations()
            .iter()
            .filter(|observation| matches!(observation, KernelObservation::CheckpointTaken { .. }))
            .count(),
        1,
        "re-projecting external residency must not execute the provider-call boundary twice",
    );
}

/// §7.10 rule 2 · a digest this kernel cannot recompute is a payload it could never prove it
/// restored, so it is refused at admission rather than at page-in.
#[test]
fn an_external_tool_result_the_kernel_cannot_verify_is_refused() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let fault = runtime.reject(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![external_payload(
            "call-1",
            Digest::new("md5:deadbeef").unwrap(),
            BODY.len() as u64,
            "preview",
        )],
    ));
    assert_eq!(fault.code, KernelFaultCode::MalformedEnvelope);
    assert!(
        fault.message.contains("sha256:<64 hex>"),
        "the refusal names the only digest shape a page-in can be checked against: {}",
        fault.message
    );
}

/// §7.10 rule 2 · the configured threshold is a **total** partition. A body small enough to
/// inline may not be externalised: it would buy a `LoadPayload` round trip to read something
/// that fitted in the turn that produced it, and it would leave the one rule a host and the
/// kernel must agree on with a hole in the middle.
#[test]
fn an_external_tool_result_below_the_threshold_is_refused() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let small = "tiny";
    let fault = runtime.reject(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![external_payload(
            "call-1",
            super::super::record::canonical_digest(small.as_bytes()),
            small.len() as u64,
            small,
        )],
    ));
    assert_eq!(fault.code, KernelFaultCode::MalformedEnvelope);
    assert!(
        fault.message.contains("inlines below 64"),
        "{}",
        fault.message
    );
}

/// The preview is the part that actually occupies context, so it is the part the policy bounds.
#[test]
fn an_external_preview_over_the_resident_budget_is_refused() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let fault = runtime.reject(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![external_payload(
            "call-1",
            body_digest(),
            BODY.len() as u64,
            BODY,
        )],
    ));
    assert_eq!(fault.code, KernelFaultCode::ResourceLimitExceeded);
    assert!(fault.message.contains("preview"), "{}", fault.message);
}

/// §7.10 rules 1 and 5 · the kernel does not externalise on the host's behalf. Accepting an
/// oversized inline result and spooling it back out is the historical round trip where the body
/// crossed core twice and entered the journal twice; the only answer that keeps rule 5 true is
/// to refuse it.
#[test]
fn an_inline_tool_result_over_the_threshold_is_refused() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let fault = runtime.reject(&tools_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        &[("call-1", BODY, false)],
    ));
    assert_eq!(fault.code, KernelFaultCode::ResourceLimitExceeded);
    assert!(
        fault.message.contains("externalises at 64"),
        "{}",
        fault.message
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::ExecuteTools],
        "a rejected batch leaves the effect it was answering pending — zero mutation"
    );
}

/// A batch is adjudicated whole: one illegal result and none of it lands.
#[test]
fn one_illegal_result_rejects_the_whole_batch() {
    let mut runtime = Runtime::new();
    runtime.submit(&payload_config());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![
            tool_call("call-1", "search", json!({"q": "a"})),
            tool_call("call-2", "search", json!({"q": "b"})),
        ],
    ));
    let tools = sole_effect(&acted).effect_id.clone();
    runtime.reject(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![
            WireToolResultPayload::Inline(InlineToolResult {
                call_id: CallId::new("call-1").unwrap(),
                result: WireToolResult {
                    output: "small and legal".to_string(),
                    durable_content: None,
                    is_error: false,
                    disposition: ToolResultDisposition::Recoverable,
                },
            }),
            external_payload("call-2", Digest::new("md5:deadbeef").unwrap(), 9_000, "p"),
        ],
    ));
    let engine = runtime.driver.engine().expect("the arc built an engine");
    assert!(
        engine.ctx.payload_residency("call-1").is_none(),
        "the legal half of a refused batch must not have landed either"
    );
}

/// The full §7.10 rule 4 arc: `read_result` → `LoadPayload` → `PayloadLoaded`, with the body
/// entering context only once the digest proves it is the one that left.
#[test]
fn a_page_in_of_an_external_payload_loads_and_verifies_the_body() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let stored = runtime.submit(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![external_payload(
            "call-1",
            body_digest(),
            BODY.len() as u64,
            "the full report body, far la…",
        )],
    ));

    let read = runtime.submit(&provider_result(
        "in-read",
        1_700_000_004_000,
        &effect_id(stored.step_seq),
        vec![tool_call(
            "call-2",
            READ_RESULT_TOOL_NAME,
            json!({"call_id": "call-1"}),
        )],
    ));
    let load = sole_effect(&read);
    assert_eq!(load.tag(), EffectKindTag::LoadPayload);
    let EffectKind::LoadPayload(effect) = &load.effect else {
        panic!("expected a payload load");
    };
    assert_eq!(effect.handle_id.as_str(), "call-1");
    assert_eq!(
        effect.payload_ref.as_str(),
        "payload:01J8Y2QK7C4N0V",
        "the effect hands back the host's own opaque locator, unread"
    );
    let load = load.effect_id.clone();
    assert!(
        rejections(&runtime).is_empty(),
        "a reachable, externally-backed handle is a page-in, not a refusal"
    );

    let restored = runtime.submit(&resolved(
        "in-loaded",
        1_700_000_005_000,
        &load,
        EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
            handle_id: HandleId::new("call-1").unwrap(),
            payload: InlinePayload {
                content: BODY.to_string(),
                digest: body_digest(),
                original_size: WireU64::new(BODY.len() as u64),
            },
        }),
    ));
    assert_eq!(
        kinds(&restored),
        vec![EffectKindTag::CallProvider],
        "the paged-in body resumes the turn that asked for it"
    );

    let engine = runtime.driver.engine().expect("the arc built an engine");
    assert_eq!(
        engine.ctx.payload_residency("call-1"),
        Some(&Residency::Resident),
        "§25.9 · the handle is the fact, and it says the body came home"
    );
    let rendered = serde_json::to_string(&engine.ctx.partitions.history.messages).unwrap();
    assert!(
        rendered.contains("clears the inline threshold"),
        "the model reads the body it asked for"
    );
    assert!(observation_kinds(&runtime).contains(&"payload_residency_changed"));
}

/// The kernel never saw the body, so the digest is the only evidence there is. A mismatch is a
/// protocol violation with zero mutation, not a degraded read.
#[test]
fn a_paged_in_body_that_is_not_the_one_that_left_is_refused() {
    let (mut runtime, load) = agent_awaiting_payload_load();
    // Same length, and self-consistently declared — only the digest can tell the difference,
    // which is exactly the property the contract rests on.
    let substitute = BODY.replace("margin", "MARGIN");
    assert_eq!(substitute.len(), BODY.len());
    let fault = runtime.reject(&resolved(
        "in-loaded",
        1_700_000_005_000,
        &load,
        EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
            handle_id: HandleId::new("call-1").unwrap(),
            payload: InlinePayload {
                content: substitute.to_string(),
                digest: super::super::record::canonical_digest(substitute.as_bytes()),
                original_size: WireU64::new(substitute.len() as u64),
            },
        }),
    ));
    assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
    assert!(fault.message.contains("digests to"), "{}", fault.message);

    let engine = runtime.driver.engine().expect("the arc built an engine");
    assert!(
        matches!(
            engine.ctx.payload_residency("call-1"),
            Some(Residency::External { .. })
        ),
        "a refused restore leaves the handle exactly where it was"
    );
}

/// A loaded payload that does not agree with itself never gets as far as the digest: the size
/// it declares and the bytes it carries are the same claim stated twice.
#[test]
fn a_paged_in_body_that_contradicts_its_own_size_is_refused() {
    let (mut runtime, load) = agent_awaiting_payload_load();
    let fault = runtime.reject(&resolved(
        "in-loaded",
        1_700_000_005_000,
        &load,
        EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
            handle_id: HandleId::new("call-1").unwrap(),
            payload: InlinePayload {
                content: BODY.to_string(),
                digest: body_digest(),
                original_size: WireU64::new(BODY.len() as u64 + 1),
            },
        }),
    ));
    assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
    assert!(fault.message.contains("and carries"), "{}", fault.message);
}

/// The outcome must name the handle its effect addressed — the same rule `verify_page_out_receipt`
/// applies on the way out.
#[test]
fn a_paged_in_body_for_another_handle_is_refused() {
    let (mut runtime, load) = agent_awaiting_payload_load();
    let fault = runtime.reject(&resolved(
        "in-loaded",
        1_700_000_005_000,
        &load,
        EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
            handle_id: HandleId::new("call-9").unwrap(),
            payload: InlinePayload {
                content: BODY.to_string(),
                digest: body_digest(),
                original_size: WireU64::new(BODY.len() as u64),
            },
        }),
    ));
    assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
    assert!(fault.message.contains("names handle"), "{}", fault.message);
}

/// DEC-5 · a body the host cannot produce leaves the operation where it was. The read is
/// abandoned once, the loop continues, and the kernel does not re-issue the same load.
#[test]
fn a_payload_the_host_cannot_produce_abandons_the_read() {
    let (mut runtime, load) = agent_awaiting_payload_load();
    let resumed = runtime.submit(&failed(
        "in-load-failed",
        1_700_000_005_000,
        &load,
        HostEffectFailureKind::StorageUnavailable,
        "the blob store is offline",
    ));
    assert_eq!(
        kinds(&resumed),
        vec![EffectKindTag::CallProvider],
        "one page-in failure does not kill a live run"
    );
    assert!(observation_kinds(&runtime).contains(&"payload_load_failed"));
    let engine = runtime.driver.engine().expect("the arc built an engine");
    assert!(
        matches!(
            engine.ctx.payload_residency("call-1"),
            Some(Residency::External { .. })
        ),
        "the reference survives the failed read: the body is still out there"
    );
}

/// An address whose body core still holds is refused with the reason. There is no locator to
/// hand back, and inventing one is the confusion the closed union removes.
#[test]
fn a_page_in_of_a_resident_handle_is_refused() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let inlined = runtime.submit(&tools_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        &[("call-1", "three sources found", false)],
    ));
    runtime.submit(&provider_result(
        "in-read",
        1_700_000_004_000,
        &effect_id(inlined.step_seq),
        vec![tool_call(
            "call-2",
            READ_RESULT_TOOL_NAME,
            json!({"call_id": "call-1"}),
        )],
    ));
    let rejected = rejections(&runtime);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, "read_result");
    assert!(
        rejected[0].2.contains("nothing to page in"),
        "got {:?}",
        rejected[0].2
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::CallProvider],
        "a body core already holds publishes no load effect"
    );
}

/// B19 · the other half of the axis. A page-out archive becomes a `PagedOut` handle — a
/// *different* state from `External`, reached from `Resident` — and the model can read it back
/// through the same `LoadPayload` effect.
#[test]
fn a_page_out_archive_becomes_a_readable_paged_out_handle() {
    let (mut runtime, archive) = agent_awaiting_page_out();
    let EffectKind::ArchivePageOut(published) = &runtime
        .tx
        .pending_effects()
        .find(|effect| effect.effect_id == archive)
        .expect("the archive is pending")
        .effect
    else {
        panic!("expected a page-out archive");
    };
    let handle_id = published.handle_id.clone();
    let digest = published.payload.digest.clone();
    let content = published.payload.content.clone();
    let original_size = published.payload.original_size;

    let archived = runtime.submit(&resolved(
        "in-archived",
        1_700_000_003_000,
        &archive,
        EffectSuccess::PageOutArchived(super::super::effect::PageOutArchivedSuccess {
            receipt: ArchiveReceipt {
                handle_id: handle_id.clone(),
                payload_ref: PayloadRef::new("payload:archive-1").unwrap(),
                digest: digest.clone(),
                original_size,
            },
        }),
    ));
    let engine = runtime.driver.engine().expect("the arc built an engine");
    assert_eq!(
        engine.ctx.payload_residency(handle_id.as_str()),
        Some(&Residency::PagedOut {
            payload_ref: "payload:archive-1".to_string(),
            digest: digest.as_str().to_string(),
        }),
        "an archived body is paged out, never external — it *was* resident"
    );
    assert!(observation_kinds(&runtime).contains(&"payload_residency_changed"));

    // and the same read_result path addresses it
    let read = runtime.submit(&provider_result(
        "in-read",
        1_700_000_004_000,
        &effect_id(archived.step_seq),
        vec![tool_call(
            "call-7",
            READ_RESULT_TOOL_NAME,
            json!({ "call_id": handle_id.as_str() }),
        )],
    ));
    let load = sole_effect(&read);
    assert_eq!(load.tag(), EffectKindTag::LoadPayload);
    let load = load.effect_id.clone();

    runtime.submit(&resolved(
        "in-loaded",
        1_700_000_005_000,
        &load,
        EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
            handle_id: handle_id.clone(),
            payload: InlinePayload {
                content: content.clone(),
                digest,
                original_size,
            },
        }),
    ));
    let engine = runtime.driver.engine().expect("the arc built an engine");
    assert_eq!(
        engine.ctx.payload_residency(handle_id.as_str()),
        Some(&Residency::Resident),
        "the archived history came home through the same effect the external body uses"
    );
}

/// An agent holding one external payload, with a page-in of it pending.
fn agent_awaiting_payload_load() -> (Runtime, EffectId) {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let stored = runtime.submit(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![external_payload(
            "call-1",
            body_digest(),
            BODY.len() as u64,
            "the full report body, far la…",
        )],
    ));
    let read = runtime.submit(&provider_result(
        "in-read",
        1_700_000_004_000,
        &effect_id(stored.step_seq),
        vec![tool_call(
            "call-2",
            READ_RESULT_TOOL_NAME,
            json!({"call_id": "call-1"}),
        )],
    ));
    (runtime, sole_effect(&read).effect_id.clone())
}

// -----------------------------------------------------------------------------------------
// fixture: removed-large-result-spool-effect
// -----------------------------------------------------------------------------------------

/// §7.10 rules 5 and 6 / §25.10 · the body does not cross core, in either direction.
///
/// The scan is over everything the arc made durable or published: no record, no effect and no
/// observation carries the persisted body, and no effect kind exists through which the kernel
/// could hand a body back out to be persisted. The full output enters only through a verified
/// host-owned payload reference.
#[test]
fn no_canonical_record_effect_or_observation_carries_an_external_body() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    runtime.submit(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![external_payload(
            "call-1",
            body_digest(),
            BODY.len() as u64,
            "the full report body, far la…",
        )],
    ));

    // The record's canonical input is a base64 envelope in JSON, so scanning the record bytes
    // alone would pass for free. Every accepted input is decoded back out and scanned as text.
    let mut surfaces: Vec<(String, String)> = Vec::new();
    for record in &runtime.journal {
        surfaces.push((
            "record".to_string(),
            String::from_utf8_lossy(record.record_bytes().as_slice()).into_owned(),
        ));
        surfaces.push((
            "accepted input".to_string(),
            serde_json::to_string(&record.normalized_input().expect("the record decodes")).unwrap(),
        ));
    }
    for effect in runtime.tx.pending_effects() {
        surfaces.push(("effect".to_string(), serde_json::to_string(effect).unwrap()));
    }
    for observation in runtime.observations() {
        surfaces.push((
            "observation".to_string(),
            serde_json::to_string(observation).unwrap(),
        ));
    }
    assert!(surfaces.len() >= 4, "the arc produced nothing to scan");
    for (surface, text) in &surfaces {
        assert!(
            !text.contains("clears the inline threshold"),
            "the canonical {surface} carries the externalised body: {text}"
        );
        assert!(
            !text.contains("spool"),
            "the canonical {surface} still speaks of spooling: {text}"
        );
    }

    // and no effect kind can express handing a body back out to be written
    for tag in EffectKindTag::ALL {
        assert!(
            !tag.as_str().contains("spool"),
            "{} would be the round trip §7.10 deletes",
            tag.as_str()
        );
    }
}

// -----------------------------------------------------------------------------------------
// fixture: removed-session-log-payload-lookup
// -----------------------------------------------------------------------------------------

/// §7.10 rules 4 and 7 · a page-in addresses the handle table and nothing else.
///
/// The historical `read_result` resolved by scanning a spool directory and then falling back to
/// a linear walk of the SessionLog, so any path-shaped string was a readable address and a body
/// that had left the table was still reachable. Here the two refusals are total: an address the
/// caller does not hold, and a locator-shaped string that is not an address at all. Neither
/// produces an effect, so there is no lookup to fall back *to*.
#[test]
fn a_page_in_cannot_address_anything_outside_the_handle_table() {
    let (mut runtime, tools) = agent_awaiting_tool_results();
    let stored = runtime.submit(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &tools,
        vec![external_payload(
            "call-1",
            body_digest(),
            BODY.len() as u64,
            "the full report body, far la…",
        )],
    ));
    // the locator the host chose is *not* an address: only the handle is
    runtime.submit(&provider_result(
        "in-read",
        1_700_000_004_000,
        &effect_id(stored.step_seq),
        vec![tool_call(
            "call-2",
            READ_RESULT_TOOL_NAME,
            json!({"call_id": "payload:01J8Y2QK7C4N0V"}),
        )],
    ));
    let rejected = rejections(&runtime);
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0].2.contains("not reachable"),
        "got {:?}",
        rejected[0].2
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::CallProvider],
        "no effect is published for an address the table does not hold"
    );
}

// -----------------------------------------------------------------------------------------
// determinism: the record chain replays to the same steps
// -----------------------------------------------------------------------------------------

#[test]
fn the_journal_replays_to_byte_identical_records() {
    let runtime = drive_workflow_root_to_terminal();
    verify_record_chain(&runtime.journal).expect("the chain links up");

    let mut replay = CanonicalOperationDriver::new();
    let rebuilt: KernelTransaction<PlannedStep, InMemoryRecordIndex> =
        KernelTransaction::rebuild_from_records(
            &runtime.journal,
            ConfigDefaults::default(),
            InMemoryRecordIndex::new(),
            |context| replay.fold(context),
        )
        .expect("a deterministic driver rebuilds its own journal");

    assert_eq!(rebuilt.lifecycle(), OperationLifecycle::Completed);
    assert_eq!(rebuilt.terminal(), runtime.tx.terminal());
    assert_eq!(replay.root_kind(), runtime.driver.root_kind());
    assert_eq!(replay.focus(), runtime.driver.focus());
}

/// A journal that contains syscall transitions rebuilds to the same steps — the caller a
/// request was attributed to, the causation it spent and the graph it grew are all kernel state
/// derived from the records, not host state a resume has to reassemble (§10.3).
#[test]
fn a_journal_with_syscall_transitions_replays_to_byte_identical_records() {
    let mut runtime = workflow_root_awaiting_first_child();
    runtime.submit(&child_done_with(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![
            SyscallRequest::AppendWorkflowNodes(
                super::super::syscall::AppendWorkflowNodesRequest {
                    nodes: vec![wire_node("verify", "verify", &[])],
                },
            ),
            SyscallRequest::AppendWorkflowNodes(
                super::super::syscall::AppendWorkflowNodesRequest {
                    nodes: vec![wire_node("orphan", "orphan", &["nowhere"])],
                },
            ),
        ],
    ));
    verify_record_chain(&runtime.journal).expect("the chain links up");

    let mut replay = CanonicalOperationDriver::new();
    let rebuilt: KernelTransaction<PlannedStep, InMemoryRecordIndex> =
        KernelTransaction::rebuild_from_records(
            &runtime.journal,
            ConfigDefaults::default(),
            InMemoryRecordIndex::new(),
            |context| replay.fold(context),
        )
        .expect("a deterministic driver rebuilds its own syscall journal");

    assert_eq!(rebuilt.lifecycle(), runtime.tx.lifecycle());
    assert_eq!(replay.focus(), runtime.driver.focus());
    assert_eq!(
        replay.engine().unwrap().workflow_node_count(),
        runtime.driver.engine().unwrap().workflow_node_count(),
        "the appended node is a kernel fact the replay reproduces"
    );
    assert_eq!(
        replay.attempts, runtime.driver.attempts,
        "live attempts rebuild identically, so authority after a rebuild is the same"
    );
}

#[test]
fn a_plan_that_never_commits_fails_closed_instead_of_drifting() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());

    // plan a start, then abandon it: the semantic kernel already moved under that plan
    let preparation = runtime.prepare(&agent_start("in-start", 1_700_000_001_000));
    let token = preparation.token().unwrap().clone();
    runtime.tx.abort(&token).expect("abort before the append");

    let fault = runtime
        .driver
        .plan(&PlanContext {
            input: &runtime.journal[0].normalized_input().unwrap(),
            step_seq: WireU64::new(1),
            previous_head: None,
            config: runtime.tx.config().unwrap(),
            resolving: None,
            pending: &[],
        })
        .expect_err("a discarded plan poisons the driver");
    assert_eq!(fault.code, KernelFaultCode::TransactionConflict);
    assert!(runtime.driver.poison().is_some(), "the driver is poisoned");
}

// -----------------------------------------------------------------------------------------
// §7.9 · unified effect resolution (Task 12)
//
// Three slices, in the order the spec suggests: provider/tool, workflow/control,
// memory/page-out. Each effect kind gets success, failure and replay.
// -----------------------------------------------------------------------------------------

use crate::runtime::kernel::wire::effect::{
    ApprovalSuccess, ArchiveReceipt, EffectFailed, HostEffectFailure, HostEffectFailureKind,
    InlinePayload, InlineToolResult, MemoryPersistReceipt, MemoryPersistedSuccess,
    MemoryQueriedSuccess, MemoryRecall, MilestoneCheckResult as WireMilestoneResult,
    MilestoneEvaluatedSuccess, PageOutArchivedSuccess, PayloadLoadedSuccess,
    ProviderContextOverflow, ProviderStopReason, TaskAlreadyFinished, TaskPreemptOutcome,
    TaskPreemptStatus, TaskPreempted, TasksPreemptedSuccess, ToolResult as WireToolResult,
    ToolsSuccess,
};
use crate::runtime::kernel::wire::scalar::{CallId, HandleId};
use crate::runtime::kernel::wire::syscall::MemoryKind as SyscallMemoryKind;
use crate::runtime::kernel::wire::{Digest, MemoryRecordRef, PayloadRef};

/// The support set `syscall_config` declares, plus whatever an arc additionally needs. Adding
/// rather than replacing keeps §7.3's cross-field rules satisfied (a declared tool catalog
/// implies `execute_tools` + `load_payload`).
fn support_with(extra: impl IntoIterator<Item = EffectKindTag>) -> HostEffectSupport {
    HostEffectSupport::new(
        [
            EffectKindTag::CallProvider,
            EffectKindTag::ExecuteTools,
            EffectKindTag::LoadPayload,
            EffectKindTag::SpawnTasks,
            EffectKindTag::PreemptTasks,
            EffectKindTag::PersistMemory,
            EffectKindTag::QueryMemory,
        ]
        .into_iter()
        .chain(extra),
    )
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

fn failed(
    id: &str,
    at: u64,
    effect: &EffectId,
    kind: HostEffectFailureKind,
    message: &str,
) -> WireEnvelope {
    envelope(
        id,
        at,
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: effect.clone(),
            outcome: EffectOutcome::Failed(EffectFailed {
                failure: HostEffectFailure {
                    kind,
                    message: message.to_string(),
                    retryable: None,
                },
            }),
        }),
    )
}

/// A provider turn that finished with plain text and no tool calls.
fn provider_answer(id: &str, at: u64, effect: &EffectId, text: &str) -> WireEnvelope {
    resolved(
        id,
        at,
        effect,
        EffectSuccess::Provider(super::super::effect::ProviderSuccess {
            outcome: ProviderOutcome::Completed(ProviderCompleted {
                message: ProviderMessage {
                    role: MessageRole::Assistant,
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

fn provider_overflow(id: &str, at: u64, effect: &EffectId) -> WireEnvelope {
    resolved(
        id,
        at,
        effect,
        EffectSuccess::Provider(super::super::effect::ProviderSuccess {
            outcome: ProviderOutcome::ContextOverflow(ProviderContextOverflow {
                observed_input_tokens: Some(999_999),
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
                        },
                    })
                })
                .collect(),
            measurements: Vec::new(),
        }),
    )
}

fn kinds(committed: &CommittedTransition<PlannedStep>) -> Vec<EffectKindTag> {
    committed
        .published_effects()
        .iter()
        .map(|effect| effect.tag())
        .collect()
}

/// Every observation the last committed transition recorded, by variant name.
fn observation_kinds(runtime: &Runtime) -> Vec<&'static str> {
    runtime
        .observations()
        .iter()
        .map(observation_label)
        .collect()
}

fn observation_label(observation: &KernelObservation) -> &'static str {
    match observation {
        KernelObservation::MemoryWritten { .. } => "memory_written",
        KernelObservation::MemoryWriteFailed { .. } => "memory_write_failed",
        KernelObservation::MemoryQueried { .. } => "memory_queried",
        KernelObservation::MemoryQueryFailed { .. } => "memory_query_failed",
        KernelObservation::PageOutArchived { .. } => "page_out_archived",
        KernelObservation::PageOutArchiveFailed { .. } => "page_out_archive_failed",
        KernelObservation::ApprovalResolutionFailed { .. } => "approval_resolution_failed",
        KernelObservation::AgentPreemptFailed { .. } => "agent_preempt_failed",
        KernelObservation::ControlRequestRejected { .. } => "control_request_rejected",
        KernelObservation::Compressed { .. } => "compressed",
        KernelObservation::WorkflowBatchSpawned { .. } => "workflow_batch_spawned",
        KernelObservation::WorkflowCompleted { .. } => "workflow_completed",
        KernelObservation::MilestoneAdvanced { .. } => "milestone_advanced",
        KernelObservation::MilestoneBlocked { .. } => "milestone_blocked",
        KernelObservation::Resumed { .. } => "resumed",
        KernelObservation::Suspended { .. } => "suspended",
        KernelObservation::SignalDeliveryDisposed { .. } => "signal_delivery_disposed",
        KernelObservation::SignalDisplaced { .. } => "signal_displaced",
        KernelObservation::SignalExpired { .. } => "signal_expired",
        KernelObservation::SignalsPending { .. } => "signals_pending",
        KernelObservation::OperationCancelled { .. } => "operation_cancelled",
        KernelObservation::LivePolicyChanged { .. } => "live_policy_changed",
        KernelObservation::CapabilityChanged { .. } => "capability_changed",
        KernelObservation::AgentPreempted { .. } => "agent_preempted",
        KernelObservation::PayloadResidencyChanged { .. } => "payload_residency_changed",
        KernelObservation::PayloadLoadFailed { .. } => "payload_load_failed",
        _ => "other",
    }
}

/// The `(disposition, signal_id, delivery_id, attempt)` of every delivery the last transition
/// disposed of.
fn dispositions(runtime: &Runtime) -> Vec<(String, String, String, u32)> {
    runtime
        .observations()
        .iter()
        .filter_map(|observation| match observation {
            KernelObservation::SignalDeliveryDisposed {
                disposition,
                signal_id,
                delivery_id,
                attempt,
                ..
            } => Some((
                disposition.clone(),
                signal_id.clone(),
                delivery_id.clone(),
                *attempt,
            )),
            _ => None,
        })
        .collect()
}

/// The rendered history of the operation, as text — what the model will read next turn.
fn history_text(runtime: &Runtime) -> Vec<String> {
    runtime
        .driver
        .engine()
        .map(|engine| {
            engine
                .ctx
                .partitions
                .history
                .messages
                .iter()
                .map(|message| format!("{:?}:{}", message.role, message_text(message)))
                .collect()
        })
        .unwrap_or_default()
}

fn message_text(message: &CoreMessage) -> String {
    match &message.content {
        Content::Text(text) => text.clone(),
        Content::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                crate::types::message::ContentPart::ToolResult { output, .. } => output.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

// ----- slice 1 · provider / tool -----------------------------------------------------------

#[test]
fn a_provider_turn_with_host_tool_calls_publishes_execute_tools() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let dispatched = runtime.submit(&provider_result(
        "in-search",
        1_700_000_002_000,
        &provider,
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    ));
    assert_eq!(kinds(&dispatched), vec![EffectKindTag::ExecuteTools]);
    let EffectKind::ExecuteTools(execute) = &sole_effect(&dispatched).effect else {
        panic!("expected a tool batch");
    };
    assert_eq!(execute.calls.len(), 1);
    assert_eq!(execute.calls[0].name, "search");
    assert_eq!(execute.calls[0].call_id.as_str(), "call-1");

    // and the results resolve that effect and re-ask the provider — the ordinary turn cycle
    let resumed = runtime.submit(&tools_resolved(
        "in-results",
        1_700_000_003_000,
        &effect_id(dispatched.step_seq),
        &[("call-1", "three sources found", false)],
    ));
    assert_eq!(kinds(&resumed), vec![EffectKindTag::CallProvider]);
    assert!(
        history_text(&runtime)
            .iter()
            .any(|line| line.contains("three sources found")),
        "the tool output is what the next turn reads"
    );
}

#[test]
fn a_provider_answer_with_no_tool_calls_commits_the_agent_terminal() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let finished = runtime.submit(&provider_answer(
        "in-final",
        1_700_000_002_000,
        &provider,
        "the brief is written",
    ));
    let terminal = finished.terminal().expect("a final answer terminates");
    let KernelTerminal::Agent(agent) = terminal else {
        panic!("expected an agent terminal, got {terminal:?}");
    };
    assert_eq!(agent.result.termination, WireTermination::Completed);
    assert_eq!(
        agent.result.final_message.as_ref().unwrap().content,
        "the brief is written"
    );
    assert!(
        finished.published_effects().is_empty(),
        "§7.12 · a terminal step publishes no effect"
    );
}

// fixture: budget-usage-reported-once-at-terminal
#[test]
fn the_usage_report_rides_the_terminal_and_only_the_terminal() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let finished = runtime.submit(&provider_answer(
        "in-final",
        1_700_000_002_000,
        &provider,
        "done",
    ));
    let KernelTerminal::Agent(agent) = finished.terminal().unwrap() else {
        panic!("expected an agent terminal");
    };
    let reported = agent.usage.clone();

    // Replaying the very input that committed the terminal answers with the same record and
    // does not mint a second report.
    let replayed = runtime.prepare(&provider_answer(
        "in-final",
        1_700_000_002_000,
        &provider,
        "done",
    ));
    let RecordPreparation::Replayed(replay) = replayed else {
        panic!("an exact replay is a replay, not a new step");
    };
    let Some(committed_step) = &replay.committed_step else {
        panic!("a replay above the checkpoint floor carries its step");
    };
    let StepDisposition::Terminal(terminal) = &committed_step.disposition else {
        panic!("the replayed step is the terminal one");
    };
    let KernelTerminal::Agent(replayed_agent) = &terminal.terminal else {
        panic!("expected an agent terminal");
    };
    assert_eq!(replayed_agent.usage, reported, "one report, one terminal");
    assert_eq!(replay.step_seq, finished.step_seq);
}

// fixture: agent-syscall-caller-is-derived (§5k · the syscall-only continuation)
#[test]
fn a_syscall_only_turn_continues_with_another_provider_call() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let continued = runtime.submit(&provider_result(
        "in-plan",
        1_700_000_002_000,
        &provider,
        vec![
            tool_call("call-1", "skill", json!({"name": "debug"})),
            tool_call(
                "call-2",
                "update_plan",
                json!({"progress": "sources listed"}),
            ),
        ],
    ));

    assert_eq!(
        kinds(&continued),
        vec![EffectKindTag::CallProvider],
        "a pure control-plane batch publishes no effect of its own, so the kernel continues \
             the turn itself rather than leaving the operation with nothing outstanding"
    );
    let history = history_text(&runtime);
    assert!(
        history.iter().any(|line| line.contains("skill activated")),
        "every syscall the kernel executed is answered so the transcript pairs: {history:?}"
    );
    assert!(
        history.iter().any(|line| line.contains("plan updated")),
        "{history:?}"
    );
    // and the assistant turn the model emitted is in history verbatim, calls included
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .partitions
            .history
            .messages
            .iter()
            .any(|message| message.tool_calls.len() == 2),
        "the model reads back the turn it actually emitted"
    );
}

#[test]
fn a_syscall_batch_that_published_an_effect_waits_instead_of_re_asking() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let queried = runtime.submit(&provider_result(
        "in-memory",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-1",
            crate::context::manager::MEMORY_TOOL_NAME,
            json!({"query": "prior briefs"}),
        )],
    ));
    assert_eq!(
        kinds(&queried),
        vec![EffectKindTag::QueryMemory],
        "the turn resumes when the recall it asked for resolves; a provider call now would \
             race the very facts it was issued to read"
    );
}

#[test]
fn a_mixed_batch_adjudicates_the_syscall_and_dispatches_the_tool() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let mixed = runtime.submit(&provider_result(
        "in-mixed",
        1_700_000_002_000,
        &provider,
        vec![
            tool_call("call-1", "skill", json!({"name": "debug"})),
            tool_call("call-2", "search", json!({"q": "x"})),
        ],
    ));
    assert_eq!(kinds(&mixed), vec![EffectKindTag::ExecuteTools]);
    let EffectKind::ExecuteTools(execute) = &sole_effect(&mixed).effect else {
        panic!("expected a tool batch");
    };
    assert_eq!(
        execute
            .calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>(),
        vec!["search"],
        "a P1 syscall is never dispatched to a host executor"
    );
    assert!(
        history_text(&runtime)
            .iter()
            .any(|line| line.contains("skill activated")),
        "the syscall half still closes its own transcript pair"
    );
}

// fixture: fault-effect-resolution-fails-closed
#[test]
fn a_context_overflow_compacts_and_re_asks_without_reading_vendor_text() {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));
    let provider = sole_effect(&started).effect_id.clone();

    let recovered = runtime.submit(&provider_overflow(
        "in-overflow",
        1_700_000_002_000,
        &provider,
    ));
    assert_eq!(
        kinds(&recovered),
        vec![EffectKindTag::CallProvider],
        "an overflow is a semantic outcome the kernel recovers from, not a transport failure"
    );
    assert!(
        observation_kinds(&runtime).contains(&"compressed"),
        "the recovery ladder ran: {:?}",
        observation_kinds(&runtime)
    );
}

#[test]
fn canonical_genesis_installs_the_entropy_watch_on_the_engine() {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.execution_policy = Some(ExecutionPolicy {
            max_turns: Some(12),
            entropy_watch: Some(EntropyWatchPolicy {
                enabled: Some(true),
                threshold_ppm: Some(Ppm::new(100_000).unwrap()),
                hysteresis_ppm: Some(Ppm::ZERO),
                cooldown_turns: Some(0),
                notify_model: Some(true),
            }),
            ..ExecutionPolicy::default()
        });
    }));
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({ "q": "same" }))],
    ));
    let resolved = runtime.submit(&tools_resolved(
        "in-results",
        1_700_000_003_000,
        &effect_id(acted.step_seq),
        &[("call-1", "failed", true)],
    ));

    assert!(
        resolved
            .step
            .observations
            .iter()
            .any(|observation| matches!(observation, KernelObservation::EntropyAlert { .. })),
        "the resolved canonical execution policy must arm the semantic engine's entropy watch"
    );
}

/// **Task 14 · core does not parse a raw vendor error string.**
///
/// Provider prose has no recovery semantics: the same words are ordinary completion content
/// or failure diagnostics. Only the typed `ContextOverflow` outcome drives recovery.
#[test]
fn vendor_error_prose_is_content_and_never_a_recovery_decision() {
    const VENDOR_PROSE: &str = "HTTP 413: prompt is too long — context_length_exceeded, \
                                    maximum context length is 128000 tokens";

    // (a) as the model's own words: an ordinary completed turn
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));
    let answered = runtime.submit(&provider_answer(
        "in-prose",
        1_700_000_002_000,
        &sole_effect(&started).effect_id.clone(),
        VENDOR_PROSE,
    ));
    assert!(
        answered.terminal().is_some() || kinds(&answered) == vec![EffectKindTag::CallProvider],
        "the words are content, not a classification"
    );
    assert!(
        !observation_kinds(&runtime).contains(&"compressed"),
        "no recovery ladder ran: {:?}",
        observation_kinds(&runtime)
    );

    // (b) as a host failure message: a failed terminal, not an overflow recovery
    let (mut runtime, provider) = agent_awaiting_provider();
    let ended = runtime.submit(&failed(
        "in-prose-failure",
        1_700_000_002_000,
        &provider,
        HostEffectFailureKind::TransportExhausted,
        VENDOR_PROSE,
    ));
    let Some(KernelTerminal::Failed(failure)) = ended.terminal() else {
        panic!("expected a failed terminal, got {:?}", ended.terminal());
    };
    assert_eq!(failure.failure.code, KernelFailureCode::HostEffectFailed);
    assert!(
        !observation_kinds(&runtime).contains(&"compressed"),
        "a failure message is diagnostics; it never selects the recovery ladder: {:?}",
        observation_kinds(&runtime)
    );
}

/// **Task 14 · no vendor vocabulary survives anywhere on the canonical face.**
///
/// Every closed vocabulary the wire publishes, scanned against the words hosts historically
/// forwarded verbatim. This is the regression guard for "the host maps, core never learns":
/// adding a `rate_limited` failure class or a `stop` stop-reason would break it here rather
/// than three releases later when something starts branching on it.
#[test]
fn no_canonical_vocabulary_contains_a_vendor_word() {
    const VENDOR_WORDS: [&str; 16] = [
        "rate_limit",
        "429",
        "503",
        "413",
        "overloaded",
        // `storage_unavailable` is the deliberate near-miss: a *storage* classification, not a
        // vendor's "service unavailable" — which folds into `transport_exhausted` once the
        // host's own ladder is spent. So the banned word is the vendor's compound, not the
        // bare adjective.
        "service_unavailable",
        "context_length",
        "max_context",
        "too_long",
        "finish_reason",
        "openai",
        "anthropic",
        "gemini",
        "deepseek",
        "qwen",
        "minimax",
    ];

    let mut vocabulary: Vec<&'static str> = Vec::new();
    vocabulary.extend(EffectKindTag::ALL.iter().map(|tag| tag.as_str()));
    vocabulary.extend(
        super::super::effect::EffectSuccessTag::ALL
            .iter()
            .map(|tag| tag.as_str()),
    );
    vocabulary.extend(HostEffectFailureKind::ALL.iter().map(|kind| kind.as_str()));
    vocabulary.extend(ProviderStopReason::ALL.iter().map(|r| r.as_str()));
    vocabulary.extend(ToolResultDisposition::ALL.iter().map(|d| d.as_str()));
    assert!(vocabulary.len() >= 34, "the scan lost a vocabulary");

    for word in vocabulary {
        for vendor in VENDOR_WORDS {
            assert!(
                !word.contains(vendor),
                "{word:?} carries the vendor word {vendor:?}; the canonical face is the \
                     host's mapping *target*, never its passthrough"
            );
        }
    }

    // `storage_unavailable` is the one near-miss and it is deliberate: it is a *storage*
    // classification, not a vendor's "service unavailable" — which folds into
    // `transport_exhausted` after the host's own ladder is spent.
    assert!(
        HostEffectFailureKind::ALL
            .iter()
            .any(|kind| kind.as_str() == "storage_unavailable")
    );
    assert!(
        !HostEffectFailureKind::ALL
            .iter()
            .any(|kind| kind.as_str().contains("service"))
    );
}

#[test]
fn the_overflow_ladder_is_bounded_and_ends_in_an_honest_terminal() {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.recovery_policy = Some(crate::runtime::kernel::wire::command::RecoveryPolicy {
            provider_recovery_attempts: Some(1),
            ..Default::default()
        });
    }));
    let started = runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));
    let mut provider = sole_effect(&started).effect_id.clone();

    let first = runtime.submit(&provider_overflow("in-of-1", 1_700_000_002_000, &provider));
    assert_eq!(kinds(&first), vec![EffectKindTag::CallProvider]);
    provider = sole_effect(&first).effect_id.clone();

    let exhausted = runtime.submit(&provider_overflow("in-of-2", 1_700_000_003_000, &provider));
    let KernelTerminal::Agent(agent) = exhausted.terminal().expect("the ladder is bounded") else {
        panic!("expected an agent terminal");
    };
    assert_eq!(agent.result.termination, WireTermination::ContextOverflow);
}

// fixture: removed-kernel-auto-redispatch
#[test]
fn a_provider_failure_commits_a_terminal_and_never_re_asks() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let ended = runtime.submit(&failed(
        "in-dead",
        1_700_000_002_000,
        &provider,
        HostEffectFailureKind::TransportExhausted,
        "the vendor returned 503 five times",
    ));
    assert!(
        ended.published_effects().is_empty(),
        "DEC-5 · the kernel makes one policy decision and does not re-emit the same intent"
    );
    let KernelTerminal::Failed(failure) = ended.terminal().expect("a terminal was committed")
    else {
        panic!("expected a failed terminal, got {:?}", ended.terminal());
    };
    assert_eq!(failure.failure.code, KernelFailureCode::HostEffectFailed);
    assert!(
        failure.failure.message.contains("transport_exhausted"),
        "the classification decides; the prose is only what an operator reads: {}",
        failure.failure.message
    );
}

// fixture: removed-kernel-auto-redispatch
#[test]
fn a_tool_batch_failure_answers_every_dispatched_call_and_never_re_runs_it() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let dispatched = runtime.submit(&provider_result(
        "in-search",
        1_700_000_002_000,
        &provider,
        vec![
            tool_call("call-1", "search", json!({"q": "a"})),
            tool_call("call-2", "search", json!({"q": "b"})),
        ],
    ));
    let tools = effect_id(dispatched.step_seq);

    let answered = runtime.submit(&failed(
        "in-exec-failed",
        1_700_000_003_000,
        &tools,
        HostEffectFailureKind::PermissionDenied,
        "the executor is not allowed to run tools in this sandbox",
    ));
    assert_eq!(
        kinds(&answered),
        vec![EffectKindTag::CallProvider],
        "the batch is abandoned and the model is asked again — the kernel never re-dispatches \
             the same batch"
    );
    let history = history_text(&runtime);
    let refusals = history
        .iter()
        .filter(|line| line.contains("permission_denied"))
        .count();
    assert_eq!(
        refusals, 2,
        "every dispatched call still gets a result: {history:?}"
    );
}

// ----- §7.10 · tool result disposition (Task 14) --------------------------------------------

/// A batch of two calls, dispatched and awaiting results.
fn agent_dispatching_two_tools() -> (Runtime, EffectId) {
    let (mut runtime, provider) = agent_awaiting_provider();
    let dispatched = runtime.submit(&provider_result(
        "in-search",
        1_700_000_002_000,
        &provider,
        vec![
            tool_call("call-1", "search", json!({"q": "a"})),
            tool_call("call-2", "search", json!({"q": "b"})),
        ],
    ));
    assert_eq!(kinds(&dispatched), vec![EffectKindTag::ExecuteTools]);
    let tools = effect_id(dispatched.step_seq);
    (runtime, tools)
}

fn tool_batch(
    id: &str,
    at: u64,
    effect: &EffectId,
    results: &[(&str, &str, bool, ToolResultDisposition)],
) -> WireEnvelope {
    resolved(
        id,
        at,
        effect,
        EffectSuccess::Tools(ToolsSuccess {
            results: results
                .iter()
                .map(|(call_id, output, is_error, disposition)| {
                    WireToolResultPayload::Inline(InlineToolResult {
                        call_id: CallId::new(*call_id).unwrap(),
                        result: WireToolResult {
                            output: (*output).to_string(),
                            durable_content: None,
                            is_error: *is_error,
                            disposition: *disposition,
                        },
                    })
                })
                .collect(),
            measurements: Vec::new(),
        }),
    )
}

#[test]
fn a_fatal_result_closes_out_the_calls_its_batch_never_answered() {
    // `fatal` = the executor stopped. The unanswered call is not pending — it will never be
    // answered — so leaving it alone would produce an assistant turn whose tool_call has no
    // matching tool_result, which is malformed on every vendor wire.
    let (mut runtime, tools) = agent_dispatching_two_tools();
    let settled = runtime.submit(&tool_batch(
        "in-fatal",
        1_700_000_003_000,
        &tools,
        &[(
            "call-1",
            "disk corrupt, aborting",
            true,
            ToolResultDisposition::Fatal,
        )],
    ));
    assert_eq!(
        kinds(&settled),
        vec![EffectKindTag::CallProvider],
        "the turn still completes and the model is asked again — a fatal result is model \
             feedback, not a rollback (v0.2.42)"
    );

    let history = history_text(&runtime);
    assert!(
        history.iter().any(|line| line.contains("disk corrupt")),
        "the failure the host reported stays visible: {history:?}"
    );
    // The pairing invariant is the point: both dispatched calls end the turn with a result.
    let answered = tool_result_call_ids(&runtime);
    assert_eq!(
        answered,
        vec!["call-1".to_string(), "call-2".to_string()],
        "the call the batch never answered is closed out"
    );
    assert!(
        history
            .iter()
            .any(|line| line.contains("not executed") && line.contains("fatally")),
        "the close-out says why the call did not run: {history:?}"
    );
}

/// Every `call_id` that has a committed tool result in history, in order.
fn tool_result_call_ids(runtime: &Runtime) -> Vec<String> {
    runtime
        .driver
        .engine()
        .map(|engine| {
            engine
                .ctx
                .partitions
                .history
                .messages
                .iter()
                .flat_map(|message| match &message.content {
                    Content::Parts(parts) => parts
                        .iter()
                        .filter_map(|part| match part {
                            crate::types::message::ContentPart::ToolResult { call_id, .. } => {
                                Some(call_id.to_string())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    Content::Text(_) => Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_ordinary_batch_closes_out_nothing() {
    // The close-out is fatal-only. A short *recoverable* batch says nothing about the calls it
    // omitted, so the kernel must not invent results for them.
    let (mut runtime, tools) = agent_dispatching_two_tools();
    runtime.submit(&tool_batch(
        "in-partial",
        1_700_000_003_000,
        &tools,
        &[(
            "call-1",
            "no matches",
            false,
            ToolResultDisposition::Recoverable,
        )],
    ));
    let history = history_text(&runtime);
    assert!(
        !history.iter().any(|line| line.contains("not executed")),
        "a recoverable batch closes nothing out: {history:?}"
    );
}

/// §7.10 rule 9 · the close-out is **total over residency**.
///
/// A tool that fails after producing a huge diagnostic body is the common shape, not a rare
/// one. If the fatality scan read only the inline arm, whether a fatal failure stopped the
/// batch would depend on how many bytes its traceback happened to be.
#[test]
fn an_externalised_fatal_stops_the_batch_exactly_as_an_inline_one_does() {
    let (mut runtime, tools) = agent_dispatching_two_tools();
    let settled = runtime.submit(&resolved(
        "in-fatal-external",
        1_700_000_003_000,
        &tools,
        EffectSuccess::Tools(ToolsSuccess {
            results: vec![external_payload_with(
                "call-1",
                Digest::new(
                    "sha256:3b1f4a7c9e2d05186a4c7f0b9d3e8c25714f6a0b8c5d2e9f1a3b6c8d0e2f4a61",
                )
                .unwrap(),
                524_288,
                "Traceback (most recent call last):",
                true,
                ToolResultDisposition::Fatal,
            )],
            measurements: Vec::new(),
        }),
    ));
    assert_eq!(kinds(&settled), vec![EffectKindTag::CallProvider]);
    assert_eq!(
        tool_result_call_ids(&runtime),
        vec!["call-1".to_string(), "call-2".to_string()],
        "an externalised fatal closes the batch out just like an inline one"
    );
    let history = history_text(&runtime);
    assert!(
        history
            .iter()
            .any(|line| line.contains("not executed") && line.contains("fatally")),
        "{history:?}"
    );

    // …and the failure itself stays visible as an error, which the old shape could not express
    // at all: an externalised failure was indistinguishable from an externalised success.
    assert!(
        runtime
            .driver
            .engine()
            .expect("engine")
            .ctx
            .partitions
            .history
            .messages
            .iter()
            .any(
                |message| matches!(&message.content, Content::Parts(parts) if parts
                .iter()
                .any(|part| matches!(
                    part,
                    crate::types::message::ContentPart::ToolResult {
                        call_id,
                        durable_content: None,
                        is_error: true,
                        ..
                    } if call_id.as_str() == "call-1"
                )))
            ),
        "the externalised failure is committed as an error result"
    );
}

#[test]
fn a_fatal_result_does_not_synthesise_an_answer_the_host_already_gave() {
    // The close-out is keyed on *unanswered* calls, so a complete fatal batch adds nothing —
    // otherwise a call would get two results.
    let (mut runtime, tools) = agent_dispatching_two_tools();
    runtime.submit(&tool_batch(
        "in-fatal-complete",
        1_700_000_003_000,
        &tools,
        &[
            ("call-1", "boom", true, ToolResultDisposition::Fatal),
            (
                "call-2",
                "ok anyway",
                false,
                ToolResultDisposition::Recoverable,
            ),
        ],
    ));
    let history = history_text(&runtime);
    assert!(
        !history.iter().any(|line| line.contains("not executed")),
        "every call was answered by the host: {history:?}"
    );
    assert_eq!(
        history
            .iter()
            .filter(|line| line.contains("ok anyway"))
            .count(),
        1,
        "no call gets two results: {history:?}"
    );
}

// ----- §7.9 · DEC-5 differential (Task 14) --------------------------------------------------

/// **Recovery exhaustion differential.** The kernel's decision on a failure is chosen by the
/// effect kind *it* published, never by the failure kind the host reports and never by
/// `retryable`. The strongest statement of that is byte equality: for every one of the ten
/// effect kinds, all six failure classes × three `retryable` values must plan the same step.
///
/// If any of those ever became an input, this is the test that breaks — which is the point,
/// because "the kernel does not retry" is otherwise only a comment.
#[test]
fn a_host_failure_plans_the_same_step_whatever_the_host_advises() {
    for kind in HostEffectFailureKind::ALL {
        let mut planned: Option<Value> = None;
        for retryable in [None, Some(true), Some(false)] {
            let (mut runtime, tools) = agent_dispatching_two_tools();
            let settled = runtime.submit(&envelope(
                "in-failed",
                1_700_000_003_000,
                KernelInput::ResolveEffect(ResolveEffect {
                    effect_id: tools.clone(),
                    outcome: EffectOutcome::Failed(EffectFailed {
                        failure: HostEffectFailure {
                            kind,
                            // the message is fixed: only the advice varies
                            message: "the executor could not run".to_string(),
                            retryable,
                        },
                    }),
                }),
            ));
            let step = serde_json::to_value(&settled.step).unwrap();
            match &planned {
                None => planned = Some(step),
                Some(first) => assert_eq!(
                    first, &step,
                    "{kind:?} with retryable={retryable:?} planned a different step; \
                         `retryable` is advice and DEC-5 leaves it no branch to select"
                ),
            }
        }
    }
}

#[test]
fn the_recovery_decision_reads_the_effect_kind_the_kernel_published() {
    // The other half of the same claim: the *kind* the host reports does not select the
    // decision either. Six wildly different failure classes on the same effect all produce the
    // one decision that effect kind gets — here, "answer every call and ask the model again".
    for kind in HostEffectFailureKind::ALL {
        let (mut runtime, tools) = agent_dispatching_two_tools();
        let settled = runtime.submit(&failed(
            "in-failed",
            1_700_000_003_000,
            &tools,
            kind,
            "the executor could not run",
        ));
        assert_eq!(
            kinds(&settled),
            vec![EffectKindTag::CallProvider],
            "{kind:?} must take the same decision as every other failure class"
        );
        assert!(
            runtime.pending_effect_kinds() == vec![EffectKindTag::CallProvider],
            "{kind:?}: the failed batch is never re-issued (DEC-5)"
        );
    }
}

// fixture: fault-effect-resolution-fails-closed
#[test]
fn duplicate_and_conflicting_resolutions_are_settled_before_the_driver_sees_them() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let first = runtime.submit(&provider_result(
        "in-skill",
        1_700_000_002_000,
        &provider,
        vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
    ));

    // the same resolution under a fresh input id is a replay of the existing record (DEC-1)
    let replay = runtime.prepare(&provider_result(
        "in-skill-again",
        1_700_000_002_500,
        &provider,
        vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
    ));
    let RecordPreparation::Replayed(replayed) = replay else {
        panic!("a semantically identical redelivery is a replay");
    };
    assert_eq!(replayed.step_seq, first.step_seq);

    // a *different* payload for the same effect is a conflict
    let conflict = runtime.reject(&provider_result(
        "in-skill-conflict",
        1_700_000_002_600,
        &provider,
        vec![tool_call("call-9", "skill", json!({"name": "debug"}))],
    ));
    assert_eq!(conflict.code, KernelFaultCode::UnexpectedEffectOutcome);

    // an effect nobody is waiting on
    let unknown = EffectId::new("op-driver-1:step:77:effect:0").unwrap();
    let stray = runtime.reject(&provider_answer(
        "in-stray",
        1_700_000_002_700,
        &unknown,
        "hello",
    ));
    assert_eq!(stray.code, KernelFaultCode::UnexpectedEffectOutcome);

    // and a result of the wrong *kind* for the pending effect
    let pending = effect_id(first.step_seq);
    let mismatched = runtime.reject(&tools_resolved(
        "in-mismatch",
        1_700_000_002_800,
        &pending,
        &[("call-1", "x", false)],
    ));
    assert_eq!(mismatched.code, KernelFaultCode::UnexpectedEffectOutcome);
}

// ----- slice 2 · workflow / control ---------------------------------------------------------

#[test]
fn spc_008_01_wire_node_metadata_requested_capabilities_threads_into_the_core_spec() {
    use crate::types::capability::{
        ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
        ResourceSelector,
    };

    let capability = Capability {
        id: CapabilityId("cap-1".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/src/**".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: true,
        issuer: Principal("root".into()),
    };
    let metadata = json!({ "requested_capabilities": [capability.clone()] });

    let core = build_core_spec(&WireSpec {
        name: "cap-thread".to_string(),
        nodes: vec![WireNode {
            node_id: NodeId::new("solo").unwrap(),
            task: LogicalTask::new("do work"),
            depends_on: Vec::new(),
            run_spec: Some(LogicalAgentSpec {
                metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                ..LogicalAgentSpec::new("do work")
            }),
        }],
    })
    .expect("well-formed requested_capabilities builds");

    assert_eq!(core.nodes[0].requested_capabilities, vec![capability]);
}

#[test]
fn spc_008_02_wire_node_metadata_requested_budget_threads_into_the_core_spec() {
    use crate::scheduler::budget_grant::ResourceBudget;

    let budget = ResourceBudget {
        tokens: Some(1_000),
        ..ResourceBudget::default()
    };
    let metadata = json!({ "requested_budget": budget });

    let core = build_core_spec(&WireSpec {
        name: "budget-thread".to_string(),
        nodes: vec![WireNode {
            node_id: NodeId::new("solo").unwrap(),
            task: LogicalTask::new("do work"),
            depends_on: Vec::new(),
            run_spec: Some(LogicalAgentSpec {
                metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                ..LogicalAgentSpec::new("do work")
            }),
        }],
    })
    .expect("well-formed requested_budget builds");

    assert_eq!(core.nodes[0].requested_budget, Some(budget));
}

#[test]
fn spc_016_06_wire_node_scheduling_factors_thread_into_the_core_spec() {
    let factors = crate::orchestration::task_graph::SchedulingFactors {
        deadline_urgency: 3,
        process_priority: 2,
        resource_pressure: 1,
        budget_pressure: 4,
    };
    let metadata = json!({ "scheduling_factors": factors });

    let core = build_core_spec(&WireSpec {
        name: "scheduler-factors".to_string(),
        nodes: vec![WireNode {
            node_id: NodeId::new("solo").unwrap(),
            task: LogicalTask::new("do work"),
            depends_on: Vec::new(),
            run_spec: Some(LogicalAgentSpec {
                metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                ..LogicalAgentSpec::new("do work")
            }),
        }],
    })
    .expect("well-formed scheduling factors build");

    assert_eq!(core.nodes[0].scheduling_factors, factors);
}

#[test]
fn spc_016_06_malformed_wire_node_scheduling_factors_fail_closed() {
    let metadata = json!({ "scheduling_factors": { "deadline_urgency": "urgent" } });
    let error = build_core_spec(&WireSpec {
        name: "scheduler-factors-malformed".to_string(),
        nodes: vec![WireNode {
            node_id: NodeId::new("solo").unwrap(),
            task: LogicalTask::new("do work"),
            depends_on: Vec::new(),
            run_spec: Some(LogicalAgentSpec {
                metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                ..LogicalAgentSpec::new("do work")
            }),
        }],
    })
    .expect_err("malformed scheduling factors must not silently become zeros");
    assert_eq!(error.code, KernelFaultCode::InvalidConfig);
}

#[test]
fn spc_008_02_malformed_requested_budget_metadata_fails_closed() {
    let metadata = json!({ "requested_budget": {"tokens": "not a number"} });

    let error = build_core_spec(&WireSpec {
        name: "budget-malformed".to_string(),
        nodes: vec![WireNode {
            node_id: NodeId::new("solo").unwrap(),
            task: LogicalTask::new("do work"),
            depends_on: Vec::new(),
            run_spec: Some(LogicalAgentSpec {
                metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                ..LogicalAgentSpec::new("do work")
            }),
        }],
    })
    .expect_err("malformed requested_budget must fail closed, not silently drop");
    assert_eq!(error.code, KernelFaultCode::InvalidConfig);
}

#[test]
fn spc_008_01_malformed_requested_capabilities_metadata_fails_closed() {
    // A security-relevant declaration that fails to parse must reject the whole spec, not
    // silently degrade into "no capability requested" (which would make the attenuation check
    // a no-op instead of denying).
    let metadata = json!({ "requested_capabilities": [{"not": "a capability"}] });

    let error = build_core_spec(&WireSpec {
        name: "cap-malformed".to_string(),
        nodes: vec![WireNode {
            node_id: NodeId::new("solo").unwrap(),
            task: LogicalTask::new("do work"),
            depends_on: Vec::new(),
            run_spec: Some(LogicalAgentSpec {
                metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                ..LogicalAgentSpec::new("do work")
            }),
        }],
    })
    .expect_err("malformed requested_capabilities must fail closed, not silently drop");
    assert_eq!(error.code, KernelFaultCode::InvalidConfig);
}

// fixture: removed-kernel-auto-redispatch
#[test]
fn a_spawn_failure_fails_the_whole_batch_and_never_re_launches_it() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    let spawn = effect_id(started.step_seq);

    let after = runtime.submit(&failed(
        "in-launch-failed",
        1_700_000_002_000,
        &spawn,
        HostEffectFailureKind::ResourceExhausted,
        "no worker slots",
    ));
    assert!(
        !after
            .published_effects()
            .iter()
            .any(|effect| effect.tag() == EffectKindTag::SpawnTasks),
        "DEC-5 · the same launch intent is never re-emitted"
    );
    let KernelTerminal::Workflow(workflow) = after
        .terminal()
        .expect("a DAG whose only ready node could not start drains")
    else {
        panic!("expected a workflow terminal, got {:?}", after.terminal());
    };
    assert_eq!(workflow.outcome.status, WorkflowStatus::Failed);
    assert!(
        runtime.driver.attempts.is_empty(),
        "a launch that never happened leaves no live attempt to complete later"
    );
}

#[test]
fn an_approval_resolution_dispatches_exactly_what_was_approved() {
    let (mut runtime, provider) = agent_awaiting_approval();
    let requested = runtime.submit(&provider_result(
        "in-gated",
        1_700_000_002_000,
        &provider,
        vec![
            tool_call("call-1", "search", json!({"q": "a"})),
            tool_call("call-2", "search", json!({"q": "b"})),
        ],
    ));
    assert_eq!(kinds(&requested), vec![EffectKindTag::RequestApproval]);
    let approval = effect_id(requested.step_seq);
    assert!(matches!(
        runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(ROOT_TASK_ID)
            .unwrap()
            .wait_set
            .as_ref()
            .unwrap()
            .conditions
            .as_slice(),
        [WaitCondition::Approval(_)]
    ));

    let resumed = runtime.submit(&resolved(
        "in-approved",
        1_700_000_003_000,
        &approval,
        EffectSuccess::Approval(ApprovalSuccess {
            approved_call_ids: vec![CallId::new("call-1").unwrap()],
            denied_call_ids: vec![CallId::new("call-2").unwrap()],
        }),
    ));
    assert_eq!(kinds(&resumed), vec![EffectKindTag::ExecuteTools]);
    let EffectKind::ExecuteTools(execute) = &sole_effect(&resumed).effect else {
        panic!("expected a tool batch");
    };
    assert_eq!(
        execute
            .calls
            .iter()
            .map(|call| call.call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-1"],
        "only the approved call reaches a host"
    );
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(ROOT_TASK_ID)
            .unwrap()
            .wait_set
            .is_none(),
        "approval success consumes the durable approval wait"
    );
}

// fixture: removed-kernel-auto-redispatch
#[test]
fn an_approval_failure_denies_every_gated_call_and_never_re_asks() {
    let (mut runtime, provider) = agent_awaiting_approval();
    let requested = runtime.submit(&provider_result(
        "in-gated",
        1_700_000_002_000,
        &provider,
        vec![tool_call("call-1", "search", json!({"q": "a"}))],
    ));
    let approval = effect_id(requested.step_seq);

    let after = runtime.submit(&failed(
        "in-approval-failed",
        1_700_000_003_000,
        &approval,
        HostEffectFailureKind::StorageUnavailable,
        "the approval queue is down",
    ));
    assert!(
        !after
            .published_effects()
            .iter()
            .any(|effect| effect.tag() == EffectKindTag::RequestApproval),
        "DEC-5 · `retry_approval` is deleted on this wire"
    );
    assert_eq!(kinds(&after), vec![EffectKindTag::CallProvider]);
    assert!(
        observation_kinds(&runtime).contains(&"approval_resolution_failed"),
        "the failure is a typed audit fact: {:?}",
        observation_kinds(&runtime)
    );
    assert!(
        history_text(&runtime)
            .iter()
            .any(|line| line.contains("permission denied")),
        "fail closed: an approval that never arrived approves nothing"
    );
}

#[test]
fn a_preempt_resolution_settles_every_attempt_it_names() {
    let (mut runtime, spawn_effect) = workflow_with_live_child();
    let attempts = vec![
        TaskPreemptOutcome {
            task_id: TaskId::new("wf-node0").unwrap(),
            attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
            outcome: TaskPreemptStatus::Preempted(TaskPreempted {}),
        },
        TaskPreemptOutcome {
            task_id: TaskId::new("wf-node1").unwrap(),
            attempt_id: WireAttemptId::new("wf-node1:attempt:1").unwrap(),
            outcome: TaskPreemptStatus::AlreadyFinished(TaskAlreadyFinished {}),
        },
    ];
    // The preempt effect has no canonical producer yet (its only trigger is the signal /
    // cancel path). Publishing it through the driver's own mint is what makes the *resolution*
    // contract — the half Task 12 owns — reachable.
    let carrier = syscall_carrier("in-preempt-effect", 1_700_000_004_000);
    let published = runtime.submit_planned(&carrier, |driver, context| {
        let mut index = 0;
        let effect = driver.mint_effect(
            context,
            EffectKind::PreemptTasks(PreemptTasksEffect {
                attempts: vec![TaskAttemptRef {
                    task_id: TaskId::new("wf-node0").unwrap(),
                    attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                }],
                reason: "cancelled".to_string(),
            }),
            &mut index,
        );
        Ok(PlannedStep {
            root_kind: Some(RootKind::Workflow),
            focus: driver.focus().cloned(),
            observations: Vec::new(),
            disposition: StepDisposition::Effects(EffectsDisposition {
                effects: vec![effect],
            }),
        })
    });
    let preempt = effect_id(published.step_seq);
    assert!(runtime.driver.attempts.contains_key("wf-node0"));

    runtime.submit(&resolved(
        "in-preempted",
        1_700_000_005_000,
        &preempt,
        EffectSuccess::TasksPreempted(TasksPreemptedSuccess { attempts }),
    ));
    assert!(
        !runtime.driver.attempts.contains_key("wf-node0"),
        "a preempted attempt is spent, so a later completion naming it is a stale causation"
    );
    let _ = spawn_effect;
}

// fixture: removed-kernel-auto-redispatch
#[test]
fn a_preempt_failure_is_an_audit_fact_and_not_a_second_preemption() {
    let (mut runtime, _) = workflow_with_live_child();
    let carrier = syscall_carrier("in-preempt-effect", 1_700_000_004_000);
    let published = runtime.submit_planned(&carrier, |driver, context| {
        let mut index = 0;
        let effect = driver.mint_effect(
            context,
            EffectKind::PreemptTasks(PreemptTasksEffect {
                attempts: vec![TaskAttemptRef {
                    task_id: TaskId::new("wf-node0").unwrap(),
                    attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                }],
                reason: "cancelled".to_string(),
            }),
            &mut index,
        );
        Ok(PlannedStep {
            root_kind: Some(RootKind::Workflow),
            focus: driver.focus().cloned(),
            observations: Vec::new(),
            disposition: StepDisposition::Effects(EffectsDisposition {
                effects: vec![effect],
            }),
        })
    });
    let preempt = effect_id(published.step_seq);

    let after = runtime.submit(&failed(
        "in-preempt-failed",
        1_700_000_005_000,
        &preempt,
        HostEffectFailureKind::Unknown,
        "the supervisor did not answer",
    ));
    assert!(
        after.published_effects().is_empty(),
        "DEC-5 · `retry_preempt` is deleted on this wire"
    );
    assert!(
        observation_kinds(&runtime).contains(&"agent_preempt_failed"),
        "{:?}",
        observation_kinds(&runtime)
    );
}

#[test]
fn a_milestone_verdict_advances_the_contract_it_belongs_to() {
    let (mut runtime, provider) = agent_awaiting_milestone_check();
    let requested = runtime.submit(&provider_answer(
        "in-claim",
        1_700_000_002_000,
        &provider,
        "phase one is done",
    ));
    assert_eq!(kinds(&requested), vec![EffectKindTag::EvaluateMilestone]);
    let milestone = effect_id(requested.step_seq);

    let blocked = runtime.submit(&resolved(
        "in-verdict",
        1_700_000_003_000,
        &milestone,
        EffectSuccess::MilestoneEvaluated(MilestoneEvaluatedSuccess {
            result: WireMilestoneResult {
                phase_id: "collect".to_string(),
                passed: false,
                failed_criteria: vec!["no sources cited".to_string()],
                score: None,
                notes: String::new(),
            },
        }),
    ));
    assert_eq!(kinds(&blocked), vec![EffectKindTag::CallProvider]);
    assert!(
        observation_kinds(&runtime).contains(&"milestone_blocked"),
        "{:?}",
        observation_kinds(&runtime)
    );
}

/// **`EvaluateMilestone`'s canonical producer, end to end** (Task 12 SPEC-ISSUE-4).
///
/// Contract declared in `verification_contracts` → referenced by the root run spec → loaded as
/// the engine's phase cascade → phase 0 published as an `EvaluateMilestone` → a passing verdict
/// mounts that phase's `unlocks` and moves the cascade to phase 1. Before Task 14 the middle
/// three links did not exist on the wire at all: the effect, its resolution and its failure
/// path were reachable only by reaching into the engine from outside.
#[test]
fn a_declared_contract_drives_the_whole_milestone_cascade() {
    let (mut runtime, provider) = agent_awaiting_milestone_check();

    // link 3: the cascade is installed, so the first turn's completion asks for phase 0
    let requested = runtime.submit(&provider_answer(
        "in-claim",
        1_700_000_002_000,
        &provider,
        "sources collected",
    ));
    assert_eq!(kinds(&requested), vec![EffectKindTag::EvaluateMilestone]);
    let EffectKind::EvaluateMilestone(evaluate) = &sole_effect(&requested).effect else {
        panic!("expected a milestone request");
    };
    assert_eq!(
        evaluate.request.phase_id, "collect",
        "the request names the phase the declared cascade is on"
    );
    assert_eq!(
        evaluate.request.contract_id, "brief-quality-primary",
        "a phase id is unique only inside its contract, so the request carries the pair the \
             host looks its verifier up by"
    );
    // and nothing else: criteria, evidence and verifier are host-owned (§5.2)
    let request = serde_json::to_value(&evaluate.request).unwrap();
    assert_eq!(
        request.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["contract_id", "phase_id"],
        "{request}"
    );

    // link 4: a pass mounts the phase's unlocks and advances to phase 1
    let advanced = runtime.submit(&resolved(
        "in-verdict",
        1_700_000_003_000,
        &effect_id(requested.step_seq),
        EffectSuccess::MilestoneEvaluated(MilestoneEvaluatedSuccess {
            result: WireMilestoneResult {
                phase_id: "collect".to_string(),
                passed: true,
                failed_criteria: Vec::new(),
                score: None,
                notes: String::new(),
            },
        }),
    ));
    assert_eq!(kinds(&advanced), vec![EffectKindTag::CallProvider]);
    let advance = runtime
        .observations()
        .iter()
        .find_map(|observation| match observation {
            KernelObservation::MilestoneAdvanced {
                phase_id,
                capabilities_unlocked,
                ..
            } => Some((phase_id.clone(), capabilities_unlocked.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{:?}", observation_kinds(&runtime)));
    assert_eq!(advance.0, "collect");
    assert_eq!(
        advance.1,
        vec!["Tool:search".to_string()],
        "the phase's declared unlocks are the ones mounted"
    );
    assert_eq!(
        runtime
            .driver
            .engine()
            .expect("engine")
            .current_milestone_phase_id(),
        Some("write"),
        "the cascade advanced to the next declared phase"
    );

    // link 5: phase 1 unlocks a *skill*, proving the projection covers both directories
    let requested_2 = runtime.submit(&provider_answer(
        "in-claim-2",
        1_700_000_004_000,
        &effect_id(advanced.step_seq),
        "brief written",
    ));
    assert_eq!(kinds(&requested_2), vec![EffectKindTag::EvaluateMilestone]);
    runtime.submit(&resolved(
        "in-verdict-2",
        1_700_000_005_000,
        &effect_id(requested_2.step_seq),
        EffectSuccess::MilestoneEvaluated(MilestoneEvaluatedSuccess {
            result: WireMilestoneResult {
                phase_id: "write".to_string(),
                passed: true,
                failed_criteria: Vec::new(),
                score: None,
                notes: String::new(),
            },
        }),
    ));
    let unlocked_by_phase_two =
        runtime
            .observations()
            .iter()
            .find_map(|observation| match observation {
                KernelObservation::MilestoneAdvanced {
                    capabilities_unlocked,
                    ..
                } => Some(capabilities_unlocked.clone()),
                _ => None,
            });
    assert_eq!(unlocked_by_phase_two, Some(vec!["Skill:debug".to_string()]));
}

#[test]
fn a_run_spec_naming_an_undeclared_contract_is_refused_before_anything_moves() {
    // A reference that resolves to nothing is a gate the run believes it has: the agent would
    // start with no cascade, never publish an `EvaluateMilestone`, and finish having "passed"
    // a contract that was never evaluated.
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.host_effect_support = support_with([EffectKindTag::EvaluateMilestone]);
        config.verification_contracts = vec![brief_contract()];
    }));
    let head_before = runtime.tx.head();

    let fault = runtime.reject(&agent_start_under_contract(
        "in-start",
        1_700_000_001_000,
        "brief-quality-alternate",
    ));
    assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
    assert!(
        fault.message.contains("brief-quality-alternate"),
        "{}",
        fault.message
    );
    assert_eq!(runtime.tx.head(), head_before, "nothing moved");
    assert!(
        runtime.driver.root_kind().is_none(),
        "the operation is still free to start with a spec that resolves"
    );

    // …and the same spec with the declared id starts normally
    runtime.submit(&agent_start_under_contract(
        "in-start-ok",
        1_700_000_001_500,
        "brief-quality-primary",
    ));
    assert_eq!(runtime.driver.root_kind(), Some(RootKind::Agent));
}

#[test]
fn a_workflow_node_may_not_name_an_undeclared_contract_either() {
    // Same rule wherever a `LogicalAgentSpec` enters: a root, a DAG node, an authored append.
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.host_effect_support = support_with([EffectKindTag::EvaluateMilestone]);
        config.verification_contracts = vec![brief_contract()];
    }));
    let mut spec = two_node_spec();
    spec.nodes[1].run_spec = Some(LogicalAgentSpec {
        verification_contract_id: Some("no-such-contract".to_string()),
        ..LogicalAgentSpec::new("write the brief")
    });
    let fault = runtime.reject(&workflow_start("in-start", 1_700_000_001_000, spec));
    assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
    assert!(
        fault.message.contains("no-such-contract"),
        "{}",
        fault.message
    );
}

// fixture: removed-kernel-auto-redispatch
#[test]
fn a_milestone_that_could_not_be_evaluated_terminates_instead_of_advancing() {
    let (mut runtime, provider) = agent_awaiting_milestone_check();
    let requested = runtime.submit(&provider_answer(
        "in-claim",
        1_700_000_002_000,
        &provider,
        "phase one is done",
    ));
    let milestone = effect_id(requested.step_seq);

    let ended = runtime.submit(&failed(
        "in-verifier-down",
        1_700_000_003_000,
        &milestone,
        HostEffectFailureKind::StorageUnavailable,
        "the verifier could not be reached",
    ));
    assert!(ended.published_effects().is_empty());
    let KernelTerminal::Failed(failure) = ended.terminal().expect("a terminal was committed")
    else {
        panic!("expected a failed terminal, got {:?}", ended.terminal());
    };
    assert_eq!(failure.failure.code, KernelFailureCode::HostEffectFailed);
    assert!(
        failure.failure.message.contains("evaluate_milestone"),
        "{}",
        failure.failure.message
    );
}

// ----- slice 3 · memory / page-out ----------------------------------------------------------

/// §22.13 · a receipt is a locator, never a rewrite of the record the kernel authored.
#[test]
fn a_memory_receipt_cannot_restate_what_the_kernel_authored() {
    let (mut runtime, effect) = agent_awaiting_memory_write();
    let settled = runtime.submit(&resolved(
        "in-persisted",
        1_700_000_003_000,
        &effect,
        EffectSuccess::MemoryPersisted(MemoryPersistedSuccess {
            receipt: MemoryPersistReceipt {
                binding_id: MemoryBindingId::new("some-other-binding").unwrap(),
                record_ref: MemoryRecordRef::new("rec-7").unwrap(),
                digest: Digest::new("sha256:".to_string() + &"0".repeat(64)).unwrap(),
            },
        }),
    ));
    assert!(
        settled.published_effects().is_empty(),
        "a persisted record is a fact, not a new obligation"
    );
    let written = runtime
        .observations()
        .iter()
        .find_map(|observation| match observation {
            KernelObservation::MemoryWritten {
                record_id,
                scope,
                name,
                memory_kind,
                size_bytes,
                ..
            } => Some((
                record_id.clone(),
                scope.clone(),
                name.clone(),
                *memory_kind,
                *size_bytes,
            )),
            _ => None,
        })
        .expect("the resolution records the write");
    assert_eq!(written.0, "rec-7", "the host contributes its own locator");
    assert_eq!(
        written.1.namespace, "mem-binding-1",
        "and nothing else: the binding is the one the operation holds, not the one echoed back"
    );
    assert_eq!(written.2, "brief-style");
    assert_eq!(written.3, crate::mm::memory::MemoryKind::Project);
    assert_eq!(written.4, "prefers numbered sections".len() as u32);
}

#[test]
fn a_memory_write_failure_names_the_intent_the_kernel_authored() {
    let (mut runtime, effect) = agent_awaiting_memory_write();
    let settled = runtime.submit(&failed(
        "in-persist-failed",
        1_700_000_003_000,
        &effect,
        HostEffectFailureKind::StorageUnavailable,
        "the memory store is offline",
    ));
    assert!(settled.published_effects().is_empty());
    assert!(
        observation_kinds(&runtime).contains(&"memory_write_failed"),
        "{:?}",
        observation_kinds(&runtime)
    );
}

#[test]
fn a_memory_recall_enters_context_before_the_turn_resumes() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let queried = runtime.submit(&provider_result(
        "in-memory",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-1",
            crate::context::manager::MEMORY_TOOL_NAME,
            json!({"query": "prior briefs", "top_k": 9}),
        )],
    ));
    let query_effect = sole_effect(&queried);
    let EffectKind::QueryMemory(query) = &query_effect.effect else {
        panic!("expected a memory query");
    };
    assert_eq!(
        query.requested_k, 4,
        "retrieval width is the operation's policy, clamped — a model cannot widen it"
    );
    let effect = query_effect.effect_id.clone();

    let resumed = runtime.submit(&resolved(
        "in-recalls",
        1_700_000_003_000,
        &effect,
        EffectSuccess::MemoryQueried(MemoryQueriedSuccess {
            recalls: vec![MemoryRecall {
                record_ref: MemoryRecordRef::new("rec-1").unwrap(),
                name: "brief-style".to_string(),
                kind: SyscallMemoryKind::Project,
                content: "prefers numbered sections".to_string(),
                score: None,
            }],
        }),
    ));
    assert_eq!(kinds(&resumed), vec![EffectKindTag::CallProvider]);
    assert!(
        history_text(&runtime)
            .iter()
            .any(|line| line.contains("prefers numbered sections")),
        "the recall is in the context the resumed turn renders"
    );
    assert!(
        observation_kinds(&runtime).contains(&"memory_queried"),
        "{:?}",
        observation_kinds(&runtime)
    );
}

#[test]
fn a_memory_query_failure_resumes_the_turn_without_recalls() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let queried = runtime.submit(&provider_result(
        "in-memory",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-1",
            crate::context::manager::MEMORY_TOOL_NAME,
            json!({"query": "prior briefs"}),
        )],
    ));
    let effect = effect_id(queried.step_seq);

    let resumed = runtime.submit(&failed(
        "in-query-failed",
        1_700_000_003_000,
        &effect,
        HostEffectFailureKind::StorageUnavailable,
        "the memory store is offline",
    ));
    assert_eq!(
        kinds(&resumed),
        vec![EffectKindTag::CallProvider],
        "a store that could not answer is not a reason to stall the run"
    );
    assert!(
        observation_kinds(&runtime).contains(&"memory_query_failed"),
        "{:?}",
        observation_kinds(&runtime)
    );
}

#[test]
fn a_page_out_archive_holds_the_continuation_until_the_host_commits_it() {
    let (mut runtime, archive) = agent_awaiting_page_out();
    let EffectKind::ArchivePageOut(published) = &runtime
        .tx
        .pending_effects()
        .find(|effect| effect.effect_id == archive)
        .expect("the archive is pending")
        .effect
        .clone()
    else {
        panic!("expected a page-out effect");
    };

    let resumed = runtime.submit(&resolved(
        "in-archived",
        1_700_000_004_000,
        &archive,
        EffectSuccess::PageOutArchived(PageOutArchivedSuccess {
            receipt: ArchiveReceipt {
                handle_id: published.handle_id.clone(),
                payload_ref: PayloadRef::new("blob-1").unwrap(),
                digest: published.payload.digest.clone(),
                original_size: published.payload.original_size,
            },
        }),
    ));
    assert_eq!(
        kinds(&resumed),
        vec![EffectKindTag::CallProvider],
        "the provider retry the compaction deferred is released by the archive's commit"
    );
    assert!(
        observation_kinds(&runtime).contains(&"page_out_archived"),
        "{:?}",
        observation_kinds(&runtime)
    );
}

#[test]
fn an_archive_receipt_for_another_body_is_refused() {
    let (mut runtime, archive) = agent_awaiting_page_out();
    let fault = runtime.reject(&resolved(
        "in-wrong-archive",
        1_700_000_004_000,
        &archive,
        EffectSuccess::PageOutArchived(PageOutArchivedSuccess {
            receipt: ArchiveReceipt {
                handle_id: HandleId::new("some-other-handle").unwrap(),
                payload_ref: PayloadRef::new("blob-1").unwrap(),
                digest: Digest::new("sha256:".to_string() + &"1".repeat(64)).unwrap(),
                original_size: WireU64::new(1),
            },
        }),
    ));
    assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
}

// fixture: removed-kernel-auto-redispatch
#[test]
fn a_failed_archive_is_abandoned_and_the_run_stays_live() {
    let (mut runtime, archive) = agent_awaiting_page_out();
    let resumed = runtime.submit(&failed(
        "in-archive-failed",
        1_700_000_004_000,
        &archive,
        HostEffectFailureKind::StorageUnavailable,
        "the blob store is offline",
    ));
    assert_eq!(
        kinds(&resumed),
        vec![EffectKindTag::CallProvider],
        "DEC-5 · the archive is abandoned once; the compaction it belongs to already happened, \
             so the run continues degraded rather than dying on a best-effort durability effect"
    );
    assert!(
        observation_kinds(&runtime).contains(&"page_out_archive_failed"),
        "{:?}",
        observation_kinds(&runtime)
    );
}

/// A load outcome that answers no pending `LoadPayload` effect fails closed: since Task 13 the
/// producer is the P1 `PageIn { handle_id }` syscall, and a resolution is only reducible
/// against the effect it actually answers.
#[test]
fn a_payload_load_outcome_is_refused_with_its_reason() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let fault = runtime.reject(&resolved(
        "in-loaded",
        1_700_000_002_000,
        &provider,
        EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
            handle_id: HandleId::new("call-1").unwrap(),
            payload: InlinePayload {
                content: "body".to_string(),
                digest: Digest::new("sha256:".to_string() + &"2".repeat(64)).unwrap(),
                original_size: WireU64::new(4),
            },
        }),
    ));
    // the transaction refuses it first: a provider effect does not accept a payload result
    assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
}

// ----- fixtures the failure vocabulary itself has to satisfy --------------------------------

// fixture: removed-cancellation-as-effect-failure
#[test]
fn the_failure_vocabulary_cannot_express_a_cancellation() {
    for kind in HostEffectFailureKind::ALL {
        let label = kind.as_str();
        assert!(
            !label.contains("cancel"),
            "cancellation is a control-plane fact and only reaches the kernel through \
                 HostControl::Cancel; {label} would give one pending effect two meanings"
        );
    }
    assert_eq!(HostEffectFailureKind::ALL.len(), 6);
}

// ----- arcs the tests above start from ------------------------------------------------------

fn agent_start_with_history(id: &str, at: u64, messages: usize) -> WireEnvelope {
    envelope(
        id,
        at,
        KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Agent(RootAgentEntry {
                task: LogicalTask::new("write the research brief"),
                run_spec: None,
            }),
            initial_context: InitialContext {
                messages: (0..messages)
                    .map(|index| super::super::root::LogicalMessage {
                        role: if index % 2 == 0 {
                            MessageRole::User
                        } else {
                            MessageRole::Assistant
                        },
                        content: format!(
                            "turn {index}: a long enough body that compaction has something \
                                 to reclaim when the prompt stops fitting"
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

/// An agent whose governance policy gates `search` behind an approval.
fn agent_awaiting_approval() -> (Runtime, EffectId) {
    use crate::runtime::kernel::wire::command::{GovernancePolicy, PolicyAction, PolicyRule};

    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.host_effect_support = support_with([EffectKindTag::RequestApproval]);
        config.governance_policy = Some(GovernancePolicy {
            rules: vec![PolicyRule {
                tool_pattern: "search".to_string(),
                action: PolicyAction::AskUser,
            }],
            ..GovernancePolicy::default()
        });
    }));
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    (runtime, sole_effect(&started).effect_id.clone())
}

/// The two-phase skeleton the milestone arcs run on: `collect` unlocks the `search` tool,
/// `write` unlocks the `debug` skill — one of each capability directory, so the projection is
/// exercised in both directions.
fn brief_contract() -> WireVerificationContract {
    WireVerificationContract {
        contract_id: "brief-quality-primary".to_string(),
        phases: vec![
            WireMilestonePhase {
                phase_id: "collect".to_string(),
                unlocks: vec!["search".to_string()],
            },
            WireMilestonePhase {
                phase_id: "write".to_string(),
                unlocks: vec!["debug".to_string()],
            },
        ],
    }
}

fn agent_start_under_contract(id: &str, at: u64, contract_id: &str) -> WireEnvelope {
    envelope(
        id,
        at,
        KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Agent(RootAgentEntry {
                task: LogicalTask::new("write the research brief"),
                run_spec: Some(LogicalAgentSpec {
                    verification_contract_id: Some(contract_id.to_string()),
                    ..LogicalAgentSpec::new("write the research brief")
                }),
            }),
            initial_context: InitialContext::default(),
        }),
    )
}

/// An agent carrying a two-phase milestone contract, declared the canonical way: the
/// operation's `verification_contracts` catalog holds the skeleton and the root run spec
/// references it by id (Task 14 · closes Task 12's SPEC-ISSUE-4, where nothing on the wire
/// could make the kernel ask for a verdict).
fn agent_awaiting_milestone_check() -> (Runtime, EffectId) {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.host_effect_support = support_with([EffectKindTag::EvaluateMilestone]);
        config.verification_contracts = vec![brief_contract()];
    }));
    let started = runtime.submit(&agent_start_under_contract(
        "in-start",
        1_700_000_001_000,
        "brief-quality-primary",
    ));
    (runtime, sole_effect(&started).effect_id.clone())
}

/// An agent that has published a `PersistMemory` effect through the child→parent request path.
fn agent_awaiting_memory_write() -> (Runtime, EffectId) {
    use crate::runtime::kernel::wire::syscall::{MemoryWriteProposal, RequestMemoryWriteRequest};

    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    runtime.submit(&spawned(
        "in-ack",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    let completed = runtime.submit(&child_done_with(
        "in-done",
        1_700_000_002_500,
        "wf-node0",
        "wf-node0:attempt:1",
        vec![SyscallRequest::RequestMemoryWrite(
            RequestMemoryWriteRequest {
                proposal: MemoryWriteProposal {
                    name: "brief-style".to_string(),
                    kind: SyscallMemoryKind::Project,
                    content: "prefers numbered sections".to_string(),
                    description: String::new(),
                    evidence_refs: Vec::new(),
                },
            },
        )],
    ));
    let effect = completed
        .published_effects()
        .iter()
        .find(|effect| effect.tag() == EffectKindTag::PersistMemory)
        .expect("the child's request published a memory write")
        .effect_id
        .clone();
    (runtime, effect)
}

/// An agent whose context overflowed hard enough to compact, so a page-out archive is pending
/// and the provider retry it deferred is waiting behind it.
fn agent_awaiting_page_out() -> (Runtime, EffectId) {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.host_effect_support = support_with([EffectKindTag::ArchivePageOut]);
    }));
    let started = runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));
    let provider = sole_effect(&started).effect_id.clone();
    let compacted = runtime.submit(&provider_overflow(
        "in-overflow",
        1_700_000_002_000,
        &provider,
    ));
    let archive = compacted
        .published_effects()
        .iter()
        .find(|effect| effect.tag() == EffectKindTag::ArchivePageOut)
        .expect("the compaction externalised its archive")
        .effect_id
        .clone();
    (runtime, archive)
}

/// A workflow root with its first node launched and acknowledged, so a live attempt exists.
fn workflow_with_live_child() -> (Runtime, EffectId) {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    let spawn = effect_id(started.step_seq);
    runtime.submit(&spawned("in-ack", 1_700_000_002_000, &spawn, &["wf-node0"]));
    (runtime, spawn)
}

// -----------------------------------------------------------------------------------------
// §7.5 · host control plane, and §7.7 · signal delivery (Task 12b)
// -----------------------------------------------------------------------------------------

fn control(id: &str, at: u64, command: HostCommand) -> WireEnvelope {
    envelope(
        id,
        at,
        KernelInput::HostControl(super::super::envelope::HostControl { command }),
    )
}

fn cancel_with(id: &str, at: u64, reason: CancellationReason) -> WireEnvelope {
    control(
        id,
        at,
        HostCommand::Cancel(CancelCommand {
            reason,
            pending_call_ids: Vec::new(),
        }),
    )
}

fn cancel(id: &str, at: u64) -> WireEnvelope {
    cancel_with(id, at, CancellationReason::User)
}

/// One signal delivery. `delivery` and `signal` are separate arguments precisely because they
/// are separate identities (§7.7).
fn signal_delivery(
    id: &str,
    at: u64,
    delivery: &str,
    attempt: u32,
    signal: LogicalSignal,
) -> WireEnvelope {
    envelope(
        id,
        at,
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::DeliverSignal(DeliverSignal {
                delivery_id: DeliveryId::new(delivery).unwrap(),
                attempt,
                signal,
            }),
        }),
    )
}

fn logical_signal(id: &str, urgency: SignalUrgency) -> LogicalSignal {
    LogicalSignal {
        urgency: Some(urgency),
        ..LogicalSignal::new(SignalId::new(id).unwrap())
    }
}

/// A configuration with a signal policy, so queue capacity and TTL are the operation's own
/// facts rather than a compile-time default.
fn signal_config(queue_max: u32, ttl_ms: Option<u64>) -> WireEnvelope {
    syscall_config_with(|config| {
        config.signal_policy = Some(super::super::command::SignalPolicy {
            queue_max,
            ttl_ms: ttl_ms.map(WireU64::new),
            deadline_escalation: None,
        });
    })
}

#[test]
fn signal_and_timer_waits_wake_only_through_the_canonical_envelope_clock_and_event() {
    use crate::scheduler::tcb::{LogicalDeadline, SignalFilter, WaitCondition, WaitMode, WaitSet};

    let mut signal_runtime = workflow_root_awaiting_first_child();
    signal_runtime
        .driver
        .engine_mut()
        .unwrap()
        .task_table_mut()
        .register_wait_set(
            "wf-node0",
            WaitSet {
                mode: WaitMode::Any,
                conditions: vec![WaitCondition::Signal(SignalFilter("sig-wake".into()))],
            },
        );
    let mut wake_signal = logical_signal("sig-wake", SignalUrgency::Normal);
    wake_signal.target = SignalTarget::Task(super::super::event::TaskTarget {
        task_id: TaskId::new("wf-node0").unwrap(),
    });
    signal_runtime.submit(&signal_delivery(
        "in-signal-wake",
        1_700_000_002_000,
        "delivery-signal-wake",
        1,
        wake_signal,
    ));
    assert!(
        signal_runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get("wf-node0")
            .unwrap()
            .wait_set
            .is_none()
    );

    let (mut timer_runtime, _) = agent_awaiting_provider();
    timer_runtime
        .driver
        .engine_mut()
        .unwrap()
        .task_table_mut()
        .register_wait_set(
            ROOT_TASK_ID,
            WaitSet {
                mode: WaitMode::Any,
                conditions: vec![WaitCondition::Timer(LogicalDeadline(1_700_000_003_000))],
            },
        );
    timer_runtime.submit(&control(
        "in-before-deadline",
        1_700_000_002_000,
        HostCommand::UpdateTask(UpdateTaskCommand {
            update: WireTaskUpdate::default(),
        }),
    ));
    assert!(
        timer_runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(ROOT_TASK_ID)
            .unwrap()
            .wait_set
            .is_some(),
        "an earlier accepted envelope cannot wake the timer"
    );
    timer_runtime.submit(&control(
        "in-at-deadline",
        1_700_000_003_000,
        HostCommand::UpdateTask(UpdateTaskCommand {
            update: WireTaskUpdate::default(),
        }),
    ));
    assert!(
        timer_runtime
            .driver
            .engine()
            .unwrap()
            .task_table()
            .get(ROOT_TASK_ID)
            .unwrap()
            .wait_set
            .is_none(),
        "the journal-owned observed_at_ms is the timer producer"
    );
}

#[test]
fn unsupported_channel_and_resource_external_events_fail_closed_at_decode() {
    let base = serde_json::to_value(signal_delivery(
        "in-unsupported",
        1_700_000_002_000,
        "delivery-unsupported",
        1,
        logical_signal("sig", SignalUrgency::Normal),
    ))
    .unwrap();
    for kind in ["channel_ready", "resource_released"] {
        let mut forged = base.clone();
        forged["input"]["event"] = json!({"kind": kind});
        assert!(
            serde_json::from_value::<WireEnvelope>(forged).is_err(),
            "unsupported ExternalEvent {kind:?} must not decode as a no-op"
        );
    }
}

// fixture: cancel-flows-only-through-host-control
#[test]
fn cancellation_enters_only_through_the_control_plane_and_never_as_a_failure() {
    // an effect failure on the very call a cancel would abandon is a *failed* terminal, and no
    // wire shape lets it become a cancellation
    let (mut runtime, provider) = agent_awaiting_provider();
    let failed_out = runtime.submit(&failed(
        "in-transport-dead",
        1_700_000_002_000,
        &provider,
        HostEffectFailureKind::TransportExhausted,
        "the vendor gave up",
    ));
    assert!(
        matches!(failed_out.terminal(), Some(KernelTerminal::Failed(_))),
        "a host effect failure is never a cancellation (§14.3)"
    );

    // the control plane is the path that produces a cancellation
    let (mut runtime, _) = agent_awaiting_provider();
    let cancelled = runtime.submit(&cancel_with(
        "in-cancel",
        1_700_000_002_000,
        CancellationReason::Deadline,
    ));
    let Some(KernelTerminal::Cancelled(terminal)) = cancelled.terminal() else {
        panic!(
            "expected a cancelled terminal, got {:?}",
            cancelled.terminal()
        );
    };
    assert_eq!(
        terminal.reason,
        CancellationReason::Deadline,
        "the reason is the host's, not the loop's internal user-abort"
    );
    assert!(
        observation_kinds(&runtime).contains(&"operation_cancelled"),
        "{:?}",
        observation_kinds(&runtime)
    );

    // identity comes from the envelope alone: a cancel that repeats it does not decode, and a
    // cancel addressed at another operation never reaches the driver
    assert!(
        serde_json::from_value::<CancelCommand>(
            json!({ "reason": "user", "operation_id": "op-driver-1" })
        )
        .is_err(),
        "the envelope owns the operation id (§7.5)"
    );
    let (mut runtime, _) = agent_awaiting_provider();
    let mut foreign = cancel("in-foreign", 1_700_000_002_000);
    foreign.operation_id = OperationId::new("op-someone-else").unwrap();
    assert_eq!(
        runtime.reject(&foreign).code,
        KernelFaultCode::OperationMismatch
    );
}

// fixture: cancel-order-is-downstream-first
#[test]
fn cancellation_settles_every_downstream_wait_before_it_commits_the_root_terminal() {
    let (mut runtime, _) = workflow_with_live_child();
    assert!(
        runtime.driver.attempts.contains_key("wf-node0"),
        "the arc starts with a live child attempt"
    );

    let cancelled = runtime.submit(&cancel("in-cancel", 1_700_000_003_000));

    // downstream first, inside this one transition
    assert_eq!(
        runtime
            .driver
            .engine()
            .and_then(|engine| engine.task_lifecycle("wf-node0"))
            .map(TaskLifecycle::is_terminal),
        Some(true),
        "a running child is settled by the same step that cancels its parent"
    );
    assert!(
        runtime.driver.attempts.is_empty(),
        "a settled attempt is spent, so nothing downstream can be resumed by a late completion"
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        Vec::<EffectKindTag>::new(),
        "§11.1 · the cancelling step leaves nothing waiting on the host"
    );

    // and the root terminal is what that same step committed
    assert!(matches!(
        cancelled.terminal(),
        Some(KernelTerminal::Cancelled(_))
    ));
    assert!(
        cancelled.published_effects().is_empty(),
        "§7.12 · effects or a terminal, never both"
    );

    // the child's late completion cannot revive the operation
    assert_eq!(
        runtime
            .reject(&child_done(
                "in-late-done",
                1_700_000_004_000,
                "wf-node0",
                "finished anyway"
            ))
            .code,
        KernelFaultCode::InvalidLifecycle,
    );
}

// fixture: cancel-is-idempotent
#[test]
fn a_second_cancellation_commits_no_second_terminal() {
    let (mut runtime, _) = agent_awaiting_provider();
    let first = runtime.submit(&cancel("in-cancel", 1_700_000_002_000));
    let head = runtime.tx.head().map(|head| head.step_seq);

    // a re-issued cancellation under a fresh input id replays the record that already
    // committed the terminal (§18.3) — no second record, no second terminal, no new effect
    let replay = runtime.prepare(&cancel("in-cancel-again", 1_700_000_003_000));
    assert_eq!(replay.step_seq(), Some(first.step_seq));
    assert!(
        replay.record().unwrap().record_digest() == first.record.record_digest(),
        "the replay names the existing record"
    );
    assert_eq!(
        runtime.tx.head().map(|head| head.step_seq),
        head,
        "a replay moves no head"
    );

    // a cancellation that says something *different* is a conflict, not an overwrite
    assert_eq!(
        runtime
            .reject(&cancel_with(
                "in-cancel-other",
                1_700_000_003_000,
                CancellationReason::LeaseLost
            ))
            .code,
        KernelFaultCode::DuplicateInputConflict,
    );
}

// fixture: cancel-terminal-rejects-all-state-changing-input
#[test]
fn a_terminal_refuses_every_state_changing_input_including_signal_delivery() {
    let (mut runtime, provider) = agent_awaiting_provider();
    runtime.submit(&cancel("in-cancel", 1_700_000_002_000));

    let head = runtime.tx.head().map(|head| head.step_seq);
    let queue_before = signal_queue_depth(&runtime);
    let journal_len = runtime.journal.len();

    for envelope in [
        signal_delivery(
            "in-late-signal",
            1_700_000_003_000,
            "delivery-late",
            1,
            logical_signal("sig-late", SignalUrgency::Critical),
        ),
        child_done("in-late-child", 1_700_000_003_000, "wf-node0", "done"),
        provider_answer("in-late-answer", 1_700_000_003_000, &provider, "too late"),
        agent_start("in-restart", 1_700_000_003_000),
        control(
            "in-late-compact",
            1_700_000_003_000,
            HostCommand::ForceCompact(super::super::command::ForceCompactCommand {}),
        ),
        control(
            "in-late-task",
            1_700_000_003_000,
            HostCommand::UpdateTask(UpdateTaskCommand {
                update: WireTaskUpdate {
                    progress: Some("still going".to_string()),
                    ..WireTaskUpdate::default()
                },
            }),
        ),
    ] {
        let input_id = envelope.input_id.clone();
        assert_eq!(
            runtime.reject(&envelope).code,
            KernelFaultCode::InvalidLifecycle,
            "{input_id} must be refused after the terminal",
        );
    }

    assert_eq!(
        runtime.tx.head().map(|head| head.step_seq),
        head,
        "no step sequence advanced"
    );
    assert_eq!(runtime.journal.len(), journal_len, "nothing was journaled");
    assert_eq!(
        signal_queue_depth(&runtime),
        queue_before,
        "a refused signal never reaches the queue"
    );
    assert!(
        runtime.driver.poison().is_none(),
        "a typed rejection is not a driver failure"
    );
}

fn signal_queue_depth(runtime: &Runtime) -> usize {
    runtime
        .driver
        .engine()
        .map(LoopStateMachine::signal_queue_depth)
        .unwrap_or(0)
}

// fixture: signal-delivery-identity-is-distinct
#[test]
fn delivery_identity_and_signal_identity_are_two_separate_facts() {
    let mut runtime = Runtime::new();
    runtime.submit(&signal_config(8, None));
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    let first = signal_delivery(
        "in-sig-1",
        1_700_000_002_000,
        "delivery-a",
        1,
        LogicalSignal {
            dedupe_key: Some("nightly".to_string()),
            ..logical_signal("sig-nightly", SignalUrgency::Normal)
        },
    );
    runtime.submit(&first);
    assert_eq!(
        dispositions(&runtime),
        vec![(
            "queue".to_string(),
            "sig-nightly".to_string(),
            "delivery-a".to_string(),
            1
        )],
        "the audit fact names the caller's own signal id, never a minted one"
    );

    // the same delivery, retried: the envelope's idempotency key answers it as a replay, so the
    // signal is not disposed of twice
    let depth = signal_queue_depth(&runtime);
    let replay = runtime.prepare(&first);
    assert!(matches!(
        replay,
        super::super::fault::KernelPreparation::Replayed(_)
    ));
    assert_eq!(signal_queue_depth(&runtime), depth);

    // a *new* delivery of the same business signal is a distinct delivery attempt, and the
    // business dedupe key is what stops it becoming a second queued signal
    runtime.submit(&signal_delivery(
        "in-sig-2",
        1_700_000_003_000,
        "delivery-b",
        2,
        LogicalSignal {
            dedupe_key: Some("nightly".to_string()),
            ..logical_signal("sig-nightly", SignalUrgency::Normal)
        },
    ));
    assert_eq!(
        dispositions(&runtime),
        vec![(
            "ignore".to_string(),
            "sig-nightly".to_string(),
            "delivery-b".to_string(),
            2
        )],
        "a redelivery is recognisable as the same signal and a different delivery"
    );
    assert_eq!(
        signal_queue_depth(&runtime),
        depth,
        "and it does not queue a second copy"
    );

    // a delivery that cannot say which attempt it is fails closed
    assert_eq!(
        runtime
            .reject(&signal_delivery(
                "in-sig-0",
                1_700_000_004_000,
                "delivery-c",
                0,
                logical_signal("sig-other", SignalUrgency::Normal),
            ))
            .code,
        KernelFaultCode::MalformedEnvelope,
    );
}

// fixture: signal-admission-uses-accepted-time
#[test]
fn signal_ttl_is_measured_from_the_accepted_envelope_time() {
    let mut runtime = Runtime::new();
    runtime.submit(&signal_config(8, Some(60_000)));
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    // a source timestamp far older than the TTL — if admission read it, this signal would be
    // born expired
    runtime.submit(&signal_delivery(
        "in-sig-1",
        1_700_000_002_000,
        "delivery-a",
        1,
        LogicalSignal {
            source_timestamp_ms: Some(WireU64::new(1_600_000_000_000)),
            ..logical_signal("sig-stale-source", SignalUrgency::Normal)
        },
    ));
    assert_eq!(
        dispositions(&runtime)
            .iter()
            .map(|(disposition, ..)| disposition.clone())
            .collect::<Vec<_>>(),
        vec!["queue".to_string()],
        "admission uses the accepted envelope time; the source timestamp is metadata"
    );
    assert_eq!(signal_queue_depth(&runtime), 1);

    // and expiry is measured on the same clock: the next accepted input is far enough past the
    // *accepted* time of the first signal to expire it
    runtime.submit(&signal_delivery(
        "in-sig-2",
        1_700_000_200_000,
        "delivery-b",
        1,
        logical_signal("sig-fresh", SignalUrgency::Normal),
    ));
    assert!(
        observation_kinds(&runtime).contains(&"signal_expired"),
        "{:?}",
        observation_kinds(&runtime)
    );
    assert_eq!(
        signal_queue_depth(&runtime),
        1,
        "the stale signal left, the fresh one stayed"
    );
}

// fixture: signal-target-is-operation-or-task
#[test]
fn a_signal_addresses_the_operation_or_one_of_its_own_tasks() {
    let (mut runtime, _) = workflow_with_live_child();

    // the operation itself
    runtime.submit(&signal_delivery(
        "in-sig-op",
        1_700_000_003_000,
        "delivery-a",
        1,
        logical_signal("sig-op", SignalUrgency::Normal),
    ));
    assert_eq!(dispositions(&runtime).len(), 1);

    // one of its own logical tasks
    runtime.submit(&signal_delivery(
        "in-sig-task",
        1_700_000_004_000,
        "delivery-b",
        1,
        LogicalSignal {
            target: SignalTarget::Task(super::super::event::TaskTarget {
                task_id: TaskId::new("wf-node0").unwrap(),
            }),
            ..logical_signal("sig-task", SignalUrgency::Normal)
        },
    ));
    assert_eq!(dispositions(&runtime).len(), 1);

    // a task this operation does not have
    assert_eq!(
        runtime
            .reject(&signal_delivery(
                "in-sig-ghost",
                1_700_000_005_000,
                "delivery-c",
                1,
                LogicalSignal {
                    target: SignalTarget::Task(super::super::event::TaskTarget {
                        task_id: TaskId::new("ghost").unwrap(),
                    }),
                    ..logical_signal("sig-ghost", SignalUrgency::Normal)
                },
            ))
            .code,
        KernelFaultCode::InvalidAuthority,
    );

    // a host session is not an address, and the wire has no slot for one
    assert!(
        serde_json::from_value::<SignalTarget>(
            json!({ "kind": "task", "task_id": "wf-node0", "session_id": "sess-1" })
        )
        .is_err(),
        "host session identity does not enter the event (§7.7)"
    );
}

// fixture: signal-target-is-operation-or-task
#[test]
fn a_full_signal_queue_drops_by_policy_and_leaves_an_audit_fact() {
    let mut runtime = Runtime::new();
    runtime.submit(&signal_config(1, None));
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    runtime.submit(&signal_delivery(
        "in-sig-1",
        1_700_000_002_000,
        "delivery-a",
        1,
        logical_signal("sig-1", SignalUrgency::Normal),
    ));
    runtime.submit(&signal_delivery(
        "in-sig-2",
        1_700_000_003_000,
        "delivery-b",
        1,
        logical_signal("sig-2", SignalUrgency::Normal),
    ));
    assert_eq!(
        dispositions(&runtime)
            .iter()
            .map(|(disposition, ..)| disposition.clone())
            .collect::<Vec<_>>(),
        vec!["dropped".to_string()],
        "the configured capacity decides, and the loss is an audit fact"
    );
    assert_eq!(signal_queue_depth(&runtime), 1);
}

// fixture: signal-disposition-is-a-fact
#[test]
fn a_signal_disposition_is_a_fact_and_only_a_preemption_asks_the_host_for_anything() {
    // an ordinary signal: an audit fact and nothing to execute
    let mut runtime = Runtime::new();
    runtime.submit(&signal_config(8, None));
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let queued = runtime.submit(&signal_delivery(
        "in-sig-normal",
        1_700_000_002_000,
        "delivery-a",
        1,
        logical_signal("sig-normal", SignalUrgency::Normal),
    ));
    assert!(
        queued.published_effects().is_empty(),
        "queueing is a fact; it asks the host for nothing"
    );
    assert!(observation_kinds(&runtime).contains(&"signal_delivery_disposed"));

    // an urgent one that must stop running children *is* a host action, published as an effect
    let (mut runtime, _) = workflow_with_live_child();
    let interrupted = runtime.submit(&signal_delivery(
        "in-sig-critical",
        1_700_000_003_000,
        "delivery-b",
        1,
        logical_signal("sig-critical", SignalUrgency::Critical),
    ));
    assert_eq!(kinds(&interrupted), vec![EffectKindTag::PreemptTasks]);
    assert!(
        !observation_kinds(&runtime).contains(&"agent_preempted"),
        "the preemption is requested here, not committed: {:?}",
        observation_kinds(&runtime)
    );

    // only the host's resolution commits it
    let preempt = effect_id(interrupted.step_seq);
    runtime.submit(&resolved(
        "in-preempted",
        1_700_000_004_000,
        &preempt,
        EffectSuccess::TasksPreempted(super::super::effect::TasksPreemptedSuccess {
            attempts: vec![super::super::effect::TaskPreemptOutcome {
                task_id: TaskId::new("wf-node0").unwrap(),
                attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                outcome: super::super::effect::TaskPreemptStatus::Preempted(
                    super::super::effect::TaskPreempted {},
                ),
            }],
        }),
    ));
    assert!(
        observation_kinds(&runtime).contains(&"agent_preempted"),
        "{:?}",
        observation_kinds(&runtime)
    );
}

#[test]
fn an_operation_that_can_be_interrupted_is_one_that_can_stop_its_children() {
    // DEC-8 · the driver's own pre-check for `preempt_tasks` support before a critical signal
    // routes can never fire, and this is why: an operation that declares spawn capacity is
    // refused at genesis unless it also declares that it can stop what it started. The
    // guarantee lives at config time, so the signal path cannot plan an effect the host would
    // have to refuse.
    let mut runtime = Runtime::new();
    let fault = runtime.reject(&syscall_config_with(|config| {
        config.host_effect_support = HostEffectSupport::new([
            EffectKindTag::CallProvider,
            EffectKindTag::ExecuteTools,
            EffectKindTag::LoadPayload,
            EffectKindTag::SpawnTasks,
            EffectKindTag::PersistMemory,
            EffectKindTag::QueryMemory,
        ]);
    }));
    assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
    assert!(fault.message.contains("preempt_tasks"), "{fault:?}");
}

// ----- §7.7 · escalate_after_ms (Task 14, adjudication §5n item 1) --------------------------

/// A signal config with deadline escalation switched on.
fn escalating_signal_config(queue_max: u32) -> WireEnvelope {
    syscall_config_with(|config| {
        config.signal_policy = Some(super::super::command::SignalPolicy {
            queue_max,
            ttl_ms: None,
            deadline_escalation: Some(true),
        });
    })
}

fn signal_escalating_after(id: &str, urgency: SignalUrgency, after_ms: u64) -> LogicalSignal {
    LogicalSignal {
        escalate_after_ms: Some(WireU64::new(after_ms)),
        ..logical_signal(id, urgency)
    }
}

#[test]
fn a_due_escalation_raises_urgency_one_tier_and_is_anchored_to_the_accepted_time() {
    // A `low` signal is only observed. The same signal with a due `escalate_after_ms` is
    // `normal`, which queues for the next turn boundary — one tier, exactly.
    let mut runtime = Runtime::new();
    runtime.submit(&escalating_signal_config(8));
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    runtime.submit(&signal_delivery(
        "in-sig-due",
        1_700_000_002_000,
        "delivery-a",
        1,
        signal_escalating_after("sig-due", SignalUrgency::Low, 0),
    ));
    assert_eq!(
        dispositions(&runtime)
            .iter()
            .map(|(disposition, ..)| disposition.clone())
            .collect::<Vec<_>>(),
        vec!["queue".to_string()],
        "a due deadline escalated low → normal"
    );

    // Not yet due: the same signal, same accepted time, a deadline in the future. This is what
    // "anchored to the envelope's accepted time" buys — the kernel needs no clock of its own to
    // tell the two apart.
    let mut runtime = Runtime::new();
    runtime.submit(&escalating_signal_config(8));
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    runtime.submit(&signal_delivery(
        "in-sig-waiting",
        1_700_000_002_000,
        "delivery-a",
        1,
        signal_escalating_after("sig-waiting", SignalUrgency::Low, 60_000),
    ));
    assert_eq!(
        dispositions(&runtime)
            .iter()
            .map(|(disposition, ..)| disposition.clone())
            .collect::<Vec<_>>(),
        vec!["observe".to_string()],
        "a deadline that has not come due changes nothing"
    );
}

#[test]
fn escalation_is_inert_unless_the_operation_asked_for_it() {
    // The field is a request; `signal_policy.deadline_escalation` is the operation's consent.
    // Without it the same bytes decode to the same signal and route the same way.
    let mut runtime = Runtime::new();
    runtime.submit(&signal_config(8, None));
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    runtime.submit(&signal_delivery(
        "in-sig",
        1_700_000_002_000,
        "delivery-a",
        1,
        signal_escalating_after("sig-inert", SignalUrgency::Low, 0),
    ));
    assert_eq!(
        dispositions(&runtime)
            .iter()
            .map(|(disposition, ..)| disposition.clone())
            .collect::<Vec<_>>(),
        vec!["observe".to_string()],
        "escalation without the policy is inert"
    );
}

#[test]
fn an_escalation_that_reaches_critical_takes_the_whole_interrupt_arc() {
    // The full arc: a `high` signal that waited long enough becomes `critical`, and critical
    // while children are running is the one disposition that asks the host for something. This
    // is also what the driver's pre-admission `effective_urgency` check has to agree with — it
    // reads the escalated value precisely so a delivery that will publish a `PreemptTasks` is
    // adjudicated against DEC-8 before the router moves.
    let mut runtime = Runtime::new();
    runtime.submit(&escalating_signal_config(8));
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    runtime.submit(&spawned(
        "in-ack",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));

    let interrupted = runtime.submit(&signal_delivery(
        "in-sig-escalated",
        1_700_000_003_000,
        "delivery-a",
        1,
        signal_escalating_after("sig-escalated", SignalUrgency::High, 0),
    ));
    assert_eq!(
        kinds(&interrupted),
        vec![EffectKindTag::PreemptTasks],
        "high + due deadline = critical, and critical while busy preempts"
    );

    // …and without the escalation the same signal is only a soft interrupt: nothing published.
    let mut runtime = Runtime::new();
    runtime.submit(&escalating_signal_config(8));
    let started = runtime.submit(&workflow_start(
        "in-start",
        1_700_000_001_000,
        two_node_spec(),
    ));
    runtime.submit(&spawned(
        "in-ack",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    let soft = runtime.submit(&signal_delivery(
        "in-sig-plain",
        1_700_000_003_000,
        "delivery-a",
        1,
        logical_signal("sig-plain", SignalUrgency::High),
    ));
    assert!(
        soft.published_effects().is_empty(),
        "an unescalated high signal waits for the next boundary"
    );
}

#[test]
fn a_signal_carries_a_duration_not_a_deadline() {
    // DEC-2 · an absolute instant would be a second host clock on the wire, and the same
    // intent would decode to a different deadline on every redelivery. Only the duration
    // spelling exists.
    let signal = signal_escalating_after("sig", SignalUrgency::Normal, 30_000);
    let value = serde_json::to_value(&signal).unwrap();
    assert_eq!(value["escalate_after_ms"], json!("30000"));
    for banned in ["deadline_ms", "escalate_at_ms", "expires_at_ms", "now_ms"] {
        assert!(
            value.get(banned).is_none(),
            "a signal must not carry {banned}"
        );
        let mut with_instant = value.clone();
        with_instant
            .as_object_mut()
            .unwrap()
            .insert(banned.to_string(), json!("1700000000000"));
        assert!(
            serde_json::from_value::<LogicalSignal>(with_instant).is_err(),
            "{banned} must not decode"
        );
    }

    // and the field is optional: a signal that never escalates is the common case
    let plain = serde_json::to_value(logical_signal("sig", SignalUrgency::Normal)).unwrap();
    assert!(plain.get("escalate_after_ms").is_none());
}

#[test]
fn an_urgent_signal_never_publishes_a_second_provider_request() {
    // DEC-3 · a provider call is already pending, so the interrupt is admitted to the attention
    // partition and read at the next turn boundary rather than re-asking now.
    let (mut runtime, provider) = agent_awaiting_provider();
    let interrupted = runtime.submit(&signal_delivery(
        "in-sig-critical",
        1_700_000_002_000,
        "delivery-a",
        1,
        logical_signal("sig-critical", SignalUrgency::Critical),
    ));
    assert!(
        interrupted.published_effects().is_empty(),
        "a second provider call would be refused by §15.3, so none is planned"
    );
    assert_eq!(
        dispositions(&runtime)
            .iter()
            .map(|(disposition, ..)| disposition.clone())
            .collect::<Vec<_>>(),
        vec!["interrupt".to_string()],
        "the disposition reports what actually happened"
    );

    // and the pending call still resolves normally, carrying the interrupt into that turn
    let answered = runtime.submit(&provider_result(
        "in-answer",
        1_700_000_003_000,
        &provider,
        vec![tool_call("call-1", "search", json!({"q": "now what"}))],
    ));
    assert_eq!(kinds(&answered), vec![EffectKindTag::ExecuteTools]);
}

// -----------------------------------------------------------------------------------------
// §7.5 · the remaining host commands
// -----------------------------------------------------------------------------------------

#[test]
fn a_host_task_update_and_a_model_task_update_share_a_payload_but_not_an_authority() {
    let (mut runtime, provider) = agent_awaiting_provider();
    let committed = runtime.submit(&control(
        "in-host-plan",
        1_700_000_002_000,
        HostCommand::UpdateTask(UpdateTaskCommand {
            update: WireTaskUpdate {
                plan: Some(vec!["collect".to_string(), "write".to_string()]),
                progress: Some("host set the plan".to_string()),
                ..WireTaskUpdate::default()
            },
        }),
    ));
    assert!(
        committed.published_effects().is_empty() && committed.terminal().is_none(),
        "a control command changes kernel state and publishes nothing"
    );
    assert_eq!(
        runtime
            .driver
            .engine()
            .map(|engine| engine.ctx.partitions.task_state.progress.clone()),
        Some("host set the plan".to_string()),
    );

    // the model reaches the same mutation through the P1 syscall path, which *is* gated —
    // it must be a call the turn advertised, attributed to a derived caller
    let acted = runtime.submit(&provider_result(
        "in-model-plan",
        1_700_000_003_000,
        &provider,
        vec![tool_call(
            "call-1",
            "update_plan",
            json!({ "progress": "model set the plan" }),
        )],
    ));
    assert_eq!(kinds(&acted), vec![EffectKindTag::CallProvider]);
    assert_eq!(
        runtime
            .driver
            .engine()
            .map(|engine| engine.ctx.partitions.task_state.progress.clone()),
        Some("model set the plan".to_string()),
    );
}

#[test]
fn seeded_and_mutated_knowledge_enter_the_same_partition() {
    let (mut runtime, _) = agent_awaiting_provider();
    runtime.submit(&control(
        "in-seed",
        1_700_000_002_000,
        HostCommand::SeedKnowledge(SeedKnowledgeCommand {
            entries: vec![super::super::root::KnowledgeEntry {
                content: "the house style forbids bullet lists".to_string(),
                key: Some("style".to_string()),
                tokens: Some(9),
                pinned: true,
            }],
        }),
    ));
    assert!(
        knowledge_text(&runtime)
            .iter()
            .any(|text| text.contains("house style")),
        "{:?}",
        knowledge_text(&runtime)
    );

    // the keyed removal half is boundary-deferred, so it is accepted here and swept later
    let committed = runtime.submit(&control(
        "in-knowledge",
        1_700_000_003_000,
        HostCommand::ApplyKnowledgeMutation(ApplyKnowledgeMutationCommand {
            mutation: super::super::command::KnowledgeMutation {
                upsert: vec![super::super::root::KnowledgeEntry {
                    content: "cite at least three sources".to_string(),
                    key: Some("sources".to_string()),
                    tokens: Some(6),
                    pinned: false,
                }],
                remove: vec!["style".to_string(), "never-seen".to_string()],
            },
        }),
    ));
    assert!(committed.published_effects().is_empty());
    assert!(
        knowledge_text(&runtime)
            .iter()
            .any(|text| text.contains("three sources")),
        "{:?}",
        knowledge_text(&runtime)
    );
}

fn knowledge_text(runtime: &Runtime) -> Vec<String> {
    runtime
        .driver
        .engine()
        .map(|engine| {
            engine
                .ctx
                .partitions
                .knowledge
                .messages()
                .map(message_text)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_capability_patch_mounts_and_unmounts_in_one_step() {
    use super::super::root::{CapabilityGrant, CapabilityKind as WireCapabilityKind};

    let (mut runtime, _) = agent_awaiting_provider();
    runtime.submit(&control(
        "in-mount",
        1_700_000_002_000,
        HostCommand::ApplyCapabilityPatch(ApplyCapabilityPatchCommand {
            patch: super::super::command::CapabilityPatch {
                mount: vec![CapabilityGrant {
                    kind: WireCapabilityKind::McpServer,
                    id: "github".to_string(),
                    description: Some("issue and PR access".to_string()),
                }],
                unmount: Vec::new(),
            },
        }),
    ));
    assert!(observation_kinds(&runtime).contains(&"capability_changed"));
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .capabilities
            .capabilities()
            .iter()
            .any(|capability| capability.id == "github")
    );

    // withdrawing something already absent errs open, so a retry is safe
    runtime.submit(&control(
        "in-unmount",
        1_700_000_003_000,
        HostCommand::ApplyCapabilityPatch(ApplyCapabilityPatchCommand {
            patch: super::super::command::CapabilityPatch {
                mount: Vec::new(),
                unmount: vec![
                    super::super::root::CapabilityRef {
                        kind: WireCapabilityKind::McpServer,
                        id: "github".to_string(),
                    },
                    super::super::root::CapabilityRef {
                        kind: WireCapabilityKind::McpServer,
                        id: "never-mounted".to_string(),
                    },
                ],
            },
        }),
    ));
    assert!(
        !runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .capabilities
            .capabilities()
            .iter()
            .any(|capability| capability.id == "github")
    );
}

#[test]
fn a_skill_swap_is_atomic_and_refuses_a_name_outside_the_catalog() {
    let (mut runtime, _) = agent_awaiting_provider();
    let before = runtime.driver.engine().unwrap().ctx.active_skills.len();

    // one undeclared name refuses the whole swap — nothing is half-applied
    assert_eq!(
        runtime
            .reject(&control(
                "in-bad-skill",
                1_700_000_002_000,
                HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
                    activate: vec![
                        super::super::command::SkillActivation {
                            name: "debug".to_string(),
                            lease_turns: None,
                        },
                        super::super::command::SkillActivation {
                            name: "invented".to_string(),
                            lease_turns: None,
                        },
                    ],
                    deactivate: Vec::new(),
                }),
            ))
            .code,
        KernelFaultCode::InvalidConfig,
    );
    assert_eq!(
        runtime.driver.engine().unwrap().ctx.active_skills.len(),
        before,
        "a refused swap left nothing behind"
    );

    runtime.submit(&control(
        "in-skill",
        1_700_000_002_000,
        HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
            activate: vec![super::super::command::SkillActivation {
                name: "debug".to_string(),
                lease_turns: Some(2),
            }],
            deactivate: Vec::new(),
        }),
    ));
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .active_skills
            .iter()
            .any(|(skill, _)| skill == "debug")
    );
}

#[test]
fn skill_capability_grants_must_attenuate_root_authority_before_activation() {
    use crate::types::capability::{
        ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
        ResourceSelector,
    };

    let root_grant = Capability {
        id: CapabilityId("root-read-src".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/src/**".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: true,
        issuer: Principal("root".into()),
    };
    let overbroad_skill_grant = Capability {
        id: CapabilityId("read-repo".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/**".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: false,
        issuer: Principal("skill:debug".into()),
    };

    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.skill_catalog[0].capability_grants = vec![overbroad_skill_grant];
    }));
    runtime.submit(&agent_start_with_capabilities(
        "in-start",
        1_700_000_001_000,
        vec![root_grant],
    ));
    let before = runtime.driver.engine().unwrap().ctx.active_skills.clone();

    let fault = runtime.reject(&control(
        "in-overbroad-skill",
        1_700_000_002_000,
        HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
            activate: vec![super::super::command::SkillActivation {
                name: "debug".to_string(),
                lease_turns: None,
            }],
            deactivate: Vec::new(),
        }),
    ));
    assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
    assert_eq!(runtime.driver.engine().unwrap().ctx.active_skills, before);
}

#[test]
fn model_skill_activation_rejects_overbroad_grants_as_an_audit_fact() {
    use crate::types::capability::{
        ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
        ResourceSelector,
    };

    let root_grant = Capability {
        id: CapabilityId("root-read-src".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/src/**".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: true,
        issuer: Principal("root".into()),
    };
    let overbroad_skill_grant = Capability {
        id: CapabilityId("read-repo".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/**".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: false,
        issuer: Principal("skill:debug".into()),
    };

    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.skill_catalog[0].capability_grants = vec![overbroad_skill_grant];
    }));
    let started = runtime.submit(&agent_start_with_capabilities(
        "in-start",
        1_700_000_001_000,
        vec![root_grant],
    ));
    let provider = sole_effect(&started).effect_id.clone();

    runtime.submit(&provider_result(
        "in-overbroad-skill",
        1_700_000_002_000,
        &provider,
        vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
    ));

    let rejected = rejections(&runtime);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, "skill");
    assert!(rejected[0].2.contains("would widen"));
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .active_skills
            .is_empty()
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::CallProvider]
    );
}

#[test]
fn skill_capability_grants_are_effective_only_for_a_legal_active_skill() {
    use crate::types::capability::{
        ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
        ResourceSelector,
    };

    let root_grant = Capability {
        id: CapabilityId("root-read-src".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/src/**".into()),
        actions: ActionSet(["read".into(), "write".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: true,
        issuer: Principal("root".into()),
    };
    let narrowed_skill_grant = Capability {
        id: CapabilityId("read-utils".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/src/utils/**".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: false,
        issuer: Principal("skill:debug".into()),
    };

    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.skill_catalog[0].capability_grants = vec![narrowed_skill_grant.clone()];
    }));
    runtime.submit(&agent_start_with_capabilities(
        "in-start",
        1_700_000_001_000,
        vec![root_grant],
    ));
    runtime.submit(&control(
        "in-narrowed-skill",
        1_700_000_002_000,
        HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
            activate: vec![super::super::command::SkillActivation {
                name: "debug".to_string(),
                lease_turns: None,
            }],
            deactivate: Vec::new(),
        }),
    ));
    assert_eq!(
        runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .active_skill_capabilities(),
        vec![narrowed_skill_grant]
    );

    runtime.submit(&control(
        "in-deactivate-skill",
        1_700_000_003_000,
        HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
            activate: Vec::new(),
            deactivate: vec!["debug".to_string()],
        }),
    ));
    assert!(
        runtime
            .driver
            .engine()
            .unwrap()
            .ctx
            .active_skill_capabilities()
            .is_empty()
    );
}

#[test]
fn a_live_policy_patch_is_revision_guarded_and_takes_effect() {
    let mut runtime = Runtime::new();
    runtime.submit(&signal_config(8, None));
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    // a stale revision changes nothing at all
    let stale = control(
        "in-stale-policy",
        1_700_000_002_000,
        HostCommand::ApplyPolicyPatch(ApplyPolicyPatchCommand {
            expected_revision: WireU64::new(7),
            patch: super::super::command::LivePolicyPatch::ReplaceSignalPolicy(
                super::super::command::ReplaceSignalPolicy {
                    policy: super::super::command::SignalPolicy {
                        queue_max: 1,
                        ttl_ms: None,
                        deadline_escalation: None,
                    },
                },
            ),
        }),
    );
    let fault = runtime.reject(&stale);
    assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
    assert!(fault.message.contains("revision mismatch"), "{fault:?}");
    assert_eq!(
        runtime
            .driver
            .policy
            .as_ref()
            .map(LivePolicyState::revision),
        Some(WireU64::ZERO),
    );

    // a widening quota is refused rather than clamped
    let widening = control(
        "in-widen",
        1_700_000_002_000,
        HostCommand::ApplyPolicyPatch(ApplyPolicyPatchCommand {
            expected_revision: WireU64::ZERO,
            patch: super::super::command::LivePolicyPatch::TightenResourceQuota(
                super::super::command::TightenResourceQuota {
                    max_workflow_nodes: Some(999),
                    ..Default::default()
                },
            ),
        }),
    );
    assert!(
        runtime
            .reject(&widening)
            .message
            .contains("may only tighten")
    );

    // a well-formed patch applies, advances the revision, and reaches the running engine
    runtime.submit(&control(
        "in-policy",
        1_700_000_002_000,
        HostCommand::ApplyPolicyPatch(ApplyPolicyPatchCommand {
            expected_revision: WireU64::ZERO,
            patch: super::super::command::LivePolicyPatch::ReplaceSignalPolicy(
                super::super::command::ReplaceSignalPolicy {
                    policy: super::super::command::SignalPolicy {
                        queue_max: 1,
                        ttl_ms: None,
                        deadline_escalation: None,
                    },
                },
            ),
        }),
    ));
    assert_eq!(
        runtime
            .driver
            .policy
            .as_ref()
            .map(LivePolicyState::revision),
        Some(WireU64::new(1)),
    );
    assert!(
        observation_kinds(&runtime).contains(&"live_policy_changed"),
        "{:?}",
        observation_kinds(&runtime)
    );

    // the new capacity is the one the router enforces from here on
    runtime.submit(&signal_delivery(
        "in-sig-1",
        1_700_000_003_000,
        "delivery-a",
        1,
        logical_signal("sig-1", SignalUrgency::Normal),
    ));
    runtime.submit(&signal_delivery(
        "in-sig-2",
        1_700_000_004_000,
        "delivery-b",
        1,
        logical_signal("sig-2", SignalUrgency::Normal),
    ));
    assert_eq!(
        dispositions(&runtime)
            .iter()
            .map(|(disposition, ..)| disposition.clone())
            .collect::<Vec<_>>(),
        vec!["dropped".to_string()],
        "the patched queue_max is what the next admission decision reads"
    );
}

#[test]
fn an_absolute_deadline_becomes_the_wall_budget_axis() {
    let (mut runtime, provider) = agent_awaiting_provider();
    // the operation's clock started at the root start
    runtime.submit(&control(
        "in-deadline",
        1_700_000_002_000,
        HostCommand::UpdateDeadline(UpdateDeadlineCommand {
            deadline_ms: Some(WireU64::new(1_700_000_001_500)),
        }),
    ));

    // the axis is read at the one funnel that issues a provider request, so the deadline is
    // seen the next time the loop asks a question rather than mid-flight
    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_003_000,
        &provider,
        vec![tool_call("call-1", "search", json!({ "q": "sources" }))],
    ));
    let results = runtime.submit(&tools_resolved(
        "in-results",
        1_700_000_004_000,
        &effect_id(acted.step_seq),
        &[("call-1", "three sources", false)],
    ));
    assert_eq!(
        kinds(&results),
        vec![EffectKindTag::CallProvider],
        "an exhausted budget still buys exactly one bounded final turn"
    );

    let ended = runtime.submit(&provider_answer(
        "in-answer",
        1_700_000_005_000,
        &effect_id(results.step_seq),
        "half a thought",
    ));
    let Some(KernelTerminal::Agent(agent)) = ended.terminal() else {
        panic!("expected the deadline to end the operation, got {ended:?}");
    };
    assert_eq!(agent.result.termination, WireTermination::Deadline);
}

#[test]
fn a_forced_compaction_publishes_the_archive_it_produced() {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.host_effect_support = support_with([EffectKindTag::ArchivePageOut]);
    }));
    runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));

    let compacted = runtime.submit(&control(
        "in-compact",
        1_700_000_002_000,
        HostCommand::ForceCompact(super::super::command::ForceCompactCommand {}),
    ));
    assert_eq!(kinds(&compacted), vec![EffectKindTag::ArchivePageOut]);
    assert!(observation_kinds(&runtime).contains(&"compressed"));
    assert!(
        compacted
            .step
            .observations
            .iter()
            .any(|observation| matches!(observation, KernelObservation::Compressed { .. })),
        "the committed planned step is the host publication channel for observations"
    );
}

// -----------------------------------------------------------------------------------------
// lifecycle goldens
// -----------------------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kernel-wire")
}

fn golden(name: &str, produced: &Value) -> Value {
    let path = fixture_dir().join(name);
    if std::env::var("BLESS_KERNEL_RECORD_FIXTURES").as_deref() == Ok("1") {
        let mut text = serde_json::to_string_pretty(produced).unwrap();
        text.push('\n');
        fs::write(&path, text).unwrap_or_else(|e| panic!("cannot bless {name}: {e}"));
        return produced.clone();
    }
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing golden {name} ({e}); re-bless with BLESS_KERNEL_RECORD_FIXTURES=1")
    });
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"))
}

fn link(envelope: &WireEnvelope, committed: &CommittedTransition<PlannedStep>) -> Value {
    json!({
        "envelope": serde_json::to_value(envelope).unwrap(),
        "step": serde_json::to_value(&committed.step).unwrap(),
        "record": serde_json::to_value(&committed.record).unwrap(),
    })
}

#[test]
fn golden_lifecycle_agent_root() {
    let mut runtime = Runtime::new();
    let configure_envelope = configure();
    let start_envelope = agent_start("in-start", 1_700_000_001_000);
    let genesis = runtime.submit(&configure_envelope);
    let started = runtime.submit(&start_envelope);

    let produced = json!({
        "description":
            "Configure → atomic agent root start (spec 6, 7.4). Two accepted inputs reach the \
             first provider call: the genesis record freezes the resolved configuration, and \
             the start record's step carries the immutable root kind, the initial execution \
             focus and the one CallProvider effect the transition published.",
        "genesis_digest": genesis.record.record_digest().as_str(),
        "head_digest": started.record.record_digest().as_str(),
        "links": [
            link(&configure_envelope, &genesis),
            link(&start_envelope, &started),
        ],
    });

    let expected = golden("golden_lifecycle_agent_root.json", &produced);
    assert_eq!(produced, expected, "the agent root lifecycle drifted");
    assert_eq!(expected["links"][1]["step"]["root_kind"], json!("agent"));
    assert_eq!(
        expected["links"][1]["step"]["disposition"]["effects"][0]["effect"]["kind"],
        json!("call_provider"),
    );
}

#[test]
fn golden_lifecycle_agent_full_turn() {
    let mut runtime = Runtime::new();
    let configure_envelope = syscall_config();
    let start_envelope = agent_start("in-start", 1_700_000_001_000);
    let genesis = runtime.submit(&configure_envelope);
    let started = runtime.submit(&start_envelope);

    let acted_envelope = provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    );
    let acted = runtime.submit(&acted_envelope);
    let results_envelope = tools_resolved(
        "in-results",
        1_700_000_003_000,
        &effect_id(acted.step_seq),
        &[("call-1", "three sources found", false)],
    );
    let results = runtime.submit(&results_envelope);
    let answer_envelope = provider_answer(
        "in-answer",
        1_700_000_004_000,
        &effect_id(results.step_seq),
        "the brief cites three sources",
    );
    let answered = runtime.submit(&answer_envelope);

    let produced = json!({
        "description":
            "One whole agent turn cycle on the canonical wire (spec 7.9 · Task 12): configure → \
             atomic agent start → provider result carrying a tool call → tool results → final \
             provider answer → agent terminal. Every step after the start is a single \
             `ResolveEffect` input, and each one publishes exactly the next effect the loop is \
             waiting on — until the last, whose disposition is the terminal itself and which \
             publishes nothing.",
        "genesis_digest": genesis.record.record_digest().as_str(),
        "head_digest": answered.record.record_digest().as_str(),
        "links": [
            link(&configure_envelope, &genesis),
            link(&start_envelope, &started),
            link(&acted_envelope, &acted),
            link(&results_envelope, &results),
            link(&answer_envelope, &answered),
        ],
    });

    let expected = golden("golden_lifecycle_agent_full_turn.json", &produced);
    assert_eq!(produced, expected, "the agent turn cycle drifted");
    assert_eq!(
        expected["links"][2]["step"]["disposition"]["effects"][0]["effect"]["kind"],
        json!("execute_tools"),
        "a provider result carrying a host tool call publishes exactly one tool batch",
    );
    assert_eq!(
        expected["links"][3]["step"]["disposition"]["effects"][0]["effect"]["kind"],
        json!("call_provider"),
        "the tool results resume the turn with the next provider call",
    );
    assert_eq!(
        expected["links"][4]["step"]["disposition"]["terminal"]["kind"],
        json!("agent"),
    );
    assert_eq!(
        expected["links"][4]["step"]["disposition"]["effects"],
        Value::Null,
        "§7.12 · effects or a terminal, never both",
    );
}

#[test]
fn golden_lifecycle_workflow_root() {
    let mut runtime = Runtime::new();
    let configure_envelope = configure();
    let start_envelope = workflow_start("in-start", 1_700_000_001_000, two_node_spec());
    let genesis = runtime.submit(&configure_envelope);
    let started = runtime.submit(&start_envelope);

    let ack_envelope = spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    );
    let ack = runtime.submit(&ack_envelope);
    let done_envelope = child_done("in-done-1", 1_700_000_003_000, "wf-node0", "collected");
    let advanced = runtime.submit(&done_envelope);
    let ack2_envelope = spawned(
        "in-ack-2",
        1_700_000_004_000,
        &effect_id(advanced.step_seq),
        &["wf-node1"],
    );
    let ack2 = runtime.submit(&ack2_envelope);
    let done2_envelope = child_done("in-done-2", 1_700_000_005_000, "wf-node1", "written");
    let finished = runtime.submit(&done2_envelope);

    let produced = json!({
        "description":
            "Configure → atomic workflow root start → spawn ack → child completions → workflow \
             terminal (spec 10.1). No LoadWorkflow, no placeholder agent run, and no host \
             CompleteRun: the last committed step's disposition is the terminal itself.",
        "genesis_digest": genesis.record.record_digest().as_str(),
        "head_digest": finished.record.record_digest().as_str(),
        "links": [
            link(&configure_envelope, &genesis),
            link(&start_envelope, &started),
            link(&ack_envelope, &ack),
            link(&done_envelope, &advanced),
            link(&ack2_envelope, &ack2),
            link(&done2_envelope, &finished),
        ],
    });

    let expected = golden("golden_lifecycle_workflow_root.json", &produced);
    assert_eq!(produced, expected, "the workflow root lifecycle drifted");
    assert_eq!(expected["links"][1]["step"]["root_kind"], json!("workflow"));
    assert_eq!(
        expected["links"][1]["step"]["disposition"]["effects"][0]["effect"]["kind"],
        json!("spawn_tasks"),
    );
    assert_eq!(
        expected["links"][5]["step"]["disposition"]["terminal"]["kind"],
        json!("workflow"),
    );
}

#[test]
fn golden_lifecycle_cancel_arc() {
    let mut runtime = Runtime::new();
    let configure_envelope = signal_config(8, None);
    let start_envelope = workflow_start("in-start", 1_700_000_001_000, two_node_spec());
    let genesis = runtime.submit(&configure_envelope);
    let started = runtime.submit(&start_envelope);

    let ack_envelope = spawned(
        "in-ack",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    );
    let ack = runtime.submit(&ack_envelope);

    let signal_envelope = signal_delivery(
        "in-sig",
        1_700_000_003_000,
        "delivery-a",
        1,
        logical_signal("sig-abort", SignalUrgency::Critical),
    );
    let interrupted = runtime.submit(&signal_envelope);

    let preempted_envelope = resolved(
        "in-preempted",
        1_700_000_004_000,
        &effect_id(interrupted.step_seq),
        EffectSuccess::TasksPreempted(super::super::effect::TasksPreemptedSuccess {
            attempts: vec![super::super::effect::TaskPreemptOutcome {
                task_id: TaskId::new("wf-node0").unwrap(),
                attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                outcome: super::super::effect::TaskPreemptStatus::Preempted(
                    super::super::effect::TaskPreempted {},
                ),
            }],
        }),
    );
    let preempted = runtime.submit(&preempted_envelope);

    let cancel_envelope = cancel("in-cancel", 1_700_000_005_000);
    let cancelled = runtime.submit(&cancel_envelope);

    let produced = json!({
        "description":
            "The cancellation arc on the canonical wire (spec 7.5 / 7.7 / 11.1 · Task 12b): \
             configure → workflow root start → spawn ack → a critical signal that preempts the \
             running child → the host's preempt resolution → `HostControl::Cancel`, whose step \
             is the cancelled terminal itself. Two things this chain fixes in place: the only \
             host action a signal can cause is the preemption (its queueing/dropping/expiry are \
             audit facts and publish nothing), and cancellation settles every downstream wait \
             inside the same transition that commits the root terminal — §7.12 admits effects or \
             a terminal and never both, so there is no second round trip in which the operation \
             is neither running nor cancelled.",
        "genesis_digest": genesis.record.record_digest().as_str(),
        "head_digest": cancelled.record.record_digest().as_str(),
        "links": [
            link(&configure_envelope, &genesis),
            link(&start_envelope, &started),
            link(&ack_envelope, &ack),
            link(&signal_envelope, &interrupted),
            link(&preempted_envelope, &preempted),
            link(&cancel_envelope, &cancelled),
        ],
    });

    let expected = golden("golden_lifecycle_cancel_arc.json", &produced);
    assert_eq!(produced, expected, "the cancellation arc drifted");
    assert_eq!(
        expected["links"][3]["step"]["disposition"]["effects"][0]["effect"]["kind"],
        json!("preempt_tasks"),
        "an urgent signal's one host action is stopping the running child",
    );
    assert_eq!(
        expected["links"][5]["step"]["disposition"]["terminal"]["kind"],
        json!("cancelled"),
    );
    assert_eq!(
        expected["links"][5]["step"]["disposition"]["effects"],
        Value::Null,
        "§11.1 · the cancelling step leaves nothing waiting on the host",
    );
    assert_eq!(
        runtime.pending_effect_kinds(),
        Vec::<EffectKindTag>::new(),
        "and the transaction holds no pending effect after it",
    );
}

#[test]
fn golden_lifecycle_external_payload() {
    let mut runtime = Runtime::new();
    let configure_envelope = payload_config();
    let start_envelope = agent_start("in-start", 1_700_000_001_000);
    let genesis = runtime.submit(&configure_envelope);
    let started = runtime.submit(&start_envelope);

    let acted_envelope = provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    );
    let acted = runtime.submit(&acted_envelope);
    let results_envelope = payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &effect_id(acted.step_seq),
        vec![external_payload(
            "call-1",
            body_digest(),
            BODY.len() as u64,
            "the full report body, far la…",
        )],
    );
    let results = runtime.submit(&results_envelope);
    let read_envelope = provider_result(
        "in-read",
        1_700_000_004_000,
        &effect_id(results.step_seq),
        vec![tool_call(
            "call-2",
            READ_RESULT_TOOL_NAME,
            json!({"call_id": "call-1"}),
        )],
    );
    let read = runtime.submit(&read_envelope);
    let loaded_envelope = resolved(
        "in-loaded",
        1_700_000_005_000,
        &effect_id(read.step_seq),
        EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
            handle_id: HandleId::new("call-1").unwrap(),
            payload: InlinePayload {
                content: BODY.to_string(),
                digest: body_digest(),
                original_size: WireU64::new(BODY.len() as u64),
            },
        }),
    );
    let loaded = runtime.submit(&loaded_envelope);

    let produced = json!({
        "description":
            "The §7.10 external-payload arc (Task 13): configure → agent start → a provider \
             turn calling one host tool → an **external** tool result → the model's \
             `read_result` page-in → the loaded body. Read the records: the persisted body \
             appears in exactly one place on this whole chain — the `payload_loaded` outcome \
             the host sent when the kernel asked for it. It is in no effect the kernel \
             published and in no accepted input the kernel did not ask for, which is what \
             §25.10 means by \"large bodies do not enter the journal\". What the kernel holds \
             instead is the reference: an opaque `payload_ref` it never interprets, a digest \
             it checks the restored body against, and a bounded preview that is the only part \
             to occupy context.",
        "genesis_digest": genesis.record.record_digest().as_str(),
        "head_digest": loaded.record.record_digest().as_str(),
        "links": [
            link(&configure_envelope, &genesis),
            link(&start_envelope, &started),
            link(&acted_envelope, &acted),
            link(&results_envelope, &results),
            link(&read_envelope, &read),
            link(&loaded_envelope, &loaded),
        ],
    });

    let expected = golden("golden_lifecycle_external_payload.json", &produced);
    assert_eq!(produced, expected, "the external-payload arc drifted");
    assert_eq!(
        expected["links"][3]["step"]["disposition"]["effects"][0]["effect"]["kind"],
        json!("call_provider"),
        "an external result resumes the turn like any other — the body never comes back out",
    );
    assert_eq!(
        expected["links"][4]["step"]["disposition"]["effects"][0]["effect"]["kind"],
        json!("load_payload"),
        "§7.10 rule 4 · `read_result` reduces to exactly one effect",
    );
    assert_eq!(
        expected["links"][4]["step"]["disposition"]["effects"][0]["effect"]["payload_ref"],
        json!("payload:01J8Y2QK7C4N0V"),
        "the effect hands back the host's own opaque locator, unread and unjoined",
    );

    // The body appears exactly once on the whole chain, and only where the model asked for it.
    //
    // SPEC-ISSUE: §25.10 states flatly that large bodies do not enter the journal, but §7.9's
    // `PayloadLoaded` carries `InlinePayload.content` as an accepted input, and §8.1 makes the
    // accepted input part of the durable record. So a *paged-in* body is journalled by the
    // contract's own construction. The invariant that is actually achievable — and the one this
    // golden pins — is narrower: no body enters on the **ingestion** path, and no body is ever
    // carried by an effect the kernel publishes. Either §25.10 should be scoped to those two,
    // or `PayloadLoaded` needs a shape that resolves an effect without riding the record.
    for (index, link) in expected["links"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            link["envelope"]
                .to_string()
                .contains("clears the inline threshold"),
            index == 5,
            "link {index} disagrees about where a large body may enter",
        );
        // A `call_provider` effect legitimately carries whatever is in context once the model
        // has paged a body in. No effect may hand the body back out to be persisted.
        for effect in link["step"]["disposition"]["effects"]
            .as_array()
            .unwrap_or(&Vec::new())
        {
            assert!(
                !effect.to_string().contains("clears the inline threshold")
                    || effect["effect"]["kind"] == json!("call_provider"),
                "link {index} hands the body back out in a {} effect",
                effect["effect"]["kind"],
            );
        }
    }
    for record in &runtime.journal {
        let input = serde_json::to_string(&record.normalized_input().unwrap()).unwrap();
        assert_eq!(
            input.contains("clears the inline threshold"),
            record.step_seq().get() == 5,
            "only the page-in the model asked for may make a body durable, got step {}",
            record.step_seq()
        );
    }
}

// -----------------------------------------------------------------------------------------
// §12 · logical checkpoint goldens
// -----------------------------------------------------------------------------------------

/// Drive one whole agent turn and stop mid-flight, so the checkpoint has something to hold:
/// a live pending effect, a replay ledger, a task table and a live policy.
fn agent_mid_turn() -> Runtime {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    ));
    runtime.submit(&tools_resolved(
        "in-results",
        1_700_000_003_000,
        &effect_id(acted.step_seq),
        &[("call-1", "three sources found", false)],
    ));
    runtime
}

#[test]
fn checkpoint_restore_preserves_partial_durable_wait_set_progress() {
    use crate::scheduler::tcb::{WaitCondition, WaitMode, WaitSet};
    use crate::scheduler::wait_index::WaitKey;

    let mut runtime = agent_mid_turn();
    let first = EffectId::new("wait-effect-1").unwrap();
    let second = EffectId::new("wait-effect-2").unwrap();
    let table = runtime.driver.engine_mut().unwrap().task_table_mut();
    table.register_wait_set(
        ROOT_TASK_ID,
        WaitSet {
            mode: WaitMode::All,
            conditions: vec![
                WaitCondition::Effect(first.clone()),
                WaitCondition::Effect(second.clone()),
            ],
        },
    );
    assert!(table.notify(&WaitKey::Effect(first)).is_empty());

    let checkpoint = runtime.checkpoint().decode().expect("checkpoint verifies");
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
    let restored_table = restored.driver.engine_mut().unwrap().task_table_mut();
    assert_eq!(
        restored_table
            .get(ROOT_TASK_ID)
            .unwrap()
            .wait_set
            .as_ref()
            .unwrap()
            .satisfied
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        restored_table.notify(&WaitKey::Effect(second)),
        vec![crate::scheduler::tcb::TaskId::from(ROOT_TASK_ID)]
    );
    assert_eq!(
        restored_table.get(ROOT_TASK_ID).unwrap().state,
        TaskLifecycle::Ready
    );
}

#[test]
fn golden_checkpoint_agent_turn() {
    let runtime = agent_mid_turn();
    let candidate = runtime.checkpoint();
    let checkpoint = candidate.decode().expect("the candidate blob verifies");

    let produced = json!({
        "description":
            "A full-state logical checkpoint taken mid-turn (spec 12.1, 12.3). base == through \
             == the durable head, so the bounded tail is empty and a restore replays nothing \
             before the post-checkpoint records. The logical state is partitioned four ways and \
             the header repeats none of it: the pending `call_provider` effect, the replay \
             ledger and the terminal slot live in `transition`, the task table in `scheduler`, \
             the handle table in `context_vm`, the live policy and the provider-tool causation \
             in `syscall`.",
        "through_step_seq": candidate.through_step_seq.to_string(),
        "covered_head": candidate.covered_head.as_str(),
        "state_digest": candidate.state_digest.as_str(),
        "ack_token": candidate.ack_token.as_str(),
        "checkpoint": serde_json::to_value(&checkpoint).unwrap(),
    });

    let expected = golden("golden_checkpoint_agent_turn.json", &produced);
    assert_eq!(produced, expected, "the logical checkpoint drifted");
    assert!(expected["checkpoint"].get("checkpoint_version").is_none());
    assert!(expected["checkpoint"].get("abi_version").is_none());
    assert_eq!(
        expected["checkpoint"]["base_step_seq"], expected["checkpoint"]["through_step_seq"],
        "a full-state candidate carries no tail",
    );
    assert_eq!(expected["checkpoint"]["tail_inputs"], json!([]));
    assert_eq!(
        expected["checkpoint"]["logical_state"]["transition"]["pending_effects"][0]["effect"]["kind"],
        json!("call_provider"),
        "the effect the operation is waiting on is inside the checkpoint, not beside it",
    );
}

/// The incremental form of §12.1: an older logical state plus the canonical inputs that carry
/// it forward to the covered head. This is exactly the shape a Task 16 rebase produces, and
/// pinning it now is what stops the tail contract from being invented twice.
#[test]
fn golden_checkpoint_bounded_tail() {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));

    // the state at the root start — the base a later rebase keeps
    let base = runtime
        .checkpoint()
        .decode()
        .expect("the base candidate verifies");

    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    ));
    let results = runtime.submit(&tools_resolved(
        "in-results",
        1_700_000_003_000,
        &effect_id(acted.step_seq),
        &[("call-1", "three sources found", false)],
    ));
    runtime.submit(&provider_answer(
        "in-answer",
        1_700_000_004_000,
        &effect_id(results.step_seq),
        "the brief cites three sources",
    ));

    let tail: Vec<CanonicalInput> = runtime.journal[2..]
        .iter()
        .map(|record| CanonicalInput::from_record(record).expect("a record projects"))
        .collect();
    // The transaction is the authority on what the tail still holds; harvesting from the
    // journal and asking the transaction must give the same answer, or a rebase built from
    // either source would produce a different checkpoint.
    assert_eq!(
        runtime
            .tx
            .tail_inputs()
            .into_iter()
            .filter(|entry| entry.step_seq.get() > base.through_step_seq().get())
            .collect::<Vec<_>>(),
        tail,
        "the transaction's own tail and the journal agree on (base, through]",
    );
    let rebased = runtime
        .tx
        .checkpoint_rebase(
            &CheckpointBoundary {
                through_step_seq: base.through_step_seq(),
                covered_head: base.covered_transaction_head_digest().clone(),
            },
            base.logical_state().clone(),
        )
        .expect("an older state plus its exact tail is a checkpoint")
        .decode()
        .expect("the rebase blob verifies");

    let produced = json!({
        "description":
            "The incremental form of a logical checkpoint (spec 12.1, 12.2): `logical_state` is \
             the state after `base_step_seq`, and `tail_inputs` covers (base, through] exactly \
             — no hole, no duplicate, nothing outside the range. A restore replays this tail on \
             top of the state, then continues with the journal records after `through_step_seq`. \
             The state digest is byte-identical to the base checkpoint's, because the state is \
             the same state; only the tail and the header moved.",
        "base_state_digest": base.state_digest().as_str(),
        "checkpoint": serde_json::to_value(&rebased).unwrap(),
    });

    let expected = golden("golden_checkpoint_bounded_tail.json", &produced);
    assert_eq!(produced, expected, "the bounded-tail checkpoint drifted");
    assert_eq!(
        expected["checkpoint"]["state_digest"], expected["base_state_digest"],
        "a rebase carries the base state forward untouched",
    );
    assert_eq!(
        expected["checkpoint"]["base_step_seq"],
        json!("1"),
        "the base is the root start",
    );
    assert_eq!(expected["checkpoint"]["through_step_seq"], json!("4"));
    let tail = expected["checkpoint"]["tail_inputs"].as_array().unwrap();
    assert_eq!(
        tail.iter()
            .map(|entry| entry["step_seq"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["2", "3", "4"],
        "(1, 4] is exactly steps 2, 3 and 4",
    );
}

/// §12.3 rule 1 · a candidate is a read. Appends continue, and the candidate that was handed
/// out still describes the prefix it was taken over.
#[test]
fn a_candidate_neither_blocks_nor_is_invalidated_by_later_appends() {
    let mut runtime = agent_mid_turn();
    let candidate = runtime.checkpoint();
    let before = runtime.tx.head().expect("a head");
    assert_eq!(candidate.through_step_seq, before.step_seq);

    let results_effect = effect_id(before.step_seq);
    runtime.submit(&provider_answer(
        "in-answer",
        1_700_000_004_000,
        &results_effect,
        "the brief cites three sources",
    ));

    let after = runtime.tx.head().expect("a head");
    assert_ne!(after.step_seq, before.step_seq, "the journal moved on");
    assert_eq!(
        candidate.through_step_seq, before.step_seq,
        "the candidate still covers the prefix it was taken over",
    );
    candidate
        .decode()
        .expect("and it still verifies after the journal moved")
        .verify_belongs_to(&operation(), runtime.journal[0].record_digest())
        .expect("it is still this operation's checkpoint");
}

// -----------------------------------------------------------------------------------------
// §12.2 · bounded-tail restore
// -----------------------------------------------------------------------------------------

/// Drive an operation to a fixed point through a fixed envelope sequence.
///
/// One function so the two sides of a differential are *the same* sequence by construction
/// rather than by two copies that agree today. `stop_after` is how many envelopes to submit.
fn drive(runtime: &mut Runtime, envelopes: &[WireEnvelope]) {
    for envelope in envelopes {
        runtime.submit(envelope);
    }
}

/// The canonical agent turn, as a list of envelopes.
///
/// Deterministic in every field a record digests: ids, clocks and payloads are literals, and
/// the effect ids are derived from the step sequence exactly as the kernel derives them. That is
/// what makes "byte-identical" a checkable claim rather than a hope.
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

fn digests(records: &[KernelRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| record.record_digest().to_string())
        .collect()
}

/// The whole observable surface of a runtime, as bytes.
///
/// Everything a differential must compare and nothing that is allowed to differ: the state
/// digest a checkpoint would take, the pending effect identities, the terminal, and the head.
/// If a restored runtime matches an uninterrupted one on all four *and* keeps producing the same
/// records, "behaves identically" is not a judgement call.
fn surface(runtime: &Runtime) -> Value {
    json!({
        "head": runtime.tx.head().map(|head| json!({
            "digest": head.digest.as_str(),
            "step_seq": head.step_seq.to_string(),
        })),
        "lifecycle": format!("{:?}", runtime.tx.lifecycle()),
        "pending_effects": runtime
            .tx
            .pending_effects()
            .map(|effect| serde_json::to_value(effect).unwrap())
            .collect::<Vec<_>>(),
        "terminal": runtime.tx.terminal().map(|t| serde_json::to_value(t).unwrap()),
        "logical_state": serde_json::to_value(
            runtime
                .tx
                .transition_state_for_restore(
                    runtime.driver.root_kind(),
                    runtime.driver.focus().cloned(),
                )
                .expect("a configured runtime has a transition state"),
        )
        .unwrap(),
        "context_vm": serde_json::to_value(
            runtime.driver.project_logical_state().context_vm
        ).unwrap(),
        "scheduler": serde_json::to_value(
            runtime.driver.project_logical_state().scheduler
        ).unwrap(),
    })
}

/// Task 16b · a checkpoint owns the active DAG and every child process identity needed to
/// continue it. Restoring while the first node is live must neither forget the second node nor
/// rebuild the child under different role/isolation/inheritance.
#[test]
fn an_active_workflow_and_its_child_restore_to_the_same_completion() {
    let (mut uninterrupted, _) = workflow_with_live_child();
    let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);

    let first_done = child_done(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "sources collected",
    );
    let uninterrupted_next = uninterrupted.submit(&first_done);
    let restored_next = restored.submit(&first_done);
    assert_eq!(
        restored_next, uninterrupted_next,
        "the restored DAG schedules the same second node",
    );

    let uninterrupted_spawn = effect_id(uninterrupted_next.step_seq);
    let restored_spawn = effect_id(restored_next.step_seq);
    uninterrupted.submit(&spawned(
        "in-ack-2",
        1_700_000_004_000,
        &uninterrupted_spawn,
        &["wf-node1"],
    ));
    restored.submit(&spawned(
        "in-ack-2",
        1_700_000_004_000,
        &restored_spawn,
        &["wf-node1"],
    ));
    let second_done = child_done("in-done-2", 1_700_000_005_000, "wf-node1", "brief written");
    uninterrupted.submit(&second_done);
    restored.submit(&second_done);

    assert_eq!(
        surface(&restored),
        surface(&uninterrupted),
        "workflow state and child permission identity are reversible",
    );
}

/// Task 16b · queued signal source state and the router's business-dedupe memory survive a
/// checkpoint. The queued payload must drive the same follow-up provider request, while a new
/// delivery with the same key remains ignored on both sides.
#[test]
fn a_queued_signal_and_its_dedupe_key_restore_to_the_same_follow_up() {
    let mut uninterrupted = Runtime::new();
    uninterrupted.submit(&signal_config(8, None));
    let started = uninterrupted.submit(&agent_start("in-start", 1_700_000_001_000));
    uninterrupted.submit(&signal_delivery(
        "in-sig-1",
        1_700_000_002_000,
        "delivery-a",
        1,
        LogicalSignal {
            payload: super::super::scalar::BoundedJson::new(json!({
                "job": "nightly-index"
            }))
            .unwrap(),
            dedupe_key: Some("nightly-index".to_string()),
            ..logical_signal("sig-nightly", SignalUrgency::Normal)
        },
    ));

    let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);

    let duplicate = signal_delivery(
        "in-sig-2",
        1_700_000_003_000,
        "delivery-b",
        2,
        LogicalSignal {
            payload: super::super::scalar::BoundedJson::new(json!({
                "job": "nightly-index"
            }))
            .unwrap(),
            dedupe_key: Some("nightly-index".to_string()),
            ..logical_signal("sig-nightly", SignalUrgency::Normal)
        },
    );
    assert_eq!(
        restored.submit(&duplicate),
        uninterrupted.submit(&duplicate),
        "the restored router remembers the business dedupe key",
    );

    let provider = effect_id(started.step_seq);
    let answer = provider_answer(
        "in-answer",
        1_700_000_004_000,
        &provider,
        "the first request completed",
    );
    assert_eq!(
        restored.submit(&answer),
        uninterrupted.submit(&answer),
        "the queued payload produces the same follow-up provider request",
    );
    assert_eq!(surface(&restored), surface(&uninterrupted));
}

#[test]
fn caller_capability_ceiling_survives_checkpoint_restore() {
    use crate::types::capability::{
        ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Lease, Principal,
        ResourceSelector,
    };

    let capability = Capability {
        id: CapabilityId("root-read".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/src/**".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: Some(Lease {
            expires_at_turn: Some(10),
        }),
        delegatable: true,
        issuer: Principal("root".into()),
    };
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    runtime.submit(&agent_start_with_capabilities(
        "in-start",
        1_700_000_001_000,
        vec![capability.clone()],
    ));

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    let restored = Runtime::restore_with(Some(&checkpoint), &[]);

    assert_eq!(
        restored
            .driver
            .engine
            .as_ref()
            .unwrap()
            .task_capabilities("root"),
        &[capability],
        "authority state must not disappear when the reverse runtime is rebuilt"
    );
}

#[test]
fn hierarchical_budget_grant_and_settlement_marker_survive_checkpoint_restore() {
    use crate::scheduler::budget_grant::{ResourceBudget, debit, reserve};
    use crate::scheduler::tcb::TaskLifecycle;
    use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};

    let tokens = |value| ResourceBudget {
        tokens: Some(value),
        ..ResourceBudget::default()
    };
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let table = runtime.driver.engine.as_mut().unwrap().task_table_mut();
    table.get_mut("root").unwrap().child_budget_remaining = Some(tokens(100));
    let manifest = IsolationManifest {
        agent_id: "child".into(),
        role: AgentRole::Implement,
        isolation: AgentIsolation::Shared,
        context_inheritance: ContextInheritance::Full,
        permitted_capability_ids: Vec::new(),
        requested_capabilities: Vec::new(),
        requested_budget: Some(tokens(60)),
    };
    table
        .spawn_child(
            "root",
            &manifest,
            SchedulerBudget::default(),
            TaskLifecycle::Running,
        )
        .unwrap();
    let grant = reserve("root".into(), "child".into(), &tokens(100), &tokens(60)).unwrap();
    table.get_mut("root").unwrap().child_budget_remaining = Some(debit(&tokens(100), &tokens(60)));
    table.attach_child_budget_grant("child", grant);

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
    let restored_table = restored.driver.engine.as_mut().unwrap().task_table_mut();
    assert_eq!(
        restored_table
            .get("child")
            .unwrap()
            .budget_grant
            .as_ref()
            .unwrap()
            .reserved,
        tokens(60)
    );
    assert_eq!(
        restored_table.get("child").unwrap().child_budget_remaining,
        Some(tokens(60))
    );

    restored_table.return_child_budget("child");
    restored_table.return_child_budget("child");
    assert_eq!(
        restored_table.get("root").unwrap().child_budget_remaining,
        Some(tokens(100)),
        "restored grant settles exactly once"
    );
    assert!(
        restored_table
            .get("child")
            .unwrap()
            .budget_grant
            .as_ref()
            .unwrap()
            .settled
    );
}

#[test]
fn spc_019_09_cross_task_object_read_requires_capability_and_registry_restores() {
    use crate::mm::handle::{Handle, HandleKind, ObjectDescriptor};
    use crate::scheduler::tcb::TaskLifecycle;
    use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};
    use crate::types::capability::{
        ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
        ResourceSelector,
    };

    fn start_with_object(capabilities: Vec<Capability>) -> (Runtime, EffectId) {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&agent_start_with_capabilities(
            "in-start",
            1_700_000_001_000,
            capabilities,
        ));
        let provider = sole_effect(&started).effect_id.clone();
        let handle = Handle::resident_for(77, HandleKind::ToolResult, 1, "shared-77");
        let descriptor = ObjectDescriptor::from_handle("owner-a".into(), &handle, 1);
        let engine = runtime.driver.engine.as_mut().unwrap();
        engine.ctx.handles.insert(handle);
        engine
            .task_table_mut()
            .spawn_child(
                "root",
                &IsolationManifest {
                    agent_id: "owner-a".into(),
                    role: AgentRole::Implement,
                    isolation: AgentIsolation::Shared,
                    context_inheritance: ContextInheritance::Full,
                    permitted_capability_ids: Vec::new(),
                    requested_capabilities: Vec::new(),
                    requested_budget: None,
                },
                SchedulerBudget::default(),
                TaskLifecycle::Running,
            )
            .unwrap();
        engine
            .task_table_mut()
            .register_object("owner-a", descriptor)
            .unwrap();
        (runtime, provider)
    }

    let (mut denied, denied_provider) = start_with_object(Vec::new());
    denied.submit(&provider_result(
        "in-read-denied",
        1_700_000_002_000,
        &denied_provider,
        vec![tool_call(
            "call-read",
            "read_object",
            json!({"object_id": 77}),
        )],
    ));
    assert_eq!(rejections(&denied)[0].0, "read_object");

    let capability = Capability {
        id: CapabilityId("read-owner-a-77".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("object:owner-a/77".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: false,
        issuer: Principal("owner-a".into()),
    };
    let (mut allowed, allowed_provider) = start_with_object(vec![capability]);
    allowed.submit(&provider_result(
        "in-read-allowed",
        1_700_000_002_000,
        &allowed_provider,
        vec![tool_call(
            "call-read",
            "read_object",
            json!({"object_id": 77}),
        )],
    ));
    assert!(rejections(&allowed).is_empty());

    let checkpoint = allowed.checkpoint().decode().expect("verifies");
    let restored = Runtime::restore_with(Some(&checkpoint), &[]);
    assert_eq!(surface(&restored), surface(&allowed));
    assert_eq!(
        restored
            .driver
            .engine
            .as_ref()
            .unwrap()
            .task_table()
            .object(77)
            .unwrap()
            .owner
            .as_str(),
        "owner-a"
    );
}

#[test]
fn spc_019_11_canonical_workflow_and_timer_waiter_emit_one_local_runnable_trace() {
    use crate::scheduler::runnable::LocalRunnableKind;
    use crate::scheduler::tcb::{LogicalDeadline, TaskLifecycle};
    use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};

    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config());
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let provider = sole_effect(&started).effect_id.clone();
    let table = runtime.driver.engine.as_mut().unwrap().task_table_mut();
    table
        .spawn_child(
            "root",
            &IsolationManifest {
                agent_id: "timer-waiter".into(),
                role: AgentRole::Implement,
                isolation: AgentIsolation::Shared,
                context_inheritance: ContextInheritance::Full,
                permitted_capability_ids: Vec::new(),
                requested_capabilities: Vec::new(),
                requested_budget: None,
            },
            SchedulerBudget::default(),
            TaskLifecycle::Running,
        )
        .unwrap();
    table.wait_for_timer("timer-waiter", LogicalDeadline(1_700_000_002_000));

    let transition = runtime.submit(&provider_result(
        "in-workflow",
        1_700_000_002_000,
        &provider,
        vec![tool_call(
            "call-workflow",
            "start_workflow",
            serde_json::to_value(two_node_spec()).unwrap(),
        )],
    ));
    assert_eq!(sole_effect(&transition).tag(), EffectKindTag::SpawnTasks);
    let trace = runtime
        .observations()
        .iter()
        .filter_map(|observation| match observation {
            KernelObservation::LocalRunnableTrace { runnable, .. } => Some(runnable),
            _ => None,
        })
        .find(|runnable| runnable.len() == 2)
        .expect("workflow node and timer waiter share the production trace");
    assert_eq!(
        trace
            .iter()
            .map(|entry| (entry.id.as_str(), entry.kind))
            .collect::<Vec<_>>(),
        vec![
            ("timer-waiter", LocalRunnableKind::TimerWaiter),
            ("wf-node0", LocalRunnableKind::WorkflowNode),
        ]
    );

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    let restored = Runtime::restore_with(Some(&checkpoint), &[]);
    assert_eq!(surface(&restored), surface(&runtime));
}

/// Task 16b · a completed child remains the same process fact after restore: its role,
/// isolation, context inheritance, capability ceiling and join result are not inferred anew.
#[test]
fn a_subagent_process_and_join_result_restore_without_permission_drift() {
    let mut spec = two_node_spec();
    spec.nodes[0].run_spec = Some(LogicalAgentSpec {
        role: Some(WireRole::Verify),
        isolation: Some(WireIsolation::ReadOnly),
        ..LogicalAgentSpec::new("verify the collected sources")
    });

    let mut uninterrupted = Runtime::new();
    uninterrupted.submit(&syscall_config());
    let started = uninterrupted.submit(&workflow_start("in-start", 1_700_000_001_000, spec));
    uninterrupted.submit(&spawned(
        "in-ack-1",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        &["wf-node0"],
    ));
    let completed = uninterrupted.submit(&child_done(
        "in-done-1",
        1_700_000_003_000,
        "wf-node0",
        "sources verified",
    ));

    let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
    let original_task = uninterrupted
        .driver
        .project_logical_state()
        .scheduler
        .tasks
        .into_iter()
        .find(|task| task.task_id.as_str() == "wf-node0")
        .expect("the completed child remains projected");
    let restored_task = restored
        .driver
        .project_logical_state()
        .scheduler
        .tasks
        .into_iter()
        .find(|task| task.task_id.as_str() == "wf-node0")
        .expect("the restored child remains projected");
    assert_eq!(restored_task, original_task);
    let process = restored_task.process.expect("it is still a child process");
    assert_eq!(process.role, "verify");
    assert_eq!(process.isolation, "read_only");
    assert_eq!(process.context_inheritance, "none");
    assert!(
        process.join_result.is_some(),
        "the join result is source state"
    );

    let next_spawn = effect_id(completed.step_seq);
    let ack = spawned("in-ack-2", 1_700_000_004_000, &next_spawn, &["wf-node1"]);
    assert_eq!(
        restored.submit(&ack),
        uninterrupted.submit(&ack),
        "the restored process table authorizes the same next transition",
    );
    assert_eq!(surface(&restored), surface(&uninterrupted));
}

#[test]
fn spc_009_06_a_restored_root_tasks_child_budget_remaining_matches_the_checkpointed_value() {
    // Plan §8 / spc_009-06: closes the checkpoint-wire half of spc_009-05 — root's
    // `child_budget_remaining` was seeded from a real RunGroup admission grant (spc_009-05),
    // but `TaskControlState` did not carry it on the wire, so a checkpoint→restore silently
    // reset root's pool to `None`, un-seeding the hierarchical budget check right after a
    // restore even though nothing about the RunGroup grant changed.
    //
    // Narrower than originally scoped: `LoopStateMachine.budget_grant` itself (the
    // whole-operation admission grant `engine.budget_grant()` reports) needs no new wire
    // field at all — `Runtime::restore_with` already rebuilds it from `genesis_config` via
    // `build_engine`, and `budget_grant` is boot-only (absent from `LivePolicyPatch` in
    // `command.rs`), so the genesis record it is already part of is authoritative for the
    // whole operation's lifetime. Verified empirically: the checkpoint round-trip below
    // reproduces the same `engine.budget_grant()` on both sides with no `SchedulerState`
    // field for it at all. The one genuine gap is `child_budget_remaining`, which is derived,
    // per-task, debit-mutated state with no other durable home — that is what this test (and
    // `TaskControlState.child_budget_remaining`) actually closes.
    use crate::runtime::kernel::wire::config::BudgetGrant;
    use crate::scheduler::budget_grant::ResourceBudget;

    let mut uninterrupted = Runtime::new();
    uninterrupted.submit(&syscall_config_with(|config| {
        config.budget_grant = Some(BudgetGrant {
            reservation_id: "res-1".to_string(),
            tokens: Some(WireU64::new(1_000)),
            subagents: None,
            rounds: None,
        });
    }));
    uninterrupted.submit(&agent_start("in-start", 1_700_000_001_000));

    let expected_pool = uninterrupted
        .driver
        .engine()
        .expect("engine installed")
        .task_table()
        .get("root")
        .expect("root exists")
        .child_budget_remaining;
    assert_eq!(
        expected_pool,
        Some(ResourceBudget {
            tokens: Some(1_000),
            ..ResourceBudget::default()
        }),
        "sanity: root's pool really was seeded from the grant before any checkpoint"
    );

    let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
    let restored = Runtime::restore_with(Some(&checkpoint), &[]);

    let actual_pool = restored
        .driver
        .engine()
        .expect("engine installed")
        .task_table()
        .get("root")
        .expect("root exists")
        .child_budget_remaining;
    assert_eq!(
        actual_pool, expected_pool,
        "a restored root task's own grantable pool must match the checkpointed one exactly, \
             not be re-derived fresh from the admission grant (which would silently undo debits)"
    );
}

#[test]
fn spc_002_09_a_restored_approval_wait_is_indexed_the_same_as_before_checkpoint() {
    // Plan §3.1 "Replay invariants": same checkpoint+journal must reproduce the same WaitSet
    // state. `WaitIndex` is derived state that restore must reproduce, or a task that was
    // waiting when checkpointed becomes unwakeable after restore.
    use crate::scheduler::tcb::ApprovalId;
    use crate::scheduler::wait_index::WaitKey;

    let (mut uninterrupted, provider) = agent_awaiting_approval();
    let requested = uninterrupted.submit(&provider_result(
        "in-gated",
        1_700_000_002_000,
        &provider,
        vec![tool_call("call-1", "search", json!({"q": "a"}))],
    ));
    assert_eq!(kinds(&requested), vec![EffectKindTag::RequestApproval]);

    let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
    let restored = Runtime::restore_with(Some(&checkpoint), &[]);

    let key = WaitKey::Approval(ApprovalId("pending".into()));
    let expected = uninterrupted
        .driver
        .engine()
        .expect("engine installed")
        .task_table()
        .wait_index()
        .lookup(&key)
        .to_vec();
    assert!(
        !expected.is_empty(),
        "sanity: the uninterrupted run really did index a waiting task"
    );
    let actual = restored
        .driver
        .engine()
        .expect("engine installed")
        .task_table()
        .wait_index()
        .lookup(&key)
        .to_vec();
    assert_eq!(
        actual, expected,
        "a restored Approval wait must be indexed identically to the uninterrupted run"
    );
}

#[test]
fn a_pending_subagent_preemption_restores_from_the_transition_effect() {
    let (mut uninterrupted, _) = workflow_with_live_child();
    let requested = uninterrupted.submit(&signal_delivery(
        "in-sig-critical",
        1_700_000_003_000,
        "delivery-critical",
        1,
        logical_signal("sig-critical", SignalUrgency::Critical),
    ));
    let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);

    let resolved = resolved(
        "in-preempted",
        1_700_000_004_000,
        &effect_id(requested.step_seq),
        EffectSuccess::TasksPreempted(super::super::effect::TasksPreemptedSuccess {
            attempts: vec![super::super::effect::TaskPreemptOutcome {
                task_id: TaskId::new("wf-node0").unwrap(),
                attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                outcome: super::super::effect::TaskPreemptStatus::Preempted(
                    super::super::effect::TaskPreempted {},
                ),
            }],
        }),
    );
    assert_eq!(
        restored.submit(&resolved),
        uninterrupted.submit(&resolved),
        "the transition-owned preempt intent remains resolvable after restore",
    );
    assert_eq!(surface(&restored), surface(&uninterrupted));
}

/// Task 16b · a `PagedOut` handle restored from a checkpoint projects the archived tool body
/// exactly as the uninterrupted renderer does on the next provider call.
#[test]
fn a_paged_out_result_restores_to_the_same_rendered_provider_context() {
    let mut uninterrupted = Runtime::new();
    uninterrupted.submit(&syscall_config());
    let started = uninterrupted.submit(&agent_start("in-start", 1_700_000_001_000));
    let archived_body = "ARCHIVED TOOL OUTPUT ".repeat(300);
    {
        let engine = uninterrupted
            .driver
            .engine
            .as_mut()
            .expect("the configured agent has an engine");
        let mut assistant = CoreMessage::assistant("I checked the archive.");
        assistant.tool_calls = vec![crate::types::message::ToolCall {
            id: "call-archived".into(),
            name: "search".into(),
            arguments: json!({"q": "archived evidence"}),
        }];
        engine.ctx.push_history(assistant, 8);
        engine.ctx.push_history(
            CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: "call-archived".into(),
                output: archived_body,
                is_error: false,
                durable_content: None,
            }]),
            1_200,
        );
        let handle_id = engine
            .ctx
            .handles
            .all()
            .iter()
            .find(|handle| handle.source.as_deref() == Some("call-archived"))
            .expect("the tool result is addressable")
            .id;
        engine
            .ctx
            .handles
            .get_mut(handle_id)
            .expect("the handle remains live")
            .residency = Residency::PagedOut {
            payload_ref: "payload:checkpoint-archive".to_string(),
            digest: format!("sha256:{}", "1".repeat(64)),
        };
    }
    let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
    assert_eq!(
        serde_json::to_value(
            &restored
                .driver
                .engine()
                .expect("restored engine")
                .ctx
                .render()
                .turns
        )
        .unwrap(),
        serde_json::to_value(
            &uninterrupted
                .driver
                .engine()
                .expect("uninterrupted engine")
                .ctx
                .render()
                .turns
        )
        .unwrap(),
        "the restored PagedOut preview itself matches before either run advances",
    );

    let acted = provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call(
            "call-after-page-out",
            "search",
            json!({
                "q": "fresh evidence"
            }),
        )],
    );
    let uninterrupted_tools = uninterrupted.submit(&acted);
    let restored_tools = restored.submit(&acted);
    assert_eq!(restored_tools, uninterrupted_tools);

    let results = tools_resolved(
        "in-results",
        1_700_000_003_000,
        &effect_id(uninterrupted_tools.step_seq),
        &[("call-after-page-out", "fresh evidence found", false)],
    );
    let uninterrupted_provider = uninterrupted.submit(&results);
    let restored_provider = restored.submit(&results);
    assert_eq!(
        restored_provider, uninterrupted_provider,
        "the post-restore renderer emits the same provider context with a PagedOut preview",
    );
    assert_eq!(surface(&restored), surface(&uninterrupted));
}

/// **Verification 1 (full-state form)** · an uninterrupted run and a restored one are
/// byte-identical.
///
/// Both sides submit the *same* envelope list. The second takes a full-state checkpoint
/// half-way, throws the runtime away, restores from the blob plus the records above it, and
/// finishes. Every record digest on both journals must match, and so must the whole observable
/// surface — including the digest of the logical state, which is the strongest statement
/// available: the two runtimes would produce the same checkpoint.
#[test]
fn an_uninterrupted_run_and_a_full_state_restore_are_byte_identical() {
    let envelopes = turn_envelopes();

    let mut uninterrupted = Runtime::new();
    drive(&mut uninterrupted, &envelopes);

    let mut interrupted = Runtime::new();
    drive(&mut interrupted, &envelopes[..4]);
    let candidate = interrupted.checkpoint();
    let checkpoint = candidate.decode().expect("the candidate blob verifies");
    assert_eq!(
        checkpoint.base_step_seq(),
        checkpoint.through_step_seq(),
        "this half of the differential exercises the full-state form",
    );

    // The crash: everything in memory is gone, and all the host still holds is the blob and the
    // journal. Records at or below `through_step_seq` are deliberately *not* handed back.
    let mut restored = interrupted.restore(&checkpoint);
    assert_eq!(
        restored.restore_cost.unwrap().records_before_checkpoint,
        0,
        "a restore with a checkpoint reads nothing below it",
    );
    assert_eq!(
        surface(&restored),
        surface(&interrupted),
        "the restored runtime is the runtime that crashed",
    );

    for envelope in &envelopes[4..] {
        let expected = interrupted.submit(envelope);
        let actual = restored.submit(envelope);
        assert_eq!(
            actual, expected,
            "each post-restore transition matches the live runtime"
        );
    }
    assert_eq!(
        digests(&restored.journal),
        digests(&uninterrupted.journal[4..]),
        "every post-restore record is byte-identical to the uninterrupted one",
    );
    assert_eq!(
        surface(&restored),
        surface(&uninterrupted),
        "and so is the state they end in",
    );
}

#[test]
fn context_optimization_evidence_and_pending_knowledge_survive_checkpoint_restore() {
    let mut runtime = Runtime::new();
    runtime.submit(&configure());
    runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    {
        let ctx = &mut runtime.driver.engine_mut().unwrap().ctx;
        ctx.push_knowledge_entry(
            Some("reference".into()),
            CoreMessage::system("original evidence"),
            3,
            true,
        );
        ctx.push_history(CoreMessage::user("reference original evidence"), 4);
        ctx.push_knowledge_entry(
            Some("reference".into()),
            CoreMessage::system("replacement evidence"),
            7,
            true,
        );
        ctx.partitions.history.measurements[0].source =
            crate::context::measurement::MeasurementSource::LocalExact {
                tokenizer: "checkpoint-estimator".into(),
            };
        ctx.partitions.history.measurements[0].confidence =
            crate::context::measurement::MeasurementConfidence::Exact;
    }
    let checkpoint = runtime.checkpoint().decode().unwrap();
    let restored = runtime.restore(&checkpoint);
    let before = &runtime.driver.engine().unwrap().ctx;
    let after = &restored.driver.engine().unwrap().ctx;
    assert_eq!(
        before.partitions.history.measurements,
        after.partitions.history.measurements
    );
    assert_eq!(
        before.knowledge_checkpoint_state(),
        after.knowledge_checkpoint_state()
    );
    let entry = &after.partitions.knowledge.entries[0];
    assert_eq!(entry.use_count, 1);
    assert!(entry.last_used_step.is_some());
    assert_eq!(
        entry.pending.as_ref().unwrap().0.content.as_text(),
        Some("replacement evidence")
    );
    let policy = crate::evolution::ContentDigest::from_bytes(b"policy");
    let (expected, _) = before
        .prepare_candidate(OPERATION.into(), "next-step".into(), 3, policy.clone())
        .unwrap();
    let (actual, _) = after
        .prepare_candidate(OPERATION.into(), "next-step".into(), 3, policy)
        .unwrap();
    assert_eq!(expected, actual);
}

/// **Verification 1 (rebase form)** · the same differential over a checkpoint whose logical
/// state sits *below* its covered head.
///
/// The restore therefore has real work to do before it touches the journal: it replays the
/// bounded tail, verifies each replayed record against the digest the checkpoint recorded, and
/// only then continues.
#[test]
fn an_uninterrupted_run_and_a_rebased_restore_are_byte_identical() {
    let envelopes = turn_envelopes();

    let mut uninterrupted = Runtime::new();
    drive(&mut uninterrupted, &envelopes);

    let mut interrupted = Runtime::new();
    drive(&mut interrupted, &envelopes[..2]);
    let base = interrupted
        .checkpoint()
        .decode()
        .expect("the base candidate verifies");
    drive(&mut interrupted, &envelopes[2..4]);

    let rebased = interrupted
        .tx
        .checkpoint_rebase(
            &CheckpointBoundary {
                through_step_seq: base.through_step_seq(),
                covered_head: base.covered_transaction_head_digest().clone(),
            },
            base.logical_state().clone(),
        )
        .expect("a rebase over (1, 3] is assemblable")
        .decode()
        .expect("the rebase blob verifies");
    assert!(
        rebased.base_step_seq() < rebased.through_step_seq(),
        "this half of the differential exercises the rebase form",
    );
    assert_eq!(rebased.tail_inputs().len(), 2);

    let mut restored = interrupted.restore(&rebased);
    let cost = restored.restore_cost.unwrap();
    assert_eq!(cost.records_before_checkpoint, 0);
    assert_eq!(
        cost.tail_inputs_replayed, 2,
        "the tail was actually replayed"
    );
    assert_eq!(
        surface(&restored),
        surface(&interrupted),
        "replaying the bounded tail lands on the state the run was in",
    );

    // The rebase form is what Task 16 adds to the checkpoint contract, so its five candidate
    // values and the state a restore lands on are frozen together.
    let produced = json!({
        "description":
            "Spec 12.2 / 12.3 rule 11 · the rebase form end to end. `logical_state` is the \
             state after `base_step_seq`, `tail_inputs` covers (base, through] exactly, and \
             `base_record_digest` is the chain anchor the tail replays from — the record an \
             acked checkpoint is allowed to have reclaimed. Restoring the blob replays that \
             tail, verifies each replayed record against the digest the checkpoint carries, and \
             lands on the head the run was at.",
        "base_step_seq": rebased.base_step_seq().to_string(),
        "base_record_digest": rebased.base_record_digest().as_str(),
        "through_step_seq": rebased.through_step_seq().to_string(),
        "covered_head": rebased.covered_transaction_head_digest().as_str(),
        "state_digest": rebased.state_digest().as_str(),
        "tail_steps": rebased
            .tail_inputs()
            .iter()
            .map(|entry| entry.step_seq.to_string())
            .collect::<Vec<_>>(),
        "restored_head": restored.tx.head().unwrap().digest.as_str(),
        "restore_cost": {
            "records_before_checkpoint": cost.records_before_checkpoint,
            "tail_inputs_replayed": cost.tail_inputs_replayed,
            "records_after_checkpoint": cost.records_after_checkpoint,
        },
    });
    let expected = golden("golden_checkpoint_rebase_restore.json", &produced);
    assert_eq!(produced, expected, "the rebase restore drifted");
    assert_eq!(expected["base_step_seq"], json!("1"));
    assert_eq!(expected["through_step_seq"], json!("3"));
    assert_eq!(
        expected["restore_cost"]["records_before_checkpoint"],
        json!(0),
        "the whole point: nothing below the base is read",
    );

    drive(&mut restored, &envelopes[4..]);
    assert_eq!(
        digests(&restored.journal),
        digests(&uninterrupted.journal[4..]),
    );
    assert_eq!(surface(&restored), surface(&uninterrupted));
}

/// §7.10 · a body that lives with the host travels as a reference, and comes back as one.
///
/// The half of §5q-2 that keeps the checkpoint from re-inlining what §7.10 spent an effect kind
/// keeping out: the message carries the preview, the handle carries the locator and the digest,
/// and the restore rebuilds exactly that pairing.
#[test]
fn an_external_body_is_checkpointed_by_reference_and_restored_as_one() {
    let (mut runtime, effect) = agent_awaiting_tool_results();
    let body_digest =
        super::super::record::canonical_digest(b"a body far over the inline threshold");
    runtime.submit(&payloads_resolved(
        "in-results",
        1_700_000_003_000,
        &effect,
        vec![external_payload(
            "call-1",
            body_digest.clone(),
            64 * 1024,
            "the first 2 KiB of it",
        )],
    ));

    let context_vm = runtime.driver.project_logical_state().context_vm;
    let referenced: Vec<&StoredMessageState> = context_vm
        .messages
        .iter()
        .filter(|message| matches!(message.body, StoredMessageBody::Reference(_)))
        .collect();
    assert_eq!(referenced.len(), 1, "exactly the external result");
    let StoredMessageBody::Reference(reference) = &referenced[0].body else {
        unreachable!()
    };
    assert_eq!(reference.digest, body_digest.as_str());
    assert_eq!(reference.tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(reference.preview, "the first 2 KiB of it");
    assert!(
        !serde_json::to_string(&context_vm)
            .unwrap()
            .contains("a body far over the inline threshold"),
        "the body itself is nowhere in the checkpoint",
    );

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    let restored = Runtime::restore_with(Some(&checkpoint), &[]);
    assert_eq!(surface(&restored), surface(&runtime));
    assert_eq!(
        restored
            .driver
            .project_logical_state()
            .context_vm
            .handles
            .iter()
            .filter_map(|handle| handle.digest.clone())
            .collect::<Vec<_>>(),
        vec![body_digest.to_string()],
        "the handle that addresses the body is restored with its verification digest",
    );
}

#[test]
fn a_structured_inline_tool_result_survives_checkpoint_restore() {
    let (mut runtime, effect) = agent_awaiting_structured_tool_results();
    let durable = DurableContent {
        blocks: vec![
            DurableContentBlock::Text {
                text: "captured".into(),
            },
            DurableContentBlock::Image {
                source: DurableSource::Base64 {
                    data: "aW1hZ2U=".into(),
                },
                media_type: Some("image/png".into()),
                provider_options: None,
            },
            DurableContentBlock::File {
                source: DurableSource::FileId {
                    id: "file-7".into(),
                    affinity: crate::types::durable_content::EndpointAffinity {
                        provider_id: "openai".into(),
                        endpoint_id: "responses".into(),
                    },
                },
                media_type: Some("application/pdf".into()),
                provider_options: None,
            },
        ],
    };
    runtime.submit(&payloads_resolved(
        "in-structured-results",
        1_700_000_003_000,
        &effect,
        vec![WireToolResultPayload::Inline(InlineToolResult {
            call_id: CallId::new("call-1").unwrap(),
            result: WireToolResult {
                output: "captured".into(),
                durable_content: Some(durable.clone()),
                is_error: false,
                disposition: ToolResultDisposition::Recoverable,
            },
        })],
    ));

    let checkpoint = runtime.checkpoint().decode().expect("checkpoint verifies");
    let structured = checkpoint
        .logical_state()
        .context_vm
        .messages
        .iter()
        .find_map(|message| match &message.body {
            StoredMessageBody::Structured(body) => body.durable_tool_results.first(),
            _ => None,
        })
        .expect("structured tool result stays in checkpoint");
    assert_eq!(structured.call_id, "call-1");
    assert_eq!(structured.blocks, durable.blocks);

    let restored = Runtime::restore_with(Some(&checkpoint), &[]);
    let restored_result = restored
        .driver
        .engine()
        .expect("engine")
        .ctx
        .partitions
        .history
        .messages
        .iter()
        .find_map(|message| match &message.content {
            Content::Parts(parts) => parts.iter().find_map(|part| match part {
                ContentPart::ToolResult {
                    call_id,
                    durable_content,
                    ..
                } if call_id.as_str() == "call-1" => durable_content.as_ref(),
                _ => None,
            }),
            Content::Text(_) => None,
        })
        .expect("restored result keeps durable blocks");
    assert_eq!(restored_result, &durable);
}

#[test]
fn an_inline_tool_result_rejects_invalid_durable_content_before_state_mutation() {
    let (mut runtime, effect) = agent_awaiting_tool_results();
    let fault = runtime.reject(&payloads_resolved(
        "in-invalid-structured-result",
        1_700_000_003_000,
        &effect,
        vec![WireToolResultPayload::Inline(InlineToolResult {
            call_id: CallId::new("call-1").unwrap(),
            result: WireToolResult {
                output: "bad".into(),
                durable_content: Some(DurableContent {
                    blocks: vec![DurableContentBlock::Image {
                        source: DurableSource::Url { url: String::new() },
                        media_type: None,
                        provider_options: None,
                    }],
                }),
                is_error: false,
                disposition: ToolResultDisposition::Recoverable,
            },
        })],
    ));
    assert_eq!(fault.code, KernelFaultCode::MalformedEnvelope);
    assert!(fault.message.contains("invalid durable content"));
    assert_eq!(
        runtime.pending_effect_kinds(),
        vec![EffectKindTag::ExecuteTools]
    );
}

#[test]
fn structured_tool_result_is_explicitly_downgraded_when_micro_compacted() {
    use crate::context::compression::{Compressor, MicroCompactor};
    use crate::context::partitions::ContextPartitions;
    use crate::context::token_engine::ContextTokenEngine;

    let durable = DurableContent {
        blocks: vec![DurableContentBlock::Image {
            source: DurableSource::Base64 {
                data: "aW1hZ2U=".into(),
            },
            media_type: Some("image/png".into()),
            provider_options: None,
        }],
    };
    let mut partitions = ContextPartitions::default();
    let message = CoreMessage::tool(vec![ContentPart::ToolResult {
        call_id: "call-1".into(),
        output: "x".repeat(12_000),
        is_error: false,
        durable_content: Some(durable),
    }]);
    partitions.history.push(message, 3_000);
    let engine = ContextTokenEngine::char_approx();
    MicroCompactor.compress(&mut partitions, 0, 0, 0, &engine);
    let Content::Parts(parts) = &partitions.history.messages[0].content else {
        panic!("tool message remains structured")
    };
    let [
        ContentPart::ToolResult {
            durable_content,
            output,
            ..
        },
    ] = parts.as_slice()
    else {
        panic!("tool message keeps its result part")
    };
    assert!(durable_content.is_none());
    assert!(output.starts_with("[tool result:"));
}

/// §12.3 rule 11 · the two candidate forms agree on `state_digest` for the same logical state.
///
/// This is what makes them interchangeable rather than merely both legal: a host that switches
/// from full-state to rebase does not change what its checkpoints *mean*, only how much they
/// re-serialise.
#[test]
fn a_rebase_and_a_full_state_candidate_agree_on_the_state_digest() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..2]);
    let base = runtime
        .checkpoint()
        .decode()
        .expect("the base candidate verifies");
    drive(&mut runtime, &envelopes[2..4]);

    let full_state = runtime.checkpoint();
    let rebase = runtime
        .tx
        .checkpoint_rebase(
            &CheckpointBoundary {
                through_step_seq: base.through_step_seq(),
                covered_head: base.covered_transaction_head_digest().clone(),
            },
            base.logical_state().clone(),
        )
        .expect("a rebase is assemblable");

    assert_eq!(
        full_state.through_step_seq, rebase.through_step_seq,
        "both cover the same prefix",
    );
    assert_eq!(full_state.covered_head, rebase.covered_head);
    assert_ne!(
        full_state.state_digest, rebase.state_digest,
        "they carry *different* logical states — one at the head, one at the base",
    );
    assert_eq!(
        rebase.state_digest,
        *base.state_digest(),
        "and a rebase carries its base's state forward untouched, byte for byte",
    );

    // Restoring either one lands on the same place, which is the property the digests are
    // evidence *for*.
    let from_full = runtime.restore(&full_state.decode().unwrap());
    let from_rebase = runtime.restore(&rebase.decode().unwrap());
    assert_eq!(surface(&from_full), surface(&from_rebase));
    assert_eq!(surface(&from_full), surface(&runtime));
}

/// **Verification 2** · restore cost is bounded by the tail, not by how long the run is.
///
/// Deterministic counters, not a timer: the claim is about how much history is read, and that is
/// a number the restore can report exactly. The run is driven to three different lengths and the
/// cost is asserted *equal* across all three — not merely "small".
#[test]
fn long_run_restore_cost_is_bounded_by_the_tail_not_the_run() {
    fn cost_after(turns: usize) -> RestoreCost {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.execution_policy = Some(ExecutionPolicy {
                max_turns: Some(10_000),
                ..ExecutionPolicy::default()
            });
        }));
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let mut effect = effect_id(started.step_seq);
        let mut at = 1_700_000_002_000;
        for turn in 0..turns {
            let acted = runtime.submit(&provider_result(
                &format!("in-acted-{turn}"),
                at,
                &effect,
                vec![tool_call(
                    &format!("call-{turn}"),
                    "search",
                    json!({ "q": turn }),
                )],
            ));
            at += 1_000;
            let results = runtime.submit(&tools_resolved(
                &format!("in-results-{turn}"),
                at,
                &effect_id(acted.step_seq),
                &[(&format!("call-{turn}"), "a source", false)],
            ));
            at += 1_000;
            effect = effect_id(results.step_seq);
        }

        // The host checkpoints at the head and hands the restore only what is above it.
        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        let after = runtime.journal_from(&checkpoint);
        assert!(after.is_empty(), "the checkpoint covers the whole journal");
        Runtime::restore_with(Some(&checkpoint), &after)
            .restore_cost
            .expect("a restored runtime reports its cost")
    }

    let short = cost_after(2);
    let medium = cost_after(8);
    let long = cost_after(32);

    assert_eq!(short.total_transitions(), 0, "nothing is replayed at all");
    assert_eq!(
        (short, medium),
        (medium, long),
        "restore cost does not grow with the length of the run",
    );

    // And the contrast that makes the number mean something: without a checkpoint the same
    // restore reads the whole journal.
    let mut runtime = Runtime::new();
    drive(&mut runtime, &turn_envelopes());
    let journal = runtime.journal.clone();
    let from_genesis = Runtime::restore_with(None, &journal);
    assert_eq!(
        from_genesis.restore_cost.unwrap().records_before_checkpoint,
        journal.len() as u64,
        "the no-checkpoint arm is O(run), which is exactly what §12 replaces",
    );
    assert_eq!(surface(&from_genesis), surface(&runtime));
}

/// §12.2 last line · with no checkpoint the fold starts at genesis and runs the **same** path.
#[test]
fn a_restore_without_a_checkpoint_uses_the_same_transaction_fold() {
    let mut runtime = Runtime::new();
    drive(&mut runtime, &turn_envelopes()[..4]);
    let from_genesis = Runtime::restore_with(None, &runtime.journal);
    assert_eq!(surface(&from_genesis), surface(&runtime));
    assert_eq!(from_genesis.restore_cost.unwrap().tail_inputs_replayed, 0);
}

/// **Verification 3, row 1** · §12.3 rule 5: a crash between install and ack still restores
/// from the installed checkpoint.
///
/// The ack is a *retention* signal, not a durability one. Nothing about the checkpoint's
/// validity depends on it, which is why this row is a test rather than a caveat.
#[test]
fn crash_matrix_install_then_crash_before_ack_restores_from_the_installed_checkpoint() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);

    let candidate = runtime.checkpoint();
    let checkpoint = candidate.decode().expect("the host installed this blob");
    // ... and then the process dies. `note_checkpoint_acked` was never called.
    let restored = runtime.restore(&checkpoint);
    assert_eq!(surface(&restored), surface(&runtime));
    assert_eq!(restored.restore_cost.unwrap().records_before_checkpoint, 0);

    // §12.2 line 8 · what the host is handed back: the effects to (re-)execute, or a terminal.
    let recovered = restore_operation(
        Some(&checkpoint),
        &[],
        ConfigDefaults::default(),
        InMemoryRecordIndex::new(),
    )
    .expect("the ladder runs");
    assert_eq!(
        recovered
            .pending_effects()
            .iter()
            .map(|effect| effect.tag())
            .collect::<Vec<_>>(),
        vec![EffectKindTag::CallProvider],
        "§5g-1 · the effect the operation is waiting on is exposed again",
    );
    assert!(recovered.terminal().is_none(), "the run had not ended");
}

/// **Verification 3, row 2** · §12.3 rules 1 and 3: records appended after the candidate are
/// kept as tail, and a restore replays them from the journal.
#[test]
fn crash_matrix_appends_after_a_candidate_are_restored_from_the_journal() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    // The candidate is a read: the operation keeps going while the host persists the blob.
    drive(&mut runtime, &envelopes[4..6]);

    let restored = runtime.restore(&checkpoint);
    let cost = restored.restore_cost.unwrap();
    assert_eq!(
        cost.records_after_checkpoint, 2,
        "the two records appended after the candidate are replayed from the journal",
    );
    assert_eq!(cost.records_before_checkpoint, 0);
    assert_eq!(surface(&restored), surface(&runtime));
}

/// **Verification 3, row 3** · §12.3 rule 2: install does not require the covered head to still
/// be the current head.
#[test]
fn crash_matrix_install_when_the_covered_head_has_moved_on() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);
    let candidate = runtime.checkpoint();
    let covered = candidate.through_step_seq;

    drive(&mut runtime, &envelopes[4..]);
    assert_ne!(
        runtime.tx.head().unwrap().step_seq,
        covered,
        "the journal has moved past the covered head",
    );

    // The ack still names a prefix of *this* journal, which is the only precondition rule 2
    // leaves standing.
    let mut acked = Runtime::new();
    drive(&mut acked, &envelopes);
    let usage = acked
        .tx
        .note_checkpoint_acked(&candidate.boundary())
        .expect("a checkpoint that covers a prefix is ackable after the head moved");
    assert_eq!(
        usage.records, 3,
        "acking reclaims the covered prefix and keeps the rest as tail",
    );

    // And the blob still restores to the prefix it was taken over.
    let restored = runtime.restore(&candidate.decode().unwrap());
    assert_eq!(surface(&restored), surface(&runtime));
}

/// **Verification 3, row 4** · §12.3 rules 6, 7 and 10: after an ack the prefix may be
/// reclaimed, and a redelivery from down there is still answered — by reference.
///
/// This is the row that would have been silently wrong without the ledger: with the record
/// gone, a `prepare` that consulted only the journal would accept the input a **second** time.
#[test]
fn crash_matrix_ack_then_prune_then_restore_still_answers_a_redelivery() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    runtime
        .tx
        .note_checkpoint_acked(&checkpoint.boundary())
        .expect("the boundary names a prefix of this journal");

    // Retention reclaims everything the checkpoint covers: the restore gets the blob and an
    // empty journal, exactly as a pruned host would hand it over.
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
    assert_eq!(surface(&restored), surface(&runtime));

    // A redelivery of an input from the reclaimed prefix is acknowledged, not re-accepted.
    let redelivered = restored.prepare(&envelopes[3]);
    let RecordPreparation::Replayed(replay) = redelivered else {
        panic!("a redelivery below the checkpoint base must not be accepted again");
    };
    assert_eq!(replay.step_seq, WireU64::new(3));
    assert_eq!(
        replay.record_digest,
        *runtime.journal[3].record_digest(),
        "§12.3 rule 10 · the answer is the original step and record digest",
    );
    assert!(
        replay.committed_step.is_none() && replay.record.is_none(),
        "and it carries no step payload — the guarantee down there is idempotent \
             acknowledgement, not step reproduction",
    );

    // The operation still runs forward from where it was.
    drive(&mut restored, &envelopes[4..]);
    let mut uninterrupted = Runtime::new();
    drive(&mut uninterrupted, &envelopes);
    assert_eq!(surface(&restored), surface(&uninterrupted));
}

/// §12.3 rule 8 / Task 16 acceptance · identity does not move across a restore.
#[test]
fn a_restore_preserves_effect_terminal_attempt_and_handle_identity() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);

    let before_effects: Vec<String> = runtime
        .tx
        .pending_effects()
        .map(|effect| effect.effect_id.to_string())
        .collect();
    let before_handles =
        serde_json::to_value(runtime.driver.project_logical_state().context_vm.handles).unwrap();
    let before_attempts =
        serde_json::to_value(runtime.driver.project_logical_state().scheduler.attempts).unwrap();

    let checkpoint = runtime.checkpoint().decode().expect("verifies");
    let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);

    assert!(!before_effects.is_empty(), "the fixture has live identity");
    assert_eq!(
        restored
            .tx
            .pending_effects()
            .map(|effect| effect.effect_id.to_string())
            .collect::<Vec<_>>(),
        before_effects,
        "§5g-1 · appended-but-unpublished effects are re-exposed under their own ids",
    );
    assert_eq!(
        serde_json::to_value(restored.driver.project_logical_state().context_vm.handles).unwrap(),
        before_handles,
        "handle identity survives",
    );
    assert_eq!(
        serde_json::to_value(restored.driver.project_logical_state().scheduler.attempts).unwrap(),
        before_attempts,
        "task attempt identity survives",
    );

    // A terminal survives too, and closes the restored operation to the same inputs.
    drive(&mut restored, &envelopes[4..]);
    let terminal = restored.tx.terminal().cloned().expect("the run ended");
    let after_terminal = Runtime::restore_with(Some(&restored.checkpoint().decode().unwrap()), &[]);
    assert_eq!(
        after_terminal.tx.terminal(),
        Some(&terminal),
        "the terminal is restored as the same terminal, not re-derived",
    );
}

/// §12.3 · the hard tail limit is a retryable `CheckpointRequired`, and there is no latch.
///
/// The whole arc, because the latch this replaces was only visibly wrong at the *end* of it:
/// refuse → checkpoint → ack → the same envelope, unchanged, succeeds.
#[test]
fn a_full_tail_refuses_retryably_and_the_same_envelope_succeeds_after_an_ack() {
    let mut runtime = Runtime::new();
    runtime.submit(&syscall_config_with(|config| {
        config.recovery_policy = Some(super::super::command::RecoveryPolicy {
            provider_recovery_attempts: None,
            output_recovery_attempts: None,
            tail_bounds: Some(super::super::command::TailBoundsPolicy {
                soft_records: Some(WireU64::new(2)),
                hard_records: Some(WireU64::new(3)),
                soft_bytes: Some(WireU64::new(64 * 1024)),
                hard_bytes: Some(WireU64::new(1024 * 1024)),
            }),
        });
    }));
    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    ));

    let next = tools_resolved(
        "in-results",
        1_700_000_003_000,
        &effect_id(acted.step_seq),
        &[("call-1", "three sources found", false)],
    );
    let fault = runtime.reject(&next);
    assert_eq!(fault.code, KernelFaultCode::CheckpointRequired);
    assert!(fault.is_retryable(), "exactly one code says retry");

    // The refusal's shape is a contract in its own right — it is what tells four hosts to take
    // a checkpoint rather than to give up — so it is frozen as a fixture.
    let produced = json!({
        "expect": "checkpoint_required",
        "description":
            "Spec 12.3 · the next transaction would carry the journal tail past its hard limit, \
             so `prepare` refuses with the one retryable fault code and zero mutation. The \
             input was never accepted: after a checkpoint candidate is installed and acked, the \
             *same* envelope — same input id, same clock, same payload — is submitted again and \
             commits. There is no permanent overflow latch: an acked checkpoint takes the tail \
             pressure straight back to nominal.",
        "tail_bounds": {
            "soft_records": "2",
            "hard_records": "3",
            "soft_bytes": "65536",
            "hard_bytes": "1048576",
        },
        "tail_usage_at_refusal": {
            "records": runtime.tx.tail_usage().records.to_string(),
        },
        "refused_envelope": serde_json::to_value(&next).unwrap(),
        "fault": serde_json::to_value(&fault).unwrap(),
        "retryable": fault.is_retryable(),
    });
    let expected = golden(
        "reject_transaction_checkpoint_required_tail_full.json",
        &produced,
    );
    assert_eq!(produced, expected, "the CheckpointRequired refusal drifted");

    // Zero mutation: the refusal moved nothing, so the checkpoint below covers the same prefix
    // the refusal saw.
    let head_before = runtime.tx.head().expect("a head");
    let candidate = runtime.checkpoint();
    assert_eq!(candidate.through_step_seq, head_before.step_seq);

    runtime
        .tx
        .note_checkpoint_acked(&candidate.boundary())
        .expect("the ack names this journal's head");

    // The *same* envelope, byte for byte — §5e-3 forbids re-stamping the clock — now commits.
    let committed = runtime.submit(&next);
    assert_eq!(committed.step_seq, WireU64::new(3));
    assert_eq!(
        runtime.tx.tail_pressure(),
        TailPressure::Nominal,
        "an acked checkpoint moves the pressure straight back — there is no latch",
    );
}

/// §12.3 · crossing the soft watermark is advice, delivered once, on the commit that crossed it.
#[test]
fn the_soft_watermark_is_advice_delivered_once_on_the_crossing() {
    let mut runtime = Runtime::new();
    let genesis = runtime.submit(&syscall_config_with(|config| {
        config.recovery_policy = Some(super::super::command::RecoveryPolicy {
            provider_recovery_attempts: None,
            output_recovery_attempts: None,
            tail_bounds: Some(super::super::command::TailBoundsPolicy {
                soft_records: Some(WireU64::new(2)),
                hard_records: Some(WireU64::new(8)),
                soft_bytes: Some(WireU64::new(64 * 1024)),
                hard_bytes: Some(WireU64::new(1024 * 1024)),
            }),
        });
    }));
    assert!(
        genesis.checkpoint_advice.is_none(),
        "one record is not a watermark crossing",
    );

    let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
    let advice = started
        .checkpoint_advice
        .expect("the second record takes the tail to the soft watermark");
    assert_eq!(advice.through_step_seq, started.step_seq);
    assert_eq!(advice.usage.records, 2);
    assert_eq!(advice.bounds.soft_records, WireU64::new(2));

    let acted = runtime.submit(&provider_result(
        "in-acted",
        1_700_000_002_000,
        &effect_id(started.step_seq),
        vec![tool_call("call-1", "search", json!({"q": "sources"}))],
    ));
    assert!(
        acted.checkpoint_advice.is_none(),
        "advice is edge-triggered: staying over the watermark is not news",
    );
}

/// §12.3 rule 9 · rebuilding after a poisoned transaction costs O(tail) when a checkpoint
/// exists.
///
/// The failure this replaces re-executed every accepted input of the run, so the cost of one
/// CAS conflict grew with the age of the operation. Here the recovery path *is* the restore
/// path, and the counter says so.
#[test]
fn rebuilding_after_a_conflict_costs_the_tail_not_the_run() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..6]);
    let checkpoint = runtime.checkpoint().decode().expect("verifies");

    // A CAS conflict poisons the transaction: the journal moved under this runtime and §8.3
    // leaves exactly one way forward.
    let preparation = runtime.prepare(&envelopes[6]);
    let token = preparation.token().expect("prepared").clone();
    let fault = runtime.tx.note_append_conflict(&token, None);
    assert_eq!(fault.code, KernelFaultCode::TransactionConflict);
    assert!(runtime.tx.is_poisoned());

    let rebuilt = Runtime::restore_with(Some(&checkpoint), &[]);
    let cost = rebuilt.restore_cost.unwrap();
    assert_eq!(
        cost.total_transitions(),
        0,
        "the rebuild reads the checkpoint and nothing else — O(tail), not O(run)",
    );
    assert!(!rebuilt.tx.is_poisoned());
}

/// A checkpoint whose logical state was edited fails the restore's own re-projection check.
///
/// The point of the check is that it catches a *hydration* gap as well as a tampered blob: both
/// present as "the state that came back is not the state that was captured".
#[test]
fn a_restore_that_does_not_reproduce_the_captured_state_fails_closed() {
    let envelopes = turn_envelopes();
    let mut runtime = Runtime::new();
    drive(&mut runtime, &envelopes[..4]);
    let checkpoint = runtime.checkpoint().decode().expect("verifies");

    // Re-assemble the same header over a *different* logical state, so every digest is
    // internally consistent and only the state is wrong. A blob edited in storage would be
    // caught by `verify()`; this one gets past it and has to be caught by the re-projection.
    // A *derived* field is the honest probe here: the partition token counters come back from
    // re-pushing the messages, so a forged counter is exactly what a hydration gap would look
    // like from the outside — the state that came back is not the state that was captured.
    let mut state = checkpoint.logical_state().clone();
    state.context_vm.partition_tokens.history += 7;
    let forged = KernelCheckpoint::assemble(CheckpointDraft {
        operation_id: operation(),
        genesis_digest: checkpoint.genesis_digest().clone(),
        base_step_seq: checkpoint.base_step_seq(),
        base_record_digest: checkpoint.base_record_digest().clone(),
        through_step_seq: checkpoint.through_step_seq(),
        covered_transaction_head_digest: checkpoint.covered_transaction_head_digest().clone(),
        logical_state: state,
        tail_inputs: Vec::new(),
    })
    .expect("a self-consistent checkpoint over a different state");

    let error = restore_operation(
        Some(&forged),
        &[],
        ConfigDefaults::default(),
        InMemoryRecordIndex::new(),
    )
    .expect_err("a state that does not come back is a refusal, not a warning");
    assert_eq!(error.code, KernelFaultCode::CheckpointCorrupted);
    assert!(error.message.contains("restored logical state hashes to"));
}

/// §5e-5 · the tail bound the transaction enforces is the one the genesis record froze, not
/// the binary's baseline.
#[test]
fn the_tail_bound_comes_from_the_genesis_configuration() {
    let mut runtime = Runtime::new();
    assert_eq!(
        runtime.tx.bounds(),
        TailBounds::DEFAULT,
        "before genesis the transaction runs on the bootstrap baseline",
    );

    runtime.submit(&syscall_config_with(|config| {
        config.recovery_policy = Some(super::super::command::RecoveryPolicy {
            provider_recovery_attempts: None,
            output_recovery_attempts: None,
            tail_bounds: Some(super::super::command::TailBoundsPolicy {
                soft_records: Some(WireU64::new(4)),
                hard_records: Some(WireU64::new(8)),
                soft_bytes: Some(WireU64::new(64 * 1024)),
                hard_bytes: Some(WireU64::new(256 * 1024)),
            }),
        });
    }));

    assert_eq!(
        runtime.tx.bounds(),
        TailBounds::new(4, 8, 64 * 1024, 256 * 1024).unwrap(),
        "the genesis record's resolved configuration is what bounds the tail",
    );
    assert_eq!(
        runtime
            .tx
            .config()
            .expect("configured")
            .recovery_policy
            .tail_bounds,
        runtime.tx.bounds(),
        "and it is frozen in the record, so a rebuild re-derives the same bound",
    );
}

// Durable content checkpoint projections.

#[test]
fn structured_checkpoint_body_uses_durable_content_and_restores_media() {
    let content = Content::Parts(vec![
        ContentPart::Text {
            text: "caption".into(),
        },
        ContentPart::Image {
            source: DurableSource::Base64 {
                data: "aW1hZ2U=".into(),
            },
            media_type: Some("image/png".into()),
            detail: Some("low".into()),
        },
    ]);
    let durable = content_to_durable(&content).unwrap();
    let restored = content_from_durable(&durable).unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(content).unwrap()
    );
}

#[test]
fn checkpoint_rejects_unrestorable_durable_file_source() {
    let content = DurableContent {
        blocks: vec![DurableContentBlock::File {
            source: DurableSource::FileId {
                id: "file-1".into(),
                affinity: crate::types::durable_content::EndpointAffinity {
                    provider_id: "provider".into(),
                    endpoint_id: "endpoint".into(),
                },
            },
            media_type: Some("application/pdf".into()),
            provider_options: None,
        }],
    };
    assert!(content_from_durable(&content).is_err());
}

#[test]
fn structured_tool_result_checkpoint_form_keeps_correlation_and_blocks() {
    let result = DurableToolResult {
        call_id: "call-screenshot".into(),
        is_error: false,
        blocks: vec![
            DurableContentBlock::Text {
                text: "captured".into(),
            },
            DurableContentBlock::Image {
                source: DurableSource::Base64 {
                    data: "aW1hZ2U=".into(),
                },
                media_type: Some("image/png".into()),
                provider_options: None,
            },
            DurableContentBlock::File {
                source: DurableSource::FileId {
                    id: "file-7".into(),
                    affinity: crate::types::durable_content::EndpointAffinity {
                        provider_id: "openai".into(),
                        endpoint_id: "responses".into(),
                    },
                },
                media_type: Some("application/pdf".into()),
                provider_options: None,
            },
        ],
    };
    let content = content_from_durable_tool_result(&result).unwrap();
    let durable = durable_tool_result_from_content(&content).unwrap();
    assert_eq!(durable, result);
    let Content::Parts(parts) = content else {
        panic!("tool result must restore as parts")
    };
    let [
        ContentPart::ToolResult {
            call_id,
            output,
            durable_content,
            ..
        },
    ] = parts.as_slice()
    else {
        panic!("tool result must have one correlated part")
    };
    assert_eq!(call_id.as_str(), "call-screenshot");
    assert_eq!(output, "captured");
    assert_eq!(durable_content.as_ref().unwrap().blocks, result.blocks);
}

#[test]
fn plain_multi_tool_results_use_the_canonical_checkpoint_carrier() {
    let content = Content::Parts(vec![
        ContentPart::ToolResult {
            call_id: "call-1".into(),
            output: "first".into(),
            is_error: false,
            durable_content: None,
        },
        ContentPart::ToolResult {
            call_id: "call-2".into(),
            output: "second".into(),
            is_error: true,
            durable_content: None,
        },
    ]);
    assert!(
        message_body_parts(&CoreMessage::tool(match content.clone() {
            Content::Parts(parts) => parts,
            Content::Text(_) => unreachable!(),
        }))
        .is_none(),
        "multiple call ids require the structured canonical carrier"
    );
    assert_eq!(
        durable_tool_results_from_content(&content)
            .expect("canonical tool results")
            .len(),
        2
    );
    assert!(
        content_to_durable(&content).is_err(),
        "nested tool results remain forbidden"
    );
}

#[test]
fn structured_multi_tool_result_message_has_one_durable_envelope_per_call() {
    let content = Content::Parts(vec![
        ContentPart::ToolResult {
            call_id: "call-1".into(),
            output: "first".into(),
            is_error: false,
            durable_content: Some(DurableContent::text("first")),
        },
        ContentPart::ToolResult {
            call_id: "call-2".into(),
            output: "second".into(),
            is_error: true,
            durable_content: None,
        },
    ]);
    let results = durable_tool_results_from_content(&content).unwrap();
    assert_eq!(
        results
            .iter()
            .map(|result| result.call_id.as_str())
            .collect::<Vec<_>>(),
        ["call-1", "call-2"]
    );
    assert_eq!(
        durable_tool_results_from_content(&content_from_durable_tool_results(&results).unwrap())
            .unwrap(),
        results,
    );
}
