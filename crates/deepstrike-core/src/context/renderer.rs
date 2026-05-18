use super::partitions::ContextPartitions;
use crate::types::message::{Content, Message};

/// Structured render output — replaces the flat Vec<Message> interface.
///
/// Separates system-level configuration from the conversation transcript so
/// every provider can map each field to its own API contract without ad-hoc
/// role-filtering.
#[derive(Debug, Clone)]
pub struct RenderedContext {
    /// Combined system text: system partition + dashboard (when non-empty).
    /// Maps to: Anthropic `system` param · OpenAI messages[0] system role ·
    /// Gemini `systemInstruction`.
    pub system_text: String,

    /// Strictly alternating user / assistant / tool turns drawn from the
    /// history partition.  Any working-partition signals are folded into
    /// the first user turn as a prefix so providers never see two
    /// consecutive user messages.
    pub turns: Vec<Message>,
}

/// Build the system text by concatenating the system partition with the
/// dashboard overlay.  The dashboard is only appended when it has content
/// (non-empty goal_progress, plan, or scratchpad).
fn build_system_text(partitions: &ContextPartitions) -> String {
    let system_parts: Vec<&str> = partitions
        .system
        .messages
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect();

    let system = system_parts.join("\n\n");
    let dashboard = partitions.dashboard.format_compact();

    if dashboard.is_empty() {
        system
    } else if system.is_empty() {
        dashboard
    } else {
        format!("{system}\n\n{dashboard}")
    }
}

/// Collect working-partition signal texts (already formatted as
/// "[INTERRUPT] …" or "[SIGNAL] …").
fn collect_signals(partitions: &ContextPartitions) -> Vec<String> {
    partitions
        .working
        .messages
        .iter()
        .filter_map(|m| m.content.as_text().map(|s| s.to_owned()))
        .collect()
}

