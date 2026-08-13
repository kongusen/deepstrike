//! # DeepStrike Node.js Bindings
//!
//! napi-rs bindings exposing the Rust kernel to Node.js.
//! Build with: `napi build --release --platform`
//!
//! ## High-level API
//!
//! ```typescript
//! import {
//!   ContextEngine, LoopStateMachine, RuntimeTask, LoopPolicy,
//!   Message, ToolCall, ToolResult, ToolSchema,
//!   SkillMetadata,
//! } from '@deepstrike/core'
//!
//! const sm = new LoopStateMachine({ maxTokens: 128_000 })
//! // Register skills once; the kernel auto-injects the `skill` meta-tool.
//! sm.setAvailableSkills([
//!   { name: 'debug', description: 'Debug helper', estimatedTokens: 0 },
//! ])
//!
//! let action = sm.start({ goal: 'Fix the bug' })
//! while (!sm.isTerminal()) {
//!   if (action.kind === 'call_llm') {
//!     // tools list already includes the `skill` meta-tool
//!     const msg = await callLlm(action.context, action.tools)
//!     action = sm.feedLlmResponse(msg)
//!   } else if (action.kind === 'execute_tools') {
//!     // SDK intercepts calls where name === 'skill' and reads the file
//!     const results = await execTools(action.calls)
//!     action = sm.feedToolResults(results)
//!   } else if (action.kind === 'done') {
//!     break
//!   }
//! }
//! ```

#![deny(clippy::all)]
#![allow(deprecated)]

use napi::bindgen_prelude::*;
use napi_derive::napi;

use deepstrike_core::governance::constraint::{ConstraintRule, ParamConstraint};
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline as RustGovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::harness::eval::{
    Criterion as RustCriterion, build_eval_messages as rust_build_eval_messages,
    parse_verdict as rust_parse_verdict, verdict_output_schema as rust_verdict_output_schema,
};
use deepstrike_core::runtime::kernel::wire::{
    CanonicalKernel as RustCanonicalKernel, CheckpointBoundary as RustCheckpointBoundary,
    Digest as RustDigest, KernelCheckpoint as RustKernelCheckpoint,
    KernelPreparation as RustKernelPreparation, OperationLifecycle as RustOperationLifecycle,
    PrepareToken as RustPrepareToken, WireU64 as RustWireU64,
};
use deepstrike_core::scheduler::tcb::TaskLifecycle;
use deepstrike_core::signals::router::SignalRouter as RustSignalRouter;
use deepstrike_core::types::agent::AgentIdentity;
use deepstrike_core::types::contract::{
    AcceptanceCriterion as RustAcceptanceCriterion,
    VerificationContract as RustVerificationContract,
};
use deepstrike_core::types::message::{
    Content, ContentPart, Message as RustMessage, Role, ToolCall as RustToolCall,
};
use deepstrike_core::types::policy::GovernanceVerdict as RustGovernanceVerdict;
use deepstrike_core::types::policy::SignalDisposition as RustSignalDisposition;
use deepstrike_core::types::signal::{
    RuntimeSignal as RustRuntimeSignal, SignalSource as RustSignalSource,
    SignalType as RustSignalType, Urgency as RustUrgency,
};

// ────────────────────────────────────── POD types (plain JS objects) ──────────────────────────────────────

#[napi(object)]
#[derive(Clone)]
pub struct ContentPartObj {
    /// `"text"` | `"image"` | `"audio"` | `"tool_result"`
    pub r#type: String,
    pub text: Option<String>,
    pub url: Option<String>,
    pub data: Option<String>,
    pub media_type: Option<String>,
    pub detail: Option<String>,
    pub call_id: Option<String>,
    pub output: Option<String>,
    pub is_error: Option<bool>,
}

#[napi(object)]
#[derive(Clone)]
pub struct Message {
    pub role: String,
    /// Plain-text content. When `content_parts` is present, this holds only the
    /// concatenated text segments for backward compatibility.
    pub content: String,
    /// Structured multimodal content parts. When present, takes precedence over `content`.
    pub content_parts: Option<Vec<ContentPartObj>>,
    pub token_count: Option<u32>,
    pub tool_calls: Vec<ToolCall>,
}

#[napi(object)]
#[derive(Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments. JS: `JSON.stringify(args)`.
    pub arguments: String,
}

#[napi(object)]
#[derive(Clone)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
    pub is_fatal: Option<bool>,
    pub error_kind: Option<String>,
    pub token_count: Option<u32>,
}

