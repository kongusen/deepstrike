#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::WorkingMemory;
    use crate::safety::{PermissionManager, PermissionMode};
    use crate::signals::ScheduledPrompt;
    use crate::tools::{RegisteredTool, ToolChunk, execute_tools, read_file_tool, validate_tool_arguments};
    use deepstrike_core::types::message::ToolCall;
    use compact_str::CompactString;
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
        let call = ToolCall { id: CompactString::new("1"), name: CompactString::new("nope"), arguments: serde_json::json!({}) };
        let results = execute_tools(&[call], &HashMap::new()).await;
        assert!(results[0].is_error);
    }

    #[tokio::test]
    async fn execute_tools_success() {
        let tool = RegisteredTool::text(
            "add", "Add two numbers.",
            serde_json::json!({ "type": "object" }),
            |args| Box::pin(async move {
                let x = args["x"].as_i64().unwrap_or(0);
                let y = args["y"].as_i64().unwrap_or(0);
                Ok(format!("{}", x + y))
            }),
        );
        let mut registry = HashMap::new();
        registry.insert("add".to_string(), tool);
        let call = ToolCall { id: CompactString::new("1"), name: CompactString::new("add"), arguments: serde_json::json!({"x": 2, "y": 3}) };
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
        assert_eq!(ToolChunk::progress(0.5, Some("half".into())).text_projection(), "");
    }

    #[test]
    fn harness_request_builder() {
        let req = crate::harness::HarnessRequest::new("Write a haiku");
        assert_eq!(req.goal, "Write a haiku");
        assert!(req.criteria.is_empty());
    }
}
