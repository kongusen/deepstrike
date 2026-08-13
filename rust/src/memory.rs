use async_trait::async_trait;
use deepstrike_core::memory::durable::SessionData;
use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord,
    MemoryScope, MemoryTrustLevel,
};

/// Durable-memory host storage. Runner writes through `put` only after the kernel's
/// `WriteMemory` gate accepts the record.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn put(&self, agent_id: &str, record: MemoryRecord) -> crate::Result<()>;

    /// Return one record by its agent-local id, or `None` when it does not exist.
    async fn get(&self, agent_id: &str, record_id: &str) -> crate::Result<Option<MemoryRecord>>;

    /// Delete one record by its agent-local id. Missing records are a successful no-op.
    async fn delete(&self, agent_id: &str, record_id: &str) -> crate::Result<()>;

    /// Semantic search over the agent's long-term memories.
    /// Called on demand during a session when the LLM invokes the `memory` meta-tool.
    async fn search(&self, agent_id: &str, query: &MemoryQuery)
    -> crate::Result<Vec<MemoryRecall>>;

    /// Persist a completed session before the runner's one extraction pass.
    async fn save_session(
        &self,
        data: deepstrike_core::memory::durable::SessionData,
    ) -> crate::Result<()>;
}

/// Search options for an agent-bound [`DurableMemory`] descriptor.
#[derive(Debug, Clone, Default)]
pub struct MemorySearchOptions {
    pub top_k: Option<usize>,
    pub kinds: Vec<MemoryKind>,
    pub min_score: Option<f64>,
}

/// Public durable memory bound to one agent and scope.
///
/// This is separate from [`WorkingMemory`], which is an in-process scratch pad, and from
/// [`MemoryStore`], which remains host-owned storage for runners and public descriptors.
pub struct DurableMemory {
    store: std::sync::Arc<dyn MemoryStore>,
    agent_id: String,
    scope: MemoryScope,
}

impl DurableMemory {
    pub fn new(
        store: std::sync::Arc<dyn MemoryStore>,
        agent_id: impl Into<String>,
        scope: MemoryScope,
    ) -> Self {
        Self {
            store,
            agent_id: agent_id.into(),
            scope,
        }
    }

    pub fn namespace(&self) -> &str {
        &self.scope.namespace
    }

    pub async fn search(
        &self,
        query: impl Into<String>,
        options: MemorySearchOptions,
    ) -> crate::Result<Vec<MemoryRecord>> {
        let request = MemoryQuery {
            scope: self.scope.clone(),
            query: query.into(),
            top_k: options.top_k.unwrap_or(5),
            kinds: options.kinds,
            min_score: options.min_score,
        };
        Ok(self
            .store
            .search(&self.agent_id, &request)
            .await?
            .into_iter()
            .map(|hit| hit.record)
            .filter(|record| record.scope == self.scope)
            .collect())
    }

    pub async fn get(&self, record_id: &str) -> crate::Result<Option<MemoryRecord>> {
        Ok(self
            .store
            .get(&self.agent_id, record_id)
            .await?
            .filter(|record| record.scope == self.scope))
    }

    pub async fn put(&self, record: MemoryRecord) -> crate::Result<()> {
        if record.scope != self.scope {
            return Err(crate::Error::Other(
                "memory record scope must match the bound Memory scope".into(),
            ));
        }
        self.store.put(&self.agent_id, record).await
    }

    pub async fn delete(&self, record_id: &str) -> crate::Result<()> {
        if self.get(record_id).await?.is_some() {
            self.store.delete(&self.agent_id, record_id).await?;
        }
        Ok(())
    }
}