#[napi(object)]
#[derive(Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON-encoded JSON Schema. JS: `JSON.stringify(schema)`.
    pub parameters: String,
}

#[napi(object)]
#[derive(Clone)]
pub struct RuntimeTask {
    pub goal: String,
    pub criteria: Option<Vec<String>>,
    /// Freeform lane label. Well-known: `"orchestrate"` | `"implement"` (default) | `"retrieve"` | `"verify"`.
    pub lane: Option<String>,
}

// ────────────────────────────────────── Contract types ──────────────────────────────────────

#[napi(object)]
#[derive(Clone)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub text: String,
    pub required: bool,
    pub weight: f64,
    pub machine_checkable: bool,
}

#[napi(object)]
#[derive(Clone)]
pub struct VerificationContract {
    pub id: String,
    pub goal: String,
    pub acceptance: Vec<AcceptanceCriterion>,
    pub anti_patterns: Vec<String>,
    pub evidence_required: Vec<String>,
}

#[napi(object)]
#[derive(Clone)]
pub struct LoopResult {
    pub termination: String,
    pub final_message: Option<Message>,
    pub turns_used: u32,
    pub total_tokens_used: BigInt,
}

// ────────────────────────────────────── Skill types ──────────────────────────────────────

// ────────────────────────────────────── Signal types ──────────────────────────────────────

/// Unified RuntimeSignal exposed to Node.js — mirrors the kernel type.
#[napi(object)]
#[derive(Clone)]
pub struct RuntimeSignal {
    pub id: String,
    /// "cron" | "gateway" | "heartbeat" | "custom"
    pub source: String,
    /// "event" | "job" | "alert"
    pub signal_type: String,
    /// "low" | "normal" | "high" | "critical"
    pub urgency: String,
    pub summary: String,
    /// JSON-encoded payload.
    pub payload: String,
    pub dedupe_key: Option<String>,
    /// Target a specific session loop (sessionId). Omitted ⇒ broadcast.
    pub recipient: Option<String>,
    /// Absolute journal-clock deadline for optional urgency escalation.
    pub deadline_ms: Option<f64>,
    /// Merge this signal with an unconsumed queued signal carrying the same key.
    pub coalesce_key: Option<String>,
    /// Deterministic number of host signals represented by this value.
    pub coalesced_count: Option<u32>,
    pub timestamp_ms: f64,
}

fn runtime_signal_to_rust(s: RuntimeSignal) -> Result<RustRuntimeSignal> {
    // §7.7 · the signal id is the caller's own branded ref, kept verbatim. It used to have to
    // parse as a UUID, which forced every non-UUID signal into a minted second identity.
    let id = s.id.clone();
    let source = match s.source.as_str() {
        "cron" => RustSignalSource::Cron,
        "gateway" => RustSignalSource::Gateway,
        "heartbeat" => RustSignalSource::Heartbeat,
        _ => RustSignalSource::Custom,
    };
    let signal_type = match s.signal_type.as_str() {
        "job" => RustSignalType::Job,
        "alert" => RustSignalType::Alert,
        _ => RustSignalType::Event,
    };
    let urgency = match s.urgency.as_str() {
        "critical" => RustUrgency::Critical,
        "high" => RustUrgency::High,
        "low" => RustUrgency::Low,
        _ => RustUrgency::Normal,
    };
    let payload: serde_json::Value =
        serde_json::from_str(&s.payload).unwrap_or(serde_json::Value::Null);
    let mut sig = RustRuntimeSignal::new(source, signal_type, urgency, s.summary.as_str())
        .with_payload(payload)
        .with_timestamp(s.timestamp_ms as u64);
    sig.id = id.into();
    if let Some(key) = s.dedupe_key {
        sig = sig.with_dedupe(key.as_str());
    }
    if let Some(recipient) = s.recipient {
        sig = sig.with_recipient(recipient.as_str());
    }
    if let Some(deadline_ms) = s.deadline_ms {
        sig = sig.with_deadline(deadline_ms as u64);
    }
    if let Some(coalesce_key) = s.coalesce_key {
        sig = sig.with_coalesce(coalesce_key.as_str());
    }
    sig.coalesced_count = s.coalesced_count.unwrap_or(1).max(1);
    Ok(sig)
}

