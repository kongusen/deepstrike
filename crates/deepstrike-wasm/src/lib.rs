use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use tsify_next::Tsify;
use wasm_bindgen::prelude::*;

use deepstrike_core::context::manager::ContextManager;
use deepstrike_core::context::pressure::PressureAction;
use deepstrike_core::context::renderer::RenderedContext as RustRenderedContext;
use deepstrike_core::governance::pipeline::GovernancePipeline as RustGovernancePipeline;
use deepstrike_core::harness::eval_pipeline::{
    Criterion as RustCriterion, EvalAction as RustEvalAction, EvalEvent as RustEvalEvent,
    EvalPipeline as RustEvalPipeline, EvalPolicy as RustEvalPolicy, EvalResult as RustEvalResult,
};
use deepstrike_core::runtime::{
    KernelInput as RustKernelInput, KernelRuntime as RustKernelRuntime,
};
use deepstrike_core::scheduler::policy::LoopPolicy as RustLoopPolicy;
use deepstrike_core::scheduler::state_machine::{
    LoopAction as RustLoopAction, LoopEvent as RustLoopEvent,
    LoopObservation as RustLoopObservation, LoopStateMachine as RustLoopStateMachine,
};
use deepstrike_core::signals::router::SignalRouter as RustSignalRouter;
use deepstrike_core::types::agent::AgentIdentity;
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
    /// `"orchestrate"` | `"implement"` (default) | `"retrieve"` | `"verify"`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
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
    pub timestamp_ms: f64,
}

fn runtime_signal_to_rust(s: RuntimeSignal) -> RustRuntimeSignal {
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
    sig
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
        timestamp_ms: s.timestamp_ms as f64,
    }
}

fn disposition_str(d: RustSignalDisposition) -> &'static str {
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

// ────────────────────────────── Tagged unions: LoopAction / LoopObservation ──────────────────────────────

/// Structured context for a provider call — emitted with `kind === "call_llm"`.
#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct RenderedContext {
    pub system_text: String,
    pub turns: Vec<Message>,
}

/// Discriminated union; inspect `kind`:
/// - `"call_llm"`           → `context`, `tools`
/// - `"execute_tools"`      → `calls`
/// - `"done"`               → `result`
/// - `"evaluate_milestone"` → `phase_id`, `criteria`
#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct LoopAction {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<RenderedContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<LoopResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criteria: Option<Vec<String>>,
}

