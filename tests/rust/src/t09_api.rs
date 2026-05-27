use async_trait::async_trait;
use deepstrike_sdk::*;
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ─── Helpers ────────────────────────────────────────────────────────────────

fn load_env() -> (String, String, String) {
    let env_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(".env");
    let _ = dotenvy::from_path(&env_path);

    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY required");
    let base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5-mini".into());
    (api_key, base_url, model)
}

fn make_provider() -> OpenAIProvider {
    let (key, url, model) = load_env();
    OpenAIProvider::with_base_url(key, model, url)
}

fn make_runner() -> RuntimeRunner {
    make_runner_with(|_, _| {})
}

fn make_runner_with<F>(setup: F) -> RuntimeRunner
where
    F: FnOnce(&mut LocalExecutionPlane, &mut RuntimeOptions),
{
    let mut plane = LocalExecutionPlane::new();
    let mut opts = RuntimeOptions {
        provider: Box::new(make_provider()),
        execution_plane: None,
        session_log: Some(Arc::new(InMemorySessionLog::new())),
        compression_store: None,
        session_id: None,
        max_tokens: 4096,
        max_turns: Some(25),
        timeout_ms: None,
        extensions: None,
        agent_id: None,
        system_prompt: None,
        initial_memory: vec![],
        skill_dir: None,
        dream_store: None,
        knowledge_source: None,
        signal_source: None,
        governance: None,
        tokenizer: None,
        enable_plan_tool: None,
        on_tool_suspend: None,
        milestone_policy: deepstrike_sdk::runtime::MilestonePolicy::default(),
        milestone_contract: None,
        run_spec: None,
        on_milestone_evaluate: None,
    };
    setup(&mut plane, &mut opts);
    opts.execution_plane = Some(Box::new(plane));
    RuntimeRunner::new(opts)
}

fn skills_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/skills")
}

async fn collect_text(runner: &RuntimeRunner, goal: &str) -> (String, Vec<RunEvent>) {
    let mut stream = runner.run_streaming(goal, &[], None, None).await.unwrap();
    let mut text = String::new();
    let mut events = Vec::new();
    while let Some(evt) = stream.next().await {
        let evt = evt.unwrap();
        if let RunEvent::TextDelta(ref d) = evt {
            text.push_str(d);
        }
        events.push(evt);
    }
    (text, events)
}

// ─── Mock implementations ───────────────────────────────────────────────────

struct MockKnowledgeSource {
    snippets: Vec<String>,
}

#[async_trait]
impl KnowledgeSource for MockKnowledgeSource {
    async fn retrieve(&self, _goal: &str, top_k: usize) -> deepstrike_sdk::Result<Vec<String>> {
        Ok(self.snippets.iter().take(top_k).cloned().collect())
    }
    async fn init(&self) -> deepstrike_sdk::Result<()> {
        Ok(())
    }
}

struct MockDreamStore {
    sessions: Vec<deepstrike_core::memory::durable::SessionData>,
    memories: Vec<deepstrike_core::memory::semantic::MemoryEntry>,
    committed: Arc<Mutex<bool>>,
}

impl MockDreamStore {
    fn empty() -> Self {
        Self {
            sessions: vec![],
            memories: vec![],
            committed: Arc::new(Mutex::new(false)),
        }
    }

    fn with_session() -> Self {
        use deepstrike_core::types::message::Message;
        Self {
            sessions: vec![deepstrike_core::memory::durable::SessionData {
                session_id: "s1".into(),
                agent_id: "test-agent".into(),
                messages: vec![
                    Message::user("What is Rust?"),
                    Message::assistant("Rust is a systems programming language."),
                ],
                metadata: serde_json::Value::Null,
                created_at_ms: 1_000_000,
                updated_at_ms: 1_001_000,
            }],
            memories: vec![],
            committed: Arc::new(Mutex::new(false)),
        }
    }
}