fn runtime_signal_from_rust(s: &RustRuntimeSignal) -> RuntimeSignal {
    let source = match s.source {
        RustSignalSource::Cron => "cron",
        RustSignalSource::Gateway => "gateway",
        RustSignalSource::Heartbeat => "heartbeat",
        RustSignalSource::Custom => "custom",
    };
    let signal_type = match s.signal_type {
        RustSignalType::Event => "event",
        RustSignalType::Job => "job",
        RustSignalType::Alert => "alert",
    };
    let urgency = match s.urgency {
        RustUrgency::Critical => "critical",
        RustUrgency::High => "high",
        RustUrgency::Normal => "normal",
        RustUrgency::Low => "low",
    };
    RuntimeSignal {
        id: s.id.to_string(),
        source: source.into(),
        signal_type: signal_type.into(),
        urgency: urgency.into(),
        summary: s.summary.to_string(),
        payload: serde_json::to_string(&s.payload).unwrap_or_else(|_| "null".into()),
        dedupe_key: s.dedupe_key.as_ref().map(|k| k.to_string()),
        recipient: s.recipient.as_ref().map(|r| r.to_string()),
        deadline_ms: s.deadline_ms.map(|value| value as f64),
        coalesce_key: s.coalesce_key.as_ref().map(|key| key.to_string()),
        coalesced_count: Some(s.coalesced_count.max(1)),
        timestamp_ms: s.timestamp_ms as f64,
    }
}

fn disposition_to_str(d: RustSignalDisposition) -> &'static str {
    d.label()
}

#[napi(object)]
#[derive(Clone)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub effort: Option<u8>,
    pub estimated_tokens: u32,
}

// ────────────────────────────────── FFI panic guard ──────────────────────────────────

/// Run `f` with a `catch_unwind` net so a Rust panic becomes a catchable JS error
/// instead of aborting the whole Node process. A panic unwinding across the
/// `extern "C"` napi boundary is turned into a hard `abort` by the Rust runtime —
/// which no `uncaughtException`/`unhandledRejection` handler can intercept, taking
/// down every other session sharing the process. Converting it to `napi::Error`
/// keeps the process (and its other sessions) alive; the failed call surfaces as a
/// normal thrown error the SDK loop can catch and fail just that one run.
///
/// State touched before the panic may be left inconsistent, so callers must treat a
/// returned error as fatal to *that run* — but that is strictly better than an abort.
fn ffi_guard<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(Error::new(
                Status::GenericFailure,
                format!("internal kernel panic in {what}: {detail}"),
            ))
        }
    }
}

// ────────────────────────────────── conversion helpers ──────────────────────────────────

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn content_part_from_rust(p: &ContentPart) -> ContentPartObj {
    match p {
        ContentPart::Text { text } => ContentPartObj {
            r#type: "text".into(),
            text: Some(text.clone()),
            url: None,
            data: None,
            media_type: None,
            detail: None,
            call_id: None,
            output: None,
            is_error: None,
        },
        ContentPart::Image {
            url,
            data,
            media_type,
            detail,
        } => ContentPartObj {
            r#type: "image".into(),
            text: None,
            url: url.clone(),
            data: data.clone(),
            media_type: media_type.clone(),
            detail: detail.clone(),
            call_id: None,
            output: None,
            is_error: None,
        },
        ContentPart::Audio { data, media_type } => ContentPartObj {
            r#type: "audio".into(),
            text: None,
            url: None,
            data: Some(data.clone()),
            media_type: Some(media_type.clone()),
            detail: None,
            call_id: None,
            output: None,
            is_error: None,
        },
        ContentPart::ToolResult {
            call_id,
            output,
            is_error,
            ..
        } => ContentPartObj {
            r#type: "tool_result".into(),
            text: None,
            url: None,
            data: None,
            media_type: None,
            detail: None,
            call_id: Some(call_id.to_string()),
            output: Some(output.clone()),
            is_error: Some(*is_error),
        },
    }
}

fn message_from_rust(m: &RustMessage) -> Message {
    let (content, content_parts) = match &m.content {
        Content::Text(s) => (s.clone(), None),
        Content::Parts(parts) => {
            let text_only: String = parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let objs: Vec<ContentPartObj> = parts.iter().map(content_part_from_rust).collect();
            (text_only, Some(objs))
        }
    };
    Message {
        role: role_to_str(m.role).to_string(),
        content,
        content_parts,
        token_count: m.token_count,
        tool_calls: m.tool_calls.iter().map(tool_call_from_rust).collect(),
    }
}

fn tool_call_from_rust(c: &RustToolCall) -> ToolCall {
    ToolCall {
        id: c.id.to_string(),
        name: c.name.to_string(),
        arguments: serde_json::to_string(&c.arguments).unwrap_or_else(|_| "null".into()),
    }
}