/// Discriminated union for runtime observations:
/// - `"compressed"`         → `action`, `rhoAfter`, `summary`, `archived`
/// - `"renewed"`            → `sprint`
/// - `"rollbacked"`         → `turn`, `checkpointHistoryLen`
/// - `"capability_changed"` → `turn`, `added`, `removed`
/// - `"milestone_advanced"` → `turn`, `phaseId`, `capabilitiesUnlocked`
/// - `"milestone_blocked"`  → `turn`, `phaseId`, `milestoneReason`
#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct LoopObservation {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rho_after: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sprint: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<Vec<Message>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_history_len: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities_unlocked: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone_reason: Option<String>,
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
#[tsify(from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct EvalPipelineOptions {
    #[serde(default)]
    pub extract_skill_on_pass: bool,
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

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct EvalPipelineAction {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<CriterionResult>>,
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

fn role_str_to_rust(role: &str) -> Result<Role, JsValue> {
    match role {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(JsValue::from_str(&format!("invalid role: {other}"))),
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

fn message_to_rust(m: Message) -> Result<RustMessage, JsValue> {
    let role = role_str_to_rust(&m.role)?;
    let tool_calls: Vec<RustToolCall> = m
        .tool_calls
        .into_iter()
        .map(tool_call_to_rust)
        .collect::<Result<_, _>>()?;
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

fn tool_call_to_rust(c: ToolCall) -> Result<RustToolCall, JsValue> {
    let args: serde_json::Value = serde_json::from_str(&c.arguments)
        .map_err(|e| JsValue::from_str(&format!("invalid JSON arguments: {e}")))?;
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
        is_fatal: false,
        token_count: r.token_count,
    }
}

fn tool_schema_to_rust(t: ToolSchema) -> Result<RustToolSchema, JsValue> {
    let params: serde_json::Value = serde_json::from_str(&t.parameters)
        .map_err(|e| JsValue::from_str(&format!("invalid JSON parameters: {e}")))?;
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
        allowed_tools: s.allowed_tools.iter().map(CompactString::new).collect(),
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
        criteria: t.criteria,
        metadata: serde_json::Value::Null,
        lane: task_lane_to_rust(t.lane),
    }
}

fn policy_to_rust(p: LoopPolicy) -> RustLoopPolicy {
    RustLoopPolicy {
        max_tokens: p.max_tokens,
        max_turns: p.max_turns,
        max_total_tokens: p.max_total_tokens as u64,
        timeout_ms: p.timeout_ms.map(|x| x as u64),
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
        total_tokens_used: r.total_tokens_used as f64,
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
            phase_id: None,
            criteria: None,
        },
        RustLoopAction::ExecuteTools { calls } => LoopAction {
            kind: "execute_tools".into(),
            context: None,
            tools: None,
            calls: Some(calls.iter().map(tool_call_from_rust).collect()),
            result: None,
            phase_id: None,
            criteria: None,
        },
        RustLoopAction::Done { result } => LoopAction {
            kind: "done".into(),
            context: None,
            tools: None,
            calls: None,
            result: Some(loop_result_from_rust(&result)),
            phase_id: None,
            criteria: None,
        },
        RustLoopAction::EvaluateMilestone { phase_id, criteria } => LoopAction {
            kind: "evaluate_milestone".into(),
            context: None,
            tools: None,
            calls: None,
            result: None,
            phase_id: Some(phase_id),
            criteria: Some(criteria),
        },
    }
}

fn rendered_context_from_rust(rc: RustRenderedContext) -> RenderedContext {
    RenderedContext {
        system_text: rc.system_text,
        turns: rc.turns.iter().map(message_from_rust).collect(),
    }
}

fn observation_from_rust(o: RustLoopObservation) -> LoopObservation {
    match o {
        RustLoopObservation::Compressed { action, rho_after, summary, archived } => LoopObservation {
            kind: "compressed".into(),
            action: Some(pressure_action_str(action).into()),
            rho_after: Some(rho_after),
            sprint: None,
            summary,
            archived: Some(archived.iter().map(message_from_rust).collect()),
            turn: None,
            checkpoint_history_len: None,
            added: None,
            removed: None,
            phase_id: None,
            capabilities_unlocked: None,
            milestone_reason: None,
        },
        RustLoopObservation::Renewed { sprint } => LoopObservation {
            kind: "renewed".into(),
            action: None,
            rho_after: None,
            sprint: Some(sprint),
            summary: None,
            archived: None,
            turn: None,
            checkpoint_history_len: None,
            added: None,
            removed: None,
            phase_id: None,
            capabilities_unlocked: None,
            milestone_reason: None,
        },
        RustLoopObservation::Rollbacked { turn, checkpoint_history_len } => LoopObservation {
            kind: "rollbacked".into(),
            action: None,
            rho_after: None,
            sprint: None,
            summary: None,
            archived: None,
            turn: Some(turn),
            checkpoint_history_len: Some(checkpoint_history_len as u32),
            added: None,
            removed: None,
            phase_id: None,
            capabilities_unlocked: None,
            milestone_reason: None,
        },
        RustLoopObservation::CapabilityChanged { turn, added, removed } => LoopObservation {
            kind: "capability_changed".into(),
            action: None,
            rho_after: None,
            sprint: None,
            summary: None,
            archived: None,
            turn: Some(turn),
            checkpoint_history_len: None,
            added: Some(added),
            removed: Some(removed),
            phase_id: None,
            capabilities_unlocked: None,
            milestone_reason: None,
        },
        RustLoopObservation::MilestoneAdvanced { turn, phase_id, capabilities_unlocked } => LoopObservation {
            kind: "milestone_advanced".into(),
            action: None,
            rho_after: None,
            sprint: None,
            summary: None,
            archived: None,
            turn: Some(turn),
            checkpoint_history_len: None,
            added: None,
            removed: None,
            phase_id: Some(phase_id),
            capabilities_unlocked: Some(capabilities_unlocked),
            milestone_reason: None,
        },
        RustLoopObservation::MilestoneBlocked { turn, phase_id, reason } => LoopObservation {
            kind: "milestone_blocked".into(),
            action: None,
            rho_after: None,
            sprint: None,
            summary: None,
            archived: None,
            turn: Some(turn),
            checkpoint_history_len: None,
            added: None,
            removed: None,
            phase_id: Some(phase_id),
            capabilities_unlocked: None,
            milestone_reason: Some(reason),
        },
    }
}

fn eval_done_action(result: RustEvalResult) -> EvalPipelineAction {
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
                .map(|detail| CriterionResult {
                    criterion: detail.criterion,
                    passed: detail.passed,
                    score: detail.score as f64,
                    feedback: detail.feedback,
                })
                .collect(),
        ),
        skill_candidate: result.skill_candidate.map(|skill| SkillCandidate {
            name: skill.name,
            description: skill.description,
            when_to_use: skill.when_to_use,
            content: skill.content,
        }),
    }
}

