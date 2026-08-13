#![allow(deprecated)]

use compact_str::CompactString;
use js_sys::Uint8Array;
use serde::{Deserialize, Serialize};
use tsify_next::Tsify;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

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
use deepstrike_core::types::message::{
    Content, ContentPart, Message as RustMessage, Role, ToolCall as RustToolCall,
};
use deepstrike_core::types::policy::GovernanceVerdict as RustGovernanceVerdict;
use deepstrike_core::types::policy::SignalDisposition as RustSignalDisposition;
use deepstrike_core::types::signal::{
    RuntimeSignal as RustRuntimeSignal, SignalSource as RustSignalSource,
    SignalType as RustSignalType, Urgency as RustUrgency,
};

// ────────────────────────────────────────────── POD types ──────────────────────────────────────────────

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct ContentPartObj {
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_parts: Option<Vec<ContentPartObj>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u32>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_fatal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u32>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: String,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTask {
    pub goal: String,
    #[serde(default)]
    pub criteria: Vec<String>,
    /// Freeform lane label. Well-known: `"orchestrate"` | `"implement"` (default) | `"retrieve"` | `"verify"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct LoopPolicy {
    pub max_tokens: u32,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_max_total_tokens")]
    pub max_total_tokens: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<f64>,
}

fn default_max_turns() -> u32 {
    25
}
fn default_max_total_tokens() -> f64 {
    1_000_000.0
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct LoopResult {
    pub termination: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_message: Option<Message>,
    pub turns_used: u32,
    pub total_tokens_used: f64,
}

// ────────────────────────────────────────────── Skill types ──────────────────────────────────────────────

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct SkillMetadata {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<u8>,
    #[serde(default)]
    pub estimated_tokens: u32,
}

// ────────────────────────────────────────────── Signal types ──────────────────────────────────────────────

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSignal {
    pub id: String,
    /// "cron" | "gateway" | "heartbeat" | "custom"
    pub source: String,
    /// "event" | "job" | "alert"
    pub signal_type: String,
    /// "low" | "normal" | "high" | "critical"
    pub urgency: String,
    pub summary: String,
    pub payload: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    /// Target a specific session loop (sessionId). Omitted ⇒ broadcast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    /// Absolute journal-clock deadline for optional urgency escalation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<f64>,
    /// Merge this signal with an unconsumed queued signal carrying the same key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_key: Option<String>,
    /// Deterministic number of host signals represented by this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesced_count: Option<u32>,
    pub timestamp_ms: f64,
}

fn runtime_signal_to_rust(s: RuntimeSignal) -> Result<RustRuntimeSignal, JsValue> {
    // §7.7 · the signal id is the caller's own branded ref, kept verbatim (see the Node binding).
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
    RuntimeSignal {
        id: s.id.to_string(),
        source: match s.source {
            RustSignalSource::Cron => "cron",
            RustSignalSource::Gateway => "gateway",
            RustSignalSource::Heartbeat => "heartbeat",
            RustSignalSource::Custom => "custom",
        }
        .into(),
        signal_type: match s.signal_type {
            RustSignalType::Event => "event",
            RustSignalType::Job => "job",
            RustSignalType::Alert => "alert",
        }
        .into(),
        urgency: match s.urgency {
            RustUrgency::Critical => "critical",
            RustUrgency::High => "high",
            RustUrgency::Normal => "normal",
            RustUrgency::Low => "low",
        }
        .into(),
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

fn disposition_str(d: RustSignalDisposition) -> &'static str {
    d.label()
}

// ────────────────────────────── Provider context ──────────────────────────────

/// Structured context for a provider call — emitted with `kind === "call_llm"`.
#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct RenderedContext {
    pub system_text: String,
    pub system_stable: String,
    pub system_knowledge: String,
    pub turns: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_turn: Option<Message>,
    /// P1-E: count of leading `turns` forming the frozen prefix; absent ⇒ rolling-pair fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_prefix_len: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_overflow: Option<ContextBudgetOverflow>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct ContextBudgetOverflow {
    pub kind: String,
    pub required_tokens: u32,
    pub max_tokens: u32,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct Criterion {
    pub text: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct CriterionResult {
    pub criterion: String,
    pub passed: bool,
    pub score: f64,
    pub feedback: String,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct SkillCandidate {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    pub content: String,
}

/// The structured verdict from `parseVerdict`.
#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub passed: bool,
    pub overall_score: f64,
    pub feedback: String,
    pub details: Vec<CriterionResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_candidate: Option<SkillCandidate>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceVerdict {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<f64>,
}

// ────────────────────────────────────── conversion helpers ──────────────────────────────────────

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

fn governance_verdict_from_rust(verdict: RustGovernanceVerdict) -> GovernanceVerdict {
    match verdict {
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

// ─────────────────────────────── Canonical Kernel ABI ───────────────────────────────

#[derive(Tsify, Serialize)]
#[tsify(into_wasm_abi)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CanonicalPreparation {
    Prepared {
        prepare_token: String,
        step_seq: String,
        #[tsify(optional)]
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_head: Option<String>,
        record_digest: String,
        #[tsify(type = "Uint8Array")]
        #[serde(serialize_with = "serialize_bytes")]
        record_bytes: Vec<u8>,
        planned_step_json: String,
    },
    Replayed {
        step_seq: String,
        #[tsify(optional)]
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_head: Option<String>,
        record_digest: String,
        #[tsify(optional, type = "Uint8Array")]
        #[serde(
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_optional_bytes"
        )]
        record_bytes: Option<Vec<u8>>,
        #[tsify(optional)]
        #[serde(skip_serializing_if = "Option::is_none")]
        planned_step_json: Option<String>,
    },
    Rejected {
        fault_json: String,
    },
}

#[derive(Tsify, Serialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalCommit {
    pub step_seq: String,
    pub record_digest: String,
    pub planned_step_json: String,
    #[tsify(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_advice_json: Option<String>,
}

#[derive(Tsify, Serialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalCheckpoint {
    #[tsify(type = "Uint8Array")]
    #[serde(serialize_with = "serialize_bytes")]
    pub checkpoint_bytes: Vec<u8>,
    pub through_step_seq: String,
    pub covered_head: String,
    pub state_digest: String,
    pub ack_token: String,
}

#[derive(Tsify, Serialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalRestoreCost {
    pub records_before_checkpoint: String,
    pub tail_inputs_replayed: String,
    pub records_after_checkpoint: String,
    pub bytes_read: String,
}

/// TypeScript surface for `CanonicalKernel.restore`'s record-bytes argument.
///
/// Intentionally NOT `#[tsify(from_wasm_abi)]`: tsify's throw-on-bad-arg path runs
/// before the `&mut self` WasmRefCell guard is released, permanently bricking the
/// handle (`recursive use of an object`). Conversion is fallible and in-body.
#[wasm_bindgen(typescript_custom_section)]
const TS_CANONICAL_RECORD_BYTES: &'static str = r#"
export type CanonicalRecordBytes = Uint8Array[];
"#;

#[derive(Tsify, Serialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalLifecycle {
    Created,
    Configured,
    Running,
    Suspended,
    Completed,
    Cancelled,
    Failed,
}

