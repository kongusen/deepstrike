use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use super::message::ToolSchema;
use super::skill::SkillMetadata;

/// Lease specification for temporary capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityLease {
    pub expires_at_turn: u32,
}

/// Stable capability category used for model-visible capability manifests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    Tool,
    Skill,
    Memory,
    Knowledge,
    McpServer,
    Command,
    Agent,
}

impl CapabilityKind {
    /// Stable PascalCase label used in capability-change observations
    /// (e.g. `"Tool:read_file"`). This is part of the observation wire format.
    pub fn label(self) -> &'static str {
        match self {
            CapabilityKind::Tool => "Tool",
            CapabilityKind::Skill => "Skill",
            CapabilityKind::Memory => "Memory",
            CapabilityKind::Knowledge => "Knowledge",
            CapabilityKind::McpServer => "McpServer",
            CapabilityKind::Command => "Command",
            CapabilityKind::Agent => "Agent",
        }
    }
}

impl std::fmt::Display for CapabilityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// One model-visible capability.
///
/// The kernel stores metadata only. SDKs still perform all I/O: loading skill
/// markdown, contacting MCP servers, invoking commands, or spawning agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub id: CompactString,
    pub kind: CapabilityKind,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_schema: Option<ToolSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<SkillMetadata>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<CapabilityLease>,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Who requested this capability to be mounted (e.g. "sdk", "milestone:phase_id", agent id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounted_by: Option<String>,
    /// Human-readable reason this capability was mounted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_reason: Option<String>,
}

impl CapabilityDescriptor {
    pub fn tool(schema: ToolSchema) -> Self {
        Self {
            id: schema.name.clone(),
            kind: CapabilityKind::Tool,
            description: schema.description.clone(),
            tool_schema: Some(schema),
            skill: None,
            metadata: serde_json::Value::Null,
            lease: None,
            is_pinned: false,
            version: None,
            mounted_by: None,
            mount_reason: None,
        }
    }

    pub fn skill(skill: SkillMetadata) -> Self {
        Self {
            id: skill.name.clone(),
            kind: CapabilityKind::Skill,
            description: skill.description.clone(),
            tool_schema: None,
            skill: Some(skill),
            metadata: serde_json::Value::Null,
            lease: None,
            is_pinned: false,
            version: None,
            mounted_by: None,
            mount_reason: None,
        }
    }

