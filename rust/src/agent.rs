use compact_str::CompactString;
use deepstrike_core::scheduler::state_machine::{LoopEvent, LoopStateMachine};
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::types::message::{Content, Message, Role, ToolCall};
use deepstrike_core::types::task::RuntimeTask;
use deepstrike_core::signals::router::SignalRouter;
use deepstrike_core::types::policy::SignalDisposition;
use deepstrike_core::types::signal::{RuntimeSignal as KernelSignal, SignalSource as KernelSignalSource, SignalType as KernelSignalType, Urgency};
use futures::StreamExt;
use std::collections::HashMap;

use deepstrike_core::memory::idle_pipeline::{IdleAction, IdleEvent, IdlePolicy, IdlePipeline};

use crate::harness::{Harness, HarnessOutcome, HarnessRequest, QualityGate};
use crate::knowledge::KnowledgeSource;
use crate::memory::{DreamResult, DreamStore};
use crate::providers::{LLMProvider, StreamEvent};
use crate::signals::SignalSource;
use crate::tools::{RegisteredTool, execute_tools};
use crate::{Error, Result};

pub struct AgentOptions {
    pub max_tokens: u32,
    pub max_turns: u32,
    pub timeout_ms: Option<u64>,
    pub extensions: Option<serde_json::Value>,
    /// System-level instructions prepended to every context render.
    /// Injected into the `system` partition before `start()` is called.
    pub system_prompt: Option<String>,
    /// Long-term memory snippets pre-seeded into the `memory` context partition.
    /// Injected before `start()` so they are available from the first LLM call.
    pub initial_memory: Vec<String>,
    /// Directory containing skill `.md` files. The kernel auto-injects the
    /// `skill` meta-tool so the model can load any skill on demand.
    pub skill_dir: Option<std::path::PathBuf>,
    pub knowledge_source: Option<Box<dyn KnowledgeSource>>,
    pub signal_source: Option<Box<dyn SignalSource>>,
    /// Backing store for the idle dreaming pipeline. When set, `Agent::dream()`
    /// becomes available to trigger a memory consolidation cycle.
    pub dream_store: Option<Box<dyn DreamStore>>,
    /// Stable identifier for this agent. Required together with `dream_store` to
    /// enable in-session memory retrieval via the `memory` meta-tool.
    pub agent_id: Option<String>,
}

impl AgentOptions {
    pub fn new(max_tokens: u32) -> Self {
        Self {
            max_tokens,
            max_turns: 25,
            timeout_ms: None,
            extensions: None,
            system_prompt: None,
            initial_memory: Vec::new(),
            skill_dir: None,
            knowledge_source: None,
            signal_source: None,
            dream_store: None,
            agent_id: None,
        }
    }
}

pub struct Agent {
    provider: Box<dyn LLMProvider>,
    options: AgentOptions,
    tools: HashMap<String, RegisteredTool>,
    blocked_tools: std::collections::HashSet<String>,
    interrupted: std::sync::atomic::AtomicBool,
}

