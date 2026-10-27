use async_trait::async_trait;
use deepstrike_core::memory::durable::SessionData;
use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecallLifecycle,
    MemoryRecord, MemoryScope, MemoryTrustLevel,
};
use serde::{Deserialize, Serialize};

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

    /// M3 · mirror a recall lifecycle the kernel derived into durable state. The counts arrive
    /// already computed — a store never derives `recall_count + 1` itself. The default keeps no
    /// recall history, which is the behaviour of a store that predates this hook.
    async fn record_recall(
        &self,
        _agent_id: &str,
        _recalls: &[MemoryRecallLifecycle],
    ) -> crate::Result<()> {
        Ok(())
    }
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

/// Host-facing memory runtime policy (contract M1). Each capability is an independent switch: a
/// host can keep retrieval while turning off the auxiliary-model writes that run implicitly after
/// a session. `Disabled` is checked before any auxiliary provider call, so a disabled path costs
/// nothing. `CandidateSink` keeps a write path on but hands each write to the host's
/// [`MemoryCandidateSink`] (M2) instead of the memory store; a run refuses to start without one.
/// No policy (`RuntimeOptions::memory_runtime_policy = None`) keeps every path on and writing to
/// the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRuntimePolicy {
    pub retrieval: MemoryRetrievalPolicy,
    pub session_extraction: MemoryWritePolicy,
    pub model_writes: MemoryWritePolicy,
    pub semantic_archive: MemoryWritePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRetrievalPolicy {
    Disabled,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWritePolicy {
    Disabled,
    CandidateSink,
}

impl MemoryRuntimePolicy {
    /// The write paths set to `CandidateSink`, by their policy field names.
    pub fn candidate_sink_paths(&self) -> Vec<&'static str> {
        [
            ("session_extraction", self.session_extraction),
            ("model_writes", self.model_writes),
            ("semantic_archive", self.semantic_archive),
        ]
        .into_iter()
        .filter(|(_, policy)| *policy == MemoryWritePolicy::CandidateSink)
        .map(|(name, _)| name)
        .collect()
    }
}

/// Which write path produced a candidate. `ChildRequest` is the kernel's PersistMemory effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateSource {
    SessionExtraction,
    SemanticArchive,
    ChildRequest,
}

impl MemoryCandidateSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::SessionExtraction => "session_extraction",
            Self::SemanticArchive => "semantic_archive",
            Self::ChildRequest => "child_request",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCandidate {
    /// Deterministic for the write that produced it (see [`memory_candidate_id`]): a retry of the
    /// same write carries the same id, so the sink can answer it from what it already holds.
    pub candidate_id: String,
    pub source: MemoryCandidateSource,
    pub record: MemoryRecord,
}

/// The sink's answer. Its JSON form (`status` tag plus fields) is the body of the kernel's
/// `memory_candidate_result` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MemoryCandidateReceipt {
    Pending {
        candidate_id: String,
        receipt_id: String,
    },
    Accepted {
        candidate_id: String,
        receipt_id: String,
        record_ref: String,
        digest: String,
    },
    Rejected {
        candidate_id: String,
        receipt_id: String,
        reason_code: String,
    },
}

impl MemoryCandidateReceipt {
    pub fn candidate_id(&self) -> &str {
        match self {
            Self::Pending { candidate_id, .. }
            | Self::Accepted { candidate_id, .. }
            | Self::Rejected { candidate_id, .. } => candidate_id,
        }
    }

    pub fn receipt_id(&self) -> &str {
        match self {
            Self::Pending { receipt_id, .. }
            | Self::Accepted { receipt_id, .. }
            | Self::Rejected { receipt_id, .. } => receipt_id,
        }
    }
}

/// The host's candidate store. Contract (spec §2.3):
///
/// - The sink mints `receipt_id`; it is the stable name of its answer.
/// - Identity is scoped by `agent_id`; source and review metadata are part of conflict detection.
/// - The same `candidate_id` with the same payload returns the original receipt and stores
///   nothing new, so a write whose acknowledgement was lost can be redelivered safely.
/// - The same `candidate_id` with a different payload is rejected with
///   `reason_code = "candidate_conflict"` and does not overwrite what is held.
/// - An error means no answer was obtained; the caller may redeliver the same candidate.
#[async_trait]
pub trait MemoryCandidateSink: Send + Sync {
    async fn submit(
        &self,
        agent_id: &str,
        candidate: MemoryCandidate,
    ) -> crate::Result<MemoryCandidateReceipt>;
}

/// A candidate id from the write's canonical identity — never random, so a retry repeats it.
/// The parts are hashed as a compact JSON array, byte-identical to the Node/Python/WASM rule.
pub fn memory_candidate_id(parts: &[&str]) -> String {
    format!(
        "candidate:{}",
        sha256_hex(&serde_json::to_string(parts).expect("string parts serialize"))
    )
}

