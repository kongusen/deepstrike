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
//!         msg = call_llm(action.messages, action.tools)
//!         action = sm.feed_llm_response(msg)
//!     elif action.kind == "execute_tools":
//!         # SDK intercepts calls where name == "skill" and reads the file
//!         results = exec_tools(action.calls)
//!         action = sm.feed_tool_results(results)
//!     elif action.kind == "done":
//!         break
//! ```

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use compact_str::CompactString;

use deepstrike_core::context::manager::ContextManager;
use deepstrike_core::context::pressure::PressureAction;
use deepstrike_core::governance::pipeline::GovernancePipeline as RustGovernancePipeline;
use deepstrike_core::harness::eval_pipeline::{
    EvalAction as RustEvalAction, EvalEvent as RustEvalEvent, EvalPolicy as RustEvalPolicy,
    EvalPipeline as RustEvalPipeline, EvalResult as RustEvalResult, SkillCandidate as RustSkillCandidate,
};
use deepstrike_core::memory::curator::CurationResult as RustCurationResult;
use deepstrike_core::memory::durable::SessionData as RustSessionData;
use deepstrike_core::memory::idle_pipeline::{
    IdleAction as RustIdleAction, IdleEvent as RustIdleEvent, IdlePolicy as RustIdlePolicy,
    IdlePipeline as RustIdlePipeline,
};
use deepstrike_core::memory::semantic::MemoryEntry as RustMemoryEntry;
use deepstrike_core::scheduler::policy::LoopPolicy as RustLoopPolicy;
use deepstrike_core::scheduler::state_machine::{
    LoopAction as RustLoopAction, LoopEvent as RustLoopEvent, LoopObservation as RustLoopObservation,
    LoopStateMachine as RustLoopStateMachine,
};
use deepstrike_core::signals::router::SignalRouter as RustSignalRouter;
use deepstrike_core::types::policy::SignalDisposition as RustSignalDisposition;
use deepstrike_core::types::signal::{
    RuntimeSignal as RustRuntimeSignal, SignalSource as RustSignalSource,
    SignalType as RustSignalType, Urgency as RustUrgency,
};
use deepstrike_core::types::message::{
    Content, ContentPart, Message as RustMessage, Role, ToolCall as RustToolCall,
    ToolResult as RustToolResult, ToolSchema as RustToolSchema,
};
use deepstrike_core::types::result::LoopResult as RustLoopResult;
use deepstrike_core::types::skill::SkillMetadata as RustSkillMetadata;
use deepstrike_core::types::task::RuntimeTask as RustRuntimeTask;

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
    timestamp_ms: f64,
}