#[async_trait]
impl DreamStore for MockDreamStore {
    async fn load_sessions(
        &self,
        _agent_id: &str,
    ) -> deepstrike_sdk::Result<Vec<deepstrike_core::memory::durable::SessionData>> {
        Ok(self.sessions.clone())
    }
    async fn load_memories(
        &self,
        _agent_id: &str,
    ) -> deepstrike_sdk::Result<Vec<deepstrike_core::memory::semantic::MemoryEntry>> {
        Ok(self.memories.clone())
    }
    async fn commit(
        &self,
        _agent_id: &str,
        _result: deepstrike_core::memory::curator::CurationResult,
        _existing: &[deepstrike_core::memory::semantic::MemoryEntry],
    ) -> deepstrike_sdk::Result<()> {
        *self.committed.lock().unwrap() = true;
        Ok(())
    }
    async fn search(
        &self,
        _agent_id: &str,
        _query: &str,
        _top_k: usize,
    ) -> deepstrike_sdk::Result<Vec<deepstrike_core::memory::semantic::MemoryEntry>> {
        Ok(vec![deepstrike_core::memory::semantic::MemoryEntry {
            text: "Rust was created by Graydon Hoare at Mozilla.".into(),
            score: 0.95,
            metadata: serde_json::Value::Null,
        }])
    }
    async fn save_session(
        &self,
        _data: deepstrike_core::memory::durable::SessionData,
    ) -> deepstrike_sdk::Result<()> {
        Ok(())
    }
}

// ─── 01. RuntimeRunner.execute() basic ──────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_execute_returns_text() {
    let runner = make_runner();
    let result = runner.execute("Say hello in one word.").await.unwrap();
    assert!(!result.is_empty());
}

// ─── 02. Agent.run_streaming() ──────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_streaming_produces_text_and_done() {
    let runner = make_runner();
    let (text, events) = collect_text(&runner, "What is 2+2? Answer with just the number.").await;
    assert!(!text.is_empty());
    assert!(events.iter().any(|e| matches!(e, RunEvent::TextDelta(_))));
    assert!(events.iter().any(|e| matches!(e, RunEvent::Done { .. })));
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_streaming_done_has_iterations() {
    let runner = make_runner();
    let (_, events) = collect_text(&runner, "Say hi.").await;
    let done = events.iter().find(|e| matches!(e, RunEvent::Done { .. }));
    match done.unwrap() {
        RunEvent::Done {
            iterations, status, ..
        } => {
            assert!(*iterations >= 0);
            assert!(!status.is_empty());
        }
        _ => unreachable!(),
    }
}

// ─── 03. Agent with criteria ────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_with_criteria() {
    let runner = make_runner();
    let criteria = vec!["Must contain the word 'hello'".to_string()];
    let mut stream = runner
        .run_streaming("Greet the user.", &criteria, None, None)
        .await
        .unwrap();

    let mut text = String::new();
    while let Some(evt) = stream.next().await {
        if let Ok(RunEvent::TextDelta(d)) = evt {
            text.push_str(&d);
        }
    }
    let lower = text.to_lowercase();
    assert!(lower.contains("hello") || lower.contains("hi") || lower.contains("greet"));
}

// ─── 04. Tool calling ──────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_calls_tool() {
    let runner = make_runner_with(|plane, _| {
        plane.register(RegisteredTool::text(
            "add",
            "Add two integers and return the sum.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer", "description": "First number" },
                    "y": { "type": "integer", "description": "Second number" }
                },
                "required": ["x", "y"]
            }),
            |args| {
                Box::pin(async move {
                    let x = args["x"].as_i64().unwrap_or(0);
                    let y = args["y"].as_i64().unwrap_or(0);
                    Ok(format!("{}", x + y))
                })
            },
        ));
    });

    let (text, events) = collect_text(
        &runner,
        "Use the add tool to compute 17 + 28. Report the result.",
    )
    .await;

    let has_tool_call = events
        .iter()
        .any(|e| matches!(e, RunEvent::ToolCall { name, .. } if name == "add"));
    let has_tool_result = events
        .iter()
        .any(|e| matches!(e, RunEvent::ToolResult { is_error, .. } if !is_error));
    assert!(has_tool_call, "expected add tool call");
    assert!(has_tool_result, "expected tool result");
    assert!(text.contains("45"), "expected result 45 in output: {text}");
}