impl Agent {
    pub fn new(provider: impl LLMProvider + 'static, options: AgentOptions) -> Self {
        Self {
            provider: Box::new(provider),
            options,
            tools: HashMap::new(),
            blocked_tools: Default::default(),
            interrupted: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn register(&mut self, tool: RegisteredTool) -> &mut Self {
        self.tools.insert(tool.schema.name.to_string(), tool);
        self
    }

    pub fn unregister(&mut self, name: &str) -> &mut Self {
        self.tools.remove(name);
        self
    }

    pub fn block_tool(&mut self, name: impl Into<String>) -> &mut Self {
        self.blocked_tools.insert(name.into());
        self
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn run(&self, goal: &str) -> Result<String> {
        let mut final_text = String::new();
        let mut status = "error".to_string();
        let mut iterations = 0u32;

        let mut stream = self.run_streaming(goal, &[], None).await?;
        while let Some(evt) = stream.next().await {
            match evt? {
                RunEvent::TextDelta(d) => final_text.push_str(&d),
                RunEvent::Done { iterations: i, status: s, .. } => { iterations = i; status = s; }
                _ => {}
            }
        }
        Ok(format!("done in {iterations} turns ({status})"))
    }

    pub async fn run_streaming<'a>(
        &'a self,
        goal: &'a str,
        criteria: &'a [String],
        extensions: Option<&'a serde_json::Value>,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + 'a>>> {
        self.interrupted.store(false, std::sync::atomic::Ordering::Relaxed);

        // Warm up the knowledge source once per run.
        if let Some(ks) = &self.options.knowledge_source {
            ks.init().await?;
        }

        let policy = LoopPolicy {
            max_tokens: self.options.max_tokens,
            max_turns: self.options.max_turns,
            timeout_ms: self.options.timeout_ms,
            ..Default::default()
        };
        let mut sm = LoopStateMachine::new(policy);
        sm.tools = self.tools.values().map(|t| t.schema.clone()).collect();

        // Enable in-session memory retrieval when both store and agent identity are configured.
        if self.options.dream_store.is_some() && self.options.agent_id.is_some() {
            sm.ctx.set_memory_enabled(true);
        }

        // Enable knowledge meta-tool when a KnowledgeSource is configured.
        if self.options.knowledge_source.is_some() {
            sm.ctx.set_knowledge_enabled(true);
        }

        // Inject system prompt into the system partition before starting.
        if let Some(ref sp) = self.options.system_prompt {
            let tokens = ((sp.len() / 4) as u32).max(1);
            sm.ctx.partitions.system.push(
                deepstrike_core::types::message::Message::system(sp.clone()),
                tokens,
            );
        }

        // Pre-seed the memory partition with caller-supplied long-term memories.
        for mem in &self.options.initial_memory {
            let tokens = ((mem.len() / 4) as u32).max(1);
            sm.ctx.partitions.memory.push(
                deepstrike_core::types::message::Message::user(mem.clone()),
                tokens,
            );
        }

        // Scan skill directory and register metadata so the kernel injects the skill meta-tool.
        if let Some(skill_dir) = &self.options.skill_dir {
            if let Ok(entries) = std::fs::read_dir(skill_dir) {
                let mut metas = Vec::new();
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("md") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            let name = path.file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("")
                                .to_string();
                            let description = parse_frontmatter_description(&content);
                            metas.push(deepstrike_core::types::skill::SkillMetadata::new(name, description));
                        }
                    }
                }
                sm.ctx.set_available_skills(metas);
            }
        }

        let task = RuntimeTask::new(goal).with_criteria(criteria.to_vec());
        let mut action = sm.start(task);

        let ext = match (self.options.extensions.as_ref(), extensions) {
            (Some(base), Some(over)) => {
                let mut merged = base.clone();
                if let (Some(m), Some(o)) = (merged.as_object_mut(), over.as_object()) {
                    for (k, v) in o { m.insert(k.clone(), v.clone()); }
                }
                Some(merged)
            }
            (Some(b), None) => Some(b.clone()),
            (None, Some(o)) => Some(o.clone()),
            _ => None,
        };

        Ok(Box::pin(async_stream::try_stream! {
            let mut final_text = String::new();
            let mut router = SignalRouter::new(256);
            let session_start_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let mut session_messages: Vec<deepstrike_core::types::message::Message> =
                vec![deepstrike_core::types::message::Message::user(goal.to_string())];

            loop {
                if self.interrupted.load(std::sync::atomic::Ordering::Relaxed) {
                    action = sm.feed(LoopEvent::Timeout);
                    break;
                }
                if let Some(ss) = &self.options.signal_source {
                    if let Some(sdk_sig) = ss.next_signal().await? {
                        let urgency = match sdk_sig.kind.as_str() {
                            "interrupt" => Urgency::Critical,
                            _ => Urgency::Normal,
                        };
                        let kernel_sig = KernelSignal::new(
                            KernelSignalSource::Custom,
                            KernelSignalType::Event,
                            urgency,
                            sdk_sig.kind.as_str(),
                        ).with_payload(sdk_sig.payload.clone())
                         .with_timestamp(std::time::SystemTime::now()
                             .duration_since(std::time::UNIX_EPOCH)
                             .unwrap_or_default().as_millis() as u64);
                        let is_executing = matches!(action, deepstrike_core::scheduler::state_machine::LoopAction::ExecuteTools { .. });
                        match router.ingest(kernel_sig, is_executing) {
                            SignalDisposition::InterruptNow | SignalDisposition::Interrupt => {
                                action = sm.feed(LoopEvent::Timeout);
                                break;
                            }
                            _ => {}
                        }
                    }
                }

                sm.take_observations(); // drain

                match &action {
                    deepstrike_core::scheduler::state_machine::LoopAction::CallLLM { messages, tools } => {
                        final_text.clear();
                        let mut final_tool_calls: Vec<ToolCall> = Vec::new();
                        let mut provider_stream = self.provider.stream(messages, tools, ext.as_ref()).await?;
                        while let Some(evt) = provider_stream.next().await {
                            match evt? {
                                StreamEvent::TextDelta { delta } => {
                                    final_text.push_str(&delta);
                                    yield RunEvent::TextDelta(delta);
                                }
                                StreamEvent::ThinkingDelta { delta } => {
                                    yield RunEvent::ThinkingDelta(delta);
                                }
                                StreamEvent::ToolCall { id, name, arguments } => {
                                    yield RunEvent::ToolCall { id: id.clone(), name: name.clone() };
                                    final_tool_calls.push(ToolCall {
                                        id: CompactString::new(&id),
                                        name: CompactString::new(&name),
                                        arguments,
                                    });
                                }
                                StreamEvent::Done => {}
                            }
                        }
                        action = sm.feed(LoopEvent::LLMResponse {
                            message: Message {
                                role: Role::Assistant,
                                content: Content::Text(final_text.clone()),
                                tool_calls: final_tool_calls.clone(),
                                token_count: None,
                            },
                        });
                        session_messages.push(Message {
                            role: Role::Assistant,
                            content: Content::Text(final_text.clone()),
                            tool_calls: final_tool_calls,
                            token_count: None,
                        });
                    }
                    deepstrike_core::scheduler::state_machine::LoopAction::ExecuteTools { calls } => {
                        let blocked: Vec<_> = calls.iter().filter(|c| self.blocked_tools.contains(c.name.as_str())).collect();
                        let unblocked: Vec<ToolCall> = calls.iter().filter(|c| !self.blocked_tools.contains(c.name.as_str())).cloned().collect();
                        for c in &blocked {
                            yield RunEvent::Error(format!("tool blocked: {}", c.name));
                        }

                        // Intercept `skill`, `memory`, and `knowledge` meta-tool calls.
                        use deepstrike_core::context::skill_catalog::SKILL_TOOL_NAME;
                        use deepstrike_core::context::manager::{MEMORY_TOOL_NAME, KNOWLEDGE_TOOL_NAME};
                        let mut all_results: Vec<deepstrike_core::types::message::ToolResult> = Vec::new();
                        let (skill_calls, rest): (Vec<ToolCall>, Vec<ToolCall>) =
                            unblocked.into_iter().partition(|c| c.name.as_str() == SKILL_TOOL_NAME);
                        let (memory_calls, rest2): (Vec<ToolCall>, Vec<ToolCall>) =
                            rest.into_iter().partition(|c| c.name.as_str() == MEMORY_TOOL_NAME);
                        let (knowledge_calls, regular_calls): (Vec<ToolCall>, Vec<ToolCall>) =
                            rest2.into_iter().partition(|c| c.name.as_str() == KNOWLEDGE_TOOL_NAME);

                        for c in skill_calls {
                            let name = c.arguments.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let (output, is_error) = if let Some(dir) = &self.options.skill_dir {
                                let path = dir.join(format!("{name}.md"));
                                match tokio::fs::read_to_string(&path).await {
                                    Ok(content) => (strip_frontmatter(&content).to_string(), false),
                                    Err(_) => (format!("Skill \"{name}\" not found."), true),
                                }
                            } else {
                                ("No skill directory configured.".into(), true)
                            };
                            let call_id = c.id.clone();
                            yield RunEvent::ToolResult { call_id: call_id.to_string(), content: output.clone(), is_error };
                            all_results.push(deepstrike_core::types::message::ToolResult {
                                call_id,
                                output: Content::Text(output),
                                is_error,
                                token_count: None,
                            });
                        }

                        for c in memory_calls {
                            let query = c.arguments.get("query")
                                .and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let top_k = c.arguments.get("top_k")
                                .and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                            let (output, is_error) = match (&self.options.dream_store, &self.options.agent_id) {
                                (Some(store), Some(agent_id)) => {
                                    match store.search(agent_id, &query, top_k).await {
                                        Ok(entries) if !entries.is_empty() => {
                                            let text = entries.iter()
                                                .map(|e| format!("[score={:.3}] {}", e.score, e.text))
                                                .collect::<Vec<_>>()
                                                .join("\n---\n");
                                            (text, false)
                                        }
                                        Ok(_) => ("No relevant memories found.".into(), false),
                                        Err(e) => (format!("Memory search error: {e}"), true),
                                    }
                                }
                                _ => ("Memory retrieval not configured.".into(), true),
                            };
                            let call_id = c.id.clone();
                            yield RunEvent::ToolResult { call_id: call_id.to_string(), content: output.clone(), is_error };
                            all_results.push(deepstrike_core::types::message::ToolResult {
                                call_id,
                                output: Content::Text(output),
                                is_error,
                                token_count: None,
                            });
                        }

                        for c in knowledge_calls {
                            let query = c.arguments.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let top_k = c.arguments.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                            let (output, is_error) = if let Some(ks) = &self.options.knowledge_source {
                                match ks.retrieve(&query, top_k).await {
                                    Ok(snippets) if !snippets.is_empty() => (snippets.join("\n---\n"), false),
                                    Ok(_) => ("No relevant knowledge found.".into(), false),
                                    Err(e) => (format!("Knowledge retrieval error: {e}"), true),
                                }
                            } else {
                                ("Knowledge source not configured.".into(), true)
                            };
                            let call_id = c.id.clone();
                            yield RunEvent::ToolResult { call_id: call_id.to_string(), content: output.clone(), is_error };
                            all_results.push(deepstrike_core::types::message::ToolResult {
                                call_id, output: Content::Text(output), is_error, token_count: None,
                            });
                        }

                        let results = execute_tools(&regular_calls, &self.tools).await;
                        for r in &results {
                            let content = match &r.output { Content::Text(s) => s.clone(), _ => String::new() };
                            yield RunEvent::ToolResult { call_id: r.call_id.to_string(), content, is_error: r.is_error };
                        }
                        all_results.extend(results);
                        action = sm.feed(LoopEvent::ToolResults { results: all_results });
                    }
                    deepstrike_core::scheduler::state_machine::LoopAction::Done { result } => {
                        // Auto-save session when DreamStore is configured.
                        if let (Some(store), Some(agent_id)) = (&self.options.dream_store, &self.options.agent_id) {
                            if session_messages.len() > 1 {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64;
                                let session = deepstrike_core::memory::durable::SessionData {
                                    session_id: format!("{agent_id}-{session_start_ms}"),
                                    agent_id: agent_id.clone(),
                                    messages: session_messages.clone(),
                                    metadata: serde_json::Value::Null,
                                    created_at_ms: session_start_ms,
                                    updated_at_ms: now_ms,
                                };
                                let _ = store.save_session(session).await;
                            }
                        }
                        let termination = format!("{:?}", result.termination).to_lowercase();
                        yield RunEvent::Done {
                            iterations: result.turns_used,
                            total_tokens: result.total_tokens_used,
                            status: termination,
                        };
                        return;
                    }
                }

                if sm.is_terminal() { break; }
            }

            // terminal without Done action
            yield RunEvent::Done { iterations: sm.turn, total_tokens: 0, status: "error".into() };
        }))
    }

