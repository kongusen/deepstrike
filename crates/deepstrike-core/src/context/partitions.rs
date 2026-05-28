use super::config::ContextConfig;
use super::task_state::TaskState;
use super::token_engine::ContextTokenEngine;
use crate::types::message::Message;

/// A single context partition — a named bucket of messages with a token counter.
#[derive(Debug, Clone)]
pub struct Partition {
    pub messages: Vec<Message>,
    pub token_count: u32,
}

impl Partition {
    pub fn new() -> Self {
        Self { messages: Vec::new(), token_count: 0 }
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

    pub fn len(&self) -> usize { self.messages.len() }
    pub fn is_empty(&self) -> bool { self.messages.is_empty() }
}

impl Default for Partition {
    fn default() -> Self { Self::new() }
}

/// Three-partition context model aligned with LLM API slots:
///
///   Slot 1 — Identity  (system):    who the agent is; role, rules, constraints.
///                                    Maps to: Anthropic system[0] cache_control, OpenAI system role.
///                                    Never changes within a run.
///
///   Slot 2 — Knowledge (knowledge): what the agent knows; memory retrievals, skill
///                                    definitions, artifacts. Low-frequency changes.
///                                    Maps to: Anthropic system[1] cache_control.
///
///   Slot 3 — State     (task_state + signals): what the agent is doing right now.
///                                    task_state = goal/plan/progress (structured).
///                                    signals = runtime events (rollback notes, interrupts).
///                                    Maps to: messages[0] user turn, rebuilt every call.
///
///   Slot 4 — History   (history):   what the agent has done; conversation turns,
///                                    tool calls and results. Compression pipeline target.
///                                    Maps to: messages[1..N].
pub struct ContextPartitions {
    pub system: Partition,
    pub knowledge: Partition,
    pub task_state: TaskState,
    /// Runtime signals injected into the current turn (rollback notes, interrupts).
    /// Cleared after each render — signals are ephemeral per-turn events.
    pub signals: Vec<String>,
    pub history: Partition,
}

impl ContextPartitions {
    pub fn new(_config: &ContextConfig) -> Self {
        Self {
            system: Partition::new(),
            knowledge: Partition::new(),
            task_state: TaskState::default(),
            signals: Vec::new(),
            history: Partition::new(),
        }
    }

    /// Total token count across all slots.
    /// task_state tokens are measured from its rendered compact form.
    pub fn total_tokens(&self, engine: &ContextTokenEngine) -> u32 {
        self.system.token_count
            + self.knowledge.token_count
            + engine.count(&self.task_state.format_compact())
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

    fn engine() -> ContextTokenEngine { ContextTokenEngine::char_approx() }

    #[test]
    fn push_updates_token_count() {
        let mut ctx = ContextPartitions::new(&ContextConfig::default());
        let base = ctx.total_tokens(&engine());
        ctx.system.push(Message::system("rules"), 10);
        ctx.history.push(Message::user("hello"), 5);
        assert_eq!(ctx.total_tokens(&engine()), base + 15);
    }

    #[test]
    fn task_state_tokens_included_in_total() {
        use crate::context::task_state::TaskState;
        let mut ctx = ContextPartitions::new(&ContextConfig::default());
        let before = ctx.total_tokens(&engine());
        ctx.task_state = TaskState { goal: "do something important".to_string(), ..Default::default() };
        let after = ctx.total_tokens(&engine());
        assert!(after > before, "task_state should contribute to total_tokens");
    }

    #[test]
    fn knowledge_tokens_included_in_total() {
        let mut ctx = ContextPartitions::new(&ContextConfig::default());
        let before = ctx.total_tokens(&engine());
        ctx.knowledge.push(Message::system("skill: debug"), 20);
        assert_eq!(ctx.total_tokens(&engine()), before + 20);
    }
}