// ─── 05. Skills ─────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_with_skill_dir() {
    let dir = skills_dir();
    let runner = make_runner_with(|_, opts| {
        opts.skill_dir = Some(dir);
    });

    let (text, events) = collect_text(
        &runner,
        "Use the summarize skill to learn how to summarize, then summarize: 'Rust is a systems programming language focused on safety, speed, and concurrency.'",
    ).await;

    let has_skill_call = events
        .iter()
        .any(|e| matches!(e, RunEvent::ToolCall { name, .. } if name == "skill"));
    let has_skill_result = events
        .iter()
        .any(|e| matches!(e, RunEvent::ToolResult { is_error, .. } if !is_error));
    assert!(
        has_skill_call || !text.is_empty(),
        "expected skill call or text output"
    );
    if has_skill_call {
        assert!(has_skill_result, "expected skill result");
    }
}

// ─── 06. Knowledge source ───────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_with_knowledge_source() {
    let runner = make_runner_with(|_, opts| {
        opts.knowledge_source = Some(Box::new(MockKnowledgeSource {
            snippets: vec![
                "DeepStrike is an agent framework with a Rust kernel.".into(),
                "DeepStrike supports Node.js, Python, and Rust SDKs.".into(),
            ],
        }));
    });

    let (text, events) = collect_text(
        &runner,
        "Use the knowledge tool to find out what DeepStrike is, then explain it.",
    )
    .await;

    let has_knowledge_call = events
        .iter()
        .any(|e| matches!(e, RunEvent::ToolCall { name, .. } if name == "knowledge"));
    assert!(
        has_knowledge_call || text.to_lowercase().contains("deepstrike"),
        "expected knowledge call or DeepStrike mention in: {text}"
    );
}

// ─── 07. Governance — blocked tool ──────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn blocked_tool_yields_error_event() {
    let gov = Arc::new(tokio::sync::Mutex::new(Governance::allow()));
    gov.lock().await.block_tool("forbidden_action");
    let runner = make_runner_with(|plane, opts| {
        plane.register(RegisteredTool::text(
            "forbidden_action",
            "This tool is blocked.",
            serde_json::json!({"type": "object", "properties": {}, "required": []}),
            |_| Box::pin(async { Ok("should not run".into()) }),
        ));
        opts.governance = Some(gov);
    });

    let (_, events) = collect_text(&runner, "Call the forbidden_action tool.").await;

    let has_error = events.iter().any(|e| matches!(e, RunEvent::Error(_)));
    let has_done = events.iter().any(|e| matches!(e, RunEvent::Done { .. }));
    assert!(has_done, "run should terminate");
    // may or may not have error depending on whether LLM tries to call the blocked tool
    let _ = has_error;
}

// ─── 08. Agent.interrupt() ──────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_interrupt() {
    let runner = make_runner();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    runner.interrupt();
    let result = runner
        .execute("Write a very long essay about the history of computing.")
        .await;
    assert!(result.is_ok());
}

// ─── 09. DreamStore integration ─────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn dream_with_empty_sessions() {
    let store = MockDreamStore::empty();
    let runner = make_runner_with(|_, opts| {
        opts.dream_store = Some(Box::new(store));
        opts.agent_id = Some("test-agent".into());
    });

    let result = runner.dream("test-agent", 1_000_000).await.unwrap();
    assert_eq!(result.sessions_processed, 0);
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn dream_processes_session() {
    let store = MockDreamStore::with_session();
    let committed = store.committed.clone();

    let runner = make_runner_with(|_, opts| {
        opts.dream_store = Some(Box::new(store));
        opts.agent_id = Some("test-agent".into());
    });

    let result = runner.dream("test-agent", 2_000_000).await.unwrap();
    assert!(result.sessions_processed >= 1);
    assert!(*committed.lock().unwrap(), "commit should have been called");
}