    /// Trigger an idle dreaming cycle for the given agent.
    ///
    /// This orchestrates the two-phase consolidation pipeline:
    /// 1. **Rule-based analysis** — kernel scans sessions, builds LLM prompt  (pure computation)
    /// 2. **LLM synthesis**       — SDK calls the provider to generate insights (I/O, here)
    /// 3. **Curation**            — kernel deduplicates and trims                (pure computation)
    /// 4. **Commit**              — SDK writes the delta back to the store        (I/O, here)
    ///
    /// `now_ms` is wall-clock time injected by the caller; the kernel never reads it directly.
    pub async fn dream(&self, agent_id: &str, now_ms: u64) -> Result<DreamResult> {
        let store = self.options.dream_store.as_ref().ok_or_else(|| {
            Error::Other("dream_store not configured on AgentOptions".into())
        })?;

        // --- SDK I/O: load raw data -----------------------------------------
        let sessions = store.load_sessions(agent_id).await?;
        let existing_memories = store.load_memories(agent_id).await?;

        if sessions.is_empty() {
            return Ok(DreamResult::default());
        }

        // --- Phase 1: kernel builds prompt (pure computation) ----------------
        let policy = IdlePolicy::new(agent_id);
        let mut pipeline = IdlePipeline::new(policy);

        let messages = match pipeline.feed(IdleEvent::Trigger {
            sessions,
            existing_memories: existing_memories.clone(),
            now_ms,
        }) {
            IdleAction::SynthesizeInsights { messages } => messages,
            IdleAction::Noop => return Ok(DreamResult::default()),
            _ => return Err(Error::Other("unexpected action from IdlePipeline::Trigger".into())),
        };

        // --- Phase 2: SDK calls LLM (I/O) ------------------------------------
        let mut synthesis_text = String::new();
        let mut stream = self.provider.stream(&messages, &[], None).await?;
        while let Some(evt) = stream.next().await {
            if let Ok(StreamEvent::TextDelta { delta }) = evt {
                synthesis_text.push_str(&delta);
            }
        }

        // --- Phase 3: kernel parses + curates (pure computation) -------------
        let (curation_result, run_result) =
            match pipeline.feed(IdleEvent::SynthesisResult { content: synthesis_text }) {
                IdleAction::CommitMemories { result, run_result, .. } => (result, run_result),
                _ => {
                    return Err(Error::Other(
                        "unexpected action from IdlePipeline::SynthesisResult".into(),
                    ))
                }
            };

        let entries_added = curation_result.stats.entries_added;
        let entries_removed = curation_result.to_remove_indices.len();

        // --- Phase 4: SDK writes delta to store (I/O) ------------------------
        store.commit(agent_id, curation_result, &existing_memories).await?;

        Ok(DreamResult {
            sessions_processed: run_result.sessions_processed,
            insights_extracted: run_result.insights_extracted,
            entries_added,
            entries_removed,
        })
    }
}

