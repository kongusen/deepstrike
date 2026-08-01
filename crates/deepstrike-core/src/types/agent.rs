use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use super::capability::{CapabilityDescriptor, CapabilityKind, CapabilityManifest};
use super::milestone::MilestoneContract;

/// Unified agent identity — shared across scheduler, memory, and governance.
///
/// § Task 11 · the two session fields below are **host projection only**. No kernel decision reads
/// them and no kernel output echoes them. The canonical wire has no slot for them at all
/// (`LogicalAgentSpec` carries no identity), so the canonical driver builds this struct with an
/// empty session. Hosts retain these fields only for their own persistence and audit projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent_id: CompactString,
    /// Host persistence/audit identity. Never a kernel fact (§22.6) — see the type doc.
    pub session_id: CompactString,
    pub is_sub_agent: bool,
    /// Session ID of the parent agent that spawned this one — host lineage only, see the type doc.
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

    /// The **kind axis alone** — the id allow-list is ignored.
    ///
    /// Used only for kernel-owned meta surfaces (`EXPOSURE_EXEMPT_META_TOOLS`) when the scheduler
    /// filters the exposed toolset: an id profile enumerates *task* tools, so it must not delete
    /// the model's route back to kernel state, while an explicit kind restriction (sub-agent
    /// isolation admitting only [`CapabilityKind::Tool`]) is a deliberate statement about
    /// capability families and still applies.
    ///
    /// Deliberately NOT routed through by [`Self::allows`], which stays the full two-axis contract
    /// every other consumer (notably `IsolationManifest::from_spec`) depends on.
    pub fn allows_kind(&self, kind: CapabilityKind) -> bool {
        self.allowed_kinds.is_empty() || self.allowed_kinds.contains(&kind)
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
///
/// § Task 11 · purely logical. Isolation is decided from role, requested isolation mode and the
/// capability filter — never from who the host says the parent session is (§22.6). The manifest
/// therefore has no session field to stamp onto the child's TCB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsolationManifest {
    pub agent_id: CompactString,
    pub role: AgentRole,
    pub isolation: AgentIsolation,
    pub context_inheritance: ContextInheritance,
    /// Capability IDs visible to the sub-agent after applying the capability filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permitted_capability_ids: Vec<CompactString>,
}

impl IsolationManifest {
    /// Build an isolation manifest from a spawn spec and the parent's live capability snapshot.
    pub fn from_spec(spec: &AgentRunSpec, available: &CapabilityManifest) -> Self {
        let context_inheritance = Self::role_default_context_inheritance(spec.role);
        let filtered = spec.filter_manifest(available);
        let permitted_capability_ids = filtered
            .capabilities()
            .iter()
            .map(|c| c.id.clone())
            .collect();
        Self {
            agent_id: spec.identity.agent_id.clone(),
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
    /// **Exposure baseline** — the pre-activation tool surface *under* the
    /// [`Self::capability_filter`] ceiling. `capability_filter.allowed_ids` bounds what this run may
    /// EVER expose; the baseline selects which of those are exposed before any skill activates, so
    /// the narrow→wide progressive-disclosure shape becomes expressible (a tool can be reachable
    /// after `skill(x)` without being advertised beforehand).
    ///
    /// `None` (default) ⇒ exactly the pre-baseline behavior (ceiling filter + errs-open skill
    /// narrowing). `Some([])` is legitimate and distinct: the minimal surface (meta-tools +
    /// stable-core only). See the exposure formula in `emit_call_llm`. Additive ABI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_baseline: Option<Vec<CompactString>>,
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
            exposure_baseline: None,
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

    /// Set the pre-activation exposure baseline (see [`Self::exposure_baseline`]). Passing an empty
    /// list is meaningful: meta-tools + stable-core only.
    pub fn with_exposure_baseline(
        mut self,
        ids: impl IntoIterator<Item = impl Into<CompactString>>,
    ) -> Self {
        self.exposure_baseline = Some(ids.into_iter().map(Into::into).collect());
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

    /// § Task 11 · isolation is decided from logical identity alone. Two specs that differ **only**
    /// in every session field a host can name produce the same contract, and the contract itself has
    /// nowhere to put a session.
    #[test]
    fn isolation_is_decided_without_any_session_input() {
        let mut available = CapabilityManifest::new();
        available.add_marker(CapabilityKind::Tool, "read_file", "read files");
        available.add_marker(CapabilityKind::Tool, "write_file", "write files");

        let spec_of = |session: &str| {
            AgentRunSpec::new(
                AgentIdentity::sub_agent("worker", format!("{session}-worker"))
                    .with_parent(format!("{session}-parent")),
                AgentRole::Explore,
                "inspect only",
            )
            .with_capability_filter(AgentCapabilityFilter {
                allowed_kinds: vec![CapabilityKind::Tool],
                allowed_ids: vec!["read_file".into()],
            })
        };

        let a = IsolationManifest::from_spec(&spec_of("host-a"), &available);
        let b = IsolationManifest::from_spec(&spec_of("host-b"), &available);

        let json = |manifest: &IsolationManifest| serde_json::to_value(manifest).unwrap();
        assert_eq!(
            json(&a),
            json(&b),
            "two hosts naming their sessions differently must get the same isolation contract"
        );
        assert_eq!(a.role, AgentRole::Explore);
        assert_eq!(a.context_inheritance, ContextInheritance::SystemOnly);
        assert_eq!(a.permitted_capability_ids.len(), 1);

        let text = json(&a).to_string();
        assert!(
            !text.contains("session"),
            "the isolation contract names a session: {text}"
        );
    }
}
