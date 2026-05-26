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

use napi::bindgen_prelude::*;
use napi_derive::napi;

use compact_str::CompactString;

use deepstrike_core::context::manager::ContextManager;
use deepstrike_core::context::pressure::PressureAction;
use deepstrike_core::context::renderer::RenderedContext as RustRenderedContext;
use deepstrike_core::context::renewal::{
    ContractCheckResult as RustContractCheckResult, HandoffArtifact as RustHandoffArtifact,
};
use deepstrike_core::governance::constraint::{ConstraintRule, ParamConstraint};
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline as RustGovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::harness::eval_pipeline::{
    Criterion as RustCriterion, EvalAction as RustEvalAction, EvalEvent as RustEvalEvent,
    EvalPipeline as RustEvalPipeline, EvalPolicy as RustEvalPolicy,
};
use deepstrike_core::memory::curator::CurationResult as RustCurationResult;
use deepstrike_core::memory::durable::SessionData as RustSessionData;
use deepstrike_core::memory::idle_pipeline::{
    IdleAction as RustIdleAction, IdleEvent as RustIdleEvent, IdlePipeline as RustIdlePipeline,
    IdlePolicy as RustIdlePolicy,
};
use deepstrike_core::memory::semantic::MemoryEntry as RustMemoryEntry;
use deepstrike_core::scheduler::policy::LoopPolicy as RustLoopPolicy;
use deepstrike_core::scheduler::state_machine::{
    LoopAction as RustLoopAction, LoopEvent as RustLoopEvent,
    LoopObservation as RustLoopObservation, LoopStateMachine as RustLoopStateMachine,
};
use deepstrike_core::signals::router::SignalRouter as RustSignalRouter;
use deepstrike_core::types::agent::AgentIdentity;
use deepstrike_core::types::contract::{
    AcceptanceCriterion as RustAcceptanceCriterion,
    VerificationContract as RustVerificationContract,
};
use deepstrike_core::types::message::{
    Content, ContentPart, Message as RustMessage, Role, ToolCall as RustToolCall,
    ToolResult as RustToolResult, ToolSchema as RustToolSchema,
};
use deepstrike_core::types::policy::GovernanceVerdict as RustGovernanceVerdict;
use deepstrike_core::types::policy::SignalDisposition as RustSignalDisposition;
use deepstrike_core::types::result::LoopResult as RustLoopResult;
use deepstrike_core::types::signal::{
    RuntimeSignal as RustRuntimeSignal, SignalSource as RustSignalSource,
    SignalType as RustSignalType, Urgency as RustUrgency,
};
use deepstrike_core::types::skill::SkillMetadata as RustSkillMetadata;
use deepstrike_core::types::task::{RuntimeTask as RustRuntimeTask, TaskLane as RustTaskLane};

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
    /// `"orchestrate"` | `"implement"` (default) | `"retrieve"` | `"verify"`
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
pub struct ContractCheckResult {
    pub criterion_id: String,
    pub passed: bool,
    pub evidence: Option<String>,
}

#[napi(object)]
#[derive(Clone)]
pub struct HandoffArtifact {
    pub goal: String,
    pub sprint: u32,
    pub progress_summary: String,
    pub open_tasks: Vec<String>,
    /// JSON-encoded context snapshot.
    pub context_snapshot: String,
    pub contract_status: Vec<ContractCheckResult>,
    pub drift_rate_24h: f64,
    pub blocked_on: Vec<String>,
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

#[napi(object)]
#[derive(Clone)]
pub struct TaskUpdate {
    pub plan: Option<Vec<String>>,
    pub current_step: Option<u32>,
    pub progress: Option<String>,
    pub scratchpad: Option<String>,
    pub blocked_on: Option<Vec<String>>,
    pub preserved_refs: Option<Vec<String>>,
}

fn task_update_to_rust(u: TaskUpdate) -> deepstrike_core::context::task_state::TaskUpdate {
    deepstrike_core::context::task_state::TaskUpdate {
        plan: u.plan,
        current_step: u.current_step.map(|s| s as usize),
        progress: u.progress,
        scratchpad: u.scratchpad,
        blocked_on: u.blocked_on,
        preserved_refs: u.preserved_refs,
    }
}

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
    pub timestamp_ms: f64,
}

