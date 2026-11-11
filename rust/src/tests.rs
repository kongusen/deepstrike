#[cfg(test)]
mod tests {
    use super::*;
    use crate::RunEvent;
    use crate::memory::WorkingMemory;
    use crate::safety::{PermissionManager, PermissionMode};
    use crate::signals::{ScheduledPrompt, SignalSource};
    use crate::tools::{
        RegisteredTool, ToolChunk, execute_tools, read_file_tool, validate_tool_arguments,
    };
    use compact_str::CompactString;
    use deepstrike_core::types::message::ToolCall;
    use futures::StreamExt;
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
        assert_eq!(sig.source, "cron");
        assert_eq!(sig.signal_type, "job");
        assert_eq!(sig.urgency, "normal");
        assert_eq!(sig.payload["goal"], "standup");
    }

    #[tokio::test]
    async fn signal_gateway_redelivery_preserves_identity_and_increments_attempt() {
        let gateway = crate::SignalGateway::new();
        let receiver = gateway.subscribe();
        gateway.ingest(crate::RuntimeSignal {
            source: "gateway".into(),
            signal_type: "event".into(),
            urgency: "normal".into(),
            payload: serde_json::json!({"goal": "retry"}),
            dedupe_key: Some("logical-signal".into()),
            recipient: Some("session-1".into()),
            deadline_ms: Some(1_700_000_000_000),
            coalesce_key: Some("retry".into()),
            coalesced_count: 1,
        });

        let first = receiver
            .claim_signal()
            .await
            .expect("claim")
            .expect("first delivery");
        let first_receipt = crate::SignalDeliveryReceipt {
            delivery_id: first.delivery_id.clone(),
            lease_token: first.lease_token.clone(),
        };
        assert!(receiver.nack_signal(&first_receipt).await.expect("nack"));
        let second = receiver
            .claim_signal()
            .await
            .expect("claim")
            .expect("redelivery");
        assert_eq!(second.delivery_id, first.delivery_id);
        assert_eq!(second.signal_id, first.signal_id);
        assert_eq!(second.delivery_attempt, 2);
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

    // ── format_tool_error ────────────────────────────────────────────────────────

    #[test]
    fn format_tool_error_strips_thiserror_prefix_for_tool_variant() {
        let e = crate::Error::Tool("disk full".into());
        // `e.to_string()` would produce `"tool error: disk full"`; the formatter strips the prefix
        // so the model sees the bare message.
        assert_eq!(crate::format_tool_error(&e), "disk full");
    }

    #[test]
    fn format_tool_error_strips_prefix_for_tool_execution_failed() {
        let e = crate::Error::ToolExecutionFailed {
            output: "kaboom".into(),
            is_fatal: false,
            error_kind: None,
        };
        assert_eq!(crate::format_tool_error(&e), "kaboom");
    }

    #[test]
    fn format_tool_error_emits_json_for_coded_tool_fail() {
        let e = crate::tools::tool_fail(
            "no such section",
            Some("not_found".into()),
            Some("call document_outline first".into()),
        );
        let out = crate::format_tool_error(&e);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["message"], "no such section");
        assert_eq!(parsed["code"], "not_found");
        assert_eq!(parsed["hint"], "call document_outline first");
    }

    #[test]
    fn format_tool_error_passes_through_bare_tool_fail_message() {
        let e = crate::tools::tool_fail("bare error", None, None);
        // No code/hint → plain message string (no JSON wrapping).
        assert_eq!(crate::format_tool_error(&e), "bare error");
    }

    // ── safe_tool envelope ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn safe_tool_wraps_plain_data_in_ok_envelope() {
        let tool = crate::safe_tool(
            "echo",
            "Echo",
            serde_json::json!({ "type": "object" }),
            |args| async move { Ok(crate::tools::SafeToolResult::Data(args["x"].clone())) },
        );
        let mut registry = HashMap::new();
        registry.insert("echo".to_string(), tool);
        let call = ToolCall {
            id: CompactString::new("1"),
            name: CompactString::new("echo"),
            arguments: serde_json::json!({"x": "hi"}),
        };
        let results = execute_tools(&[call], &registry).await;
        assert!(!results[0].is_error);
        let parsed: serde_json::Value =
            serde_json::from_str(results[0].output.as_text().unwrap()).unwrap();
        assert_eq!(parsed["success"], true);
        assert_eq!(parsed["data"], "hi");
    }

    #[tokio::test]
    async fn safe_tool_passes_through_fail_envelope() {
        let tool = crate::safe_tool(
            "lookup",
            "Lookup",
            serde_json::json!({ "type": "object" }),
            |args| async move {
                let id = args["id"].as_str().unwrap_or("");
                if id == "good" {
                    Ok(crate::ok(Some(serde_json::json!({"found": true}))).into())
                } else {
                    Ok(crate::fail(
                        "not_found",
                        format!("no row {id}"),
                        Some("list rows via /index".into()),
                    )
                    .into())
                }
            },
        );
        let mut registry = HashMap::new();
        registry.insert("lookup".to_string(), tool);

        let results = execute_tools(
            &[ToolCall {
                id: CompactString::new("1"),
                name: CompactString::new("lookup"),
                arguments: serde_json::json!({"id": "missing"}),
            }],
            &registry,
        )
        .await;
        let parsed: serde_json::Value =
            serde_json::from_str(results[0].output.as_text().unwrap()).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["code"], "not_found");
        assert_eq!(parsed["error"], "no row missing");
        assert_eq!(parsed["hint"], "list rows via /index");
    }

    #[tokio::test]
    async fn safe_tool_converts_tool_fail_throw_into_fail_envelope() {
        let tool = crate::safe_tool(
            "section_read",
            "Read",
            serde_json::json!({ "type": "object" }),
            |args| async move {
                let heading = args["heading"].as_str().unwrap_or("");
                Err(crate::tools::tool_fail(
                    format!(r#"no section "{heading}""#),
                    Some("not_found".into()),
                    Some("call document_outline first".into()),
                ))
            },
        );
        let mut registry = HashMap::new();
        registry.insert("section_read".to_string(), tool);
        let results = execute_tools(
            &[ToolCall {
                id: CompactString::new("1"),
                name: CompactString::new("section_read"),
                arguments: serde_json::json!({"heading": "X"}),
            }],
            &registry,
        )
        .await;
        let parsed: serde_json::Value =
            serde_json::from_str(results[0].output.as_text().unwrap()).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["code"], "not_found");
        assert_eq!(parsed["error"], r#"no section "X""#);
        assert_eq!(parsed["hint"], "call document_outline first");
    }

    #[tokio::test]
    async fn safe_tool_uses_internal_code_for_generic_error() {
        let tool = crate::safe_tool(
            "crash",
            "Crash",
            serde_json::json!({ "type": "object" }),
            |_args| async move { Err(crate::Error::Other("kaboom".into())) },
        );
        let mut registry = HashMap::new();
        registry.insert("crash".to_string(), tool);
        let results = execute_tools(
            &[ToolCall {
                id: CompactString::new("1"),
                name: CompactString::new("crash"),
                arguments: serde_json::json!({}),
            }],
            &registry,
        )
        .await;
        let parsed: serde_json::Value =
            serde_json::from_str(results[0].output.as_text().unwrap()).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["code"], "internal");
        assert_eq!(parsed["error"], "kaboom");
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
    fn validate_tool_arguments_additional_properties_true_keeps_keys() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "bag": { "type": "object", "additionalProperties": true,
                         "properties": { "kind": { "type": "string" } } }
            }
        });
        let mut args = serde_json::json!({
            "bag": { "kind": "a", "anyKey": { "nested": 1 }, "x": [1, 2] }
        });
        validate_tool_arguments(&schema, &mut args).expect("should succeed");
        // arbitrary nested keys survive untouched
        assert_eq!(
            args["bag"],
            serde_json::json!({ "kind": "a", "anyKey": { "nested": 1 }, "x": [1, 2] })
        );
    }

    #[test]
    fn validate_tool_arguments_additional_properties_undefined_still_strips() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "a": { "type": "string" } }
        });
        let mut args = serde_json::json!({ "a": "x", "extra": 1 });
        let repaired = validate_tool_arguments(&schema, &mut args).expect("should succeed");
        assert!(repaired);
        assert_eq!(args, serde_json::json!({ "a": "x" })); // back-compat: extra trimmed
    }

    #[test]
    fn validate_tool_arguments_additional_properties_subschema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": { "type": "number" }
        });
        // "10" gets auto-cast by the {type:number} sub-schema → float 10.0
        let mut args = serde_json::json!({ "a": "10", "b": 2 });
        validate_tool_arguments(&schema, &mut args).expect("should succeed");
        assert_eq!(args, serde_json::json!({ "a": 10.0, "b": 2 }));

        let mut bad = serde_json::json!({ "a": { "not": "a number" } });
        assert!(validate_tool_arguments(&schema, &mut bad).is_err());
    }

    #[test]
    fn validate_tool_arguments_coerces_item_array() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "ops": { "type": "array", "items": {
                    "type": "object", "properties": { "op": { "type": "string" } },
                    "required": ["op"] } }
            },
            "required": ["ops"]
        });

        // { item: [...] } unwraps
        let mut a = serde_json::json!({ "ops": { "item": [{ "op": "add" }, { "op": "remove" }] } });
        assert!(validate_tool_arguments(&schema, &mut a).expect("ok"));
        assert_eq!(
            a["ops"],
            serde_json::json!([{ "op": "add" }, { "op": "remove" }])
        );

        // { items: {obj} } wraps a single object
        let mut b = serde_json::json!({ "ops": { "items": { "op": "add" } } });
        validate_tool_arguments(&schema, &mut b).expect("ok");
        assert_eq!(b["ops"], serde_json::json!([{ "op": "add" }]));

        // lone object wraps
        let mut c = serde_json::json!({ "ops": { "op": "add" } });
        validate_tool_arguments(&schema, &mut c).expect("ok");
        assert_eq!(c["ops"], serde_json::json!([{ "op": "add" }]));

        // precise per-element error restored after coercion
        let mut d = serde_json::json!({ "ops": { "item": { "path": "/x" } } });
        assert_eq!(
            validate_tool_arguments(&schema, &mut d).unwrap_err(),
            "$.ops[0].op is required"
        );

        // well-formed array untouched (no repair)
        let mut e = serde_json::json!({ "ops": [{ "op": "add" }] });
        assert!(!validate_tool_arguments(&schema, &mut e).expect("ok"));
        assert_eq!(e["ops"], serde_json::json!([{ "op": "add" }]));
    }

    #[test]
    fn validate_tool_arguments_oneof_polymorphic() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "oneOf": [
                    { "type": "string" },
                    { "type": "object", "properties": { "path": { "type": "string" } },
                      "required": ["path"] }
                ] }
            },
            "required": ["text"]
        });

        let mut scalar = serde_json::json!({ "text": "hello" });
        validate_tool_arguments(&schema, &mut scalar).expect("scalar branch");
        assert_eq!(scalar["text"], "hello");

        let mut binding = serde_json::json!({ "text": { "path": "/k" } });
        validate_tool_arguments(&schema, &mut binding).expect("object branch");
        assert_eq!(binding["text"], serde_json::json!({ "path": "/k" }));

        let mut bad = serde_json::json!({ "text": 123 });
        assert!(validate_tool_arguments(&schema, &mut bad).is_err());
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
    fn attempt_request_builder() {
        let req = crate::harness_loop::AttemptRequest::new("session-1", "Write a haiku");
        assert_eq!(req.session_id, "session-1");
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
            _context: &deepstrike_core::context::renderer::InternalRenderedContext,
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
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 2048,
            max_turns: Some(4),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
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

    /// P0-C: a provider that emits a usage event (with a cache split) then finishes — one turn,
    /// no tool calls.
    struct MetricsProvider;

    #[async_trait::async_trait]
    impl crate::providers::LLMProvider for MetricsProvider {
        async fn stream(
            &self,
            _context: &deepstrike_core::context::renderer::InternalRenderedContext,
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
            Ok(Box::new(futures::stream::iter(vec![
                Ok(crate::providers::StreamEvent::Usage {
                    total_tokens: 1050,
                    input_tokens: 1000,
                    output_tokens: 50,
                    cache_read_input_tokens: 900,
                    cache_creation_input_tokens: 100,
                    cache_read_input_tokens_by_slot: None,
                    stop_reason: None,
                }),
                Ok(crate::providers::StreamEvent::TextDelta {
                    delta: "done".into(),
                }),
            ])))
        }
    }

    #[tokio::test]
    async fn on_turn_metrics_reports_exposure_and_cache_split() {
        use crate::runtime::{
            InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner, TurnMetrics,
        };
        use crate::tools::RegisteredTool;
        use futures::StreamExt;
        use std::sync::Arc;
        let captured: Arc<std::sync::Mutex<Vec<TurnMetrics>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = captured.clone();

        let mut plane = LocalExecutionPlane::new();
        for name in ["read", "write"] {
            plane.register(RegisteredTool::text(
                name,
                "tool",
                serde_json::json!({ "type": "object", "properties": {} }),
                |_args| Box::pin(async { Ok("ok".into()) }),
            ));
        }

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MetricsProvider),
            execution_plane: Some(Box::new(plane)),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 2048,
            max_turns: Some(2),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: Some(vec!["read".into(), "write".into()]),
            on_turn_metrics: Some(Arc::new(move |m| sink.lock().unwrap().push(m))),
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let mut stream = runner.run_streaming("go", &[], None, None).await.unwrap();
        while stream.next().await.transpose().unwrap().is_some() {}

        let seen = captured.lock().unwrap();
        assert!(!seen.is_empty(), "expected at least one turn metric");
        let m = &seen[0];
        assert_eq!(m.tools_exposed, 2);
        assert_eq!(m.tools_called, 0);
        assert_eq!(m.input_tokens, 1000);
        assert_eq!(m.cache_read_tokens, 900);
        assert_eq!(m.cache_creation_tokens, 100);
        assert!(m.active_skill.is_none());
    }

    /// P1-B B3: turn 1 loads a skill (via a `skill` tool call); turn 2 finishes.
    struct GatingProvider {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::providers::LLMProvider for GatingProvider {
        async fn stream(
            &self,
            _context: &deepstrike_core::context::renderer::InternalRenderedContext,
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
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let evt = if n == 1 {
                crate::providers::StreamEvent::ToolCall {
                    id: "s1".into(),
                    name: "skill".into(),
                    arguments: serde_json::json!({ "name": "debug" }),
                }
            } else {
                crate::providers::StreamEvent::TextDelta {
                    delta: "done".into(),
                }
            };
            Ok(Box::new(futures::stream::iter(vec![Ok(evt)])))
        }
    }

    #[tokio::test]
    async fn active_skill_gates_exposed_tools_e2e() {
        use crate::runtime::{
            InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner, TurnMetrics,
        };
        use crate::tools::RegisteredTool;
        use futures::StreamExt;
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Arc, Mutex};

        let dir = std::env::temp_dir().join(format!("ds-gate-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("debug.md"),
            "---\nname: debug\ndescription: Debug\nallowed_tools: read, grep\n---\nbody",
        )
        .unwrap();

        let exposures: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = exposures.clone();

        let mut plane = LocalExecutionPlane::new();
        for name in ["read", "write", "bash", "grep"] {
            plane.register(RegisteredTool::text(
                name,
                "tool",
                serde_json::json!({ "type": "object", "properties": {} }),
                |_args| Box::pin(async { Ok("ok".into()) }),
            ));
        }

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(GatingProvider {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            execution_plane: Some(Box::new(plane)),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 4096,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: Some(dir.clone()),
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: Some(Arc::new(move |m: TurnMetrics| {
                sink.lock().unwrap().push(m.tools_exposed)
            })),
            stable_core_tool_ids: vec!["bash".to_string()],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let mut stream = runner.run_streaming("go", &[], None, None).await.unwrap();
        while stream.next().await.transpose().unwrap().is_some() {}

        let e = exposures.lock().unwrap();
        assert!(e.len() >= 2, "expected ≥2 turns, got {e:?}");
        // Turn 1: the minimal surface exposes bash (stable-core) and skill (meta).
        assert_eq!(e[0], 2, "turn-1 exposure {e:?}");
        // Turn 2: read+grep (declared) widen the surface alongside bash and skill.
        assert_eq!(*e.last().unwrap(), 4, "post-load exposure {e:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct TooLongThenOkProvider {
        call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        /// Rendered history bytes the provider received on each call.
        sent_history: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
    }

    #[async_trait::async_trait]
    impl crate::providers::LLMProvider for TooLongThenOkProvider {
        async fn stream(
            &self,
            context: &deepstrike_core::context::renderer::InternalRenderedContext,
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
            self.sent_history.lock().unwrap().push(
                context
                    .turns
                    .iter()
                    .map(|turn| turn.content.as_text().map_or(0, str::len))
                    .sum(),
            );
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if n == 1 {
                return Err(crate::providers::ProviderError::new(
                    "test",
                    crate::providers::ProviderErrorKind::ContextOverflow,
                    false,
                    "request exceeded the context window",
                )
                .into());
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
        check_reactive_context_recovery(20, true).await;
    }

    #[tokio::test]
    async fn runner_reactive_stops_when_initial_compaction_exhausts_history() {
        check_reactive_context_recovery(1, false).await;
    }

    async fn check_reactive_context_recovery(seed_turns: usize, expect_recovery: bool) {
        use crate::runtime::{InMemorySessionLog, RuntimeOptions, RuntimeRunner, SessionLog};
        use futures::StreamExt;
        use std::sync::Arc;

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sent_history = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(TooLongThenOkProvider {
                call_count: call_count.clone(),
                sent_history: sent_history.clone(),
            }),
            execution_plane: None,
            session_log: Some(session_log.clone()),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1_000,
            max_turns: Some(4),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let session_id = "reactive-compact-rust";
        for turn in 0..seed_turns {
            session_log
                .append(
                    session_id,
                    deepstrike_core::runtime::session::SessionEvent::RunStarted {
                        run_id: format!("seed-{turn}"),
                        goal: "seed ".repeat(1200),
                        criteria: vec![],
                        agent_id: None,
                        system_prompt: None,
                        attachments: vec![],
                        route: None,
                        metadata: None,
                    },
                )
                .await
                .unwrap();
            session_log
                .append(
                    session_id,
                    deepstrike_core::runtime::session::SessionEvent::llm_completed(
                        0,
                        deepstrike_core::types::message::CoreMessage {
                            role: deepstrike_core::types::message::Role::Assistant,
                            content: deepstrike_core::types::message::Content::Text(
                                "prior answer ".repeat(400),
                            ),
                            tool_calls: vec![],
                        },
                        None,
                    ),
                )
                .await
                .unwrap();
            session_log
                .append(
                    session_id,
                    deepstrike_core::runtime::session::SessionEvent::RunTerminal {
                        reason: "completed".to_string(),
                        turns_used: 1,
                        total_tokens: 0,
                    },
                )
                .await
                .unwrap();
        }

        // The goal fits the fixed budget. With many prior turns, proactive compaction leaves
        // history above the recent-unit floor for the reactive ladder to archive. With only one
        // prior turn, the initial snip already exhausts compaction: retrying would send the same
        // rejected prompt again, so the runner must report context overflow without retrying.
        let goal = "resume the prior task".to_string();
        let mut stream = runner
            .run_streaming(&goal, &[], None, Some(session_id))
            .await
            .unwrap();
        let mut text = String::new();
        let mut errors = vec![];
        let mut status = None;
        while let Some(evt) = stream.next().await {
            match evt.unwrap() {
                crate::RunEvent::TextDelta(delta) => text.push_str(&delta),
                crate::RunEvent::Error(error) => errors.push(error),
                crate::RunEvent::Done {
                    status: final_status,
                    ..
                } => status = Some(final_status),
                _ => {}
            }
        }

        let expected_calls = if expect_recovery { 2 } else { 1 };
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            expected_calls
        );
        if expect_recovery {
            assert_eq!(text, "recovered");
            assert!(
                errors.is_empty(),
                "recovered errors must stay hidden: {errors:?}"
            );
            assert_eq!(status.as_deref(), Some("completed"));
        } else {
            assert!(text.is_empty());
            assert_eq!(errors, ["request exceeded the context window"]);
            assert_eq!(status.as_deref(), Some("contextoverflow"));
        }

        let entries = session_log.read(session_id, 0, None).await.unwrap();
        assert!(
            entries.iter().any(|entry| matches!(
                entry.event,
                deepstrike_core::runtime::session::SessionEvent::Compressed { .. }
            )),
            "inherited history must compact before the first provider request"
        );
        let sent_history = sent_history.lock().unwrap();
        assert_eq!(sent_history.len(), expected_calls);
        if expect_recovery {
            assert!(
                sent_history[1] < sent_history[0],
                "the retry must actually reduce the rejected prompt: {sent_history:?}"
            );
        }
    }

    #[tokio::test]
    async fn recoverable_tool_failure_preserves_replay_context() {
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::replay::replay_messages;
        use crate::runtime::runner::{RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use deepstrike_core::types::message::{Content, CoreMessage, Role, ToolCall};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

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
                _context: &deepstrike_core::context::renderer::InternalRenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
            ) -> crate::Result<CoreMessage> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    Ok(CoreMessage {
                        role: Role::Assistant,
                        content: Content::Text("Let's call tool".into()),
                        tool_calls: vec![ToolCall {
                            id: compact_str::CompactString::new("call_1"),
                            name: compact_str::CompactString::new("fail_tool"),
                            arguments: serde_json::json!({}),
                        }],
                    })
                } else {
                    Ok(CoreMessage {
                        role: Role::Assistant,
                        content: Content::Text("Recovered".into()),
                        tool_calls: vec![],
                    })
                }
            }
            async fn stream(
                &self,
                context: &deepstrike_core::context::renderer::InternalRenderedContext,
                tools: &[deepstrike_core::types::message::ToolSchema],
                extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<
                Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>,
            > {
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
            |_args| Box::pin(async { Err(crate::Error::Tool("Tool crashed!".into())) }),
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
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: Some(vec!["fail_tool".into()]),
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
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
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::execution_plane::LocalExecutionPlane;
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use deepstrike_core::types::milestone::{MilestoneContract, MilestonePhase};
        use std::sync::Arc;

        #[derive(Clone)]
        struct FakeProvider;
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::InternalRenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<
                Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>,
            > {
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta {
                        delta: "done".into(),
                    }),
                    Ok(StreamEvent::Done),
                ])))
            }
        }

        let contract =
            MilestoneContract::new().phase(MilestonePhase::new("phase1").with_criterion("test"));
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider),
            execution_plane: Some(Box::new(LocalExecutionPlane::new())),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: Some(contract),
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let mut stream = runner
            .run_streaming("test", &[], None, Some("s_auto_rust"))
            .await
            .unwrap();
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
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::execution_plane::LocalExecutionPlane;
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use deepstrike_core::types::milestone::{MilestoneContract, MilestonePhase};
        use std::sync::Arc;

        #[derive(Clone)]
        struct FakeProvider;
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::InternalRenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<
                Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>,
            > {
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta {
                        delta: "done".into(),
                    }),
                    Ok(StreamEvent::Done),
                ])))
            }
        }

        let contract =
            MilestoneContract::new().phase(MilestonePhase::new("phase1").with_criterion("test"));
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider),
            execution_plane: Some(Box::new(LocalExecutionPlane::new())),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::RequireVerifier,
            milestone_contract: Some(contract),
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let mut stream = runner
            .run_streaming("test", &[], None, Some("s_pending_rust"))
            .await
            .unwrap();
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
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::execution_plane::LocalExecutionPlane;
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use deepstrike_core::types::milestone::{
            MilestoneCheckResult, MilestoneContract, MilestonePhase,
        };
        use std::sync::Arc;
        use std::sync::Mutex;

        #[derive(Clone)]
        struct FakeProvider;
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::InternalRenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<
                Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>,
            > {
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta {
                        delta: "done".into(),
                    }),
                    Ok(StreamEvent::Done),
                ])))
            }
        }

        let contract =
            MilestoneContract::new().phase(MilestonePhase::new("phase1").with_criterion("test"));
        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();

        let verifier = Arc::new(move |ctx: crate::runtime::MilestoneEvaluationContext| {
            let called_clone = called_clone.clone();
            Box::pin(async move {
                assert_eq!(ctx.phase_id, "phase1");
                assert_eq!(ctx.criteria, vec!["test".to_string()]);
                *called_clone.lock().unwrap() = true;
                Ok(MilestoneCheckResult::pass(ctx.phase_id))
            })
                as futures::future::BoxFuture<'static, crate::Result<MilestoneCheckResult>>
        });

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider),
            execution_plane: Some(Box::new(LocalExecutionPlane::new())),
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::RequireVerifier,
            milestone_contract: Some(contract),
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: Some(verifier),
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let mut stream = runner
            .run_streaming("test", &[], None, Some("s_callback_rust"))
            .await
            .unwrap();
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

    use crate::memory::MemoryStore;
    use crate::runtime::InMemorySessionLog;
    use deepstrike_core::mm::memory::{
        MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord,
        MemoryScope, MemoryTrustLevel,
    };

    fn memory_record(name: &str, content: &str) -> MemoryRecord {
        MemoryRecord {
            record_id: format!("record-{name}"),
            scope: MemoryScope::new("agent-memory", "rust-tests"),
            name: name.into(),
            kind: MemoryKind::Feedback,
            content: content.into(),
            description: format!("test memory {name}"),
            provenance: MemoryProvenance {
                session_id: None,
                author: MemoryAuthor::Host,
                trust: MemoryTrustLevel::HostVerified,
                evidence_refs: Vec::new(),
            },
            created_at: 1,
            updated_at: 1,
            last_recalled_at: None,
            recall_count: 0,
            confidence: 0.9,
            links: Vec::new(),
            pinned: false,
            ttl_days: None,
        }
    }

    fn memory_recall(record: MemoryRecord) -> MemoryRecall {
        MemoryRecall {
            record,
            score: 0.9,
            why: "fixture".into(),
        }
    }

    struct MockLLMProvider;

    #[async_trait::async_trait]
    impl crate::providers::LLMProvider for MockLLMProvider {
        async fn stream(
            &self,
            _context: &deepstrike_core::context::renderer::InternalRenderedContext,
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
    async fn test_page_out_observation_does_not_trigger_memory_store_io() {
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use deepstrike_core::runtime::kernel::{KernelObservation, KernelPressureAction};
        use std::sync::Arc;

        let memories = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));

        struct SharedMockMemoryStore {
            memories: Arc<std::sync::Mutex<Vec<MemoryRecord>>>,
            sessions: Arc<std::sync::Mutex<Vec<deepstrike_core::memory::durable::SessionData>>>,
        }

        #[async_trait::async_trait]
        impl MemoryStore for SharedMockMemoryStore {
            async fn put(&self, _agent_id: &str, record: MemoryRecord) -> crate::Result<()> {
                self.memories.lock().unwrap().push(record);
                Ok(())
            }
            async fn get(
                &self,
                _agent_id: &str,
                record_id: &str,
            ) -> crate::Result<Option<MemoryRecord>> {
                Ok(self
                    .memories
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|record| record.record_id == record_id)
                    .cloned())
            }
            async fn delete(&self, _agent_id: &str, record_id: &str) -> crate::Result<()> {
                self.memories
                    .lock()
                    .unwrap()
                    .retain(|record| record.record_id != record_id);
                Ok(())
            }
            async fn search(
                &self,
                _agent_id: &str,
                _query: &MemoryQuery,
            ) -> crate::Result<Vec<MemoryRecall>> {
                Ok(self
                    .memories
                    .lock()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .map(memory_recall)
                    .collect())
            }
            async fn save_session(
                &self,
                data: deepstrike_core::memory::durable::SessionData,
            ) -> crate::Result<()> {
                self.sessions.lock().unwrap().push(data);
                Ok(())
            }
        }

        let store = SharedMockMemoryStore {
            memories: memories.clone(),
            sessions: sessions.clone(),
        };

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MockLLMProvider),
            execution_plane: None,
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("test-agent".to_string()),
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: Some(Box::new(store)),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        // A committed page-out observation is telemetry only. Host persistence must have
        // happened while executing ArchivePageOut, never as an observation side effect.
        let mut obs = vec![KernelObservation::PageOutArchived {
            turn: 1,
            action: KernelPressureAction::AutoCompact,
            summary: Some("PageOut summary".to_string()),
            tier: "semantic".to_string(),
            message_count: 1,
            archive_ref: Some("archive://batch".to_string()),
        }];

        let kernel = Arc::new(tokio::sync::Mutex::new(
            crate::runtime::canonical_runner_runtime::CanonicalRunnerRuntime::new(
                crate::runtime::canonical_kernel::CanonicalKernel::default(),
                Arc::new(crate::runtime::InMemoryKernelJournal::new()),
                "test-session",
                crate::runtime::canonical_runner_runtime::CanonicalRunnerOptions {
                    max_context_tokens: 1_000,
                    max_turns: Some(3),
                    max_total_tokens: None,
                    max_wall_ms: None,
                    memory_binding_id: "test-memory".into(),
                    persist_payload: None,
                },
            )
            .unwrap(),
        ));
        runner
            .append_observations(
                "test-session",
                &kernel,
                &mut obs,
                &mut std::collections::VecDeque::new(),
                0,
            )
            .await;

        let mems = memories.lock().unwrap();
        assert!(mems.is_empty());
    }

    #[tokio::test]
    async fn test_write_memory_syscall_commits_to_memory_store() {
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::SessionLog;
        use std::sync::Arc;

        let memories = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));

        struct Store {
            memories: Arc<std::sync::Mutex<Vec<MemoryRecord>>>,
            sessions: Arc<std::sync::Mutex<Vec<deepstrike_core::memory::durable::SessionData>>>,
        }

        #[async_trait::async_trait]
        impl MemoryStore for Store {
            async fn put(&self, _agent_id: &str, record: MemoryRecord) -> crate::Result<()> {
                self.memories.lock().unwrap().push(record);
                Ok(())
            }
            async fn get(
                &self,
                _agent_id: &str,
                record_id: &str,
            ) -> crate::Result<Option<MemoryRecord>> {
                Ok(self
                    .memories
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|record| record.record_id == record_id)
                    .cloned())
            }
            async fn delete(&self, _agent_id: &str, record_id: &str) -> crate::Result<()> {
                self.memories
                    .lock()
                    .unwrap()
                    .retain(|record| record.record_id != record_id);
                Ok(())
            }
            async fn search(
                &self,
                _agent_id: &str,
                _query: &MemoryQuery,
            ) -> crate::Result<Vec<MemoryRecall>> {
                Ok(self
                    .memories
                    .lock()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .map(memory_recall)
                    .collect())
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
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("agent-memory".to_string()),
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: Some(Box::new(Store {
                memories: memories.clone(),
                sessions,
            })),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        runner
            .write_memory(
                memory_record(
                    "prefers-small-tests",
                    "User prefers focused unit tests for SDK behavior.",
                ),
                Some("memory-syscall-rs"),
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            memories.lock().unwrap()[0].content,
            "User prefers focused unit tests for SDK behavior."
        );
        let events = session_log
            .read("memory-syscall-rs", 0, None)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event.kind_str() == "memory_written")
        );
    }

    #[tokio::test]
    /// P1-15 · a host write with no live operation is judged by the kernel's own rule. The
    /// rolling write quota is a per-operation kernel ledger; the SDK no longer keeps a second one.
    async fn test_memory_policy_judges_a_write_with_no_live_run() {
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::SessionLog;
        use deepstrike_core::runtime::kernel::wire::MemoryPolicy;
        use std::sync::Arc;

        let commits = Arc::new(std::sync::Mutex::new(0usize));

        struct Store {
            commits: Arc<std::sync::Mutex<usize>>,
        }

        #[async_trait::async_trait]
        impl MemoryStore for Store {
            async fn put(&self, _agent_id: &str, _record: MemoryRecord) -> crate::Result<()> {
                *self.commits.lock().unwrap() += 1;
                Ok(())
            }
            async fn get(
                &self,
                _agent_id: &str,
                _record_id: &str,
            ) -> crate::Result<Option<MemoryRecord>> {
                Ok(None)
            }
            async fn delete(&self, _agent_id: &str, _record_id: &str) -> crate::Result<()> {
                Ok(())
            }
            async fn search(
                &self,
                _agent_id: &str,
                _query: &MemoryQuery,
            ) -> crate::Result<Vec<MemoryRecall>> {
                Ok(vec![])
            }
            async fn save_session(
                &self,
                _data: deepstrike_core::memory::durable::SessionData,
            ) -> crate::Result<()> {
                Ok(())
            }
        }

        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MockLLMProvider),
            execution_plane: None,
            session_log: Some(session_log.clone()),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("agent-memory".to_string()),
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: Some(Box::new(Store {
                commits: commits.clone(),
            })),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: Some(MemoryPolicy {
                max_content_bytes: Some(8),
                ..MemoryPolicy::default()
            }),
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let admitted = runner
            .write_memory(
                memory_record("too-large", "This write should not be committed."),
                Some("memory-quota-rs"),
                None,
            )
            .await
            .unwrap();

        assert!(!admitted);
        assert_eq!(*commits.lock().unwrap(), 0);
        let events = session_log.read("memory-quota-rs", 0, None).await.unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event.kind_str() == "memory_validation_failed")
        );
    }

    #[test]
    fn test_public_agent_os_shape_helpers() {
        use crate::{
            DEFAULT_NATIVE_SIGNAL_POLICY, MemoryWriteRateLimit, OsProfile, SchedulerPolicyConfig,
            assert_native_profile,
        };

        let profile = assert_native_profile(Some(OsProfile::Native)).unwrap();
        assert_eq!(profile.id, "native");
        assert_eq!(
            profile.signal_policy.queue_max,
            DEFAULT_NATIVE_SIGNAL_POLICY.queue_max,
        );

        let scheduler_policy = SchedulerPolicyConfig {
            critical_path_weight: 1_000_000,
            fanout_weight: 10_000,
            age_weight: 1_000,
            token_cost_weight: 1,
            deadline_weight: 0,
            process_priority_weight: 0,
            resource_pressure_weight: 0,
            budget_pressure_weight: 0,
        };

        let json = serde_json::to_value(scheduler_policy).unwrap();
        assert_eq!(json["critical_path_weight"], 1_000_000);
        assert!(json.get("scheduler_max_wall_ms").is_none());

        let write_limit: (u32, u64) = MemoryWriteRateLimit {
            max_writes: 3,
            window_ms: 1000,
        }
        .into();
        assert_eq!(write_limit, (3, 1000));
    }

    #[tokio::test]
    async fn test_query_memory_syscall_returns_memory_store_hits() {
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::SessionLog;
        use std::sync::Arc;

        let memories = Arc::new(std::sync::Mutex::new(vec![memory_record(
            "testing",
            "Use small focused tests.",
        )]));
        let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));

        struct Store {
            memories: Arc<std::sync::Mutex<Vec<MemoryRecord>>>,
            sessions: Arc<std::sync::Mutex<Vec<deepstrike_core::memory::durable::SessionData>>>,
        }

        #[async_trait::async_trait]
        impl MemoryStore for Store {
            async fn put(&self, _agent_id: &str, _record: MemoryRecord) -> crate::Result<()> {
                Ok(())
            }
            async fn get(
                &self,
                _agent_id: &str,
                record_id: &str,
            ) -> crate::Result<Option<MemoryRecord>> {
                Ok(self
                    .memories
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|record| record.record_id == record_id)
                    .cloned())
            }
            async fn delete(&self, _agent_id: &str, record_id: &str) -> crate::Result<()> {
                self.memories
                    .lock()
                    .unwrap()
                    .retain(|record| record.record_id != record_id);
                Ok(())
            }
            async fn search(
                &self,
                _agent_id: &str,
                query: &MemoryQuery,
            ) -> crate::Result<Vec<MemoryRecall>> {
                if query.query.contains("tests") && query.top_k == 1 {
                    Ok(self
                        .memories
                        .lock()
                        .unwrap()
                        .clone()
                        .into_iter()
                        .map(memory_recall)
                        .collect())
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
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("agent-memory".to_string()),
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: Some(Box::new(Store { memories, sessions })),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let hits = runner
            .query_memory(
                MemoryQuery {
                    scope: MemoryScope::new("agent-memory", "rust-tests"),
                    query: "Need memory about tests".to_string(),
                    top_k: 1,
                    kinds: Vec::new(),
                    min_score: None,
                },
                Some("memory-query-syscall-rs"),
                None,
            )
            .await
            .unwrap();

        assert_eq!(hits[0].record.content, "Use small focused tests.");
        let events = session_log
            .read("memory-query-syscall-rs", 0, None)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event.kind_str() == "memory_queried")
        );
        assert!(
            events
                .iter()
                .any(|e| e.event.kind_str() == "memory_retrieval_result")
        );
    }

    #[tokio::test]
    async fn test_write_memory_validation_failure_is_logged() {
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::SessionLog;
        use std::sync::Arc;

        struct Store;
        #[async_trait::async_trait]
        impl MemoryStore for Store {
            async fn put(&self, _agent_id: &str, _record: MemoryRecord) -> crate::Result<()> {
                Ok(())
            }
            async fn get(
                &self,
                _agent_id: &str,
                _record_id: &str,
            ) -> crate::Result<Option<MemoryRecord>> {
                Ok(None)
            }
            async fn delete(&self, _agent_id: &str, _record_id: &str) -> crate::Result<()> {
                Ok(())
            }
            async fn search(
                &self,
                _agent_id: &str,
                _query: &MemoryQuery,
            ) -> crate::Result<Vec<MemoryRecall>> {
                Ok(vec![])
            }
            async fn save_session(
                &self,
                _data: deepstrike_core::memory::durable::SessionData,
            ) -> crate::Result<()> {
                Ok(())
            }
        }

        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MockLLMProvider),
            execution_plane: None,
            session_log: Some(session_log.clone()),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: Some("agent-memory".to_string()),
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: Some(Box::new(Store)),
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::AutoPass,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let mut invalid = memory_record("invalid", "invalid write");
        invalid.name.clear();
        invalid.description = "missing name".into();
        runner
            .write_memory(invalid, Some("memory-validation-fail-rs"), None)
            .await
            .unwrap();

        let events = session_log
            .read("memory-validation-fail-rs", 0, None)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event.kind_str() == "memory_validation_failed")
        );
        assert!(
            !events
                .iter()
                .any(|e| e.event.kind_str() == "memory_written")
        );
    }

    #[test]
    fn workflow_spec_reexport_builds_and_validates() {
        use crate::{WorkflowSpec, fanout_synthesize};
        use deepstrike_core::types::task::RuntimeTask;

        let spec: WorkflowSpec = fanout_synthesize(
            vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
            RuntimeTask::new("synth"),
        );
        assert_eq!(spec.nodes.len(), 3);
        spec.validate().expect("valid dag");
        // workers ready first; synth gated behind both.
        let mut graph = spec.to_task_graph().expect("graph");
        assert_eq!(graph.ready_tasks(), vec![0, 1]);
    }

    // ---------------------------------------------------------------------------------------
    // Task 0 pre-migration stop-the-bleeding regressions (canonical-kernel-abi Phase 0).
    // ---------------------------------------------------------------------------------------

    /// R-B29 / Task 21b: a canonical transition rejection must reach the runner as `Result::Err`,
    /// never as a panic or a silently dropped fault projection.
    #[tokio::test]
    async fn kernel_apply_surfaces_canonical_rejection() {
        use crate::runtime::runner::kernel_apply;
        use std::sync::Arc;

        let kernel = Arc::new(tokio::sync::Mutex::new(
            crate::runtime::canonical_runner_runtime::CanonicalRunnerRuntime::new(
                crate::runtime::canonical_kernel::CanonicalKernel::default(),
                Arc::new(crate::runtime::InMemoryKernelJournal::new()),
                "canonical-rejection",
                crate::runtime::canonical_runner_runtime::CanonicalRunnerOptions {
                    max_context_tokens: 1_000,
                    max_turns: Some(3),
                    max_total_tokens: None,
                    max_wall_ms: None,
                    memory_binding_id: "test-memory".into(),
                    persist_payload: None,
                },
            )
            .unwrap(),
        ));

        let mut observations = Vec::new();
        {
            let mut runtime = kernel.lock().await;
            runtime
                .start_agent_value(
                    serde_json::json!({
                        "goal": "exercise canonical rejection",
                        "criteria": [],
                    }),
                    None,
                )
                .await
                .expect("start must produce a pending provider effect");
        }

        let err = kernel_apply(
            &kernel,
            &mut observations,
            serde_json::json!({
                "kind": "provider_result",
                "effect_id": "not-the-pending-effect",
                "message": {
                    "role": "assistant",
                    "content": "unexpected",
                    "tool_calls": [],
                },
            }),
        )
        .await
        .expect_err("a faulted apply-only transition must not report success");
        let msg = err.to_string();
        assert!(
            msg.contains("unexpected_effect_outcome"),
            "fault code must survive into the host error: {msg}"
        );
    }

    /// End-to-end companion: the `?` threading actually reaches the caller — a rejected
    /// `add_system_message` ends the run as a stream-level `Err` instead of silently running with
    /// a system prompt the kernel refused.
    #[tokio::test]
    async fn runner_surfaces_apply_path_kernel_fault_to_the_caller() {
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::runner::{
            KernelReliability, MilestonePolicy, RuntimeOptions, RuntimeRunner,
        };
        use crate::runtime::session_log::InMemorySessionLog;
        use std::sync::Arc;

        struct FakeProvider;
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::InternalRenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<
                Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>,
            > {
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta {
                        delta: "should never run".into(),
                    }),
                    Ok(StreamEvent::Done),
                ])))
            }
        }

        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider),
            execution_plane: None,
            session_log: Some(Arc::new(InMemorySessionLog::new())),
            compression_store: None,
            payload_store: None,
            notes: None,
            kernel_reliability: Some(KernelReliability {
                max_input_bytes: Some(8_192),
                ..KernelReliability::default()
            }),
            session_id: None,
            max_tokens: 100_000,
            max_turns: Some(2),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            // Far over `max_input_bytes` ⇒ the kernel rejects this `add_system_message`.
            system_prompt: Some("s".repeat(32_768)),
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::RequireVerifier,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let mut stream = runner
            .run_streaming("test", &[], None, Some("s_apply_fault_rust"))
            .await
            .unwrap();
        let mut fault_err: Option<String> = None;
        let mut saw_text = false;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(RunEvent::TextDelta(_)) => saw_text = true,
                Ok(_) => {}
                Err(e) => fault_err = Some(e.to_string()),
            }
        }

        let msg = fault_err.expect("a rejected apply-path input must reach the caller as Err");
        assert!(
            msg.contains("resource_limit_exceeded"),
            "fault code must survive into the host error: {msg}"
        );
        assert!(
            !saw_text,
            "the run must not proceed to the provider on a rejected kernel input"
        );
    }

    /// A provider exception raised AFTER the first chunk must be reported to the kernel as
    /// `provider_error` (parity with the stream-open path and with node/python). Before the fix
    /// `evt?` propagated straight out of `try_stream!`: the run ended as a stream-level `Err`
    /// with no terminal, and the `call_provider` effect stayed pending forever.
    #[tokio::test]
    async fn runner_mid_stream_provider_error_is_reported_to_the_kernel() {
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::execution_plane::LocalExecutionPlane;
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use std::sync::Arc;

        struct MidStreamFailProvider;
        #[async_trait::async_trait]
        impl LLMProvider for MidStreamFailProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::InternalRenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<
                Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>,
            > {
                // Opening the stream succeeds; the failure lands after the first chunk.
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta {
                        delta: "partial".into(),
                    }),
                    Err(crate::Error::Provider(
                        "upstream socket closed mid-chunk".into(),
                    )),
                ])))
            }
        }

        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(MidStreamFailProvider),
            execution_plane: Some(Box::new(LocalExecutionPlane::new())),
            session_log: Some(session_log.clone()),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::RequireVerifier,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let session_id = "s_mid_stream_error_rust";
        let mut stream = runner
            .run_streaming("test", &[], None, Some(session_id))
            .await
            .unwrap();

        let mut error_seen: Option<String> = None;
        let mut done_status: Option<String> = None;
        let mut pending_at_error: Option<usize> = None;
        while let Some(evt) = stream.next().await {
            // No stream-level `Err`: the failure travels through the kernel, not around it.
            let evt = evt.expect("mid-stream provider error must not escape as a stream error");
            match evt {
                RunEvent::Error(msg) => {
                    pending_at_error = runner.active_pending_effect_count();
                    error_seen = Some(msg);
                }
                RunEvent::Done { status, .. } => done_status = Some(status),
                _ => {}
            }
        }

        let msg = error_seen.expect("kernel gave up ⇒ the raw provider error is surfaced");
        assert!(
            msg.contains("upstream socket closed mid-chunk"),
            "raw provider message must be forwarded: {msg}"
        );
        // The kernel classified a non-overflow provider failure ⇒ honest `error` terminal.
        assert_eq!(done_status.as_deref(), Some("error"));
        // `provider_error` resolved the in-flight `call_provider` effect: nothing left pending.
        assert_eq!(pending_at_error, Some(0));

        let entries = session_log.read(session_id, 0, None).await.unwrap();
        assert!(entries.iter().any(|entry| matches!(
            &entry.event,
            deepstrike_core::runtime::session::SessionEvent::RunTerminal { reason, .. }
                if reason == "error"
        )));
    }

    /// R-B27: `evaluate_milestone` with neither a verifier policy nor an `on_milestone_evaluate`
    /// hook used to `return` without feeding a result, leaving the milestone effect dangling in
    /// the kernel's `pending_effects` — an unresolvable pending item once checkpoint recovery
    /// lands. The branch now feeds back the most conservative result the current wire can carry
    /// (`passed = false` + `reason`), so the effect is resolved and the phase does NOT advance.
    #[tokio::test]
    async fn runner_milestone_without_verifier_resolves_the_pending_effect() {
        use crate::providers::LLMProvider;
        use crate::providers::StreamEvent;
        use crate::runtime::execution_plane::LocalExecutionPlane;
        use crate::runtime::runner::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use deepstrike_core::types::milestone::{MilestoneContract, MilestonePhase};
        use std::sync::Arc;

        struct FakeProvider;
        #[async_trait::async_trait]
        impl LLMProvider for FakeProvider {
            async fn stream(
                &self,
                _context: &deepstrike_core::context::renderer::InternalRenderedContext,
                _tools: &[deepstrike_core::types::message::ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<
                Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>,
            > {
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta {
                        delta: "done".into(),
                    }),
                    Ok(StreamEvent::Done),
                ])))
            }
        }

        let contract =
            MilestoneContract::new().phase(MilestonePhase::new("phase1").with_criterion("test"));
        let session_log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(FakeProvider),
            execution_plane: Some(Box::new(LocalExecutionPlane::new())),
            session_log: Some(session_log.clone()),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 1000,
            max_turns: Some(3),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: MilestonePolicy::RequireVerifier,
            milestone_contract: Some(contract),
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: None,
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });

        let session_id = "s_milestone_leak_rust";
        let mut stream = runner
            .run_streaming("test", &[], None, Some(session_id))
            .await
            .unwrap();
        let mut done_status: Option<String> = None;
        while let Some(evt) = stream.next().await {
            // Feeding a result for an effect the kernel does NOT have pending raises an
            // `UnexpectedEffectResult` fault, which now surfaces as a stream-level `Err`. Getting
            // a clean `Done` instead is the proof that the milestone effect WAS pending and was
            // consumed (kernel `runtime.rs` removes it on a matching result).
            if let RunEvent::Done { status, .. } = evt.expect("no kernel fault") {
                done_status = Some(status);
            }
        }
        // Host-visible behaviour is unchanged: the run still suspends as `milestone_pending`.
        assert_eq!(done_status.as_deref(), Some("milestone_pending"));

        let entries = session_log.read(session_id, 0, None).await.unwrap();
        let blocked = entries
            .iter()
            .find_map(|entry| match &entry.event {
                deepstrike_core::runtime::session::SessionEvent::MilestoneBlocked {
                    phase_id,
                    reason,
                    ..
                } => Some((phase_id.clone(), reason.clone())),
                _ => None,
            })
            .expect("the unverified milestone must be recorded as blocked, not left dangling");
        assert_eq!(blocked.0, "phase1");
        assert!(
            blocked.1.contains("no verifier"),
            "conservative reason must explain why the phase did not advance: {}",
            blocked.1
        );
        // Fail-closed: an unverified phase never advances and unlocks nothing.
        assert!(!entries.iter().any(|entry| matches!(
            entry.event,
            deepstrike_core::runtime::session::SessionEvent::MilestoneAdvanced { .. }
        )));
    }

    #[test]
    fn rust_production_runtime_does_not_reference_the_retired_kernel_input_abi() {
        let runtime_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime");
        let production_modules = [
            runtime_dir.join("runner.rs"),
            runtime_dir.join("canonical_kernel_step.rs"),
            runtime_dir.join("canonical_runner_runtime.rs"),
            runtime_dir.join("os_profile.rs"),
        ];
        let forbidden = [
            "KernelInputEvent",
            "KernelRuntime",
            "ABI-v1",
            "ABI-v2",
            "\"abi_version\": 3",
            ".step(",
        ];
        let mut hits = Vec::new();

        for path in production_modules {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            for (line_index, line) in source.lines().enumerate() {
                for needle in forbidden {
                    if line.contains(needle) {
                        hits.push(format!(
                            "{}:{} contains {needle:?}: {}",
                            path.display(),
                            line_index + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }

        assert!(
            hits.is_empty(),
            "retired kernel input ABI is still reachable from Rust production modules:\n{}",
            hits.join("\n")
        );
    }

    // ── P1 boundary regressions (0.2.74 audit) ───────────────────────────────────────────────
    mod boundary_p1 {
        use crate::providers::{LLMProvider, StreamEvent};
        use crate::run_event::RunEvent;
        use crate::runtime::runner::{RuntimeOptions, RuntimeRunner};
        use crate::runtime::session_log::{InMemorySessionLog, SessionLog};
        use deepstrike_core::context::renderer::InternalRenderedContext;
        use deepstrike_core::types::message::{CoreMessage, ToolSchema};
        use futures::StreamExt;
        use std::sync::{Arc, Mutex};

        /// Plays one scripted turn per provider call (then "done") and records each context.
        struct Scripted {
            turns: Vec<Vec<StreamEvent>>,
            contexts: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl LLMProvider for Scripted {
            async fn complete(
                &self,
                _context: &InternalRenderedContext,
                _tools: &[ToolSchema],
                _extensions: Option<&serde_json::Value>,
            ) -> crate::Result<CoreMessage> {
                unreachable!("the runner streams")
            }
            async fn stream(
                &self,
                context: &InternalRenderedContext,
                _tools: &[ToolSchema],
                _extensions: Option<&serde_json::Value>,
                _state: Option<&crate::providers::ProviderRunState>,
            ) -> crate::Result<
                Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>,
            > {
                let mut contexts = self.contexts.lock().unwrap();
                contexts.push(serde_json::to_string(context).unwrap_or_default());
                let turn = self
                    .turns
                    .get(contexts.len() - 1)
                    .cloned()
                    .unwrap_or_else(|| {
                        vec![StreamEvent::TextDelta {
                            delta: "done".into(),
                        }]
                    });
                let mut events: Vec<crate::Result<StreamEvent>> =
                    turn.into_iter().map(Ok).collect();
                events.push(Ok(StreamEvent::Done));
                Ok(Box::new(futures::stream::iter(events)))
            }
        }

        fn options(
            provider: Scripted,
            plane: crate::runtime::execution_plane::LocalExecutionPlane,
            tools: &[&str],
        ) -> RuntimeOptions {
            RuntimeOptions {
                provider: Box::new(provider),
                execution_plane: Some(Box::new(plane)),
                session_log: Some(Arc::new(InMemorySessionLog::new())),
                compression_store: None,
                payload_store: None,
                kernel_reliability: None,
                notes: None,
                session_id: None,
                max_tokens: 8000,
                max_turns: Some(5),
                timeout_ms: None,
                extensions: None,
                agent_id: None,
                memory_scope: None,
                system_prompt: None,
                initial_memory: vec![],
                dependency_outputs: Default::default(),
                inherited_knowledge: Vec::new(),
                skill_dir: None,
                memory_store: None,
                knowledge_source: None,
                signal_source: None,
                governance: None,
                os_profile: None,
                governance_policy: None,
                signal_policy: None,
                scheduler_policy: None,
                resource_quota: None,
                memory_policy: None,
                tokenizer: None,
                enable_plan_tool: None,
                on_tool_suspend: None,
                on_permission_request: None,
                milestone_policy: crate::runtime::MilestonePolicy::Terminate,
                milestone_contract: None,
                run_spec: None,
                allowed_tool_ids: None,
                baseline_tool_ids: Some(tools.iter().map(|t| t.to_string()).collect()),
                on_turn_metrics: None,
                stable_core_tool_ids: vec![],
                pre_query_memory: None,
                on_milestone_evaluate: None,
                memory_runtime_policy: None,
                memory_candidate_sink: None,
            }
        }

        async fn drain(runner: &RuntimeRunner, session_id: &str) -> Vec<RunEvent> {
            let mut stream = runner
                .run_streaming("go", &[], None, Some(session_id))
                .await
                .expect("run starts");
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                events.push(event.expect("run event"));
            }
            events
        }

        /// Plan task 6.3 · a model-authored workflow runs its nodes as child runs on the parent's
        /// provider: a dependent node sees its dependency's output, and the parent resumes with the
        /// node results in context.
        #[tokio::test]
        async fn a_model_authored_workflow_runs_its_nodes_as_child_runs() {
            let mut plane = crate::runtime::execution_plane::LocalExecutionPlane::new();
            plane.register(crate::tools::RegisteredTool::text(
                "start_workflow",
                "Author and run a sub-workflow",
                serde_json::json!({ "type": "object", "properties": { "spec": { "type": "object" } } }),
                |_| Box::pin(async { Ok(String::new()) }),
            ));
            let contexts = Arc::new(Mutex::new(Vec::new()));
            let provider = Scripted {
                turns: vec![
                    vec![StreamEvent::ToolCall {
                        id: "c1".into(),
                        name: "start_workflow".into(),
                        arguments: serde_json::json!({ "spec": { "nodes": [
                            { "task": "collect the sources" },
                            { "task": "write the brief", "depends_on": [0] },
                        ] } }),
                    }],
                    vec![StreamEvent::TextDelta { delta: "sources: ALPHA-7".into() }],
                    vec![StreamEvent::TextDelta { delta: "brief written from ALPHA-7".into() }],
                ],
                contexts: contexts.clone(),
            };
            let runner = RuntimeRunner::new(options(provider, plane, &["start_workflow"]));
            let events = drain(&runner, "wf-6-3").await;

            let contexts = contexts.lock().unwrap();
            assert_eq!(contexts.len(), 4, "parent, two nodes, parent again");
            assert!(contexts[1].contains("collect the sources"));
            assert!(contexts[2].contains("write the brief") && contexts[2].contains("ALPHA-7"));
            assert!(contexts[3].contains("brief written from ALPHA-7"), "{}", contexts[3]);
            assert!(matches!(
                events.last(),
                Some(RunEvent::Done { status, .. }) if status == "completed"
            ));
        }

        /// Plan tasks 6.2 / 6.3 · a host program runs agents on a dynamic workflow root and shares
        /// state with them through the blackboard, by scope.
        #[tokio::test]
        async fn a_dynamic_workflow_program_runs_agents_and_shares_a_blackboard() {
            use crate::runtime::dynamic_workflow::{
                AgentOptions, BlackboardScope, DynamicWorkflowLimits, DynamicWorkflowOptions, agent_task,
            };
            let contexts = Arc::new(Mutex::new(Vec::new()));
            let provider = Scripted { turns: vec![], contexts: contexts.clone() };
            let runner = RuntimeRunner::new(options(
                provider,
                crate::runtime::execution_plane::LocalExecutionPlane::new(),
                &[],
            ));
            let run = runner
                .run_dynamic_workflow(DynamicWorkflowOptions::default(), |ctx| {
                    Box::pin(async move {
                        let first = ctx.agent("survey the field", AgentOptions::default()).await?;
                        ctx.blackboard().put("criteria", "primary sources only", BlackboardScope::Workflow).await?;
                        let drafts = ctx
                            .phase("draft", || async {
                                ctx.blackboard()
                                    .put("outline", serde_json::json!({ "sections": 3 }), BlackboardScope::Phase)
                                    .await?;
                                ctx.parallel_agents(vec![
                                    agent_task("draft part one", AgentOptions::default()),
                                    agent_task("draft part two", AgentOptions::default()),
                                ])
                                .await
                            })
                            .await?;
                        let review = ctx
                            .agent("review", AgentOptions { label: Some("reviewer".into()), ..Default::default() })
                            .await?;
                        Ok((first, drafts, review, ctx.blackboard().get("outline")))
                    })
                })
                .await
                .expect("dynamic workflow runs");
            let (first, drafts, review, outline) = run.value;
            assert_eq!(first.map(|r| r.text), Some("done".to_string()));
            assert!(drafts.iter().all(|r| r.as_ref().is_some_and(|r| r.text == "done")));
            assert_eq!(review.map(|r| r.node_id), Some("reviewer".to_string()));
            assert_eq!(outline, None, "a phase-scoped entry left with its phase");
            assert_eq!(run.progress.agents_completed, 4);

            {
                let contexts = contexts.lock().unwrap();
                let of = |goal: &str| contexts.iter().find(|c| c.contains(goal)).cloned().unwrap_or_default();
                assert!(!of("survey the field").contains("primary sources only"));
                for draft in ["draft part one", "draft part two"] {
                    assert!(of(draft).contains("primary sources only") && of(draft).contains("sections"), "{draft}");
                }
                assert!(of("review").contains("primary sources only") && !of("review").contains("sections"));
            }

            let refused = runner
                .run_dynamic_workflow(
                    DynamicWorkflowOptions { limits: DynamicWorkflowLimits { max_agents_per_run: 1, ..Default::default() }, ..Default::default() },
                    |ctx| Box::pin(async move {
                        ctx.agent("one", AgentOptions::default()).await?;
                        ctx.agent("two", AgentOptions::default()).await?;
                        Ok(())
                    }),
                )
                .await;
            assert!(refused.is_err(), "past max_agents_per_run the program fails before spawning");
        }

        /// P1-13 · argument text that is not a JSON object fails the call; the tool never runs.
        #[tokio::test]
        async fn a_truncated_call_never_executes() {
            let executed = Arc::new(Mutex::new(Vec::new()));
            let seen = executed.clone();
            let mut plane = crate::runtime::execution_plane::LocalExecutionPlane::new();
            plane.register(crate::tools::RegisteredTool::text(
                "write",
                "write",
                serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
                move |args| {
                    seen.lock().unwrap().push(args);
                    Box::pin(async { Ok("wrote".to_string()) })
                },
            ));
            let contexts = Arc::new(Mutex::new(Vec::new()));
            let provider = Scripted {
                turns: vec![vec![StreamEvent::ToolCall {
                    id: "c1".into(),
                    name: "write".into(),
                    arguments: crate::runtime::tool_arguments::from_model_text("{\"path\": \"/tm"),
                }]],
                contexts: contexts.clone(),
            };
            let runner = RuntimeRunner::new(options(provider, plane, &["write"]));
            let events = drain(&runner, "p1-13").await;
            assert!(
                executed.lock().unwrap().is_empty(),
                "the tool never ran with invented args"
            );
            let result = events.iter().find_map(|e| match e {
                RunEvent::ToolResult {
                    content, is_error, ..
                } => Some((content.clone(), *is_error)),
                _ => None,
            });
            let (content, is_error) = result.expect("a tool result");
            assert!(
                is_error && content.contains("invalid arguments"),
                "{content}"
            );
            assert!(contexts.lock().unwrap()[1].contains("invalid arguments"));
        }

        /// P1-15 · a store whose search returns fixed hits and which records every recall batch.
        struct RecallStore {
            hits: Vec<deepstrike_core::mm::memory::MemoryRecord>,
            recalls: Arc<Mutex<Vec<(String, u64)>>>,
        }

        #[async_trait::async_trait]
        impl crate::memory::MemoryStore for RecallStore {
            async fn put(
                &self,
                _: &str,
                _: deepstrike_core::mm::memory::MemoryRecord,
            ) -> crate::Result<()> {
                Ok(())
            }
            async fn get(
                &self,
                _: &str,
                record_id: &str,
            ) -> crate::Result<Option<deepstrike_core::mm::memory::MemoryRecord>> {
                Ok(self
                    .hits
                    .iter()
                    .find(|record| record.record_id == record_id)
                    .cloned())
            }
            async fn delete(&self, _: &str, _: &str) -> crate::Result<()> {
                Ok(())
            }
            async fn search(
                &self,
                _: &str,
                _: &deepstrike_core::mm::memory::MemoryQuery,
            ) -> crate::Result<Vec<deepstrike_core::mm::memory::MemoryRecall>> {
                Ok(self
                    .hits
                    .iter()
                    .cloned()
                    .map(|record| deepstrike_core::mm::memory::MemoryRecall {
                        record,
                        score: 0.9,
                        why: "fixture".into(),
                    })
                    .collect())
            }
            async fn save_session(
                &self,
                _: deepstrike_core::memory::durable::SessionData,
            ) -> crate::Result<()> {
                Ok(())
            }
            async fn record_recall(
                &self,
                _: &str,
                recalls: &[deepstrike_core::mm::memory::MemoryRecallLifecycle],
            ) -> crate::Result<()> {
                self.recalls.lock().unwrap().extend(
                    recalls
                        .iter()
                        .map(|r| (r.record_id.clone(), r.recall_count)),
                );
                Ok(())
            }
        }

        fn stored(record_id: &str, recall_count: u64) -> deepstrike_core::mm::memory::MemoryRecord {
            let mut record = super::memory_record(record_id, "fixture body");
            record.record_id = record_id.into();
            record.recall_count = recall_count;
            record
        }

        /// P1-15 · a model memory query: the kernel derives the next count from the stored one
        /// and the runner mirrors it — before, a model recall was never counted at all.
        #[tokio::test]
        async fn a_model_memory_query_mirrors_the_kernel_derived_count() {
            let recalls = Arc::new(Mutex::new(Vec::new()));
            let provider = Scripted {
                turns: vec![vec![StreamEvent::ToolCall {
                    id: "m1".into(),
                    name: "memory".into(),
                    arguments: serde_json::json!({ "query": "prefs" }),
                }]],
                contexts: Arc::new(Mutex::new(Vec::new())),
            };
            let mut opts = options(
                provider,
                crate::runtime::execution_plane::LocalExecutionPlane::new(),
                &[],
            );
            opts.agent_id = Some("p1-15".into());
            opts.memory_scope = Some(deepstrike_core::mm::memory::MemoryScope::new(
                "agent-memory",
                "rust-tests",
            ));
            opts.memory_store = Some(Box::new(RecallStore {
                hits: vec![stored("crossing", 2)],
                recalls: recalls.clone(),
            }));
            let runner = RuntimeRunner::new(opts);
            drain(&runner, "p1-15-query").await;
            // The kernel's start-of-run recall, then the model's query: each mirrors the count
            // the kernel derived from the stored one.
            assert_eq!(
                *recalls.lock().unwrap(),
                vec![("crossing".to_string(), 3), ("crossing".to_string(), 3)]
            );
        }

        /// M5 · `memory(record_id)` reads one record in full; a record outside this agent's memory
        /// scope is not found, not leaked.
        #[tokio::test]
        async fn a_model_memory_fetch_reads_one_record_by_record_id() {
            let contexts = Arc::new(Mutex::new(Vec::new()));
            let provider = Scripted {
                turns: vec![
                    vec![StreamEvent::ToolCall {
                        id: "call_a".into(),
                        name: "memory".into(),
                        arguments: serde_json::json!({ "record_id": "crossing" }),
                    }],
                    vec![StreamEvent::ToolCall {
                        id: "call_b".into(),
                        name: "memory".into(),
                        arguments: serde_json::json!({ "record_id": "elsewhere" }),
                    }],
                ],
                contexts: contexts.clone(),
            };
            let mut opts = options(
                provider,
                crate::runtime::execution_plane::LocalExecutionPlane::new(),
                &[],
            );
            opts.agent_id = Some("p1-15".into());
            opts.memory_scope = Some(deepstrike_core::mm::memory::MemoryScope::new(
                "agent-memory",
                "rust-tests",
            ));
            let mut elsewhere = super::memory_record("elsewhere", "elsewhere body");
            elsewhere.record_id = "elsewhere".into();
            elsewhere.scope = deepstrike_core::mm::memory::MemoryScope::new("agent-memory", "other");
            let mut crossing = super::memory_record("crossing", "crossing body");
            crossing.record_id = "crossing".into();
            opts.memory_store = Some(Box::new(RecallStore {
                hits: vec![crossing, elsewhere],
                recalls: Arc::new(Mutex::new(Vec::new())),
            }));
            let runner = RuntimeRunner::new(opts);
            let events = drain(&runner, "p1-15-fetch").await;
            assert!(
                !events.iter().any(|e| matches!(e, RunEvent::Error(_))),
                "{events:?}"
            );
            let contexts = contexts.lock().unwrap();
            assert!(contexts.len() >= 3, "{contexts:?}");
            assert!(
                contexts[1].contains("[MEMORY record_ref=crossing kind=feedback] crossing body"),
                "{}",
                contexts[1]
            );
            assert!(
                contexts[2].contains("[MEMORY record_ref=elsewhere] not found"),
                "{}",
                contexts[2]
            );
            assert!(!contexts[2].contains("elsewhere body"));
        }

        /// Plan task 3.1 · a run holding a dependency output reads it in full with `read_result`.
        #[tokio::test]
        async fn a_dependency_output_is_read_through_read_result() {
            let contexts = Arc::new(Mutex::new(Vec::new()));
            let provider = Scripted {
                turns: vec![vec![StreamEvent::ToolCall {
                    id: "r1".into(),
                    name: "read_result".into(),
                    arguments: serde_json::json!({ "call_id": "dep:wf-node0" }),
                }]],
                contexts: contexts.clone(),
            };
            let mut opts = options(
                provider,
                crate::runtime::execution_plane::LocalExecutionPlane::new(),
                &[],
            );
            opts.dependency_outputs
                .insert("wf-node0".into(), "FULL_SURVEY_BODY with every finding".into());
            let runner = RuntimeRunner::new(opts);
            let events = drain(&runner, "dep-read").await;
            assert!(!events.iter().any(|e| matches!(e, RunEvent::Error(_))), "{events:?}");
            let contexts = contexts.lock().unwrap();
            assert!(contexts.len() >= 2, "{contexts:?}");
            assert!(
                contexts[1].contains("[PAYLOAD handle_id=dep:wf-node0]")
                    && contexts[1].contains("FULL_SURVEY_BODY with every finding"),
                "{}",
                contexts[1]
            );
        }

        /// P1-15 · a host recall is counted by the kernel's derivation, once per record.
        #[tokio::test]
        async fn a_host_recall_derives_its_counts_in_the_kernel() {
            let recalls = Arc::new(Mutex::new(Vec::new()));
            let provider = Scripted {
                turns: vec![],
                contexts: Arc::new(Mutex::new(Vec::new())),
            };
            let mut opts = options(
                provider,
                crate::runtime::execution_plane::LocalExecutionPlane::new(),
                &[],
            );
            opts.agent_id = Some("p1-15".into());
            opts.memory_store = Some(Box::new(RecallStore {
                hits: vec![stored("a", 1), stored("a", 1)],
                recalls: recalls.clone(),
            }));
            let runner = RuntimeRunner::new(opts);
            runner
                .query_memory(
                    deepstrike_core::mm::memory::MemoryQuery {
                        scope: deepstrike_core::mm::memory::MemoryScope::new(
                            "agent-memory",
                            "rust-tests",
                        ),
                        query: "a".into(),
                        top_k: 5,
                        kinds: Vec::new(),
                        min_score: None,
                    },
                    None,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(*recalls.lock().unwrap(), vec![("a".to_string(), 2)]);
        }

        /// P1-7 · the model's update_plan progress survives a tool batch; the host writes none.
        #[tokio::test]
        async fn the_host_never_writes_the_models_task_state() {
            let mut plane = crate::runtime::execution_plane::LocalExecutionPlane::new();
            plane.register(crate::tools::RegisteredTool::text(
                "noop",
                "noop",
                serde_json::json!({ "type": "object", "properties": {} }),
                |_| Box::pin(async { Ok("ok".to_string()) }),
            ));
            let contexts = Arc::new(Mutex::new(Vec::new()));
            let provider = Scripted {
                turns: vec![
                    vec![StreamEvent::ToolCall {
                        id: "plan".into(),
                        name: "update_plan".into(),
                        arguments: serde_json::json!({ "progress": "drafting section two" }),
                    }],
                    vec![StreamEvent::ToolCall {
                        id: "noop".into(),
                        name: "noop".into(),
                        arguments: serde_json::json!({}),
                    }],
                ],
                contexts: contexts.clone(),
            };
            let mut opts = options(provider, plane, &["noop"]);
            opts.enable_plan_tool = Some(true);
            let runner = RuntimeRunner::new(opts);
            let events = drain(&runner, "p1-7").await;
            assert!(
                !events.iter().any(|e| matches!(e, RunEvent::Error(_))),
                "{events:?}"
            );
            let contexts = contexts.lock().unwrap();
            assert!(contexts.len() >= 3);
            assert!(contexts[2].contains("drafting section two"));
            assert!(!contexts[2].contains("Executed tools"));
        }

        /// P1-12 · a cancellation names logical calls only, never the in-flight effect id.
        #[tokio::test]
        async fn cancellation_names_logical_calls_only() {
            let runner_slot: Arc<Mutex<Option<Arc<RuntimeRunner>>>> = Arc::new(Mutex::new(None));
            let slot = runner_slot.clone();
            let mut plane = crate::runtime::execution_plane::LocalExecutionPlane::new();
            plane.register(crate::tools::RegisteredTool::text(
                "stop_me",
                "stop",
                serde_json::json!({ "type": "object", "properties": {} }),
                move |_| {
                    if let Some(runner) = slot.lock().unwrap().as_ref() {
                        runner.interrupt();
                    }
                    Box::pin(async { Ok("ok".to_string()) })
                },
            ));
            let provider = Scripted {
                turns: vec![vec![StreamEvent::ToolCall {
                    id: "c1".into(),
                    name: "stop_me".into(),
                    arguments: serde_json::json!({}),
                }]],
                contexts: Arc::new(Mutex::new(Vec::new())),
            };
            let log = Arc::new(InMemorySessionLog::new());
            let mut opts = options(provider, plane, &["stop_me"]);
            opts.session_log = Some(log.clone());
            let runner = Arc::new(RuntimeRunner::new(opts));
            *runner_slot.lock().unwrap() = Some(runner.clone());
            drain(&runner, "p1-12").await;
            let entries = log.read("p1-12", 0, None).await.unwrap();
            let pending = entries.iter().find_map(|entry| match &entry.event {
                deepstrike_core::runtime::session::SessionEvent::OperationCancelled {
                    pending_call_ids,
                    ..
                } => Some(pending_call_ids.clone()),
                _ => None,
            });
            assert_eq!(pending, Some(vec![]));
            *runner_slot.lock().unwrap() = None;
        }

        /// P1-14 · the runner's control methods need a run, and apply a revision-guarded patch.
        #[tokio::test]
        async fn control_commands_require_an_active_run() {
            let provider = Scripted {
                turns: vec![],
                contexts: Arc::new(Mutex::new(Vec::new())),
            };
            let runner = RuntimeRunner::new(options(
                provider,
                crate::runtime::execution_plane::LocalExecutionPlane::new(),
                &[],
            ));
            let error = runner.force_compact().await.expect_err("no active run");
            assert!(error.to_string().contains("requires an active run"));
        }
    }
}

/// Plan task 9.1 · notes the model writes with the kernel's `note` tool (mirrors
/// node/tests/notes.test.ts).
mod notes {
    use crate::providers::{LLMProvider, ProviderRunState, StreamEvent};
    use crate::runtime::{InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner, SessionLog};
    use crate::tools::RegisteredTool;
    use deepstrike_core::context::renderer::InternalRenderedContext;
    use deepstrike_core::runtime::session::SessionEvent;
    use deepstrike_core::types::message::ToolSchema;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Script {
        calls: Mutex<u32>,
        offered: Mutex<Vec<Vec<String>>>,
        states: Mutex<Vec<String>>,
    }

    struct ScriptedProvider(Arc<Script>);

    #[async_trait::async_trait]
    impl LLMProvider for ScriptedProvider {
        async fn stream(
            &self,
            context: &InternalRenderedContext,
            tools: &[ToolSchema],
            _extensions: Option<&serde_json::Value>,
            _state: Option<&ProviderRunState>,
        ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>> {
            let script = &self.0;
            let call = {
                let mut calls = script.calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            script.offered.lock().unwrap().push(tools.iter().map(|t| t.name.to_string()).collect());
            script.states.lock().unwrap().push(serde_json::to_string(&context.state_turn).unwrap_or_default());
            let events = match call {
                1 => vec![Ok(StreamEvent::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "logs/a.log" }),
                })],
                2 if tools.iter().any(|t| t.name == "note") => vec![Ok(StreamEvent::ToolCall {
                    id: "c2".into(),
                    name: "note".into(),
                    arguments: serde_json::json!({
                        "title": "Tally", "kind": "state", "body": "3 qualifying lines in logs/a.log"
                    }),
                })],
                _ => vec![Ok(StreamEvent::TextDelta { delta: "3".into() })],
            };
            Ok(Box::new(futures::stream::iter(events)))
        }
    }

    async fn run(script: Arc<Script>, notes_budget_tokens: Option<u32>) -> Vec<SessionEvent> {
        let mut plane = LocalExecutionPlane::new();
        plane.register(RegisteredTool::text(
            "read",
            "Read a file",
            serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            |_args| Box::pin(async { Ok("ERROR x\nERROR y\nERROR z".to_string()) }),
        ));
        let log = Arc::new(InMemorySessionLog::new());
        let runner = RuntimeRunner::new(RuntimeOptions {
            provider: Box::new(ScriptedProvider(script)),
            execution_plane: Some(Box::new(plane)),
            session_log: Some(log.clone()),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: notes_budget_tokens.map(|budget_tokens| crate::runtime::NotesOptions {
                budget_tokens,
                ..Default::default()
            }),
            session_id: Some("notes".into()),
            max_tokens: 8_000,
            max_turns: Some(10),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: Some(vec!["read".into()]),
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        });
        let mut stream = runner.run_streaming("count the errors", &[], None, None).await.unwrap();
        while stream.next().await.transpose().unwrap().is_some() {}
        log.read("notes", 0, None).await.unwrap().into_iter().map(|entry| entry.event).collect()
    }

    #[tokio::test]
    async fn notes_are_written_by_the_model_kept_by_the_kernel_and_shown_in_the_task_state() {
        let script = Arc::new(Script::default());
        let events = run(script.clone(), Some(600)).await;
        assert!(script.offered.lock().unwrap()[0].iter().any(|name| name == "note"));
        let states = script.states.lock().unwrap();
        assert!(states[2].contains("[state] Tally: 3 qualifying lines in logs/a.log"), "{}", states[2]);
        let report = events
            .iter()
            .rev()
            .find_map(|event| match event {
                SessionEvent::ContextLifecycleReported { notes_written, notes_active, .. } => {
                    Some((*notes_written, *notes_active))
                }
                _ => None,
            })
            .expect("a lifecycle report");
        assert_eq!(report, (1, 1));
    }

    #[tokio::test]
    async fn notes_are_off_without_a_budget() {
        let script = Arc::new(Script::default());
        run(script.clone(), None).await;
        assert!(!script.offered.lock().unwrap()[0].iter().any(|name| name == "note"));
    }
}

/// Plan task 9.4 · transcript search and a consolidation run (mirrors node/tests/consolidate.test.ts).
mod consolidate {
    use crate::consolidate::{consolidate_memory, search_transcripts, transcript_lines, transcript_search_tool};
    use crate::providers::{LLMProvider, ProviderRunState, StreamEvent};
    use crate::runtime::{InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner, SessionLog};
    use crate::tools::RegisteredTool;
    use deepstrike_core::context::renderer::InternalRenderedContext;
    use deepstrike_core::types::message::ToolSchema;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    type Script = dyn Fn(u32, &[ToolSchema]) -> Vec<StreamEvent> + Send + Sync;

    struct Scripted {
        calls: Mutex<u32>,
        offered: Mutex<Vec<Vec<String>>>,
        script: Box<Script>,
    }

    #[async_trait::async_trait]
    impl LLMProvider for Arc<Scripted> {
        async fn stream(
            &self,
            _context: &InternalRenderedContext,
            tools: &[ToolSchema],
            _extensions: Option<&serde_json::Value>,
            _state: Option<&ProviderRunState>,
        ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>> {
            let call = {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            self.offered.lock().unwrap().push(tools.iter().map(|t| t.name.to_string()).collect());
            let events: Vec<crate::Result<StreamEvent>> = (self.script)(call, tools).into_iter().map(Ok).collect();
            Ok(Box::new(futures::stream::iter(events)))
        }
    }

    fn options(provider: Arc<Scripted>, log: Arc<InMemorySessionLog>, plane: Option<LocalExecutionPlane>, baseline: Vec<String>) -> RuntimeOptions {
        RuntimeOptions {
            provider: Box::new(provider),
            execution_plane: plane.map(|p| Box::new(p) as Box<dyn crate::runtime::ExecutionPlane>),
            session_log: Some(log),
            compression_store: None,
            payload_store: None,
            kernel_reliability: None,
            notes: None,
            session_id: None,
            max_tokens: 8_000,
            max_turns: Some(8),
            timeout_ms: None,
            extensions: None,
            agent_id: None,
            memory_scope: None,
            system_prompt: None,
            initial_memory: vec![],
            dependency_outputs: Default::default(),
            inherited_knowledge: Vec::new(),
            skill_dir: None,
            memory_store: None,
            knowledge_source: None,
            signal_source: None,
            governance: None,
            os_profile: None,
            governance_policy: None,
            signal_policy: None,
            scheduler_policy: None,
            resource_quota: None,
            memory_policy: None,
            tokenizer: None,
            enable_plan_tool: None,
            on_tool_suspend: None,
            on_permission_request: None,
            milestone_policy: crate::runtime::MilestonePolicy::Terminate,
            milestone_contract: None,
            run_spec: None,
            allowed_tool_ids: None,
            baseline_tool_ids: Some(baseline),
            on_turn_metrics: None,
            stable_core_tool_ids: vec![],
            pre_query_memory: None,
            on_milestone_evaluate: None,
            memory_runtime_policy: None,
            memory_candidate_sink: None,
        }
    }

    async fn earlier_session(log: Arc<InMemorySessionLog>) {
        let provider = Arc::new(Scripted {
            calls: Mutex::new(0),
            offered: Mutex::new(Vec::new()),
            script: Box::new(|call, _| match call {
                1 => vec![StreamEvent::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "logs/svc-03.log" }),
                }],
                _ => vec![StreamEvent::TextDelta { delta: "req ids ending in 0 are retries".into() }],
            }),
        });
        let mut plane = LocalExecutionPlane::new();
        plane.register(RegisteredTool::text(
            "read",
            "Read a file",
            serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            |_args| Box::pin(async { Ok("2026-10-02 ERROR code=E503 req=r120 upstream timeout (retry)".to_string()) }),
        ));
        let mut opts = options(provider, log, Some(plane), vec!["read".into()]);
        opts.session_id = Some("s1".into());
        let runner = RuntimeRunner::new(opts);
        let mut stream = runner.run_streaming("count the E503 errors", &[], None, Some("s1")).await.unwrap();
        while stream.next().await.transpose().unwrap().is_some() {}
    }

    #[tokio::test]
    async fn transcript_search_finds_tool_results_labeled_with_their_call() {
        let log = Arc::new(InMemorySessionLog::new());
        earlier_session(log.clone()).await;
        let entries = log.read("s1", 0, None).await.unwrap();
        let lines = transcript_lines("s1", &entries);
        assert!(lines.iter().any(|l| l.source == "goal" && l.text.contains("E503")));
        let found = search_transcripts(&lines, "E503 retry", 8);
        assert!(found[0].source.contains("c1 read"), "{found:?}");
        assert!(found[0].text.contains("code=E503 req=r120"));
        let tool = transcript_search_tool(log.clone(), vec!["s1".into()], 8);
        assert_eq!(tool.schema.name, "transcript_search");
    }

    #[tokio::test]
    async fn consolidation_is_an_ordinary_run_with_transcript_search() {
        let log = Arc::new(InMemorySessionLog::new());
        earlier_session(log.clone()).await;
        let provider = Arc::new(Scripted {
            calls: Mutex::new(0),
            offered: Mutex::new(Vec::new()),
            script: Box::new(|call, _| match call {
                1 => vec![StreamEvent::ToolCall {
                    id: "t1".into(),
                    name: "transcript_search".into(),
                    arguments: serde_json::json!({ "query": "E503 retry" }),
                }],
                _ => vec![StreamEvent::TextDelta { delta: "checked".into() }],
            }),
        });
        let opts = options(provider.clone(), log.clone(), None, vec![]);
        let (session, text) = consolidate_memory(opts, vec!["s1".into()], None, 8).await.unwrap();
        assert_eq!((session.as_str(), text.as_str()), ("consolidate-1", "checked"));
        assert!(provider.offered.lock().unwrap()[0].iter().any(|n| n == "transcript_search"));
        let events = log.read("consolidate-1", 0, None).await.unwrap();
        let rendered = serde_json::to_string(&events.iter().map(|e| &e.event).collect::<Vec<_>>()).unwrap();
        assert!(rendered.contains("code=E503 req=r120"), "the search result reached the run");
    }
}