fn strip_frontmatter(content: &str) -> &str {
    let s = content.trim_start();
    if !s.starts_with("---") { return s; }
    let rest = &s[3..];
    if let Some(end) = rest.find("\n---") {
        rest[end + 4..].trim_start_matches('\n')
    } else {
        s
    }
}

/// Extract `description:` from YAML frontmatter in a skill `.md` file.
fn parse_frontmatter_description(content: &str) -> String {
    let body = content.trim_start();
    if !body.starts_with("---") {
        return String::new();
    }
    let rest = &body[3..];
    let end = rest.find("\n---").unwrap_or(rest.len());
    for line in rest[..end].lines() {
        if let Some(val) = line.strip_prefix("description:") {
            return val.trim().to_string();
        }
    }
    String::new()
}

#[derive(Debug, Clone)]
pub enum RunEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCall { id: String, name: String },
    ToolResult { call_id: String, content: String, is_error: bool },
    Done { iterations: u32, total_tokens: u64, status: String },
    Error(String),
}

/// SinglePassHarness — run once, always passes.
pub struct SinglePassHarness<'a> {
    agent: &'a Agent,
}

impl<'a> SinglePassHarness<'a> {
    pub fn new(agent: &'a Agent) -> Self { Self { agent } }

    pub async fn run(&self, request: HarnessRequest) -> Result<HarnessOutcome> {
        let (text, iterations, total_tokens, status) = collect_run(self.agent, &request).await?;
        Ok(HarnessOutcome { result: text, passed: true, iterations, total_tokens, status, overall_score: 1.0, feedback: None, details: vec![] })
    }
}

/// EvalLoopHarness — retry until QualityGate passes (deprecated, use HarnessLoop).
pub struct EvalLoopHarness<'a, G: QualityGate> {
    agent: &'a Agent,
    gate: G,
    max_attempts: usize,
}