/// The single ABI revision accepted by core. Hosts must read this export instead of copying it.
#[wasm_bindgen(js_name = kernelAbiVersion)]
pub fn kernel_abi_version() -> u32 {
    deepstrike_core::runtime::kernel::wire::KERNEL_ABI_VERSION
}

/// Durable canonical operation handle. It exposes no direct `step` method.
#[wasm_bindgen]
pub struct CanonicalKernel {
    inner: RustCanonicalKernel,
}

#[wasm_bindgen]
impl CanonicalKernel {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: RustCanonicalKernel::default(),
        }
    }

    /// Strictly decode and prepare one canonical envelope.
    #[wasm_bindgen]
    pub fn prepare(&mut self, input_json: String) -> Result<CanonicalPreparation, JsValue> {
        canonical_preparation_from_rust(self.inner.prepare_json(&input_json))
    }

    /// Commit only after the journal appended the prepared record.
    #[wasm_bindgen]
    pub fn commit(
        &mut self,
        prepare_token: String,
        appended_head: String,
    ) -> Result<CanonicalCommit, JsValue> {
        let token = RustPrepareToken::new(prepare_token)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let head = RustDigest::new(appended_head)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
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
    }

    /// Abort a candidate that did not reach the journal.
    #[wasm_bindgen]
    pub fn abort(&mut self, prepare_token: String) -> Result<(), JsValue> {
        let token = RustPrepareToken::new(prepare_token)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        self.inner
            .abort(&token)
            .map(|_| ())
            .map_err(canonical_kernel_error)
    }

    #[wasm_bindgen(js_name = checkpointCandidate)]
    pub fn checkpoint_candidate(&self) -> Result<CanonicalCheckpoint, JsValue> {
        self.inner
            .checkpoint_candidate()
            .map_err(canonical_kernel_error)
            .map(canonical_checkpoint_from_rust)
    }

    #[wasm_bindgen(js_name = checkpointRebase)]
    pub fn checkpoint_rebase(
        &self,
        checkpoint_bytes: Uint8Array,
    ) -> Result<CanonicalCheckpoint, JsValue> {
        let checkpoint = RustKernelCheckpoint::from_checkpoint_bytes(&checkpoint_bytes.to_vec())
            .map_err(|error| canonical_kernel_error(error.fault()))?;
        self.inner
            .checkpoint_rebase(&checkpoint)
            .map_err(canonical_kernel_error)
            .map(canonical_checkpoint_from_rust)
    }

    #[wasm_bindgen(js_name = ackCheckpoint)]
    pub fn ack_checkpoint(
        &mut self,
        through_step_seq: String,
        covered_head: String,
    ) -> Result<(), JsValue> {
        let boundary = RustCheckpointBoundary {
            through_step_seq: RustWireU64::parse(&through_step_seq)
                .map_err(|error| JsValue::from_str(&error.to_string()))?,
            covered_head: RustDigest::new(covered_head)
                .map_err(|error| JsValue::from_str(&error.to_string()))?,
        };
        self.inner
            .note_checkpoint_acked(&boundary)
            .map(|_| ())
            .map_err(canonical_kernel_error)
    }

    /// Replace this handle in place from checkpoint bytes plus post-checkpoint record bytes.
    #[wasm_bindgen]
    pub fn restore(
        &mut self,
        checkpoint_bytes: Option<Uint8Array>,
        record_bytes: JsValue,
    ) -> Result<CanonicalRestoreCost, JsValue> {
        // Decode inside the method body so a bad argument returns `Err` through the
        // normal wasm-bindgen Result path (which releases the WasmRefCell borrow).
        let records = canonical_record_bytes_from_js(record_bytes)?;
        let checkpoint = checkpoint_bytes.as_ref().map(Uint8Array::to_vec);
        let cost = self
            .inner
            .restore_bytes(checkpoint.as_deref(), &records)
            .map_err(canonical_kernel_error)?;

        Ok(CanonicalRestoreCost {
            records_before_checkpoint: cost.records_before_checkpoint.to_string(),
            tail_inputs_replayed: cost.tail_inputs_replayed.to_string(),
            records_after_checkpoint: cost.records_after_checkpoint.to_string(),
            bytes_read: cost.bytes_read.to_string(),
        })
    }

    #[wasm_bindgen]
    pub fn lifecycle(&self) -> CanonicalLifecycle {
        match self.inner.lifecycle() {
            RustOperationLifecycle::Created => CanonicalLifecycle::Created,
            RustOperationLifecycle::Configured => CanonicalLifecycle::Configured,
            RustOperationLifecycle::Running => CanonicalLifecycle::Running,
            RustOperationLifecycle::Suspended => CanonicalLifecycle::Suspended,
            RustOperationLifecycle::Completed => CanonicalLifecycle::Completed,
            RustOperationLifecycle::Cancelled => CanonicalLifecycle::Cancelled,
            RustOperationLifecycle::Failed => CanonicalLifecycle::Failed,
        }
    }

    #[wasm_bindgen(js_name = pendingEffectsJson)]
    pub fn pending_effects_json(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.inner.pending_effects().collect::<Vec<_>>()).map_err(json_error)
    }

    #[wasm_bindgen(js_name = terminalJson)]
    pub fn terminal_json(&self) -> Result<Option<String>, JsValue> {
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
) -> Result<CanonicalPreparation, JsValue> {
    match preparation {
        RustKernelPreparation::Prepared(prepared) => Ok(CanonicalPreparation::Prepared {
            prepare_token: prepared.token.to_string(),
            step_seq: prepared.record.step_seq().to_string(),
            expected_head: prepared.record.expected_head().map(ToString::to_string),
            record_digest: prepared.record.record_digest().to_string(),
            record_bytes: prepared.record.record_bytes().as_slice().to_vec(),
            planned_step_json: serde_json::to_string(&prepared.planned_step).map_err(json_error)?,
        }),
        RustKernelPreparation::Replayed(replayed) => Ok(CanonicalPreparation::Replayed {
            step_seq: replayed.step_seq.to_string(),
            expected_head: replayed
                .record
                .as_ref()
                .and_then(|record| record.expected_head())
                .map(ToString::to_string),
            record_digest: replayed.record_digest.to_string(),
            record_bytes: replayed
                .record
                .map(|record| record.record_bytes().as_slice().to_vec()),
            planned_step_json: replayed
                .committed_step
                .map(|step| serde_json::to_string(&step))
                .transpose()
                .map_err(json_error)?,
        }),
        RustKernelPreparation::Rejected(rejected) => Ok(CanonicalPreparation::Rejected {
            fault_json: serde_json::to_string(&rejected.fault).map_err(json_error)?,
        }),
    }
}

