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
        assert_eq!(filtered.by_kind(CapabilityKind::Skill)[0].id.as_str(), "debug");
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