fn acceptance_criterion_to_rust(c: AcceptanceCriterion) -> RustAcceptanceCriterion {
    RustAcceptanceCriterion {
        id: c.id,
        text: c.text,
        required: c.required,
        weight: c.weight as f32,
        machine_checkable: c.machine_checkable,
    }
}

fn verification_contract_to_rust(v: VerificationContract) -> RustVerificationContract {
    RustVerificationContract {
        id: v.id,
        goal: v.goal,
        acceptance: v
            .acceptance
            .into_iter()
            .map(acceptance_criterion_to_rust)
            .collect(),
        anti_patterns: v.anti_patterns,
        evidence_required: v.evidence_required,
    }
}

// ────────────────────────────────── Contract helpers ──────────────────────────────────────

/// Format a VerificationContract as a markdown string suitable for injection
/// into an agent's system prompt. The returned string is ready to pass to
/// `LoopStateMachine.addSystemMessage()` or `LoopStateMachine.setContract()`.
#[napi]
pub fn format_contract_for_system_prompt(contract: VerificationContract) -> String {
    verification_contract_to_rust(contract).format_for_system_prompt()
}

// ─────────────────────────────── Canonical Kernel ABI ───────────────────────────────

/// The single ABI revision accepted by core. Hosts must read this export instead of copying it.
#[napi]
pub fn kernel_abi_version() -> u32 {
    deepstrike_core::runtime::kernel::wire::KERNEL_ABI_VERSION
}

#[napi(object)]
pub struct CanonicalPreparation {
    /// `prepared` | `replayed` | `rejected`.
    pub status: String,
    pub prepare_token: Option<String>,
    /// Decimal `WireU64`; never a JavaScript number.
    pub step_seq: Option<String>,
    pub expected_head: Option<String>,
    pub record_digest: Option<String>,
    /// Exact `KernelRecord::record_bytes()` from core.
    pub record_bytes: Option<Buffer>,
    pub planned_step_json: Option<String>,
    pub fault_json: Option<String>,
}

#[napi(object)]
pub struct CanonicalCommit {
    /// Decimal `WireU64`; never a JavaScript number.
    pub step_seq: String,
    pub record_digest: String,
    pub planned_step_json: String,
    pub checkpoint_advice_json: Option<String>,
}

#[napi(object)]
pub struct CanonicalCheckpoint {
    pub checkpoint_bytes: Buffer,
    /// Decimal `WireU64`; never a JavaScript number.
    pub through_step_seq: String,
    pub covered_head: String,
    pub state_digest: String,
    pub ack_token: String,
}

#[napi(object)]
pub struct CanonicalRestoreCost {
    pub records_before_checkpoint: String,
    pub tail_inputs_replayed: String,
    pub records_after_checkpoint: String,
    pub bytes_read: String,
}

/// Durable canonical operation handle. It exposes no direct `step` method.
#[napi]
pub struct CanonicalKernel {
    inner: RustCanonicalKernel,
}

