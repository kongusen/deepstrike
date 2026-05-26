use super::partitions::ContextPartitions;
use super::token_engine::ContextTokenEngine;
use crate::types::message::{Content, Message};

/// Structured render output.
#[derive(Debug, Clone)]
pub struct RenderedContext {
    /// Combined system text: system partition + task_state + dashboard.
    pub system_text: String,
    /// Strictly alternating user / assistant / tool turns from history,
    /// with working signals folded into the first user turn.
    pub turns: Vec<Message>,
}

/// Build system_text: system partition → task_state block → dashboard block.
/// Each non-empty block is separated by a blank line.
fn build_system_text(partitions: &ContextPartitions) -> String {
    let system = partitions
        .system
        .messages
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join("\n\n");

    let task = partitions.task_state.format_compact();
    let dashboard = partitions.dashboard.format_compact();

    [system.as_str(), task.as_str(), dashboard.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn collect_signals(partitions: &ContextPartitions) -> Vec<String> {
    partitions
        .working
        .messages
        .iter()
        .filter_map(|m| m.content.as_text().map(str::to_owned))
        .collect()
}

/// Render the context into a `RenderedContext` suitable for a provider API call.
///
/// Token budget accounting:
///   - System text tokens are measured by the engine and subtracted from budget.
///   - Remaining budget is allocated to history turns in order.
///   - If a message fits entirely, it is included as-is.
///   - If it exceeds the remaining budget, text messages are truncated via the
///     engine; Parts messages are included whole (mangling structure is worse
///     than a minor overrun).
pub fn render(
    partitions: &ContextPartitions,
    budget: u32,
    engine: &ContextTokenEngine,
) -> RenderedContext {
    let system_text = build_system_text(partitions);
    let signals = collect_signals(partitions);

    let system_tokens = engine.count(&system_text).min(budget);
    let mut remaining = budget.saturating_sub(system_tokens);

    let mut turns: Vec<Message> = Vec::new();

    for msg in &partitions.history.messages {
        let tokens = msg.token_count.unwrap_or_else(|| engine.count_message(msg));

        if tokens == 0 {
            continue;
        }

        if tokens <= remaining {
            turns.push(msg.clone());
            remaining = remaining.saturating_sub(tokens);
        } else if remaining > 0 {
            match &msg.content {
                Content::Text(_) => {
                    let truncated = engine.truncate_message(msg, remaining);
                    turns.push(truncated);
                    remaining = 0;
                }
                Content::Parts(_) => {
                    turns.push(msg.clone());
                    remaining = remaining.saturating_sub(tokens);
                }
            }
        } else {
            break;
        }
    }

    // Fold working signals into the first user turn as a prefix.
    if !signals.is_empty() {
        let prefix = signals.join("\n");
        if let Some(first) = turns.first_mut() {
            if let Content::Text(ref mut text) = first.content {
                *text = format!("{prefix}\n\n{text}");
            }
        } else {
            turns.push(Message::user(prefix));
        }
    }

    RenderedContext { system_text, turns }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::partitions::ContextPartitions;
    use crate::context::task_state::TaskState;
    use crate::context::token_engine::ContextTokenEngine;
    use crate::types::message::{Message, Role};

    fn engine() -> ContextTokenEngine {
        ContextTokenEngine::char_approx()
    }
    fn ctx() -> ContextPartitions {
        ContextPartitions::new(&ContextConfig::default())
    }

    #[test]
    fn system_text_contains_system_partition() {
        let mut c = ctx();
        c.system.push(Message::system("You are helpful."), 10);
        assert!(
            render(&c, 10_000, &engine())
                .system_text
                .contains("You are helpful.")
        );
    }

    #[test]
    fn task_state_appears_in_system_text() {
        let mut c = ctx();
        c.system.push(Message::system("rules"), 5);
        c.task_state = TaskState {
            goal: "find the bug".to_string(),
            ..Default::default()
        };
        let rc = render(&c, 10_000, &engine());
        assert!(rc.system_text.contains("[TASK STATE] goal: find the bug"));
    }

    #[test]
    fn dashboard_appended_when_non_empty() {
        let mut c = ctx();
        c.system.push(Message::system("rules"), 5);
        c.dashboard.goal_progress = "halfway".to_string();
        let rc = render(&c, 10_000, &engine());
        assert!(rc.system_text.contains("halfway"));
    }

    #[test]
    fn empty_task_state_not_in_system_text() {
        let mut c = ctx();
        c.system.push(Message::system("rules"), 5);
        let rc = render(&c, 10_000, &engine());
        assert!(!rc.system_text.contains("[TASK STATE]"));
    }

    #[test]
    fn working_signals_folded_into_first_turn() {
        let mut c = ctx();
        c.working.push(Message::user("[INTERRUPT] stop"), 0);
        c.history.push(Message::user("do the task"), 5);
        let rc = render(&c, 10_000, &engine());
        assert_eq!(rc.turns.len(), 1);
        let text = rc.turns[0].content.as_text().unwrap();
        assert!(text.contains("[INTERRUPT] stop"));
        assert!(text.contains("do the task"));
    }

    #[test]
    fn no_consecutive_user_messages_with_signals() {
        let mut c = ctx();
        c.working.push(Message::user("[INTERRUPT] x"), 0);
        c.history.push(Message::user("goal"), 5);
        c.history.push(Message::assistant("reply"), 5);
        let rc = render(&c, 10_000, &engine());
        let consecutive = rc
            .turns
            .windows(2)
            .any(|w| w[0].role == Role::User && w[1].role == Role::User);
        assert!(!consecutive);
    }

    #[test]
    fn zero_token_messages_skipped() {
        let mut c = ctx();
        c.history.push(Message::user("zero"), 0);
        c.history.push(Message::user("real"), 5);
        let rc = render(&c, 10_000, &engine());
        assert_eq!(rc.turns.len(), 1);
        assert_eq!(rc.turns[0].content.as_text().unwrap(), "real");
    }

    #[test]
    fn text_truncated_when_budget_exhausted() {
        let mut c = ctx();
        c.history.push(Message::user("first message"), 5);
        c.history.push(Message::user("a".repeat(1000)), 250);
        let rc = render(&c, 10, &engine());
        let has_first = rc.turns.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("first message"))
                .unwrap_or(false)
        });
        assert!(has_first);
    }

    #[test]
    fn cjk_truncation_produces_valid_utf8() {
        let mut c = ctx();
        c.history.push(Message::user("first"), 5);
        c.history.push(Message::user("你".repeat(400)), 250);
        let rc = render(&c, 10, &engine());
        for turn in &rc.turns {
            if let Some(t) = turn.content.as_text() {
                assert!(std::str::from_utf8(t.as_bytes()).is_ok());
            }
        }
    }
}
