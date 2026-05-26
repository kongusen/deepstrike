use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// Cheap, frontmatter-only metadata for a skill.
/// SDK layer parses skill files (markdown + YAML) and produces these structs.
/// The kernel uses metadata for goal-matching and budget planning without ever
/// touching the filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: CompactString,
    pub description: String,
    /// Comma-separated keywords for goal-matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<CompactString>,
    /// Effort level 1-5; controls per-skill token budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<u8>,
    /// SDK-provided cheap token estimate (frontmatter only).
    #[serde(default)]
    pub estimated_tokens: u32,
}

/// Skill with full content materialized.
/// SDK layer loads file content and constructs this; kernel renders it
/// into the C_skill partition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedSkill {
    pub metadata: SkillMetadata,
    pub content: String,
    /// Token count of the content (post-truncation, if any).
    pub content_tokens: u32,
}

impl SkillMetadata {
    pub fn new(name: impl Into<CompactString>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            when_to_use: None,
            allowed_tools: Vec::new(),
            effort: None,
            estimated_tokens: 0,
        }
    }

    pub fn with_when_to_use(mut self, hint: impl Into<String>) -> Self {
        self.when_to_use = Some(hint.into());
        self
    }

    pub fn with_effort(mut self, effort: u8) -> Self {
        self.effort = Some(effort);
        self
    }

    pub fn with_estimated_tokens(mut self, tokens: u32) -> Self {
        self.estimated_tokens = tokens;
        self
    }
}