fn canonical_checkpoint_from_rust(
    checkpoint: deepstrike_core::runtime::kernel::wire::CheckpointCandidate,
) -> CanonicalCheckpoint {
    CanonicalCheckpoint {
        checkpoint_bytes: checkpoint.checkpoint_bytes.as_slice().to_vec(),
        through_step_seq: checkpoint.through_step_seq.to_string(),
        covered_head: checkpoint.covered_head.to_string(),
        state_digest: checkpoint.state_digest.to_string(),
        ack_token: checkpoint.ack_token.to_string(),
    }
}

fn serialize_bytes<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_bytes(bytes)
}

fn serialize_optional_bytes<S>(bytes: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match bytes {
        Some(bytes) => serializer.serialize_some(&Bytes(bytes)),
        None => serializer.serialize_none(),
    }
}

struct Bytes<'a>(&'a [u8]);

impl Serialize for Bytes<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bytes(self.0)
    }
}

/// Fallible JS → record-bytes conversion that returns `Err` instead of throwing across
/// the WasmRefCell boundary (see C1 / `CanonicalKernel::restore`).
fn canonical_record_bytes_from_js(value: JsValue) -> Result<Vec<Vec<u8>>, JsValue> {
    if value.is_null() || value.is_undefined() || !js_sys::Array::is_array(&value) {
        return Err(JsValue::from_str(
            "record_bytes must be an Array of Uint8Array",
        ));
    }
    let array = js_sys::Array::from(&value);
    let mut records = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let entry = array.get(index);
        let bytes = entry
            .dyn_into::<Uint8Array>()
            .map_err(|_| JsValue::from_str("record_bytes entries must be Uint8Array"))?;
        records.push(bytes.to_vec());
    }
    Ok(records)
}