fn runtime_signal_to_rust(s: RuntimeSignal) -> Result<RustRuntimeSignal> {
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
    if let Some(key) = s.dedupe_key {
        sig = sig.with_dedupe(key.as_str());
    }
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
        timestamp_ms: s.timestamp_ms as f64,
    }
}

fn disposition_to_str(d: RustSignalDisposition) -> &'static str {
    match d {
        RustSignalDisposition::Ignore => "ignore",
        RustSignalDisposition::Observe => "observe",
        RustSignalDisposition::Queue => "queue",
        RustSignalDisposition::Run { .. } => "run",
        RustSignalDisposition::Interrupt => "interrupt",
        RustSignalDisposition::InterruptNow => "interrupt_now",
        RustSignalDisposition::Dropped => "dropped",
    }
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

// ────────────────────────────── Tagged unions: LoopAction / LoopObservation ──────────────────────────────

/// Structured context for a provider call — emitted with `kind === "call_llm"`.
/// Separates system configuration from the conversation transcript so providers
/// can map each field to their own API contract without role-filtering.
#[napi(object)]
#[derive(Clone)]
pub struct RenderedContext {
    /// Combined system text: system partition + dashboard (when non-empty).
    /// Anthropic → `system` param · OpenAI → `messages[0]` system role ·
    /// Gemini → `systemInstruction`.
    pub system_text: String,
    /// Strictly alternating user / assistant / tool turns.
    /// Working-partition signals are already folded into the first user turn.
    pub turns: Vec<Message>,
}

/// Discriminated union. Inspect `kind`:
/// - `"call_llm"`      → `context` (RenderedContext), `tools`
/// - `"execute_tools"` → `calls`
/// - `"done"`          → `result`
#[napi(object)]
#[derive(Clone)]
pub struct LoopAction {
    pub kind: String,
    pub context: Option<RenderedContext>,
    pub tools: Option<Vec<ToolSchema>>,
    pub calls: Option<Vec<ToolCall>>,
    pub result: Option<LoopResult>,
}

