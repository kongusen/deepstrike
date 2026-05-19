use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use compact_str::CompactString;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::governance::permission::PermissionAction;
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::types::message::{Content, Message, Role, ToolCall, ToolResult, ToolSchema};
use deepstrike_sdk::{
    Governance, InMemorySessionLog, LocalExecutionPlane, LLMProvider, RegisteredTool, RunContext,
    RuntimeOptions, RuntimeRunner, RunEvent, SessionLog, StreamEvent,
};
use deepstrike_sdk::ExecutionPlane;
use futures::StreamExt;

struct ResumeAwareProvider {
    stream_calls: std::sync::atomic::AtomicU32,
}

impl ResumeAwareProvider {
    fn new() -> Self {
        Self {
            stream_calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.stream_calls.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait]
impl LLMProvider for ResumeAwareProvider {
    async fn stream(
        &self,
        context: &RenderedContext,
        _tools: &[ToolSchema],
        _extensions: Option<&serde_json::Value>,
        _state: Option<&deepstrike_sdk::ProviderRunState>,
    ) -> deepstrike_sdk::Result<Box<dyn futures::Stream<Item = deepstrike_sdk::Result<StreamEvent>> + Send + Unpin>>
    {
        self.stream_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let has_tool = context.turns.iter().any(|m| m.role == Role::Tool);
        let events: Vec<_> = if has_tool {
            vec![Ok(StreamEvent::TextDelta {
                delta: "finished".into(),
            })]
        } else {
            vec![Ok(StreamEvent::ToolCall {
                id: "call_ping".into(),
                name: "ping".into(),
                arguments: serde_json::json!({}),
            })]
        };
        Ok(Box::new(futures::stream::iter(events)))
    }
}

fn default_runtime_opts(
    provider: Box<dyn LLMProvider>,
    plane: LocalExecutionPlane,
    session_log: Arc<InMemorySessionLog>,
) -> RuntimeOptions {
    RuntimeOptions {
        provider,
        execution_plane: Some(Box::new(plane)),
        session_log: Some(session_log),
        session_id: None,
        max_tokens: 2048,
        max_turns: Some(4),
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
        on_tool_suspend: None,
    }
}

#[tokio::test]
async fn governance_denies_tool_on_plane() {
    let mut plane = LocalExecutionPlane::new();
    plane.register(RegisteredTool::text(
        "secret",
        "Secret tool",
        serde_json::json!({"type": "object", "properties": {}}),
        |_| Box::pin(async { Ok("leaked".into()) }),
    ));

    let mut gov = Governance::allow();
    gov.add_permission_rule("secret", PermissionAction::Deny);

    let call = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("secret"),
        arguments: serde_json::json!({}),
    };

    let ctx = RunContext {
        agent_id: None,
        skill_dir: None,
        dream_store: None,
        knowledge_source: None,
        governance: Some(Arc::new(Mutex::new(gov))),
        on_tool_suspend: None,
    };

    let calls = [call];
    let mut stream = plane.execute_all(&calls, ctx);
    let mut saw_error = false;
    let mut saw_denied_result = false;
    while let Some(evt) = stream.next().await {
        match evt.unwrap() {
            RunEvent::Error(_) => saw_error = true,
            RunEvent::ToolResult { is_error: true, .. } => saw_denied_result = true,
            _ => {}
        }
    }
    assert!(saw_error);
    assert!(saw_denied_result);
}

#[tokio::test]
async fn wake_continues_after_tool_completed() {
    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "crash-test";

    session_log
        .append(
            session_id,
            SessionEvent::RunStarted {
                run_id: "r1".into(),
                goal: "use ping".into(),
                criteria: vec![],
                agent_id: None,
                system_prompt: None,
            },
        )
        .await
        .unwrap();

    session_log
        .append(
            session_id,
            SessionEvent::LlmCompleted {
                turn: 0,
                message: Message {
                    role: Role::Assistant,
                    content: Content::Text(String::new()),
                    tool_calls: vec![ToolCall {
                        id: CompactString::new("call_ping"),
                        name: CompactString::new("ping"),
                        arguments: serde_json::json!({}),
                    }],
                    token_count: None,
                },
            },
        )
        .await
        .unwrap();

    session_log
        .append(
            session_id,
            SessionEvent::ToolCompleted {
                turn: 0,
                results: vec![ToolResult {
                    call_id: CompactString::new("call_ping"),
                    output: Content::Text("pong".into()),
                    is_error: false,
                    token_count: None,
                }],
            },
        )
        .await
        .unwrap();

    let mut plane = LocalExecutionPlane::new();
    plane.register(RegisteredTool::text(
        "ping",
        "Ping",
        serde_json::json!({"type": "object", "properties": {}}),
        |_| Box::pin(async { Ok("pong".into()) }),
    ));

    let provider = ResumeAwareProvider::new();
    let runner = RuntimeRunner::new(default_runtime_opts(
        Box::new(provider),
        plane,
        session_log.clone(),
    ));

    let text = runner.wake(session_id).await.unwrap();
    assert_eq!(text, "finished");

    let entries = session_log.read(session_id, 0).await.unwrap();
    assert!(
        entries
            .iter()
            .any(|e| matches!(e.event, SessionEvent::RunTerminal { .. }))
    );
}

#[tokio::test]
async fn run_streaming_emits_text_and_done() {
    struct TextProvider;

    #[async_trait]
    impl LLMProvider for TextProvider {
        async fn stream(
            &self,
            _context: &RenderedContext,
            _tools: &[ToolSchema],
            _extensions: Option<&serde_json::Value>,
            _state: Option<&deepstrike_sdk::ProviderRunState>,
        ) -> deepstrike_sdk::Result<
            Box<dyn futures::Stream<Item = deepstrike_sdk::Result<StreamEvent>> + Send + Unpin>,
        > {
            Ok(Box::new(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta {
                    delta: "hello".into(),
                }),
                Ok(StreamEvent::Done),
            ])))
        }
    }

    let runner = RuntimeRunner::new(default_runtime_opts(
        Box::new(TextProvider),
        LocalExecutionPlane::new(),
        Arc::new(InMemorySessionLog::new()),
    ));

    let mut stream = runner
        .run_streaming("hi", &[], None, Some("s1"))
        .await
        .unwrap();

    let mut text = String::new();
    let mut saw_done = false;
    while let Some(evt) = stream.next().await {
        match evt.unwrap() {
            RunEvent::TextDelta(d) => text.push_str(&d),
            RunEvent::Done { .. } => saw_done = true,
            _ => {}
        }
    }
    assert_eq!(text, "hello");
    assert!(saw_done);
}

#[tokio::test]
async fn execute_collects_text_via_helper() {
    struct TextProvider;

    #[async_trait]
    impl LLMProvider for TextProvider {
        async fn stream(
            &self,
            _context: &RenderedContext,
            _tools: &[ToolSchema],
            _extensions: Option<&serde_json::Value>,
            _state: Option<&deepstrike_sdk::ProviderRunState>,
        ) -> deepstrike_sdk::Result<
            Box<dyn futures::Stream<Item = deepstrike_sdk::Result<StreamEvent>> + Send + Unpin>,
        > {
            Ok(Box::new(futures::stream::iter(vec![Ok(StreamEvent::TextDelta {
                delta: "ok".into(),
            })])))
        }
    }

    let runner = RuntimeRunner::new(default_runtime_opts(
        Box::new(TextProvider),
        LocalExecutionPlane::new(),
        Arc::new(InMemorySessionLog::new()),
    ));

    let text = runner.execute("ping").await.unwrap();
    assert_eq!(text, "ok");
}
