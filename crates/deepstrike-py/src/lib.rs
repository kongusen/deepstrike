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
use pyo3::types::PyBytes;

use compact_str::CompactString;

use deepstrike_core::governance::constraint::{ConstraintRule, ParamConstraint};
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline as RustGovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::harness::eval::{
    Criterion as RustCriterion, SkillCandidate as RustSkillCandidate,
    build_eval_messages as rust_build_eval_messages, parse_verdict as rust_parse_verdict,
    verdict_output_schema as rust_verdict_output_schema,
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
    deadline_ms: Option<f64>,
    #[pyo3(get, set)]
    coalesce_key: Option<String>,
    #[pyo3(get, set)]
    coalesced_count: u32,
    #[pyo3(get, set)]
    timestamp_ms: f64,
}

#[pymethods]
impl RuntimeSignal {
    #[new]
    #[pyo3(signature = (source, urgency, summary, signal_type="event", payload="null", dedupe_key=None, timestamp_ms=0.0, recipient=None, deadline_ms=None, coalesce_key=None, coalesced_count=1))]
    fn new(
        source: String,
        urgency: String,
        summary: String,
        signal_type: &str,
        payload: &str,
        dedupe_key: Option<String>,
        timestamp_ms: f64,
        recipient: Option<String>,
        deadline_ms: Option<f64>,
        coalesce_key: Option<String>,
        coalesced_count: u32,
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
            deadline_ms,
            coalesce_key,
            coalesced_count: coalesced_count.max(1),
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
    fn to_rust(&self) -> PyResult<RustRuntimeSignal> {
        // §7.7 · the signal id is the caller's own branded ref, kept verbatim (see the Node binding).
        let id = self.id.clone();
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
        sig.id = id.into();
        if let Some(ref key) = self.dedupe_key {
            sig = sig.with_dedupe(key.as_str());
        }
        if let Some(ref recipient) = self.recipient {
            sig = sig.with_recipient(recipient.as_str());
        }
        if let Some(deadline_ms) = self.deadline_ms {
            sig = sig.with_deadline(deadline_ms as u64);
        }
        if let Some(ref coalesce_key) = self.coalesce_key {
            sig = sig.with_coalesce(coalesce_key.as_str());
        }
        sig.coalesced_count = self.coalesced_count.max(1);
        Ok(sig)
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
            deadline_ms: s.deadline_ms.map(|value| value as f64),
            coalesce_key: s.coalesce_key.as_ref().map(|key| key.to_string()),
            coalesced_count: s.coalesced_count.max(1),
            timestamp_ms: s.timestamp_ms as f64,
        }
    }
}

fn disposition_str(d: RustSignalDisposition) -> &'static str {
    d.label()
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

// ─────────────────────────────── Canonical Kernel ABI ───────────────────────────────

#[pyclass(name = "_CanonicalPreparation")]
struct CanonicalPreparation {
    #[pyo3(get)]
    status: String,
    #[pyo3(get)]
    prepare_token: Option<String>,
    /// Python's arbitrary-precision `int` projection of `WireU64`.
    #[pyo3(get)]
    step_seq: Option<u64>,
    #[pyo3(get)]
    expected_head: Option<String>,
    #[pyo3(get)]
    record_digest: Option<String>,
    /// Exact `KernelRecord::record_bytes()` from core.
    #[pyo3(get)]
    record_bytes: Option<Py<PyBytes>>,
    #[pyo3(get)]
    planned_step_json: Option<String>,
    #[pyo3(get)]
    fault_json: Option<String>,
}

#[pyclass(name = "_CanonicalCommit")]
struct CanonicalCommit {
    #[pyo3(get)]
    step_seq: u64,
    #[pyo3(get)]
    record_digest: String,
    #[pyo3(get)]
    planned_step_json: String,
    #[pyo3(get)]
    checkpoint_advice_json: Option<String>,
}

#[pyclass(name = "_CanonicalCheckpoint")]
struct CanonicalCheckpoint {
    #[pyo3(get)]
    checkpoint_bytes: Py<PyBytes>,
    #[pyo3(get)]
    through_step_seq: u64,
    #[pyo3(get)]
    covered_head: String,
    #[pyo3(get)]
    state_digest: String,
    #[pyo3(get)]
    ack_token: String,
}