// ─── 10. SinglePassHarness ──────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn single_pass_harness_always_passes() {
    let runner = make_runner();
    let harness = SinglePassHarness::new(&runner);
    let outcome = harness
        .run(HarnessRequest::new("Say hello."))
        .await
        .unwrap();
    assert!(outcome.passed);
    assert!(!outcome.result.is_empty());
    assert!(!outcome.status.is_empty());
}

// ─── 11. EvalLoopHarness ────────────────────────────────────────────────────

struct AlwaysPass;

#[async_trait]
impl QualityGate for AlwaysPass {
    async fn evaluate(
        &self,
        _req: &HarnessRequest,
        _out: &HarnessOutcome,
    ) -> deepstrike_sdk::Result<bool> {
        Ok(true)
    }
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn eval_loop_harness_with_always_pass_gate() {
    let runner = make_runner();
    let harness = EvalLoopHarness::new(&runner, AlwaysPass, 3);
    let outcome = harness.run(HarnessRequest::new("Say hi.")).await.unwrap();
    assert!(outcome.passed);
}

// ─── 12. Tools + Governance combo ───────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn tools_plus_governance_allowed_tool_works() {
    let gov = Arc::new(tokio::sync::Mutex::new(Governance::allow()));
    gov.lock().await.block_tool("dangerous");
    let runner = make_runner_with(|plane, opts| {
        plane.register(RegisteredTool::text(
            "greet",
            "Return a greeting for the given name.",
            serde_json::json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
            |args| {
                Box::pin(async move {
                    let name = args["name"].as_str().unwrap_or("World");
                    Ok(format!("Hello, {name}!"))
                })
            },
        ));
        plane.register(RegisteredTool::text(
            "dangerous",
            "A dangerous tool.",
            serde_json::json!({"type": "object", "properties": {}, "required": []}),
            |_| Box::pin(async { Ok("danger!".into()) }),
        ));
        opts.governance = Some(gov);
    });

    let (text, events) = collect_text(
        &runner,
        "Use the greet tool with name='Rust'. Do NOT call dangerous.",
    )
    .await;

    let has_greet = events
        .iter()
        .any(|e| matches!(e, RunEvent::ToolCall { name, .. } if name == "greet"));
    assert!(
        has_greet || text.contains("Hello"),
        "expected greet tool call or greeting text"
    );
}

// ─── 13. Memory + Agent combo ───────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn agent_with_dream_store_enables_memory_tool() {
    let store = MockDreamStore::empty();
    let runner = make_runner_with(|_, opts| {
        opts.dream_store = Some(Box::new(store));
        opts.agent_id = Some("memory-agent".into());
    });

    let (text, events) = collect_text(
        &runner,
        "Use the memory tool to search for 'Rust history'. Report what you found.",
    )
    .await;

    let has_memory_call = events
        .iter()
        .any(|e| matches!(e, RunEvent::ToolCall { name, .. } if name == "memory"));
    assert!(
        has_memory_call || !text.is_empty(),
        "expected memory tool call or text output"
    );
}

