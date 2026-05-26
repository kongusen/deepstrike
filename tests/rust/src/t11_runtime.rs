use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use compact_str::CompactString;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::governance::permission::PermissionAction;
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::scheduler::state_machine::{LoopObservation, LoopStateMachine};
use deepstrike_core::types::capability::{CapabilityDescriptor, CapabilityKind};
use deepstrike_core::types::message::{Content, Message, Role, ToolCall, ToolResult, ToolSchema};
use deepstrike_sdk::ExecutionPlane;
use deepstrike_sdk::{
    Governance, InMemorySessionLog, LLMProvider, LocalExecutionPlane, RegisteredTool, RunContext,
    RunEvent, RuntimeOptions, RuntimeRunner, SessionLog, StreamEvent,
};
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
    ) -> deepstrike_sdk::Result<
        Box<dyn futures::Stream<Item = deepstrike_sdk::Result<StreamEvent>> + Send + Unpin>,
    > {
        self.stream_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        compression_store: None,
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
        tokenizer: None,
        enable_plan_tool: None,
        on_tool_suspend: None,
        milestone_policy: deepstrike_sdk::runtime::MilestonePolicy::AutoPass,
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
    let mut saw_denied_event = false;
    let mut saw_denied_result = false;
    while let Some(evt) = stream.next().await {
        match evt.unwrap() {
            RunEvent::ToolDenied { .. } => saw_denied_event = true,
            RunEvent::ToolResult { is_error: true, .. } => saw_denied_result = true,
            _ => {}
        }
    }
    assert!(saw_denied_event, "expected ToolDenied event from governance");
    assert!(saw_denied_result, "expected error ToolResult from governance denial");
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
                provider_replay: None,
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
                    is_fatal: false,
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
            Ok(Box::new(futures::stream::iter(vec![Ok(
                StreamEvent::TextDelta { delta: "ok".into() },
            )])))
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

#[tokio::test]
async fn reactive_compact_on_413_retry() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TooLongThenOkProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LLMProvider for TooLongThenOkProvider {
        async fn stream(
            &self,
            _context: &RenderedContext,
            _tools: &[ToolSchema],
            _extensions: Option<&serde_json::Value>,
            _state: Option<&deepstrike_sdk::ProviderRunState>,
        ) -> deepstrike_sdk::Result<
            Box<dyn futures::Stream<Item = deepstrike_sdk::Result<StreamEvent>> + Send + Unpin>,
        > {
            let count = self.calls.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                return Err(deepstrike_sdk::Error::Provider(
                    "413 prompt too long".to_string(),
                ));
            }
            Ok(Box::new(futures::stream::iter(vec![Ok(
                StreamEvent::TextDelta { delta: "recovered".into() },
            )])))
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "test-session".to_string();

    session_log.append(&session_id, SessionEvent::RunStarted {
        run_id: "r1".to_string(),
        goal: "hi".to_string(),
        criteria: vec![],
        agent_id: None,
        system_prompt: None,
    }).await.unwrap();

    for i in 0..3 {
        session_log.append(&session_id, SessionEvent::LlmCompleted {
            turn: i * 2,
            message: Message::assistant("a".repeat(200)),
            provider_replay: None,
        }).await.unwrap();

        session_log.append(&session_id, SessionEvent::LlmCompleted {
            turn: i * 2 + 1,
            message: Message::user("q".repeat(200)),
            provider_replay: None,
        }).await.unwrap();
    }

    let mut opts = default_runtime_opts(
        Box::new(TooLongThenOkProvider { calls: calls.clone() }),
        LocalExecutionPlane::new(),
        session_log.clone(),
    );
    opts.session_id = Some(session_id.clone());
    let runner = RuntimeRunner::new(opts);

    let res = runner.execute("hi").await;
    println!("=== RUNNER EXECUTE RESULT: {:?} ===", res);
    let text = res.unwrap();
    assert_eq!(text, "recovered");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let events = session_log.read(&session_id, 0).await.unwrap();
    assert!(events.iter().any(|e| matches!(e.event, SessionEvent::Compressed { .. })));
}

// ─── Milestone contract: SM-level cascade → SessionEvent chain ───────────

/// Verifies the full milestone cascade chain:
/// `LoopStateMachine::load_milestone_contract` → `EvaluateMilestone` action →
/// `MilestoneResult` feed → `LoopObservation::MilestoneAdvanced` →
/// `SessionEvent::MilestoneAdvanced` written to audit log.
#[tokio::test]
async fn milestone_pass_writes_session_event() {
    use deepstrike_core::types::milestone::{MilestoneCheckResult, MilestoneContract, MilestonePhase};

    let mut sm = LoopStateMachine::new(LoopPolicy {
        max_tokens: 128_000,
        ..LoopPolicy::default()
    });

    let contract = MilestoneContract::new()
        .phase(MilestonePhase::new("plan").with_criterion("Plan is complete"))
        .phase(MilestonePhase::new("implement"));
    sm.load_milestone_contract(contract);

    use deepstrike_core::types::message::Role;
    use deepstrike_core::types::task::RuntimeTask;
    use deepstrike_core::scheduler::state_machine::{LoopAction, LoopEvent};

    sm.start(RuntimeTask::new("build feature"));

    // LLM produces text-only → SM requests milestone evaluation
    let action = sm.feed(LoopEvent::LLMResponse {
        message: deepstrike_core::types::message::Message::assistant("plan complete"),
    });
    assert!(
        matches!(action, LoopAction::EvaluateMilestone { ref phase_id, .. } if phase_id == "plan"),
        "expected EvaluateMilestone for 'plan'",
    );

    // Advance with a pass result
    sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::pass("plan"),
    });

    let observations = sm.take_observations();
    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "milestone-chain-test";

    for obs in &observations {
        if let LoopObservation::MilestoneAdvanced { turn, phase_id, capabilities_unlocked } = obs {
            session_log
                .append(
                    session_id,
                    SessionEvent::MilestoneAdvanced {
                        turn: *turn,
                        phase_id: phase_id.clone(),
                        capabilities_unlocked: capabilities_unlocked.clone(),
                    },
                )
                .await
                .unwrap();
        }
    }

    let entries = session_log.read(session_id, 0).await.unwrap();
    assert!(
        entries.iter().any(|e| matches!(&e.event,
            SessionEvent::MilestoneAdvanced { phase_id, .. } if phase_id == "plan"
        )),
        "SessionEvent::MilestoneAdvanced for 'plan' must be in audit log",
    );
}