#[pymethods]
impl RuntimeSignal {
    #[new]
    #[pyo3(signature = (source, urgency, summary, signal_type="event", payload="null", dedupe_key=None, timestamp_ms=0.0))]
    fn new(source: String, urgency: String, summary: String, signal_type: &str, payload: &str, dedupe_key: Option<String>, timestamp_ms: f64) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            source, urgency, summary,
            signal_type: signal_type.into(),
            payload: payload.into(),
            dedupe_key,
            timestamp_ms,
        }
    }

    fn __repr__(&self) -> String {
        format!("RuntimeSignal(urgency={:?}, summary={:?})", self.urgency, self.summary)
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
        let payload: serde_json::Value = serde_json::from_str(&self.payload).unwrap_or(serde_json::Value::Null);
        let mut sig = RustRuntimeSignal::new(source, signal_type, urgency, self.summary.as_str())
            .with_payload(payload)
            .with_timestamp(self.timestamp_ms as u64);
        if let Some(ref key) = self.dedupe_key {
            sig = sig.with_dedupe(key.as_str());
        }
        sig
    }

    fn from_rust(s: &RustRuntimeSignal) -> Self {
        Self {
            id: s.id.to_string(),
            source: match s.source { RustSignalSource::Cron => "cron", RustSignalSource::Gateway => "gateway", RustSignalSource::Heartbeat => "heartbeat", RustSignalSource::Custom => "custom" }.into(),
            signal_type: match s.signal_type { RustSignalType::Event => "event", RustSignalType::Job => "job", RustSignalType::Alert => "alert" }.into(),
            urgency: match s.urgency { RustUrgency::Critical => "critical", RustUrgency::High => "high", RustUrgency::Normal => "normal", RustUrgency::Low => "low" }.into(),
            summary: s.summary.to_string(),
            payload: serde_json::to_string(&s.payload).unwrap_or_else(|_| "null".into()),
            dedupe_key: s.dedupe_key.as_ref().map(|k| k.to_string()),
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
struct Message {
    #[pyo3(get, set)]
    role: String,
    #[pyo3(get, set)]
    content: String,
    #[pyo3(get, set)]
    token_count: Option<u32>,
    #[pyo3(get)]
    tool_calls: Vec<ToolCall>,
}

#[pymethods]
impl Message {
    #[new]
    #[pyo3(signature = (role, content, token_count = None, tool_calls = None))]
    fn new(
        role: String,
        content: String,
        token_count: Option<u32>,
        tool_calls: Option<Vec<ToolCall>>,
    ) -> Self {
        Self {
            role,
            content,
            token_count,
            tool_calls: tool_calls.unwrap_or_default(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Message(role={:?}, content={:?}, tokens={:?})",
            self.role, self.content, self.token_count
        )
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
        Ok(RustMessage {
            role,
            content: Content::Text(self.content.clone()),
            tool_calls: self.tool_calls.iter().map(|c| c.to_rust()).collect::<Result<_, _>>()?,
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
        let content = match &msg.content {
            Content::Text(s) => s.clone(),
            Content::Parts(parts) => parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => text.clone(),
                    ContentPart::Image { url } => format!("[image: {url}]"),
                    ContentPart::ToolResult { call_id, output, .. } => {
                        format!("[tool_result {call_id}]: {output}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        Self {
            role: role.to_string(),
            content,
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
        Self { id, name, arguments }
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
    token_count: Option<u32>,
}

#[pymethods]
impl ToolResult {
    #[new]
    #[pyo3(signature = (call_id, output, is_error = false, token_count = None))]
    fn new(call_id: String, output: String, is_error: bool, token_count: Option<u32>) -> Self {
        Self { call_id, output, is_error, token_count }
    }
}

impl ToolResult {
    fn to_rust(&self) -> RustToolResult {
        RustToolResult {
            call_id: CompactString::new(&self.call_id),
            output: Content::Text(self.output.clone()),
            is_error: self.is_error,
            token_count: self.token_count,
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
        Self { name, description, parameters }
    }
}

impl ToolSchema {
    fn to_rust(&self) -> Result<RustToolSchema, PyErr> {
        let params: serde_json::Value = serde_json::from_str(&self.parameters)
            .map_err(|e| PyValueError::new_err(format!("invalid JSON parameters: {e}")))?;
        Ok(RustToolSchema {
            name: CompactString::new(&self.name),
            description: self.description.clone(),
            parameters: params,
        })
    }
}

#[pyclass]
#[derive(Clone)]
struct RuntimeTask {
    #[pyo3(get, set)]
    goal: String,
    #[pyo3(get, set)]
    criteria: Vec<String>,
}

#[pymethods]
impl RuntimeTask {
    #[new]
    #[pyo3(signature = (goal, criteria = None))]
    fn new(goal: String, criteria: Option<Vec<String>>) -> Self {
        Self { goal, criteria: criteria.unwrap_or_default() }
    }
}

impl RuntimeTask {
    fn to_rust(&self) -> RustRuntimeTask {
        RustRuntimeTask {
            goal: self.goal.clone(),
            criteria: self.criteria.clone(),
            metadata: serde_json::Value::Null,
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
    fn new(max_tokens: u32, max_turns: u32, max_total_tokens: u64, timeout_ms: Option<u64>) -> Self {
        Self { max_tokens, max_turns, max_total_tokens, timeout_ms }
    }
}

impl LoopPolicy {
    fn to_rust(&self) -> RustLoopPolicy {
        RustLoopPolicy {
            max_tokens: self.max_tokens,
            max_turns: self.max_turns,
            max_total_tokens: self.max_total_tokens,
            timeout_ms: self.timeout_ms,
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

impl LoopResult {
    fn from_rust(r: &RustLoopResult) -> Self {
        let termination = match r.termination {
            deepstrike_core::types::result::TerminationReason::Completed => "completed",
            deepstrike_core::types::result::TerminationReason::MaxTurns => "max_turns",
            deepstrike_core::types::result::TerminationReason::TokenBudget => "token_budget",
            deepstrike_core::types::result::TerminationReason::Timeout => "timeout",
            deepstrike_core::types::result::TerminationReason::UserAbort => "user_abort",
            deepstrike_core::types::result::TerminationReason::Error => "error",
        };
        Self {
            termination: termination.to_string(),
            final_message: r.final_message.as_ref().map(Message::from_rust),
            turns_used: r.turns_used,
            total_tokens_used: r.total_tokens_used,
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
        format!("SkillMetadata(name={:?}, effort={:?}, est_tokens={})", self.name, self.effort, self.estimated_tokens)
    }
}

impl SkillMetadata {
    fn to_rust(&self) -> RustSkillMetadata {
        RustSkillMetadata {
            name: CompactString::new(&self.name),
            description: self.description.clone(),
            when_to_use: self.when_to_use.clone(),
            allowed_tools: self.allowed_tools.iter().map(CompactString::new).collect(),
            effort: self.effort,
            estimated_tokens: self.estimated_tokens,
        }
    }

    fn from_rust(m: &RustSkillMetadata) -> Self {
        Self {
            name: m.name.to_string(),
            description: m.description.clone(),
            when_to_use: m.when_to_use.clone(),
            allowed_tools: m.allowed_tools.iter().map(|s| s.to_string()).collect(),
            effort: m.effort,
            estimated_tokens: m.estimated_tokens,
        }
    }
}

// ─────────────────────────────── Tagged-union: LoopAction / Observation ────────────────────────

/// Tagged union for `LoopAction`. Inspect `kind` then read the matching field:
/// - `kind == "call_llm"`      → `messages`, `tools` (includes `skill` meta-tool when skills registered)
/// - `kind == "execute_tools"` → `calls`
/// - `kind == "done"`          → `result`
#[pyclass]
#[derive(Clone)]
struct LoopAction {
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    messages: Option<Vec<Message>>,
    #[pyo3(get)]
    tools: Option<Vec<ToolSchema>>,
    #[pyo3(get)]
    calls: Option<Vec<ToolCall>>,
    #[pyo3(get)]
    result: Option<LoopResult>,
}

#[pymethods]
impl LoopAction {
    fn __repr__(&self) -> String {
        format!("LoopAction(kind={:?})", self.kind)
    }
}

impl LoopAction {
    fn from_rust(a: RustLoopAction) -> Self {
        match a {
            RustLoopAction::CallLLM { messages, tools } => Self {
                kind: "call_llm".to_string(),
                messages: Some(messages.iter().map(Message::from_rust).collect()),
                tools: Some(
                    tools
                        .iter()
                        .map(|t| ToolSchema {
                            name: t.name.to_string(),
                            description: t.description.clone(),
                            parameters: serde_json::to_string(&t.parameters)
                                .unwrap_or_else(|_| "null".into()),
                        })
                        .collect(),
                ),
                calls: None,
                result: None,
            },
            RustLoopAction::ExecuteTools { calls } => Self {
                kind: "execute_tools".to_string(),
                messages: None,
                tools: None,
                calls: Some(calls.iter().map(ToolCall::from_rust).collect()),
                result: None,
            },
            RustLoopAction::Done { result } => Self {
                kind: "done".to_string(),
                messages: None,
                tools: None,
                calls: None,
                result: Some(LoopResult::from_rust(&result)),
            },
        }
    }
}

/// Tagged union for `LoopObservation`. Inspect `kind`:
/// - `kind == "compressed"` → `action`, `rho_after`
#[pyclass]
#[derive(Clone)]
struct LoopObservation {
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    action: Option<String>,
    #[pyo3(get)]
    rho_after: Option<f64>,
}

impl LoopObservation {
    fn from_rust(o: RustLoopObservation) -> Self {
        match o {
            RustLoopObservation::Compressed { action, rho_after } => {
                let action_str = match action {
                    PressureAction::None => "none",
                    PressureAction::SnipCompact => "snip_compact",
                    PressureAction::MicroCompact => "micro_compact",
                    PressureAction::ContextCollapse => "context_collapse",
                    PressureAction::AutoCompact => "auto_compact",
                };
                Self {
                    kind: "compressed".into(),
                    action: Some(action_str.into()),
                    rho_after: Some(rho_after),
                }
            }
        }
    }
}

// ─────────────────────────────────── ContextEngine (manager) ───────────────────────────────────

#[pyclass]
struct ContextEngine {
    inner: ContextManager,
}

#[pymethods]
impl ContextEngine {
    #[new]
    fn new(max_tokens: u32) -> Self {
        Self { inner: ContextManager::new(max_tokens) }
    }

    fn add_system_message(&mut self, content: String, tokens: u32) {
        self.inner
            .partitions
            .system
            .push(RustMessage::system(content), tokens);
    }

    fn add_user_message(&mut self, content: String, tokens: u32) {
        self.inner.push_history(RustMessage::user(content), tokens);
    }

    fn add_assistant_message(&mut self, content: String, tokens: u32) {
        self.inner
            .push_history(RustMessage::assistant(content), tokens);
    }

    fn pressure(&self) -> f64 {
        self.inner.rho()
    }

    fn total_tokens(&self) -> u32 {
        self.inner.partitions.total_tokens()
    }

    /// Run compression at the level the current pressure recommends.
    /// Returns tokens saved.
    fn compress(&mut self) -> u32 {
        let action = self.inner.should_compress();
        if action == PressureAction::None {
            return 0;
        }
        let before = self.inner.partitions.total_tokens();
        self.inner.compress(action);
        let after = self.inner.partitions.total_tokens();
        before.saturating_sub(after)
    }

    fn render(&self, _budget: u32) -> Vec<Message> {
        self.inner.render().iter().map(Message::from_rust).collect()
    }

    /// Replace the available-skills set. The kernel auto-injects the `skill`
    /// meta-tool into every `CallLLM` action when skills are registered.
    fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.inner
            .set_available_skills(skills.iter().map(|s| s.to_rust()).collect());
    }
}

// ──────────────────────────────────────── LoopStateMachine ────────────────────────────────────

#[pyclass]
struct LoopStateMachine {
    inner: RustLoopStateMachine,
}

#[pymethods]
impl LoopStateMachine {
    #[new]
    fn new(policy: LoopPolicy) -> Self {
        Self { inner: RustLoopStateMachine::new(policy.to_rust()) }
    }

    /// Convenience: forward to inner ContextManager for skill registration
    /// without making the user juggle two objects.
    fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.inner
            .ctx
            .set_available_skills(skills.iter().map(|s| s.to_rust()).collect());
    }

    /// Enable the `memory` meta-tool. Call with `True` when a DreamStore and agent_id
    /// are configured — the SDK layer intercepts `memory` tool calls and runs the search.
    fn set_memory_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_memory_enabled(enabled);
    }

    /// Enable the `knowledge` meta-tool. Call with `True` when a KnowledgeSource
    /// is configured — the SDK layer intercepts `knowledge` tool calls and runs retrieval.
    fn set_knowledge_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_knowledge_enabled(enabled);
    }

    fn set_tools(&mut self, tools: Vec<ToolSchema>) -> PyResult<()> {
        let rust_tools: Vec<RustToolSchema> = tools
            .iter()
            .map(|t| t.to_rust())
            .collect::<Result<_, _>>()?;
        self.inner.tools = rust_tools;
        Ok(())
    }

    fn start(&mut self, task: RuntimeTask) -> LoopAction {
        LoopAction::from_rust(self.inner.start(task.to_rust()))
    }

    fn feed_llm_response(&mut self, message: Message) -> PyResult<LoopAction> {
        let msg = message.to_rust()?;
        Ok(LoopAction::from_rust(self.inner.feed(RustLoopEvent::LLMResponse { message: msg })))
    }

    fn feed_tool_results(&mut self, results: Vec<ToolResult>) -> LoopAction {
        let results: Vec<RustToolResult> = results.iter().map(|r| r.to_rust()).collect();
        LoopAction::from_rust(self.inner.feed(RustLoopEvent::ToolResults { results }))
    }

    fn feed_timeout(&mut self) -> LoopAction {
        LoopAction::from_rust(self.inner.feed(RustLoopEvent::Timeout))
    }

    fn is_terminal(&self) -> bool {
        self.inner.is_terminal()
    }

    fn turn(&self) -> u32 {
        self.inner.turn
    }

    fn pressure(&self) -> f64 {
        self.inner.ctx.rho()
    }

    fn take_observations(&mut self) -> Vec<LoopObservation> {
        self.inner
            .take_observations()
            .into_iter()
            .map(LoopObservation::from_rust)
            .collect()
    }

    /// Read-only access to the rendered context for inspection / LLM call building.
    fn render(&self) -> Vec<Message> {
        self.inner.ctx.render().iter().map(Message::from_rust).collect()
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
        Self { inner: RustSignalRouter::new(max_queue_size) }
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

    fn depth(&self) -> usize {
        self.inner.depth()
    }

    fn clear_dedup(&mut self) {
        self.inner.clear_dedup();
    }
}

// ──────────────────────────────────────── Governance (passthrough) ─────────────────────────────

#[pyclass]
struct Governance {
    inner: RustGovernancePipeline,
}

#[pymethods]
impl Governance {
    #[new]
    fn new() -> Self {
        Self { inner: RustGovernancePipeline::default() }
    }

    fn block_tool(&mut self, name: String) {
        self.inner.veto.block_tool(name);
    }

    fn set_time(&mut self, now_ms: u64) {
        self.inner.set_time(now_ms);
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
        Self { session_id, agent_id, messages, metadata, created_at_ms, updated_at_ms }
    }

    fn __repr__(&self) -> String {
        format!("SessionData(session_id={:?}, agent_id={:?})", self.session_id, self.agent_id)
    }
}

impl SessionData {
    fn to_rust(&self) -> Result<RustSessionData, PyErr> {
        let messages: Vec<RustMessage> =
            self.messages.iter().map(|m| m.to_rust()).collect::<Result<_, _>>()?;
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
        Self { text, score, metadata }
    }

    fn __repr__(&self) -> String {
        format!("MemoryEntry(text={:?}, score={})", self.text, self.score)
    }
}

impl MemoryEntry {
    fn to_rust(&self) -> RustMemoryEntry {
        let metadata: serde_json::Value =
            serde_json::from_str(&self.metadata).unwrap_or(serde_json::Value::Null);
        RustMemoryEntry { text: self.text.clone(), score: self.score, metadata }
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
            RustIdleAction::CommitMemories { agent_id, result, run_result } => Self {
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
        Self { inner: RustIdlePipeline::new(RustIdlePolicy::new(agent_id)) }
    }

    /// Phase 1 — provide sessions + current memory snapshot; kernel builds the LLM prompt.
    fn feed_trigger(
        &mut self,
        sessions: Vec<SessionData>,
        existing_memories: Vec<MemoryEntry>,
        now_ms: f64,
    ) -> PyResult<IdlePipelineAction> {
        let rust_sessions: Vec<RustSessionData> =
            sessions.iter().map(|s| s.to_rust()).collect::<Result<_, _>>()?;
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
        IdlePipelineAction::from_rust(
            self.inner.feed(RustIdleEvent::SynthesisResult { content }),
        )
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
        Self { name: s.name, description: s.description, when_to_use: s.when_to_use, content: s.content }
    }
}

/// Tagged union returned by `EvalPipeline` methods. Inspect `kind`:
/// - `"evaluate"` → `messages` (SDK must call evaluator LLM, then `feed_eval_result`)
/// - `"done"`     → `passed`, `feedback`, optional `skill_candidate`
#[pyclass]
#[derive(Clone)]
struct EvalPipelineAction {
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    messages: Option<Vec<Message>>,
    #[pyo3(get)]
    passed: Option<bool>,
    #[pyo3(get)]
    feedback: Option<String>,
    #[pyo3(get)]
    skill_candidate: Option<SkillCandidate>,
}

#[pymethods]
impl EvalPipelineAction {
    fn __repr__(&self) -> String {
        format!("EvalPipelineAction(kind={:?}, passed={:?})", self.kind, self.passed)
    }
}

impl EvalPipelineAction {
    fn from_rust_action(a: RustEvalAction) -> Self {
        match a {
            RustEvalAction::Evaluate { messages } => Self {
                kind: "evaluate".into(),
                messages: Some(messages.iter().map(Message::from_rust).collect()),
                passed: None,
                feedback: None,
                skill_candidate: None,
            },
            RustEvalAction::Done { result } => Self::from_rust_result(result),
        }
    }

    fn from_rust_result(r: RustEvalResult) -> Self {
        Self {
            kind: "done".into(),
            messages: None,
            passed: Some(r.passed),
            feedback: Some(r.feedback),
            skill_candidate: r.skill_candidate.map(SkillCandidate::from_rust),
        }
    }
}

/// Kernel state machine for the evaluation cycle.
///
/// Drive it like this:
/// 1. `feed_outcome(goal, criteria, result, attempt)` → `"evaluate"` action
/// 2. Call evaluator LLM with `action.messages`, collect the text response
/// 3. `feed_eval_result(text)` → `"done"` action
/// 4. Read `action.passed` / `action.feedback` / `action.skill_candidate`
/// 5. Call `reset()` before the next attempt
#[pyclass]
struct EvalPipeline {
    inner: RustEvalPipeline,
}

#[pymethods]
impl EvalPipeline {
    #[new]
    #[pyo3(signature = (extract_skill_on_pass = true))]
    fn new(extract_skill_on_pass: bool) -> Self {
        Self { inner: RustEvalPipeline::new(RustEvalPolicy { extract_skill_on_pass }) }
    }

    fn feed_outcome(
        &mut self,
        goal: String,
        criteria: Vec<String>,
        result: String,
        attempt: u32,
    ) -> EvalPipelineAction {
        EvalPipelineAction::from_rust_action(
            self.inner.feed(RustEvalEvent::Outcome { goal, criteria, result, attempt }),
        )
    }

    fn feed_eval_result(&mut self, content: String) -> EvalPipelineAction {
        EvalPipelineAction::from_rust_action(
            self.inner.feed(RustEvalEvent::EvalResult { content }),
        )
    }

    fn is_idle(&self) -> bool {
        self.inner.is_idle()
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

// ──────────────────────────────────────── module registration ─────────────────────────────────

#[pymodule]
fn _kernel(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // POD types
    m.add_class::<Message>()?;
    m.add_class::<ToolCall>()?;
    m.add_class::<ToolResult>()?;
    m.add_class::<ToolSchema>()?;
    m.add_class::<RuntimeTask>()?;
    m.add_class::<LoopPolicy>()?;
    m.add_class::<LoopResult>()?;
    // Skill types
    m.add_class::<SkillMetadata>()?;
    // Loop control
    m.add_class::<LoopAction>()?;
    m.add_class::<LoopObservation>()?;
    m.add_class::<LoopStateMachine>()?;
    // Engines
    m.add_class::<ContextEngine>()?;
    // Signal types
    m.add_class::<RuntimeSignal>()?;
    m.add_class::<SignalRouter>()?;
    m.add_class::<Governance>()?;
    // Dream / idle-pipeline
    m.add_class::<SessionData>()?;
    m.add_class::<MemoryEntry>()?;
    m.add_class::<CurationStats>()?;
    m.add_class::<CurationResult>()?;
    m.add_class::<IdleRunResult>()?;
    m.add_class::<IdlePipelineAction>()?;
    m.add_class::<IdlePipeline>()?;
    // Eval / harness pipeline
    m.add_class::<SkillCandidate>()?;
    m.add_class::<EvalPipelineAction>()?;
    m.add_class::<EvalPipeline>()?;
    Ok(())
}

// Python-level integration tests live in `tests/` (pytest after `maturin develop`).
// We deliberately don't add Rust unit tests here because the `extension-module`
// PyO3 feature breaks `cargo test` linking — the tested behavior is already
// covered exhaustively in deepstrike-core's own test suite.
