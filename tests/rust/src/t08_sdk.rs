use compact_str::CompactString;
use deepstrike_core::types::message::ToolCall;
use deepstrike_sdk::*;
use std::collections::HashMap;

// ─── RegisteredTool ─────────────────────────────────────────────────────────

#[test]
fn registered_tool_schema() {
    let tool = RegisteredTool::text(
        "add",
        "Add two numbers",
        serde_json::json!({
            "type": "object",
            "properties": {
                "x": { "type": "integer" },
                "y": { "type": "integer" }
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
    );
    assert_eq!(tool.schema.name.as_str(), "add");
    assert_eq!(tool.schema.description, "Add two numbers");
}

#[test]
fn read_file_tool_has_correct_schema() {
    let tool = read_file_tool();
    assert_eq!(tool.schema.name.as_str(), "read_file");
    assert!(tool.schema.description.contains("Read"));
    assert!(tool.schema.parameters["properties"]["path"].is_object());
}

// ─── execute_tools ──────────────────────────────────────────────────────────

#[tokio::test]
async fn execute_tools_success() {
    let tool = RegisteredTool::text(
        "multiply",
        "Multiply two numbers.",
        serde_json::json!({"type": "object"}),
        |args| {
            Box::pin(async move {
                let x = args["x"].as_i64().unwrap_or(0);
                let y = args["y"].as_i64().unwrap_or(0);
                Ok(format!("{}", x * y))
            })
        },
    );
    let mut registry = HashMap::new();
    registry.insert("multiply".to_string(), tool);

    let call = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("multiply"),
        arguments: serde_json::json!({"x": 3, "y": 7}),
    };
    let results = execute_tools(&[call], &registry).await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].is_error);
    assert_eq!(results[0].output.as_text(), Some("21"));
}

#[tokio::test]
async fn execute_tools_unknown_tool() {
    let call = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("nonexistent"),
        arguments: serde_json::json!({}),
    };
    let results = execute_tools(&[call], &HashMap::new()).await;
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
}

#[tokio::test]
async fn execute_tools_error_propagation() {
    let tool = RegisteredTool::text("fail", "Always fails.", serde_json::json!({}), |_| {
        Box::pin(async move { Err(deepstrike_sdk::Error::Tool("intentional error".into())) })
    });
    let mut registry = HashMap::new();
    registry.insert("fail".to_string(), tool);

    let call = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("fail"),
        arguments: serde_json::json!({}),
    };
    let results = execute_tools(&[call], &registry).await;
    assert!(results[0].is_error);
    let text = results[0].output.as_text().unwrap();
    assert!(text.contains("intentional error"));
}

#[tokio::test]
async fn execute_multiple_tools_parallel() {
    let add = RegisteredTool::text("add", "Add.", serde_json::json!({}), |args| {
        Box::pin(async move {
            let x = args["x"].as_i64().unwrap_or(0);
            let y = args["y"].as_i64().unwrap_or(0);
            Ok(format!("{}", x + y))
        })
    });
    let sub = RegisteredTool::text("sub", "Subtract.", serde_json::json!({}), |args| {
        Box::pin(async move {
            let x = args["x"].as_i64().unwrap_or(0);
            let y = args["y"].as_i64().unwrap_or(0);
            Ok(format!("{}", x - y))
        })
    });

    let mut registry = HashMap::new();
    registry.insert("add".to_string(), add);
    registry.insert("sub".to_string(), sub);

    let calls = vec![
        ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("add"),
            arguments: serde_json::json!({"x": 10, "y": 5}),
        },
        ToolCall {
            id: CompactString::new("c2"),
            name: CompactString::new("sub"),
            arguments: serde_json::json!({"x": 10, "y": 5}),
        },
    ];
    let results = execute_tools(&calls, &registry).await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].output.as_text(), Some("15"));
    assert_eq!(results[1].output.as_text(), Some("5"));
}

// ─── RuntimeOptions (field presence via construction) ─────────────────────────

