use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::types::message::{Content, ContentPart, Message, Role};

use super::session_log::SessionEntry;

pub fn is_mid_run(entries: &[SessionEntry]) -> bool {
    !entries.is_empty()
        && !entries
            .iter()
            .any(|e| matches!(e.event, SessionEvent::RunTerminal { .. }))
}

pub fn replay_messages(entries: &[SessionEntry]) -> Vec<Message> {
    let mut messages = Vec::new();
    for entry in entries {
        match &entry.event {
            SessionEvent::RunStarted {
                goal,
                criteria,
                ..
            } => {
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
            SessionEvent::LlmCompleted { message, .. } => {
                messages.push(message.clone());
            }
            SessionEvent::ToolCompleted { results, .. } => {
                for r in results {
                    let output = match &r.output {
                        Content::Text(t) => t.clone(),
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
