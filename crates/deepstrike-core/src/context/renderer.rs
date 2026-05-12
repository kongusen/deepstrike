use super::partitions::ContextPartitions;
use crate::types::message::Message;

/// Renders the five-partition context into a flat message sequence
/// suitable for LLM API calls.
///
/// Rendering order: system → working(dashboard) → memory → skill → history
pub fn render(partitions: &ContextPartitions, budget: u32) -> Vec<Message> {
    let mut result: Vec<Message> = Vec::new();
    let mut remaining = budget;

    // system first
    for msg in &partitions.system.messages {
        let tokens = msg.token_count.unwrap_or(0);
        if tokens == 0 { continue; }
        if tokens <= remaining {
            result.push(msg.clone());
            remaining = remaining.saturating_sub(tokens);
        }
    }

    // working partition second (goal, signals, interrupts)
    // Always included — working messages are critical context even when token_count=0.
    for msg in &partitions.working.messages {
        let tokens = msg.token_count.unwrap_or(0);
        result.push(msg.clone());
        remaining = remaining.saturating_sub(tokens);
    }

    // dashboard overlay
    let dashboard_msg = partitions.dashboard.format_message();
    let dashboard_tokens = (dashboard_msg.content.text_len() / 4) as u32;
    if dashboard_tokens <= remaining {
        result.push(dashboard_msg);
        remaining = remaining.saturating_sub(dashboard_tokens);
    }

    // memory, skill, history
    let ordered: [&[Message]; 3] = [
        &partitions.memory.messages,
        &partitions.skill.messages,
        &partitions.history.messages,
    ];

    for messages in ordered {
        for msg in messages {
            let tokens = msg.token_count.unwrap_or(0);
            if tokens == 0 { continue; }
            if tokens <= remaining {
                result.push(msg.clone());
                remaining = remaining.saturating_sub(tokens);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::partitions::ContextPartitions;
    use crate::types::message::{Message, Role};

    #[test]
    fn render_prioritizes_system() {
        let mut ctx = ContextPartitions::new();
        ctx.system.push(Message::system("safety first"), 50);
        ctx.history.push(Message::user("hi"), 100);

        let msgs = render(&ctx, 60);
        assert_eq!(msgs[0].role, Role::System);
    }

    #[test]
    fn render_order_system_dashboard_history() {
        let mut ctx = ContextPartitions::new();
        ctx.system.push(Message::system("rules"), 10);
        ctx.history.push(Message::user("hello"), 5);

        let msgs = render(&ctx, 10000);
        // system first, then dashboard, then history
        assert_eq!(msgs[0].role, Role::System);
        // dashboard is a system message too
        assert_eq!(msgs[1].role, Role::System);
        assert_eq!(msgs[2].role, Role::User);
    }
}

