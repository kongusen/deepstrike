use crate::context::text::truncate_with_suffix;
use crate::runtime::session::{ProviderReplay, SessionEvent};
use crate::types::message::{Content, ContentPart, Message, Role, ToolCall};

/// Sanitize text for recovery paths: ensure valid UTF-8 and apply an optional
/// byte cap derived from the caller's context config. When `max_bytes` is 0
/// no cap is applied.
pub fn sanitize_recovery_text(text: &str) -> String {
    sanitize_recovery_text_bounded(text, 0)
}

pub fn sanitize_recovery_text_bounded(text: &str, max_bytes: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    if max_bytes > 0 && text.len() > max_bytes {
        return truncate_with_suffix(text, max_bytes, "… [replay truncated]");
    }
    text.to_owned()
}

fn estimate_token_count(text: &str) -> u32 {
    // Char count / 4 approximation — more accurate than byte count for CJK.
    (text.chars().count() as u32 / 4).max(1)
}

/// Minimal Anthropic-style blocks from plain `content` + `tool_calls` when
/// `provider_replay` was not persisted (legacy logs or crash before append).
pub fn synthesize_provider_replay(
    content: &str,
    tool_calls: &[ToolCall],
) -> Option<ProviderReplay> {
    if tool_calls.is_empty() {
        return None;
    }
    let mut blocks = Vec::new();
    if !content.is_empty() {
        blocks.push(serde_json::json!({ "type": "text", "text": content }));
    }
    for tc in tool_calls {
        blocks.push(serde_json::json!({
            "type": "tool_use",
            "id": tc.id,
            "name": tc.name,
            "input": tc.arguments,
        }));
    }
    Some(ProviderReplay {
        native_blocks: Some(blocks),
        reasoning_content: None,
    })
}

/// Prefer persisted replay; fall back to synthesis for tool turns.
pub fn effective_provider_replay(
    content: &str,
    tool_calls: &[ToolCall],
    stored: Option<&ProviderReplay>,
) -> Option<ProviderReplay> {
    if let Some(stored) = stored {
        if stored.native_blocks.as_ref().is_some_and(|b| !b.is_empty())
            || stored.reasoning_content.is_some()
        {
            return Some(stored.clone());
        }
    }
    synthesize_provider_replay(content, tool_calls)
}

fn normalize_assistant_message(message: &mut Message) {
    normalize_assistant_message_with_cap(message, 0);
}

fn normalize_assistant_message_with_cap(message: &mut Message, max_bytes: usize) {
    if message.token_count.is_none() {
        message.token_count = Some(estimate_token_count(
            message.content.as_text().unwrap_or(""),
        ));
    }
    if let Content::Text(text) = &mut message.content {
        *text = sanitize_recovery_text_bounded(text, max_bytes);
    }
}

/// Normalize a single `LlmCompleted` for recovery (message fields + provider_replay).
pub fn repair_llm_completed(message: &mut Message, provider_replay: &mut Option<ProviderReplay>) {
    repair_llm_completed_with_cap(message, provider_replay, 0);
}

pub fn repair_llm_completed_with_cap(
    message: &mut Message,
    provider_replay: &mut Option<ProviderReplay>,
    max_bytes: usize,
) {
    normalize_assistant_message_with_cap(message, max_bytes);
    let content = message.content.as_text().unwrap_or("").to_owned();
    *provider_replay =
        effective_provider_replay(&content, &message.tool_calls, provider_replay.as_ref());
}

/// Repair event log entries in place for recovery minimum set completeness.
pub fn repair_events(events: Vec<SessionEvent>) -> Vec<SessionEvent> {
    repair_events_with_cap(events, 0)
}

pub fn repair_events_with_cap(events: Vec<SessionEvent>, max_bytes: usize) -> Vec<SessionEvent> {
    events
        .into_iter()
        .map(|mut event| {
            if let SessionEvent::LlmCompleted {
                ref mut message,
                ref mut provider_replay,
                ..
            } = event
            {
                repair_llm_completed_with_cap(message, provider_replay, max_bytes);
            }
            event
        })
        .collect()
}

/// Pending tool calls after the last assistant turn in preloaded history.
pub fn pending_tool_calls_from_messages(messages: &[Message]) -> Vec<ToolCall> {
    let Some(assistant_idx) = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant && !m.tool_calls.is_empty())
    else {
        return Vec::new();
    };

    let assistant = &messages[assistant_idx];
    let mut completed = std::collections::HashSet::new();
    for msg in &messages[assistant_idx + 1..] {
        if msg.role != Role::Tool {
            continue;
        }
        if let Content::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolResult { call_id, .. } = part {
                    completed.insert(call_id.clone());
                }
            }
        }
    }

    assistant
        .tool_calls
        .iter()
        .filter(|tc| !completed.contains(&tc.id))
        .cloned()
        .collect()
}

