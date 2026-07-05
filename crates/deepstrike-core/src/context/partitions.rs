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

/// One durable knowledge entry. Unlike history messages, knowledge entries have IDENTITY —
/// a host-assigned key enabling upsert (refresh a pinned reference) and targeted removal —
/// plus lifecycle flags driving the boundary sweep (K1/K2 of the dynamic-control spec).
#[derive(Debug, Clone)]
pub struct KnowledgeEntry {
    /// `None` ⇒ legacy unkeyed append (initialMemory, old snapshots). Keyed entries upsert.
    pub key: Option<compact_str::CompactString>,
    pub message: Message,
    pub tokens: u32,
    /// Host-pinned ⇒ never budget-evicted (K2). Skill pins are NOT host-pinned (K3 governs them).
    pub pinned: bool,
    /// Marked for removal at the next compaction/renewal boundary. Knowledge renders into the
    /// cached system[1] block, so existing bytes are only rewritten where the prompt-cache prefix
    /// is being rebuilt anyway — the same principle as `reset_collapse_generation`.
    pub evict_at_boundary: bool,
    /// Deferred upsert: a same-key push mid-generation stages its replacement here instead of
    /// rewriting rendered bytes; applied by [`KnowledgePartition::sweep_at_boundary`].
    pub pending: Option<Box<(Message, u32)>>,
}

/// Outcome of one boundary sweep, for the `KnowledgeSwept` kernel observation.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeSweep {
    pub removed_keys: Vec<String>,
    pub tokens_freed: u32,
    /// True when the sweep changed anything (removal OR applied upsert).
    pub changed: bool,
}

/// The knowledge partition: durable, identity-bearing entries rendered into system[1].
/// Appends are immediate (they extend the cached prefix — the cheap direction); mutation and
/// removal of existing entries are boundary-deferred (see [`KnowledgeEntry::evict_at_boundary`]).
#[derive(Debug, Clone, Default)]
pub struct KnowledgePartition {
    pub entries: Vec<KnowledgeEntry>,
    pub token_count: u32,
}

impl KnowledgePartition {
    pub fn new() -> Self { Self::default() }

    /// Unkeyed immediate append — exactly `push_entry(None, msg, tokens, false)`.
    pub fn push(&mut self, msg: Message, token_count: u32) {
        self.push_entry(None, msg, token_count, false);
    }

    /// Keyed push: a fresh key (or `None`) appends immediately; an existing key stages a
    /// boundary-deferred upsert (and clears any pending eviction — the entry is wanted again).
    /// `pinned` takes effect immediately in both cases (it is bookkeeping, not rendered bytes).
    pub fn push_entry(
        &mut self,
        key: Option<compact_str::CompactString>,
        mut msg: Message,
        tokens: u32,
        pinned: bool,
    ) {
        msg.token_count = Some(tokens);
        if let Some(ref k) = key {
            if let Some(entry) = self.entries.iter_mut().find(|e| e.key.as_ref() == Some(k)) {
                entry.pending = Some(Box::new((msg, tokens)));
                entry.evict_at_boundary = false;
                entry.pinned = pinned;
                return;
            }
        }
        self.token_count += tokens;
        self.entries.push(KnowledgeEntry {
            key,
            message: msg,
            tokens,
            pinned,
            evict_at_boundary: false,
            pending: None,
        });
    }

    /// Mark the keyed entry for removal at the next boundary. Errs-open: unknown key is a no-op.
    /// Returns whether a matching entry was marked.
    pub fn remove(&mut self, key: &str) -> bool {
        match self.entries.iter_mut().find(|e| e.key.as_deref() == Some(key)) {
            Some(entry) => {
                entry.evict_at_boundary = true;
                entry.pending = None;
                true
            }
            None => false,
        }
    }

