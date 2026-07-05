//! # DeepStrike Python Bindings
//!
//! PyO3 bindings exposing the Rust kernel to Python.
//! Build with: `maturin develop` or `maturin build --release`
//!
//! ## High-level API
//!
//! ```python
//! from deepstrike._kernel import (
//!     LoopStateMachine, RuntimeTask, LoopPolicy, SkillMetadata,
//! )
//!
//! sm = LoopStateMachine(LoopPolicy(max_tokens=128_000))
//! # Register skills; kernel auto-injects the `skill` meta-tool into every CallLLM.
//! sm.set_available_skills([
//!     SkillMetadata(name="debug", description="Debug helper"),
//! ])
//!
//! action = sm.start(RuntimeTask("Fix the bug"))
//! while not sm.is_terminal():
//!     if action.kind == "call_llm":
//!         # tools list already includes the `skill` meta-tool
//!         msg = call_llm(action.context, action.tools)
//!         action = sm.feed_llm_response(msg)
//!     elif action.kind == "execute_tools":
//!         # SDK intercepts calls where name == "skill" and reads the file
//!         results = exec_tools(action.calls)
//!         action = sm.feed_tool_results(results)
//!     elif action.kind == "done":
//!         break
//! ```

#![allow(deprecated)]

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use compact_str::CompactString;

use deepstrike_core::context::renderer::RenderedContext as RustRenderedContext;
use deepstrike_core::governance::constraint::{ConstraintRule, ParamConstraint};
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline as RustGovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::harness::eval::{
    build_eval_messages as rust_build_eval_messages, parse_verdict as rust_parse_verdict,
    verdict_output_schema as rust_verdict_output_schema, Criterion as RustCriterion,
    SkillCandidate as RustSkillCandidate,
};
use deepstrike_core::memory::curator::CurationResult as RustCurationResult;
use deepstrike_core::memory::durable::SessionData as RustSessionData;
use deepstrike_core::memory::idle_pipeline::{
    IdleAction as RustIdleAction, IdleEvent as RustIdleEvent, IdlePipeline as RustIdlePipeline,
    IdlePolicy as RustIdlePolicy,
};
use deepstrike_core::memory::semantic::MemoryEntry as RustMemoryEntry;
use deepstrike_core::runtime::{
    KernelInput as RustKernelInput, KernelRuntime as RustKernelRuntime,
};
use deepstrike_core::scheduler::policy::SchedulerBudget as RustLoopPolicy;
use deepstrike_core::signals::router::SignalRouter as RustSignalRouter;
use deepstrike_core::types::agent::AgentIdentity;
use deepstrike_core::types::message::{
    Content, ContentPart, Message as RustMessage, Role, ToolCall as RustToolCall,
};
use deepstrike_core::types::policy::{
    GovernanceVerdict as RustGovernanceVerdict, SignalDisposition as RustSignalDisposition,
};
use deepstrike_core::types::signal::{
    RuntimeSignal as RustRuntimeSignal, SignalSource as RustSignalSource,
    SignalType as RustSignalType, Urgency as RustUrgency,
};

// ───────────────────────────────────────── Signal types ──────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct RuntimeSignal {
    #[pyo3(get, set)]
    id: String,
    #[pyo3(get, set)]
    source: String,
    #[pyo3(get, set)]
    signal_type: String,
    #[pyo3(get, set)]
    urgency: String,
    #[pyo3(get, set)]
    summary: String,
    #[pyo3(get, set)]
    payload: String,
    #[pyo3(get, set)]
    dedupe_key: Option<String>,
    #[pyo3(get, set)]
    recipient: Option<String>,
    #[pyo3(get, set)]
    topic: Option<String>,
    #[pyo3(get, set)]
    timestamp_ms: f64,
}

#[pymethods]
impl RuntimeSignal {
    #[new]
    #[pyo3(signature = (source, urgency, summary, signal_type="event", payload="null", dedupe_key=None, timestamp_ms=0.0, recipient=None, topic=None))]
    fn new(
        source: String,
        urgency: String,
        summary: String,
        signal_type: &str,
        payload: &str,
        dedupe_key: Option<String>,
        timestamp_ms: f64,
        recipient: Option<String>,
        topic: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            source,
            urgency,
            summary,
            signal_type: signal_type.into(),
            payload: payload.into(),
            dedupe_key,
            recipient,
            topic,
            timestamp_ms,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "RuntimeSignal(urgency={:?}, summary={:?})",
            self.urgency, self.summary
        )
    }
}

impl RuntimeSignal {
    fn to_rust(&self) -> RustRuntimeSignal {
        let source = match self.source.as_str() {
            "cron" => RustSignalSource::Cron,
            "gateway" => RustSignalSource::Gateway,
            "heartbeat" => RustSignalSource::Heartbeat,
            _ => RustSignalSource::Custom,
        };
        let signal_type = match self.signal_type.as_str() {
            "job" => RustSignalType::Job,
            "alert" => RustSignalType::Alert,
            _ => RustSignalType::Event,
        };
        let urgency = match self.urgency.as_str() {
            "critical" => RustUrgency::Critical,
            "high" => RustUrgency::High,
            "low" => RustUrgency::Low,
            _ => RustUrgency::Normal,
        };
        let payload: serde_json::Value =
            serde_json::from_str(&self.payload).unwrap_or(serde_json::Value::Null);
        let mut sig = RustRuntimeSignal::new(source, signal_type, urgency, self.summary.as_str())
            .with_payload(payload)
            .with_timestamp(self.timestamp_ms as u64);
        if let Some(ref key) = self.dedupe_key {
            sig = sig.with_dedupe(key.as_str());
        }
        if let Some(ref recipient) = self.recipient {
            sig = sig.with_recipient(recipient.as_str());
        }
        if let Some(ref topic) = self.topic {
            sig = sig.with_topic(topic.as_str());
        }
        sig
    }

