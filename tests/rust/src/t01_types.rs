use deepstrike_core::types::message::*;
use deepstrike_core::types::task::RuntimeTask;
use deepstrike_core::types::skill::SkillMetadata;
use deepstrike_core::types::result::TerminationReason;
use deepstrike_core::AgentIdentity;
use compact_str::CompactString;

// ─── Message constructors ───────────────────────────────────────────────────

#[test]
fn system_message_has_correct_role() {
    let msg = Message::system("You are helpful.");
    assert_eq!(msg.role, Role::System);
    assert_eq!(msg.content.as_text().unwrap(), "You are helpful.");
    assert!(msg.tool_calls.is_empty());
}

#[test]
fn user_message_has_correct_role() {
    let msg = Message::user("Hello");
    assert_eq!(msg.role, Role::User);
    assert_eq!(msg.content.as_text().unwrap(), "Hello");
}

#[test]
fn assistant_message_has_correct_role() {
    let msg = Message::assistant("World");
    assert_eq!(msg.role, Role::Assistant);
    assert_eq!(msg.content.as_text().unwrap(), "World");
}

#[test]
fn tool_message_with_result_parts() {
    let parts = vec![ContentPart::ToolResult {
        call_id: CompactString::new("c1"),
        output: "42".to_string(),
        is_error: false,
    }];
    let msg = Message::tool(parts);
    assert_eq!(msg.role, Role::Tool);
    assert!(matches!(msg.content, Content::Parts(_)));
}

// ─── Content ────────────────────────────────────────────────────────────────

#[test]
fn content_text_as_text() {
    let c = Content::Text("abc".into());
    assert_eq!(c.as_text(), Some("abc"));
    assert_eq!(c.text_len(), 3);
}

#[test]
fn content_parts_as_text_returns_none() {
    let c = Content::Parts(vec![ContentPart::text("hello")]);
    assert!(c.as_text().is_none());
}

#[test]
fn content_parts_text_len_sums_parts() {
    let c = Content::Parts(vec![
        ContentPart::text("hello"),
        ContentPart::text("world"),
    ]);
    assert_eq!(c.text_len(), 10);
}

// ─── ContentPart constructors ───────────────────────────────────────────────

#[test]
fn content_part_text_constructor() {
    let p = ContentPart::text("foo");
    assert!(matches!(p, ContentPart::Text { text } if text == "foo"));
}

#[test]
fn content_part_image_url_constructor() {
    let p = ContentPart::image_url("https://img.png");
    match p {
        ContentPart::Image { url, data, .. } => {
            assert_eq!(url.as_deref(), Some("https://img.png"));
            assert!(data.is_none());
        }
        _ => panic!("expected Image"),
    }
}

#[test]
fn content_part_image_base64_constructor() {
    let p = ContentPart::image_base64("abc123", "image/png");
    match p {
        ContentPart::Image { data, media_type, .. } => {
            assert_eq!(data.as_deref(), Some("abc123"));
            assert_eq!(media_type.as_deref(), Some("image/png"));
        }
        _ => panic!("expected Image"),
    }
}

#[test]
fn content_part_audio_constructor() {
    let p = ContentPart::audio("base64data", "audio/wav");
    assert!(matches!(p, ContentPart::Audio { .. }));
}

// ─── Multimodal message ─────────────────────────────────────────────────────

#[test]
fn user_multimodal_message() {
    let msg = Message::user_multimodal(vec![
        ContentPart::text("Describe this image"),
        ContentPart::image_url("https://example.com/image.png"),
    ]);
    assert_eq!(msg.role, Role::User);
    match msg.content {
        Content::Parts(parts) => assert_eq!(parts.len(), 2),
        _ => panic!("expected Parts"),
    }
}

// ─── ToolCall / ToolResult ──────────────────────────────────────────────────

#[test]
fn tool_call_fields() {
    let tc = ToolCall {
        id: CompactString::new("call-1"),
        name: CompactString::new("add"),
        arguments: serde_json::json!({"x": 1, "y": 2}),
    };
    assert_eq!(tc.id.as_str(), "call-1");
    assert_eq!(tc.name.as_str(), "add");
    assert_eq!(tc.arguments["x"], 1);
}

