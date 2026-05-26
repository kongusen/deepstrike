use async_trait::async_trait;
use deepstrike_core::memory::curator::CurationResult;
use deepstrike_core::memory::durable::SessionData;
use deepstrike_core::memory::semantic::MemoryEntry;

/// Backing store for the idle dreaming pipeline.
///
/// Implementors bridge the kernel's pure-computation delta to whatever durable
/// storage the application uses (Postgres, SQLite, a vector DB, etc.).
///
/// # Contract
/// `commit` is always called with the `CurationResult` produced from the
/// `existing` slice returned by `load_memories` in the same cycle.
/// The `to_remove_indices` inside `CurationResult` index into that slice,
/// so the implementor must correlate them itself.
#[async_trait]
pub trait DreamStore: Send + Sync {
    /// Load recent sessions for the given agent. The kernel caps processing
    /// at `IdlePolicy::max_sessions_per_run`; returning more is fine.
    async fn load_sessions(&self, agent_id: &str) -> crate::Result<Vec<SessionData>>;

    /// Load all current long-term memory entries for the agent.
    async fn load_memories(&self, agent_id: &str) -> crate::Result<Vec<MemoryEntry>>;

    /// Apply the curation delta — add new entries, remove stale ones.
    ///
    /// `existing` is the same slice returned by `load_memories` in this cycle;
    /// it is needed to resolve `result.to_remove_indices` back to concrete entries.
    async fn commit(
        &self,
        agent_id: &str,
        result: CurationResult,
        existing: &[MemoryEntry],
    ) -> crate::Result<()>;

    /// Semantic search over the agent's long-term memories.
    /// Called on demand during a session when the LLM invokes the `memory` meta-tool.
    async fn search(
        &self,
        agent_id: &str,
        query: &str,
        top_k: usize,
    ) -> crate::Result<Vec<MemoryEntry>>;

    /// Persist a completed session for future consolidation via `Agent::dream()`.
    async fn save_session(
        &self,
        data: deepstrike_core::memory::durable::SessionData,
    ) -> crate::Result<()>;
}

/// Summary of one dreaming cycle returned to the caller.
#[derive(Debug, Default, Clone)]
pub struct DreamResult {
    pub sessions_processed: usize,
    pub insights_extracted: usize,
    pub entries_added: usize,
    pub entries_removed: usize,
}

/// In-process scratch pad for within-run state.
#[derive(Default)]
pub struct WorkingMemory {
    store: std::collections::HashMap<String, serde_json::Value>,
}

impl WorkingMemory {
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.store.insert(key.into(), value.into());
    }
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.store.get(key)
    }
    pub fn clear(&mut self) {
        self.store.clear();
    }
}