    fn from_rust(s: &RustRuntimeSignal) -> Self {
        Self {
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

// ───────────────────────────────────────── POD types ─────────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct ContentPartObj {
    #[pyo3(get, set)]
    r#type: String,
    #[pyo3(get, set)]
    text: Option<String>,
    #[pyo3(get, set)]
    url: Option<String>,
    #[pyo3(get, set)]
    data: Option<String>,
    #[pyo3(get, set)]
    media_type: Option<String>,
    #[pyo3(get, set)]
    detail: Option<String>,
    #[pyo3(get, set)]
    call_id: Option<String>,
    #[pyo3(get, set)]
    output: Option<String>,
    #[pyo3(get, set)]
    is_error: Option<bool>,
}

#[pymethods]
impl ContentPartObj {
    #[new]
    #[pyo3(signature = (r#type, text=None, url=None, data=None, media_type=None, detail=None, call_id=None, output=None, is_error=None))]
    fn new(
        r#type: String,
        text: Option<String>,
        url: Option<String>,
        data: Option<String>,
        media_type: Option<String>,
        detail: Option<String>,
        call_id: Option<String>,
        output: Option<String>,
        is_error: Option<bool>,
    ) -> Self {
        Self {
            r#type,
            text,
            url,
            data,
            media_type,
            detail,
            call_id,
            output,
            is_error,
        }
    }

    #[staticmethod]
    fn text_part(text: String) -> Self {
        Self {
            r#type: "text".into(),
            text: Some(text),
            url: None,
            data: None,
            media_type: None,
            detail: None,
            call_id: None,
            output: None,
            is_error: None,
        }
    }

    #[staticmethod]
    #[pyo3(signature = (url, detail=None))]
    fn image_url(url: String, detail: Option<String>) -> Self {
        Self {
            r#type: "image".into(),
            text: None,
            url: Some(url),
            data: None,
            media_type: None,
            detail,
            call_id: None,
            output: None,
            is_error: None,
        }
    }

    #[staticmethod]
    #[pyo3(signature = (data, media_type, detail=None))]
    fn image_base64(data: String, media_type: String, detail: Option<String>) -> Self {
        Self {
            r#type: "image".into(),
            text: None,
            url: None,
            data: Some(data),
            media_type: Some(media_type),
            detail,
            call_id: None,
            output: None,
            is_error: None,
        }
    }

    #[staticmethod]
    fn audio(data: String, media_type: String) -> Self {
        Self {
            r#type: "audio".into(),
            text: None,
            url: None,
            data: Some(data),
            media_type: Some(media_type),
            detail: None,
            call_id: None,
            output: None,
            is_error: None,
        }
    }

    fn __repr__(&self) -> String {
        format!("ContentPart(type={:?})", self.r#type)
    }
}

#[pyclass]
#[derive(Clone)]
struct Message {
    #[pyo3(get, set)]
    role: String,
    #[pyo3(get, set)]
    content: String,
    #[pyo3(get, set)]
    content_parts: Option<Vec<ContentPartObj>>,
    #[pyo3(get, set)]
    token_count: Option<u32>,
    #[pyo3(get)]
    tool_calls: Vec<ToolCall>,
}

#[pymethods]
impl Message {
    #[new]
    #[pyo3(signature = (role, content, token_count = None, tool_calls = None, content_parts = None))]
    fn new(
        role: String,
        content: String,
        token_count: Option<u32>,
        tool_calls: Option<Vec<ToolCall>>,
        content_parts: Option<Vec<ContentPartObj>>,
    ) -> Self {
        Self {
            role,
            content,
            content_parts,
            token_count,
            tool_calls: tool_calls.unwrap_or_default(),
        }
    }

    fn __repr__(&self) -> String {
        let parts_info = match &self.content_parts {
            Some(p) => format!(", parts={}", p.len()),
            None => String::new(),
        };
        format!(
            "Message(role={:?}, content={:?}, tokens={:?}{})",
            self.role, self.content, self.token_count, parts_info
        )
    }
}

fn content_part_obj_to_rust(p: &ContentPartObj) -> ContentPart {
    match p.r#type.as_str() {
        "image" => ContentPart::Image {
            url: p.url.clone(),
            data: p.data.clone(),
            media_type: p.media_type.clone(),
            detail: p.detail.clone(),
        },
        "audio" => ContentPart::Audio {
            data: p.data.clone().unwrap_or_default(),
            media_type: p.media_type.clone().unwrap_or_else(|| "audio/wav".into()),
        },
        "tool_result" => ContentPart::ToolResult {
            call_id: CompactString::new(p.call_id.as_deref().unwrap_or("")),
            output: p.output.clone().unwrap_or_default(),
            is_error: p.is_error.unwrap_or(false),
        },
        _ => ContentPart::Text {
            text: p.text.clone().unwrap_or_default(),
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

impl Message {
    fn to_rust(&self) -> Result<RustMessage, PyErr> {
        let role = match self.role.as_str() {
            "system" => Role::System,
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            other => return Err(PyValueError::new_err(format!("invalid role: {other}"))),
        };
        let content = match &self.content_parts {
            Some(parts) if !parts.is_empty() => {
                Content::Parts(parts.iter().map(content_part_obj_to_rust).collect())
            }
            _ => Content::Text(self.content.clone()),
        };
        Ok(RustMessage {
            role,
            content,
            tool_calls: self
                .tool_calls
                .iter()
                .map(|c| c.to_rust())
                .collect::<Result<_, _>>()?,
            token_count: self.token_count,
        })
    }

    fn from_rust(msg: &RustMessage) -> Self {
        let role = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let (content, content_parts) = match &msg.content {
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
        Self {
            role: role.to_string(),
            content,
            content_parts,
            token_count: msg.token_count,
            tool_calls: msg.tool_calls.iter().map(ToolCall::from_rust).collect(),
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct ToolCall {
    #[pyo3(get, set)]
    id: String,
    #[pyo3(get, set)]
    name: String,
    /// Arguments encoded as a JSON string. Python: `json.dumps(args)`.
    #[pyo3(get, set)]
    arguments: String,
}

#[pymethods]
impl ToolCall {
    #[new]
    fn new(id: String, name: String, arguments: String) -> Self {
        Self {
            id,
            name,
            arguments,
        }
    }

    fn __repr__(&self) -> String {
        format!("ToolCall(id={:?}, name={:?})", self.id, self.name)
    }
}

impl ToolCall {
    fn to_rust(&self) -> Result<RustToolCall, PyErr> {
        let args: serde_json::Value = serde_json::from_str(&self.arguments)
            .map_err(|e| PyValueError::new_err(format!("invalid JSON arguments: {e}")))?;
        Ok(RustToolCall {
            id: CompactString::new(&self.id),
            name: CompactString::new(&self.name),
            arguments: args,
        })
    }

    fn from_rust(c: &RustToolCall) -> Self {
        Self {
            id: c.id.to_string(),
            name: c.name.to_string(),
            arguments: serde_json::to_string(&c.arguments).unwrap_or_else(|_| "null".into()),
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct ToolResult {
    #[pyo3(get, set)]
    call_id: String,
    #[pyo3(get, set)]
    output: String,
    #[pyo3(get, set)]
    is_error: bool,
    #[pyo3(get, set)]
    is_fatal: bool,
    #[pyo3(get, set)]
    error_kind: Option<String>,
    #[pyo3(get, set)]
    token_count: Option<u32>,
}

#[pymethods]
impl ToolResult {
    #[new]
    #[pyo3(signature = (call_id, output, is_error = false, token_count = None, is_fatal = false, error_kind = None))]
    fn new(
        call_id: String,
        output: String,
        is_error: bool,
        token_count: Option<u32>,
        is_fatal: bool,
        error_kind: Option<String>,
    ) -> Self {
        Self {
            call_id,
            output,
            is_error,
            is_fatal,
            error_kind,
            token_count,
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct ToolSchema {
    #[pyo3(get, set)]
    name: String,
    #[pyo3(get, set)]
    description: String,
    /// JSON-encoded JSON Schema. Python: `json.dumps(schema)`.
    #[pyo3(get, set)]
    parameters: String,
}

#[pymethods]
impl ToolSchema {
    #[new]
    fn new(name: String, description: String, parameters: String) -> Self {
        Self {
            name,
            description,
            parameters,
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct RuntimeTask {
    #[pyo3(get, set)]
    goal: String,
    #[pyo3(get, set)]
    criteria: Vec<String>,
    /// `"orchestrate"` | `"implement"` (default) | `"retrieve"` | `"verify"`
    #[pyo3(get, set)]
    lane: Option<String>,
}

#[pymethods]
impl RuntimeTask {
    #[new]
    #[pyo3(signature = (goal, criteria = None, lane = None))]
    fn new(goal: String, criteria: Option<Vec<String>>, lane: Option<String>) -> Self {
        Self {
            goal,
            criteria: criteria.unwrap_or_default(),
            lane,
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct LoopPolicy {
    #[pyo3(get, set)]
    max_tokens: u32,
    #[pyo3(get, set)]
    max_turns: u32,
    #[pyo3(get, set)]
    max_total_tokens: u64,
    #[pyo3(get, set)]
    timeout_ms: Option<u64>,
}

#[pymethods]
impl LoopPolicy {
    #[new]
    #[pyo3(signature = (max_tokens = 128_000, max_turns = 25, max_total_tokens = 1_000_000, timeout_ms = None))]
    fn new(
        max_tokens: u32,
        max_turns: u32,
        max_total_tokens: u64,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            max_tokens,
            max_turns,
            max_total_tokens,
            timeout_ms,
        }
    }
}

impl LoopPolicy {
    fn to_rust(&self) -> RustLoopPolicy {
        RustLoopPolicy {
            max_tokens: self.max_tokens,
            max_turns: self.max_turns,
            max_total_tokens: self.max_total_tokens,
            max_wall_ms: self.timeout_ms,
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct LoopResult {
    #[pyo3(get)]
    termination: String,
    #[pyo3(get)]
    final_message: Option<Message>,
    #[pyo3(get)]
    turns_used: u32,
    #[pyo3(get)]
    total_tokens_used: u64,
}

#[pyclass]
#[derive(Clone, Default)]
pub struct TaskUpdate {
    #[pyo3(get, set)]
    pub plan: Option<Vec<String>>,
    #[pyo3(get, set)]
    pub current_step: Option<u32>,
    #[pyo3(get, set)]
    pub progress: Option<String>,
    #[pyo3(get, set)]
    pub scratchpad: Option<String>,
    #[pyo3(get, set)]
    pub blocked_on: Option<Vec<String>>,
    #[pyo3(get, set)]
    pub preserved_refs: Option<Vec<String>>,
}

#[pymethods]
impl TaskUpdate {
    #[new]
    #[pyo3(signature = (plan = None, current_step = None, progress = None, scratchpad = None, blocked_on = None, preserved_refs = None))]
    fn new(
        plan: Option<Vec<String>>,
        current_step: Option<u32>,
        progress: Option<String>,
        scratchpad: Option<String>,
        blocked_on: Option<Vec<String>>,
        preserved_refs: Option<Vec<String>>,
    ) -> Self {
        Self {
            plan,
            current_step,
            progress,
            scratchpad,
            blocked_on,
            preserved_refs,
        }
    }
}

// ───────────────────────────────────────── Skill types ─────────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct SkillMetadata {
    #[pyo3(get, set)]
    name: String,
    #[pyo3(get, set)]
    description: String,
    #[pyo3(get, set)]
    when_to_use: Option<String>,
    #[pyo3(get, set)]
    allowed_tools: Vec<String>,
    #[pyo3(get, set)]
    effort: Option<u8>,
    #[pyo3(get, set)]
    estimated_tokens: u32,
}

#[pymethods]
impl SkillMetadata {
    #[new]
    #[pyo3(signature = (name, description = String::new(), when_to_use = None, allowed_tools = None, effort = None, estimated_tokens = 0))]
    fn new(
        name: String,
        description: String,
        when_to_use: Option<String>,
        allowed_tools: Option<Vec<String>>,
        effort: Option<u8>,
        estimated_tokens: u32,
    ) -> Self {
        Self {
            name,
            description,
            when_to_use,
            allowed_tools: allowed_tools.unwrap_or_default(),
            effort,
            estimated_tokens,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "SkillMetadata(name={:?}, effort={:?}, est_tokens={})",
            self.name, self.effort, self.estimated_tokens
        )
    }
}

// ─────────────────────────────── Provider context ────────────────────────

/// Structured context for a provider call — present when `kind == "call_llm"`.
#[pyclass]
#[derive(Clone)]
struct RenderedContext {
    #[pyo3(get)]
    system_text: String,
    #[pyo3(get)]
    system_stable: String,
    #[pyo3(get)]
    system_knowledge: String,
    /// History turns only — the stable, cacheable message prefix.
    #[pyo3(get)]
    turns: Vec<Message>,
    /// Volatile State turn (task_state + signals), rendered after the cacheable history.
    #[pyo3(get)]
    state_turn: Option<Message>,
    /// P1-E: count of leading `turns` forming the frozen prefix (byte-stable until the next
    /// compaction). Providers pin a deep cache breakpoint here; absent ⇒ rolling-pair fallback.
    #[pyo3(get)]
    frozen_prefix_len: Option<usize>,
}

#[pymethods]
impl RenderedContext {
    fn __repr__(&self) -> String {
        format!("RenderedContext(turns={})", self.turns.len())
    }
}

impl RenderedContext {
    fn from_rust(rc: RustRenderedContext) -> Self {
        Self {
            system_text: rc.system_text,
            system_stable: rc.system_stable,
            system_knowledge: rc.system_knowledge,
            turns: rc.turns.iter().map(Message::from_rust).collect(),
            state_turn: rc.state_turn.as_ref().map(Message::from_rust),
            frozen_prefix_len: rc.frozen_prefix_len,
        }
    }
}

// ──────────────────────────────────────── KernelRuntime ────────────────────────────────────

#[pyclass]
struct KernelRuntime {
    inner: RustKernelRuntime,
}

#[pymethods]
impl KernelRuntime {
    #[new]
    fn new(policy: LoopPolicy) -> Self {
        Self {
            inner: RustKernelRuntime::new(policy.to_rust()),
        }
    }

    /// Feed a JSON-encoded KernelInput and return a JSON-encoded KernelStep.
    fn step(&mut self, input_json: String) -> PyResult<String> {
        let input: RustKernelInput = serde_json::from_str(&input_json)
            .map_err(|e| PyValueError::new_err(format!("invalid KernelInput JSON: {e}")))?;
        serde_json::to_string(&self.inner.step(input))
            .map_err(|e| PyValueError::new_err(format!("failed to encode KernelStep: {e}")))
    }

    fn is_terminal(&self) -> bool {
        self.inner.is_terminal()
    }

    fn turn(&self) -> u32 {
        self.inner.state_machine().turn
    }

    /// L1 (RunGroup): cumulative sub-agent spawns this run, for charging the group ledger at run end.
    fn local_subagents_spawned(&self) -> u32 {
        self.inner.local_subagents_spawned()
    }

    fn recovery_content_bytes(&self) -> u32 {
        let sm = self.inner.state_machine();
        let tokens = sm.ctx.config.recovery_content_tokens(sm.ctx.max_tokens);
        sm.ctx.engine.token_budget_to_bytes(tokens) as u32
    }

    fn render(&self) -> RenderedContext {
        RenderedContext::from_rust(self.inner.state_machine().ctx.render())
    }

    fn drain_new_messages(&mut self) -> Vec<Message> {
        self.inner
            .state_machine_mut()
            .drain_new_messages()
            .iter()
            .map(Message::from_rust)
            .collect()
    }

    fn preserved_refs(&self) -> Vec<String> {
        self.inner
            .state_machine()
            .ctx
            .partitions
            .task_state
            .preserved_refs
            .clone()
    }
}

// ──────────────────────────────────────── SignalRouter (passthrough) ───────────────────────────

#[pyclass]
struct SignalRouter {
    inner: RustSignalRouter,
}

#[pymethods]
impl SignalRouter {
    #[new]
    fn new(max_queue_size: usize) -> Self {
        Self {
            inner: RustSignalRouter::new(max_queue_size),
        }
    }

    /// Ingest a signal. Returns disposition string:
    /// "ignore" | "observe" | "queue" | "run" | "interrupt" | "interrupt_now" | "dropped"
    fn ingest(&mut self, signal: RuntimeSignal, is_running: bool) -> &'static str {
        disposition_str(self.inner.ingest(signal.to_rust(), is_running))
    }

    /// Pull the next queued signal (highest priority first).
    fn next(&mut self) -> Option<RuntimeSignal> {
        self.inner.next().as_ref().map(RuntimeSignal::from_rust)
    }

    /// Pull the next queued signal visible to `recipient` (broadcasts plus signals
    /// addressed to it); other recipients' signals stay queued. None ⇒ no filter.
    #[pyo3(signature = (recipient=None))]
    fn next_for(&mut self, recipient: Option<String>) -> Option<RuntimeSignal> {
        self.inner
            .next_for(recipient.as_deref())
            .as_ref()
            .map(RuntimeSignal::from_rust)
    }

    fn depth(&self) -> usize {
        self.inner.depth()
    }

    fn clear_dedup(&mut self) {
        self.inner.clear_dedup();
    }
}

// ──────────────────────────────────────── Governance ────────────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct GovernanceVerdict {
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    reason: Option<String>,
    #[pyo3(get)]
    retry_after_ms: Option<f64>,
}

#[pymethods]
impl GovernanceVerdict {
    fn __repr__(&self) -> String {
        format!("GovernanceVerdict(kind={:?})", self.kind)
    }
}

fn governance_verdict_from_rust(v: RustGovernanceVerdict) -> GovernanceVerdict {
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

#[pyclass]
struct Governance {
    inner: RustGovernancePipeline,
    agent_id: String,
    session_id: String,
}

#[pymethods]
impl Governance {
    #[new]
    #[pyo3(signature = (default_action = "allow"))]
    fn new(default_action: &str) -> Self {
        let action = match default_action {
            "deny" => PermissionAction::Deny,
            "ask_user" => PermissionAction::AskUser,
            _ => PermissionAction::Allow,
        };
        Self {
            inner: RustGovernancePipeline::new(action),
            agent_id: "anonymous".into(),
            session_id: "".into(),
        }
    }

    fn set_identity(&mut self, agent_id: String, session_id: String) {
        self.agent_id = agent_id;
        self.session_id = session_id;
    }

    fn add_permission_rule(&mut self, pattern: String, action: String) {
        let action = match action.as_str() {
            "deny" => PermissionAction::Deny,
            "ask_user" => PermissionAction::AskUser,
            _ => PermissionAction::Allow,
        };
        self.inner.permission.add_rule(PermissionRule {
            tool_pattern: pattern.into(),
            action,
        });
    }

    fn block_tool(&mut self, name: String) {
        self.inner.veto.block_tool(name);
    }

    fn set_rate_limit(&mut self, tool_name: String, max_calls: u32, window_ms: u64) {
        self.inner.rate_limiter.set_limit(
            tool_name,
            RateLimit {
                max_calls,
                window_ms,
            },
        );
    }

    fn require_param(&mut self, tool_name: String, param_path: String) {
        self.inner.constraints.add(ParamConstraint {
            tool_name,
            param_path,
            rule: ConstraintRule::Required,
        });
    }

    fn allow_param_values(
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

    #[pyo3(signature = (tool_name, param_path, min = None, max = None))]
    fn limit_param_range(
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

    fn set_time(&mut self, now_ms: u64) {
        self.inner.set_time(now_ms);
    }

    fn evaluate(&mut self, tool_name: String, args_json: String) -> GovernanceVerdict {
        let args: serde_json::Value =
            serde_json::from_str(&args_json).unwrap_or(serde_json::Value::Null);
        let call = RustToolCall {
            id: CompactString::new(""),
            name: CompactString::new(&tool_name),
            arguments: args,
        };
        let caller = AgentIdentity::new(self.agent_id.as_str(), self.session_id.as_str());
        governance_verdict_from_rust(self.inner.evaluate(&call, &caller))
    }
}

// ────────────────────────── Dream / idle-pipeline types ──────────────────────────────────────────

/// A single session of agent messages, used as input to `IdlePipeline.feed_trigger`.
#[pyclass]
#[derive(Clone)]
struct SessionData {
    #[pyo3(get, set)]
    session_id: String,
    #[pyo3(get, set)]
    agent_id: String,
    #[pyo3(get, set)]
    messages: Vec<Message>,
    /// JSON-encoded metadata blob.
    #[pyo3(get, set)]
    metadata: String,
    #[pyo3(get, set)]
    created_at_ms: f64,
    #[pyo3(get, set)]
    updated_at_ms: f64,
}

#[pymethods]
impl SessionData {
    #[new]
    #[pyo3(signature = (session_id, agent_id, messages, metadata = String::from("null"), created_at_ms = 0.0, updated_at_ms = 0.0))]
    fn new(
        session_id: String,
        agent_id: String,
        messages: Vec<Message>,
        metadata: String,
        created_at_ms: f64,
        updated_at_ms: f64,
    ) -> Self {
        Self {
            session_id,
            agent_id,
            messages,
            metadata,
            created_at_ms,
            updated_at_ms,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "SessionData(session_id={:?}, agent_id={:?})",
            self.session_id, self.agent_id
        )
    }
}

impl SessionData {
    fn to_rust(&self) -> Result<RustSessionData, PyErr> {
        let messages: Vec<RustMessage> = self
            .messages
            .iter()
            .map(|m| m.to_rust())
            .collect::<Result<_, _>>()?;
        let metadata: serde_json::Value =
            serde_json::from_str(&self.metadata).unwrap_or(serde_json::Value::Null);
        Ok(RustSessionData {
            session_id: self.session_id.clone(),
            agent_id: self.agent_id.clone(),
            messages,
            metadata,
            created_at_ms: self.created_at_ms as u64,
            updated_at_ms: self.updated_at_ms as u64,
        })
    }
}

/// A long-term memory entry as stored by the agent.
#[pyclass]
#[derive(Clone)]
struct MemoryEntry {
    #[pyo3(get, set)]
    text: String,
    #[pyo3(get, set)]
    score: f64,
    /// JSON-encoded metadata blob.
    #[pyo3(get, set)]
    metadata: String,
}

#[pymethods]
impl MemoryEntry {
    #[new]
    #[pyo3(signature = (text, score = 0.0, metadata = String::from("null")))]
    fn new(text: String, score: f64, metadata: String) -> Self {
        Self {
            text,
            score,
            metadata,
        }
    }

    fn __repr__(&self) -> String {
        format!("MemoryEntry(text={:?}, score={})", self.text, self.score)
    }
}

impl MemoryEntry {
    fn to_rust(&self) -> RustMemoryEntry {
        let metadata: serde_json::Value =
            serde_json::from_str(&self.metadata).unwrap_or(serde_json::Value::Null);
        RustMemoryEntry {
            text: self.text.clone(),
            score: self.score,
            metadata,
        }
    }

    fn from_rust(e: &RustMemoryEntry) -> Self {
        Self {
            text: e.text.clone(),
            score: e.score,
            metadata: serde_json::to_string(&e.metadata).unwrap_or_else(|_| "null".into()),
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct CurationStats {
    #[pyo3(get)]
    insights_processed: u32,
    #[pyo3(get)]
    duplicates_removed: u32,
    #[pyo3(get)]
    conflicts_resolved: u32,
    #[pyo3(get)]
    entries_added: u32,
}

#[pymethods]
impl CurationStats {
    fn __repr__(&self) -> String {
        format!(
            "CurationStats(added={}, removed={}, conflicts={})",
            self.entries_added, self.duplicates_removed, self.conflicts_resolved
        )
    }
}

/// The delta `DreamStore.commit` must apply: add `to_add`, remove `to_remove_indices`.
#[pyclass]
#[derive(Clone)]
struct CurationResult {
    #[pyo3(get)]
    to_add: Vec<MemoryEntry>,
    /// Indices into the `existing_memories` slice passed to `feed_trigger`.
    #[pyo3(get)]
    to_remove_indices: Vec<u32>,
    #[pyo3(get)]
    stats: CurationStats,
}

impl CurationResult {
    fn from_rust(r: RustCurationResult) -> Self {
        Self {
            to_add: r.to_add.iter().map(MemoryEntry::from_rust).collect(),
            to_remove_indices: r.to_remove_indices.iter().map(|&i| i as u32).collect(),
            stats: CurationStats {
                insights_processed: r.stats.insights_processed as u32,
                duplicates_removed: r.stats.duplicates_removed as u32,
                conflicts_resolved: r.stats.conflicts_resolved as u32,
                entries_added: r.stats.entries_added as u32,
            },
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct IdleRunResult {
    #[pyo3(get)]
    sessions_processed: u32,
    #[pyo3(get)]
    insights_extracted: u32,
}

#[pymethods]
impl IdleRunResult {
    fn __repr__(&self) -> String {
        format!(
            "IdleRunResult(sessions={}, insights={})",
            self.sessions_processed, self.insights_extracted
        )
    }
}

/// Tagged union returned by `IdlePipeline` methods. Inspect `kind`:
/// - `"synthesize_insights"` → `messages`
/// - `"commit_memories"`     → `agent_id`, `curation_result`, `run_result`
/// - `"noop"` | `"aborted"`
#[pyclass]
#[derive(Clone)]
struct IdlePipelineAction {
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    messages: Option<Vec<Message>>,
    #[pyo3(get)]
    agent_id: Option<String>,
    #[pyo3(get)]
    curation_result: Option<CurationResult>,
    #[pyo3(get)]
    run_result: Option<IdleRunResult>,
}

#[pymethods]
impl IdlePipelineAction {
    fn __repr__(&self) -> String {
        format!("IdlePipelineAction(kind={:?})", self.kind)
    }
}

impl IdlePipelineAction {
    fn from_rust(a: RustIdleAction) -> Self {
        match a {
            RustIdleAction::SynthesizeInsights { messages } => Self {
                kind: "synthesize_insights".into(),
                messages: Some(messages.iter().map(Message::from_rust).collect()),
                agent_id: None,
                curation_result: None,
                run_result: None,
            },
            RustIdleAction::CommitMemories {
                agent_id,
                result,
                run_result,
            } => Self {
                kind: "commit_memories".into(),
                messages: None,
                agent_id: Some(agent_id),
                curation_result: Some(CurationResult::from_rust(result)),
                run_result: Some(IdleRunResult {
                    sessions_processed: run_result.sessions_processed as u32,
                    insights_extracted: run_result.insights_extracted as u32,
                }),
            },
            RustIdleAction::Noop => Self {
                kind: "noop".into(),
                messages: None,
                agent_id: None,
                curation_result: None,
                run_result: None,
            },
            RustIdleAction::Aborted => Self {
                kind: "aborted".into(),
                messages: None,
                agent_id: None,
                curation_result: None,
                run_result: None,
            },
        }
    }
}

/// Kernel state machine for the idle dreaming cycle.
///
/// Drive it like this:
/// 1. `feed_trigger(sessions, existing_memories, now_ms)` → `"synthesize_insights"` action
/// 2. Call LLM with `action.messages`, collect the text response
/// 3. `feed_synthesis_result(text)` → `"commit_memories"` action
/// 4. Apply `action.curation_result` via `DreamStore.commit`, then call `reset()`
#[pyclass]
struct IdlePipeline {
    inner: RustIdlePipeline,
}

#[pymethods]
impl IdlePipeline {
    #[new]
    fn new(agent_id: String) -> Self {
        Self {
            inner: RustIdlePipeline::new(RustIdlePolicy::new(agent_id)),
        }
    }

    /// Phase 1 — provide sessions + current memory snapshot; kernel builds the LLM prompt.
    fn feed_trigger(
        &mut self,
        sessions: Vec<SessionData>,
        existing_memories: Vec<MemoryEntry>,
        now_ms: f64,
    ) -> PyResult<IdlePipelineAction> {
        let rust_sessions: Vec<RustSessionData> = sessions
            .iter()
            .map(|s| s.to_rust())
            .collect::<Result<_, _>>()?;
        let rust_memories: Vec<RustMemoryEntry> =
            existing_memories.iter().map(|e| e.to_rust()).collect();
        let action = self.inner.feed(RustIdleEvent::Trigger {
            sessions: rust_sessions,
            existing_memories: rust_memories,
            now_ms: now_ms as u64,
        });
        Ok(IdlePipelineAction::from_rust(action))
    }

    /// Phase 2 — feed back the LLM's synthesis text; kernel parses and curates.
    fn feed_synthesis_result(&mut self, content: String) -> IdlePipelineAction {
        IdlePipelineAction::from_rust(self.inner.feed(RustIdleEvent::SynthesisResult { content }))
    }

    fn is_idle(&self) -> bool {
        self.inner.is_idle()
    }

    /// Reset to `Idle` after handling `CommitMemories` to allow the next cycle.
    fn reset(&mut self) {
        self.inner.reset();
    }
}

// ──────────────────────────────── EvalPipeline ────────────────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct PyCriterionResult {
    #[pyo3(get)]
    criterion: String,
    #[pyo3(get)]
    passed: bool,
    #[pyo3(get)]
    score: f32,
    #[pyo3(get)]
    feedback: String,
}

#[pymethods]
impl PyCriterionResult {
    fn __repr__(&self) -> String {
        format!(
            "CriterionResult(criterion={:?}, passed={:?})",
            self.criterion, self.passed
        )
    }
}

#[pyclass]
#[derive(Clone)]
struct SkillCandidate {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    description: String,
    #[pyo3(get)]
    when_to_use: Option<String>,
    #[pyo3(get)]
    content: String,
}

#[pymethods]
impl SkillCandidate {
    fn __repr__(&self) -> String {
        format!("SkillCandidate(name={:?})", self.name)
    }
}

impl SkillCandidate {
    fn from_rust(s: RustSkillCandidate) -> Self {
        Self {
            name: s.name,
            description: s.description,
            when_to_use: s.when_to_use,
            content: s.content,
        }
    }
}

/// The structured verdict from [`parse_verdict`]: `passed`, `overall_score`, `feedback`, per-criterion
/// `details`, and an optional `skill_candidate` distilled from a passing run.
#[pyclass]
#[derive(Clone)]
struct Verdict {
    #[pyo3(get)]
    passed: bool,
    #[pyo3(get)]
    overall_score: f32,
    #[pyo3(get)]
    feedback: String,
    #[pyo3(get)]
    details: Vec<PyCriterionResult>,
    #[pyo3(get)]
    skill_candidate: Option<SkillCandidate>,
}

#[pymethods]
impl Verdict {
    fn __repr__(&self) -> String {
        format!(
            "Verdict(passed={:?}, overall_score={:?})",
            self.passed, self.overall_score
        )
    }
}

fn criteria_from_py(criteria: Vec<PyObject>) -> Vec<RustCriterion> {
    use pyo3::Python;
    Python::with_gil(|py| {
        criteria
            .iter()
            .map(|obj| {
                let text = obj
                    .getattr(py, "text")
                    .and_then(|v| v.extract::<String>(py))
                    .unwrap_or_default();
                let required = obj
                    .getattr(py, "required")
                    .and_then(|v| v.extract::<bool>(py))
                    .unwrap_or(true);
                let weight = obj
                    .getattr(py, "weight")
                    .and_then(|v| v.extract::<f32>(py))
                    .unwrap_or(1.0);
                RustCriterion {
                    text,
                    required,
                    weight,
                }
            })
            .collect::<Vec<_>>()
    })
}

/// Build the impartial-evaluator messages for one attempt. Call the evaluator LLM with these, then
/// feed the text to `parse_verdict`. (0.5.0 fold of the former `EvalPipeline` class, OS-axis #6.)
#[pyfunction]
#[pyo3(signature = (goal, criteria, result, attempt, extract_skill_on_pass = true))]
fn build_eval_messages(
    goal: String,
    criteria: Vec<PyObject>,
    result: String,
    attempt: u32,
    extract_skill_on_pass: bool,
) -> Vec<Message> {
    let rust_criteria = criteria_from_py(criteria);
    rust_build_eval_messages(&goal, &rust_criteria, &result, attempt, extract_skill_on_pass)
        .iter()
        .map(Message::from_rust)
        .collect()
}

/// Parse the evaluator LLM's JSON response into a structured `Verdict`.
#[pyfunction]
fn parse_verdict(content: String) -> Verdict {
    let r = rust_parse_verdict(&content);
    Verdict {
        passed: r.passed,
        overall_score: r.overall_score,
        feedback: r.feedback,
        details: r
            .details
            .into_iter()
            .map(|d| PyCriterionResult {
                criterion: d.criterion,
                passed: d.passed,
                score: d.score,
                feedback: d.feedback,
            })
            .collect(),
        skill_candidate: r.skill_candidate.map(SkillCandidate::from_rust),
    }
}

/// JSON Schema (as a JSON string) for the verdict an eval node must produce — used as the
/// `output_schema` of the eval node in the `gen_eval` workflow template.
#[pyfunction]
#[pyo3(signature = (extract_skill_on_pass = true))]
fn verdict_output_schema(extract_skill_on_pass: bool) -> String {
    rust_verdict_output_schema(extract_skill_on_pass).to_string()
}

// ──────────────────────────────────────── module registration ─────────────────────────────────

#[pymodule]
fn _kernel(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // POD types
    m.add_class::<ContentPartObj>()?;
    m.add_class::<Message>()?;
    m.add_class::<ToolCall>()?;
    m.add_class::<ToolResult>()?;
    m.add_class::<ToolSchema>()?;
    m.add_class::<RuntimeTask>()?;
    m.add_class::<LoopPolicy>()?;
    m.add_class::<LoopResult>()?;
    m.add_class::<TaskUpdate>()?;
    // Skill types
    m.add_class::<SkillMetadata>()?;
    // Loop control
    m.add_class::<KernelRuntime>()?;
    // Signal types
    m.add_class::<RuntimeSignal>()?;
    m.add_class::<SignalRouter>()?;
    m.add_class::<GovernanceVerdict>()?;
    m.add_class::<Governance>()?;
    // Dream / idle-pipeline
    m.add_class::<SessionData>()?;
    m.add_class::<MemoryEntry>()?;
    m.add_class::<CurationStats>()?;
    m.add_class::<CurationResult>()?;
    m.add_class::<IdleRunResult>()?;
    m.add_class::<IdlePipelineAction>()?;
    m.add_class::<IdlePipeline>()?;
    // Eval / harness quality gate (0.5.0 fold: free functions, was the EvalPipeline class)
    m.add_class::<PyCriterionResult>()?;
    m.add_class::<SkillCandidate>()?;
    m.add_class::<Verdict>()?;
    m.add_function(wrap_pyfunction!(build_eval_messages, m)?)?;
    m.add_function(wrap_pyfunction!(parse_verdict, m)?)?;
    m.add_function(wrap_pyfunction!(verdict_output_schema, m)?)?;
    Ok(())
}

// Python-level integration tests live in `tests/` (pytest after `maturin develop`).
// We deliberately don't add Rust unit tests here because the `extension-module`
// PyO3 feature breaks `cargo test` linking — the tested behavior is already
// covered exhaustively in deepstrike-core's own test suite.
