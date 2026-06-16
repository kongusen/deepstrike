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

/// `InMemoryDreamStore` — a lightweight `DreamStore` backed by per-agent in-memory maps.
///
/// Rust port of node/src/memory/in-memory-store.ts. Use for benchmarks, unit tests, and local
/// development where persistent memory isn't needed. `search()` is a non-semantic slice — it
/// returns the first `top_k` memories regardless of `query`. The kernel ranks by score, so
/// caller insertion order is what surfaces.
pub struct InMemoryDreamStore {
    sessions: std::sync::Mutex<std::collections::HashMap<String, Vec<SessionData>>>,
    memories: std::sync::Mutex<std::collections::HashMap<String, Vec<MemoryEntry>>>,
    initial_memories: Vec<MemoryEntry>,
    saved_sessions: std::sync::Mutex<Vec<SessionData>>,
}

impl InMemoryDreamStore {
    pub fn new() -> Self {
        Self::with_initial_memories(Vec::new())
    }

    pub fn with_initial_memories(initial: Vec<MemoryEntry>) -> Self {
        Self {
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            memories: std::sync::Mutex::new(std::collections::HashMap::new()),
            initial_memories: initial,
            saved_sessions: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn add_session(&self, agent_id: impl Into<String>, session: SessionData) {
        self.sessions
            .lock()
            .unwrap()
            .entry(agent_id.into())
            .or_default()
            .push(session);
    }

    pub fn add_memories(&self, agent_id: impl Into<String>, entries: Vec<MemoryEntry>) {
        self.memories
            .lock()
            .unwrap()
            .entry(agent_id.into())
            .or_default()
            .extend(entries);
    }

    pub fn saved_sessions(&self) -> Vec<SessionData> {
        self.saved_sessions.lock().unwrap().clone()
    }
}

impl Default for InMemoryDreamStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DreamStore for InMemoryDreamStore {
    async fn load_sessions(&self, agent_id: &str) -> crate::Result<Vec<SessionData>> {
        Ok(self.sessions.lock().unwrap().get(agent_id).cloned().unwrap_or_default())
    }

    async fn load_memories(&self, agent_id: &str) -> crate::Result<Vec<MemoryEntry>> {
        let mut memories = self.memories.lock().unwrap();
        if let Some(existing) = memories.get(agent_id) {
            return Ok(existing.clone());
        }
        if !self.initial_memories.is_empty() {
            memories.insert(agent_id.to_string(), self.initial_memories.clone());
            return Ok(self.initial_memories.clone());
        }
        Ok(Vec::new())
    }

    async fn commit(
        &self,
        agent_id: &str,
        result: CurationResult,
        existing: &[MemoryEntry],
    ) -> crate::Result<()> {
        let remove: std::collections::HashSet<usize> = result.to_remove_indices.iter().copied().collect();
        let mut kept: Vec<MemoryEntry> = existing
            .iter()
            .enumerate()
            .filter_map(|(i, m)| if remove.contains(&i) { None } else { Some(m.clone()) })
            .collect();
        kept.extend(result.to_add);
        self.memories
            .lock()
            .unwrap()
            .insert(agent_id.to_string(), kept);
        Ok(())
    }

    async fn search(
        &self,
        agent_id: &str,
        _query: &str,
        top_k: usize,
    ) -> crate::Result<Vec<MemoryEntry>> {
        let all = self.load_memories(agent_id).await?;
        Ok(all.into_iter().take(top_k).collect())
    }

    async fn save_session(&self, data: SessionData) -> crate::Result<()> {
        self.saved_sessions.lock().unwrap().push(data.clone());
        self.sessions
            .lock()
            .unwrap()
            .entry(data.agent_id.clone())
            .or_default()
            .push(data);
        Ok(())
    }
}
