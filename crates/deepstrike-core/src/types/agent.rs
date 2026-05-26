use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use super::capability::{CapabilityDescriptor, CapabilityKind, CapabilityManifest};
use super::milestone::MilestoneContract;

/// Unified agent identity — shared across scheduler, memory, and governance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent_id: CompactString,
    pub session_id: CompactString,
    pub is_sub_agent: bool,
}

impl AgentIdentity {
    pub fn new(agent_id: impl Into<CompactString>, session_id: impl Into<CompactString>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            is_sub_agent: false,
        }
    }

    pub fn sub_agent(
        agent_id: impl Into<CompactString>,
        session_id: impl Into<CompactString>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            is_sub_agent: true,
        }
    }
}

/// Agent role expressed as a runtime contract rather than a prompt convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Explore,
    Plan,
    Implement,
    Verify,
    Custom,
}

/// Isolation mode requested for an agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentIsolation {
    Shared,
    ReadOnly,
    Worktree,
    Remote,
}

/// Capability filter attached to an `AgentRunSpec`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCapabilityFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_kinds: Vec<CapabilityKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_ids: Vec<CompactString>,
}

impl AgentCapabilityFilter {
    pub fn allows(&self, capability: &CapabilityDescriptor) -> bool {
        let kind_allowed =
            self.allowed_kinds.is_empty() || self.allowed_kinds.contains(&capability.kind);
        let id_allowed = self.allowed_ids.is_empty() || self.allowed_ids.contains(&capability.id);
        kind_allowed && id_allowed
    }
}

/// First-class contract for spawning a role-isolated agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunSpec {
    pub identity: AgentIdentity,
    pub role: AgentRole,
    pub isolation: AgentIsolation,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_contract_id: Option<CompactString>,
    #[serde(default)]
    pub capability_filter: AgentCapabilityFilter,
    /// Optional milestone contract defining phase-gated execution.
    /// When set, the kernel evaluates each phase's criteria before advancing
    /// and mounts the phase's `unlocks` capabilities on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestones: Option<MilestoneContract>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

impl AgentRunSpec {
    pub fn new(identity: AgentIdentity, role: AgentRole, goal: impl Into<String>) -> Self {
        Self {
            identity,
            role,
            isolation: AgentIsolation::Shared,
            goal: goal.into(),
            verification_contract_id: None,
            capability_filter: AgentCapabilityFilter::default(),
            milestones: None,
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_milestones(mut self, contract: MilestoneContract) -> Self {
        self.milestones = Some(contract);
        self
    }

    pub fn with_isolation(mut self, isolation: AgentIsolation) -> Self {
        self.isolation = isolation;
        self
    }

    pub fn with_verification_contract(mut self, id: impl Into<CompactString>) -> Self {
        self.verification_contract_id = Some(id.into());
        self
    }

    pub fn with_capability_filter(mut self, filter: AgentCapabilityFilter) -> Self {
        self.capability_filter = filter;
        self
    }

    pub fn filter_manifest(&self, manifest: &CapabilityManifest) -> CapabilityManifest {
        manifest.filtered(|capability| self.capability_filter.allows(capability))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::skill::SkillMetadata;

    #[test]
    fn agent_filter_limits_manifest_by_kind() {
        let mut manifest = CapabilityManifest::new();
        manifest.add_marker(CapabilityKind::Tool, "write_file", "write files");
        manifest.add_skill(SkillMetadata::new("verify", "verify output"));

        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("explore", "session"),
            AgentRole::Explore,
            "inspect only",
        )
        .with_capability_filter(AgentCapabilityFilter {
            allowed_kinds: vec![CapabilityKind::Skill],
            allowed_ids: vec![],
        });

        let filtered = spec.filter_manifest(&manifest);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered.by_kind(CapabilityKind::Skill)[0].id.as_str(), "verify");
    }

    #[test]
    fn verify_agent_can_reference_contract() {
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("verify", "session"),
            AgentRole::Verify,
            "check work",
        )
        .with_verification_contract("contract-1");

        assert_eq!(spec.verification_contract_id.unwrap().as_str(), "contract-1");
    }
}