#[pyclass(name = "_CanonicalRestoreCost")]
struct CanonicalRestoreCost {
    #[pyo3(get)]
    records_before_checkpoint: u64,
    #[pyo3(get)]
    tail_inputs_replayed: u64,
    #[pyo3(get)]
    records_after_checkpoint: u64,
    #[pyo3(get)]
    bytes_read: u64,
}

/// Durable canonical operation handle. It exposes no direct `step` method.
#[pyclass(name = "_CanonicalKernel")]
struct CanonicalKernel {
    inner: RustCanonicalKernel,
}

#[pymethods]
impl CanonicalKernel {
    #[new]
    fn new() -> Self {
        Self {
            inner: RustCanonicalKernel::default(),
        }
    }

    fn prepare(&mut self, py: Python<'_>, input_json: String) -> PyResult<CanonicalPreparation> {
        canonical_preparation_from_rust(py, self.inner.prepare_json(&input_json))
    }

    fn commit(
        &mut self,
        prepare_token: String,
        appended_head: String,
    ) -> PyResult<CanonicalCommit> {
        let token = RustPrepareToken::new(prepare_token)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let head = RustDigest::new(appended_head)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let committed = self
            .inner
            .commit(&token, &head)
            .map_err(canonical_kernel_error)?;
        Ok(CanonicalCommit {
            step_seq: committed.step_seq.get(),
            record_digest: committed.record.record_digest().to_string(),
            planned_step_json: serde_json::to_string(&committed.step)
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
            checkpoint_advice_json: committed
                .checkpoint_advice
                .map(canonical_checkpoint_advice_json)
                .transpose()?,
        })
    }

    fn abort(&mut self, prepare_token: String) -> PyResult<()> {
        let token = RustPrepareToken::new(prepare_token)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        self.inner
            .abort(&token)
            .map(|_| ())
            .map_err(canonical_kernel_error)
    }

    fn checkpoint_candidate(&self, py: Python<'_>) -> PyResult<CanonicalCheckpoint> {
        self.inner
            .checkpoint_candidate()
            .map(|checkpoint| canonical_checkpoint_from_rust(py, checkpoint))
            .map_err(canonical_kernel_error)
    }

    fn checkpoint_rebase(
        &self,
        py: Python<'_>,
        checkpoint_bytes: &Bound<'_, PyBytes>,
    ) -> PyResult<CanonicalCheckpoint> {
        let checkpoint = RustKernelCheckpoint::from_checkpoint_bytes(checkpoint_bytes.as_bytes())
            .map_err(|error| canonical_kernel_error(error.fault()))?;
        self.inner
            .checkpoint_rebase(&checkpoint)
            .map(|candidate| canonical_checkpoint_from_rust(py, candidate))
            .map_err(canonical_kernel_error)
    }

    fn ack_checkpoint(&mut self, through_step_seq: u64, covered_head: String) -> PyResult<()> {
        let boundary = RustCheckpointBoundary {
            through_step_seq: RustWireU64::new(through_step_seq),
            covered_head: RustDigest::new(covered_head)
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
        };
        self.inner
            .note_checkpoint_acked(&boundary)
            .map(|_| ())
            .map_err(canonical_kernel_error)
    }

    /// Replace this native object in place from a checkpoint plus post-checkpoint records.
    #[pyo3(signature = (checkpoint_bytes, record_bytes))]
    fn restore(
        &mut self,
        checkpoint_bytes: Option<&Bound<'_, PyBytes>>,
        record_bytes: Vec<Bound<'_, PyBytes>>,
    ) -> PyResult<CanonicalRestoreCost> {
        let records = record_bytes
            .iter()
            .map(|bytes| bytes.as_bytes().to_vec())
            .collect::<Vec<_>>();
        let cost = self
            .inner
            .restore_bytes(checkpoint_bytes.map(|bytes| bytes.as_bytes()), &records)
            .map_err(canonical_kernel_error)?;
        Ok(CanonicalRestoreCost {
            records_before_checkpoint: cost.records_before_checkpoint,
            tail_inputs_replayed: cost.tail_inputs_replayed,
            records_after_checkpoint: cost.records_after_checkpoint,
            bytes_read: cost.bytes_read,
        })
    }