fn canonical_kernel_error(fault: deepstrike_core::runtime::kernel::wire::KernelFault) -> JsValue {
    JsValue::from_str(&serde_json::to_string(&fault).unwrap_or_else(|_| fault.to_string()))
}

fn json_error(error: serde_json::Error) -> JsValue {
    JsValue::from_str(&error.to_string())
}

// ────────────────────────────────────────────── SignalRouter ──────────────────────────────────────────────

#[wasm_bindgen]
pub struct SignalRouter {
    inner: RustSignalRouter,
}

#[wasm_bindgen]
impl SignalRouter {
    #[wasm_bindgen(constructor)]
    pub fn new(max_queue_size: u32) -> Self {
        Self {
            inner: RustSignalRouter::new(max_queue_size as usize),
        }
    }

    /// Ingest a signal. Returns disposition: "ignore"|"observe"|"queue"|"run"|"interrupt"|"interrupt_now"|"dropped"
    #[wasm_bindgen]
    pub fn ingest(&mut self, signal: RuntimeSignal, lifecycle: String) -> Result<String, JsValue> {
        let lifecycle = match lifecycle.as_str() {
            "ready" => TaskLifecycle::Ready,
            "running" => TaskLifecycle::Running,
            "suspended" => TaskLifecycle::Suspended,
            "done" => {
                TaskLifecycle::Done(deepstrike_core::types::result::TerminationReason::Completed)
            }
            other => {
                return Err(JsValue::from_str(&format!(
                    "invalid task lifecycle {other:?}; expected ready|running|suspended|done"
                )));
            }
        };
        Ok(disposition_str(
            self.inner
                .ingest(runtime_signal_to_rust(signal)?, lifecycle),
        )
        .into())
    }

    /// Pull the next queued signal (highest priority first).
    #[wasm_bindgen]
    pub fn next(&mut self) -> Option<RuntimeSignal> {
        self.inner.next().as_ref().map(runtime_signal_from_rust)
    }

    #[wasm_bindgen]
    pub fn depth(&self) -> u32 {
        self.inner.depth() as u32
    }

    #[wasm_bindgen(js_name = clearDedup)]
    pub fn clear_dedup(&mut self) {
        self.inner.clear_dedup();
    }
}

// ────────────────────────────────────────────── Eval primitives ──────────────────────────────────────────────
// The generate→evaluate quality gate's stateless compute (0.5.0 fold of the former `EvalPipeline`
// class, OS-axis #6). The SDK `AttemptLoop` drives the loop; these expose the kernel's prompt
// builder + verdict parser + verdict schema.