/// The part of a record a candidate is judged by. Store ids, timestamps and recall counters are
/// excluded: a retry of the same write may mint them afresh without changing what is proposed.
pub fn memory_candidate_payload_digest(record: &MemoryRecord) -> String {
    // JSON.stringify writes an integral number without a fraction; match it so the digest is the
    // same one the other SDKs compute.
    let confidence = if record.confidence.fract() == 0.0 && record.confidence.abs() < 1e15 {
        serde_json::json!(record.confidence as i64)
    } else {
        serde_json::json!(record.confidence)
    };
    let payload = serde_json::json!([
        record.scope.tenant_id,
        record.scope.namespace,
        record.kind,
        record.name,
        record.description,
        record.content,
        record.provenance.author,
        record.provenance.trust,
        record.provenance.session_id,
        record.provenance.evidence_refs,
        confidence,
        record.links,
        record.pinned,
        record.ttl_days,
    ]);
    format!("sha256:{}", sha256_hex(&payload.to_string()))
}

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct HeldCandidate {
    agent_id: String,
    candidate: MemoryCandidate,
    payload_digest: String,
    receipt: MemoryCandidateReceipt,
}

/// In-process sink: every candidate is held as `pending`. For tests and local development; it
/// does not persist across processes and does not authenticate where a candidate came from.
#[derive(Default)]
pub struct InMemoryMemoryCandidateSink {
    state: std::sync::Mutex<InMemoryCandidateState>,
}

#[derive(Default)]
struct InMemoryCandidateState {
    held: Vec<HeldCandidate>,
    next_receipt: u64,
}

impl InMemoryCandidateState {
    fn mint_receipt_id(&mut self) -> String {
        self.next_receipt += 1;
        format!("receipt:{}", self.next_receipt)
    }
}

impl InMemoryMemoryCandidateSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Candidates held for `agent_id` (all agents when `None`), in submission order.
    pub fn candidates(&self, agent_id: Option<&str>) -> Vec<MemoryCandidate> {
        self.state
            .lock()
            .unwrap()
            .held
            .iter()
            .filter(|held| agent_id.is_none_or(|agent_id| held.agent_id == agent_id))
            .map(|held| held.candidate.clone())
            .collect()
    }
}

#[async_trait]
impl MemoryCandidateSink for InMemoryMemoryCandidateSink {
    async fn submit(
        &self,
        agent_id: &str,
        candidate: MemoryCandidate,
    ) -> crate::Result<MemoryCandidateReceipt> {
        let payload_digest = memory_candidate_payload_digest(&candidate.record);
        let mut state = self.state.lock().unwrap();
        if let Some(existing) = state.held.iter().find(|held| {
            held.agent_id == agent_id && held.candidate.candidate_id == candidate.candidate_id
        }) {
            if existing.payload_digest == payload_digest
                && existing.candidate.source == candidate.source
            {
                return Ok(existing.receipt.clone());
            }
            return Ok(MemoryCandidateReceipt::Rejected {
                candidate_id: candidate.candidate_id,
                receipt_id: state.mint_receipt_id(),
                reason_code: "candidate_conflict".into(),
            });
        }
        let receipt = MemoryCandidateReceipt::Pending {
            candidate_id: candidate.candidate_id.clone(),
            receipt_id: state.mint_receipt_id(),
        };
        state.held.push(HeldCandidate {
            agent_id: agent_id.to_string(),
            candidate,
            payload_digest,
            receipt: receipt.clone(),
        });
        Ok(receipt)
    }
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