    pub fn marker(
        kind: CapabilityKind,
        id: impl Into<CompactString>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            description: description.into(),
            tool_schema: None,
            skill: None,
            metadata: serde_json::Value::Null,
            lease: None,
            is_pinned: false,
            version: None,
            mounted_by: None,
            mount_reason: None,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_lease(mut self, lease: CapabilityLease) -> Self {
        self.lease = Some(lease);
        self
    }

    pub fn pinned(mut self) -> Self {
        self.is_pinned = true;
        self
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn with_provenance(
        mut self,
        mounted_by: impl Into<String>,
        mount_reason: impl Into<String>,
    ) -> Self {
        self.mounted_by = Some(mounted_by.into());
        self.mount_reason = Some(mount_reason.into());
        self
    }
}

/// spc_004 §2: placeholder identity/value newtypes for the Object Capability model. Minimal on
/// purpose — no validation beyond storage — until later cards (004-02+) need real semantics.
/// `ResourceSelector` stores a glob string (path/resource pattern); attenuation in spc_004-02
/// starts with string-prefix comparison over it, not full glob semantics.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CapabilityId(pub CompactString);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceSelector(pub CompactString);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActionSet(pub std::collections::BTreeSet<CompactString>);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConstraintSet(pub std::collections::BTreeSet<CompactString>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// `None` ⇒ permanent — never expires.
    pub expires_at_turn: Option<u32>,
}

impl Lease {
    /// spc_004-05: pure — `now` is the caller's current logical turn (this crate's existing
    /// wall-clock-free convention for capability/budget-turn bookkeeping; see `CapabilityLease`
    /// and `BudgetLedger.turns`). `now >= expiry` matches the `>=` convention used everywhere
    /// else expiry is checked in this crate (e.g. `budget_verdict`, `signals::escalate_deadlines`).
    pub fn is_expired(&self, now: u32) -> bool {
        self.expires_at_turn.is_some_and(|expiry| now >= expiry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Principal(pub CompactString);

/// spc_004 §2: an Object Capability — `resource`/`actions`/`constraints`-scoped authority, richer
/// than the tool-name glob `PermissionRule` (`governance/permission.rs`) can express. Additive
/// only in this card — not wired to `CapabilityManifest` or `PermissionManager` (spc_004-04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub id: CapabilityId,
    pub kind: CapabilityKind,
    pub resource: ResourceSelector,
    pub actions: ActionSet,
    pub constraints: ConstraintSet,
    pub lease: Option<Lease>,
    pub delegatable: bool,
    pub issuer: Principal,
}

/// The concrete-path prefix of a (still string/glob-placeholder) `ResourceSelector`: strips a
/// trailing `**` or `*` so two selectors can be compared by simple prefix containment. Not full
/// glob semantics — deliberately, per spc_004-02's stated scope.
pub(crate) fn resource_prefix(selector: &ResourceSelector) -> &str {
    let pattern = selector.0.as_str();
    pattern
        .strip_suffix("**")
        .or_else(|| pattern.strip_suffix('*'))
        .unwrap_or(pattern)
}

/// spc_004-02 / spec §3: is `child` a legal narrowing of `parent`? Resource must be a path
/// subset, actions must be an action subset, constraints must be equal or *more* restrictive
/// (a superset — attenuation only ever adds limits), and the capability kind must match (a Tool
/// capability cannot "attenuate" into a Skill capability). Pure: no I/O, no mutation.
pub fn is_attenuation_of(child: &Capability, parent: &Capability) -> bool {
    child.kind == parent.kind
        && resource_prefix(&child.resource).starts_with(resource_prefix(&parent.resource))
        && child.actions.0.is_subset(&parent.actions.0)
        && child.constraints.0.is_superset(&parent.constraints.0)
}

/// spc_004-03 / core invariant: `Caps(children) ⊆ Caps(parent)`. Every capability in `children`
/// must be [`is_attenuation_of`] at least one capability in `parents`; violators are collected
/// (not just the first) so the caller can report exactly what was over-broad.
pub fn caps_subset(children: &[Capability], parents: &[Capability]) -> Result<(), Vec<Capability>> {
    let violations: Vec<Capability> = children
        .iter()
        .filter(|child| {
            !parents
                .iter()
                .any(|parent| is_attenuation_of(child, parent))
        })
        .cloned()
        .collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Unified source of truth for what the model should know it can do.
///
/// This is deliberately additive: existing SDKs can continue passing raw tool
/// schemas while newer SDKs build and filter a manifest before each model call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityManifest {
    capabilities: Vec<CapabilityDescriptor>,
}

impl CapabilityManifest {
    pub fn new() -> Self {
        Self {
            capabilities: Vec::new(),
        }
    }

    pub fn from_tools(tools: Vec<ToolSchema>) -> Self {
        let mut manifest = Self::new();
        for tool in tools {
            manifest.upsert(CapabilityDescriptor::tool(tool));
        }
        manifest
    }

    pub fn upsert(&mut self, capability: CapabilityDescriptor) {
        if let Some(existing) = self
            .capabilities
            .iter_mut()
            .find(|c| c.kind == capability.kind && c.id == capability.id)
        {
            *existing = capability;
        } else {
            self.capabilities.push(capability);
        }
    }

    pub fn add_tool(&mut self, schema: ToolSchema) {
        self.upsert(CapabilityDescriptor::tool(schema));
    }

    pub fn add_skill(&mut self, skill: SkillMetadata) {
        self.upsert(CapabilityDescriptor::skill(skill));
    }

    pub fn add_marker(
        &mut self,
        kind: CapabilityKind,
        id: impl Into<CompactString>,
        description: impl Into<String>,
    ) {
        self.upsert(CapabilityDescriptor::marker(kind, id, description));
    }

    pub fn remove(&mut self, kind: CapabilityKind, id: &str) {
        self.capabilities
            .retain(|c| !(c.kind == kind && c.id.as_str() == id));
    }

    pub fn remove_kind(&mut self, kind: CapabilityKind) {
        self.capabilities.retain(|c| c.kind != kind);
    }

    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }

    pub fn capabilities(&self) -> &[CapabilityDescriptor] {
        &self.capabilities
    }

    pub fn get_mut(&mut self, kind: CapabilityKind, id: &str) -> Option<&mut CapabilityDescriptor> {
        self.capabilities
            .iter_mut()
            .find(|c| c.kind == kind && c.id.as_str() == id)
    }

    pub fn by_kind(&self, kind: CapabilityKind) -> Vec<&CapabilityDescriptor> {
        let mut out = self
            .capabilities
            .iter()
            .filter(|c| c.kind == kind)
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Return all executable tool schemas in a deterministic order.
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = self
            .capabilities
            .iter()
            .filter_map(|c| c.tool_schema.clone())
            .collect::<Vec<_>>();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        schemas
    }

    pub fn filtered<F>(&self, mut predicate: F) -> Self
    where
        F: FnMut(&CapabilityDescriptor) -> bool,
    {
        let mut manifest = Self::new();
        for capability in &self.capabilities {
            if predicate(capability) {
                manifest.upsert(capability.clone());
            }
        }
        manifest
    }

    /// Compact model-facing inventory for system guidance.
    pub fn format_inventory(&self) -> String {
        if self.capabilities.is_empty() {
            return String::new();
        }

        let mut capabilities = self.capabilities.iter().collect::<Vec<_>>();
        capabilities.sort_by(|a, b| {
            format!("{:?}:{}", a.kind, a.id).cmp(&format!("{:?}:{}", b.kind, b.id))
        });

        let mut out = String::from("<capabilities>\n");
        for capability in capabilities {
            out.push_str(&format!(
                "  <capability kind=\"{:?}\" id=\"{}\">{}</capability>\n",
                capability.kind, capability.id, capability.description
            ));
        }
        out.push_str("</capabilities>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(name: &str) -> ToolSchema {
        ToolSchema {
            name: CompactString::new(name),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn cap(resource: &str, actions: &[&str]) -> Capability {
        Capability {
            id: CapabilityId("cap".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector(resource.into()),
            actions: ActionSet(actions.iter().map(|a| (*a).into()).collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("issuer".into()),
        }
    }

    #[test]
    fn lease_is_expired_before_the_deadline_is_false() {
        let lease = Lease {
            expires_at_turn: Some(10),
        };
        assert!(!lease.is_expired(9));
    }

    #[test]
    fn lease_is_expired_at_or_after_the_deadline_is_true() {
        let lease = Lease {
            expires_at_turn: Some(10),
        };
        assert!(lease.is_expired(10));
        assert!(lease.is_expired(11));
    }

    #[test]
    fn lease_with_no_expiry_never_expires() {
        let lease = Lease {
            expires_at_turn: None,
        };
        assert!(!lease.is_expired(0));
        assert!(!lease.is_expired(u32::MAX));
    }

    #[test]
    fn caps_subset_ok_when_every_child_narrows_some_parent() {
        let parents = vec![cap("/repo/src/**", &["read"])];
        let children = vec![
            cap("/repo/src/utils/**", &["read"]),
            cap("/repo/src/**", &["read"]),
        ];
        assert_eq!(caps_subset(&children, &parents), Ok(()));
    }

    #[test]
    fn caps_subset_pinpoints_the_violating_child() {
        let parents = vec![cap("/repo/src/**", &["read"])];
        let ok_child = cap("/repo/src/utils/**", &["read"]);
        let violating_child = cap("/repo/**", &["read"]);
        let children = vec![ok_child.clone(), violating_child.clone()];

        let result = caps_subset(&children, &parents);
        assert_eq!(result, Err(vec![violating_child]));
    }

    #[test]
    fn is_attenuation_of_accepts_a_narrower_child_resource() {
        let parent = cap("/repo/src/**", &["read"]);
        let child = cap("/repo/src/utils/**", &["read"]);
        assert!(is_attenuation_of(&child, &parent));
    }

    #[test]
    fn is_attenuation_of_rejects_a_wider_child_resource() {
        let parent = cap("/repo/src/**", &["read"]);
        let child = cap("/repo/**", &["read"]);
        assert!(!is_attenuation_of(&child, &parent));
    }

    #[test]
    fn is_attenuation_of_accepts_an_identical_capability() {
        let parent = cap("/repo/src/**", &["read"]);
        let child = cap("/repo/src/**", &["read"]);
        assert!(is_attenuation_of(&child, &parent));
    }

    #[test]
    fn capability_fields_are_readable_and_round_trip_through_json() {
        let cap = Capability {
            id: CapabilityId("cap-1".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: Some(Lease {
                expires_at_turn: Some(10),
            }),
            delegatable: true,
            issuer: Principal("agent-7".into()),
        };

        assert_eq!(cap.id, CapabilityId("cap-1".into()));
        assert_eq!(cap.resource, ResourceSelector("/repo/src/**".into()));
        assert!(cap.delegatable);

        let json = serde_json::to_value(&cap).unwrap();
        let back: Capability = serde_json::from_value(json).unwrap();
        assert_eq!(back, cap);
    }

    #[test]
    fn tool_schemas_are_deterministic() {
        let mut manifest = CapabilityManifest::new();
        manifest.add_tool(schema("zeta"));
        manifest.add_tool(schema("alpha"));

        let names = manifest
            .tool_schemas()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect::<Vec<_>>();

        assert_eq!(names, ["alpha", "zeta"]);
    }

    #[test]
    fn upsert_replaces_same_kind_and_id() {
        let mut manifest = CapabilityManifest::new();
        manifest.add_marker(CapabilityKind::Command, "doctor", "old");
        manifest.add_marker(CapabilityKind::Command, "doctor", "new");

        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest.capabilities()[0].description, "new");
    }

    #[test]
    fn same_id_can_exist_in_different_kinds() {
        let mut manifest = CapabilityManifest::new();
        manifest.add_marker(CapabilityKind::Command, "debug", "command");
        manifest.add_skill(SkillMetadata::new("debug", "skill"));

        assert_eq!(manifest.len(), 2);
        assert_eq!(manifest.by_kind(CapabilityKind::Skill).len(), 1);
        assert_eq!(manifest.by_kind(CapabilityKind::Command).len(), 1);
    }

    #[test]
    fn inventory_mentions_non_tool_capabilities() {
        let mut manifest = CapabilityManifest::new();
        manifest.add_marker(CapabilityKind::Agent, "verify", "verification agent");

        let inventory = manifest.format_inventory();

        assert!(inventory.contains("verify"));
        assert!(inventory.contains("verification agent"));
    }

    #[test]
    fn remove_kind_clears_only_that_kind() {
        let mut manifest = CapabilityManifest::new();
        manifest.add_marker(CapabilityKind::Command, "debug", "command");
        manifest.add_skill(SkillMetadata::new("debug", "skill"));

        manifest.remove_kind(CapabilityKind::Command);

        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest.by_kind(CapabilityKind::Skill).len(), 1);
    }

    #[test]
    fn filtered_returns_matching_capabilities() {
        let mut manifest = CapabilityManifest::new();
        manifest.add_marker(CapabilityKind::Command, "debug", "command");
        manifest.add_skill(SkillMetadata::new("debug", "skill"));

        let filtered = manifest.filtered(|c| c.kind == CapabilityKind::Skill);

        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered.by_kind(CapabilityKind::Skill)[0].id.as_str(),
            "debug"
        );
    }
}

/// Commands representing direct actions on the capability bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CapabilityCommand {
    Mount {
        capability: CapabilityDescriptor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mounted_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mount_reason: Option<String>,
    },
    Unmount {
        kind: CapabilityKind,
        id: String,
    },
    Replace {
        old_kind: CapabilityKind,
        old_id: String,
        new_capability: CapabilityDescriptor,
    },
    Pin {
        kind: CapabilityKind,
        id: String,
    },
}
