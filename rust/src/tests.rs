#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use crate::RunEvent;
    use crate::memory::WorkingMemory;
    use crate::safety::{PermissionManager, PermissionMode};
    use crate::signals::ScheduledPrompt;
    use crate::tools::{
        RegisteredTool, ToolChunk, execute_tools, read_file_tool, validate_tool_arguments,
    };
    use compact_str::CompactString;
    use deepstrike_core::types::message::ToolCall;
    use std::collections::HashMap;

    #[test]
    fn working_memory_set_get_clear() {
        let mut mem = WorkingMemory::default();
        mem.set("step", 1);
        assert_eq!(mem.get("step"), Some(&serde_json::json!(1)));
        mem.clear();
        assert!(mem.get("step").is_none());
    }

    #[test]
    fn permission_manager_default_mode() {
        let mut pm = PermissionManager::new(PermissionMode::Default);
        pm.grant("fs", "read");
        assert!(pm.evaluate("fs", "read").allowed);
        assert!(!pm.evaluate("fs", "write").allowed);
    }

    #[test]
    fn permission_manager_wildcard() {
        let mut pm = PermissionManager::new(PermissionMode::Default);
        pm.grant("fs", "*");
        assert!(pm.evaluate("fs", "anything").allowed);
    }

    #[test]
    fn permission_manager_auto_allows_all() {
        let pm = PermissionManager::new(PermissionMode::Auto);
        assert!(pm.evaluate("bash", "execute").allowed);
    }

    #[test]
    fn permission_manager_plan_blocks_all() {
        let mut pm = PermissionManager::new(PermissionMode::Plan);
        pm.grant("fs", "*");
        assert!(!pm.evaluate("fs", "read").allowed);
    }

    #[test]
    fn scheduled_prompt_to_signal() {
        let p = ScheduledPrompt::new("standup", 1_700_000_000_000);
        let sig = p.to_signal();
        assert_eq!(sig.kind, "scheduled");
        assert_eq!(sig.payload["goal"], "standup");
    }

    #[test]
    fn read_file_tool_has_correct_schema() {
        let t = read_file_tool();
        assert_eq!(t.schema.name.as_str(), "read_file");
    }

    #[tokio::test]
    async fn execute_tools_unknown_tool() {
        let call = ToolCall {
            id: CompactString::new("1"),
            name: CompactString::new("nope"),
            arguments: serde_json::json!({}),
        };
        let results = execute_tools(&[call], &HashMap::new()).await;
        assert!(results[0].is_error);
    }

    #[tokio::test]
    async fn execute_tools_success() {
        let tool = RegisteredTool::text(
            "add",
            "Add two numbers.",
            serde_json::json!({ "type": "object" }),
            |args| {
                Box::pin(async move {
                    let x = args["x"].as_i64().unwrap_or(0);
                    let y = args["y"].as_i64().unwrap_or(0);
                    Ok(format!("{}", x + y))
                })
            },
        );
        let mut registry = HashMap::new();
        registry.insert("add".to_string(), tool);
        let call = ToolCall {
            id: CompactString::new("1"),
            name: CompactString::new("add"),
            arguments: serde_json::json!({"x": 2, "y": 3}),
        };
        let results = execute_tools(&[call], &registry).await;
        assert!(!results[0].is_error);
        assert_eq!(results[0].output.as_text(), Some("5"));
    }

    #[test]
    fn validate_tool_arguments_rejects_missing_required_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"]
        });
        let mut args = serde_json::json!({});
        assert!(validate_tool_arguments(&schema, &mut args).is_err());
    }

    #[test]
    fn validate_tool_arguments_repairs_white_listed_patterns() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer" },
                "enabled": { "type": "boolean" },
                "ratio": { "type": "number", "default": 0.5 },
                "name": { "type": "string" }
            },
            "required": ["count"]
        });

        // 1. 类型强转 (String to Int/Bool) + 补默认值 + 裁剪多余字段
        let mut args = serde_json::json!({
            "count": "10",
            "enabled": "true",
            "extra_field": "remove_me"
        });
        let repaired = validate_tool_arguments(&schema, &mut args).expect("should succeed");
        assert!(repaired);
        assert_eq!(args["count"], 10);
        assert_eq!(args["enabled"], true);
        assert_eq!(args["ratio"], 0.5); // Default value injected
        assert!(args.get("extra_field").is_none()); // Trimmed extra field

        // 2. 无法自愈 (缺失 required 字段)
        let mut args_invalid = serde_json::json!({
            "enabled": false
        });
        assert!(validate_tool_arguments(&schema, &mut args_invalid).is_err());
    }

    #[test]
    fn text_tool_chunk_projects_to_text() {
        assert_eq!(ToolChunk::text("hello").text_projection(), "hello");
        assert_eq!(
            ToolChunk::progress(0.5, Some("half".into())).text_projection(),
            ""
        );
    }

    #[test]
    fn harness_request_builder() {
        let req = crate::harness::HarnessRequest::new("Write a haiku");
        assert_eq!(req.goal, "Write a haiku");
        assert!(req.criteria.is_empty());
    }

    struct StatefulTestProvider {
        states: std::sync::Arc<std::sync::Mutex<Vec<Option<crate::providers::ProviderRunState>>>>,
        call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        marker: crate::providers::ProviderRunState,
    }

    #[async_trait::async_trait]
    impl crate::providers::LLMProvider for StatefulTestProvider {
        fn create_run_state(&self) -> Option<crate::providers::ProviderRunState> {
            Some(self.marker.clone())
        }

        async fn stream(
            &self,
            _context: &deepstrike_core::context::renderer::RenderedContext,
            _tools: &[deepstrike_core::types::message::ToolSchema],
            _extensions: Option<&serde_json::Value>,
            state: Option<&crate::providers::ProviderRunState>,
        ) -> crate::Result<
            Box<
                dyn futures::Stream<Item = crate::Result<crate::providers::StreamEvent>>
                    + Send
                    + Unpin,
            >,
        > {
            self.states.lock().unwrap().push(state.cloned());
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let events: Vec<crate::Result<crate::providers::StreamEvent>> = if n == 1 {
                vec![Ok(crate::providers::StreamEvent::ToolCall {
                    id: "call_1".into(),
                    name: "ping".into(),
                    arguments: serde_json::json!({}),
                })]
            } else {
                vec![Ok(crate::providers::StreamEvent::TextDelta {
                    delta: "done".into(),
                })]
            };
            Ok(Box::new(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn runner_threads_provider_run_state_through_turns() {
        use crate::runtime::{
            InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner,
        };
        use crate::tools::RegisteredTool;
        use futures::StreamExt;
        use std::sync::Arc;

        let states = std::sync::Arc::new(std::sync::Mutex::new(Vec::<
            Option<crate::providers::ProviderRunState>,
        >::new()));
        let provider = StatefulTestProvider {
            states: states.clone(),
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            marker: serde_json::json!({ "marker": "test-run-state" }),
        };

        let mut plane = LocalExecutionPlane::new();
        plane.register(RegisteredTool::text(
            "ping",
            "Ping",
            serde_json::json!({ "type": "object", "properties": {} }),
            |_args| Box::pin(async { Ok("pong".into()) }),
        ));

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(provider),
            execution_plane: Some(Box::new(plane)),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
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
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            on_milestone_evaluate: None,
        });

        let mut stream = runner
            .run_streaming("Use ping once, then finish.", &[], None, None)
            .await
            .unwrap();
        while stream.next().await.transpose().unwrap().is_some() {}

        let seen = states.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1]);
    }

    struct TooLongThenOkProvider {
        call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::providers::LLMProvider for TooLongThenOkProvider {
        async fn stream(
            &self,
            _context: &deepstrike_core::context::renderer::RenderedContext,
            _tools: &[deepstrike_core::types::message::ToolSchema],
            _extensions: Option<&serde_json::Value>,
            _state: Option<&crate::providers::ProviderRunState>,
        ) -> crate::Result<
            Box<
                dyn futures::Stream<Item = crate::Result<crate::providers::StreamEvent>>
                    + Send
                    + Unpin,
            >,
        > {
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if n == 1 {
                return Err(crate::Error::Provider("413 prompt too long".to_string()));
            }
            Ok(Box::new(futures::stream::iter(vec![Ok(
                crate::providers::StreamEvent::TextDelta {
                    delta: "recovered".into(),
                },
            )])))
        }
    }

    #[tokio::test]
    async fn runner_reactive_compacts_and_retries_prompt_too_long() {
        use crate::runtime::{InMemorySessionLog, RuntimeOptions, RuntimeRunner, SessionLog};
        use futures::StreamExt;
        use std::sync::Arc;

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(TooLongThenOkProvider {
                call_count: call_count.clone(),
            }),
            execution_plane: None,
            session_log: Some(session_log.clone()),
            compression_store: None,
            session_id: None,
            max_tokens: 1_000,
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
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            on_milestone_evaluate: None,
        });

        let session_id = "reactive-compact-rust";
        session_log.append(session_id, deepstrike_core::runtime::session::SessionEvent::RunStarted {
            run_id: "seed".to_string(),
            goal: "seed ".repeat(1200),
            criteria: vec![],
            agent_id: None,
            system_prompt: None,
        }).await;
        session_log.append(session_id, deepstrike_core::runtime::session::SessionEvent::LlmCompleted {
            turn: 0,
            message: deepstrike_core::types::message::Message {
                role: deepstrike_core::types::message::Role::Assistant,
                content: deepstrike_core::types::message::Content::Text("prior answer ".repeat(400)),
                tool_calls: vec![],
                token_count: None,
            },
            provider_replay: None,
        }).await;
        session_log.append(session_id, deepstrike_core::runtime::session::SessionEvent::RunTerminal {
            reason: "completed".to_string(),
            turns_used: 1,
            total_tokens: 0,
        }).await;

        let goal = "a".repeat(5000);
        let mut stream = runner
            .run_streaming(&goal, &[], None, Some(session_id))
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(evt) = stream.next().await {
            if let crate::RunEvent::TextDelta(delta) = evt.unwrap() {
                text.push_str(&delta);
            }
        }

        assert_eq!(text, "recovered");
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            2
        );

        let entries = session_log.read(session_id, 0, None).await.unwrap();
        assert!(entries.iter().any(|entry| {
            matches!(
                entry.event,
                deepstrike_core::runtime::session::SessionEvent::Compressed { .. }
            )
        }));
    }

    #[tokio::test]
    async fn recoverable_tool_failure_preserves_replay_context() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use deepstrike_core::types::message::{Message, Role, Content, ToolCall};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use crate::runtime::runner::{RuntimeRunner, RuntimeOptions};
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::replay::replay_messages;

        use futures::StreamExt;

        // 1. 创建一个 LLMProvider。它在第一轮返回一个带有工具调用的 assistant 消息，第二轮返回 "done"。
        #[derive(Clone)]
        struct FakeProvider {
            call_count: Arc<AtomicU32>,
        }
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn complete(
                &self,
                _context: &deepstrike_core::context::renderer::RenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
            ) -> crate::Result<Message> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    Ok(Message {
                        role: Role::Assistant,
                        content: Content::Text("Let's call tool".into()),
                        tool_calls: vec![ToolCall {
                            id: compact_str::CompactString::new("call_1"),
                            name: compact_str::CompactString::new("fail_tool"),
                            arguments: serde_json::json!({}),
                        }],
                        token_count: None,
                    })
                } else {
                    Ok(Message {
                        role: Role::Assistant,
                        content: Content::Text("Recovered".into()),
                        tool_calls: vec![],
                        token_count: None,
                    })
                }
            }
            async fn stream(
                &self,
                context: &deepstrike_core::context::renderer::RenderedContext,
                tools: &[deepstrike_core::types::message::ToolSchema],
                extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>> {
                let msg = self.complete(context, tools, extensions).await?;
                let mut stream = vec![];
                if !msg.tool_calls.is_empty() {
                    for tc in &msg.tool_calls {
                        stream.push(Ok(StreamEvent::ToolCall {
                            id: tc.id.to_string(),
                            name: tc.name.to_string(),
                            arguments: tc.arguments.clone(),
                        }));
                    }
                } else {
                    if let Content::Text(txt) = msg.content {
                        stream.push(Ok(StreamEvent::TextDelta { delta: txt }));
                    }
                }
                stream.push(Ok(StreamEvent::Done));
                Ok(Box::new(futures::stream::iter(stream)))
            }
        }

        // 2. 创建一个 ExecutionPlane。它执行 "fail_tool" 并且返回错误 (is_error: true)。
        let mut plane = crate::runtime::execution_plane::LocalExecutionPlane::new();
        plane.register(crate::tools::RegisteredTool::text(
            "fail_tool",
            "Fails always",
            serde_json::json!({ "type": "object", "properties": {} }),
            |_args| Box::pin(async {
                Err(crate::Error::Tool("Tool crashed!".into()))
            }),
        ));

        let session_log = Arc::new(InMemorySessionLog::new());
        let call_count = Arc::new(AtomicU32::new(0));

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider {
                call_count: call_count.clone(),
            }),
            execution_plane: Some(Box::new(plane)),
            session_log: Some(session_log.clone()),
            compression_store: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
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
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            on_milestone_evaluate: None,
        });

        let session_id = "test-rollback";
        let mut stream = runner
            .run_streaming("run", &[], None, Some(session_id))
            .await
            .unwrap();
        while let Some(evt) = stream.next().await {
            let _ = evt.unwrap();
        }

        // 3. 普通 tool error 是 recoverable，不应该触发 rollback。
        let entries = session_log.read(session_id, 0, None).await.unwrap();
        assert!(!entries.iter().any(|entry| {
            matches!(
                entry.event,
                deepstrike_core::runtime::session::SessionEvent::Rollbacked { .. }
            )
        }));

        // 4. 重放整个事件流，错误结果应保留在 history 中，供模型自愈。
        let messages = replay_messages(&entries);

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[2].role, Role::Tool);
        assert_eq!(messages[3].role, Role::Assistant);
        if let Content::Text(ref txt) = messages[3].content {
            assert_eq!(txt, "Recovered");
        } else {
            panic!("Expected text assistant response");
        }
    }

    #[tokio::test]
    async fn runner_milestone_auto_pass() {
        use std::sync::Arc;
        use deepstrike_core::types::milestone::{MilestoneContract, MilestonePhase};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use crate::runtime::runner::{RuntimeRunner, RuntimeOptions, MilestonePolicy};
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::execution_plane::LocalExecutionPlane;

        #[derive(Clone)]
        struct FakeProvider;
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::RenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>> {
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta { delta: "done".into() }),
                    Ok(StreamEvent::Done),
                ])))
            }
        }

        let contract = MilestoneContract::new().phase(MilestonePhase::new("phase1").with_criterion("test"));
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider),
            execution_plane: Some(Box::new(LocalExecutionPlane::new())),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
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
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: Some(contract),
            run_spec: None,
            on_milestone_evaluate: None,
        });

        let mut stream = runner.run_streaming("test", &[], None, Some("s_auto_rust")).await.unwrap();
        let mut done_seen = false;
        while let Some(evt) = stream.next().await {
            if let RunEvent::Done { status, .. } = evt.unwrap() {
                assert_eq!(status, "completed");
                done_seen = true;
            }
        }
        assert!(done_seen);
    }

    #[tokio::test]
    async fn runner_milestone_pending_by_default() {
        use std::sync::Arc;
        use deepstrike_core::types::milestone::{MilestoneContract, MilestonePhase};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use crate::runtime::runner::{RuntimeRunner, RuntimeOptions, MilestonePolicy};
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::execution_plane::LocalExecutionPlane;

        #[derive(Clone)]
        struct FakeProvider;
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::RenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>> {
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta { delta: "done".into() }),
                    Ok(StreamEvent::Done),
                ])))
            }
        }

        let contract = MilestoneContract::new().phase(MilestonePhase::new("phase1").with_criterion("test"));
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider),
            execution_plane: Some(Box::new(LocalExecutionPlane::new())),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
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
            on_permission_request: None,
            milestone_policy: MilestonePolicy::RequireVerifier,
            milestone_contract: Some(contract),
            run_spec: None,
            on_milestone_evaluate: None,
        });

        let mut stream = runner.run_streaming("test", &[], None, Some("s_pending_rust")).await.unwrap();
        let mut done_seen = false;
        while let Some(evt) = stream.next().await {
            if let RunEvent::Done { status, .. } = evt.unwrap() {
                assert_eq!(status, "milestone_pending");
                done_seen = true;
            }
        }
        assert!(done_seen);
    }

    #[tokio::test]
    async fn runner_milestone_verifier_callback() {
        use std::sync::Arc;
        use std::sync::Mutex;
        use deepstrike_core::types::milestone::{MilestoneContract, MilestonePhase, MilestoneCheckResult};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use crate::runtime::runner::{RuntimeRunner, RuntimeOptions, MilestonePolicy};
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::execution_plane::LocalExecutionPlane;

        #[derive(Clone)]
        struct FakeProvider;
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::RenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>> {
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta { delta: "done".into() }),
                    Ok(StreamEvent::Done),
                ])))
            }
        }

        let contract = MilestoneContract::new().phase(MilestonePhase::new("phase1").with_criterion("test"));
        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();

        let verifier = Arc::new(move |ctx: crate::runtime::MilestoneEvaluationContext| {
            let called_clone = called_clone.clone();
            Box::pin(async move {
                assert_eq!(ctx.phase_id, "phase1");
                assert_eq!(ctx.criteria, vec!["test".to_string()]);
                *called_clone.lock().unwrap() = true;
                Ok(MilestoneCheckResult::pass(ctx.phase_id))
            }) as futures::future::BoxFuture<'static, crate::Result<MilestoneCheckResult>>
        });

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider),
            execution_plane: Some(Box::new(LocalExecutionPlane::new())),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
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
            on_permission_request: None,
            milestone_policy: MilestonePolicy::RequireVerifier,
            milestone_contract: Some(contract),
            run_spec: None,
            on_milestone_evaluate: Some(verifier),
        });

        let mut stream = runner.run_streaming("test", &[], None, Some("s_callback_rust")).await.unwrap();
        let mut done_seen = false;
        while let Some(evt) = stream.next().await {
            if let RunEvent::Done { status, .. } = evt.unwrap() {
                assert_eq!(status, "completed");
                done_seen = true;
            }
        }
        assert!(done_seen);
        assert!(*called.lock().unwrap());
    }

    #[tokio::test]
    async fn test_local_execution_plane_spool_read_intercept() {
        use crate::runtime::execution_plane::{ExecutionPlane, LocalExecutionPlane, RunContext};
        use deepstrike_core::types::message::ToolCall;
        
        // 1. Create a dummy spool file
        let spool_dir = std::path::Path::new(".spool");
        let _ = std::fs::create_dir_all(spool_dir);
        let spool_file = spool_dir.join("test-spool-intercept.txt");
        let expected_content = "This is the spooled output content that should be transparently read!";
        std::fs::write(&spool_file, expected_content).unwrap();
        
        // 2. Create local execution plane
        let plane = LocalExecutionPlane::new();
        let call = ToolCall {
            id: compact_str::CompactString::new("call_read"),
            name: compact_str::CompactString::new("read_file"),
            arguments: serde_json::json!({
                "path": spool_file.to_string_lossy().to_string()
            }),
        };
        
        let ctx = RunContext {
            agent_id: None,
            skill_dir: None,
            dream_store: None,
            knowledge_source: None,
            governance: None,
            on_tool_suspend: None,
            on_permission_request: None,
        };
        
        let events: Vec<RunEvent> = plane.execute_all(&[call], ctx)
            .map(|r| r.unwrap())
            .collect()
            .await;
            
        // 3. Clean up the spool file
        let _ = std::fs::remove_file(spool_file);
        
        // 4. Assert transparent intercept worked
        assert_eq!(events.len(), 1);
        if let RunEvent::ToolResult { call_id, content, is_error, .. } = &events[0] {
            assert_eq!(call_id, "call_read");
            assert_eq!(content, expected_content);
            assert!(!is_error);
        } else {
            panic!("Expected RunEvent::ToolResult");
        }
    }

    use crate::memory::DreamStore;
    use deepstrike_core::memory::semantic::MemoryEntry;
    use crate::runtime::InMemorySessionLog;


    struct MockLLMProvider;

    #[async_trait::async_trait]
    impl crate::providers::LLMProvider for MockLLMProvider {
        async fn stream(
            &self,
            _context: &deepstrike_core::context::renderer::RenderedContext,
            _tools: &[deepstrike_core::types::message::ToolSchema],
            _extensions: Option<&serde_json::Value>,
            _state: Option<&crate::providers::ProviderRunState>,
        ) -> crate::Result<
            Box<
                dyn futures::Stream<Item = crate::Result<crate::providers::StreamEvent>>
                    + Send
                    + Unpin,
            >,
        > {
            let events = vec![Ok(crate::providers::StreamEvent::TextDelta {
                delta: "Summary of page out conversation".to_string(),
            })];
            Ok(Box::new(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn test_semantic_page_out_archives_to_dream_store() {
        use crate::runtime::runner::{RuntimeRunner, RuntimeOptions, MilestonePolicy};
        use deepstrike_core::runtime::kernel::{KernelObservation, KernelPressureAction};
        use deepstrike_core::types::message::{Message, Role};
        use std::sync::Arc;

        let memories = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));

        struct SharedMockDreamStore {
            memories: Arc<std::sync::Mutex<Vec<MemoryEntry>>>,
            sessions: Arc<std::sync::Mutex<Vec<deepstrike_core::memory::durable::SessionData>>>,
        }

        #[async_trait::async_trait]
        impl DreamStore for SharedMockDreamStore {
            async fn load_sessions(&self, _agent_id: &str) -> crate::Result<Vec<deepstrike_core::memory::durable::SessionData>> {
                Ok(self.sessions.lock().unwrap().clone())
            }
            async fn load_memories(&self, _agent_id: &str) -> crate::Result<Vec<MemoryEntry>> {
                Ok(self.memories.lock().unwrap().clone())
            }
            async fn commit(
                &self,
                _agent_id: &str,
                result: deepstrike_core::memory::curator::CurationResult,
                _existing: &[MemoryEntry],
            ) -> crate::Result<()> {
                let mut mems = self.memories.lock().unwrap();
                for idx in result.to_remove_indices.iter().rev() {
                    if *idx < mems.len() {
                        mems.remove(*idx);
                    }
                }
                mems.extend(result.to_add);
                Ok(())
            }
            async fn search(
                &self,
                _agent_id: &str,
                _query: &str,
                _top_k: usize,
            ) -> crate::Result<Vec<MemoryEntry>> {
                Ok(self.memories.lock().unwrap().clone())
            }
            async fn save_session(
                &self,
                data: deepstrike_core::memory::durable::SessionData,
            ) -> crate::Result<()> {
                self.sessions.lock().unwrap().push(data);
                Ok(())
            }
        }

        let store = SharedMockDreamStore {
            memories: memories.clone(),
            sessions: sessions.clone(),
        };

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MockLLMProvider),
            execution_plane: None,
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("test-agent".to_string()),
            system_prompt: None,
            initial_memory: vec![],
            skill_dir: None,
            dream_store: Some(Box::new(store)),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            on_milestone_evaluate: None,
        });

        let mut obs = vec![KernelObservation::PageOut {
            turn: 1,
            action: KernelPressureAction::AutoCompact,
            rho_after: 0.5,
            summary: Some("PageOut summary".to_string()),
            archived: vec![Message::user("Hello memory")],
            tier_hint: "semantic".to_string(),
        }];

        let kernel = std::sync::Mutex::new(deepstrike_core::runtime::kernel::KernelRuntime::new(
            deepstrike_core::scheduler::policy::LoopPolicy::default(),
        ));
        let mut pending_spools = std::collections::HashMap::new();

        runner.append_observations(
            "test-session",
            &kernel,
            &mut obs,
            &mut pending_spools,
            0,
        ).await;

        let mems = memories.lock().unwrap();
        assert_eq!(mems.len(), 1);
        assert_eq!(mems[0].text, "Summary of page out conversation");
    }

    #[tokio::test]
    async fn test_write_memory_syscall_commits_to_dream_store() {
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::SessionLog;
        use deepstrike_core::mm::memory::{MemoryKind, MemoryMetadata, MemoryWriteRequest};
        use std::sync::Arc;

        let memories = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));

        struct Store {
            memories: Arc<std::sync::Mutex<Vec<MemoryEntry>>>,
            sessions: Arc<std::sync::Mutex<Vec<deepstrike_core::memory::durable::SessionData>>>,
        }

        #[async_trait::async_trait]
        impl DreamStore for Store {
            async fn load_sessions(&self, _agent_id: &str) -> crate::Result<Vec<deepstrike_core::memory::durable::SessionData>> {
                Ok(self.sessions.lock().unwrap().clone())
            }
            async fn load_memories(&self, _agent_id: &str) -> crate::Result<Vec<MemoryEntry>> {
                Ok(self.memories.lock().unwrap().clone())
            }
            async fn commit(
                &self,
                _agent_id: &str,
                result: deepstrike_core::memory::curator::CurationResult,
                _existing: &[MemoryEntry],
            ) -> crate::Result<()> {
                self.memories.lock().unwrap().extend(result.to_add);
                Ok(())
            }
            async fn search(
                &self,
                _agent_id: &str,
                _query: &str,
                _top_k: usize,
            ) -> crate::Result<Vec<MemoryEntry>> {
                Ok(self.memories.lock().unwrap().clone())
            }
            async fn save_session(
                &self,
                data: deepstrike_core::memory::durable::SessionData,
            ) -> crate::Result<()> {
                self.sessions.lock().unwrap().push(data);
                Ok(())
            }
        }

        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MockLLMProvider),
            execution_plane: None,
            session_log: Some(session_log.clone()),
            compression_store: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("agent-memory".to_string()),
            system_prompt: None,
            initial_memory: vec![],
            skill_dir: None,
            dream_store: Some(Box::new(Store { memories: memories.clone(), sessions })),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            on_milestone_evaluate: None,
        });

        runner.write_memory(
            MemoryWriteRequest {
                metadata: MemoryMetadata {
                    name: "prefers-small-tests".to_string(),
                    description: "User prefers small focused tests".to_string(),
                    kind: Some(MemoryKind::BehaviorPreference),
                    created_at: 1,
                    updated_at: 1,
                    ..Default::default()
                },
                content: "User prefers focused unit tests for SDK behavior.".to_string(),
            },
            Some("memory-syscall-rs"),
            None,
        ).await.unwrap();

        assert_eq!(memories.lock().unwrap()[0].text, "User prefers focused unit tests for SDK behavior.");
        let events = session_log.read("memory-syscall-rs", 0, None).await.unwrap();
        assert!(events.iter().any(|e| e.event.kind_str() == "memory_written"));
    }

    #[tokio::test]
    async fn test_query_memory_syscall_returns_dream_store_hits() {
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::SessionLog;
        use deepstrike_core::mm::memory::MemoryQuery;
        use std::sync::Arc;

        let memories = Arc::new(std::sync::Mutex::new(vec![MemoryEntry {
            text: "Use small focused tests.".to_string(),
            score: 0.9,
            metadata: serde_json::json!({"name": "testing"}),
        }]));
        let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));

        struct Store {
            memories: Arc<std::sync::Mutex<Vec<MemoryEntry>>>,
            sessions: Arc<std::sync::Mutex<Vec<deepstrike_core::memory::durable::SessionData>>>,
        }

        #[async_trait::async_trait]
        impl DreamStore for Store {
            async fn load_sessions(&self, _agent_id: &str) -> crate::Result<Vec<deepstrike_core::memory::durable::SessionData>> {
                Ok(self.sessions.lock().unwrap().clone())
            }
            async fn load_memories(&self, _agent_id: &str) -> crate::Result<Vec<MemoryEntry>> {
                Ok(self.memories.lock().unwrap().clone())
            }
            async fn commit(
                &self,
                _agent_id: &str,
                _result: deepstrike_core::memory::curator::CurationResult,
                _existing: &[MemoryEntry],
            ) -> crate::Result<()> {
                Ok(())
            }
            async fn search(
                &self,
                _agent_id: &str,
                query: &str,
                top_k: usize,
            ) -> crate::Result<Vec<MemoryEntry>> {
                if query.contains("tests") && top_k == 1 {
                    Ok(self.memories.lock().unwrap().clone())
                } else {
                    Ok(Vec::new())
                }
            }
            async fn save_session(
                &self,
                data: deepstrike_core::memory::durable::SessionData,
            ) -> crate::Result<()> {
                self.sessions.lock().unwrap().push(data);
                Ok(())
            }
        }

        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MockLLMProvider),
            execution_plane: None,
            session_log: Some(session_log.clone()),
            compression_store: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("agent-memory".to_string()),
            system_prompt: None,
            initial_memory: vec![],
            skill_dir: None,
            dream_store: Some(Box::new(Store { memories, sessions })),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            on_milestone_evaluate: None,
        });

        let hits = runner.query_memory(
            MemoryQuery {
                current_context: "Need memory about tests".to_string(),
                active_tools: vec![],
                already_surfaced: vec![],
                top_k: 1,
            },
            Some("memory-query-syscall-rs"),
            None,
        ).await.unwrap();

        assert_eq!(hits[0].text, "Use small focused tests.");
        let events = session_log.read("memory-query-syscall-rs", 0, None).await.unwrap();
        assert!(events.iter().any(|e| e.event.kind_str() == "memory_queried"));
        assert!(events.iter().any(|e| e.event.kind_str() == "memory_retrieval_result"));
    }

    #[tokio::test]
    async fn test_write_memory_validation_failure_is_logged() {
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::SessionLog;
        use deepstrike_core::memory::semantic::MemoryEntry;
        use deepstrike_core::mm::memory::{MemoryKind, MemoryMetadata, MemoryWriteRequest};
        use std::sync::Arc;

        struct Store;
        #[async_trait::async_trait]
        impl DreamStore for Store {
            async fn load_sessions(&self, _agent_id: &str) -> crate::Result<Vec<deepstrike_core::memory::durable::SessionData>> {
                Ok(vec![])
            }
            async fn load_memories(&self, _agent_id: &str) -> crate::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn commit(
                &self,
                _agent_id: &str,
                _result: deepstrike_core::memory::curator::CurationResult,
                _existing: &[MemoryEntry],
            ) -> crate::Result<()> {
                Ok(())
            }
            async fn search(&self, _agent_id: &str, _query: &str, _top_k: usize) -> crate::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn save_session(&self, _data: deepstrike_core::memory::durable::SessionData) -> crate::Result<()> {
                Ok(())
            }
        }

        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MockLLMProvider),
            execution_plane: None,
            session_log: Some(session_log.clone()),
            compression_store: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("agent-memory".to_string()),
            system_prompt: None,
            initial_memory: vec![],
            skill_dir: None,
            dream_store: Some(Box::new(Store)),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            on_milestone_evaluate: None,
        });

        runner.write_memory(
            MemoryWriteRequest {
                metadata: MemoryMetadata {
                    name: String::new(),
                    description: "missing name".to_string(),
                    kind: Some(MemoryKind::BehaviorPreference),
                    created_at: 1,
                    updated_at: 1,
                    ..Default::default()
                },
                content: "invalid write".to_string(),
            },
            Some("memory-validation-fail-rs"),
            None,
        ).await.unwrap();

        let events = session_log.read("memory-validation-fail-rs", 0, None).await.unwrap();
        assert!(events.iter().any(|e| e.event.kind_str() == "memory_validation_failed"));
        assert!(!events.iter().any(|e| e.event.kind_str() == "memory_written"));
    }
}
