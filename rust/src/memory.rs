use async_trait::async_trait;
use deepstrike_core::memory::durable::SessionData;
use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord,
    MemoryScope, MemoryTrustLevel,
};

/// Durable-memory host storage. `upsert` is the only mutation and is called only after the
/// kernel's `WriteMemory` gate accepts the record.
#[async_trait]
pub trait DreamStore: Send + Sync {
    async fn upsert(&self, agent_id: &str, record: MemoryRecord) -> crate::Result<()>;

    /// Semantic search over the agent's long-term memories.
    /// Called on demand during a session when the LLM invokes the `memory` meta-tool.
    async fn search(
        &self,
        agent_id: &str,
        query: &MemoryQuery,
    ) -> crate::Result<Vec<MemoryRecall>>;

    /// Persist a completed session before the runner's one extraction pass.
    async fn save_session(
        &self,
        data: deepstrike_core::memory::durable::SessionData,
    ) -> crate::Result<()>;
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
                record_id: format!("{}:{}:{}:{name}", scope.tenant_id, scope.namespace, kind.label()),
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
                pinned: draft.get("pinned").and_then(serde_json::Value::as_bool).unwrap_or(false),
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

/// `InMemoryDreamStore` — a lightweight `DreamStore` backed by per-agent in-memory maps.
///
/// Rust port of node/src/memory/in-memory-store.ts. Use for benchmarks, unit tests, and local
/// development where persistent memory isn't needed. `search()` is a deterministic reference
/// ranker: distinct lexical overlap first, metadata recency second, insertion order last.
pub struct InMemoryDreamStore {
    memories: std::sync::Mutex<std::collections::HashMap<String, Vec<MemoryRecord>>>,
    initial_memories: Vec<MemoryRecord>,
    saved_sessions: std::sync::Mutex<Vec<SessionData>>,
}

impl InMemoryDreamStore {
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

impl Default for InMemoryDreamStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DreamStore for InMemoryDreamStore {
    async fn upsert(&self, agent_id: &str, incoming: MemoryRecord) -> crate::Result<()> {
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
                    && query.min_score.is_none_or(|minimum| record.confidence >= minimum)
            })
            .filter_map(|(insertion_index, record)| {
                let searchable = format!("{} {} {}", record.name, record.description, record.content);
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
    use super::{DreamStore, InMemoryDreamStore};
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
        let store = InMemoryDreamStore::with_initial_memories(vec![
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
        let hits = store.search("agent", &query("scheduler rust")).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.content, "rust scheduler fairness");
        assert!(store
            .search("agent", &query("nonexistent"))
            .await
            .unwrap()
            .is_empty());
    }
}
