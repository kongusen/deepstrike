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
            provider_replay,
            ..
        } = &entry.event
        {
            if let Some(replay) = provider_replay {
                let content = message.content.as_text().unwrap_or("").to_string();
                provider.seed_provider_replay(&content, &message.tool_calls, replay);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{LLMProvider, ProviderRunState, StreamEvent};
    use async_trait::async_trait;
    use deepstrike_core::context::renderer::RenderedContext;
    use deepstrike_core::runtime::session::SessionEvent;
    use deepstrike_core::types::message::{Content, Message, Role, ToolSchema};
    use futures::{Stream, stream};
    use std::sync::Mutex;

    #[derive(Default)]
    struct CapturingProvider {
        seeded: Mutex<Vec<(String, ProviderReplay)>>,
    }

    #[async_trait]
    impl LLMProvider for CapturingProvider {
        fn seed_provider_replay(
            &self,
            content: &str,
            _tool_calls: &[ToolCall],
            replay: &ProviderReplay,
        ) {
            self.seeded
                .lock()
                .unwrap()
                .push((content.to_owned(), replay.clone()));
        }

        async fn stream(
            &self,
            _context: &RenderedContext,
            _tools: &[ToolSchema],
            _extensions: Option<&serde_json::Value>,
            _state: Option<&ProviderRunState>,
        ) -> crate::Result<Box<dyn Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>>
        {
            Ok(Box::new(Box::pin(stream::empty())))
        }
    }

    #[test]
    fn seeds_only_persisted_provider_replay_from_events() {
        let provider = CapturingProvider::default();
        let replay = ProviderReplay {
            native_blocks: None,
            reasoning_content: Some("trace".into()),
            extra: serde_json::Map::new(),
        };
        let entries = vec![
            SessionEntry {
                seq: 0,
                event: SessionEvent::LlmCompleted {
                    turn: 0,
                    message: Message {
                        role: Role::Assistant,
                        content: Content::Text("without replay".into()),
                        tool_calls: vec![],
                        token_count: None,
                    },
                    provider_replay: None,
                },
            },
            SessionEntry {
                seq: 1,
                event: SessionEvent::LlmCompleted {
                    turn: 1,
                    message: Message {
                        role: Role::Assistant,
                        content: Content::Text("with replay".into()),
                        tool_calls: vec![],
                        token_count: None,
                    },
                    provider_replay: Some(replay.clone()),
                },
            },
        ];

        seed_provider_replay_from_events(&provider, &entries);

        let seeded = provider.seeded.lock().unwrap();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].0, "with replay");
        assert_eq!(seeded[0].1, replay);
    }
}