#[test]
fn tool_result_fields() {
    let tr = ToolResult {
        call_id: CompactString::new("call-1"),
        output: Content::Text("3".into()),
        is_error: false,
        token_count: Some(5),
    };
    assert_eq!(tr.output.as_text(), Some("3"));
    assert!(!tr.is_error);
    assert_eq!(tr.token_count, Some(5));
}

#[test]
fn tool_schema_fields() {
    let ts = ToolSchema {
        name: CompactString::new("read_file"),
        description: "Read a file.".into(),
        parameters: serde_json::json!({"type": "object"}),
    };
    assert_eq!(ts.name.as_str(), "read_file");
}

// ─── Message serialization roundtrip ────────────────────────────────────────

#[test]
fn message_json_roundtrip() {
    let msg = Message::user("Test");
    let json = serde_json::to_string(&msg).unwrap();
    let decoded: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.role, Role::User);
    assert_eq!(decoded.content.as_text().unwrap(), "Test");
}

#[test]
fn role_serialization() {
    assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
    assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
    assert_eq!(serde_json::to_string(&Role::Assistant).unwrap(), "\"assistant\"");
    assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
}

// ─── RuntimeTask ────────────────────────────────────────────────────────────

#[test]
fn runtime_task_new() {
    let t = RuntimeTask::new("Write a haiku");
    assert_eq!(t.goal, "Write a haiku");
    assert!(t.criteria.is_empty());
}

#[test]
fn runtime_task_with_criteria() {
    let t = RuntimeTask::new("Write a haiku")
        .with_criteria(vec!["Must be 5-7-5".into(), "About nature".into()]);
    assert_eq!(t.criteria.len(), 2);
    assert_eq!(t.criteria[0], "Must be 5-7-5");
}

#[test]
fn runtime_task_json_roundtrip() {
    let t = RuntimeTask::new("test").with_criteria(vec!["c1".into()]);
    let json = serde_json::to_string(&t).unwrap();
    let decoded: RuntimeTask = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.goal, "test");
    assert_eq!(decoded.criteria.len(), 1);
}

// ─── SkillMetadata ──────────────────────────────────────────────────────────

#[test]
fn skill_metadata_new() {
    let s = SkillMetadata::new("debug", "Debug helper");
    assert_eq!(s.name.as_str(), "debug");
    assert_eq!(s.description, "Debug helper");
    assert!(s.when_to_use.is_none());
    assert!(s.effort.is_none());
}

#[test]
fn skill_metadata_builder_chain() {
    let s = SkillMetadata::new("api_call", "Call APIs")
        .with_when_to_use("When making HTTP requests")
        .with_effort(3)
        .with_estimated_tokens(500);
    assert_eq!(s.when_to_use.as_deref(), Some("When making HTTP requests"));
    assert_eq!(s.effort, Some(3));
    assert_eq!(s.estimated_tokens, 500);
}

// ─── TerminationReason ──────────────────────────────────────────────────────

#[test]
fn termination_reason_serialization() {
    assert_eq!(
        serde_json::to_string(&TerminationReason::Completed).unwrap(),
        "\"completed\""
    );
    assert_eq!(
        serde_json::to_string(&TerminationReason::MaxTurns).unwrap(),
        "\"max_turns\""
    );
}

// ─── AgentIdentity ──────────────────────────────────────────────────────────

#[test]
fn agent_identity_new() {
    let id = AgentIdentity::new("agent-1", "session-1");
    assert_eq!(id.agent_id.as_str(), "agent-1");
    assert_eq!(id.session_id.as_str(), "session-1");
    assert!(!id.is_sub_agent);
}

#[test]
fn agent_identity_sub_agent() {
    let id = AgentIdentity::sub_agent("child-1", "s1");
    assert!(id.is_sub_agent);
}

// ─── Image detail token estimates ───────────────────────────────────────────

#[test]
fn image_low_detail_token_estimate() {
    let c = Content::Parts(vec![ContentPart::Image {
        url: Some("https://example.com/img.png".into()),
        data: None,
        media_type: None,
        detail: Some("low".into()),
    }]);
    assert_eq!(c.text_len(), 340);
}

#[test]
fn image_high_detail_token_estimate() {
    let c = Content::Parts(vec![ContentPart::Image {
        url: Some("https://example.com/img.png".into()),
        data: None,
        media_type: None,
        detail: Some("high".into()),
    }]);
    assert_eq!(c.text_len(), 2720);
}
