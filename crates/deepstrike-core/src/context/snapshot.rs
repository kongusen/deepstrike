use super::sections::{ContextSectionRegistry, SectionCachePolicy};
use crate::types::capability::CapabilityManifest;
use serde::{Deserialize, Serialize};

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

fn stable_hash(bytes: &[u8]) -> u64 {
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