    fn lifecycle(&self) -> &'static str {
        match self.inner.lifecycle() {
            RustOperationLifecycle::Created => "created",
            RustOperationLifecycle::Configured => "configured",
            RustOperationLifecycle::Running => "running",
            RustOperationLifecycle::Suspended => "suspended",
            RustOperationLifecycle::Completed => "completed",
            RustOperationLifecycle::Cancelled => "cancelled",
            RustOperationLifecycle::Failed => "failed",
        }
    }

    fn pending_effects_json(&self) -> PyResult<String> {
        serde_json::to_string(&self.inner.pending_effects().collect::<Vec<_>>())
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    fn terminal_json(&self) -> PyResult<Option<String>> {
        self.inner
            .terminal()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }
}

fn canonical_preparation_from_rust(
    py: Python<'_>,
    preparation: RustKernelPreparation<
        deepstrike_core::runtime::kernel::wire::KernelRecord,
        deepstrike_core::runtime::kernel::wire::PlannedStep,
    >,
) -> PyResult<CanonicalPreparation> {
    match preparation {
        RustKernelPreparation::Prepared(prepared) => Ok(CanonicalPreparation {
            status: "prepared".into(),
            prepare_token: Some(prepared.token.to_string()),
            step_seq: Some(prepared.record.step_seq().get()),
            expected_head: prepared.record.expected_head().map(ToString::to_string),
            record_digest: Some(prepared.record.record_digest().to_string()),
            record_bytes: Some(
                PyBytes::new(py, prepared.record.record_bytes().as_slice()).unbind(),
            ),
            planned_step_json: Some(
                serde_json::to_string(&prepared.planned_step)
                    .map_err(|error| PyValueError::new_err(error.to_string()))?,
            ),
            fault_json: None,
        }),
        RustKernelPreparation::Replayed(replayed) => Ok(CanonicalPreparation {
            status: "replayed".into(),
            prepare_token: None,
            step_seq: Some(replayed.step_seq.get()),
            expected_head: replayed
                .record
                .as_ref()
                .and_then(|record| record.expected_head())
                .map(ToString::to_string),
            record_digest: Some(replayed.record_digest.to_string()),
            record_bytes: replayed
                .record
                .map(|record| PyBytes::new(py, record.record_bytes().as_slice()).unbind()),
            planned_step_json: replayed
                .committed_step
                .map(|step| serde_json::to_string(&step))
                .transpose()
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
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
            fault_json: Some(
                serde_json::to_string(&rejected.fault)
                    .map_err(|error| PyValueError::new_err(error.to_string()))?,
            ),
        }),
    }
}

fn canonical_checkpoint_from_rust(
    py: Python<'_>,
    checkpoint: deepstrike_core::runtime::kernel::wire::CheckpointCandidate,
) -> CanonicalCheckpoint {
    CanonicalCheckpoint {
        checkpoint_bytes: PyBytes::new(py, checkpoint.checkpoint_bytes.as_slice()).unbind(),
        through_step_seq: checkpoint.through_step_seq.get(),
        covered_head: checkpoint.covered_head.to_string(),
        state_digest: checkpoint.state_digest.to_string(),
        ack_token: checkpoint.ack_token.to_string(),
    }
}

fn canonical_checkpoint_advice_json(
    advice: deepstrike_core::runtime::kernel::wire::CheckpointAdvice,
) -> PyResult<String> {
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
    .map_err(|error| PyValueError::new_err(error.to_string()))
}