    /// Apply pending upserts and drop marked entries. Call ONLY at compaction/renewal
    /// boundaries — this is the one place existing system[1] bytes may be rewritten.
    pub fn sweep_at_boundary(&mut self) -> KnowledgeSweep {
        let mut sweep = KnowledgeSweep::default();
        for entry in &mut self.entries {
            if let Some(replacement) = entry.pending.take() {
                let (msg, tokens) = *replacement;
                self.token_count = self.token_count - entry.tokens + tokens;
                entry.message = msg;
                entry.tokens = tokens;
                sweep.changed = true;
            }
        }
        let before = self.entries.len();
        self.entries.retain(|e| {
            if e.evict_at_boundary {
                if let Some(ref k) = e.key {
                    sweep.removed_keys.push(k.to_string());
                }
                sweep.tokens_freed += e.tokens;
                false
            } else {
                true
            }
        });
        if self.entries.len() != before {
            self.token_count = self.token_count.saturating_sub(sweep.tokens_freed);
            sweep.changed = true;
        }
        sweep
    }

    /// The rendered messages, in entry order (renderer / snapshot surface).
    pub fn messages(&self) -> impl Iterator<Item = &Message> {
        self.entries.iter().map(|e| &e.message)
    }

    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
}

/// Four-slot context model aligned with LLM API slots (five fields — slot 3 spans
/// `task_state` + `signals`):
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
    pub knowledge: KnowledgePartition,
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
            knowledge: KnowledgePartition::new(),
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

    // ── K1: keyed knowledge entries ──────────────────────────────────────────

    fn text_of(p: &KnowledgePartition) -> Vec<String> {
        p.messages().filter_map(|m| m.content.as_text().map(str::to_string)).collect()
    }

    #[test]
    fn keyed_upsert_defers_to_boundary() {
        let mut p = KnowledgePartition::new();
        p.push_entry(Some("ref".into()), Message::system("v1"), 10, false);
        p.push_entry(Some("ref".into()), Message::system("v2"), 12, false);
        // Mid-generation: still ONE entry rendering the ORIGINAL bytes (system[1] untouched).
        assert_eq!(p.len(), 1);
        assert_eq!(text_of(&p), vec!["v1"]);
        assert_eq!(p.token_count, 10);

        let sweep = p.sweep_at_boundary();
        assert!(sweep.changed);
        assert!(sweep.removed_keys.is_empty(), "upsert-only sweep removes nothing");
        assert_eq!(text_of(&p), vec!["v2"]);
        assert_eq!(p.token_count, 12);
    }

    #[test]
    fn remove_marks_then_sweep_drops() {
        let mut p = KnowledgePartition::new();
        p.push_entry(Some("ref".into()), Message::system("pinned ref"), 8, false);
        assert!(p.remove("ref"));
        // Still rendered until the boundary (no mid-generation byte rewrite).
        assert_eq!(p.len(), 1);
        assert_eq!(text_of(&p), vec!["pinned ref"]);

        let sweep = p.sweep_at_boundary();
        assert!(sweep.changed);
        assert_eq!(sweep.removed_keys, vec!["ref".to_string()]);
        assert_eq!(sweep.tokens_freed, 8);
        assert!(p.is_empty());
        assert_eq!(p.token_count, 0);
    }

    #[test]
    fn remove_unknown_key_errs_open() {
        let mut p = KnowledgePartition::new();
        p.push(Message::system("unkeyed"), 5);
        assert!(!p.remove("missing"));
        assert!(!p.sweep_at_boundary().changed);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn same_key_push_after_remove_revives_entry() {
        let mut p = KnowledgePartition::new();
        p.push_entry(Some("ref".into()), Message::system("v1"), 5, false);
        p.remove("ref");
        // Re-pushing the key means the entry is wanted again — the eviction mark clears and the
        // fresh content lands as a deferred upsert.
        p.push_entry(Some("ref".into()), Message::system("v2"), 6, false);
        let sweep = p.sweep_at_boundary();
        assert!(sweep.removed_keys.is_empty());
        assert_eq!(text_of(&p), vec!["v2"]);
    }

    #[test]
    fn fresh_keys_and_unkeyed_append_immediately() {
        let mut p = KnowledgePartition::new();
        p.push(Message::system("legacy"), 3);
        p.push_entry(Some("a".into()), Message::system("fresh"), 4, true);
        // Appends are visible right away (cache-cheap direction: prefix only extends).
        assert_eq!(text_of(&p), vec!["legacy", "fresh"]);
        assert_eq!(p.token_count, 7);
        assert!(p.entries[1].pinned);
    }
}
