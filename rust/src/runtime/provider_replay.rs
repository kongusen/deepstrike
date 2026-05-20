use deepstrike_core::runtime::session::{ProviderReplay, SessionEvent};
use deepstrike_core::types::message::ToolCall;

use super::session_log::SessionEntry;

pub fn seed_provider_replay_from_events(
    provider: &dyn crate::providers::LLMProvider,
    events: &[SessionEntry],
) {
    for entry in events {
        if let SessionEvent::LlmCompleted {
            message,
            provider_replay: Some(replay),
            ..
        } = &entry.event
        {
            let content = message.content.as_text().unwrap_or("").to_string();
            provider.seed_provider_replay(&content, &message.tool_calls, replay);
        }
    }
}

pub fn peek_provider_replay(
    provider: &dyn crate::providers::LLMProvider,
    content: &str,
    tool_calls: &[ToolCall],
) -> Option<ProviderReplay> {
    provider.peek_provider_replay(content, tool_calls)
}

pub fn assistant_replay_key(content: &str, tool_calls: &[ToolCall]) -> String {
    serde_json::json!({
        "content": content,
        "toolCalls": tool_calls.iter().map(|tc| {
            serde_json::json!({
                "id": tc.id.as_str(),
                "name": tc.name.as_str(),
                "arguments": tc.arguments.to_string(),
            })
        }).collect::<Vec<_>>(),
    })
    .to_string()
}