fn canonical_kernel_error(fault: deepstrike_core::runtime::kernel::wire::KernelFault) -> PyErr {
    PyValueError::new_err(serde_json::to_string(&fault).unwrap_or_else(|_| fault.to_string()))
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
    fn ingest(&mut self, signal: RuntimeSignal, lifecycle: &str) -> PyResult<&'static str> {
        let lifecycle = match lifecycle {
            "ready" => TaskLifecycle::Ready,
            "running" => TaskLifecycle::Running,
            "suspended" => TaskLifecycle::Suspended,
            "done" => {
                TaskLifecycle::Done(deepstrike_core::types::result::TerminationReason::Completed)
            }
            other => {
                return Err(PyValueError::new_err(format!(
                    "invalid task lifecycle {other:?}; expected ready|running|suspended|done"
                )));
            }
        };
        Ok(disposition_str(
            self.inner.ingest(signal.to_rust()?, lifecycle),
        ))
    }

    /// Pull the next queued signal (highest priority first).
    fn next(&mut self) -> Option<RuntimeSignal> {
        self.inner.next().as_ref().map(RuntimeSignal::from_rust)
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

/// A completed session transcript for durable-memory extraction.
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

#[pyclass]
#[derive(Clone)]
struct MemoryScope {
    #[pyo3(get, set)]
    tenant_id: String,
    #[pyo3(get, set)]
    namespace: String,
}

#[pymethods]
impl MemoryScope {
    #[new]
    fn new(tenant_id: String, namespace: String) -> Self {
        Self {
            tenant_id,
            namespace,
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct MemoryProvenance {
    #[pyo3(get, set)]
    session_id: Option<String>,
    #[pyo3(get, set)]
    author: String,
    #[pyo3(get, set)]
    trust: String,
    #[pyo3(get, set)]
    evidence_refs: Vec<String>,
}

#[pymethods]
impl MemoryProvenance {
    #[new]
    #[pyo3(signature = (author, trust, session_id = None, evidence_refs = Vec::new()))]
    fn new(
        author: String,
        trust: String,
        session_id: Option<String>,
        evidence_refs: Vec<String>,
    ) -> Self {
        Self {
            session_id,
            author,
            trust,
            evidence_refs,
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct MemoryRecord {
    #[pyo3(get, set)]
    record_id: String,
    #[pyo3(get, set)]
    scope: MemoryScope,
    #[pyo3(get, set)]
    name: String,
    #[pyo3(get, set)]
    kind: String,
    #[pyo3(get, set)]
    content: String,
    #[pyo3(get, set)]
    description: String,
    #[pyo3(get, set)]
    provenance: MemoryProvenance,
    #[pyo3(get, set)]
    created_at: u64,
    #[pyo3(get, set)]
    updated_at: u64,
    #[pyo3(get, set)]
    last_recalled_at: Option<u64>,
    #[pyo3(get, set)]
    recall_count: u64,
    #[pyo3(get, set)]
    confidence: f64,
    #[pyo3(get, set)]
    links: Vec<String>,
    #[pyo3(get, set)]
    pinned: bool,
    #[pyo3(get, set)]
    ttl_days: Option<u32>,
}

#[pymethods]
impl MemoryRecord {
    #[new]
    #[pyo3(signature = (record_id, scope, name, kind, content, description, provenance, created_at, updated_at, confidence, last_recalled_at = None, recall_count = 0, links = Vec::new(), pinned = false, ttl_days = None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        record_id: String,
        scope: MemoryScope,
        name: String,
        kind: String,
        content: String,
        description: String,
        provenance: MemoryProvenance,
        created_at: u64,
        updated_at: u64,
        confidence: f64,
        last_recalled_at: Option<u64>,
        recall_count: u64,
        links: Vec<String>,
        pinned: bool,
        ttl_days: Option<u32>,
    ) -> Self {
        Self {
            record_id,
            scope,
            name,
            kind,
            content,
            description,
            provenance,
            created_at,
            updated_at,
            last_recalled_at,
            recall_count,
            confidence,
            links,
            pinned,
            ttl_days,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "MemoryRecord(record_id={:?}, name={:?}, kind={:?})",
            self.record_id, self.name, self.kind
        )
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
    rust_build_eval_messages(
        &goal,
        &rust_criteria,
        &result,
        attempt,
        extract_skill_on_pass,
    )
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
    m.add(
        "KERNEL_ABI_VERSION",
        deepstrike_core::runtime::kernel::wire::KERNEL_ABI_VERSION,
    )?;
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
    m.add_class::<CanonicalPreparation>()?;
    m.add_class::<CanonicalCommit>()?;
    m.add_class::<CanonicalCheckpoint>()?;
    m.add_class::<CanonicalRestoreCost>()?;
    m.add_class::<CanonicalKernel>()?;
    // Signal types
    m.add_class::<RuntimeSignal>()?;
    m.add_class::<SignalRouter>()?;
    m.add_class::<GovernanceVerdict>()?;
    m.add_class::<Governance>()?;
    // Durable-memory wire values
    m.add_class::<SessionData>()?;
    m.add_class::<MemoryScope>()?;
    m.add_class::<MemoryProvenance>()?;
    m.add_class::<MemoryRecord>()?;
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
