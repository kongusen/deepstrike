use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Named context partition used by section planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSectionPartition {
    System,
    Skill,
    Memory,
    Working,
    History,
}

/// Cache behavior for a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionCachePolicy {
    /// Stable across runs unless the application changes it.
    Static,
    /// Stable within a session until an invalidation event occurs.
    SessionCached,
    /// Recomputed on every turn.
    TurnDynamic,
}

/// Events that can invalidate cached context sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionInvalidation {
    Never,
    OnCompact,
    OnSkillChange,
    OnCapabilityChange,
    OnMemoryRefresh,
    EveryTurn,
}

/// One context section declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSection {
    pub id: CompactString,
    pub partition: ContextSectionPartition,
    /// Higher priority sections are rendered earlier.
    pub priority: i16,
    pub cache_policy: SectionCachePolicy,
    pub invalidation: SectionInvalidation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u32>,
    #[serde(default)]
    pub enabled: bool,
    /// Pinned sections are exempt from GC/compression even under token pressure.
    #[serde(default)]
    pub is_pinned: bool,
}

impl ContextSection {
    pub fn new(
        id: impl Into<CompactString>,
        partition: ContextSectionPartition,
        priority: i16,
    ) -> Self {
        Self {
            id: id.into(),
            partition,
            priority,
            cache_policy: SectionCachePolicy::TurnDynamic,
            invalidation: SectionInvalidation::EveryTurn,
            token_budget: None,
            enabled: true,
            is_pinned: false,
        }
    }

    pub fn pinned(mut self) -> Self {
        self.is_pinned = true;
        self
    }

    pub fn with_cache_policy(mut self, policy: SectionCachePolicy) -> Self {
        self.cache_policy = policy;
        self
    }

    pub fn with_invalidation(mut self, invalidation: SectionInvalidation) -> Self {
        self.invalidation = invalidation;
        self
    }

    pub fn with_token_budget(mut self, token_budget: u32) -> Self {
        self.token_budget = Some(token_budget);
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// Deterministic section plan produced by the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSectionPlan {
    pub ids: Vec<CompactString>,
}

/// Registry for prompt/context sections and their lifecycle policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextSectionRegistry {
    sections: BTreeMap<CompactString, ContextSection>,
}

impl ContextSectionRegistry {
    pub fn new() -> Self {
        Self {
            sections: BTreeMap::new(),
        }
    }

    /// Baseline sections that match DeepStrike's current 5-partition context.
    pub fn default_agent_sections() -> Self {
        let mut registry = Self::new();
        registry.upsert(
            ContextSection::new("system.base", ContextSectionPartition::System, 100)
                .with_cache_policy(SectionCachePolicy::Static)
                .with_invalidation(SectionInvalidation::Never),
        );
        registry.upsert(
            ContextSection::new("system.task_state", ContextSectionPartition::System, 90)
                .with_cache_policy(SectionCachePolicy::TurnDynamic)
                .with_invalidation(SectionInvalidation::EveryTurn),
        );
        registry.upsert(
            ContextSection::new(
                "capabilities.inventory",
                ContextSectionPartition::System,
                80,
            )
            .with_cache_policy(SectionCachePolicy::SessionCached)
            .with_invalidation(SectionInvalidation::OnCapabilityChange),
        );
        registry.upsert(
            ContextSection::new("skill.active", ContextSectionPartition::Skill, 70)
                .with_cache_policy(SectionCachePolicy::SessionCached)
                .with_invalidation(SectionInvalidation::OnSkillChange),
        );
        registry.upsert(
            ContextSection::new("memory.retrieved", ContextSectionPartition::Memory, 60)
                .with_cache_policy(SectionCachePolicy::TurnDynamic)
                .with_invalidation(SectionInvalidation::OnMemoryRefresh),
        );
        registry.upsert(
            ContextSection::new("working.signals", ContextSectionPartition::Working, 50)
                .with_cache_policy(SectionCachePolicy::TurnDynamic)
                .with_invalidation(SectionInvalidation::EveryTurn),
        );
        registry.upsert(
            ContextSection::new("history.rolling", ContextSectionPartition::History, 10)
                .with_cache_policy(SectionCachePolicy::TurnDynamic)
                .with_invalidation(SectionInvalidation::OnCompact),
        );
        registry
    }

    pub fn upsert(&mut self, section: ContextSection) {
        self.sections.insert(section.id.clone(), section);
    }

    pub fn get(&self, id: &str) -> Option<&ContextSection> {
        self.sections.get(id)
    }

