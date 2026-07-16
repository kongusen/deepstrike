//! Pure record-level curation helpers.
//!
//! This module performs no storage mutation and owns no lifecycle state. Durable writes always
//! travel through the kernel `WriteMemory` syscall; hosts may use these helpers inside their
//! upsert implementation to resolve a candidate against an already-loaded snapshot.

use crate::mm::memory::MemoryRecord;

/// Conflict rule used by a host while resolving a fuzzy duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictResolution {
    PreferNewer,
    PreferHigherConfidence,
}

/// Whether two records address the same durable key.
pub fn same_scoped_key(left: &MemoryRecord, right: &MemoryRecord) -> bool {
    left.scope == right.scope && left.kind == right.kind && left.name == right.name
}

/// Deterministic term-set Jaccard score used only after scoped-key lookup misses.
///
/// Tokenizes through the shared [`crate::lexical`] vocabulary (word runs + Han
/// bigrams) rather than whitespace, so near-duplicate CJK content — which has no
/// whitespace to split on — is actually detectable against a dedupe threshold.
pub fn jaccard_similarity(left: &str, right: &str) -> f64 {
    crate::lexical::jaccard(left, right)
}

/// Select the record retained for a conflict without performing any I/O.
pub fn resolve_conflict<'a>(
    existing: &'a MemoryRecord,
    incoming: &'a MemoryRecord,
    policy: ConflictResolution,
) -> &'a MemoryRecord {
    match policy {
        ConflictResolution::PreferNewer => incoming,
        ConflictResolution::PreferHigherConfidence => {
            if incoming.confidence >= existing.confidence {
                incoming
            } else {
                existing
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::memory::{
        MemoryAuthor, MemoryKind, MemoryProvenance, MemoryScope, MemoryTrustLevel,
    };

    fn record(name: &str, content: &str, confidence: f64) -> MemoryRecord {
        MemoryRecord {
            record_id: format!("record-{name}"),
            scope: MemoryScope::new("tenant", "project"),
            name: name.into(),
            kind: MemoryKind::Project,
            content: content.into(),
            description: String::new(),
            provenance: MemoryProvenance {
                session_id: Some("session".into()),
                author: MemoryAuthor::Extraction,
                trust: MemoryTrustLevel::Untrusted,
                evidence_refs: Vec::new(),
            },
            created_at: 1,
            updated_at: 1,
            last_recalled_at: None,
            recall_count: 0,
            confidence,
            links: Vec::new(),
            pinned: false,
            ttl_days: None,
        }
    }

    #[test]
    fn scoped_key_precedes_content_similarity() {
        let existing = record("compiler", "prefer cargo nextest", 0.8);
        let same_key = record("compiler", "use cargo test", 0.7);
        let other_key = record("editor", "prefer cargo nextest", 0.9);
        assert!(same_scoped_key(&existing, &same_key));
        assert!(!same_scoped_key(&existing, &other_key));
        assert_eq!(
            jaccard_similarity(&existing.content, &other_key.content),
            1.0
        );
    }

    #[test]
    fn cjk_near_duplicates_are_detectable_after_key_miss() {
        // Whitespace tokenization turned a whole Chinese sentence into one token, so
        // near-duplicates scored ~0 and no dedupe threshold could ever fire on them.
        let near = jaccard_similarity("用户偏好深色模式界面", "用户偏好浅色模式界面");
        let unrelated = jaccard_similarity("用户偏好深色模式界面", "周五之前完成部署上线");
        assert!(near > 0.5, "near-duplicate CJK must be detectable: {near}");
        assert!(unrelated < 0.2, "unrelated CJK must stay low: {unrelated}");
    }

    #[test]
    fn conflict_resolution_is_pure_and_deterministic() {
        let old = record("compiler", "old", 0.9);
        let new = record("compiler", "new", 0.7);
        assert_eq!(
            resolve_conflict(&old, &new, ConflictResolution::PreferNewer).content,
            "new"
        );
        assert_eq!(
            resolve_conflict(&old, &new, ConflictResolution::PreferHigherConfidence).content,
            "old"
        );
    }
}
