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
use deepstrike_core::harness::eval_pipeline::{
    Criterion as RustCriterion, EvalAction as RustEvalAction, EvalEvent as RustEvalEvent,
    EvalPipeline as RustEvalPipeline, EvalPolicy as RustEvalPolicy, EvalResult as RustEvalResult,
};
use deepstrike_core::runtime::{
    KernelInput as RustKernelInput, KernelRuntime as RustKernelRuntime,
};
use deepstrike_core::scheduler::policy::LoopPolicy as RustLoopPolicy;
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
    /// `"orchestrate"` | `"implement"` (default) | `"retrieve"` | `"verify"`
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

    #[wasm_bindgen(js_name = turn)]
    pub fn turn(&self) -> u32 {
        self.inner.state_machine().turn
    }

    #[wasm_bindgen(js_name = recoveryContentBytes)]
    pub fn recovery_content_bytes(&self) -> u32 {
        let sm = self.inner.state_machine();
        let tokens = sm.ctx.config.recovery_content_tokens(sm.ctx.max_tokens);
        sm.ctx.engine.token_budget_to_bytes(tokens) as u32
    }

    #[wasm_bindgen(js_name = render)]
    pub fn render(&self) -> RenderedContext {
        rendered_context_from_rust(self.inner.state_machine().ctx.render())
    }

    #[wasm_bindgen(js_name = drainNewMessages)]
    pub fn drain_new_messages(&mut self) -> Vec<Message> {
        self.inner
            .state_machine_mut()
            .drain_new_messages()
            .iter()
            .map(message_from_rust)
            .collect()
    }

    #[wasm_bindgen(js_name = preservedRefs)]
    pub fn preserved_refs(&self) -> Vec<String> {
        self.inner
            .state_machine()
            .ctx
            .partitions
            .task_state
            .preserved_refs
            .clone()
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
