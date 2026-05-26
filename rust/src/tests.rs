#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(validate_tool_arguments(&schema, &serde_json::json!({})).is_err());
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
        });

        let session_id = "reactive-compact-rust";
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

        let entries = session_log.read(session_id, 0).await.unwrap();
        assert!(entries.iter().any(|entry| {
            matches!(
                entry.event,
                deepstrike_core::runtime::session::SessionEvent::Compressed { .. }
            )
        }));
    }
}
