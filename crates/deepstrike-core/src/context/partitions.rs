use super::dashboard::Dashboard;
use crate::types::message::Message;

/// Priority level for context partitions.
/// Higher priority partitions are compressed last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// History — first to compress
    Low = 0,
    /// Skills — dynamic, can shed unused
    MediumLow = 1,
    /// Memory — durable but compressible
    Medium = 2,
    /// Working — dashboard, active events
    High = 3,
    /// System — safety rules, never compress
    Critical = 4,
}

/// A single context partition with its messages and metadata.
#[derive(Debug, Clone)]
pub struct Partition {
    pub messages: Vec<Message>,
    pub token_count: u32,
    pub priority: Priority,
    pub compressible: bool,
}

impl Partition {
    pub fn new(priority: Priority, compressible: bool) -> Self {
        Self {
            messages: Vec::new(),
            token_count: 0,
            priority,
            compressible,
        }
    }

    pub fn push(&mut self, mut msg: Message, token_count: u32) {
        msg.token_count = Some(token_count);
        self.token_count += token_count;
        self.messages.push(msg);
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.token_count = 0;
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Five-partition context model:
///   C = C_system + C_working + C_memory + C_skill + C_history
pub struct ContextPartitions {
    /// Safety rules, system instructions — never compressed
    pub system: Partition,
    /// Working messages (goal, signals, interrupts) — high priority
    pub working: Partition,
    /// Structured dashboard state — rendered as a system message overlay
    pub dashboard: Dashboard,
    /// Long-term memory entries — compressible
    pub memory: Partition,
    /// Tool/skill declarations — dynamically compressible
    pub skill: Partition,
    /// Execution transcript — lowest priority, compress first
    pub history: Partition,
}

impl ContextPartitions {
    pub fn new() -> Self {
        Self {
            system: Partition::new(Priority::Critical, false),
            working: Partition::new(Priority::High, false),
            dashboard: Dashboard::default(),
            memory: Partition::new(Priority::Medium, true),
            skill: Partition::new(Priority::MediumLow, true),
            history: Partition::new(Priority::Low, true),
        }
    }

    /// Total token count across all partitions.
    /// Dashboard tokens are estimated once from cached text_len.
    pub fn total_tokens(&self) -> u32 {
        self.system.token_count
            + self.working.token_count
            + self.dashboard.token_estimate()
            + self.memory.token_count
            + self.skill.token_count
            + self.history.token_count
    }

    /// Partitions in priority order (lowest first), excluding working and dashboard.
    pub fn partitions_by_priority(&self) -> [&Partition; 4] {
        [&self.history, &self.skill, &self.memory, &self.system]
    }

}

impl Default for ContextPartitions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::Message;

    #[test]
    fn push_message_updates_token_count() {
        let mut ctx = ContextPartitions::new();
        let base = ctx.total_tokens();
        ctx.system.push(Message::system("You are helpful."), 10);
        ctx.history.push(Message::user("Hello"), 5);
        assert_eq!(ctx.total_tokens(), base + 15);
    }

    #[test]
    fn priority_ordering() {
        let parts = ContextPartitions::new();
        let ordered = parts.partitions_by_priority();
        assert_eq!(ordered[0].priority, Priority::Low);
        assert_eq!(ordered[3].priority, Priority::Critical);
    }
}