#[test]
fn runtime_options_can_be_constructed() {
    use deepstrike_sdk::{
        InMemorySessionLog, LocalExecutionPlane, OpenAIProvider, RuntimeOptions, RuntimeRunner,
    };
    use std::sync::Arc;

    let runner = RuntimeRunner::new(RuntimeOptions {
        provider: Box::new(OpenAIProvider::new("sk-test")),
        execution_plane: Some(Box::new(LocalExecutionPlane::new())),
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
    });
    assert_eq!(runner.execution_plane().schemas().len(), 0);
}

// ─── PermissionManager (SDK) ────────────────────────────────────────────────

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
fn permission_manager_revoke() {
    let mut pm = PermissionManager::new(PermissionMode::Default);
    pm.grant("fs", "read");
    assert!(pm.evaluate("fs", "read").allowed);
    pm.revoke("fs", "read");
    assert!(!pm.evaluate("fs", "read").allowed);
}

#[test]
fn permission_manager_grant_with_approval() {
    let mut pm = PermissionManager::new(PermissionMode::Default);
    pm.grant_with_approval("db", "write", "Requires DBA approval");
    let decision = pm.evaluate("db", "write");
    assert!(!decision.allowed);
    assert!(decision.requires_approval);
}

// ─── RunEvent ───────────────────────────────────────────────────────────────

#[test]
fn run_event_variants() {
    let _td = RunEvent::TextDelta("hello".into());
    let _thd = RunEvent::ThinkingDelta("thought".into());
    let _tc = RunEvent::ToolCall {
        id: "c1".into(),
        name: "add".into(),
    };
    let _tr = RunEvent::ToolResult {
        call_id: "c1".into(),
        content: "3".into(),
        is_error: false,
    };
    let _done = RunEvent::Done {
        iterations: 1,
        total_tokens: 100,
        status: "completed".into(),
    };
    let _err = RunEvent::Error("boom".into());
}

// ─── OpenAIProvider construction ────────────────────────────────────────────

#[test]
fn openai_provider_new() {
    let _provider = OpenAIProvider::new("test-key");
}

#[test]
fn openai_provider_with_base_url() {
    let _provider = OpenAIProvider::with_base_url("key", "gpt-5-mini", "https://xiaoai.plus/v1");
}

// ─── Provider factory functions ─────────────────────────────────────────────

#[test]
fn provider_factories() {
    let _q = deepstrike_sdk::providers::openai::qwen("key");
    let _d = deepstrike_sdk::providers::openai::deepseek("key");
    let _m = deepstrike_sdk::providers::openai::minimax("key");
    let _o = deepstrike_sdk::providers::openai::ollama("llama3");
    let _k = deepstrike_sdk::providers::openai::kimi("key");
}

// ─── KnowledgeSource trait ──────────────────────────────────────────────────

#[test]
fn knowledge_source_is_object_safe() {
    fn _takes_ks(_: &dyn KnowledgeSource) {}
}

// ─── DreamResult defaults ───────────────────────────────────────────────────

#[test]
fn dream_result_default() {
    let dr = deepstrike_sdk::DreamResult::default();
    assert_eq!(dr.sessions_processed, 0);
    assert_eq!(dr.insights_extracted, 0);
    assert_eq!(dr.entries_added, 0);
    assert_eq!(dr.entries_removed, 0);
}

// ─── Error type ─────────────────────────────────────────────────────────────

#[test]
fn error_display() {
    let e = deepstrike_sdk::Error::Provider("timeout".into());
    assert_eq!(format!("{e}"), "provider error: timeout");

    let e2 = deepstrike_sdk::Error::Tool("bad input".into());
    assert_eq!(format!("{e2}"), "tool error: bad input");

    let e3 = deepstrike_sdk::Error::Other("misc".into());
    assert_eq!(format!("{e3}"), "misc");
}

// ─── TokenUsage ─────────────────────────────────────────────────────────────

#[test]
fn token_usage_total() {
    let usage = deepstrike_sdk::providers::TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
    };
    assert_eq!(usage.total_tokens(), 150);
}

#[test]
fn token_usage_default() {
    let usage = deepstrike_sdk::providers::TokenUsage::default();
    assert_eq!(usage.total_tokens(), 0);
}
