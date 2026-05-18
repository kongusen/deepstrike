use super::compression::CompressionPipeline;
use super::partitions::ContextPartitions;
use super::pressure::{PressureAction, PressureMonitor};
use super::renderer::RenderedContext;
use super::renewal::{HandoffArtifact, RenewalPolicy};
use super::skill_catalog::SkillCatalog;
use crate::types::message::{Message, ToolSchema};
use crate::types::skill::SkillMetadata;
use compact_str::CompactString;

/// The built-in meta-tool name the kernel injects when memory retrieval is enabled.
pub const MEMORY_TOOL_NAME: &str = "memory";
/// The built-in meta-tool name the kernel injects when knowledge retrieval is enabled.
pub const KNOWLEDGE_TOOL_NAME: &str = "knowledge";

pub struct ContextManager {
    pub partitions: ContextPartitions,
    pub max_tokens: u32,
    pub current_goal: String,
    pub sprint: u32,
    pub last_handoff: Option<HandoffArtifact>,
    pub skills: SkillCatalog,
    pub memory_enabled: bool,
    pub knowledge_enabled: bool,
    compression: CompressionPipeline,
    pressure: PressureMonitor,
    renewal: RenewalPolicy,
}

impl ContextManager {
    pub fn new(max_tokens: u32) -> Self {
        Self {
            partitions: ContextPartitions::new(),
            max_tokens,
            current_goal: String::new(),
            sprint: 0,
            last_handoff: None,
            skills: SkillCatalog::new(),
            memory_enabled: false,
            knowledge_enabled: false,
            compression: CompressionPipeline::new(),
            pressure: PressureMonitor::new(max_tokens),
            renewal: RenewalPolicy::default(),
        }
    }

    pub fn rho(&self) -> f64 {
        self.pressure.pressure(&self.partitions)
    }

    pub fn should_compress(&self) -> PressureAction {
        self.pressure.recommend(self.rho())
    }

    pub fn compress(&mut self, action: PressureAction) {
        self.compression.compress(&mut self.partitions, action, self.max_tokens);
    }

    pub fn should_renew(&self) -> bool {
        self.renewal.should_renew(&self.pressure, &self.partitions)
    }

    pub fn renew(&mut self) {
        let (renewed, artifact) = self.renewal.renew(
            &self.partitions,
            &self.current_goal,
            self.sprint,
        );
        self.partitions = renewed;
        self.last_handoff = Some(artifact);
        self.sprint += 1;
    }

    pub fn render(&self) -> RenderedContext {
        super::renderer::render(&self.partitions, self.max_tokens)
    }

    pub fn push_history(&mut self, msg: Message, tokens: u32) {
        self.partitions.history.push(msg, tokens);
    }

    pub fn push_memory(&mut self, msg: Message, tokens: u32) {
        self.partitions.memory.push(msg, tokens);
    }

    /// Replace the available-skills set. SDK layer calls this once at
    /// agent construction (and on hot-reload) with frontmatter-only metadata.
    pub fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.skills.set_available(skills);
    }

    /// Build the `skill` meta-tool schema to inject into the next `CallLLM` action.
    /// Returns `None` when no skills are registered.
    pub fn skill_tool_schema(&self) -> Option<ToolSchema> {
        self.skills.build_tool_schema()
    }

    /// Enable or disable the `memory` meta-tool. Call with `true` when a DreamStore
    /// is configured — the SDK intercepts the resulting tool calls and runs the search.
    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
    }

    /// Enable or disable the `knowledge` meta-tool. Call with `true` when a KnowledgeSource
    /// is configured — the SDK intercepts the resulting tool calls and runs retrieval.
    pub fn set_knowledge_enabled(&mut self, enabled: bool) {
        self.knowledge_enabled = enabled;
    }

    /// Build the `memory` meta-tool schema to inject into every `CallLLM` action.
    /// Returns `None` when memory retrieval is disabled.
    pub fn memory_tool_schema(&self) -> Option<ToolSchema> {
        if !self.memory_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(MEMORY_TOOL_NAME),
            description: "Search your long-term memory for relevant past experiences and knowledge. \
                Call this when you need context from prior sessions that is not present in the current conversation."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural-language description of what you want to recall." },
                    "top_k": { "type": "integer", "description": "Maximum number of memory entries to return. Defaults to 5." }
                },
                "required": ["query"]
            }),
        })
    }

    /// Build the `knowledge` meta-tool schema to inject into every `CallLLM` action.
    /// Returns `None` when knowledge retrieval is disabled.
    pub fn knowledge_tool_schema(&self) -> Option<ToolSchema> {
        if !self.knowledge_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(KNOWLEDGE_TOOL_NAME),
            description: "Search the external knowledge base for facts, documentation, or reference data. \
                Call this when you need information that may exist in the knowledge base but is not in your context."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What you want to look up." },
                    "top_k": { "type": "integer", "description": "Maximum number of results to return. Defaults to 5." }
                },
                "required": ["query"]
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::Message;
    use crate::types::skill::SkillMetadata;

    #[test]
    fn manager_renew_produces_handoff() {
        let mut mgr = ContextManager::new(1000);
        mgr.current_goal = "test goal".to_string();
        mgr.partitions.system.push(Message::system("rules"), 10);
        for i in 0..10 {
            mgr.push_history(Message::user(format!("msg {i}")), 50);
        }
        mgr.renew();
        let artifact = mgr.last_handoff.as_ref().unwrap();
        assert_eq!(artifact.goal, "test goal");
        assert_eq!(artifact.sprint, 0);
        assert_eq!(mgr.sprint, 1);
    }

    #[test]
    fn compress_only_touches_history_not_memory() {
        let mut mgr = ContextManager::new(1000);
        mgr.partitions.memory.push(Message::user("memory"), 100);
        for _ in 0..5 {
            mgr.push_history(Message::user("history msg"), 50);
        }
        let memory_before = mgr.partitions.memory.token_count;
        mgr.compress(PressureAction::AutoCompact);
        assert_eq!(mgr.partitions.memory.token_count, memory_before);
        // AutoCompact collapses history to a 10-token placeholder.
        assert!(mgr.partitions.history.token_count <= 10);
    }

    #[test]
    fn skill_tool_schema_empty_when_no_skills() {
        let mgr = ContextManager::new(10_000);
        assert!(mgr.skill_tool_schema().is_none());
    }

    #[test]
    fn skill_tool_schema_present_when_skills_registered() {
        let mut mgr = ContextManager::new(10_000);
        mgr.set_available_skills(vec![
            SkillMetadata::new("debug", "Debug helper"),
        ]);
        let schema = mgr.skill_tool_schema().unwrap();
        assert!(schema.description.contains("debug"));
    }
}