impl<'a, G: QualityGate> EvalLoopHarness<'a, G> {
    pub fn new(agent: &'a Agent, gate: G, max_attempts: usize) -> Self {
        Self { agent, gate, max_attempts }
    }

    pub async fn run(&self, request: HarnessRequest) -> Result<HarnessOutcome> {
        let mut outcome = HarnessOutcome { result: String::new(), passed: false, iterations: 0, total_tokens: 0, status: "error".into(), overall_score: 0.0, feedback: None, details: vec![] };
        for _ in 0..self.max_attempts {
            let (text, iterations, total_tokens, status) = collect_run(self.agent, &request).await?;
            outcome = HarnessOutcome { result: text, passed: false, iterations, total_tokens, status, overall_score: 0.0, feedback: None, details: vec![] };
            if self.gate.evaluate(&request, &outcome).await? {
                outcome.passed = true;
                return Ok(outcome);
            }
        }
        Ok(outcome)
    }
}

/// HarnessLoop — LLM-as-judge with feedback injection and skill extraction.
pub struct HarnessLoop<'a> {
    agent: &'a Agent,
    eval_provider: Box<dyn LLMProvider>,
    max_attempts: usize,
    skill_dir: Option<std::path::PathBuf>,
}

impl<'a> HarnessLoop<'a> {
    pub fn new(
        agent: &'a Agent,
        eval_provider: impl LLMProvider + 'static,
        max_attempts: usize,
        skill_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self { agent, eval_provider: Box::new(eval_provider), max_attempts, skill_dir }
    }