#[napi]
impl CanonicalKernel {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: RustCanonicalKernel::default(),
        }
    }

    /// Strictly decode and prepare one canonical envelope.
    #[napi]
    pub fn prepare(&mut self, input_json: String) -> Result<CanonicalPreparation> {
        ffi_guard("CanonicalKernel.prepare", || {
            canonical_preparation_from_rust(self.inner.prepare_json(&input_json))
        })
    }

    /// Commit only after the journal appended the prepared record.
    #[napi]
    pub fn commit(
        &mut self,
        prepare_token: String,
        appended_head: String,
    ) -> Result<CanonicalCommit> {
        ffi_guard("CanonicalKernel.commit", || {
            let token = RustPrepareToken::new(prepare_token)
                .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?;
            let head = RustDigest::new(appended_head)
                .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?;
            let committed = self
                .inner
                .commit(&token, &head)
                .map_err(canonical_kernel_error)?;
            Ok(CanonicalCommit {
                step_seq: committed.step_seq.to_string(),
                record_digest: committed.record.record_digest().to_string(),
                planned_step_json: serde_json::to_string(&committed.step).map_err(json_error)?,
                checkpoint_advice_json: committed
                    .checkpoint_advice
                    .map(|advice| {
                        serde_json::to_string(&serde_json::json!({
                            "through_step_seq": advice.through_step_seq,
                            "usage": {
                                "records": advice.usage.records.to_string(),
                                "bytes": advice.usage.bytes.to_string(),
                            },
                            "bounds": {
                                "soft_records": advice.bounds.soft_records,
                                "hard_records": advice.bounds.hard_records,
                                "soft_bytes": advice.bounds.soft_bytes,
                                "hard_bytes": advice.bounds.hard_bytes,
                            },
                        }))
                    })
                    .transpose()
                    .map_err(json_error)?,
            })
        })
    }

    /// Abort a candidate that did not reach the journal.
    #[napi]
    pub fn abort(&mut self, prepare_token: String) -> Result<()> {
        ffi_guard("CanonicalKernel.abort", || {
            let token = RustPrepareToken::new(prepare_token)
                .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?;
            self.inner.abort(&token).map_err(canonical_kernel_error)?;
            Ok(())
        })
    }

    #[napi(js_name = "checkpointCandidate")]
    pub fn checkpoint_candidate(&self) -> Result<CanonicalCheckpoint> {
        self.inner
            .checkpoint_candidate()
            .map(canonical_checkpoint_from_rust)
            .map_err(canonical_kernel_error)
    }

    #[napi(js_name = "checkpointRebase")]
    pub fn checkpoint_rebase(&self, checkpoint_bytes: Buffer) -> Result<CanonicalCheckpoint> {
        let checkpoint = RustKernelCheckpoint::from_checkpoint_bytes(checkpoint_bytes.as_ref())
            .map_err(|error| canonical_kernel_error(error.fault()))?;
        self.inner
            .checkpoint_rebase(&checkpoint)
            .map(canonical_checkpoint_from_rust)
            .map_err(canonical_kernel_error)
    }

    #[napi(js_name = "ackCheckpoint")]
    pub fn ack_checkpoint(&mut self, through_step_seq: String, covered_head: String) -> Result<()> {
        let boundary = RustCheckpointBoundary {
            through_step_seq: RustWireU64::parse(&through_step_seq)
                .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?,
            covered_head: RustDigest::new(covered_head)
                .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?,
        };
        self.inner
            .note_checkpoint_acked(&boundary)
            .map(|_| ())
            .map_err(canonical_kernel_error)
    }

    /// Replace this handle in place from checkpoint bytes plus post-checkpoint record bytes.
    #[napi]
    pub fn restore(
        &mut self,
        checkpoint_bytes: Option<Buffer>,
        record_bytes: Vec<Buffer>,
    ) -> Result<CanonicalRestoreCost> {
        let records = record_bytes
            .into_iter()
            .map(|bytes| bytes.to_vec())
            .collect::<Vec<_>>();
        let cost = self
            .inner
            .restore_bytes(checkpoint_bytes.as_deref(), &records)
            .map_err(canonical_kernel_error)?;
        Ok(CanonicalRestoreCost {
            records_before_checkpoint: cost.records_before_checkpoint.to_string(),
            tail_inputs_replayed: cost.tail_inputs_replayed.to_string(),
            records_after_checkpoint: cost.records_after_checkpoint.to_string(),
            bytes_read: cost.bytes_read.to_string(),
        })
    }

    #[napi]
    pub fn lifecycle(&self) -> String {
        match self.inner.lifecycle() {
            RustOperationLifecycle::Created => "created",
            RustOperationLifecycle::Configured => "configured",
            RustOperationLifecycle::Running => "running",
            RustOperationLifecycle::Suspended => "suspended",
            RustOperationLifecycle::Completed => "completed",
            RustOperationLifecycle::Cancelled => "cancelled",
            RustOperationLifecycle::Failed => "failed",
        }
        .into()
    }

    #[napi(js_name = "pendingEffectsJson")]
    pub fn pending_effects_json(&self) -> Result<String> {
        serde_json::to_string(&self.inner.pending_effects().collect::<Vec<_>>()).map_err(json_error)
    }

    #[napi(js_name = "terminalJson")]
    pub fn terminal_json(&self) -> Result<Option<String>> {
        self.inner
            .terminal()
            .map(serde_json::to_string)
            .transpose()
            .map_err(json_error)
    }
}

