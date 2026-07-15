//! Long-term memory management (Phase 7).
//!
//! Kernel defines memory types and validation rules; SDKs perform I/O and selection.
//! No I/O in this module — pure classification and validation logic.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Memory kind (4 types, mirroring Claude Code's taxonomy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// User profile: who they are, expertise level, role.
    User,
    /// Behavior preference: what they like/dislike, approved patterns.
    Feedback,
    /// Project context: what's happening, milestones, phases.
    Project,
    /// External pointer: where to find things (tickets, docs).
    Reference,
}

impl MemoryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }
}

/// Isolation boundary for a memory record.
///
/// Both components participate in identity so records cannot collide across tenants or
/// application-defined namespaces.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryScope {
    pub tenant_id: String,
    pub namespace: String,
}

impl MemoryScope {
    pub fn new(tenant_id: impl Into<String>, namespace: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            namespace: namespace.into(),
        }
    }
}

/// Stable logical key used for memory upserts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryKey {
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub name: String,
}

/// Principal responsible for producing a memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAuthor {
    Model,
    Host,
    Extraction,
}

/// Explicit trust classification kept separate from authorship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTrustLevel {
    Untrusted,
    UserAsserted,
    HostVerified,
}

/// Origin and evidence attached to a memory record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub author: MemoryAuthor,
    pub trust: MemoryTrustLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

/// A durable fact with stable identity, provenance, and lifecycle state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    pub record_id: String,
    pub scope: MemoryScope,
    pub name: String,
    pub kind: MemoryKind,
    pub content: String,
    pub description: String,
    pub provenance: MemoryProvenance,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recalled_at: Option<u64>,
    #[serde(default)]
    pub recall_count: u64,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_days: Option<u32>,
}