#[tokio::test]
async fn milestone_block_writes_session_event() {
    use deepstrike_core::types::milestone::{MilestoneCheckResult, MilestoneContract, MilestonePhase};
    use deepstrike_core::types::task::RuntimeTask;
    use deepstrike_core::scheduler::state_machine::{LoopAction, LoopEvent};

    let mut sm = LoopStateMachine::new(LoopPolicy {
        max_tokens: 128_000,
        ..LoopPolicy::default()
    });

    sm.load_milestone_contract(
        MilestoneContract::new().phase(MilestonePhase::new("verify")),
    );
    sm.start(RuntimeTask::new("verify task"));

    sm.feed(LoopEvent::LLMResponse {
        message: deepstrike_core::types::message::Message::assistant("not done"),
    });

    sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::fail("verify", "missing test coverage"),
    });

    let observations = sm.take_observations();
    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "milestone-block-test";

    for obs in &observations {
        if let LoopObservation::MilestoneBlocked { turn, phase_id, reason } = obs {
            session_log
                .append(
                    session_id,
                    SessionEvent::MilestoneBlocked {
                        turn: *turn,
                        phase_id: phase_id.clone(),
                        reason: reason.clone(),
                    },
                )
                .await
                .unwrap();
        }
    }

    let entries = session_log.read(session_id, 0).await.unwrap();
    assert!(
        entries.iter().any(|e| matches!(&e.event,
            SessionEvent::MilestoneBlocked { phase_id, reason, .. }
            if phase_id == "verify" && reason.contains("missing test coverage")
        )),
        "SessionEvent::MilestoneBlocked must be in audit log",
    );
}