fn canonical_preparation_from_rust(
    preparation: RustKernelPreparation<
        deepstrike_core::runtime::kernel::wire::KernelRecord,
        deepstrike_core::runtime::kernel::wire::PlannedStep,
    >,
) -> Result<CanonicalPreparation> {
    match preparation {
        RustKernelPreparation::Prepared(prepared) => Ok(CanonicalPreparation {
            status: "prepared".into(),
            prepare_token: Some(prepared.token.to_string()),
            step_seq: Some(prepared.record.step_seq().to_string()),
            expected_head: prepared.record.expected_head().map(ToString::to_string),
            record_digest: Some(prepared.record.record_digest().to_string()),
            record_bytes: Some(Buffer::from(
                prepared.record.record_bytes().as_slice().to_vec(),
            )),
            planned_step_json: Some(
                serde_json::to_string(&prepared.planned_step).map_err(json_error)?,
            ),
            fault_json: None,
        }),
        RustKernelPreparation::Replayed(replayed) => Ok(CanonicalPreparation {
            status: "replayed".into(),
            prepare_token: None,
            step_seq: Some(replayed.step_seq.to_string()),
            expected_head: replayed
                .record
                .as_ref()
                .and_then(|record| record.expected_head())
                .map(ToString::to_string),
            record_digest: Some(replayed.record_digest.to_string()),
            record_bytes: replayed
                .record
                .map(|record| Buffer::from(record.record_bytes().as_slice().to_vec())),
            planned_step_json: replayed
                .committed_step
                .map(|step| serde_json::to_string(&step))
                .transpose()
                .map_err(json_error)?,
            fault_json: None,
        }),
        RustKernelPreparation::Rejected(rejected) => Ok(CanonicalPreparation {
            status: "rejected".into(),
            prepare_token: None,
            step_seq: None,
            expected_head: None,
            record_digest: None,
            record_bytes: None,
            planned_step_json: None,
            fault_json: Some(serde_json::to_string(&rejected.fault).map_err(json_error)?),
        }),
    }
}

fn canonical_checkpoint_from_rust(
    checkpoint: deepstrike_core::runtime::kernel::wire::CheckpointCandidate,
) -> CanonicalCheckpoint {
    CanonicalCheckpoint {
        checkpoint_bytes: Buffer::from(checkpoint.checkpoint_bytes.as_slice().to_vec()),
        through_step_seq: checkpoint.through_step_seq.to_string(),
        covered_head: checkpoint.covered_head.to_string(),
        state_digest: checkpoint.state_digest.to_string(),
        ack_token: checkpoint.ack_token.to_string(),
    }
}

fn canonical_kernel_error(fault: deepstrike_core::runtime::kernel::wire::KernelFault) -> Error {
    Error::new(
        Status::InvalidArg,
        serde_json::to_string(&fault).unwrap_or_else(|_| fault.to_string()),
    )
}

fn json_error(error: serde_json::Error) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

// ─────────────────────────────────────────── SignalRouter ───────────────────────────────────────────

#[napi]
pub struct SignalRouter {
    inner: RustSignalRouter,
}

#[napi]
impl SignalRouter {
    #[napi(constructor)]
    pub fn new(max_queue_size: u32) -> Self {
        Self {
            inner: RustSignalRouter::new(max_queue_size as usize),
        }
    }

    /// Ingest a signal. Returns the disposition string:
    /// "ignore" | "observe" | "queue" | "run" | "interrupt" | "interrupt_now" | "dropped"
    #[napi]
    pub fn ingest(&mut self, signal: RuntimeSignal, lifecycle: String) -> Result<String> {
        let rust_sig = runtime_signal_to_rust(signal)?;
        let lifecycle = match lifecycle.as_str() {
            "ready" => TaskLifecycle::Ready,
            "running" => TaskLifecycle::Running,
            "suspended" => TaskLifecycle::Suspended,
            "done" => {
                TaskLifecycle::Done(deepstrike_core::types::result::TerminationReason::Completed)
            }
            other => {
                return Err(Error::from_reason(format!(
                    "invalid task lifecycle {other:?}; expected ready|running|suspended|done"
                )));
            }
        };
        Ok(disposition_to_str(self.inner.ingest(rust_sig, lifecycle)).into())
    }

    /// Pull the next queued signal (highest priority first).
    #[napi]
    pub fn next(&mut self) -> Option<RuntimeSignal> {
        self.inner.next().as_ref().map(runtime_signal_from_rust)
    }

    #[napi]
    pub fn depth(&self) -> u32 {
        self.inner.depth() as u32
    }

    #[napi]
    pub fn clear_dedup(&mut self) {
        self.inner.clear_dedup();
    }
}

// ─────────────────────────────────────────── Governance ───────────────────────────────────────────

