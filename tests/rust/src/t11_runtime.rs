#![allow(deprecated)]

use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use compact_str::CompactString;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::governance::permission::PermissionAction;
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::runtime::{KernelEffect, KernelInput, KernelInputEvent, KernelRuntime};
use deepstrike_core::runtime::kernel::KernelReliabilityConfig;
use deepstrike_core::scheduler::policy::SchedulerBudget;
use deepstrike_core::scheduler::state_machine::{KernelObservation, LoopStateMachine};
use deepstrike_core::types::capability::{CapabilityDescriptor, CapabilityKind};
use deepstrike_core::types::message::{Content, Message, Role, ToolCall, ToolResult, ToolSchema};
use deepstrike_sdk::ExecutionPlane;
use deepstrike_sdk::{
    Governance, InMemorySessionLog, LLMProvider, LocalExecutionPlane, PermissionResponse,
    RegisteredTool, RunContext, RunEvent, RuntimeOptions, RuntimeRunner, SessionLog, StreamEvent,
};
use futures::StreamExt;

#[test]
fn kernel_effect_result_retry_keeps_the_same_next_action() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let start = runtime.step(KernelInput::correlated(
        "op-host-retry",
        "event-start",
        1,
        KernelInputEvent::StartRun {
            task: deepstrike_core::types::task::RuntimeTask::new("call ping"),
            run_spec: None,
        },
    ));
    let mut response = Message::assistant("");
    response.tool_calls.push(ToolCall {
        id: "call-ping".into(),
        name: "ping".into(),
        arguments: serde_json::json!({}),
    });
    let effect_id = start.actions[0].effect_id.clone();

    let first = runtime.step(KernelInput::correlated(
        "op-host-retry",
        "event-provider-result-1",
        2,
        KernelInputEvent::ProviderResult {
            effect_id: effect_id.clone(),
            message: response.clone(),
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    ));
    let retry = runtime.step(KernelInput::correlated(
        "op-host-retry",
        "event-provider-result-2",
        3,
        KernelInputEvent::ProviderResult {
            effect_id,
            message: response,
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    ));

    assert!(matches!(
        first.actions.as_slice(),
        [action] if matches!(action.effect, KernelEffect::ExecuteTool { .. })
    ));
    assert_eq!(
        serde_json::to_value(retry).unwrap(),
        serde_json::to_value(first).unwrap(),
    );
}

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
        spool_dir: None,
        kernel_reliability: None,
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
        os_profile: None,
        governance_policy: None,
        attention_policy: None,
        scheduler_budget: None,
        resource_quota: None,
        memory_policy: None,
        tokenizer: None,
        enable_plan_tool: None,
        on_tool_suspend: None,
        on_permission_request: None,
        milestone_policy: deepstrike_sdk::runtime::MilestonePolicy::AutoPass,
        milestone_contract: None,
        run_spec: None,
        on_milestone_evaluate: None,
        allowed_tool_ids: None,
        on_turn_metrics: None,
        stable_core_tool_ids: Vec::new(),
        pre_query_memory: None,
    }
}

struct FinalTextProvider;