fn eval_action_from_rust(action: RustEvalAction) -> EvalPipelineAction {
    match action {
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

// ────────────────────────────────────────────── KernelRuntime ──────────────────────────────────────────────

#[wasm_bindgen]
pub struct KernelRuntime {
    inner: RustKernelRuntime,
}

#[wasm_bindgen]
impl KernelRuntime {
    #[wasm_bindgen(constructor)]
    pub fn new(policy: LoopPolicy) -> Self {
        Self {
            inner: RustKernelRuntime::new(policy_to_rust(policy)),
        }
    }

    /// Feed a JSON-encoded KernelInput and return a JSON-encoded KernelStep.
    #[wasm_bindgen(js_name = step)]
    pub fn step(&mut self, input_json: String) -> Result<String, JsValue> {
        let input: RustKernelInput = serde_json::from_str(&input_json)
            .map_err(|e| JsValue::from_str(&format!("invalid KernelInput JSON: {e}")))?;
        serde_json::to_string(&self.inner.step(input))
            .map_err(|e| JsValue::from_str(&format!("failed to encode KernelStep: {e}")))
    }

    #[wasm_bindgen(js_name = isTerminal)]
    pub fn is_terminal(&self) -> bool {
        self.inner.is_terminal()
    }
}

// ────────────────────────────────────────────── DeepStrikeRuntime ──────────────────────────────────────────────

#[wasm_bindgen]
pub struct DeepStrikeRuntime {
    inner: RustLoopStateMachine,
}

#[wasm_bindgen]
impl DeepStrikeRuntime {
    #[wasm_bindgen(constructor)]
    pub fn new(policy: LoopPolicy) -> Self {
        Self {
            inner: RustLoopStateMachine::new(policy_to_rust(policy)),
        }
    }

    #[wasm_bindgen(js_name = setAvailableSkills)]
    pub fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.inner
            .ctx
            .set_available_skills(skills.into_iter().map(skill_metadata_to_rust).collect());
    }

    #[wasm_bindgen(js_name = setMemoryEnabled)]
    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_memory_enabled(enabled);
    }

    #[wasm_bindgen(js_name = setKnowledgeEnabled)]
    pub fn set_knowledge_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_knowledge_enabled(enabled);
    }

    /// Prepend a system-level instruction to the context. Must be called before `start`.
    /// `tokens` is a caller-supplied estimate (use `content.length / 4` if unsure).
    /// The renderer skips messages with `tokens == 0`, so always pass at least 1.
    #[wasm_bindgen(js_name = addSystemMessage)]
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
    #[wasm_bindgen(js_name = addMemoryMessage)]
    pub fn add_memory_message(&mut self, content: String, tokens: u32) {
        self.inner
            .ctx
            .partitions
            .memory
            .push(RustMessage::user(content), tokens.max(1));
    }

    /// Pre-populate the history partition with a prior transcript message.
    /// Must be called before `start`.
    #[wasm_bindgen(js_name = addHistoryMessage)]
    pub fn add_history_message(&mut self, message: Message, tokens: u32) -> Result<(), JsValue> {
        self.inner
            .ctx
            .push_history(message_to_rust(message)?, tokens.max(1));
        Ok(())
    }

    #[wasm_bindgen(js_name = setTools)]
    pub fn set_tools(&mut self, tools: Vec<ToolSchema>) -> Result<(), JsValue> {
        self.inner.tools = tools
            .into_iter()
            .map(tool_schema_to_rust)
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    #[wasm_bindgen]
    pub fn start(&mut self, task: RuntimeTask) -> LoopAction {
        loop_action_from_rust(self.inner.start(task_to_rust(task)))
    }

    #[wasm_bindgen(js_name = feedLlmResponse)]
    pub fn feed_llm_response(&mut self, message: Message) -> Result<LoopAction, JsValue> {
        let msg = message_to_rust(message)?;
        Ok(loop_action_from_rust(
            self.inner.feed(RustLoopEvent::LLMResponse { message: msg }),
        ))
    }

    #[wasm_bindgen(js_name = feedToolResults)]
    pub fn feed_tool_results(&mut self, results: Vec<ToolResult>) -> LoopAction {
        let results: Vec<RustToolResult> = results.into_iter().map(tool_result_to_rust).collect();
        loop_action_from_rust(self.inner.feed(RustLoopEvent::ToolResults { results }))
    }

    #[wasm_bindgen(js_name = feedTimeout)]
    pub fn feed_timeout(&mut self) -> LoopAction {
        loop_action_from_rust(self.inner.feed(RustLoopEvent::Timeout))
    }

    #[wasm_bindgen(js_name = isTerminal)]
    pub fn is_terminal(&self) -> bool {
        self.inner.is_terminal()
    }

    #[wasm_bindgen(getter)]
    pub fn turn(&self) -> u32 {
        self.inner.turn
    }

    #[wasm_bindgen]
    pub fn pressure(&self) -> f64 {
        self.inner.ctx.rho()
    }

    #[wasm_bindgen(js_name = preservedRefs)]
    pub fn preserved_refs(&self) -> Vec<String> {
        self.inner.ctx.partitions.task_state.preserved_refs.clone()
    }

    #[wasm_bindgen(js_name = forceCompact)]
    pub fn force_compact(&mut self) -> bool {
        self.inner.force_compact()
    }

    #[wasm_bindgen(js_name = takeObservations)]
    pub fn take_observations(&mut self) -> Vec<LoopObservation> {
        self.inner
            .take_observations()
            .into_iter()
            .map(observation_from_rust)
            .collect()
    }

    #[wasm_bindgen]
    pub fn render(&self) -> RenderedContext {
        rendered_context_from_rust(self.inner.ctx.render())
    }

    #[wasm_bindgen(js_name = initTask)]
    pub fn init_task(&mut self, goal: String, criteria: Vec<String>) {
        self.inner.ctx.init_task(goal, criteria);
    }

    #[wasm_bindgen(js_name = updateTask)]
    pub fn update_task(&mut self, update: TaskUpdate) {
        self.inner.ctx.update_task(task_update_to_rust(update));
    }

    #[wasm_bindgen(js_name = recoveryContentBytes)]
    pub fn recovery_content_bytes(&self) -> u32 {
        let tokens = self
            .inner
            .ctx
            .config
            .recovery_content_tokens(self.inner.ctx.max_tokens);
        self.inner.ctx.engine.token_budget_to_bytes(tokens) as u32
    }

    #[wasm_bindgen(js_name = setTokenizer)]
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

    #[wasm_bindgen(js_name = setPlanToolEnabled)]
    pub fn set_plan_tool_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_plan_tool_enabled(enabled);
    }

    #[wasm_bindgen(js_name = preloadHistory)]
    pub fn preload_history(&mut self, messages: Vec<Message>) -> Result<(), JsValue> {
        let rust_msgs: Vec<RustMessage> = messages
            .into_iter()
            .map(message_to_rust)
            .collect::<Result<_, _>>()?;
        self.inner.preload_history(rust_msgs);
        Ok(())
    }

    #[wasm_bindgen(js_name = resumeAfterPreload)]
    pub fn resume_after_preload(&mut self) -> LoopAction {
        loop_action_from_rust(self.inner.resume_after_preload())
    }

    #[wasm_bindgen(js_name = drainNewMessages)]
    pub fn drain_new_messages(&self) -> Vec<Message> {
        self.inner
            .drain_new_messages()
            .iter()
            .map(message_from_rust)
            .collect()
    }
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
    pub fn ingest(&mut self, signal: RuntimeSignal, is_running: bool) -> String {
        disposition_str(
            self.inner
                .ingest(runtime_signal_to_rust(signal), is_running),
        )
        .into()
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

// ────────────────────────────────────────────── EvalPipeline ──────────────────────────────────────────────

#[wasm_bindgen]
pub struct EvalPipeline {
    inner: RustEvalPipeline,
}

#[wasm_bindgen]
impl EvalPipeline {
    #[wasm_bindgen(constructor)]
    pub fn new(options: EvalPipelineOptions) -> Self {
        Self {
            inner: RustEvalPipeline::new(RustEvalPolicy {
                extract_skill_on_pass: options.extract_skill_on_pass,
            }),
        }
    }

    #[wasm_bindgen(js_name = feedOutcome)]
    pub fn feed_outcome(
        &mut self,
        goal: String,
        criteria: Vec<Criterion>,
        result: String,
        attempt: u32,
    ) -> EvalPipelineAction {
        let criteria = criteria
            .into_iter()
            .map(|criterion| RustCriterion {
                text: criterion.text,
                required: criterion.required,
                weight: criterion.weight.map(|weight| weight as f32).unwrap_or(1.0),
            })
            .collect();
        eval_action_from_rust(self.inner.feed(RustEvalEvent::Outcome {
            goal,
            criteria,
            result,
            attempt,
        }))
    }

    #[wasm_bindgen(js_name = feedEvalResult)]
    pub fn feed_eval_result(&mut self, content: String) -> EvalPipelineAction {
        eval_action_from_rust(self.inner.feed(RustEvalEvent::EvalResult { content }))
    }

    #[wasm_bindgen]
    pub fn reset(&mut self) {
        self.inner.reset();
    }

    #[wasm_bindgen(js_name = isIdle)]
    pub fn is_idle(&self) -> bool {
        self.inner.is_idle()
    }
}

// ────────────────────────────────────────────── Governance ──────────────────────────────────────────────

#[wasm_bindgen]
pub struct Governance {
    inner: RustGovernancePipeline,
}

#[wasm_bindgen]
impl Governance {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: RustGovernancePipeline::default(),
        }
    }

    #[wasm_bindgen(js_name = blockTool)]
    pub fn block_tool(&mut self, name: String) {
        self.inner.veto.block_tool(name);
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
        let caller = AgentIdentity::new("anonymous", "");
        governance_verdict_from_rust(self.inner.evaluate(&call, &caller))
    }
}
