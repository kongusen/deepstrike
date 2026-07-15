#![allow(deprecated)]

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use tsify_next::Tsify;
use wasm_bindgen::prelude::*;

use deepstrike_core::context::renderer::RenderedContext as RustRenderedContext;
use deepstrike_core::governance::constraint::{ConstraintRule, ParamConstraint};
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline as RustGovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::harness::eval::{
    build_eval_messages as rust_build_eval_messages, parse_verdict as rust_parse_verdict,
    verdict_output_schema as rust_verdict_output_schema, Criterion as RustCriterion,
};
use deepstrike_core::memory::curator::CurationResult as RustCurationResult;
use deepstrike_core::memory::durable::SessionData as RustSessionData;
use deepstrike_core::memory::idle_pipeline::{
    IdleAction as RustIdleAction, IdleEvent as RustIdleEvent, IdlePipeline as RustIdlePipeline,
    IdlePolicy as RustIdlePolicy,
};
use deepstrike_core::memory::semantic::MemoryEntry as RustMemoryEntry;
use deepstrike_core::runtime::KernelRuntime as RustKernelRuntime;
use deepstrike_core::scheduler::policy::SchedulerBudget as RustLoopPolicy;
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
    /// Optional pub/sub topic (carried through; routing deferred).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
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
    if let Some(recipient) = s.recipient {
        sig = sig.with_recipient(recipient.as_str());
    }
    if let Some(topic) = s.topic {
        sig = sig.with_topic(topic.as_str());
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
        recipient: s.recipient.as_ref().map(|r| r.to_string()),
        topic: s.topic.as_ref().map(|t| t.to_string()),
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

fn policy_to_rust(p: LoopPolicy) -> RustLoopPolicy {
    RustLoopPolicy {
        max_tokens: p.max_tokens,
        max_turns: p.max_turns,
        max_total_tokens: p.max_total_tokens as u64,
        max_wall_ms: p.timeout_ms.map(|x| x as u64),
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
        let step = self.inner.step_json(&input_json)
            .map_err(|e| JsValue::from_str(&format!("invalid KernelInput JSON: {e}")))?;
        serde_json::to_string(&step)
            .map_err(|e| JsValue::from_str(&format!("failed to encode KernelStep: {e}")))
    }

    #[wasm_bindgen(js_name = prepareStep)]
    pub fn prepare_step(&mut self, input_json: String) -> Result<String, JsValue> {
        let prepared = self
            .inner
            .prepare_step_json(&input_json)
            .map_err(|e| JsValue::from_str(&format!("invalid KernelInput JSON: {e}")))?;
        serde_json::to_string(&prepared)
            .map_err(|e| JsValue::from_str(&format!("failed to encode KernelPreparedStep: {e}")))
    }

    #[wasm_bindgen(js_name = commitPrepared)]
    pub fn commit_prepared(&mut self, prepare_token: String) -> Result<String, JsValue> {
        let step = self
            .inner
            .commit_prepared(&prepare_token)
            .map_err(|fault| {
                JsValue::from_str(&serde_json::to_string(&fault).unwrap_or(fault.message))
            })?;
        serde_json::to_string(&step)
            .map_err(|e| JsValue::from_str(&format!("failed to encode KernelStep: {e}")))
    }

    #[wasm_bindgen(js_name = abortPrepared)]
    pub fn abort_prepared(&mut self, prepare_token: String) -> Result<(), JsValue> {
        self.inner.abort_prepared(&prepare_token).map_err(|fault| {
            JsValue::from_str(&serde_json::to_string(&fault).unwrap_or(fault.message))
        })
    }

    #[wasm_bindgen(js_name = snapshot)]
    pub fn snapshot(&self) -> Result<String, JsValue> {
        self.inner.snapshot_json().map_err(|fault| {
            JsValue::from_str(&serde_json::to_string(&fault).unwrap_or(fault.message))
        })
    }

    #[wasm_bindgen(js_name = restore)]
    pub fn restore(&mut self, snapshot_json: String) -> Result<(), JsValue> {
        self.inner = RustKernelRuntime::restore_snapshot_json(&snapshot_json).map_err(|fault| {
            JsValue::from_str(&serde_json::to_string(&fault).unwrap_or(fault.message))
        })?;
        Ok(())
    }

    /// Return a read-only JSON resource projection without mutating kernel state.
    #[wasm_bindgen(js_name = diagnostics)]
    pub fn diagnostics(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.inner.diagnostics()).map_err(|error| {
            JsValue::from_str(&format!("failed to encode kernel diagnostics: {error}"))
        })
    }

    #[wasm_bindgen(js_name = isTerminal)]
    pub fn is_terminal(&self) -> bool {
        self.inner.is_terminal()
    }

    #[wasm_bindgen(js_name = turn)]
    pub fn turn(&self) -> u32 {
        self.inner.turn()
    }

    #[wasm_bindgen(js_name = recoveryContentBytes)]
    pub fn recovery_content_bytes(&self) -> u32 {
        self.inner.recovery_content_bytes() as u32
    }

    #[wasm_bindgen(js_name = render)]
    pub fn render(&self) -> RenderedContext {
        rendered_context_from_rust(self.inner.render())
    }

    #[wasm_bindgen(js_name = drainNewMessages)]
    pub fn drain_new_messages(&mut self) -> Vec<Message> {
        self.inner.drain_new_messages()
            .iter()
            .map(message_from_rust)
            .collect()
    }

    #[wasm_bindgen(js_name = preservedRefs)]
    pub fn preserved_refs(&self) -> Vec<String> {
        self.inner.preserved_refs()
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

// ────────────────────────────────────────────── Eval primitives ──────────────────────────────────────────────
// The generate→evaluate quality gate's stateless compute (0.5.0 fold of the former `EvalPipeline`
// class, OS-axis #6). The SDK `HarnessLoop` drives the loop; these expose the kernel's prompt
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
    rust_build_eval_messages(&goal, &rust_criteria, &result, attempt, extract_skill_on_pass)
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


// ────────────────────────────── Dream / idle consolidation pipeline ──────────────────────────────
// Parity with the napi/pyo3 IdlePipeline (the wasm SDK's Agent.dream() drives this; the ambient
// wasm-kernel.d.ts already promised this class).

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
pub struct MemoryEntry {
    pub text: String,
    pub score: f64,
    /// JSON-encoded metadata blob.
    pub metadata: String,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct CurationStats {
    pub insights_processed: u32,
    pub duplicates_removed: u32,
    pub conflicts_resolved: u32,
    pub entries_added: u32,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct CurationResult {
    pub to_add: Vec<MemoryEntry>,
    /// Indices into the `existingMemories` slice passed to `feedTrigger`.
    pub to_remove_indices: Vec<u32>,
    pub stats: CurationStats,
}

#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct IdleRunResult {
    pub sessions_processed: u32,
    pub insights_extracted: u32,
}

/// Discriminated union returned by `IdlePipeline` methods. Inspect `kind`:
/// - `"synthesize_insights"` → `messages` (SDK must call the LLM, then `feedSynthesisResult`)
/// - `"commit_memories"`     → `agentId`, `curationResult`, `runResult`
/// - `"noop"` | `"aborted"`
#[derive(Tsify, Clone, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase")]
pub struct IdlePipelineAction {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curation_result: Option<CurationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_result: Option<IdleRunResult>,
}

fn role_str_to_rust(role: &str) -> Result<Role, JsValue> {
    match role {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(JsValue::from_str(&format!("invalid role: {other}"))),
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

fn message_to_rust(m: Message) -> Result<RustMessage, JsValue> {
    let role = role_str_to_rust(&m.role)?;
    let tool_calls: Vec<RustToolCall> = m
        .tool_calls
        .into_iter()
        .map(|c| {
            let args: serde_json::Value =
                serde_json::from_str(&c.arguments).unwrap_or(serde_json::Value::Null);
            RustToolCall {
                id: CompactString::new(&c.id),
                name: CompactString::new(&c.name),
                arguments: args,
            }
        })
        .collect();
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

fn session_data_to_rust(s: SessionData) -> Result<RustSessionData, JsValue> {
    let messages: Vec<RustMessage> = s
        .messages
        .into_iter()
        .map(message_to_rust)
        .collect::<Result<_, _>>()?;
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
    RustMemoryEntry { text: e.text, score: e.score, metadata }
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
        RustIdleAction::CommitMemories { agent_id, result, run_result } => IdlePipelineAction {
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

/// Two-phase offline dream pipeline (parity with the napi/pyo3 exports):
/// 1. `feedTrigger(sessions, existingMemories, nowMs)` → `"synthesize_insights"` + prompt messages
/// 2. Call the LLM with those messages, collect the text response
/// 3. `feedSynthesisResult(text)` → `"commit_memories"` with the curation delta
#[wasm_bindgen]
pub struct IdlePipeline {
    inner: RustIdlePipeline,
}

#[wasm_bindgen]
impl IdlePipeline {
    #[wasm_bindgen(constructor)]
    pub fn new(agent_id: String) -> Self {
        Self {
            inner: RustIdlePipeline::new(RustIdlePolicy::new(agent_id)),
        }
    }

    #[wasm_bindgen(js_name = feedTrigger)]
    pub fn feed_trigger(
        &mut self,
        sessions: Vec<SessionData>,
        existing_memories: Vec<MemoryEntry>,
        now_ms: f64,
    ) -> Result<IdlePipelineAction, JsValue> {
        let rust_sessions: Vec<RustSessionData> = sessions
            .into_iter()
            .map(session_data_to_rust)
            .collect::<Result<_, _>>()?;
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

    #[wasm_bindgen(js_name = feedSynthesisResult)]
    pub fn feed_synthesis_result(&mut self, content: String) -> IdlePipelineAction {
        idle_pipeline_action_from_rust(self.inner.feed(RustIdleEvent::SynthesisResult { content }))
    }
}
