use crate::types::message::Message;
use crate::memory::durable::{SessionData, SessionStore};
use crate::memory::semantic::SemanticMemory;
use crate::memory::extractor::MemoryExtractor;
use crate::memory::session::{RestorePolicy, RestoreConfig, restore};

pub struct MemoryRuntime {
    pub session_store: Box<dyn SessionStore>,
    pub semantic_store: Option<Box<dyn SemanticMemory>>,
    pub extractor: MemoryExtractor,
    pub restore_policy: RestorePolicy,
    pub restore_config: RestoreConfig,
}

impl MemoryRuntime {
    pub fn new(
        session_store: Box<dyn SessionStore>,
        semantic_store: Option<Box<dyn SemanticMemory>>,
        extractor: MemoryExtractor,
        restore_policy: RestorePolicy,
    ) -> Self {
        Self {
            session_store,
            semantic_store,
            extractor,
            restore_policy,
            restore_config: RestoreConfig::default(),
        }
    }

    /// Load session history and apply restore policy to build initial context.
    pub fn on_run_start(&self, session_id: &str, _goal: &str) -> Vec<Message> {
        let history = self
            .session_store
            .load(session_id)
            .ok()
            .flatten()
            .map(|d| d.messages)
            .unwrap_or_default();
        restore(&self.restore_policy, &self.restore_config, &history)
    }

    /// Extract memories from the current turn and store in semantic memory.
    pub fn on_turn_end(&mut self, user_msg: &Message, assistant_msg: &Message) {
        if let Some(sem) = &self.semantic_store {
            for entry in self.extractor.extract(&[user_msg.clone(), assistant_msg.clone()]) {
                let _ = sem.store(entry);
            }
        }
    }

    /// Optionally store a tool result in semantic memory.
    pub fn on_tool_result(&mut self, _tool_name: &str, result: &str) {
        if let Some(sem) = &self.semantic_store {
            let _ = sem.store(crate::memory::semantic::MemoryEntry {
                text: result.to_string(),
                score: 0.0,
                metadata: serde_json::Value::Null,
            });
        }
    }

    /// Extract memories from the run and persist the session.
    /// `now_ms` is injected by the SDK layer — the kernel never reads system time.
    pub fn on_run_end(
        &mut self,
        session_id: &str,
        agent_id: &str,
        messages: &[Message],
        now_ms: u64,
    ) {
        let extracted = self.extractor.extract(messages);
        if let Some(sem) = &self.semantic_store {
            for entry in extracted {
                let _ = sem.store(entry);
            }
        }
        let data = SessionData {
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            messages: messages.to_vec(),
            metadata: serde_json::Value::Null,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        let _ = self.session_store.save(session_id, &data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::durable::InMemoryStore;
    use crate::memory::extractor::ExtractionPolicy;

    fn make_runtime() -> MemoryRuntime {
        MemoryRuntime::new(
            Box::new(InMemoryStore::new()),
            None,
            MemoryExtractor::new(ExtractionPolicy::default()),
            RestorePolicy::Window,
        )
    }

    #[test]
    fn on_run_start_returns_empty_for_new_session() {
        let rt = make_runtime();
        let msgs = rt.on_run_start("new-session", "do something");
        assert!(msgs.is_empty());
    }

    #[test]
    fn on_run_end_persists_and_run_start_restores() {
        let mut rt = make_runtime();
        let messages = vec![Message::user("hello"), Message::assistant("a".repeat(101))];
        rt.on_run_end("s1", "agent1", &messages, 1_000_000);
        let restored = rt.on_run_start("s1", "continue");
        assert!(!restored.is_empty());
    }
}