impl MemoryRecord {
    pub fn key(&self) -> MemoryKey {
        MemoryKey {
            scope: self.scope.clone(),
            kind: self.kind,
            name: self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryUpsertOutcome {
    Inserted { record_id: String },
    Updated { record_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryUpsertError {
    RecordIdConflict {
        record_id: String,
        existing_key: MemoryKey,
        incoming_key: MemoryKey,
    },
}

impl MemoryUpsertError {
    pub fn record_id(&self) -> &str {
        match self {
            Self::RecordIdConflict { record_id, .. } => record_id,
        }
    }
}

/// Pure in-kernel helper implementing scoped, identity-preserving upsert semantics.
#[derive(Debug, Clone, Default)]
pub struct MemoryRecordStore {
    records: BTreeMap<MemoryKey, MemoryRecord>,
    keys_by_id: BTreeMap<String, MemoryKey>,
}

impl MemoryRecordStore {
    pub fn upsert(
        &mut self,
        mut incoming: MemoryRecord,
    ) -> Result<MemoryUpsertOutcome, MemoryUpsertError> {
        let key = incoming.key();

        if let Some(existing) = self.records.get(&key) {
            if incoming.record_id != existing.record_id {
                if let Some(existing_key) = self.keys_by_id.get(&incoming.record_id) {
                    if existing_key != &key {
                        return Err(MemoryUpsertError::RecordIdConflict {
                            record_id: incoming.record_id,
                            existing_key: existing_key.clone(),
                            incoming_key: key,
                        });
                    }
                }
            }

            let stable_id = existing.record_id.clone();
            incoming.record_id = stable_id.clone();
            incoming.created_at = existing.created_at;
            incoming.updated_at = incoming.updated_at.max(existing.updated_at);
            incoming.last_recalled_at = existing.last_recalled_at;
            incoming.recall_count = existing.recall_count;

            self.records.insert(key, incoming);
            return Ok(MemoryUpsertOutcome::Updated {
                record_id: stable_id,
            });
        }

        if let Some(existing_key) = self.keys_by_id.get(&incoming.record_id) {
            return Err(MemoryUpsertError::RecordIdConflict {
                record_id: incoming.record_id,
                existing_key: existing_key.clone(),
                incoming_key: key,
            });
        }

        let record_id = incoming.record_id.clone();
        self.keys_by_id.insert(record_id.clone(), key.clone());
        self.records.insert(key, incoming);
        Ok(MemoryUpsertOutcome::Inserted { record_id })
    }

    pub fn get(&self, scope: &MemoryScope, kind: MemoryKind, name: &str) -> Option<&MemoryRecord> {
        self.records.get(&MemoryKey {
            scope: scope.clone(),
            kind,
            name: name.to_owned(),
        })
    }

    pub fn get_by_id(&self, record_id: &str) -> Option<&MemoryRecord> {
        self.keys_by_id
            .get(record_id)
            .and_then(|key| self.records.get(key))
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Commit a successful recall as part of the journaled query-result transaction.
    pub fn record_recall(&mut self, record_id: &str, recalled_at: u64) -> Option<&MemoryRecord> {
        let key = self.keys_by_id.get(record_id)?.clone();
        let record = self.records.get_mut(&key)?;
        record.recall_count = record.recall_count.saturating_add(1);
        record.last_recalled_at = Some(recalled_at);
        Some(record)
    }

    pub fn promotion_suggested(&self, record_id: &str, threshold: u64) -> bool {
        self.get_by_id(record_id)
            .is_some_and(|record| !record.pinned && record.recall_count >= threshold)
    }
}

/// Scoped recall request. The host owns retrieval; the kernel validates this deterministic wire and
/// caps `top_k` through [`MemoryPolicy`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryQuery {
    pub scope: MemoryScope,
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<MemoryKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_score: Option<f64>,
}

fn default_top_k() -> usize {
    5
}

impl Default for MemoryQuery {
    fn default() -> Self {
        Self {
            scope: MemoryScope::new(String::new(), String::new()),
            query: String::new(),
            top_k: default_top_k(),
            kinds: Vec::new(),
            min_score: None,
        }
    }
}

impl MemoryQuery {
    pub fn validate(&self) -> Result<(), String> {
        if self.scope.tenant_id.is_empty() || self.scope.namespace.is_empty() {
            return Err("memory query scope tenant_id and namespace must be non-empty".into());
        }
        if self.query.trim().is_empty() {
            return Err("memory query text must be non-empty".into());
        }
        if self.top_k == 0 {
            return Err("memory query top_k must be greater than zero".into());
        }
        if self
            .min_score
            .is_some_and(|score| !score.is_finite() || !(0.0..=1.0).contains(&score))
        {
            return Err("memory query min_score must be finite and between zero and one".into());
        }
        Ok(())
    }

    pub fn validate_hits(&self, hits: &[MemoryRecall], requested_k: usize) -> Result<(), String> {
        if hits.len() > requested_k {
            return Err(format!(
                "memory query returned {} hits but requested at most {requested_k}",
                hits.len()
            ));
        }
        let mut record_ids = std::collections::BTreeSet::new();
        for hit in hits {
            if hit.record.scope != self.scope {
                return Err(format!(
                    "memory recall {} escaped the requested scope",
                    hit.record.record_id
                ));
            }
            if hit.record.record_id.is_empty() || !record_ids.insert(hit.record.record_id.as_str())
            {
                return Err("memory recall record_id must be non-empty and unique".into());
            }
            if !hit.score.is_finite() || !(0.0..=1.0).contains(&hit.score) {
                return Err(format!(
                    "memory recall {} score must be finite and between zero and one",
                    hit.record.record_id
                ));
            }
            if self.min_score.is_some_and(|minimum| hit.score < minimum) {
                return Err(format!(
                    "memory recall {} score is below min_score",
                    hit.record.record_id
                ));
            }
            if !self.kinds.is_empty() && !self.kinds.contains(&hit.record.kind) {
                return Err(format!(
                    "memory recall {} kind was not requested",
                    hit.record.record_id
                ));
            }
        }
        Ok(())
    }
}

/// One scored host recall. `score` is relevance, distinct from the record's stored confidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecall {
    pub record: MemoryRecord,
    pub score: f64,
    pub why: String,
}

/// Memory validation error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "error_kind", rename_all = "snake_case")]
pub enum MemoryValidationError {
    MissingRequiredField { field: String },
    ContentTooLarge { size: u32, limit: u32 },
    ForbiddenPattern { pattern: String, reason: String },
    InvalidKind { kind: String },
    NameTooLong { length: usize, limit: usize },
}

/// Memory validation rules (kernel-enforced).
#[derive(Debug, Clone)]
pub struct MemoryValidation {
    pub max_size_bytes: u32,
    pub max_name_length: usize,
    pub required_fields: Vec<String>,
    pub forbidden_patterns: Vec<(String, &'static str)>,
}

impl MemoryValidation {
    /// Validate a memory write request.
    pub fn validate(&self, record: &MemoryRecord) -> Result<(), MemoryValidationError> {
        // Check required fields
        for field in &self.required_fields {
            match field.as_str() {
                "record_id" if record.record_id.is_empty() => {
                    return Err(MemoryValidationError::MissingRequiredField {
                        field: "record_id".into(),
                    });
                }
                "scope.tenant_id" if record.scope.tenant_id.is_empty() => {
                    return Err(MemoryValidationError::MissingRequiredField {
                        field: "scope.tenant_id".into(),
                    });
                }
                "scope.namespace" if record.scope.namespace.is_empty() => {
                    return Err(MemoryValidationError::MissingRequiredField {
                        field: "scope.namespace".into(),
                    });
                }
                "name" if record.name.is_empty() => {
                    return Err(MemoryValidationError::MissingRequiredField {
                        field: "name".into(),
                    });
                }
                "description" if record.description.is_empty() => {
                    return Err(MemoryValidationError::MissingRequiredField {
                        field: "description".into(),
                    });
                }
                _ => {}
            }
        }

        // Check name length
        if record.name.len() > self.max_name_length {
            return Err(MemoryValidationError::NameTooLong {
                length: record.name.len(),
                limit: self.max_name_length,
            });
        }

        // Check content size
        if record.content.len() > self.max_size_bytes as usize {
            return Err(MemoryValidationError::ContentTooLarge {
                size: record.content.len() as u32,
                limit: self.max_size_bytes,
            });
        }

        // Check forbidden patterns
        for (pattern, reason) in &self.forbidden_patterns {
            if record.content.contains(pattern) {
                return Err(MemoryValidationError::ForbiddenPattern {
                    pattern: pattern.clone(),
                    reason: reason.to_string(),
                });
            }
        }

        Ok(())
    }
}

/// Validate a memory write request with default validation rules.
pub fn validate_memory_write(record: &MemoryRecord) -> Result<(), MemoryValidationError> {
    MemoryValidation::default().validate(record)
}

/// Declarative configuration for the kernel's long-term memory subsystem.
///
/// Installed via the `set_memory_policy` input event (opt-in). When no policy is installed the
/// kernel preserves pre-policy behavior: every `write_memory` is validated with the default rules
/// and `query_memory` uses the requested `top_k` verbatim. Installing a policy makes these knobs
/// authoritative:
/// - `validation_enabled = false` admits every write without validation.
/// - `retrieval_top_k` is an upper bound: the emitted `requested_k` is `min(query.top_k, top_k)`.
/// - `max_content_bytes` / `max_name_length` override the validation size limits when set.
///
/// `memory_path` and `stale_warning_days` are not enforced inside the kernel (the kernel performs
/// no recall I/O); they are carried so the SDK consumes a single authoritative config.
#[derive(Debug, Clone)]
pub struct MemoryPolicy {
    pub memory_path: String,
    pub stale_warning_days: u32,
    pub retrieval_top_k: usize,
    pub validation_enabled: bool,
    pub max_content_bytes: Option<u32>,
    pub max_name_length: Option<usize>,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            memory_path: String::new(),
            stale_warning_days: 2,
            retrieval_top_k: 5,
            validation_enabled: true,
            max_content_bytes: None,
            max_name_length: None,
        }
    }
}

impl MemoryPolicy {
    /// Build the validation rules this policy implies, starting from the kernel defaults and
    /// applying any size / name-length overrides.
    pub fn validation(&self) -> MemoryValidation {
        let mut v = MemoryValidation::default();
        if let Some(bytes) = self.max_content_bytes {
            v.max_size_bytes = bytes;
        }
        if let Some(len) = self.max_name_length {
            v.max_name_length = len;
        }
        v
    }

    /// Clamp a requested retrieval count to this policy's `retrieval_top_k` upper bound.
    pub fn clamp_top_k(&self, requested: usize) -> usize {
        requested.min(self.retrieval_top_k)
    }
}

/// Default validation rules (aligned with Claude Code's "what NOT to store").
impl Default for MemoryValidation {
    fn default() -> Self {
        Self {
            max_size_bytes: 10_000,
            max_name_length: 100,
            required_fields: vec![
                "record_id".into(),
                "scope.tenant_id".into(),
                "scope.namespace".into(),
                "name".into(),
                "description".into(),
            ],
            // P13: no baked-in content heuristics — what belongs in memory is host/model
            // judgment. Hosts configure forbidden prefixes via MemoryPolicy when they want them.
            forbidden_patterns: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(namespace: &str) -> MemoryScope {
        MemoryScope::new("tenant-a", namespace)
    }

    fn record(record_id: &str, namespace: &str, name: &str, content: &str) -> MemoryRecord {
        MemoryRecord {
            record_id: record_id.into(),
            scope: scope(namespace),
            name: name.into(),
            kind: MemoryKind::Project,
            content: content.into(),
            description: format!("description for {name}"),
            provenance: MemoryProvenance {
                session_id: Some("session-1".into()),
                author: MemoryAuthor::Extraction,
                trust: MemoryTrustLevel::Untrusted,
                evidence_refs: vec!["turn:1".into()],
            },
            created_at: 10,
            updated_at: 10,
            last_recalled_at: None,
            recall_count: 0,
            confidence: 0.8,
            links: Vec::new(),
            pinned: false,
            ttl_days: Some(30),
        }
    }

    #[test]
    fn memory_kind_labels_correct() {
        assert_eq!(MemoryKind::User.label(), "user");
        assert_eq!(MemoryKind::Feedback.label(), "feedback");
        assert_eq!(MemoryKind::Project.label(), "project");
        assert_eq!(MemoryKind::Reference.label(), "reference");
    }

    #[test]
    fn validation_passes_for_valid_request() {
        let validation = MemoryValidation::default();
        let record = record("mem-1", "project:p1", "test-memory", "This is fine");
        assert!(validation.validate(&record).is_ok());
    }

    #[test]
    fn validation_rejects_missing_name() {
        let validation = MemoryValidation::default();
        let mut record = record("mem-1", "project:p1", "name", "content");
        record.name.clear();
        assert!(matches!(
            validation.validate(&record),
            Err(MemoryValidationError::MissingRequiredField { field }) if field == "name"
        ));
    }

    #[test]
    fn validation_rejects_host_configured_forbidden_pattern() {
        // P13: no baked-in content heuristics — the mechanism only bites when a HOST
        // configures forbidden prefixes on its MemoryPolicy.
        let mut validation = MemoryValidation::default();
        assert!(validation.forbidden_patterns.is_empty(), "no defaults");
        validation
            .forbidden_patterns
            .push(("TODO:".into(), "transient tasks do not belong in memory"));
        let record = record("mem-1", "project:p1", "bad-memory", "TODO: ship it");
        assert!(matches!(
            validation.validate(&record),
            Err(MemoryValidationError::ForbiddenPattern { .. })
        ));
    }

    #[test]
    fn validation_rejects_oversized_content() {
        let validation = MemoryValidation::default();
        let record = record("mem-1", "project:p1", "huge-memory", &"x".repeat(20_000));
        assert!(matches!(
            validation.validate(&record),
            Err(MemoryValidationError::ContentTooLarge { .. })
        ));
    }

    #[test]
    fn memory_query_defaults_top_k_to_5() {
        let query = MemoryQuery {
            scope: scope("project:p1"),
            query: "test".into(),
            ..Default::default()
        };
        assert_eq!(query.top_k, 5);
    }

    #[test]
    fn legacy_memory_wire_shapes_are_rejected() {
        assert!(
            serde_json::from_value::<MemoryQuery>(serde_json::json!({
                "current_context": "legacy",
                "top_k": 5
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<MemoryRecord>(serde_json::json!({
                "metadata": { "name": "legacy" },
                "content": "old write request"
            }))
            .is_err()
        );
    }

    #[test]
    fn memory_record_wire_shape_carries_identity_scope_provenance_and_lifecycle() {
        let value = serde_json::to_value(record("mem-1", "project:p1", "build", "use cargo"))
            .expect("record serializes");

        assert_eq!(value["record_id"], "mem-1");
        assert_eq!(value["scope"]["tenant_id"], "tenant-a");
        assert_eq!(value["scope"]["namespace"], "project:p1");
        assert_eq!(value["kind"], "project");
        assert_eq!(value["provenance"]["author"], "extraction");
        assert_eq!(value["provenance"]["trust"], "untrusted");
        assert_eq!(value["recall_count"], 0);
        assert_eq!(value["ttl_days"], 30);
    }

    #[test]
    fn scored_recall_updates_lifecycle_and_suggests_promotion() {
        let mut store = MemoryRecordStore::default();
        let record = record("recall-me", "agent-a", "preferences", "Use terse answers");
        store.upsert(record).unwrap();

        let recalled = store.record_recall("recall-me", 42).expect("record exists");
        assert_eq!(recalled.recall_count, 1);
        assert_eq!(recalled.last_recalled_at, Some(42));
        assert!(!store.promotion_suggested("recall-me", 2));

        store.record_recall("recall-me", 43).unwrap();
        assert!(store.promotion_suggested("recall-me", 2));
    }

    #[test]
    fn memory_key_is_scope_kind_and_name() {
        let project = record("mem-project", "project:p1", "build", "cargo");
        let other_scope = record("mem-project-2", "project:p2", "build", "npm");
        let mut other_kind = project.clone();
        other_kind.record_id = "mem-user".into();
        other_kind.kind = MemoryKind::User;

        assert_ne!(project.key(), other_scope.key());
        assert_ne!(project.key(), other_kind.key());
        assert_eq!(project.key().name, "build");
    }

    #[test]
    fn scoped_upsert_preserves_stable_identity_and_recall_lifecycle() {
        let mut store = MemoryRecordStore::default();
        let mut existing = record("stable-id", "project:p1", "build", "cargo build");
        existing.recall_count = 7;
        existing.last_recalled_at = Some(80);
        assert!(matches!(
            store.upsert(existing).unwrap(),
            MemoryUpsertOutcome::Inserted { .. }
        ));

        let mut replacement = record("incoming-id", "project:p1", "build", "cargo nextest");
        replacement.created_at = 90;
        replacement.updated_at = 100;
        replacement.provenance.author = MemoryAuthor::Host;
        replacement.provenance.trust = MemoryTrustLevel::HostVerified;
        let outcome = store.upsert(replacement).unwrap();

        assert_eq!(
            outcome,
            MemoryUpsertOutcome::Updated {
                record_id: "stable-id".into()
            }
        );
        let stored = store
            .get(&scope("project:p1"), MemoryKind::Project, "build")
            .unwrap();
        assert_eq!(stored.record_id, "stable-id");
        assert_eq!(stored.created_at, 10);
        assert_eq!(stored.updated_at, 100);
        assert_eq!(stored.content, "cargo nextest");
        assert_eq!(stored.recall_count, 7);
        assert_eq!(stored.last_recalled_at, Some(80));
        assert_eq!(stored.provenance.author, MemoryAuthor::Host);
        assert_eq!(stored.provenance.trust, MemoryTrustLevel::HostVerified);

        let mut stale_update = record("another-id", "project:p1", "build", "older fact");
        stale_update.updated_at = 50;
        store.upsert(stale_update).unwrap();
        assert_eq!(
            store
                .get(&scope("project:p1"), MemoryKind::Project, "build")
                .unwrap()
                .updated_at,
            100,
            "upsert cannot move the lifecycle clock backwards"
        );
    }

    #[test]
    fn same_name_in_a_different_scope_inserts_a_distinct_record() {
        let mut store = MemoryRecordStore::default();
        store
            .upsert(record("mem-p1", "project:p1", "build", "cargo"))
            .unwrap();
        store
            .upsert(record("mem-p2", "project:p2", "build", "npm"))
            .unwrap();

        assert_eq!(store.len(), 2);
        assert_eq!(
            store
                .get(&scope("project:p2"), MemoryKind::Project, "build")
                .unwrap()
                .record_id,
            "mem-p2"
        );
    }

    #[test]
    fn record_id_collision_across_keys_is_rejected() {
        let mut store = MemoryRecordStore::default();
        store
            .upsert(record("same-id", "project:p1", "build", "cargo"))
            .unwrap();

        let error = store
            .upsert(record("same-id", "project:p2", "deploy", "ship"))
            .expect_err("record id cannot alias another scoped key");

        assert!(matches!(
            error,
            MemoryUpsertError::RecordIdConflict { record_id, .. } if record_id == "same-id"
        ));
        assert_eq!(store.len(), 1);
    }
}