// ─── CapabilityManifest: mount → observation → SessionEvent chain ─────────

/// Verifies the full chain:
/// `LoopStateMachine::mount_capability` emits `LoopObservation::CapabilityChanged`,
/// which the runner converts to `SessionEvent::CapabilityChanged` in the audit log.
/// The test simulates the runner's observation-processing step inline so no real
/// LLM provider is needed.
#[tokio::test]
async fn capability_mount_emits_capability_changed_session_event() {
    let mut sm = LoopStateMachine::new(LoopPolicy {
        max_tokens: 128_000,
        ..LoopPolicy::default()
    });

    let schema = ToolSchema {
        name: CompactString::new("my_tool"),
        description: "A dynamically mounted test tool".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    };
    sm.mount_capability(CapabilityDescriptor::tool(schema));

    let observations = sm.take_observations();
    assert_eq!(observations.len(), 1, "expected exactly one observation after mount");

    // Simulate what RuntimeRunner does: convert each observation to a SessionEvent.
    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "cap-mount-test";
    let mut saw_capability_changed = false;

    for obs in &observations {
        if let LoopObservation::CapabilityChanged { turn, added, removed } = obs {
            session_log
                .append(
                    session_id,
                    SessionEvent::CapabilityChanged {
                        turn: *turn,
                        added: added.clone(),
                        removed: removed.clone(),
                    },
                )
                .await
                .unwrap();

            assert_eq!(added.len(), 1, "mount should produce one added entry");
            assert!(
                added[0].contains("my_tool"),
                "added entry should identify the tool: {added:?}",
            );
            assert!(removed.is_empty(), "mount must not produce removed entries");
            saw_capability_changed = true;
        }
    }
    assert!(saw_capability_changed, "no CapabilityChanged observation emitted");

    // Confirm the event was written to the session log.
    let entries = session_log.read(session_id, 0).await.unwrap();
    assert!(
        entries.iter().any(|e| matches!(e.event, SessionEvent::CapabilityChanged { .. })),
        "SessionEvent::CapabilityChanged must be present in audit log after mount",
    );
}

#[tokio::test]
async fn capability_unmount_emits_capability_changed_session_event() {
    let mut sm = LoopStateMachine::new(LoopPolicy {
        max_tokens: 128_000,
        ..LoopPolicy::default()
    });

    // Mount first so there is something to remove.
    let schema = ToolSchema {
        name: CompactString::new("ephemeral_tool"),
        description: "Tool to be removed".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    };
    sm.mount_capability(CapabilityDescriptor::tool(schema));
    sm.take_observations(); // clear mount observation

    sm.unmount_capability(CapabilityKind::Tool, "ephemeral_tool");

    let observations = sm.take_observations();
    assert_eq!(observations.len(), 1, "expected exactly one observation after unmount");

    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "cap-unmount-test";

    for obs in &observations {
        if let LoopObservation::CapabilityChanged { turn, added, removed } = obs {
            session_log
                .append(
                    session_id,
                    SessionEvent::CapabilityChanged {
                        turn: *turn,
                        added: added.clone(),
                        removed: removed.clone(),
                    },
                )
                .await
                .unwrap();

            assert!(added.is_empty(), "unmount must not produce added entries");
            assert_eq!(removed.len(), 1, "unmount should produce one removed entry");
            assert!(
                removed[0].contains("ephemeral_tool"),
                "removed entry should identify the tool: {removed:?}",
            );
        }
    }

    let entries = session_log.read(session_id, 0).await.unwrap();
    assert!(
        entries.iter().any(|e| matches!(e.event, SessionEvent::CapabilityChanged { .. })),
        "SessionEvent::CapabilityChanged must be present in audit log after unmount",
    );
}