/// Build the impartial-evaluator messages for one attempt. Call the evaluator LLM with these, then
/// feed the text to `parseVerdict`.
#[wasm_bindgen(js_name = buildEvalMessages)]
pub fn build_eval_messages(
    goal: String,
    criteria: Vec<Criterion>,
    result: String,
    attempt: u32,
    extract_skill_on_pass: bool,
) -> Vec<Message> {
    let rust_criteria: Vec<RustCriterion> = criteria
        .into_iter()
        .map(|criterion| RustCriterion {
            text: criterion.text,
            required: criterion.required,
            weight: criterion.weight.map(|weight| weight as f32).unwrap_or(1.0),
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

/// Parse the evaluator LLM's JSON response into a structured `Verdict`.
#[wasm_bindgen(js_name = parseVerdict)]
pub fn parse_verdict(content: String) -> Verdict {
    let r = rust_parse_verdict(&content);
    Verdict {
        passed: r.passed,
        overall_score: r.overall_score as f64,
        feedback: r.feedback,
        details: r
            .details
            .into_iter()
            .map(|detail| CriterionResult {
                criterion: detail.criterion,
                passed: detail.passed,
                score: detail.score as f64,
                feedback: detail.feedback,
            })
            .collect(),
        skill_candidate: r.skill_candidate.map(|skill| SkillCandidate {
            name: skill.name,
            description: skill.description,
            when_to_use: skill.when_to_use,
            content: skill.content,
        }),
    }
}

/// JSON Schema (as a JSON string) for the verdict an eval node must produce — used as the
/// `outputSchema` of the eval node in the `gen_eval` workflow template.
#[wasm_bindgen(js_name = verdictOutputSchema)]
pub fn verdict_output_schema(extract_skill_on_pass: bool) -> String {
    rust_verdict_output_schema(extract_skill_on_pass).to_string()
}

// ────────────────────────────────────────────── Governance ──────────────────────────────────────────────

#[wasm_bindgen]
pub struct Governance {
    inner: RustGovernancePipeline,
    agent_id: String,
    session_id: String,
}

#[wasm_bindgen]
impl Governance {
    #[wasm_bindgen(constructor)]
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

    #[wasm_bindgen(js_name = setIdentity)]
    pub fn set_identity(&mut self, agent_id: String, session_id: String) {
        self.agent_id = agent_id;
        self.session_id = session_id;
    }

    #[wasm_bindgen(js_name = addPermissionRule)]
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

    #[wasm_bindgen(js_name = blockTool)]
    pub fn block_tool(&mut self, name: String) {
        self.inner.veto.block_tool(name);
    }

    #[wasm_bindgen(js_name = setRateLimit)]
    pub fn set_rate_limit(&mut self, tool_name: String, max_calls: u32, window_ms: f64) {
        self.inner.rate_limiter.set_limit(
            tool_name,
            RateLimit {
                max_calls,
                window_ms: window_ms as u64,
            },
        );
    }

    #[wasm_bindgen(js_name = requireParam)]
    pub fn require_param(&mut self, tool_name: String, param_path: String) {
        self.inner.constraints.add(ParamConstraint {
            tool_name,
            param_path,
            rule: ConstraintRule::Required,
        });
    }

    #[wasm_bindgen(js_name = allowParamValues)]
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

    #[wasm_bindgen(js_name = limitParamRange)]
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

    #[wasm_bindgen(js_name = setTime)]
    pub fn set_time(&mut self, now_ms: f64) {
        self.inner.set_time(now_ms as u64);
    }

    #[wasm_bindgen]
    pub fn evaluate(&mut self, tool_name: String, args_json: String) -> GovernanceVerdict {
        let args = serde_json::from_str(&args_json).unwrap_or(serde_json::Value::Null);
        let call = RustToolCall {
            id: CompactString::new(""),
            name: CompactString::new(&tool_name),
            arguments: args,
        };
        let caller = AgentIdentity::new(&self.agent_id, &self.session_id);
        governance_verdict_from_rust(self.inner.evaluate(&call, &caller))
    }
}

// ────────────────────────────── Durable-memory wire values ──────────────────────────────────────

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct SessionData {
    pub session_id: String,
    pub agent_id: String,
    pub messages: Vec<Message>,
    /// JSON-encoded metadata blob.
    pub metadata: String,
    pub created_at_ms: f64,
    pub updated_at_ms: f64,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecord {
    pub record_id: String,
    pub scope: MemoryScope,
    pub name: String,
    pub kind: String,
    pub content: String,
    pub description: String,
    pub provenance: MemoryProvenance,
    pub created_at: f64,
    pub updated_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recalled_at: Option<f64>,
    pub recall_count: f64,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_days: Option<u32>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryScope {
    pub tenant_id: String,
    pub namespace: String,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub author: String,
    pub trust: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}
