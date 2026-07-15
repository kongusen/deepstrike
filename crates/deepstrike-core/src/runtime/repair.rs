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

/// Normalize a single `LlmCompleted` for recovery (message fields only).
///
/// Provider-neutral: the stored `provider_replay` envelope is left untouched.
/// The core never synthesizes a protocol-specific replay shape — legacy
/// reconstruction is the responsibility of the target provider in the SDK.
pub fn repair_llm_completed(message: &mut Message, provider_replay: &mut Option<ProviderReplay>) {
    repair_llm_completed_with_cap(message, provider_replay, 0);
}

pub fn repair_llm_completed_with_cap(
    message: &mut Message,
    _provider_replay: &mut Option<ProviderReplay>,
    max_bytes: usize,
) {
    normalize_assistant_message_with_cap(message, max_bytes);
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
    _session_id: &str,
    max_bytes: usize,
    mut load_archive: F,
) -> Vec<Message>
where
    F: FnMut(&str) -> Result<Vec<Message>, crate::context::fault::ContextFault>,
{
    let mut messages = Vec::new();
    for (event_index, event) in events.iter().enumerate() {
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

                let page_out_will_supply_archive = archive_ref.is_none()
                    && events[event_index + 1..].iter().any(|event| matches!(
                        event,
                        SessionEvent::PageOut {
                            turn: page_out_turn,
                            archive_ref: Some(reference),
                            ..
                        } if page_out_turn == turn && !reference.is_empty()
                    ));
                if !loaded_successfully && !page_out_will_supply_archive {
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
            SessionEvent::PageOut {
                turn,
                summary,
                archive_ref: Some(archive_ref),
                ..
            } if !archive_ref.is_empty() => {
                let already_loaded_by_compression = events[..event_index].iter().rev().any(|event| {
                    matches!(
                        event,
                        SessionEvent::Compressed {
                            turn: compressed_turn,
                            archive_ref: Some(reference),
                            ..
                        } if compressed_turn == turn && !reference.is_empty()
                    )
                });
                if already_loaded_by_compression {
                    continue;
                }
                match load_archive(archive_ref) {
                    Ok(archived_messages) => {
                        for mut message in archived_messages {
                            if let Content::Text(text) = &mut message.content {
                                *text = sanitize_recovery_text_bounded(text, max_bytes);
                            }
                            messages.push(message);
                        }
                    }
                    Err(_) => {
                        if let Some(summary) = summary {
                            messages.push(Message {
                                role: Role::System,
                                content: Content::Text(format!(
                                    "[Compressed context: turn {}]\n{}",
                                    turn, summary
                                )),
                                tool_calls: vec![],
                                token_count: None,
                            });
                        }
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
    fn repair_does_not_synthesize_provider_replay_for_tool_turns() {
        let mut message = Message {
            role: Role::Assistant,
            content: Content::Text("checking".into()),
            tool_calls: vec![ToolCall {
                id: CompactString::new("c1"),
                name: CompactString::new("ping"),
                arguments: serde_json::json!({}),
            }],
            token_count: None,
        };
        let mut replay: Option<ProviderReplay> = None;
        repair_llm_completed(&mut message, &mut replay);
        // Provider-neutral: no fabricated native_blocks.
        assert!(replay.is_none());
        // Message is still normalized (token count backfilled).
        assert!(message.token_count.is_some());
    }

    #[test]
    fn repair_passes_stored_replay_through() {
        let mut message = Message {
            role: Role::Assistant,
            content: Content::Text("x".into()),
            tool_calls: vec![],
            token_count: Some(1),
        };
        let mut replay = Some(ProviderReplay {
            native_blocks: None,
            reasoning_content: Some("trace".into()),
            extra: serde_json::Map::new(),
        });
        repair_llm_completed(&mut message, &mut replay);
        assert_eq!(
            replay.as_ref().and_then(|r| r.reasoning_content.as_deref()),
            Some("trace")
        );
    }

    #[test]
    fn provider_replay_round_trips_unknown_envelope_fields() {
        let json = serde_json::json!({
            "schema_version": 2,
            "provider": "deepseek",
            "protocol": "openai-chat",
            "model": "deepseek-v4-flash",
            "reasoning_content": "trace",
            "reasoning_details": [{"type": "reasoning.text", "text": "trace"}],
            "tool_calls": [{"id": "c1"}]
        });
        let replay: ProviderReplay = serde_json::from_value(json.clone()).expect("parse");
        assert_eq!(replay.reasoning_content.as_deref(), Some("trace"));
        assert_eq!(replay.extra["provider"], "deepseek");
        assert_eq!(replay.extra["protocol"], "openai-chat");
        // Re-serialize: the envelope is preserved verbatim.
        assert_eq!(serde_json::to_value(&replay).expect("serialize"), json);
    }

    #[test]
    fn reconstruct_ignores_categorized_kernel_os_events() {
        use crate::runtime::event_log::KernelEventCategory;
        use crate::runtime::session::SessionEvent;

        let events = vec![
            SessionEvent::RunStarted {
                run_id: "r1".into(),
                goal: "g".into(),
                criteria: vec![],
                agent_id: None,
                system_prompt: None,
            },
            SessionEvent::PageOut {
                turn: 1,
                action: Some("auto_compact".into()),
                summary: Some("sum".into()),
                tier_hint: Some("durable".into()),
                message_count: 3,
                archive_ref: None,
            },
            SessionEvent::SignalDisposed {
                turn: 1,
                signal_id: "sig-1".into(),
                disposition: "queue".into(),
                queue_depth: 1,
            },
        ];
        let messages = reconstruct_messages_with_fallback(&events, "s1", 0, |_| {
            Err(crate::context::fault::ContextFault::MissingArchive {
                session_id: "s1".into(),
                seq: 0,
            })
        });
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
    }

    #[test]
    fn reconstruct_loads_archive_from_committed_page_out_event() {
        use crate::runtime::session::SessionEvent;

        let events = vec![
            SessionEvent::Compressed {
                turn: 2,
                archived_seq_range: (0, 4),
                action: Some("auto_compact".into()),
                summary: Some("fallback".into()),
                summary_tokens: Some(1),
                archive_ref: None,
                preserved_refs: vec![],
            },
            SessionEvent::PageOut {
                turn: 2,
                action: Some("auto_compact".into()),
                summary: Some("fallback".into()),
                tier_hint: Some("semantic".into()),
                message_count: 1,
                archive_ref: Some("archive://turn-2".into()),
            },
        ];

        let messages = reconstruct_messages_with_fallback(&events, "s1", 1024, |reference| {
            assert_eq!(reference, "archive://turn-2");
            Ok(vec![Message::user("restored archive")])
        });

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_text(), Some("restored archive"));
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