pub(crate) fn parse_extracted_memories(
    output: &str,
    session: &SessionData,
    scope: &MemoryScope,
) -> Vec<MemoryRecord> {
    let cleaned = output
        .trim()
        .strip_prefix("```json")
        .or_else(|| output.trim().strip_prefix("```"))
        .unwrap_or(output.trim())
        .strip_suffix("```")
        .unwrap_or(output.trim())
        .trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(cleaned) else {
        return Vec::new();
    };
    let Some(drafts) = value.get("memories").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    drafts
        .iter()
        .take(10)
        .filter_map(|draft| {
            let name = draft.get("name")?.as_str()?.trim();
            let content = draft.get("content")?.as_str()?.trim();
            if name.is_empty() || content.is_empty() {
                return None;
            }
            let kind = match draft.get("kind")?.as_str()? {
                "user" => MemoryKind::User,
                "feedback" => MemoryKind::Feedback,
                "project" => MemoryKind::Project,
                "reference" => MemoryKind::Reference,
                _ => return None,
            };
            let confidence = draft
                .get("confidence")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let strings = |field: &str| {
                draft
                    .get(field)
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            Some(MemoryRecord {
                record_id: format!(
                    "{}:{}:{}:{name}",
                    scope.tenant_id,
                    scope.namespace,
                    kind.label()
                ),
                scope: scope.clone(),
                name: name.to_string(),
                kind,
                content: content.to_string(),
                description: draft
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                provenance: MemoryProvenance {
                    session_id: Some(session.session_id.clone()),
                    author: MemoryAuthor::Extraction,
                    trust: MemoryTrustLevel::Untrusted,
                    evidence_refs: strings("evidence_refs"),
                },
                created_at: session.updated_at_ms,
                updated_at: session.updated_at_ms,
                last_recalled_at: None,
                recall_count: 0,
                confidence,
                links: strings("links"),
                pinned: draft
                    .get("pinned")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                ttl_days: draft
                    .get("ttl_days")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|days| u32::try_from(days).ok())
                    .filter(|days| *days > 0),
            })
        })
        .collect()
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

/// `InMemoryMemoryStore` — a lightweight `MemoryStore` backed by per-agent in-memory maps.
///
/// Rust port of node/src/memory/in-memory-store.ts. Use for benchmarks, unit tests, and local
/// development where persistent memory isn't needed. `search()` is a deterministic reference
/// ranker: distinct lexical overlap first, metadata recency second, insertion order last.
pub struct InMemoryMemoryStore {
    memories: std::sync::Mutex<std::collections::HashMap<String, Vec<MemoryRecord>>>,
    initial_memories: Vec<MemoryRecord>,
    saved_sessions: std::sync::Mutex<Vec<SessionData>>,
}

impl InMemoryMemoryStore {
    pub fn new() -> Self {
        Self::with_initial_memories(Vec::new())
    }

    pub fn with_initial_memories(initial: Vec<MemoryRecord>) -> Self {
        Self {
            memories: std::sync::Mutex::new(std::collections::HashMap::new()),
            initial_memories: initial,
            saved_sessions: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn saved_sessions(&self) -> Vec<SessionData> {
        self.saved_sessions.lock().unwrap().clone()
    }
}

impl Default for InMemoryMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryStore for InMemoryMemoryStore {
    async fn put(&self, agent_id: &str, incoming: MemoryRecord) -> crate::Result<()> {
        let mut memories = self.memories.lock().unwrap();
        let kept = memories
            .entry(agent_id.to_string())
            .or_insert_with(|| self.initial_memories.clone());
        if let Some(index) = kept.iter().position(|record| {
            record.scope == incoming.scope
                && record.kind == incoming.kind
                && record.name == incoming.name
        }) {
            kept[index] = incoming;
        } else {
            kept.push(incoming);
        }
        Ok(())
    }

    async fn get(&self, agent_id: &str, record_id: &str) -> crate::Result<Option<MemoryRecord>> {
        let mut memories = self.memories.lock().unwrap();
        Ok(memories
            .entry(agent_id.to_string())
            .or_insert_with(|| self.initial_memories.clone())
            .iter()
            .find(|record| record.record_id == record_id)
            .cloned())
    }

    async fn delete(&self, agent_id: &str, record_id: &str) -> crate::Result<()> {
        let mut memories = self.memories.lock().unwrap();
        let records = memories
            .entry(agent_id.to_string())
            .or_insert_with(|| self.initial_memories.clone());
        records.retain(|record| record.record_id != record_id);
        Ok(())
    }

    async fn search(
        &self,
        agent_id: &str,
        query: &MemoryQuery,
    ) -> crate::Result<Vec<MemoryRecall>> {
        let all = {
            let mut memories = self.memories.lock().unwrap();
            memories
                .entry(agent_id.to_string())
                .or_insert_with(|| self.initial_memories.clone())
                .clone()
        };
        let query_terms = memory_terms(&query.query);
        let mut ranked = all
            .into_iter()
            .enumerate()
            .filter(|(_, record)| {
                record.scope == query.scope
                    && (query.kinds.is_empty() || query.kinds.contains(&record.kind))
                    && query
                        .min_score
                        .is_none_or(|minimum| record.confidence >= minimum)
            })
            .filter_map(|(insertion_index, record)| {
                let searchable =
                    format!("{} {} {}", record.name, record.description, record.content);
                let candidate_terms = memory_terms(&searchable);
                let lexical_matches = query_terms
                    .iter()
                    .filter(|term| candidate_terms.contains(*term))
                    .count();
                if !query_terms.is_empty() && lexical_matches == 0 {
                    return None;
                }
                Some((record, lexical_matches, insertion_index))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| right.0.updated_at.cmp(&left.0.updated_at))
                .then_with(|| left.2.cmp(&right.2))
        });
        Ok(ranked
            .into_iter()
            .take(query.top_k)
            .map(|(record, _, _)| MemoryRecall {
                score: record.confidence.clamp(0.0, 1.0),
                record,
                why: "deterministic lexical relevance with recency tie-breaking".into(),
            })
            .collect())
    }

    async fn save_session(&self, data: SessionData) -> crate::Result<()> {
        self.saved_sessions.lock().unwrap().push(data);
        Ok(())
    }
}

fn memory_terms(text: &str) -> std::collections::HashSet<String> {
    let mut terms = std::collections::HashSet::new();
    let mut segment = String::new();
    let flush = |segment: &mut String, terms: &mut std::collections::HashSet<String>| {
        if segment.is_empty() {
            return;
        }
        let lowered = segment.to_lowercase();
        terms.insert(lowered.clone());
        let characters = lowered.chars().collect::<Vec<_>>();
        if characters.iter().any(|character| is_han(*character)) {
            for pair in characters.windows(2) {
                terms.insert(pair.iter().collect());
            }
        }
        segment.clear();
    };
    for character in text.chars() {
        if character.is_alphanumeric() {
            segment.push(character);
        } else {
            flush(&mut segment, &mut terms);
        }
    }
    flush(&mut segment, &mut terms);
    terms
}

fn is_han(character: char) -> bool {
    matches!(character as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
}

#[cfg(test)]
mod ranking_tests {
    use super::{DurableMemory, InMemoryMemoryStore, MemorySearchOptions, MemoryStore};
    use deepstrike_core::mm::memory::{
        MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecord, MemoryScope,
        MemoryTrustLevel,
    };

    fn entry(text: &str, updated_at: u64) -> MemoryRecord {
        MemoryRecord {
            record_id: format!("record-{updated_at}"),
            scope: MemoryScope::new("tenant-test", "ranking"),
            name: text.into(),
            kind: MemoryKind::Project,
            content: text.into(),
            description: text.into(),
            provenance: MemoryProvenance {
                session_id: None,
                author: MemoryAuthor::Host,
                trust: MemoryTrustLevel::HostVerified,
                evidence_refs: Vec::new(),
            },
            created_at: 1,
            updated_at,
            last_recalled_at: None,
            recall_count: 0,
            confidence: 1.0,
            links: Vec::new(),
            pinned: false,
            ttl_days: None,
        }
    }

    #[tokio::test]
    async fn search_uses_query_and_never_falls_back_to_unrelated_entries() {
        let store = InMemoryMemoryStore::with_initial_memories(vec![
            entry("database migration checklist", 1),
            entry("rust scheduler fairness", 2),
            entry("newer unrelated note", 3),
        ]);

        let query = |text: &str| MemoryQuery {
            scope: MemoryScope::new("tenant-test", "ranking"),
            query: text.into(),
            top_k: 5,
            kinds: Vec::new(),
            min_score: None,
        };
        let hits = store
            .search("agent", &query("scheduler rust"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.content, "rust scheduler fairness");
        assert!(
            store
                .search("agent", &query("nonexistent"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn durable_memory_binds_crud_to_one_agent_scope() {
        let store: std::sync::Arc<dyn MemoryStore> =
            std::sync::Arc::new(InMemoryMemoryStore::new());
        let scope = MemoryScope::new("tenant-test", "public-contract");
        let memory = DurableMemory::new(store.clone(), "agent-a", scope.clone());
        let record = MemoryRecord {
            scope: scope.clone(),
            ..entry("architecture", 1)
        };

        memory.put(record.clone()).await.unwrap();
        assert_eq!(memory.namespace(), "public-contract");
        assert_eq!(
            memory.get(&record.record_id).await.unwrap(),
            Some(record.clone())
        );
        assert_eq!(
            memory
                .search("architecture", MemorySearchOptions::default())
                .await
                .unwrap(),
            vec![record.clone()]
        );
        memory.delete(&record.record_id).await.unwrap();
        assert_eq!(memory.get(&record.record_id).await.unwrap(), None);

        let foreign = MemoryRecord {
            scope: MemoryScope::new("tenant-test", "private"),
            ..entry("foreign", 2)
        };
        assert!(memory.put(foreign.clone()).await.is_err());
        store.put("agent-a", foreign.clone()).await.unwrap();
        assert_eq!(memory.get(&foreign.record_id).await.unwrap(), None);
        memory.delete(&foreign.record_id).await.unwrap();
        assert_eq!(
            store.get("agent-a", &foreign.record_id).await.unwrap(),
            Some(foreign)
        );
    }

    struct LeakyStore {
        foreign: MemoryRecord,
    }

    #[async_trait::async_trait]
    impl MemoryStore for LeakyStore {
        async fn put(&self, _agent_id: &str, _record: MemoryRecord) -> crate::Result<()> {
            Ok(())
        }

        async fn get(
            &self,
            _agent_id: &str,
            _record_id: &str,
        ) -> crate::Result<Option<MemoryRecord>> {
            Ok(None)
        }

        async fn delete(&self, _agent_id: &str, _record_id: &str) -> crate::Result<()> {
            Ok(())
        }

        async fn search(
            &self,
            _agent_id: &str,
            _query: &MemoryQuery,
        ) -> crate::Result<Vec<deepstrike_core::mm::memory::MemoryRecall>> {
            Ok(vec![deepstrike_core::mm::memory::MemoryRecall {
                record: self.foreign.clone(),
                score: 1.0,
                why: "broken host store".into(),
            }])
        }

        async fn save_session(
            &self,
            _data: deepstrike_core::memory::durable::SessionData,
        ) -> crate::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn durable_memory_filters_cross_scope_host_search_results() {
        let scope = MemoryScope::new("tenant-test", "public-contract");
        let foreign = MemoryRecord {
            scope: MemoryScope::new("tenant-test", "private"),
            ..entry("foreign", 1)
        };
        let store: std::sync::Arc<dyn MemoryStore> = std::sync::Arc::new(LeakyStore { foreign });
        let memory = DurableMemory::new(store, "agent-a", scope);

        assert!(
            memory
                .search("private note", MemorySearchOptions::default())
                .await
                .unwrap()
                .is_empty()
        );
    }
}