/// JS-friendly governance verdict returned by `Governance.evaluate`.
#[napi(object)]
#[derive(Clone)]
pub struct GovernanceVerdict {
    /// `"allow"` | `"deny"` | `"rate_limited"` | `"ask_user"`
    pub kind: String,
    pub reason: Option<String>,
    /// Milliseconds until the tool may be retried. Only set when `kind === "rate_limited"`.
    pub retry_after_ms: Option<f64>,
}

fn verdict_to_js(v: RustGovernanceVerdict) -> GovernanceVerdict {
    match v {
        RustGovernanceVerdict::Allow => GovernanceVerdict {
            kind: "allow".into(),
            reason: None,
            retry_after_ms: None,
        },
        RustGovernanceVerdict::Deny { reason, .. } => GovernanceVerdict {
            kind: "deny".into(),
            reason: Some(reason),
            retry_after_ms: None,
        },
        RustGovernanceVerdict::RateLimited { retry_after_ms } => GovernanceVerdict {
            kind: "rate_limited".into(),
            reason: None,
            retry_after_ms: Some(retry_after_ms as f64),
        },
        RustGovernanceVerdict::AskUser { reason } => GovernanceVerdict {
            kind: "ask_user".into(),
            reason: Some(reason),
            retry_after_ms: None,
        },
    }
}

#[napi]
pub struct Governance {
    inner: RustGovernancePipeline,
    agent_id: String,
    session_id: String,
}

#[napi]
impl Governance {
    /// Create a governance pipeline.
    /// `defaultAction` controls the fallback when no rule matches: `"allow"` (default) or `"deny"`.
    #[napi(constructor)]
    pub fn new(default_action: Option<String>) -> Self {
        let action = match default_action.as_deref() {
            Some("deny") => PermissionAction::Deny,
            Some("ask_user") => PermissionAction::AskUser,
            _ => PermissionAction::Allow,
        };
        Self {
            inner: RustGovernancePipeline::new(action),
            agent_id: "anonymous".into(),
            session_id: "".into(),
        }
    }

    /// Set the agent identity used in governance audit logs.
    #[napi]
    pub fn set_identity(&mut self, agent_id: String, session_id: String) {
        self.agent_id = agent_id;
        self.session_id = session_id;
    }

    /// Add a permission rule. `pattern` supports globs: `"db.*"`, `"*.delete"`, `"*"`, or exact names.
    /// `action`: `"allow"` | `"deny"` | `"ask_user"`.
    /// Rules are evaluated in insertion order; first match wins.
    #[napi]
    pub fn add_permission_rule(&mut self, pattern: String, action: String) {
        let perm_action = match action.as_str() {
            "deny" => PermissionAction::Deny,
            "ask_user" => PermissionAction::AskUser,
            _ => PermissionAction::Allow,
        };
        self.inner.permission.add_rule(PermissionRule {
            tool_pattern: pattern.into(),
            action: perm_action,
        });
    }

    /// Hard-block a tool name (veto stage — cannot be overridden by permission rules).
    #[napi]
    pub fn block_tool(&mut self, name: String) {
        self.inner.veto.block_tool(name);
    }

    /// Configure a per-tool sliding-window rate limit.
    #[napi]
    pub fn set_rate_limit(&mut self, tool_name: String, max_calls: u32, window_ms: BigInt) {
        self.inner.rate_limiter.set_limit(
            tool_name,
            RateLimit {
                max_calls,
                window_ms: window_ms.get_u64().1,
            },
        );
    }

    /// Require a parameter path such as `"path"` or `"payload.mode"` to be present.
    #[napi]
    pub fn require_param(&mut self, tool_name: String, param_path: String) {
        self.inner.constraints.add(ParamConstraint {
            tool_name,
            param_path,
            rule: ConstraintRule::Required,
        });
    }

    /// Restrict a string parameter path to one of the allowed values.
    #[napi]
    pub fn allow_param_values(
        &mut self,
        tool_name: String,
        param_path: String,
        allowed_values: Vec<String>,
    ) {
        self.inner.constraints.add(ParamConstraint {
            tool_name,
            param_path,
            rule: ConstraintRule::Enum(allowed_values),
        });
    }

    /// Restrict a numeric parameter path to an inclusive range.
    #[napi]
    pub fn limit_param_range(
        &mut self,
        tool_name: String,
        param_path: String,
        min: Option<f64>,
        max: Option<f64>,
    ) {
        self.inner.constraints.add(ParamConstraint {
            tool_name,
            param_path,
            rule: ConstraintRule::Range { min, max },
        });
    }

