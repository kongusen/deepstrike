use super::sections::{ContextSectionRegistry, SectionCachePolicy};
use crate::types::capability::CapabilityManifest;
use crate::types::message::Message;
use crate::context::task_state::TaskState;
use serde::{Deserialize, Serialize};

/// A single page of context memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPage {
    pub id: String,
    pub content: String,
    pub token_count: u32,
}

/// Frozen snapshot of all active context partitions at a given turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub turn: u32,
    pub system_messages: Vec<Message>,
    pub knowledge_messages: Vec<Message>,
    pub history_messages: Vec<Message>,
    pub task_state: TaskState,
}

/// Reference to an archived context segment in external storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextArchiveRef {
    pub seq: u64,
    pub archive_ref: String,
    pub summary: String,
    pub token_count: u32,
}

/// Garbage collection and pressure policy ratios.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextGcPolicy {
    pub target_tokens_ratio: f64,
    pub auto_compact_ratio: f64,
}

/// Errors/Faults that can occur during Context VM execution or replay.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum ContextFault {
    #[error("Prompt exceeds maximum token limit: budget={budget}, actual={actual}")]
    PromptTooLong { budget: u32, actual: u32 },
    #[error("Missing archive chunk {seq} for session {session_id}")]
    MissingArchive { session_id: String, seq: u64 },
    #[error("Invalid replay at turn {turn}: {reason}")]
    InvalidReplay { turn: u32, reason: String },
}


/// Provider-cache hint derived from pure kernel metadata.
///
/// SDKs can use these stable fingerprints to decide whether provider prompt
/// cache boundaries are still reusable. The kernel intentionally emits only
/// hashes and section ids; concrete cache reads/writes remain in the SDK layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshotHint {
    pub static_prefix_hash: u64,
    pub section_plan_hash: u64,
    pub capability_manifest_hash: u64,
    pub static_section_ids: Vec<String>,
}

impl ContextSnapshotHint {
    pub fn from_parts(
        sections: &ContextSectionRegistry,
        capabilities: &CapabilityManifest,
    ) -> Self {
        let plan = sections.plan();
        let mut section_plan_material = String::new();
        for id in &plan.ids {
            section_plan_material.push_str(id.as_str());
            section_plan_material.push('\n');
        }

        let mut static_section_ids = Vec::new();
        let mut static_prefix_material = String::new();
        for section in sections.sections() {
            if !section.enabled || section.cache_policy != SectionCachePolicy::Static {
                continue;
            }
            static_section_ids.push(section.id.to_string());
            static_prefix_material.push_str(section.id.as_str());
            static_prefix_material.push('|');
            static_prefix_material.push_str(&section.priority.to_string());
            static_prefix_material.push('|');
            static_prefix_material.push_str(&format!("{:?}", section.partition));
            static_prefix_material.push('\n');
        }

        Self {
            static_prefix_hash: stable_hash(static_prefix_material.as_bytes()),
            section_plan_hash: stable_hash(section_plan_material.as_bytes()),
            capability_manifest_hash: stable_hash(capabilities.format_inventory().as_bytes()),
            static_section_ids,
        }
    }
}

/// FNV-1a 64-bit. The kernel's one stable, dependency-free content hash — shared
/// by snapshot hints and the render-layer [`super::renderer::PrefixFingerprint`] so
/// both speak the same fingerprint dialect.
pub(crate) fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::sections::{
        ContextSection, ContextSectionPartition, ContextSectionRegistry, SectionCachePolicy,
    };
    use crate::types::capability::{CapabilityKind, CapabilityManifest};

    #[test]
    fn dynamic_sections_do_not_change_static_prefix_hash() {
        let mut base = ContextSectionRegistry::new();
        base.upsert(
            ContextSection::new("system.base", ContextSectionPartition::System, 100)
                .with_cache_policy(SectionCachePolicy::Static),
        );

        let mut changed = base.clone();
        changed.upsert(ContextSection::new(
            "history.rolling",
            ContextSectionPartition::History,
            10,
        ));

        let manifest = CapabilityManifest::new();
        let base_hint = ContextSnapshotHint::from_parts(&base, &manifest);
        let changed_hint = ContextSnapshotHint::from_parts(&changed, &manifest);

        assert_eq!(
            base_hint.static_prefix_hash,
            changed_hint.static_prefix_hash
        );
        assert_ne!(base_hint.section_plan_hash, changed_hint.section_plan_hash);
    }

    #[test]
    fn capability_changes_alter_manifest_hash() {
        let sections = ContextSectionRegistry::default_agent_sections();
        let mut before = CapabilityManifest::new();
        let mut after = CapabilityManifest::new();
        after.add_marker(CapabilityKind::Agent, "verify", "verification agent");

        assert_ne!(
            ContextSnapshotHint::from_parts(&sections, &before).capability_manifest_hash,
            ContextSnapshotHint::from_parts(&sections, &after).capability_manifest_hash
        );

        before.add_marker(CapabilityKind::Agent, "verify", "verification agent");
        assert_eq!(
            ContextSnapshotHint::from_parts(&sections, &before).capability_manifest_hash,
            ContextSnapshotHint::from_parts(&sections, &after).capability_manifest_hash
        );
    }
}