    pub fn len(&self) -> usize {
        self.sections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    pub fn sections(&self) -> Vec<&ContextSection> {
        let mut sections = self.sections.values().collect::<Vec<_>>();
        sections.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
        sections
    }

    pub fn plan(&self) -> ContextSectionPlan {
        ContextSectionPlan {
            ids: self
                .sections()
                .into_iter()
                .filter(|s| s.enabled)
                .map(|s| s.id.clone())
                .collect(),
        }
    }

    /// Pin a section so its partition is exempt from GC compression.
    /// Returns true if the section was found.
    pub fn pin(&mut self, id: &str) -> bool {
        if let Some(section) = self.sections.get_mut(id) {
            section.is_pinned = true;
            true
        } else {
            false
        }
    }

    /// Unpin a section, allowing its partition to be compressed again.
    /// Returns true if the section was found.
    pub fn unpin(&mut self, id: &str) -> bool {
        if let Some(section) = self.sections.get_mut(id) {
            section.is_pinned = false;
            true
        } else {
            false
        }
    }

    /// Returns true if any enabled section mapped to `partition` is pinned.
    pub fn is_partition_pinned(&self, partition: ContextSectionPartition) -> bool {
        self.sections
            .values()
            .any(|s| s.partition == partition && s.is_pinned)
    }

    /// Mark sections invalidated by an event as disabled and return their ids.
    pub fn invalidate(&mut self, event: SectionInvalidation) -> Vec<CompactString> {
        let mut invalidated = Vec::new();
        for section in self.sections.values_mut() {
            let matches_event = section.invalidation == event
                || section.invalidation == SectionInvalidation::EveryTurn
                || event == SectionInvalidation::EveryTurn;
            if matches_event && section.invalidation != SectionInvalidation::Never {
                section.enabled = false;
                invalidated.push(section.id.clone());
            }
        }
        invalidated.sort();
        invalidated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sections_include_capability_inventory() {
        let registry = ContextSectionRegistry::default_agent_sections();

        assert!(registry.get("capabilities.inventory").is_some());
        assert!(registry.get("system.base").is_some());
    }

    #[test]
    fn plan_orders_by_priority_then_id() {
        let mut registry = ContextSectionRegistry::new();
        registry.upsert(ContextSection::new(
            "b",
            ContextSectionPartition::System,
            10,
        ));
        registry.upsert(ContextSection::new(
            "a",
            ContextSectionPartition::System,
            10,
        ));
        registry.upsert(ContextSection::new(
            "top",
            ContextSectionPartition::System,
            20,
        ));

        let ids = registry
            .plan()
            .ids
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>();

        assert_eq!(ids, ["top", "a", "b"]);
    }

    #[test]
    fn upsert_replaces_section() {
        let mut registry = ContextSectionRegistry::new();
        registry.upsert(ContextSection::new(
            "same",
            ContextSectionPartition::System,
            1,
        ));
        registry.upsert(ContextSection::new(
            "same",
            ContextSectionPartition::Memory,
            2,
        ));

        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.get("same").unwrap().partition,
            ContextSectionPartition::Memory
        );
    }

    #[test]
    fn pin_marks_section_and_is_detected_by_partition() {
        let mut registry = ContextSectionRegistry::default_agent_sections();

        assert!(!registry.is_partition_pinned(ContextSectionPartition::History));
        let found = registry.pin("history.rolling");
        assert!(found);
        assert!(registry.is_partition_pinned(ContextSectionPartition::History));
        // System partition unaffected
        assert!(!registry.is_partition_pinned(ContextSectionPartition::System));
    }

    #[test]
    fn unpin_restores_compressibility() {
        let mut registry = ContextSectionRegistry::default_agent_sections();
        registry.pin("history.rolling");
        assert!(registry.is_partition_pinned(ContextSectionPartition::History));
        let found = registry.unpin("history.rolling");
        assert!(found);
        assert!(!registry.is_partition_pinned(ContextSectionPartition::History));
    }

    #[test]
    fn pin_returns_false_for_unknown_section() {
        let mut registry = ContextSectionRegistry::new();
        assert!(!registry.pin("nonexistent"));
        assert!(!registry.unpin("nonexistent"));
    }

    #[test]
    fn pinned_builder_sets_flag() {
        let section = ContextSection::new("h", ContextSectionPartition::History, 10).pinned();
        assert!(section.is_pinned);
    }

    #[test]
    fn invalidation_disables_matching_sections() {
        let mut registry = ContextSectionRegistry::new();
        registry.upsert(
            ContextSection::new("cap", ContextSectionPartition::System, 1)
                .with_invalidation(SectionInvalidation::OnCapabilityChange),
        );
        registry.upsert(
            ContextSection::new("static", ContextSectionPartition::System, 2)
                .with_invalidation(SectionInvalidation::Never),
        );

        let invalidated = registry.invalidate(SectionInvalidation::OnCapabilityChange);

        assert_eq!(invalidated, [CompactString::new("cap")]);
        assert!(!registry.get("cap").unwrap().enabled);
        assert!(registry.get("static").unwrap().enabled);
    }
}
