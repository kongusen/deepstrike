//! Development-only deterministic replay laboratory.
//!
//! The lab consumes the stable kernel ABI, but is deliberately not re-exported
//! by any SDK. Its trace and report formats are experiment artifacts, not a
//! production resume or compatibility surface.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use deepstrike_core::context::policy::{ContextPolicyV1, PPM_SCALE};
use deepstrike_core::runtime::kernel::RunConfig;
use deepstrike_core::runtime::{
    KERNEL_ABI_VERSION, KernelAction, KernelEffect, KernelInput, KernelInputEvent,
    KernelObservation, KernelRuntime, KernelSnapshotPolicyV2, KernelSnapshotV2, KernelStep,
};
use deepstrike_core::scheduler::policy::SchedulerBudget;
use deepstrike_core::types::message::{Content, ContentPart, Message, Role};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const TRACE_VERSION: u32 = 1;
pub const REPLAY_REPORT_VERSION: u32 = 1;
pub const LAB_VERSION: &str = "deepstrike-lab-v1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicalEffectKey(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TracePoint {
    Transaction(u32),
    ProviderTurn(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactProbe {
    pub id: String,
    pub introduced_at: TracePoint,
    pub required_at: TracePoint,
    pub canonical_value: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptable_handles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEffectRecord {
    pub kind: String,
    pub logical_effect_key: LogicalEffectKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceTransaction {
    pub ordinal: u32,
    pub input: KernelInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_effect_key: Option<LogicalEffectKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<TraceEffectRecord>,
}

/// Storage-independent trace. Blob keys are `fnv1a64:<hex>` digests of their
/// UTF-8 fixture content, so replay never needs the production blob store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceV1 {
    pub trace_version: u32,
    pub abi_version: u32,
    pub initial_policy: KernelSnapshotPolicyV2,
    pub transactions: Vec<TraceTransaction>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fixture_blobs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<FactProbe>,
    pub deterministic_metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabContextOverrides {
    /// An experiment-only replacement applied after the stable policy. This is
    /// intentionally not wired into `RunConfig` or any SDK type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_recent_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_after_compress_ppm: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ReplayOptions {
    pub context_policy: Option<ContextPolicyV1>,
    pub lab_overrides: LabContextOverrides,
}

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("invalid trace: {message}")]
    InvalidTrace { message: String },
    #[error("trace_not_comparable: {message}")]
    TraceNotComparable { message: String },
    #[error("kernel replay fault at transaction {transaction}: {message}")]
    KernelFault { transaction: u32, message: String },
    #[error("failed to normalize replay report: {0}")]
    Normalize(#[from] serde_json::Error),
}

impl ReplayError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidTrace { .. } => "invalid_trace",
            Self::TraceNotComparable { .. } => "trace_not_comparable",
            Self::KernelFault { .. } => "kernel_fault",
            Self::Normalize(_) => "normalize_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTurnMetrics {
    pub provider_turn: u32,
    pub transaction: u32,
    pub logical_effect_key: LogicalEffectKey,
    pub render_tokens: u32,
    pub rho_ppm: u32,
    pub frozen_prefix_len: Option<usize>,
    pub reusable_prefix_turns: usize,
    pub reusable_prefix_tokens: u32,
    pub invalidates_prefix_at: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct T1Metrics {
    pub provider_turns: Vec<ProviderTurnMetrics>,
    pub compression_count: u32,
    pub compression_by_type: BTreeMap<String, u32>,
    pub archived_messages: u64,
    pub archived_bytes: u64,
    pub prefix_invalidation_count: u32,
    pub external_payload_spool_count: u32,
    pub external_payload_bytes: u64,
    pub knowledge_eviction_count: u32,
    pub knowledge_evicted_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactProbeResult {
    pub id: String,
    pub required_at: TracePoint,
    pub retained: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantResult {
    pub name: String,
    pub provider_turn: u32,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct T2Metrics {
    pub fact_probes: Vec<FactProbeResult>,
    pub invariants: Vec<InvariantResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub report_version: u32,
    pub lab_version: String,
    pub trace_digest: String,
    pub trace_version: u32,
    pub abi_version: u32,
    pub comparable: bool,
    pub policy: Option<ContextPolicyV1>,
    pub lab_overrides: LabContextOverrides,
    pub t1: T1Metrics,
    pub t2: T2Metrics,
}

impl ReplayReport {
    pub fn normalized_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

pub fn fixture_blob_key(content: &str) -> String {
    format!("fnv1a64:{:016x}", stable_hash(content.as_bytes()))
}

pub fn export_snapshot_trace(
    snapshot: &KernelSnapshotV2,
    fixture_blobs: BTreeMap<String, String>,
    probes: Vec<FactProbe>,
) -> Result<TraceV1, ReplayError> {
    if snapshot.abi_version != KERNEL_ABI_VERSION {
        return Err(invalid_trace("snapshot ABI version is not supported"));
    }
    for (key, content) in &fixture_blobs {
        if fixture_blob_key(content) != *key {
            return Err(invalid_trace(format!(
                "fixture blob {key} does not match its content digest"
            )));
        }
    }
    validate_probe_shapes(&probes)?;

    let policy = SchedulerBudget::try_from(&snapshot.initial_policy)
        .map_err(|message| invalid_trace(format!("invalid initial policy: {message}")))?;
    let mut runtime = KernelRuntime::new(policy);
    let mut pending: HashMap<String, TraceEffectRecord> = HashMap::new();
    let mut transactions = Vec::with_capacity(snapshot.accepted_inputs.len());

    for (index, input) in snapshot.accepted_inputs.iter().cloned().enumerate() {
        let ordinal = (index + 1) as u32;
        let logical_effect_key = if let Some(effect_id) = result_effect_id(&input.event) {
            Some(
                pending
                    .remove(effect_id)
                    .ok_or_else(|| {
                        invalid_trace(format!(
                            "transaction {ordinal} resolves unknown effect {effect_id}"
                        ))
                    })?
                    .logical_effect_key,
            )
        } else {
            None
        };
        let step = runtime.step(input.clone());
        ensure_step_ok(ordinal, &step)?;
        let mut effects = Vec::with_capacity(step.actions.len());
        for action in &step.actions {
            let record = effect_record(action)?;
            if effect_expects_result(&action.effect) {
                if pending
                    .insert(action.effect_id.clone(), record.clone())
                    .is_some()
                {
                    return Err(invalid_trace(format!(
                        "transaction {ordinal} repeats effect id {}",
                        action.effect_id
                    )));
                }
            }
            effects.push(record);
        }
        transactions.push(TraceTransaction {
            ordinal,
            input,
            logical_effect_key,
            effects,
        });
    }

    let mut deterministic_metadata = BTreeMap::new();
    deterministic_metadata.insert("exporter".into(), "snapshot.accepted_inputs".into());
    deterministic_metadata.insert("lab_version".into(), LAB_VERSION.into());
    let trace = TraceV1 {
        trace_version: TRACE_VERSION,
        abi_version: snapshot.abi_version,
        initial_policy: snapshot.initial_policy.clone(),
        transactions,
        fixture_blobs,
        probes,
        deterministic_metadata,
    };
    validate_trace(&trace)?;
    Ok(trace)
}

pub fn replay_fork(trace: &TraceV1, options: &ReplayOptions) -> Result<ReplayReport, ReplayError> {
    validate_trace(trace)?;
    validate_oracle_keys(trace)?;
    let trace_digest = digest_json(trace)?;
    let policy = SchedulerBudget::try_from(&trace.initial_policy)
        .map_err(|message| invalid_trace(format!("invalid initial policy: {message}")))?;
    let max_tokens = policy.max_tokens;
    let mut runtime = KernelRuntime::new(policy);
    let mut pending: BTreeMap<LogicalEffectKey, PendingReplayEffect> = BTreeMap::new();
    let expected_comparable = comparable_effects(
        trace
            .transactions
            .iter()
            .flat_map(|transaction| transaction.effects.iter()),
    );
    let mut actual_comparable = Vec::new();
    let replay_policy = effective_policy(trace, options);
    let preserve_recent_turns = replay_policy
        .as_ref()
        .map_or(2, |policy| policy.preserve_recent_turns as usize);
    let mut collector = ReportCollector::new(trace, max_tokens, preserve_recent_turns);

    for transaction in &trace.transactions {
        if is_synthetic_host_result(&transaction.input.event) {
            continue;
        }
        let mut input = transaction.input.clone();
        apply_policy_override(&mut input.event, options)?;
        if result_effect_id(&input.event).is_some() {
            let key = transaction.logical_effect_key.as_ref().ok_or_else(|| {
                not_comparable(format!(
                    "transaction {} has an effect result without a logical_effect_key",
                    transaction.ordinal
                ))
            })?;
            let current = pending.remove(key).ok_or_else(|| {
                not_comparable(format!(
                    "transaction {} has no pending effect for {}",
                    transaction.ordinal, key.0
                ))
            })?;
            set_result_effect_id(&mut input.event, current.effect_id)?;
        }
        let step = runtime.step(input);
        ensure_step_ok(transaction.ordinal, &step)?;
        drive_step(
            trace,
            &trace_digest,
            transaction.ordinal,
            step,
            &mut runtime,
            &mut pending,
            &mut actual_comparable,
            &mut collector,
        )?;
    }

    if expected_comparable != actual_comparable {
        return Err(not_comparable(format!(
            "provider/tool demand changed; expected {}, replay produced {}",
            display_effects(&expected_comparable),
            display_effects(&actual_comparable)
        )));
    }

    let (t1, t2) = collector.finish();
    Ok(ReplayReport {
        report_version: REPLAY_REPORT_VERSION,
        lab_version: LAB_VERSION.into(),
        trace_digest,
        trace_version: trace.trace_version,
        abi_version: trace.abi_version,
        comparable: true,
        policy: replay_policy,
        lab_overrides: options.lab_overrides.clone(),
        t1,
        t2,
    })
}

#[derive(Debug, Clone)]
struct PendingReplayEffect {
    effect_id: String,
}

fn drive_step(
    trace: &TraceV1,
    trace_digest: &str,
    transaction: u32,
    step: KernelStep,
    runtime: &mut KernelRuntime,
    pending: &mut BTreeMap<LogicalEffectKey, PendingReplayEffect>,
    actual_comparable: &mut Vec<(String, LogicalEffectKey)>,
    collector: &mut ReportCollector,
) -> Result<(), ReplayError> {
    collector.observe(transaction, &step, runtime)?;
    let operation_id = step.operation_id.clone();
    let mut synthetic = Vec::new();
    for action in step.actions {
        let record = effect_record(&action)?;
        if matches!(
            action.effect,
            KernelEffect::CallProvider { .. } | KernelEffect::ExecuteTool { .. }
        ) {
            actual_comparable.push((record.kind.clone(), record.logical_effect_key.clone()));
        }
        match &action.effect {
            KernelEffect::SpoolLargeResult { .. } => {
                let reference = synthetic_ref("spool", trace_digest, &record.logical_effect_key);
                collector.synthetic_handles.insert(reference.clone());
                synthetic.push((
                    KernelInputEvent::LargeResultSpoolResult {
                        effect_id: action.effect_id.clone(),
                        spool_ref: Some(reference),
                        error: None,
                    },
                    record.logical_effect_key.clone(),
                ));
            }
            KernelEffect::ArchivePageOut { .. } => {
                let reference = synthetic_ref("archive", trace_digest, &record.logical_effect_key);
                collector.synthetic_handles.insert(reference.clone());
                synthetic.push((
                    KernelInputEvent::PageOutArchiveResult {
                        effect_id: action.effect_id.clone(),
                        archive_ref: Some(reference),
                        error: None,
                    },
                    record.logical_effect_key.clone(),
                ));
            }
            effect if effect_expects_result(effect) => {
                if pending
                    .insert(
                        record.logical_effect_key.clone(),
                        PendingReplayEffect {
                            effect_id: action.effect_id.clone(),
                        },
                    )
                    .is_some()
                {
                    return Err(not_comparable(format!(
                        "duplicate pending logical effect key {}",
                        record.logical_effect_key.0
                    )));
                }
            }
            _ => {}
        }
    }
    for (event, logical_key) in synthetic {
        let event_id = format!(
            "lab-synthetic-{:016x}",
            stable_hash(format!("{trace_digest}:{}", logical_key.0).as_bytes())
        );
        let input = KernelInput::correlated(operation_id.clone(), event_id, 0, event);
        let step = runtime.step(input);
        ensure_step_ok(transaction, &step)?;
        drive_step(
            trace,
            trace_digest,
            transaction,
            step,
            runtime,
            pending,
            actual_comparable,
            collector,
        )?;
    }
    let _ = trace;
    Ok(())
}

fn effect_record(action: &KernelAction) -> Result<TraceEffectRecord, ReplayError> {
    let (kind, material) = match &action.effect {
        KernelEffect::CallProvider { .. } => ("provider", action.causation_id.clone()),
        KernelEffect::ExecuteTool { calls } => {
            let calls = calls
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "call_id": call.id.as_str(),
                        "tool": call.name.as_str(),
                        "arguments": canonical_value(call.arguments.clone()),
                    })
                })
                .collect::<Vec<_>>();
            ("tool", digest_json(&calls)?)
        }
        KernelEffect::RequestApproval { requests } => ("approval", digest_json(requests)?),
        KernelEffect::SpawnWorkflow { nodes, budget } => {
            ("workflow_spawn", digest_json(&(nodes, budget))?)
        }
        KernelEffect::PreemptSubAgents { agent_ids, reason } => {
            ("preempt", digest_json(&(agent_ids, reason))?)
        }
        KernelEffect::PersistMemory { memory } => ("memory_persist", digest_json(memory)?),
        KernelEffect::QueryMemory { query, requested_k } => {
            ("memory_query", digest_json(&(query, requested_k))?)
        }
        KernelEffect::SpoolLargeResult {
            call_id,
            tool,
            output,
            ..
        } => ("spool", digest_json(&(call_id, tool, output))?),
        KernelEffect::ArchivePageOut {
            action,
            archived,
            tier,
            ..
        } => ("archive", digest_json(&(action, archived, tier))?),
        KernelEffect::EvaluateMilestone { phase_id, .. } => ("milestone", phase_id.clone()),
        KernelEffect::Done { .. } => ("done", action.causation_id.clone()),
    };
    let key = if matches!(kind, "provider" | "tool") {
        format!("{kind}:{material}")
    } else {
        format!("{kind}:{}:{material}", action.causation_id)
    };
    Ok(TraceEffectRecord {
        kind: kind.into(),
        logical_effect_key: LogicalEffectKey(key),
    })
}

fn effect_expects_result(effect: &KernelEffect) -> bool {
    !matches!(effect, KernelEffect::Done { .. })
}

fn result_effect_id(event: &KernelInputEvent) -> Option<&str> {
    match event {
        KernelInputEvent::ProviderResult { effect_id, .. }
        | KernelInputEvent::ProviderError { effect_id, .. }
        | KernelInputEvent::ToolResults { effect_id, .. }
        | KernelInputEvent::ApprovalResult { effect_id, .. }
        | KernelInputEvent::WorkflowSpawnResult { effect_id, .. }
        | KernelInputEvent::PreemptResult { effect_id, .. }
        | KernelInputEvent::MemoryPersistResult { effect_id, .. }
        | KernelInputEvent::MemoryQueryResult { effect_id, .. }
        | KernelInputEvent::LargeResultSpoolResult { effect_id, .. }
        | KernelInputEvent::PageOutArchiveResult { effect_id, .. }
        | KernelInputEvent::MilestoneResult { effect_id, .. } => Some(effect_id),
        _ => None,
    }
}

fn set_result_effect_id(event: &mut KernelInputEvent, value: String) -> Result<(), ReplayError> {
    match event {
        KernelInputEvent::ProviderResult { effect_id, .. }
        | KernelInputEvent::ProviderError { effect_id, .. }
        | KernelInputEvent::ToolResults { effect_id, .. }
        | KernelInputEvent::ApprovalResult { effect_id, .. }
        | KernelInputEvent::WorkflowSpawnResult { effect_id, .. }
        | KernelInputEvent::PreemptResult { effect_id, .. }
        | KernelInputEvent::MemoryPersistResult { effect_id, .. }
        | KernelInputEvent::MemoryQueryResult { effect_id, .. }
        | KernelInputEvent::LargeResultSpoolResult { effect_id, .. }
        | KernelInputEvent::PageOutArchiveResult { effect_id, .. }
        | KernelInputEvent::MilestoneResult { effect_id, .. } => {
            *effect_id = value;
            Ok(())
        }
        _ => Err(invalid_trace("attempted to bind a non-result transaction")),
    }
}

fn is_synthetic_host_result(event: &KernelInputEvent) -> bool {
    matches!(
        event,
        KernelInputEvent::LargeResultSpoolResult { .. }
            | KernelInputEvent::PageOutArchiveResult { .. }
    )
}

fn apply_policy_override(
    event: &mut KernelInputEvent,
    options: &ReplayOptions,
) -> Result<(), ReplayError> {
    let KernelInputEvent::ConfigureRun { config } = event else {
        return Ok(());
    };
    if let Some(policy) = &options.context_policy {
        policy
            .validate()
            .map_err(|message| invalid_trace(format!("invalid replay policy: {message}")))?;
        config.context_policy = Some(policy.clone());
    }
    if options.lab_overrides.preserve_recent_turns.is_some()
        || options.lab_overrides.target_after_compress_ppm.is_some()
    {
        let policy = config
            .context_policy
            .as_mut()
            .ok_or_else(|| invalid_trace("lab context overrides require a ContextPolicyV1"))?;
        if let Some(value) = options.lab_overrides.preserve_recent_turns {
            policy.preserve_recent_turns = value;
        }
        if let Some(value) = options.lab_overrides.target_after_compress_ppm {
            policy.target_after_compress_ppm = value;
        }
        policy
            .validate()
            .map_err(|message| invalid_trace(format!("invalid lab override: {message}")))?;
    }
    Ok(())
}

fn effective_policy(trace: &TraceV1, options: &ReplayOptions) -> Option<ContextPolicyV1> {
    let mut found = None;
    for transaction in &trace.transactions {
        if let KernelInputEvent::ConfigureRun { config } = &transaction.input.event {
            found = config.context_policy.clone();
            break;
        }
    }
    if options.context_policy.is_some() {
        found = options.context_policy.clone();
    }
    if let Some(policy) = found.as_mut() {
        if let Some(value) = options.lab_overrides.preserve_recent_turns {
            policy.preserve_recent_turns = value;
        }
        if let Some(value) = options.lab_overrides.target_after_compress_ppm {
            policy.target_after_compress_ppm = value;
        }
    }
    found
}

struct ReportCollector<'a> {
    trace: &'a TraceV1,
    max_tokens: u32,
    t1: T1Metrics,
    renders: Vec<RenderRecord>,
    previous_context: Option<deepstrike_core::context::renderer::RenderedContext>,
    pending_invalidation: Option<usize>,
    archive_accounting: ArchiveAccounting,
    preserve_recent_turns: usize,
    synthetic_handles: BTreeSet<String>,
    system_facts: Vec<String>,
    pinned_knowledge_facts: Vec<String>,
    recent_source_messages: Vec<Message>,
    seen_source_transactions: BTreeSet<u32>,
}

struct RenderRecord {
    transaction: u32,
    provider_turn: u32,
    text: String,
    context: deepstrike_core::context::renderer::RenderedContext,
}

impl<'a> ReportCollector<'a> {
    fn new(trace: &'a TraceV1, max_tokens: u32, preserve_recent_turns: usize) -> Self {
        Self {
            trace,
            max_tokens,
            t1: T1Metrics::default(),
            renders: Vec::new(),
            previous_context: None,
            pending_invalidation: None,
            archive_accounting: ArchiveAccounting::default(),
            preserve_recent_turns,
            synthetic_handles: BTreeSet::new(),
            system_facts: Vec::new(),
            pinned_knowledge_facts: Vec::new(),
            recent_source_messages: Vec::new(),
            seen_source_transactions: BTreeSet::new(),
        }
    }

    fn observe(
        &mut self,
        transaction: u32,
        step: &KernelStep,
        runtime: &KernelRuntime,
    ) -> Result<(), ReplayError> {
        if self.seen_source_transactions.insert(transaction)
            && let Some(source) = self
                .trace
                .transactions
                .iter()
                .find(|tx| tx.ordinal == transaction)
        {
            match &source.input.event {
                KernelInputEvent::AddSystemMessage { content, .. } => {
                    if !self.system_facts.contains(content) {
                        self.system_facts.push(content.clone());
                    }
                }
                KernelInputEvent::AddKnowledgeMessage {
                    content,
                    pinned: true,
                    ..
                } => {
                    if !self.pinned_knowledge_facts.contains(content) {
                        self.pinned_knowledge_facts.push(content.clone());
                    }
                }
                KernelInputEvent::AddHistoryMessage { message, .. } => {
                    self.recent_source_messages.push(message.clone());
                }
                KernelInputEvent::PreloadHistory { messages } => {
                    self.recent_source_messages.extend(messages.iter().cloned());
                }
                KernelInputEvent::ProviderResult { message, .. } => {
                    self.recent_source_messages.push(message.clone());
                }
                KernelInputEvent::ToolResults { results, .. } => {
                    let parts = results
                        .iter()
                        .map(|result| ContentPart::ToolResult {
                            call_id: result.call_id.clone(),
                            output: message_content_text(&result.output),
                            is_error: result.is_error,
                        })
                        .collect();
                    self.recent_source_messages.push(Message::tool(parts));
                }
                _ => {}
            }
        }
        for observation in &step.observations {
            match observation {
                KernelObservation::Compressed {
                    action,
                    archived_count,
                    invalidates_prefix_at,
                    ..
                } => {
                    self.t1.compression_count += 1;
                    *self
                        .t1
                        .compression_by_type
                        .entry(format!("{action:?}").to_ascii_lowercase())
                        .or_default() += 1;
                    self.archive_accounting
                        .observe_compressed(u64::from(*archived_count));
                    if let Some(index) = invalidates_prefix_at {
                        self.t1.prefix_invalidation_count += 1;
                        self.pending_invalidation = Some(*index);
                    }
                }
                KernelObservation::KnowledgeSwept {
                    removed_keys,
                    tokens_freed,
                    ..
                } => {
                    self.t1.knowledge_eviction_count += removed_keys.len() as u32;
                    self.t1.knowledge_evicted_tokens += u64::from(*tokens_freed);
                }
                KernelObservation::LargeResultSpooled { original_size, .. } => {
                    self.t1.external_payload_spool_count += 1;
                    self.t1.external_payload_bytes += u64::from(*original_size);
                }
                KernelObservation::PageOutArchived { message_count, .. } => {
                    self.archive_accounting
                        .observe_committed(u64::from(*message_count));
                }
                _ => {}
            }
        }
        for action in &step.actions {
            if let KernelEffect::ArchivePageOut { archived, .. } = &action.effect {
                self.t1.archived_bytes += archived.iter().map(message_bytes).sum::<usize>() as u64;
            }
            let KernelEffect::CallProvider { context, .. } = &action.effect else {
                continue;
            };
            let provider_turn = (self.renders.len() + 1) as u32;
            let render_tokens = context_tokens(runtime, context);
            let (reusable_prefix_turns, reusable_prefix_tokens) = self
                .previous_context
                .as_ref()
                .map(|previous| reusable_prefix(runtime, previous, context))
                .unwrap_or((0, 0));
            let key = effect_record(action)?.logical_effect_key;
            self.t1.provider_turns.push(ProviderTurnMetrics {
                provider_turn,
                transaction,
                logical_effect_key: key,
                render_tokens,
                rho_ppm: ((u64::from(render_tokens) * u64::from(PPM_SCALE))
                    / u64::from(self.max_tokens.max(1)))
                .min(u64::from(PPM_SCALE)) as u32,
                frozen_prefix_len: context.frozen_prefix_len,
                reusable_prefix_turns,
                reusable_prefix_tokens,
                invalidates_prefix_at: self.pending_invalidation.take(),
            });
            self.renders.push(RenderRecord {
                transaction,
                provider_turn,
                text: render_text(context),
                context: context.clone(),
            });
            self.previous_context = Some(context.clone());
        }
        Ok(())
    }

    fn finish(self) -> (T1Metrics, T2Metrics) {
        let mut this = self;
        this.t1.archived_messages = this.archive_accounting.reported();
        let mut t2 = T2Metrics::default();
        for probe in &this.trace.probes {
            let introduced = point_transaction(this.trace, &probe.introduced_at)
                .expect("validated probe introduction point");
            let render = required_render(&this.renders, &probe.required_at)
                .filter(|render| render.transaction >= introduced);
            let matched = render.and_then(|render| {
                std::iter::once(&probe.canonical_value)
                    .chain(probe.aliases.iter())
                    .chain(probe.acceptable_handles.iter())
                    .find(|candidate| contains_folded(&render.text, candidate))
                    .cloned()
            });
            t2.fact_probes.push(FactProbeResult {
                id: probe.id.clone(),
                required_at: probe.required_at.clone(),
                retained: matched.is_some(),
                matched,
            });
        }
        let mut accepted_handles = this
            .trace
            .probes
            .iter()
            .flat_map(|probe| probe.acceptable_handles.iter().cloned())
            .chain(this.trace.fixture_blobs.keys().cloned())
            .collect::<BTreeSet<_>>();
        accepted_handles.extend(this.synthetic_handles.iter().cloned());
        for render in &this.renders {
            let pairing = strict_tool_pairing(&render.context.turns);
            push_invariant(
                &mut t2,
                "tool_call_result_pairing",
                render,
                pairing,
                if pairing {
                    "all tool calls are paired"
                } else {
                    "tool call/result pairing is invalid"
                },
            );
            let orphan = find_orphan_handle(&render.text, &accepted_handles);
            push_invariant(
                &mut t2,
                "no_orphan_handle",
                render,
                orphan.is_none(),
                orphan
                    .as_deref()
                    .unwrap_or("all handles resolve to fixtures or probe allowances"),
            );
            let recent_ok = recent_messages_retained(
                &render.text,
                &this.recent_source_messages,
                this.preserve_recent_turns,
            );
            push_invariant(
                &mut t2,
                "recent_context_unit_retention",
                render,
                recent_ok,
                if recent_ok {
                    "declared recent source messages are retained"
                } else {
                    "a protected recent source message is missing"
                },
            );
            let system_ok = this
                .system_facts
                .iter()
                .all(|fact| render.context.system_stable.contains(fact));
            push_invariant(
                &mut t2,
                "system_preservation",
                render,
                system_ok,
                if system_ok {
                    "system anchors are preserved"
                } else {
                    "a system anchor is missing"
                },
            );
            let knowledge_ok = this
                .pinned_knowledge_facts
                .iter()
                .all(|fact| render.context.system_knowledge.contains(fact));
            push_invariant(
                &mut t2,
                "knowledge_preservation",
                render,
                knowledge_ok,
                if knowledge_ok {
                    "pinned knowledge anchors are preserved"
                } else {
                    "a pinned knowledge anchor is missing"
                },
            );
            let tokens = this.t1.provider_turns[(render.provider_turn - 1) as usize].render_tokens;
            let hard_limit = render.context.budget_overflow.is_none() && tokens <= this.max_tokens;
            push_invariant(
                &mut t2,
                "context_hard_limit",
                render,
                hard_limit,
                if hard_limit {
                    "render fits the declared hard limit"
                } else {
                    "render exceeds the declared hard limit"
                },
            );
        }
        (this.t1, t2)
    }
}

fn push_invariant(
    t2: &mut T2Metrics,
    name: &str,
    render: &RenderRecord,
    passed: bool,
    detail: &str,
) {
    t2.invariants.push(InvariantResult {
        name: name.into(),
        provider_turn: render.provider_turn,
        passed,
        detail: detail.into(),
    });
}

fn required_render<'a>(
    renders: &'a [RenderRecord],
    point: &TracePoint,
) -> Option<&'a RenderRecord> {
    match point {
        TracePoint::ProviderTurn(turn) => {
            renders.iter().find(|render| render.provider_turn == *turn)
        }
        TracePoint::Transaction(transaction) => renders
            .iter()
            .find(|render| render.transaction >= *transaction),
    }
}