/// Render the five-partition context into a RenderedContext suitable for
/// provider API calls.
///
/// Rendering strategy:
///   system_text = system partition + dashboard (compact)
///   turns       = working signals folded into first history turn
///                 + remaining history, respecting the token budget.
pub fn render(partitions: &ContextPartitions, budget: u32) -> RenderedContext {
    let system_text = build_system_text(partitions);
    let signals = collect_signals(partitions);

    // Account for system_text tokens when computing the remaining budget for
    // turns (rough estimate: 1 token ≈ 4 chars).
    let system_tokens = (system_text.len() as u32 / 4).min(budget);
    let mut remaining = budget.saturating_sub(system_tokens);

    let mut turns: Vec<Message> = Vec::new();

    for msg in &partitions.history.messages {
        let tokens = msg.token_count.unwrap_or(0);

        // Never silently drop a message: if it fits, include in full.
        // If it doesn't fit but there is remaining budget, truncate the
        // text content so something meaningful still reaches the LLM.
        // Parts messages (tool results) are included without truncation
        // as long as any budget remains — mangling structured content is
        // worse than exceeding the estimate by a few tokens.
        if tokens == 0 {
            // Zero-token messages are structural placeholders; skip them.
            continue;
        }

        if tokens <= remaining {
            turns.push(msg.clone());
            remaining = remaining.saturating_sub(tokens);
        } else if remaining > 0 {
            match &msg.content {
                Content::Text(text) => {
                    let keep_chars =
                        (text.len() * remaining as usize / tokens as usize).max(1);
                    let keep_chars = keep_chars.min(text.len());
                    let mut truncated = msg.clone();
                    truncated.content = Content::Text(format!(
                        "{}… [truncated]",
                        &text[..keep_chars]
                    ));
                    truncated.token_count = Some(remaining);
                    turns.push(truncated);
                    remaining = 0;
                }
                Content::Parts(_) => {
                    // Include parts messages whole — do not mangle structure.
                    turns.push(msg.clone());
                    remaining = remaining.saturating_sub(tokens);
                }
            }
        } else {
            // No budget left; stop.
            break;
        }
    }

    // Fold working signals into the first user turn.
    // If history is empty, the signals become the sole user turn.
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
    use crate::context::partitions::ContextPartitions;
    use crate::types::message::{Message, Role};

    #[test]
    fn system_text_contains_system_partition() {
        let mut ctx = ContextPartitions::new();
        ctx.system.push(Message::system("You are helpful."), 10);
        let rc = render(&ctx, 10_000);
        assert!(rc.system_text.contains("You are helpful."));
    }

    #[test]
    fn dashboard_appended_to_system_text_when_non_empty() {
        let mut ctx = ContextPartitions::new();
        ctx.system.push(Message::system("rules"), 5);
        ctx.dashboard.goal_progress = "halfway".to_string();
        let rc = render(&ctx, 10_000);
        assert!(rc.system_text.contains("rules"));
        assert!(rc.system_text.contains("halfway"));
    }

    #[test]
    fn dashboard_not_appended_when_empty() {
        let mut ctx = ContextPartitions::new();
        ctx.system.push(Message::system("rules"), 5);
        let rc = render(&ctx, 10_000);
        // Default dashboard is all-empty; compact form should be empty string.
        assert!(!rc.system_text.contains("[AGENT STATE]"));
    }

    #[test]
    fn working_signals_folded_into_first_turn() {
        let mut ctx = ContextPartitions::new();
        ctx.working.push(Message::user("[INTERRUPT] stop"), 0);
        ctx.history.push(Message::user("do the task"), 5);
        let rc = render(&ctx, 10_000);
        assert_eq!(rc.turns.len(), 1);
        let content = rc.turns[0].content.as_text().unwrap();
        assert!(content.contains("[INTERRUPT] stop"));
        assert!(content.contains("do the task"));
    }

    #[test]
    fn working_signals_become_sole_turn_when_history_empty() {
        let mut ctx = ContextPartitions::new();
        ctx.working.push(Message::user("[SIGNAL] new data"), 0);
        let rc = render(&ctx, 10_000);
        assert_eq!(rc.turns.len(), 1);
        assert_eq!(rc.turns[0].role, Role::User);
    }

    #[test]
    fn no_consecutive_user_messages_with_signals() {
        let mut ctx = ContextPartitions::new();
        ctx.working.push(Message::user("[INTERRUPT] x"), 0);
        ctx.history.push(Message::user("goal"), 5);
        ctx.history.push(Message::assistant("reply"), 5);
        let rc = render(&ctx, 10_000);
        // First turn is the merged user message; second is assistant.
        assert_eq!(rc.turns[0].role, Role::User);
        assert_eq!(rc.turns[1].role, Role::Assistant);
        // No two consecutive user turns.
        let consecutive = rc
            .turns
            .windows(2)
            .any(|w| w[0].role == Role::User && w[1].role == Role::User);
        assert!(!consecutive);
    }

    #[test]
    fn zero_token_messages_skipped() {
        let mut ctx = ContextPartitions::new();
        ctx.history.push(Message::user("zero token msg"), 0);
        ctx.history.push(Message::user("real msg"), 5);
        let rc = render(&ctx, 10_000);
        // Only the message with real tokens appears.
        assert_eq!(rc.turns.len(), 1);
        assert_eq!(rc.turns[0].content.as_text().unwrap(), "real msg");
    }

    #[test]
    fn text_truncated_when_budget_exhausted() {
        let mut ctx = ContextPartitions::new();
        ctx.history.push(Message::user("first message"), 5);
        // Second message is very large relative to remaining budget.
        ctx.history.push(Message::user("a".repeat(1000)), 250);
        let rc = render(&ctx, 10); // tight budget
        // Both messages attempted; at least the first fits.
        let has_first = rc.turns.iter().any(|m| {
            m.content.as_text().map(|t| t.contains("first message")).unwrap_or(false)
        });
        assert!(has_first);
    }

    #[test]
    fn render_order_system_then_turns() {
        let mut ctx = ContextPartitions::new();
        ctx.system.push(Message::system("rules"), 10);
        ctx.history.push(Message::user("hello"), 5);
        let rc = render(&ctx, 10_000);
        assert!(rc.system_text.contains("rules"));
        assert_eq!(rc.turns[0].role, Role::User);
    }
}