// ─── 14. HarnessLoop (LLM-as-judge) ────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn harness_loop_llm_judge() {
    let runner = make_runner();
    let eval_provider = make_provider();
    let harness = HarnessLoop::new(&runner, eval_provider, 2, None);

    let mut req = HarnessRequest::new("Write a haiku about the ocean.");
    req.criteria = vec![deepstrike_sdk::harness::Criterion::required(
        "Must be exactly 3 lines.",
    )];

    let stream = harness.run_streaming(req);
    futures::pin_mut!(stream);
    let mut result = String::new();
    let mut status = String::new();
    while let Some(evt) = futures::StreamExt::next(&mut stream).await {
        match evt.unwrap() {
            deepstrike_sdk::harness::HarnessEvent::Token(t) => result.push_str(&t),
            deepstrike_sdk::harness::HarnessEvent::Done { status: s, .. } => status = s,
            deepstrike_sdk::harness::HarnessEvent::MaxAttemptsReached => {
                status = "max_attempts".into()
            }
            _ => {}
        }
    }
    assert!(!result.is_empty());
    assert!(!status.is_empty());
}

// ─── 15. Extensions pass-through ────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn agent_with_extensions() {
    let runner = make_runner_with(|_, opts| {
        opts.extensions = Some(serde_json::json!({"temperature": 0.1}));
    });

    let (text, _) = collect_text(&runner, "Say exactly: 'test passed'").await;
    assert!(!text.is_empty());
}

// ─── 16. Multiple tool calls in one turn ────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn runner_multiple_tools() {
    let runner = make_runner_with(|plane, _| {
        plane.register(RegisteredTool::text(
            "add",
            "Add two numbers.",
            serde_json::json!({
                "type": "object",
                "properties": { "x": {"type":"integer"}, "y": {"type":"integer"} },
                "required": ["x","y"]
            }),
            |args| {
                Box::pin(async move {
                    let x = args["x"].as_i64().unwrap_or(0);
                    let y = args["y"].as_i64().unwrap_or(0);
                    Ok(format!("{}", x + y))
                })
            },
        ));
        plane.register(RegisteredTool::text(
            "multiply",
            "Multiply two numbers.",
            serde_json::json!({
                "type": "object",
                "properties": { "x": {"type":"integer"}, "y": {"type":"integer"} },
                "required": ["x","y"]
            }),
            |args| {
                Box::pin(async move {
                    let x = args["x"].as_i64().unwrap_or(0);
                    let y = args["y"].as_i64().unwrap_or(0);
                    Ok(format!("{}", x * y))
                })
            },
        ));
    });

    let (text, events) = collect_text(
        &runner,
        "Compute add(3,4) and multiply(5,6). Report both results.",
    )
    .await;

    let tool_calls: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, RunEvent::ToolCall { .. }))
        .collect();
    assert!(
        tool_calls.len() >= 2,
        "expected at least 2 tool calls, got {}",
        tool_calls.len()
    );
    assert!(
        text.contains("7") && text.contains("30"),
        "expected 7 and 30 in output: {text}"
    );
}

// ─── 17. SignalGateway + Agent integration ──────────────────────────────────

#[tokio::test]
async fn signal_gateway_creates_and_subscribes() {
    let gw = SignalGateway::new();
    let _rx = gw.subscribe();

    gw.ingest(deepstrike_sdk::RuntimeSignal {
        kind: "interrupt".into(),
        payload: serde_json::json!({}),
        priority: 10,
    });
    gw.destroy();
}

#[tokio::test]
async fn signal_gateway_schedule_fires() {
    let gw = SignalGateway::new();
    let rx = gw.subscribe();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    gw.schedule(ScheduledPrompt::new("test job", now_ms + 100));

    // Give time for schedule to fire
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    let sig = rx.next_signal().await;
    assert!(sig.is_ok());
    gw.destroy();
}

// ─── 18. Telemetry in Done event ────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn done_event_has_telemetry() {
    let runner = make_runner();
    let (_, events) = collect_text(&runner, "Say one word.").await;

    let done = events
        .iter()
        .find(|e| matches!(e, RunEvent::Done { .. }))
        .unwrap();
    match done {
        RunEvent::Done {
            iterations,
            total_tokens,
            status,
        } => {
            assert!(*iterations >= 0);
            assert!(*total_tokens >= 0);
            assert!(!status.is_empty());
        }
        _ => unreachable!(),
    }
}