    async fn record_recall(
        &self,
        agent_id: &str,
        recalls: &[MemoryRecallLifecycle],
    ) -> crate::Result<()> {
        let mut memories = self.memories.lock().unwrap();
        let records = memories
            .entry(agent_id.to_string())
            .or_insert_with(|| self.initial_memories.clone());
        for recall in recalls {
            if let Some(record) = records
                .iter_mut()
                .find(|record| record.record_id == recall.record_id)
            {
                record.recall_count = recall.recall_count;
                record.last_recalled_at = Some(recall.last_recalled_at);
            }
        }
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
mod candidate_tests {
    use super::{
        InMemoryMemoryCandidateSink, MemoryCandidate, MemoryCandidateReceipt, MemoryCandidateSink,
        MemoryCandidateSource, MemoryRuntimePolicy, memory_candidate_id,
        memory_candidate_payload_digest,
    };
    use deepstrike_core::mm::memory::{
        MemoryAuthor, MemoryKind, MemoryProvenance, MemoryRecord, MemoryScope, MemoryTrustLevel,
    };

    fn record(content: &str) -> MemoryRecord {
        MemoryRecord {
            record_id: "memory:1".into(),
            scope: MemoryScope::new("tenant", "ns"),
            name: "proposal".into(),
            kind: MemoryKind::Project,
            content: content.into(),
            description: "a proposal".into(),
            provenance: MemoryProvenance {
                session_id: None,
                author: MemoryAuthor::Model,
                trust: MemoryTrustLevel::Untrusted,
                evidence_refs: Vec::new(),
            },
            created_at: 1,
            updated_at: 1,
            last_recalled_at: None,
            recall_count: 0,
            confidence: 1.0,
            links: Vec::new(),
            pinned: false,
            ttl_days: None,
        }
    }

    fn candidate(id: &str, content: &str) -> MemoryCandidate {
        MemoryCandidate {
            candidate_id: id.into(),
            source: MemoryCandidateSource::ChildRequest,
            record: record(content),
        }
    }

    /// The fixed vector the Node, Python and WASM identity tests pin.
    #[test]
    fn candidate_ids_match_the_other_sdks() {
        assert_eq!(
            memory_candidate_id(&["child_request", "op-1", "effect-3", "0"]),
            "candidate:ead40e56240c408c06eeb95c7fe297bbb3b3c187c8cf181e0227aa269bcd7043"
        );
        assert_ne!(
            memory_candidate_id(&["a", "bc"]),
            memory_candidate_id(&["ab", "c"])
        );
    }

    #[test]
    fn the_payload_digest_ignores_store_ids_timestamps_and_recall_counts() {
        let base = record("fact");
        let digest = memory_candidate_payload_digest(&base);
        assert!(digest.starts_with("sha256:") && digest.len() == 7 + 64);
        let restamped = MemoryRecord {
            record_id: "memory:2".into(),
            created_at: 9,
            updated_at: 9,
            recall_count: 4,
            last_recalled_at: Some(9),
            ..base.clone()
        };
        assert_eq!(memory_candidate_payload_digest(&restamped), digest);
        let edited = MemoryRecord {
            confidence: 0.5,
            ..base
        };
        assert_ne!(memory_candidate_payload_digest(&edited), digest);
    }

    #[tokio::test]
    async fn the_in_memory_sink_replays_and_rejects_conflicts() {
        let sink = InMemoryMemoryCandidateSink::new();
        let first = sink.submit("a", candidate("c-1", "fact")).await.unwrap();
        assert!(matches!(first, MemoryCandidateReceipt::Pending { .. }));
        assert_eq!(
            sink.submit("a", candidate("c-1", "fact")).await.unwrap(),
            first
        );
        let conflict = sink.submit("a", candidate("c-1", "other")).await.unwrap();
        match &conflict {
            MemoryCandidateReceipt::Rejected {
                reason_code,
                receipt_id,
                ..
            } => {
                assert_eq!(reason_code, "candidate_conflict");
                assert_ne!(receipt_id, first.receipt_id());
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        let resourced = MemoryCandidate {
            source: MemoryCandidateSource::SessionExtraction,
            ..candidate("c-1", "fact")
        };
        assert!(matches!(
            sink.submit("a", resourced).await.unwrap(),
            MemoryCandidateReceipt::Rejected { .. }
        ));
        // Identity is scoped by agent.
        let other_agent = sink.submit("b", candidate("c-1", "fact")).await.unwrap();
        assert_ne!(other_agent.receipt_id(), first.receipt_id());
        assert_eq!(sink.candidates(Some("a")).len(), 1);
        assert_eq!(sink.candidates(Some("a"))[0].record.content, "fact");
        assert_eq!(sink.candidates(None).len(), 2);
    }

    #[test]
    fn receipts_and_policies_use_the_shared_wire_shape() {
        let receipt = MemoryCandidateReceipt::Rejected {
            candidate_id: "c".into(),
            receipt_id: "r".into(),
            reason_code: "candidate_conflict".into(),
        };
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::json!({
                "status": "rejected",
                "candidate_id": "c",
                "receipt_id": "r",
                "reason_code": "candidate_conflict",
            })
        );
        let policy: MemoryRuntimePolicy = serde_json::from_value(serde_json::json!({
            "retrieval": "host",
            "session_extraction": "candidate_sink",
            "model_writes": "disabled",
            "semantic_archive": "candidate_sink",
        }))
        .unwrap();
        assert_eq!(
            policy.candidate_sink_paths(),
            ["session_extraction", "semantic_archive"]
        );
        assert!(
            serde_json::from_value::<MemoryRuntimePolicy>(serde_json::json!({
                "retrieval": "host",
                "session_extraction": "store",
                "model_writes": "disabled",
                "semantic_archive": "disabled",
            }))
            .is_err(),
            "a write path is disabled or candidate_sink"
        );
        assert!(
            serde_json::from_value::<MemoryRuntimePolicy>(serde_json::json!({
                "retrieval": "host",
            }))
            .is_err(),
            "every switch is required"
        );
    }
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
