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

use compact_str::CompactString;

use deepstrike_core::context::renderer::{
    ContextBudgetOverflow as RustContextBudgetOverflow,
    ContextBudgetOverflowKind as RustContextBudgetOverflowKind,
    RenderedContext as RustRenderedContext,
};
use deepstrike_core::governance::constraint::{ConstraintRule, ParamConstraint};
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline as RustGovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::harness::eval::{
    Criterion as RustCriterion, build_eval_messages as rust_build_eval_messages,
    parse_verdict as rust_parse_verdict, verdict_output_schema as rust_verdict_output_schema,
};
use deepstrike_core::memory::durable::SessionData as RustSessionData;
use deepstrike_core::mm::memory::{
    MemoryAuthor as RustMemoryAuthor, MemoryKind as RustMemoryKind,
    MemoryProvenance as RustMemoryProvenance, MemoryRecord as RustMemoryRecord,
    MemoryScope as RustMemoryScope, MemoryTrustLevel as RustMemoryTrustLevel,
};
use deepstrike_core::runtime::KernelRuntime as RustKernelRuntime;
use deepstrike_core::scheduler::policy::SchedulerBudget as RustLoopPolicy;
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
pub struct LoopPolicy {
    pub max_tokens: u32,
    pub max_turns: Option<u32>,
    pub max_total_tokens: Option<BigInt>,
    pub timeout_ms: Option<BigInt>,
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
    let id =
        s.id.parse()
            .map_err(|_| Error::from_reason("signal id must be a UUID"))?;
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
    sig.id = id;
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

// ────────────────────────────── Provider context ──────────────────────────────

/// Structured context for a provider call — emitted with `kind === "call_llm"`.
/// Separates system configuration from the conversation transcript so providers
/// can map each field to their own API contract without role-filtering.
#[napi(object)]
#[derive(Clone)]
pub struct RenderedContext {
    /// Identity + Knowledge combined — for providers with a single system slot (OpenAI).
    pub system_text: String,
    /// Identity only (system partition). Anthropic system[0] with cache_control.
    pub system_stable: String,
    /// Knowledge (memory retrievals, skill definitions, artifacts). Anthropic system[1] with cache_control.
    pub system_knowledge: String,
    /// History turns only — the stable, cacheable message prefix.
    pub turns: Vec<Message>,
    /// Volatile State turn (task_state + signals), rendered after the cacheable history.
    pub state_turn: Option<Message>,
    /// P1-E: count of leading `turns` forming the frozen prefix (byte-stable until the next
    /// compaction). Providers pin a deep cache breakpoint here; absent ⇒ rolling-pair fallback.
    pub frozen_prefix_len: Option<u32>,
    /// Fail-closed evidence: this projection must not be submitted to a provider.
    pub budget_overflow: Option<ContextBudgetOverflow>,
}

#[napi(object)]
#[derive(Clone)]
pub struct ContextBudgetOverflow {
    pub kind: String,
    pub required_tokens: u32,
    pub max_tokens: u32,
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

fn role_str_to_rust(role: &str) -> Result<Role> {
    match role {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(Error::new(
            Status::InvalidArg,
            format!("invalid role: {other}"),
        )),
    }
}

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn content_part_to_rust(p: ContentPartObj) -> ContentPart {
    match p.r#type.as_str() {
        "image" => ContentPart::Image {
            url: p.url,
            data: p.data,
            media_type: p.media_type,
            detail: p.detail,
        },
        "audio" => ContentPart::Audio {
            data: p.data.unwrap_or_default(),
            media_type: p.media_type.unwrap_or_else(|| "audio/wav".into()),
        },
        "tool_result" => ContentPart::ToolResult {
            call_id: CompactString::new(&p.call_id.unwrap_or_default()),
            output: p.output.unwrap_or_default(),
            is_error: p.is_error.unwrap_or(false),
        },
        _ => ContentPart::Text {
            text: p.text.unwrap_or_default(),
        },
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

fn message_to_rust(m: Message) -> Result<RustMessage> {
    let role = role_str_to_rust(&m.role)?;
    let tool_calls: Vec<RustToolCall> = m
        .tool_calls
        .into_iter()
        .map(tool_call_to_rust)
        .collect::<Result<_>>()?;
    let content = match m.content_parts {
        Some(parts) if !parts.is_empty() => {
            Content::Parts(parts.into_iter().map(content_part_to_rust).collect())
        }
        _ => Content::Text(m.content),
    };
    Ok(RustMessage {
        role,
        content,
        tool_calls,
        token_count: m.token_count,
    })
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

fn tool_call_to_rust(c: ToolCall) -> Result<RustToolCall> {
    let args: serde_json::Value = serde_json::from_str(&c.arguments)
        .map_err(|e| Error::new(Status::InvalidArg, format!("invalid JSON arguments: {e}")))?;
    Ok(RustToolCall {
        id: CompactString::new(&c.id),
        name: CompactString::new(&c.name),
        arguments: args,
    })
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

fn policy_to_rust(p: LoopPolicy) -> RustLoopPolicy {
    RustLoopPolicy {
        max_tokens: p.max_tokens,
        max_turns: p.max_turns.unwrap_or(25),
        max_total_tokens: p
            .max_total_tokens
            .map(|b| b.get_u64().1)
            .unwrap_or(1_000_000),
        max_wall_ms: p.timeout_ms.map(|b| b.get_u64().1),
    }
}

fn rendered_context_from_rust(rc: RustRenderedContext) -> RenderedContext {
    RenderedContext {
        system_text: rc.system_text,
        system_stable: rc.system_stable,
        system_knowledge: rc.system_knowledge,
        turns: rc.turns.iter().map(message_from_rust).collect(),
        state_turn: rc.state_turn.as_ref().map(message_from_rust),
        frozen_prefix_len: rc.frozen_prefix_len.map(|n| n as u32),
        budget_overflow: rc.budget_overflow.as_ref().map(context_budget_overflow_from_rust),
    }
}

fn context_budget_overflow_from_rust(value: &RustContextBudgetOverflow) -> ContextBudgetOverflow {
    ContextBudgetOverflow {
        kind: match value.kind {
            RustContextBudgetOverflowKind::FixedContext => "fixed_context",
            RustContextBudgetOverflowKind::ProtectedTail => "protected_tail",
        }
        .to_string(),
        required_tokens: value.required_tokens,
        max_tokens: value.max_tokens,
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

// ─────────────────────────────────────────── KernelRuntime ───────────────────────────────────────────

/// Versioned kernel ABI runtime. Accepts/returns JSON encoded
/// `KernelInput`/`KernelStep` payloads from deepstrike-core.
#[napi]
pub struct KernelRuntime {
    inner: RustKernelRuntime,
}

#[napi]
impl KernelRuntime {
    #[napi(constructor)]
    pub fn new(policy: LoopPolicy) -> Self {
        Self {
            inner: RustKernelRuntime::new(policy_to_rust(policy)),
        }
    }

    #[napi]
    pub fn step(&mut self, input_json: String) -> Result<String> {
        // Guard the core step (context compaction, rendering, scheduling) — a panic
        // in here must not abort the whole Node process. See `ffi_guard`.
        ffi_guard("KernelRuntime.step", || {
            let step = self.inner.step_json(&input_json).map_err(|e| {
                Error::new(Status::InvalidArg, format!("invalid KernelInput JSON: {e}"))
            })?;
            serde_json::to_string(&step).map_err(|e| {
                Error::new(
                    Status::GenericFailure,
                    format!("failed to encode KernelStep: {e}"),
                )
            })
        })
    }

    /// Stage a KernelInput so the host can durably persist it before publishing the transition.
    #[napi(js_name = "prepareStep")]
    pub fn prepare_step(&mut self, input_json: String) -> Result<String> {
        ffi_guard("KernelRuntime.prepareStep", || {
            let prepared = self.inner.prepare_step_json(&input_json).map_err(|e| {
                Error::new(Status::InvalidArg, format!("invalid KernelInput JSON: {e}"))
            })?;
            serde_json::to_string(&prepared).map_err(|e| {
                Error::new(
                    Status::GenericFailure,
                    format!("failed to encode KernelPreparedStep: {e}"),
                )
            })
        })
    }

    /// Publish a previously staged transition after durable host persistence succeeds.
    #[napi(js_name = "commitPrepared")]
    pub fn commit_prepared(&mut self, prepare_token: String) -> Result<String> {
        ffi_guard("KernelRuntime.commitPrepared", || {
            let step = self
                .inner
                .commit_prepared(&prepare_token)
                .map_err(|fault| {
                    Error::new(
                        Status::InvalidArg,
                        serde_json::to_string(&fault).unwrap_or(fault.message),
                    )
                })?;
            serde_json::to_string(&step).map_err(|e| {
                Error::new(
                    Status::GenericFailure,
                    format!("failed to encode KernelStep: {e}"),
                )
            })
        })
    }

    /// Roll back a staged transition when durable host persistence fails.
    #[napi(js_name = "abortPrepared")]
    pub fn abort_prepared(&mut self, prepare_token: String) -> Result<()> {
        ffi_guard("KernelRuntime.abortPrepared", || {
            self.inner.abort_prepared(&prepare_token).map_err(|fault| {
                Error::new(
                    Status::InvalidArg,
                    serde_json::to_string(&fault).unwrap_or(fault.message),
                )
            })
        })
    }

    /// Encode a portable ABI-v2 runtime checkpoint.
    #[napi]
    pub fn snapshot(&self) -> Result<String> {
        self.inner.snapshot_json().map_err(|fault| {
            Error::new(
                Status::InvalidArg,
                serde_json::to_string(&fault).unwrap_or(fault.message),
            )
        })
    }

    /// Replace this runtime with a checkpoint restored by deterministic ABI replay.
    #[napi]
    pub fn restore(&mut self, snapshot_json: String) -> Result<()> {
        self.inner = RustKernelRuntime::restore_snapshot_json(&snapshot_json).map_err(|fault| {
            Error::new(
                Status::InvalidArg,
                serde_json::to_string(&fault).unwrap_or(fault.message),
            )
        })?;
        Ok(())
    }

    /// Return a read-only JSON resource projection without mutating kernel state.
    #[napi]
    pub fn diagnostics(&self) -> Result<String> {
        serde_json::to_string(&self.inner.diagnostics()).map_err(|error| {
            Error::new(
                Status::GenericFailure,
                format!("failed to encode kernel diagnostics: {error}"),
            )
        })
    }

    #[napi]
    pub fn is_terminal(&self) -> bool {
        self.inner.is_terminal()
    }

    #[napi]
    pub fn turn(&self) -> u32 {
        self.inner.turn()
    }

    /// L1 (RunGroup): cumulative sub-agent spawns this run, for charging the group ledger at run end.
    #[napi]
    pub fn local_subagents_spawned(&self) -> u32 {
        self.inner.local_subagents_spawned()
    }

    #[napi]
    pub fn recovery_content_bytes(&self) -> u32 {
        self.inner.recovery_content_bytes() as u32
    }

    #[napi]
    pub fn render(&self) -> Result<RenderedContext> {
        // Guard render's truncation/projection — any mis-sliced string must throw,
        // not abort the process. `Result<T>` maps to the same TS shape as `T`.
        ffi_guard("KernelRuntime.render", || {
            Ok(rendered_context_from_rust(self.inner.render()))
        })
    }

    #[napi]
    pub fn drain_new_messages(&mut self) -> Vec<Message> {
        self.inner
            .drain_new_messages()
            .iter()
            .map(message_from_rust)
            .collect()
    }

    #[napi]
    pub fn preserved_refs(&self) -> Vec<String> {
        self.inner.preserved_refs()
    }
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
        let args: serde_json::Value =
            serde_json::from_str(&args_json).unwrap_or(serde_json::Value::Null);
        let call = RustToolCall {
            id: compact_str::CompactString::new(""),
            name: compact_str::CompactString::new(&tool_name),
            arguments: args,
        };
        let caller = AgentIdentity::new(self.agent_id.as_str(), self.session_id.as_str());
        Ok(verdict_to_js(self.inner.evaluate(&call, &caller)))
    }
}

// ──────────────────────────────── Dream / idle-pipeline POD types ────────────────────────────────

/// A completed session transcript for durable-memory extraction.
#[napi(object)]
#[derive(Clone)]
pub struct SessionData {
    pub session_id: String,
    pub agent_id: String,
    /// Messages from this session.
    pub messages: Vec<Message>,
    /// JSON-encoded metadata blob.
    pub metadata: String,
    /// Unix ms timestamp.
    pub created_at_ms: f64,
    /// Unix ms timestamp.
    pub updated_at_ms: f64,
}

#[napi(object)]
#[derive(Clone)]
pub struct MemoryScope {
    pub tenant_id: String,
    pub namespace: String,
}

#[napi(object)]
#[derive(Clone)]
pub struct MemoryProvenance {
    pub session_id: Option<String>,
    pub author: String,
    pub trust: String,
    pub evidence_refs: Vec<String>,
}

/// The single public long-term memory wire.
#[napi(object)]
#[derive(Clone)]
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
    pub last_recalled_at: Option<f64>,
    pub recall_count: f64,
    pub confidence: f64,
    pub links: Vec<String>,
    pub pinned: bool,
    pub ttl_days: Option<u32>,
}

// ─────────────────────── Dream conversion helpers ───────────────────────

fn session_data_to_rust(s: SessionData) -> Result<RustSessionData> {
    let messages: Vec<RustMessage> = s
        .messages
        .into_iter()
        .map(message_to_rust)
        .collect::<Result<_>>()?;
    let metadata: serde_json::Value =
        serde_json::from_str(&s.metadata).unwrap_or(serde_json::Value::Null);
    Ok(RustSessionData {
        session_id: s.session_id,
        agent_id: s.agent_id,
        messages,
        metadata,
        created_at_ms: s.created_at_ms as u64,
        updated_at_ms: s.updated_at_ms as u64,
    })
}

fn memory_record_to_rust(record: MemoryRecord) -> Result<RustMemoryRecord> {
    let kind = match record.kind.as_str() {
        "user" => RustMemoryKind::User,
        "feedback" => RustMemoryKind::Feedback,
        "project" => RustMemoryKind::Project,
        "reference" => RustMemoryKind::Reference,
        other => return Err(Error::from_reason(format!("invalid memory kind {other:?}"))),
    };
    let author = match record.provenance.author.as_str() {
        "model" => RustMemoryAuthor::Model,
        "host" => RustMemoryAuthor::Host,
        "extraction" => RustMemoryAuthor::Extraction,
        other => return Err(Error::from_reason(format!("invalid memory author {other:?}"))),
    };
    let trust = match record.provenance.trust.as_str() {
        "untrusted" => RustMemoryTrustLevel::Untrusted,
        "user_asserted" => RustMemoryTrustLevel::UserAsserted,
        "host_verified" => RustMemoryTrustLevel::HostVerified,
        other => return Err(Error::from_reason(format!("invalid memory trust {other:?}"))),
    };
    Ok(RustMemoryRecord {
        record_id: record.record_id,
        scope: RustMemoryScope::new(record.scope.tenant_id, record.scope.namespace),
        name: record.name,
        kind,
        content: record.content,
        description: record.description,
        provenance: RustMemoryProvenance {
            session_id: record.provenance.session_id,
            author,
            trust,
            evidence_refs: record.provenance.evidence_refs,
        },
        created_at: record.created_at as u64,
        updated_at: record.updated_at as u64,
        last_recalled_at: record.last_recalled_at.map(|value| value as u64),
        recall_count: record.recall_count as u64,
        confidence: record.confidence,
        links: record.links,
        pinned: record.pinned,
        ttl_days: record.ttl_days,
    })
}

fn memory_record_from_rust(record: &RustMemoryRecord) -> MemoryRecord {
    MemoryRecord {
        record_id: record.record_id.clone(),
        scope: MemoryScope {
            tenant_id: record.scope.tenant_id.clone(),
            namespace: record.scope.namespace.clone(),
        },
        name: record.name.clone(),
        kind: record.kind.label().into(),
        content: record.content.clone(),
        description: record.description.clone(),
        provenance: MemoryProvenance {
            session_id: record.provenance.session_id.clone(),
            author: match record.provenance.author {
                RustMemoryAuthor::Model => "model",
                RustMemoryAuthor::Host => "host",
                RustMemoryAuthor::Extraction => "extraction",
            }
            .into(),
            trust: match record.provenance.trust {
                RustMemoryTrustLevel::Untrusted => "untrusted",
                RustMemoryTrustLevel::UserAsserted => "user_asserted",
                RustMemoryTrustLevel::HostVerified => "host_verified",
            }
            .into(),
            evidence_refs: record.provenance.evidence_refs.clone(),
        },
        created_at: record.created_at as f64,
        updated_at: record.updated_at as f64,
        last_recalled_at: record.last_recalled_at.map(|value| value as f64),
        recall_count: record.recall_count as f64,
        confidence: record.confidence,
        links: record.links.clone(),
        pinned: record.pinned,
        ttl_days: record.ttl_days,
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