#[async_trait]
impl LLMProvider for FinalTextProvider {
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

#[tokio::test]
async fn runtime_applies_bounded_kernel_reliability_config() {
    let mut opts = default_runtime_opts(
        Box::new(FinalTextProvider),
        LocalExecutionPlane::new(),
        Arc::new(InMemorySessionLog::new()),
    );
    opts.kernel_reliability = Some(KernelReliabilityConfig {
        event_replay_capacity: Some(512),
        completed_effect_replay_capacity: Some(256),
        provider_recovery_attempts: Some(2),
        output_recovery_attempts: Some(2),
        host_effect_retry_attempts: Some(4),
        spool_threshold_bytes: Some(2048),
        spool_preview_bytes: Some(256),
        snapshot_input_limit: Some(4096),
    });

    assert_eq!(RuntimeRunner::new(opts).execute("configured").await.unwrap(), "ok");
}

#[tokio::test]
async fn runtime_surfaces_invalid_kernel_reliability_config() {
    let mut opts = default_runtime_opts(
        Box::new(FinalTextProvider),
        LocalExecutionPlane::new(),
        Arc::new(InMemorySessionLog::new()),
    );
    opts.kernel_reliability = Some(KernelReliabilityConfig {
        event_replay_capacity: Some(0),
        ..KernelReliabilityConfig::default()
    });

    let error = RuntimeRunner::new(opts).execute("invalid").await.unwrap_err();
    assert!(error.to_string().contains("InvalidConfig"));
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
        on_permission_request: None,
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
    assert!(
        saw_denied_event,
        "expected ToolDenied event from governance"
    );
    assert!(
        saw_denied_result,
        "expected error ToolResult from governance denial"
    );
}

#[tokio::test]
async fn governance_ask_user_without_handler_resolves_denied() {
    let mut plane = LocalExecutionPlane::new();
    plane.register(RegisteredTool::text(
        "sensitive_op",
        "Needs user approval",
        serde_json::json!({"type": "object", "properties": {}}),
        |_| Box::pin(async { Ok("done".into()) }),
    ));

    let mut gov = Governance::allow();
    gov.add_permission_rule("sensitive_op", PermissionAction::AskUser);

    let call = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("sensitive_op"),
        arguments: serde_json::json!({}),
    };

    let ctx = RunContext {
        agent_id: None,
        skill_dir: None,
        dream_store: None,
        knowledge_source: None,
        governance: Some(Arc::new(Mutex::new(gov))),
        on_tool_suspend: None,
        on_permission_request: None,
    };

    let calls = [call];
    let mut stream = plane.execute_all(&calls, ctx);
    let mut saw_permission_request = false;
    let mut saw_permission_resolved = false;
    let mut saw_tool_denied = false;
    let mut saw_denied_result = false;
    while let Some(evt) = stream.next().await {
        match evt.unwrap() {
            RunEvent::PermissionRequest { .. } => saw_permission_request = true,
            RunEvent::PermissionResolved {
                approved,
                responder,
                ..
            } => {
                saw_permission_resolved = !approved && responder == "policy_gate";
            }
            RunEvent::ToolDenied { .. } => saw_tool_denied = true,
            RunEvent::ToolResult { is_error: true, .. } => saw_denied_result = true,
            _ => {}
        }
    }
    assert!(
        saw_permission_request,
        "expected PermissionRequest event for ask_user verdict"
    );
    assert!(
        saw_permission_resolved,
        "expected denied PermissionResolved event without handler"
    );
    assert!(
        saw_tool_denied,
        "expected ToolDenied event when approval is rejected"
    );
    assert!(
        saw_denied_result,
        "expected error ToolResult when no permission handler"
    );
}

