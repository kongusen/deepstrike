use super::config::ContextConfig;
use super::dashboard::Dashboard;
use super::task_state::TaskState;
use super::token_engine::ContextTokenEngine;
use crate::types::message::Message;

/// Priority level for context partitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Low = 0,
    MediumLow = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
}

/// A single context partition.
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

/// Six-partition context model plus structured task state:
///   C = C_system + C_working + task_state + C_memory + C_skill + C_artifacts + C_history
pub struct ContextPartitions {
    pub system: Partition,
    pub working: Partition,
    /// Structured task state — rendered into system_text, never compressed.
    pub task_state: TaskState,
    pub dashboard: Dashboard,
    pub memory: Partition,
    pub skill: Partition,
    pub artifacts: Partition,
    pub history: Partition,
}

impl ContextPartitions {
    pub fn new(_config: &ContextConfig) -> Self {
        Self {
            system: Partition::new(Priority::Critical, false),
            working: Partition::new(Priority::High, false),
            task_state: TaskState::default(),
            dashboard: Dashboard::default(),
            memory: Partition::new(Priority::Medium, true),
            skill: Partition::new(Priority::MediumLow, true),
            artifacts: Partition::new(Priority::Medium, false),
            history: Partition::new(Priority::Low, true),
        }
    }

    /// Total token count across all partitions.
    /// Dashboard tokens are measured by the engine on each call; TaskState
    /// tokens are measured from the rendered compact form.
    pub fn total_tokens(&self, engine: &ContextTokenEngine) -> u32 {
        self.system.token_count
            + self.working.token_count
            + engine.count(&self.task_state.format_compact())
            + engine.count(&self.dashboard.format_compact())
            + self.memory.token_count
            + self.skill.token_count
            + self.artifacts.token_count
            + self.history.token_count
    }
}

impl Default for ContextPartitions {
    fn default() -> Self {
        Self::new(&ContextConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::token_engine::ContextTokenEngine;
    use crate::types::message::Message;

    fn engine() -> ContextTokenEngine {
        ContextTokenEngine::char_approx()
    }
    fn config() -> ContextConfig {
        ContextConfig::default()
    }

    #[test]
    fn push_updates_token_count() {
        let mut ctx = ContextPartitions::new(&config());
        let base = ctx.total_tokens(&engine());
        ctx.system.push(Message::system("rules"), 10);
        ctx.history.push(Message::user("hello"), 5);
        assert_eq!(ctx.total_tokens(&engine()), base + 15);
    }

    #[test]
    fn task_state_tokens_included_in_total() {
        use crate::context::task_state::TaskState;
        let mut ctx = ContextPartitions::new(&config());
        let before = ctx.total_tokens(&engine());
        ctx.task_state = TaskState {
            goal: "do something important".to_string(),
            ..Default::default()
        };
        let after = ctx.total_tokens(&engine());
        assert!(
            after > before,
            "task_state should contribute to total_tokens"
        );
    }
}
