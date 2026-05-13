use compact_str::CompactString;
use std::collections::HashMap;

use crate::types::message::ToolSchema;
use crate::types::skill::SkillMetadata;

/// The built-in meta-tool name the kernel injects when skills are registered.
pub const SKILL_TOOL_NAME: &str = "skill";

/// Registry of available skills.
///
/// In the progressive-disclosure model the catalog has one responsibility:
/// know *what* skills exist (name + description) and build the dynamic
/// `skill` meta-tool schema that is included in every `CallLLM` action so
/// the model can invoke any skill by name.
///
/// Skill *content* is never held here — it is returned to the LLM as a
/// regular tool-call result by the SDK layer (read from disk on demand).
pub struct SkillCatalog {
    available: HashMap<CompactString, SkillMetadata>,
}

impl Default for SkillCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillCatalog {
    pub fn new() -> Self {
        Self { available: HashMap::new() }
    }

    /// Replace the full available-skills set in one shot.
    pub fn set_available(&mut self, skills: Vec<SkillMetadata>) {
        self.available = skills.into_iter().map(|s| (s.name.clone(), s)).collect();
    }

    /// Add or replace a single skill entry.
    pub fn upsert_available(&mut self, skill: SkillMetadata) {
        self.available.insert(skill.name.clone(), skill);
    }

    pub fn available_count(&self) -> usize {
        self.available.len()
    }

    pub fn is_empty(&self) -> bool {
        self.available.is_empty()
    }

    /// Build the dynamic skill meta-tool schema to inject into every LLM call.
    ///
    /// Returns `None` when no skills are registered (nothing to inject).
    /// The `description` field embeds the full `<available_skills>` XML so
    /// the model learns what is available without a separate system message.
    pub fn build_tool_schema(&self) -> Option<ToolSchema> {
        if self.available.is_empty() {
            return None;
        }

        let mut skills: Vec<&SkillMetadata> = self.available.values().collect();
        skills.sort_by_key(|s| s.name.as_str());

        let mut xml = String::from("<available_skills>\n");
        for meta in &skills {
            xml.push_str(&format!(
                "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n",
                meta.name, meta.description,
            ));
            if let Some(ref w) = meta.when_to_use {
                xml.push_str(&format!("    <when_to_use>{w}</when_to_use>\n"));
            }
            if let Some(e) = meta.effort {
                xml.push_str(&format!("    <effort>{e}</effort>\n"));
            }
            xml.push_str("  </skill>\n");
        }
        xml.push_str("</available_skills>");

        Some(ToolSchema {
            name: CompactString::new(SKILL_TOOL_NAME),
            description: format!(
                "Load a skill into your context to access specialized instructions for a task.\n\n{xml}"
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the skill to load."
                    }
                },
                "required": ["name"]
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::skill::SkillMetadata;

    #[test]
    fn empty_catalog_returns_no_schema() {
        let catalog = SkillCatalog::new();
        assert!(catalog.build_tool_schema().is_none());
        assert!(catalog.is_empty());
    }

    #[test]
    fn single_skill_builds_schema() {
        let mut catalog = SkillCatalog::new();
        catalog.set_available(vec![SkillMetadata::new("debug", "Debug helper")]);
        let schema = catalog.build_tool_schema().unwrap();
        assert_eq!(schema.name.as_str(), SKILL_TOOL_NAME);
        assert!(schema.description.contains("debug"));
        assert!(schema.description.contains("Debug helper"));
        assert!(schema.description.contains("<available_skills>"));
    }

    #[test]
    fn set_available_replaces_previous() {
        let mut catalog = SkillCatalog::new();
        catalog.set_available(vec![SkillMetadata::new("old", "Old skill")]);
        catalog.set_available(vec![SkillMetadata::new("new", "New skill")]);
        assert_eq!(catalog.available_count(), 1);
        let schema = catalog.build_tool_schema().unwrap();
        assert!(schema.description.contains("new"));
        assert!(!schema.description.contains("old"));
    }

    #[test]
    fn multiple_skills_all_appear_in_schema() {
        let mut catalog = SkillCatalog::new();
        catalog.set_available(vec![
            SkillMetadata::new("alpha", "Alpha skill"),
            SkillMetadata::new("beta", "Beta skill"),
        ]);
        let schema = catalog.build_tool_schema().unwrap();
        assert!(schema.description.contains("alpha"));
        assert!(schema.description.contains("beta"));
    }

    #[test]
    fn upsert_adds_single_skill() {
        let mut catalog = SkillCatalog::new();
        catalog.upsert_available(SkillMetadata::new("solo", "Solo skill"));
        assert_eq!(catalog.available_count(), 1);
        assert!(!catalog.is_empty());
    }
}