/// Discriminated union for observations:
/// - `"compressed"` → `action`, `rho_after`
/// - `"renewed"`    → `sprint`
#[napi(object)]
#[derive(Clone)]
pub struct LoopObservation {
    pub kind: String,
    pub action: Option<String>,
    pub rho_after: Option<f64>,
    /// Sprint number after renewal. Set when `kind === "renewed"`.
    pub sprint: Option<u32>,
    pub summary: Option<String>,
    pub archived: Option<Vec<Message>>,
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

fn tool_result_to_rust(r: ToolResult) -> RustToolResult {
    RustToolResult {
        call_id: CompactString::new(&r.call_id),
        output: Content::Text(r.output),
        is_error: r.is_error,
        token_count: r.token_count,
    }
}

fn tool_schema_to_rust(t: ToolSchema) -> Result<RustToolSchema> {
    let params: serde_json::Value = serde_json::from_str(&t.parameters)
        .map_err(|e| Error::new(Status::InvalidArg, format!("invalid JSON parameters: {e}")))?;
    Ok(RustToolSchema {
        name: CompactString::new(&t.name),
        description: t.description,
        parameters: params,
    })
}

fn tool_schema_from_rust(t: &RustToolSchema) -> ToolSchema {
    ToolSchema {
        name: t.name.to_string(),
        description: t.description.clone(),
        parameters: serde_json::to_string(&t.parameters).unwrap_or_else(|_| "null".into()),
    }
}

fn skill_metadata_to_rust(s: SkillMetadata) -> RustSkillMetadata {
    RustSkillMetadata {
        name: CompactString::new(&s.name),
        description: s.description,
        when_to_use: s.when_to_use,
        allowed_tools: s
            .allowed_tools
            .unwrap_or_default()
            .iter()
            .map(CompactString::new)
            .collect(),
        effort: s.effort,
        estimated_tokens: s.estimated_tokens,
    }
}

fn task_lane_to_rust(lane: Option<String>) -> RustTaskLane {
    match lane.as_deref() {
        Some("orchestrate") => RustTaskLane::Orchestrate,
        Some("retrieve") => RustTaskLane::Retrieve,
        Some("verify") => RustTaskLane::Verify,
        _ => RustTaskLane::Implement,
    }
}

fn task_to_rust(t: RuntimeTask) -> RustRuntimeTask {
    RustRuntimeTask {
        goal: t.goal,
        criteria: t.criteria.unwrap_or_default(),
        metadata: serde_json::Value::Null,
        lane: task_lane_to_rust(t.lane),
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

fn acceptance_criterion_from_rust(c: &RustAcceptanceCriterion) -> AcceptanceCriterion {
    AcceptanceCriterion {
        id: c.id.clone(),
        text: c.text.clone(),
        required: c.required,
        weight: c.weight as f64,
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

fn contract_check_result_from_rust(r: &RustContractCheckResult) -> ContractCheckResult {
    ContractCheckResult {
        criterion_id: r.criterion_id.clone(),
        passed: r.passed,
        evidence: r.evidence.clone(),
    }
}

fn handoff_artifact_from_rust(a: &RustHandoffArtifact) -> HandoffArtifact {
    HandoffArtifact {
        goal: a.goal.clone(),
        sprint: a.sprint,
        progress_summary: a.progress_summary.clone(),
        open_tasks: a.open_tasks.clone(),
        context_snapshot: serde_json::to_string(&a.context_snapshot)
            .unwrap_or_else(|_| "null".into()),
        contract_status: a
            .contract_status
            .iter()
            .map(contract_check_result_from_rust)
            .collect(),
        drift_rate_24h: a.drift_rate_24h,
        blocked_on: a.blocked_on.clone(),
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
        timeout_ms: p.timeout_ms.map(|b| b.get_u64().1),
    }
}

fn pressure_action_str(a: PressureAction) -> &'static str {
    match a {
        PressureAction::None => "none",
        PressureAction::SnipCompact => "snip_compact",
        PressureAction::MicroCompact => "micro_compact",
        PressureAction::ContextCollapse => "context_collapse",
        PressureAction::AutoCompact => "auto_compact",
    }
}

fn loop_result_from_rust(r: &RustLoopResult) -> LoopResult {
    let termination = match r.termination {
        deepstrike_core::types::result::TerminationReason::Completed => "completed",
        deepstrike_core::types::result::TerminationReason::MaxTurns => "max_turns",
        deepstrike_core::types::result::TerminationReason::TokenBudget => "token_budget",
        deepstrike_core::types::result::TerminationReason::Timeout => "timeout",
        deepstrike_core::types::result::TerminationReason::UserAbort => "user_abort",
        deepstrike_core::types::result::TerminationReason::Error => "error",
    };
    LoopResult {
        termination: termination.to_string(),
        final_message: r.final_message.as_ref().map(message_from_rust),
        turns_used: r.turns_used,
        total_tokens_used: BigInt::from(r.total_tokens_used),
    }
}

fn rendered_context_from_rust(rc: RustRenderedContext) -> RenderedContext {
    RenderedContext {
        system_text: rc.system_text,
        turns: rc.turns.iter().map(message_from_rust).collect(),
    }
}

fn loop_action_from_rust(a: RustLoopAction) -> LoopAction {
    match a {
        RustLoopAction::CallLLM { context, tools } => LoopAction {
            kind: "call_llm".into(),
            context: Some(rendered_context_from_rust(context)),
            tools: Some(tools.iter().map(tool_schema_from_rust).collect()),
            calls: None,
            result: None,
        },
        RustLoopAction::ExecuteTools { calls } => LoopAction {
            kind: "execute_tools".into(),
            context: None,
            tools: None,
            calls: Some(calls.iter().map(tool_call_from_rust).collect()),
            result: None,
        },
        RustLoopAction::Done { result } => LoopAction {
            kind: "done".into(),
            context: None,
            tools: None,
            calls: None,
            result: Some(loop_result_from_rust(&result)),
        },
    }
}

fn observation_from_rust(o: RustLoopObservation) -> LoopObservation {
    match o {
        RustLoopObservation::Compressed {
            action,
            rho_after,
            summary,
            archived,
        } => LoopObservation {
            kind: "compressed".into(),
            action: Some(pressure_action_str(action).into()),
            rho_after: Some(rho_after),
            sprint: None,
            summary,
            archived: Some(archived.iter().map(message_from_rust).collect()),
        },
        RustLoopObservation::Renewed { sprint } => LoopObservation {
            kind: "renewed".into(),
            action: None,
            rho_after: None,
            sprint: Some(sprint),
            summary: None,
            archived: None,
        },
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

// ─────────────────────────────────────────── ContextEngine ───────────────────────────────────────────

#[napi]
pub struct ContextEngine {
    inner: ContextManager,
}

#[napi]
impl ContextEngine {
    #[napi(constructor)]
    pub fn new(max_tokens: u32) -> Self {
        Self {
            inner: ContextManager::new(max_tokens),
        }
    }

    #[napi]
    pub fn add_system_message(&mut self, content: String, tokens: u32) {
        self.inner
            .partitions
            .system
            .push(RustMessage::system(content), tokens);
    }

    #[napi]
    pub fn add_user_message(&mut self, content: String, tokens: u32) {
        self.inner.push_history(RustMessage::user(content), tokens);
    }

    #[napi]
    pub fn add_assistant_message(&mut self, content: String, tokens: u32) {
        self.inner
            .push_history(RustMessage::assistant(content), tokens);
    }

    #[napi]
    pub fn pressure(&self) -> f64 {
        self.inner.rho()
    }

    #[napi]
    pub fn total_tokens(&self) -> u32 {
        self.inner.partitions.total_tokens(&self.inner.engine)
    }

    /// Run compression at the level the current pressure recommends.
    /// Returns tokens saved.
    #[napi]
    pub fn compress(&mut self) -> u32 {
        let action = self.inner.should_compress();
        if action == PressureAction::None {
            return 0;
        }
        let before = self.inner.partitions.total_tokens(&self.inner.engine);
        self.inner.compress(action);
        let after = self.inner.partitions.total_tokens(&self.inner.engine);
        before.saturating_sub(after)
    }

    #[napi]
    pub fn render(&self) -> RenderedContext {
        rendered_context_from_rust(self.inner.render())
    }

    /// Replace the available-skills set with frontmatter-only metadata.
    /// The kernel will auto-inject the `skill` meta-tool into every `CallLLM` action.
    #[napi]
    pub fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        let rust_skills = skills.into_iter().map(skill_metadata_to_rust).collect();
        self.inner.set_available_skills(rust_skills);
    }

    #[napi]
    pub fn init_task(&mut self, goal: String, criteria: Vec<String>) {
        self.inner.init_task(goal, criteria);
    }

    #[napi]
    pub fn update_task(&mut self, update: TaskUpdate) {
        self.inner.update_task(task_update_to_rust(update));
    }

    #[napi]
    pub fn recovery_content_bytes(&self) -> u32 {
        let tokens = self
            .inner
            .config
            .recovery_content_tokens(self.inner.max_tokens);
        self.inner.engine.token_budget_to_bytes(tokens) as u32
    }

    #[napi]
    pub fn set_tokenizer(&mut self, name: String) {
        let engine = match name.as_str() {
            "tiktoken_cl100k" | "cl100k" => {
                deepstrike_core::context::token_engine::ContextTokenEngine::cl100k()
            }
            "tiktoken_o200k" | "o200k" => {
                deepstrike_core::context::token_engine::ContextTokenEngine::o200k()
            }
            _ => deepstrike_core::context::token_engine::ContextTokenEngine::char_approx(),
        };
        self.inner.engine = engine;
    }

    #[napi]
    pub fn set_plan_tool_enabled(&mut self, enabled: bool) {
        self.inner.set_plan_tool_enabled(enabled);
    }
}

// ─────────────────────────────────────────── LoopStateMachine ───────────────────────────────────────────

#[napi]
pub struct LoopStateMachine {
    inner: RustLoopStateMachine,
}

#[napi]
impl LoopStateMachine {
    #[napi(constructor)]
    pub fn new(policy: LoopPolicy) -> Self {
        Self {
            inner: RustLoopStateMachine::new(policy_to_rust(policy)),
        }
    }

    #[napi]
    pub fn force_compact(&mut self) -> bool {
        self.inner.force_compact()
    }

    /// Convenience: register skills directly on the state machine without
    /// reaching into the inner ContextEngine.
    #[napi]
    pub fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        let rust_skills = skills.into_iter().map(skill_metadata_to_rust).collect();
        self.inner.ctx.set_available_skills(rust_skills);
    }

    /// Enable the `memory` meta-tool. Call with `true` when a DreamStore and agentId
    /// are configured — the SDK layer intercepts `memory` tool calls and runs the search.
    #[napi]
    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_memory_enabled(enabled);
    }

    /// Enable the `knowledge` meta-tool. Call with `true` when a KnowledgeSource
    /// is configured — the SDK layer intercepts `knowledge` tool calls and runs retrieval.
    #[napi]
    pub fn set_knowledge_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_knowledge_enabled(enabled);
    }

    /// Prepend a system-level instruction to the context. Must be called before `start`.
    /// `tokens` is a caller-supplied estimate (use `content.length / 4` if unsure).
    /// The renderer skips messages with `tokens == 0`, so always pass at least 1.
    #[napi]
    pub fn add_system_message(&mut self, content: String, tokens: u32) {
        self.inner
            .ctx
            .partitions
            .system
            .push(RustMessage::system(content), tokens.max(1));
    }

    /// Pre-populate the memory partition with a long-term memory snippet.
    /// Must be called before `start`. Use for seeding known context from past sessions.
    /// `tokens` is a caller-supplied estimate; pass at least 1.
    #[napi]
    pub fn add_memory_message(&mut self, content: String, tokens: u32) {
        self.inner
            .ctx
            .partitions
            .memory
            .push(RustMessage::user(content), tokens.max(1));
    }

    /// Pre-populate the history partition with a prior transcript message.
    /// Must be called before `start`.
    #[napi]
    pub fn add_history_message(&mut self, message: Message, tokens: u32) -> Result<()> {
        self.inner
            .ctx
            .push_history(message_to_rust(message)?, tokens.max(1));
        Ok(())
    }

    /// Inject a VerificationContract into the system partition.
    /// The contract is formatted as markdown and pushed to the `system` partition
    /// (Priority::Critical) so it survives context renewal and compression.
    /// Call before `start()`.
    #[napi]
    pub fn set_contract(&mut self, contract: VerificationContract) {
        let rust_contract = verification_contract_to_rust(contract);
        let formatted = rust_contract.format_for_system_prompt();
        let tokens = (formatted.len() / 4).max(1) as u32;
        self.inner
            .ctx
            .partitions
            .system
            .push(RustMessage::system(formatted), tokens);
    }

    #[napi]
    pub fn set_tools(&mut self, tools: Vec<ToolSchema>) -> Result<()> {
        let rust_tools: Vec<RustToolSchema> = tools
            .into_iter()
            .map(tool_schema_to_rust)
            .collect::<Result<_>>()?;
        self.inner.tools = rust_tools;
        Ok(())
    }

    #[napi]
    pub fn start(&mut self, task: RuntimeTask) -> LoopAction {
        loop_action_from_rust(self.inner.start(task_to_rust(task)))
    }

    #[napi]
    pub fn feed_llm_response(&mut self, message: Message) -> Result<LoopAction> {
        let msg = message_to_rust(message)?;
        Ok(loop_action_from_rust(
            self.inner.feed(RustLoopEvent::LLMResponse { message: msg }),
        ))
    }

    #[napi]
    pub fn feed_tool_results(&mut self, results: Vec<ToolResult>) -> LoopAction {
        let results: Vec<RustToolResult> = results.into_iter().map(tool_result_to_rust).collect();
        loop_action_from_rust(self.inner.feed(RustLoopEvent::ToolResults { results }))
    }

    #[napi]
    pub fn feed_timeout(&mut self) -> LoopAction {
        loop_action_from_rust(self.inner.feed(RustLoopEvent::Timeout))
    }

    #[napi]
    pub fn is_terminal(&self) -> bool {
        self.inner.is_terminal()
    }

    #[napi]
    pub fn turn(&self) -> u32 {
        self.inner.turn
    }

    #[napi]
    pub fn pressure(&self) -> f64 {
        self.inner.ctx.rho()
    }

    /// Drain observations emitted during the most recent feed call.
    #[napi]
    pub fn take_observations(&mut self) -> Vec<LoopObservation> {
        self.inner
            .take_observations()
            .into_iter()
            .map(observation_from_rust)
            .collect()
    }

    /// Pre-populate history with messages from a prior session.
    /// Call before `start()` to restore conversational continuity across runs.
    #[napi]
    pub fn preload_history(&mut self, messages: Vec<Message>) -> Result<()> {
        let rust_msgs: Vec<RustMessage> = messages
            .into_iter()
            .map(message_to_rust)
            .collect::<Result<_>>()?;
        self.inner.preload_history(rust_msgs);
        Ok(())
    }

    /// Continue from preloaded history without a new user turn (mid-run recovery).
    #[napi(js_name = "resumeAfterPreload")]
    pub fn resume_after_preload(&mut self) -> LoopAction {
        loop_action_from_rust(self.inner.resume_after_preload())
    }

    /// Return only messages added during the current run (since the last `preload_history`
    /// or construction). Use this to persist the session delta to `SessionStore.saveSession`.
    #[napi]
    pub fn drain_new_messages(&self) -> Vec<Message> {
        self.inner
            .drain_new_messages()
            .iter()
            .map(message_from_rust)
            .collect()
    }

    #[napi]
    pub fn render(&self) -> RenderedContext {
        rendered_context_from_rust(self.inner.ctx.render())
    }

    #[napi]
    pub fn init_task(&mut self, goal: String, criteria: Vec<String>) {
        self.inner.ctx.init_task(goal, criteria);
    }

    #[napi]
    pub fn update_task(&mut self, update: TaskUpdate) {
        self.inner.ctx.update_task(task_update_to_rust(update));
    }

    #[napi]
    pub fn recovery_content_bytes(&self) -> u32 {
        let tokens = self
            .inner
            .ctx
            .config
            .recovery_content_tokens(self.inner.ctx.max_tokens);
        self.inner.ctx.engine.token_budget_to_bytes(tokens) as u32
    }

    #[napi]
    pub fn set_tokenizer(&mut self, name: String) {
        let engine = match name.as_str() {
            "tiktoken_cl100k" | "cl100k" => {
                deepstrike_core::context::token_engine::ContextTokenEngine::cl100k()
            }
            "tiktoken_o200k" | "o200k" => {
                deepstrike_core::context::token_engine::ContextTokenEngine::o200k()
            }
            _ => deepstrike_core::context::token_engine::ContextTokenEngine::char_approx(),
        };
        self.inner.ctx.engine = engine;
    }

    #[napi]
    pub fn set_plan_tool_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_plan_tool_enabled(enabled);
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
    pub fn ingest(&mut self, signal: RuntimeSignal, is_running: bool) -> Result<String> {
        let rust_sig = runtime_signal_to_rust(signal)?;
        Ok(disposition_to_str(self.inner.ingest(rust_sig, is_running)).into())
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

/// A single session of agent messages, used as input to `IdlePipeline.feedTrigger`.
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

/// A long-term memory entry as stored by the agent.
#[napi(object)]
#[derive(Clone)]
pub struct MemoryEntry {
    pub text: String,
    pub score: f64,
    /// JSON-encoded metadata blob.
    pub metadata: String,
}

#[napi(object)]
#[derive(Clone)]
pub struct CurationStats {
    pub insights_processed: u32,
    pub duplicates_removed: u32,
    pub conflicts_resolved: u32,
    pub entries_added: u32,
}

/// The delta the `DreamStore.commit` must apply: add `toAdd`, remove `toRemoveIndices`.
#[napi(object)]
#[derive(Clone)]
pub struct CurationResult {
    pub to_add: Vec<MemoryEntry>,
    /// Indices into the `existingMemories` slice passed to `feedTrigger`.
    pub to_remove_indices: Vec<u32>,
    pub stats: CurationStats,
}

#[napi(object)]
#[derive(Clone)]
pub struct IdleRunResult {
    pub sessions_processed: u32,
    pub insights_extracted: u32,
}

/// Discriminated union returned by `IdlePipeline` methods. Inspect `kind`:
/// - `"synthesize_insights"` → `messages` (SDK must call LLM, then `feedSynthesisResult`)
/// - `"commit_memories"`     → `agentId`, `curationResult`, `runResult`
/// - `"noop"` | `"aborted"`
#[napi(object)]
#[derive(Clone)]
pub struct IdlePipelineAction {
    pub kind: String,
    pub messages: Option<Vec<Message>>,
    pub agent_id: Option<String>,
    pub curation_result: Option<CurationResult>,
    pub run_result: Option<IdleRunResult>,
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

fn memory_entry_to_rust(e: MemoryEntry) -> RustMemoryEntry {
    let metadata: serde_json::Value =
        serde_json::from_str(&e.metadata).unwrap_or(serde_json::Value::Null);
    RustMemoryEntry {
        text: e.text,
        score: e.score,
        metadata,
    }
}

fn memory_entry_from_rust(e: &RustMemoryEntry) -> MemoryEntry {
    MemoryEntry {
        text: e.text.clone(),
        score: e.score,
        metadata: serde_json::to_string(&e.metadata).unwrap_or_else(|_| "null".into()),
    }
}

fn curation_result_from_rust(r: RustCurationResult) -> CurationResult {
    CurationResult {
        to_add: r.to_add.iter().map(memory_entry_from_rust).collect(),
        to_remove_indices: r.to_remove_indices.iter().map(|&i| i as u32).collect(),
        stats: CurationStats {
            insights_processed: r.stats.insights_processed as u32,
            duplicates_removed: r.stats.duplicates_removed as u32,
            conflicts_resolved: r.stats.conflicts_resolved as u32,
            entries_added: r.stats.entries_added as u32,
        },
    }
}

fn idle_pipeline_action_from_rust(a: RustIdleAction) -> IdlePipelineAction {
    match a {
        RustIdleAction::SynthesizeInsights { messages } => IdlePipelineAction {
            kind: "synthesize_insights".into(),
            messages: Some(messages.iter().map(message_from_rust).collect()),
            agent_id: None,
            curation_result: None,
            run_result: None,
        },
        RustIdleAction::CommitMemories {
            agent_id,
            result,
            run_result,
        } => IdlePipelineAction {
            kind: "commit_memories".into(),
            messages: None,
            agent_id: Some(agent_id),
            curation_result: Some(curation_result_from_rust(result)),
            run_result: Some(IdleRunResult {
                sessions_processed: run_result.sessions_processed as u32,
                insights_extracted: run_result.insights_extracted as u32,
            }),
        },
        RustIdleAction::Noop => IdlePipelineAction {
            kind: "noop".into(),
            messages: None,
            agent_id: None,
            curation_result: None,
            run_result: None,
        },
        RustIdleAction::Aborted => IdlePipelineAction {
            kind: "aborted".into(),
            messages: None,
            agent_id: None,
            curation_result: None,
            run_result: None,
        },
    }
}

// ─────────────────────────────────────────── EvalPipeline ────────────────────────────────────────

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
pub struct EvalPipelineOptions {
    pub extract_skill_on_pass: Option<bool>,
}

#[napi(object)]
#[derive(Clone)]
pub struct SkillCandidate {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub content: String,
}

/// Discriminated union returned by `EvalPipeline` methods. Inspect `kind`:
/// - `"evaluate"` → `messages` (SDK must call evaluator LLM, then `feedEvalResult`)
/// - `"done"`     → `passed`, `overallScore`, `feedback`, `details`, optional `skillCandidate`
#[napi(object)]
#[derive(Clone)]
pub struct EvalPipelineAction {
    pub kind: String,
    pub messages: Option<Vec<Message>>,
    pub passed: Option<bool>,
    pub overall_score: Option<f64>,
    pub feedback: Option<String>,
    pub details: Option<Vec<CriterionResult>>,
    pub skill_candidate: Option<SkillCandidate>,
}

/// Kernel state machine for the evaluation cycle.
///
/// Drive it like this:
/// 1. `feedOutcome(goal, criteria, result, attempt)` → `"evaluate"` action
/// 2. Call evaluator LLM with `action.messages`, collect the text response
/// 3. `feedEvalResult(text)` → `"done"` action
/// 4. Read `action.passed` / `action.feedback` / `action.skillCandidate`
/// 5. Call `reset()` before the next attempt
#[napi]
pub struct EvalPipeline {
    inner: RustEvalPipeline,
}

#[napi]
impl EvalPipeline {
    #[napi(constructor)]
    pub fn new(options: Option<EvalPipelineOptions>) -> Self {
        let policy = RustEvalPolicy {
            extract_skill_on_pass: options
                .and_then(|o| o.extract_skill_on_pass)
                .unwrap_or(true),
        };
        Self {
            inner: RustEvalPipeline::new(policy),
        }
    }

    /// Phase 1 — provide the goal, criteria, agent output, and attempt number.
    /// Returns an `"evaluate"` action with messages to send to the evaluator LLM.
    #[napi]
    pub fn feed_outcome(
        &mut self,
        goal: String,
        criteria: Vec<Criterion>,
        result: String,
        attempt: u32,
    ) -> EvalPipelineAction {
        let rust_criteria = criteria
            .into_iter()
            .map(|c| RustCriterion {
                text: c.text,
                required: c.required,
                weight: c.weight.map(|w| w as f32).unwrap_or(1.0),
            })
            .collect();
        match self.inner.feed(RustEvalEvent::Outcome {
            goal,
            criteria: rust_criteria,
            result,
            attempt,
        }) {
            RustEvalAction::Evaluate { messages } => EvalPipelineAction {
                kind: "evaluate".into(),
                messages: Some(messages.iter().map(message_from_rust).collect()),
                passed: None,
                overall_score: None,
                feedback: None,
                details: None,
                skill_candidate: None,
            },
            RustEvalAction::Done { result } => eval_done_action(result),
        }
    }

    /// Phase 2 — feed back the evaluator LLM's text response.
    #[napi]
    pub fn feed_eval_result(&mut self, content: String) -> EvalPipelineAction {
        match self.inner.feed(RustEvalEvent::EvalResult { content }) {
            RustEvalAction::Done { result } => eval_done_action(result),
            RustEvalAction::Evaluate { messages } => EvalPipelineAction {
                kind: "evaluate".into(),
                messages: Some(messages.iter().map(message_from_rust).collect()),
                passed: None,
                overall_score: None,
                feedback: None,
                details: None,
                skill_candidate: None,
            },
        }
    }

    #[napi]
    pub fn reset(&mut self) {
        self.inner.reset();
    }

    #[napi]
    pub fn is_idle(&self) -> bool {
        self.inner.is_idle()
    }
}

fn eval_done_action(
    result: deepstrike_core::harness::eval_pipeline::EvalResult,
) -> EvalPipelineAction {
    EvalPipelineAction {
        kind: "done".into(),
        messages: None,
        passed: Some(result.passed),
        overall_score: Some(result.overall_score as f64),
        feedback: Some(result.feedback),
        details: Some(
            result
                .details
                .into_iter()
                .map(|d| CriterionResult {
                    criterion: d.criterion,
                    passed: d.passed,
                    score: d.score as f64,
                    feedback: d.feedback,
                })
                .collect(),
        ),
        skill_candidate: result.skill_candidate.map(|s| SkillCandidate {
            name: s.name,
            description: s.description,
            when_to_use: s.when_to_use,
            content: s.content,
        }),
    }
}

/// Kernel state machine for the idle dreaming cycle.
///
/// Drive it like this:
/// 1. `feedTrigger(sessions, existingMemories, nowMs)` → `"synthesize_insights"` action
/// 2. Call LLM with `action.messages`, collect the text response
/// 3. `feedSynthesisResult(text)` → `"commit_memories"` action
/// 4. Apply `action.curationResult` via `DreamStore.commit`, then call `reset()`
#[napi]
pub struct IdlePipeline {
    inner: RustIdlePipeline,
}

#[napi]
impl IdlePipeline {
    #[napi(constructor)]
    pub fn new(agent_id: String) -> Self {
        Self {
            inner: RustIdlePipeline::new(RustIdlePolicy::new(agent_id)),
        }
    }

    /// Phase 1 — provide sessions + current memory snapshot; kernel builds the LLM prompt.
    #[napi]
    pub fn feed_trigger(
        &mut self,
        sessions: Vec<SessionData>,
        existing_memories: Vec<MemoryEntry>,
        now_ms: f64,
    ) -> Result<IdlePipelineAction> {
        let rust_sessions: Vec<RustSessionData> = sessions
            .into_iter()
            .map(session_data_to_rust)
            .collect::<Result<_>>()?;
        let rust_memories: Vec<RustMemoryEntry> = existing_memories
            .into_iter()
            .map(memory_entry_to_rust)
            .collect();
        let action = self.inner.feed(RustIdleEvent::Trigger {
            sessions: rust_sessions,
            existing_memories: rust_memories,
            now_ms: now_ms as u64,
        });
        Ok(idle_pipeline_action_from_rust(action))
    }

    /// Phase 2 — feed back the LLM's synthesis text; kernel parses and curates.
    #[napi]
    pub fn feed_synthesis_result(&mut self, content: String) -> IdlePipelineAction {
        idle_pipeline_action_from_rust(self.inner.feed(RustIdleEvent::SynthesisResult { content }))
    }

    #[napi]
    pub fn is_idle(&self) -> bool {
        self.inner.is_idle()
    }

    /// Reset to `Idle` after handling `CommitMemories` to allow the next cycle.
    #[napi]
    pub fn reset(&mut self) {
        self.inner.reset();
    }
}