fn render_text(context: &deepstrike_core::context::renderer::RenderedContext) -> String {
    let mut values = vec![
        context.system_stable.clone(),
        context.system_knowledge.clone(),
    ];
    values.extend(context.turns.iter().map(message_text));
    if let Some(state) = &context.state_turn {
        values.push(message_text(state));
    }
    values.join("\n")
}

fn message_text(message: &Message) -> String {
    match &message.content {
        Content::Text(text) => text.clone(),
        Content::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => text.clone(),
                ContentPart::ToolResult { output, .. } => output.clone(),
                ContentPart::Image { url, .. } => url.clone().unwrap_or_default(),
                ContentPart::Audio { .. } => "[audio]".into(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn message_bytes(message: &Message) -> usize {
    serde_json::to_vec(message).map_or(0, |bytes| bytes.len())
}

fn context_tokens(
    runtime: &KernelRuntime,
    context: &deepstrike_core::context::renderer::RenderedContext,
) -> u32 {
    runtime.count_tokens(&context.system_stable)
        + runtime.count_tokens(&context.system_knowledge)
        + context
            .turns
            .iter()
            .map(|message| runtime.count_tokens(&message_text(message)))
            .sum::<u32>()
        + context
            .state_turn
            .as_ref()
            .map(|message| runtime.count_tokens(&message_text(message)))
            .unwrap_or_default()
}

fn reusable_prefix(
    runtime: &KernelRuntime,
    previous: &deepstrike_core::context::renderer::RenderedContext,
    current: &deepstrike_core::context::renderer::RenderedContext,
) -> (usize, u32) {
    if previous.system_stable != current.system_stable
        || previous.system_knowledge != current.system_knowledge
    {
        return (0, 0);
    }
    let turns = previous
        .turns
        .iter()
        .zip(current.turns.iter())
        .take_while(|(left, right)| canonical_bytes(left) == canonical_bytes(right))
        .count();
    let tokens = runtime.count_tokens(&current.system_stable)
        + runtime.count_tokens(&current.system_knowledge)
        + current.turns[..turns]
            .iter()
            .map(|message| runtime.count_tokens(&message_text(message)))
            .sum::<u32>();
    (turns, tokens)
}

fn strict_tool_pairing(messages: &[Message]) -> bool {
    let mut pending = BTreeSet::new();
    for message in messages {
        if message.role == Role::Assistant {
            for call in &message.tool_calls {
                if !pending.insert(call.id.to_string()) {
                    return false;
                }
            }
        }
        if message.role == Role::Tool {
            let Content::Parts(parts) = &message.content else {
                return false;
            };
            for part in parts {
                let ContentPart::ToolResult { call_id, .. } = part else {
                    continue;
                };
                if !pending.remove(call_id.as_str()) {
                    return false;
                }
            }
        }
    }
    pending.is_empty()
}

fn recent_messages_retained(text: &str, source: &[Message], preserve_turns: usize) -> bool {
    let units = deepstrike_core::context::units::unit_boundaries(source);
    units
        .iter()
        .rev()
        .take(preserve_turns)
        .flat_map(|range| source[range.clone()].iter())
        .map(message_text)
        .filter(|value| !value.is_empty())
        .all(|value| text.contains(&value))
}

fn find_orphan_handle(text: &str, accepted: &BTreeSet<String>) -> Option<String> {
    for token in text.split_whitespace() {
        let token = token.trim_matches(|character: char| {
            matches!(character, ',' | '.' | ')' | ']' | '}' | '"' | '\'')
        });
        if ["spool://", "archive://", "lab://"]
            .iter()
            .any(|prefix| token.starts_with(prefix))
            && !accepted
                .iter()
                .any(|handle| token == handle || token.contains(handle))
        {
            return Some(format!("orphan handle {token}"));
        }
    }
    None
}

fn contains_folded(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn comparable_effects<'a>(
    effects: impl Iterator<Item = &'a TraceEffectRecord>,
) -> Vec<(String, LogicalEffectKey)> {
    effects
        .filter(|effect| matches!(effect.kind.as_str(), "provider" | "tool"))
        .map(|effect| (effect.kind.clone(), effect.logical_effect_key.clone()))
        .collect()
}

fn display_effects(effects: &[(String, LogicalEffectKey)]) -> String {
    effects
        .iter()
        .map(|(kind, key)| format!("{kind}={}", key.0))
        .collect::<Vec<_>>()
        .join(",")
}

fn validate_trace(trace: &TraceV1) -> Result<(), ReplayError> {
    if trace.trace_version != TRACE_VERSION || trace.abi_version != KERNEL_ABI_VERSION {
        return Err(invalid_trace("unsupported trace or ABI version"));
    }
    for (index, transaction) in trace.transactions.iter().enumerate() {
        if transaction.ordinal != (index + 1) as u32 {
            return Err(invalid_trace("transaction ordinals must be contiguous"));
        }
    }
    for (key, content) in &trace.fixture_blobs {
        if fixture_blob_key(content) != *key {
            return Err(invalid_trace(format!(
                "fixture blob {key} failed digest validation"
            )));
        }
    }
    validate_probe_points(trace)
}

fn validate_oracle_keys(trace: &TraceV1) -> Result<(), ReplayError> {
    let mut keys = BTreeSet::new();
    for effect in trace
        .transactions
        .iter()
        .flat_map(|transaction| transaction.effects.iter())
        .filter(|effect| matches!(effect.kind.as_str(), "provider" | "tool"))
    {
        if !keys.insert(effect.logical_effect_key.clone()) {
            return Err(not_comparable(format!(
                "duplicate provider/tool logical effect key {}",
                effect.logical_effect_key.0
            )));
        }
    }
    Ok(())
}

fn validate_probe_shapes(probes: &[FactProbe]) -> Result<(), ReplayError> {
    let mut ids = BTreeSet::new();
    for probe in probes {
        if probe.id.is_empty() || probe.canonical_value.is_empty() || !ids.insert(&probe.id) {
            return Err(invalid_trace(
                "fact probes require unique non-empty ids and canonical values",
            ));
        }
    }
    Ok(())
}

fn validate_probe_points(trace: &TraceV1) -> Result<(), ReplayError> {
    validate_probe_shapes(&trace.probes)?;
    for probe in &trace.probes {
        let introduced = point_transaction(trace, &probe.introduced_at).ok_or_else(|| {
            invalid_trace(format!("probe {} introduced_at does not exist", probe.id))
        })?;
        let required = point_transaction(trace, &probe.required_at).ok_or_else(|| {
            invalid_trace(format!("probe {} required_at does not exist", probe.id))
        })?;
        if introduced > required {
            return Err(invalid_trace(format!(
                "probe {} is required before it is introduced",
                probe.id
            )));
        }
    }
    Ok(())
}

fn point_transaction(trace: &TraceV1, point: &TracePoint) -> Option<u32> {
    match point {
        TracePoint::Transaction(ordinal) => trace
            .transactions
            .iter()
            .any(|transaction| transaction.ordinal == *ordinal)
            .then_some(*ordinal),
        TracePoint::ProviderTurn(turn) => trace
            .transactions
            .iter()
            .flat_map(|transaction| {
                transaction
                    .effects
                    .iter()
                    .filter(|effect| effect.kind == "provider")
                    .map(move |_| transaction.ordinal)
            })
            .nth(turn.saturating_sub(1) as usize),
    }
}

#[derive(Debug, Default)]
struct ArchiveAccounting {
    compressed: u64,
    committed: u64,
}

impl ArchiveAccounting {
    fn observe_compressed(&mut self, count: u64) {
        self.compressed = self.compressed.saturating_add(count);
    }

    fn observe_committed(&mut self, count: u64) {
        self.committed = self.committed.saturating_add(count);
    }

    fn reported(&self) -> u64 {
        if self.committed == 0 {
            self.compressed
        } else {
            self.committed
        }
    }
}

fn ensure_step_ok(transaction: u32, step: &KernelStep) -> Result<(), ReplayError> {
    if step.faults.is_empty() {
        return Ok(());
    }
    Err(ReplayError::KernelFault {
        transaction,
        message: step
            .faults
            .iter()
            .map(|fault| format!("{:?}: {}", fault.code, fault.message))
            .collect::<Vec<_>>()
            .join("; "),
    })
}

fn synthetic_ref(kind: &str, trace_digest: &str, key: &LogicalEffectKey) -> String {
    let material = format!("{trace_digest}:{}:{}", key.0, 0_u64);
    format!("lab://{kind}/{:016x}", stable_hash(material.as_bytes()))
}

fn digest_json(value: &impl Serialize) -> Result<String, ReplayError> {
    let value = serde_json::to_value(value)?;
    Ok(format!(
        "fnv1a64:{:016x}",
        stable_hash(&serde_json::to_vec(&canonical_value(value))?)
    ))
}

fn canonical_bytes(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_value(value)
        .map(canonical_value)
        .and_then(|value| serde_json::to_vec(&value))
        .unwrap_or_default()
}

fn canonical_value(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_value).collect()),
        Value::Object(values) => {
            let sorted = values
                .into_iter()
                .map(|(key, value)| (key, canonical_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        value => value,
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn message_content_text(content: &Content) -> String {
    match content {
        Content::Text(text) => text.clone(),
        Content::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => text.clone(),
                ContentPart::ToolResult { output, .. } => output.clone(),
                ContentPart::Image { url, .. } => url.clone().unwrap_or_default(),
                ContentPart::Audio { .. } => "[audio]".into(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::ArchiveAccounting;

    #[test]
    fn committed_archive_count_does_not_double_count_compression_facts() {
        let mut accounting = ArchiveAccounting::default();
        accounting.observe_compressed(3);
        accounting.observe_compressed(4);
        accounting.observe_committed(3);
        accounting.observe_committed(4);
        assert_eq!(accounting.reported(), 7);
    }

    #[test]
    fn compression_facts_are_the_fallback_without_archive_commit() {
        let mut accounting = ArchiveAccounting::default();
        accounting.observe_compressed(5);
        assert_eq!(accounting.reported(), 5);
    }
}

fn invalid_trace(message: impl Into<String>) -> ReplayError {
    ReplayError::InvalidTrace {
        message: message.into(),
    }
}

fn not_comparable(message: impl Into<String>) -> ReplayError {
    ReplayError::TraceNotComparable {
        message: message.into(),
    }
}

#[allow(dead_code)]
fn _assert_run_config_is_lab_external(_: &RunConfig) {}