#[tokio::test]
async fn governance_ask_user_runs_tool_after_handler_approval() {
    let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let executed_for_tool = executed.clone();
    let mut plane = LocalExecutionPlane::new();
    plane.register(RegisteredTool::text(
        "sensitive_op",
        "Needs user approval",
        serde_json::json!({"type": "object", "properties": {}}),
        move |_| {
            let executed_for_tool = executed_for_tool.clone();
            Box::pin(async move {
                executed_for_tool.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok("approved-result".into())
            })
        },
    ));

    let mut gov = Governance::allow();
    gov.add_permission_rule("sensitive_op", PermissionAction::AskUser);

    let call = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("sensitive_op"),
        arguments: serde_json::json!({}),
    };

    let ctx = RunContext {
        agent_id: None,
        skill_dir: None,
        dream_store: None,
        knowledge_source: None,
        governance: Some(Arc::new(Mutex::new(gov))),
        on_tool_suspend: None,
        on_permission_request: Some(Arc::new(|request| {
            Box::pin(async move {
                Ok(PermissionResponse {
                    approved: request.tool_name == "sensitive_op",
                    responder: "test-host".to_string(),
                    reason: None,
                })
            })
        })),
    };

    let calls = [call];
    let mut stream = plane.execute_all(&calls, ctx);
    let mut saw_permission_resolved = false;
    let mut saw_success_result = false;
    while let Some(evt) = stream.next().await {
        match evt.unwrap() {
            RunEvent::PermissionResolved {
                approved,
                responder,
                ..
            } => {
                saw_permission_resolved = approved && responder == "test-host";
            }
            RunEvent::ToolResult {
                is_error: false,
                content,
                ..
            } => {
                saw_success_result = content == "approved-result";
            }
            RunEvent::ToolDenied { .. } => panic!("approved ask_user must not emit ToolDenied"),
            _ => {}
        }
    }

    assert!(executed.load(std::sync::atomic::Ordering::Relaxed));
    assert!(
        saw_permission_resolved,
        "expected approved PermissionResolved event"
    );
    assert!(saw_success_result, "expected successful tool result");
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
                    error_kind: None,
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

    let entries = session_log.read(session_id, 0, None).await.unwrap();
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
                StreamEvent::TextDelta {
                    delta: "recovered".into(),
                },
            )])))
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "test-session".to_string();

    session_log
        .append(
            &session_id,
            SessionEvent::RunStarted {
                run_id: "r1".to_string(),
                goal: "hi".to_string(),
                criteria: vec![],
                agent_id: None,
                system_prompt: None,
            },
        )
        .await
        .unwrap();

    for i in 0..3 {
        session_log
            .append(
                &session_id,
                SessionEvent::LlmCompleted {
                    turn: i * 2,
                    message: Message::assistant("a".repeat(200)),
                    provider_replay: None,
                },
            )
            .await
            .unwrap();

        session_log
            .append(
                &session_id,
                SessionEvent::LlmCompleted {
                    turn: i * 2 + 1,
                    message: Message::user("q".repeat(200)),
                    provider_replay: None,
                },
            )
            .await
            .unwrap();
    }

    let mut opts = default_runtime_opts(
        Box::new(TooLongThenOkProvider {
            calls: calls.clone(),
        }),
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

    let events = session_log.read(&session_id, 0, None).await.unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.event, SessionEvent::Compressed { .. }))
    );
}

// ─── Milestone contract: SM-level cascade → SessionEvent chain ───────────