    pub async fn run(&self, request: HarnessRequest) -> Result<HarnessOutcome> {
        use deepstrike_core::harness::eval_pipeline::{EvalAction, EvalEvent, EvalPipeline, EvalPolicy};
        use futures::StreamExt;

        let mut pipeline = EvalPipeline::new(EvalPolicy { extract_skill_on_pass: true });
        let mut current_goal = request.goal.clone();
        let mut outcome = HarnessOutcome {
            result: String::new(),
            passed: false,
            iterations: 0,
            total_tokens: 0,
            status: "error".into(),
            overall_score: 0.0,
            feedback: None,
            details: vec![],
        };

        for attempt in 1..=self.max_attempts as u32 {
            let (text, iterations, total_tokens, status) = collect_run_with_goal(self.agent, &current_goal, &request).await?;
            outcome = HarnessOutcome { result: text.clone(), passed: false, iterations, total_tokens, status, overall_score: 0.0, feedback: None, details: vec![] };

            // Phase 1: kernel builds eval prompt
            let eval_action = pipeline.feed(EvalEvent::Outcome {
                goal: request.goal.clone(),
                criteria: request.criteria.iter().map(|c| deepstrike_core::harness::eval_pipeline::Criterion {
                    text: c.text.clone(),
                    required: c.required,
                    weight: c.weight,
                }).collect(),
                result: text,
                attempt,
            });
            let messages = match eval_action {
                EvalAction::Evaluate { messages } => messages,
                EvalAction::Done { .. } => break,
            };

            // Phase 2: SDK calls evaluator LLM
            let mut eval_text = String::new();
            let mut eval_stream = self.eval_provider.stream(&messages, &[], None).await?;
            while let Some(evt) = eval_stream.next().await {
                if let StreamEvent::TextDelta { delta } = evt? {
                    eval_text.push_str(&delta);
                }
            }

            // Phase 3: kernel parses verdict
            let done_action = pipeline.feed(EvalEvent::EvalResult { content: eval_text });
            let eval_result = match done_action {
                EvalAction::Done { result } => result,
                _ => break,
            };

            outcome.passed = eval_result.passed;
            outcome.overall_score = eval_result.overall_score;
            outcome.feedback = Some(eval_result.feedback.clone());
            outcome.details = eval_result.details.iter().map(|d| crate::harness::CriterionResult {
                criterion: d.criterion.clone(),
                passed: d.passed,
                score: d.score,
                feedback: d.feedback.clone(),
            }).collect();

            if eval_result.passed {
                if let Some(sc) = eval_result.skill_candidate {
                    if let Some(dir) = &self.skill_dir {
                        let mut fm = format!("---\nname: {}\ndescription: {}\n", sc.name, sc.description);
                        if let Some(wtu) = &sc.when_to_use {
                            fm.push_str(&format!("when_to_use: {}\n", wtu));
                        }
                        fm.push_str("---\n\n");
                        fm.push_str(&sc.content);
                        tokio::fs::write(dir.join(format!("{}.md", sc.name)), fm).await?;
                    }
                }
                return Ok(outcome);
            }

            // Inject feedback into next attempt
            current_goal = format!("{}\n\n[Previous attempt {} failed: {}]", request.goal, attempt, eval_result.feedback);
            pipeline.reset();
        }

        Ok(outcome)
    }
}

async fn collect_run(agent: &Agent, req: &HarnessRequest) -> Result<(String, u32, u64, String)> {
    collect_run_with_goal(agent, &req.goal, req).await
}

async fn collect_run_with_goal(agent: &Agent, goal: &str, req: &HarnessRequest) -> Result<(String, u32, u64, String)> {
    let mut text = String::new();
    let mut iterations = 0u32;
    let mut total_tokens = 0u64;
    let mut status = "error".to_string();
    let criteria_texts: Vec<String> = req.criteria.iter().map(|c| c.text.clone()).collect();
    let mut stream = agent.run_streaming(goal, &criteria_texts, req.extensions.as_ref()).await?;
    while let Some(evt) = stream.next().await {
        match evt? {
            RunEvent::TextDelta(d) => text.push_str(&d),
            RunEvent::Done { iterations: i, total_tokens: t, status: s } => {
                iterations = i; total_tokens = t; status = s;
            }
            _ => {}
        }
    }
    Ok((text, iterations, total_tokens, status))
}