/// Reconstructs full messages from a sequence of events.
/// For `SessionEvent::Compressed` events:
/// 1. If `archive_ref` is present, it attempts to load the messages using `load_archive`.
/// 2. If loading succeeds, the reconstructed messages are appended to the history.
/// 3. If loading fails (returns a `ContextFault::MissingArchive` or another error),
///    or if `archive_ref` is `None`, it falls back to the embedded `summary` in the `Compressed` event (if present)
///    as a system message `[Compressed context: turn {turn}]\n{summary}`.
pub fn reconstruct_messages_with_fallback<F>(
    events: &[SessionEvent],
    session_id: &str,
    max_bytes: usize,
    mut load_archive: F,
) -> Vec<Message>
where
    F: FnMut(&str) -> Result<Vec<Message>, crate::context::snapshot::ContextFault>,
{
    let mut messages = Vec::new();
    for event in events {
        match event {
            SessionEvent::RunStarted { goal, criteria, .. } => {
                let user_text = if criteria.is_empty() {
                    goal.clone()
                } else {
                    format!(
                        "{}\n\nCriteria:\n{}",
                        goal,
                        criteria
                            .iter()
                            .enumerate()
                            .map(|(i, c)| format!("{}. {}", i + 1, c))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                };
                messages.push(Message {
                    role: Role::User,
                    content: Content::Text(user_text),
                    tool_calls: vec![],
                    token_count: None,
                });
            }
            SessionEvent::LlmCompleted { message, .. } => {
                let mut msg = message.clone();
                if let Content::Text(text) = &mut msg.content {
                    *text = sanitize_recovery_text_bounded(text, max_bytes);
                }
                messages.push(msg);
            }
            SessionEvent::ToolCompleted { results, .. } => {
                for r in results {
                    let output = match &r.output {
                        Content::Text(t) => sanitize_recovery_text_bounded(t, max_bytes),
                        Content::Parts(_) => String::new(),
                    };
                    messages.push(Message {
                        role: Role::Tool,
                        content: Content::Parts(vec![ContentPart::ToolResult {
                            call_id: r.call_id.clone(),
                            output,
                            is_error: r.is_error,
                        }]),
                        tool_calls: vec![],
                        token_count: r.token_count,
                    });
                }
            }
            SessionEvent::Compressed {
                turn,
                summary,
                archive_ref,
                ..
            } => {
                let mut loaded_successfully = false;
                if let Some(ref_str) = archive_ref {
                    if !ref_str.is_empty() {
                        match load_archive(ref_str) {
                            Ok(archived_msgs) => {
                                for mut msg in archived_msgs {
                                    if let Content::Text(text) = &mut msg.content {
                                        *text = sanitize_recovery_text_bounded(text, max_bytes);
                                    }
                                    messages.push(msg);
                                }
                                loaded_successfully = true;
                            }
                            Err(_) => {
                                // Loader failed (e.g. MissingArchive). We degrade and fallback.
                            }
                        }
                    }
                }

                if !loaded_successfully {
                    if let Some(sum) = summary {
                        let system_text = format!("[Compressed context: turn {}]\n{}", turn, sum);
                        messages.push(Message {
                            role: Role::System,
                            content: Content::Text(system_text),
                            tool_calls: vec![],
                            token_count: None,
                        });
                    }
                }
            }
            SessionEvent::Rollbacked { checkpoint_history_len, .. } => {
                messages.truncate(*checkpoint_history_len as usize);
            }
            _ => {}
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;

    #[test]
    fn synthesize_provider_replay_builds_tool_use_blocks() {
        let replay = synthesize_provider_replay(
            "checking",
            &[ToolCall {
                id: CompactString::new("c1"),
                name: CompactString::new("ping"),
                arguments: serde_json::json!({}),
            }],
        )
        .expect("replay");
        let blocks = replay.native_blocks.expect("blocks");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
    }

    #[test]
    fn effective_provider_replay_prefers_stored() {
        let stored = ProviderReplay {
            native_blocks: None,
            reasoning_content: Some("trace".into()),
        };
        let out = effective_provider_replay("x", &[], Some(&stored)).expect("replay");
        assert_eq!(out.reasoning_content.as_deref(), Some("trace"));
    }

    #[test]
    fn sanitize_recovery_text_bounded_respects_cjk_boundary() {
        let text = "你".repeat(20_000);
        // Pass an explicit byte cap: 300 bytes
        let out = sanitize_recovery_text_bounded(&text, 300);
        assert!(out.ends_with("… [replay truncated]"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
}