    /// Advance the internal clock used by rate limiting and audit.
    #[napi]
    pub fn set_time(&mut self, now_ms: BigInt) {
        self.inner.set_time(now_ms.get_u64().1);
    }

    /// Evaluate a tool call through the full pipeline (Permission → Veto → RateLimit → Constraint → Audit).
    /// `argsJson`: JSON-encoded tool arguments string.
    #[napi]
    pub fn evaluate(&mut self, tool_name: String, args_json: String) -> Result<GovernanceVerdict> {
        // Fail closed: constraints cannot be evaluated against arguments that do not
        // parse, so an unparseable payload is a denial, not a skipped check.
        let args: serde_json::Value = match serde_json::from_str(&args_json) {
            Ok(value) => value,
            Err(err) => {
                return Ok(GovernanceVerdict {
                    kind: "deny".into(),
                    reason: Some(format!("tool arguments are not valid JSON: {err}")),
                    retry_after_ms: None,
                });
            }
        };
        let call = RustToolCall {
            id: compact_str::CompactString::new(""),
            name: compact_str::CompactString::new(&tool_name),
            arguments: args,
        };
        let caller = AgentIdentity::new(self.agent_id.as_str(), self.session_id.as_str());
        Ok(verdict_to_js(self.inner.evaluate(&call, &caller)))
    }
}

// ─────────────────────────────────────────── Eval primitives ────────────────────────────────────
// The generate→evaluate quality gate's stateless compute (0.5.0 fold of the former `EvalPipeline`
// class, OS-axis #6). The SDK `AttemptLoop` drives the loop; these expose the kernel's prompt
// builder + verdict parser + verdict schema so eval compute stays single-sourced in the kernel.

#[napi(object)]
#[derive(Clone)]
pub struct Criterion {
    pub text: String,
    pub required: bool,
    pub weight: Option<f64>,
}

#[napi(object)]
#[derive(Clone)]
pub struct CriterionResult {
    pub criterion: String,
    pub passed: bool,
    pub score: f64,
    pub feedback: String,
}

#[napi(object)]
#[derive(Clone)]
pub struct SkillCandidate {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub content: String,
}

/// The structured verdict from [`parseVerdict`]: `passed`, `overallScore`, `feedback`, per-criterion
/// `details`, and an optional `skillCandidate` distilled from a passing run.
#[napi(object)]
#[derive(Clone)]
pub struct Verdict {
    pub passed: bool,
    pub overall_score: f64,
    pub feedback: String,
    pub details: Vec<CriterionResult>,
    pub skill_candidate: Option<SkillCandidate>,
}

/// Build the impartial-evaluator messages for one attempt (system contract + the goal/criteria/output
/// user message). Call the evaluator LLM with these, then feed the text to [`parseVerdict`].
#[napi]
pub fn build_eval_messages(
    goal: String,
    criteria: Vec<Criterion>,
    result: String,
    attempt: u32,
    extract_skill_on_pass: bool,
) -> Vec<Message> {
    let rust_criteria: Vec<RustCriterion> = criteria
        .into_iter()
        .map(|c| RustCriterion {
            text: c.text,
            required: c.required,
            weight: c.weight.map(|w| w as f32).unwrap_or(1.0),
        })
        .collect();
    rust_build_eval_messages(
        &goal,
        &rust_criteria,
        &result,
        attempt,
        extract_skill_on_pass,
    )
    .iter()
    .map(message_from_rust)
    .collect()
}

/// Parse the evaluator LLM's JSON response into a structured [`Verdict`] (tolerant of fences / missing
/// fields).
#[napi]
pub fn parse_verdict(content: String) -> Verdict {
    let r = rust_parse_verdict(&content);
    Verdict {
        passed: r.passed,
        overall_score: r.overall_score as f64,
        feedback: r.feedback,
        details: r
            .details
            .into_iter()
            .map(|d| CriterionResult {
                criterion: d.criterion,
                passed: d.passed,
                score: d.score as f64,
                feedback: d.feedback,
            })
            .collect(),
        skill_candidate: r.skill_candidate.map(|s| SkillCandidate {
            name: s.name,
            description: s.description,
            when_to_use: s.when_to_use,
            content: s.content,
        }),
    }
}

/// JSON Schema (as a JSON string) for the verdict an eval node must produce — used as the
/// `outputSchema` of the eval node in the `gen_eval` workflow template.
#[napi]
pub fn verdict_output_schema(extract_skill_on_pass: bool) -> String {
    rust_verdict_output_schema(extract_skill_on_pass).to_string()
}
