use deepstrike_core::runtime::repair::{
    repair_events, repair_events_with_cap, repair_llm_completed, sanitize_recovery_text,
    sanitize_recovery_text_bounded,
};
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::types::message::{Content, ContentPart, Message, Role};

use super::session_log::SessionEntry;

pub fn repair_entries(entries: &[SessionEntry]) -> Vec<SessionEntry> {
    repair_entries_with_cap(entries, 0)
}

pub fn repair_entries_with_cap(entries: &[SessionEntry], max_bytes: usize) -> Vec<SessionEntry> {
    let events: Vec<SessionEvent> = entries.iter().map(|e| e.event.clone()).collect();
    repair_events_with_cap(events, max_bytes)
        .into_iter()
        .zip(entries.iter())
        .map(|(event, entry)| SessionEntry {
            seq: entry.seq,
            event,
        })
        .collect()
}

pub fn is_mid_run(entries: &[SessionEntry]) -> bool {
    !entries.is_empty()
        && !entries
            .iter()
            .any(|e| matches!(e.event, SessionEvent::RunTerminal { .. }))
}

pub fn replay_messages(entries: &[SessionEntry]) -> Vec<Message> {
    replay_messages_with_cap(entries, 0)
}

pub fn replay_messages_with_cap(entries: &[SessionEntry], max_bytes: usize) -> Vec<Message> {
    let events: Vec<SessionEvent> = entries.iter().map(|e| e.event.clone()).collect();
    let repaired = repair_events_with_cap(events, max_bytes);
    let mut messages = Vec::new();
    for event in repaired {
        match event {
            SessionEvent::RunStarted { goal, criteria, .. } => {
                let user_text = if criteria.is_empty() {
                    goal.clone()
                } else {
                    format!(
                        "{goal}\n\nCriteria:\n{}",
                        criteria
                            .iter()
                            .enumerate()
                            .map(|(i, c)| format!("{}. {c}", i + 1))
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
            SessionEvent::LlmCompleted { mut message, .. } => {
                if let Content::Text(text) = &mut message.content {
                    *text = sanitize_recovery_text_bounded(text, max_bytes);
                }
                messages.push(message);
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
            SessionEvent::Compressed { turn, summary, .. } => {
                if let Some(sum) = summary {
                    let system_text = format!("[Compressed context: turn {turn}]\n{sum}");
                    messages.push(Message {
                        role: Role::System,
                        content: Content::Text(system_text),
                        tool_calls: vec![],
                        token_count: None,
                    });
                }
            }
            _ => {}
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepstrike_core::runtime::session::SessionEvent;
    use deepstrike_core::types::message::{Content, ToolCall, ToolResult};

    #[test]
    fn is_mid_run_when_no_terminal() {
        let entries = vec![SessionEntry {
            seq: 0,
            event: SessionEvent::RunStarted {
                run_id: "r1".into(),
                goal: "hi".into(),
                criteria: vec![],
                agent_id: None,
                system_prompt: None,
            },
        }];
        assert!(is_mid_run(&entries));
    }

    #[test]
    fn replay_includes_user_and_assistant() {
        let entries = vec![
            SessionEntry {
                seq: 0,
                event: SessionEvent::RunStarted {
                    run_id: "r1".into(),
                    goal: "ping".into(),
                    criteria: vec![],
                    agent_id: None,
                    system_prompt: None,
                },
            },
            SessionEntry {
                seq: 1,
                event: SessionEvent::LlmCompleted {
                    turn: 0,
                    message: Message {
                        role: Role::Assistant,
                        content: Content::Text("pong".into()),
                        tool_calls: vec![],
                        token_count: None,
                    },
                    provider_replay: None,
                },
            },
        ];
        let msgs = replay_messages(&entries);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
    }

    #[test]
    fn replay_tool_completed() {
        let call_id = compact_str::CompactString::new("c1");
        let entries = vec![SessionEntry {
            seq: 0,
            event: SessionEvent::ToolCompleted {
                turn: 0,
                results: vec![ToolResult {
                    call_id: call_id.clone(),
                    output: Content::Text("ok".into()),
                    is_error: false,
                    token_count: None,
                }],
            },
        }];
        let msgs = replay_messages(&entries);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::Tool);
    }
}