/// Verifies the full milestone cascade chain:
/// `LoopStateMachine::load_milestone_contract` → `EvaluateMilestone` action →
/// `MilestoneResult` feed → `KernelObservation::MilestoneAdvanced` →
/// `SessionEvent::MilestoneAdvanced` written to audit log.
#[tokio::test]
async fn milestone_pass_writes_session_event() {
    use deepstrike_core::types::milestone::{
        MilestoneCheckResult, MilestoneContract, MilestonePhase,
    };

    let mut sm = LoopStateMachine::new(SchedulerBudget {
        max_tokens: 128_000,
        ..SchedulerBudget::default()
    });

    let contract = MilestoneContract::new()
        .phase(MilestonePhase::new("plan").with_criterion("Plan is complete"))
        .phase(MilestonePhase::new("implement"));
    sm.load_milestone_contract(contract);

    use deepstrike_core::scheduler::state_machine::{LoopAction, LoopEvent};
    use deepstrike_core::types::message::Role;
    use deepstrike_core::types::task::RuntimeTask;

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
        if let KernelObservation::MilestoneAdvanced {
            turn,
            phase_id,
            capabilities_unlocked,
        } = obs
        {
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

    let entries = session_log.read(session_id, 0, None).await.unwrap();
    assert!(
        entries.iter().any(|e| matches!(&e.event,
            SessionEvent::MilestoneAdvanced { phase_id, .. } if phase_id == "plan"
        )),
        "SessionEvent::MilestoneAdvanced for 'plan' must be in audit log",
    );
}

#[tokio::test]
async fn milestone_block_writes_session_event() {
    use deepstrike_core::scheduler::state_machine::{LoopAction, LoopEvent};
    use deepstrike_core::types::milestone::{
        MilestoneCheckResult, MilestoneContract, MilestonePhase,
    };
    use deepstrike_core::types::task::RuntimeTask;

    let mut sm = LoopStateMachine::new(SchedulerBudget {
        max_tokens: 128_000,
        ..SchedulerBudget::default()
    });

    sm.load_milestone_contract(MilestoneContract::new().phase(MilestonePhase::new("verify")));
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
        if let KernelObservation::MilestoneBlocked {
            turn,
            phase_id,
            reason,
        } = obs
        {
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

    let entries = session_log.read(session_id, 0, None).await.unwrap();
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
/// `LoopStateMachine::mount_capability` emits `KernelObservation::CapabilityChanged`,
/// which the runner converts to `SessionEvent::CapabilityChanged` in the audit log.
/// The test simulates the runner's observation-processing step inline so no real
/// LLM provider is needed.
#[tokio::test]
async fn capability_mount_emits_capability_changed_session_event() {
    let mut sm = LoopStateMachine::new(SchedulerBudget {
        max_tokens: 128_000,
        ..SchedulerBudget::default()
    });

    let schema = ToolSchema {
        name: CompactString::new("my_tool"),
        description: "A dynamically mounted test tool".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    };
    sm.mount_capability(CapabilityDescriptor::tool(schema), None, None);

    let observations = sm.take_observations();
    assert_eq!(
        observations.len(),
        1,
        "expected exactly one observation after mount"
    );

    // Simulate what RuntimeRunner does: convert each observation to a SessionEvent.
    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "cap-mount-test";
    let mut saw_capability_changed = false;

    for obs in &observations {
        if let KernelObservation::CapabilityChanged {
            turn,
            added,
            removed,
            change_kind,
            capability_id,
            version,
            ..
        } = obs
        {
            session_log
                .append(
                    session_id,
                    SessionEvent::CapabilityChanged {
                        turn: *turn,
                        added: added.clone(),
                        removed: removed.clone(),
                        change_kind: change_kind.clone(),
                        capability_id: capability_id.clone(),
                        version: version.clone(),
                        mounted_by: None,
                        mount_reason: None,
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
    assert!(
        saw_capability_changed,
        "no CapabilityChanged observation emitted"
    );

    // Confirm the event was written to the session log.
    let entries = session_log.read(session_id, 0, None).await.unwrap();
    assert!(
        entries
            .iter()
            .any(|e| matches!(e.event, SessionEvent::CapabilityChanged { .. })),
        "SessionEvent::CapabilityChanged must be present in audit log after mount",
    );
}

#[tokio::test]
async fn capability_unmount_emits_capability_changed_session_event() {
    let mut sm = LoopStateMachine::new(SchedulerBudget {
        max_tokens: 128_000,
        ..SchedulerBudget::default()
    });

    // Mount first so there is something to remove.
    let schema = ToolSchema {
        name: CompactString::new("ephemeral_tool"),
        description: "Tool to be removed".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    };
    sm.mount_capability(CapabilityDescriptor::tool(schema), None, None);
    sm.take_observations(); // clear mount observation

    sm.unmount_capability(CapabilityKind::Tool, "ephemeral_tool");

    let observations = sm.take_observations();
    assert_eq!(
        observations.len(),
        1,
        "expected exactly one observation after unmount"
    );

    let session_log = Arc::new(InMemorySessionLog::new());
    let session_id = "cap-unmount-test";

    for obs in &observations {
        if let KernelObservation::CapabilityChanged {
            turn,
            added,
            removed,
            change_kind,
            capability_id,
            version,
            ..
        } = obs
        {
            session_log
                .append(
                    session_id,
                    SessionEvent::CapabilityChanged {
                        turn: *turn,
                        added: added.clone(),
                        removed: removed.clone(),
                        change_kind: change_kind.clone(),
                        capability_id: capability_id.clone(),
                        version: version.clone(),
                        mounted_by: None,
                        mount_reason: None,
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

    let entries = session_log.read(session_id, 0, None).await.unwrap();
    assert!(
        entries
            .iter()
            .any(|e| matches!(e.event, SessionEvent::CapabilityChanged { .. })),
        "SessionEvent::CapabilityChanged must be present in audit log after unmount",
    );
}
