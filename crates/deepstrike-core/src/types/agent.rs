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
    /// Session ID of the parent agent that spawned this one.
    /// `None` for top-level agents; set for any sub-agent to enable lineage replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<CompactString>,
}

impl AgentIdentity {
    pub fn new(agent_id: impl Into<CompactString>, session_id: impl Into<CompactString>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            is_sub_agent: false,
            parent_session_id: None,
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
            parent_session_id: None,
        }
    }

    pub fn with_parent(mut self, parent_session_id: impl Into<CompactString>) -> Self {
        self.parent_session_id = Some(parent_session_id.into());
        self
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

/// Context a sub-agent inherits from its parent at spawn time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextInheritance {
    /// Sub-agent starts with a clean slate (no parent context).
    #[default]
    None,
    /// Sub-agent receives only the system prompt from the parent.
    SystemOnly,
    /// Sub-agent inherits the full conversation history from the parent.
    Full,
}

/// Auto-generated isolation contract for a spawned sub-agent.
/// Derived from `AgentRunSpec` + the current capability snapshot at spawn time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsolationManifest {
    pub agent_id: CompactString,
    pub parent_session_id: CompactString,
    pub role: AgentRole,
    pub isolation: AgentIsolation,
    pub context_inheritance: ContextInheritance,
    /// Capability IDs visible to the sub-agent after applying the capability filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permitted_capability_ids: Vec<CompactString>,
}

impl IsolationManifest {
    /// Build an isolation manifest from a spawn spec and the parent's live capability snapshot.
    pub fn from_spec(
        spec: &AgentRunSpec,
        parent_session_id: &str,
        available: &CapabilityManifest,
    ) -> Self {
        let context_inheritance = Self::role_default_context_inheritance(spec.role);
        let filtered = spec.filter_manifest(available);
        let permitted_capability_ids = filtered
            .capabilities()
            .iter()
            .map(|c| c.id.clone())
            .collect();
        Self {
            agent_id: spec.identity.agent_id.clone(),
            parent_session_id: parent_session_id.into(),
            role: spec.role,
            isolation: spec.isolation,
            context_inheritance,
            permitted_capability_ids,
        }
    }

    fn role_default_context_inheritance(role: AgentRole) -> ContextInheritance {
        match role {
            AgentRole::Explore | AgentRole::Verify => ContextInheritance::SystemOnly,
            AgentRole::Plan | AgentRole::Implement => ContextInheritance::Full,
            AgentRole::Custom => ContextInheritance::None,
        }
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
    /// ③ loop-agent rounds: presence turns this run into ONE round of a paced loop —
    /// it gates exposure of the `pace` meta-tool and arms the pacing trap. Additive ABI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_round: Option<LoopRoundSpec>,
}

/// Round/pacing bounds for a loop-agent run (all optional; the kernel clamps and
/// coerces the model's `pace` proposals against them at the syscall trap).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoopRoundSpec {
    /// Hard round cap across the loop's lifetime (seeded via `seed_group_rounds`);
    /// a continue/sleep proposal at the cap is coerced to stop("max_rounds").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    /// Sleep clamp floor (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_sleep_ms: Option<u64>,
    /// Sleep clamp ceiling (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sleep_ms: Option<u64>,
    /// Fallback when the round finishes without a `pace` call: "stop" (goal loops,
    /// the default) or "sleep" (cron loops — sleeps `min_sleep_ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_action: Option<String>,
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
            loop_round: None,
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
        assert_eq!(
            filtered.by_kind(CapabilityKind::Skill)[0].id.as_str(),
            "verify"
        );
    }

    #[test]
    fn verify_agent_can_reference_contract() {
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("verify", "session"),
            AgentRole::Verify,
            "check work",
        )
        .with_verification_contract("contract-1");

        assert_eq!(
            spec.verification_contract_id.unwrap().as_str(),
            "contract-1"
        );
    }
}
