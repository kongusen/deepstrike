use super::compression::CompressionPipeline;
use super::config::ContextConfig;
use super::partitions::ContextPartitions;
use super::pressure::{PressureAction, PressureMonitor};
use super::renderer::RenderedContext;
use super::renewal::{HandoffArtifact, RenewalPolicy};
use super::sections::{ContextSectionPartition, ContextSectionRegistry};
use super::snapshot::ContextSnapshotHint;
use super::skill_catalog::SkillCatalog;
use super::task_state::{TaskState, TaskUpdate};
use super::token_engine::ContextTokenEngine;
use crate::types::capability::{CapabilityKind, CapabilityManifest};
use crate::types::message::{Message, ToolSchema};
use crate::types::skill::SkillMetadata;
use compact_str::CompactString;

pub const MEMORY_TOOL_NAME: &str = "memory";
pub const KNOWLEDGE_TOOL_NAME: &str = "knowledge";

pub struct ContextManager {
    pub partitions: ContextPartitions,
    pub max_tokens: u32,
    pub config: ContextConfig,
    pub engine: ContextTokenEngine,
    pub sprint: u32,
    pub last_handoff: Option<HandoffArtifact>,
    pub skills: SkillCatalog,
    pub capabilities: CapabilityManifest,
    pub sections: ContextSectionRegistry,
    pub memory_enabled: bool,
    pub knowledge_enabled: bool,
    pub plan_tool_enabled: bool,
    compression: CompressionPipeline,
    pressure: PressureMonitor,
    renewal: RenewalPolicy,
}

impl ContextManager {
    pub fn new(max_tokens: u32) -> Self {
        Self::with_config(
            max_tokens,
            ContextConfig::default(),
            ContextTokenEngine::char_approx(),
        )
    }

    pub fn with_config(max_tokens: u32, config: ContextConfig, engine: ContextTokenEngine) -> Self {
        let compression = CompressionPipeline::new(&config);
        let pressure = PressureMonitor::new(max_tokens, config.clone());
        let renewal = RenewalPolicy::from_config(&config);
        let partitions = ContextPartitions::new(&config);
        Self {
            partitions,
            max_tokens,
            config,
            engine,
            sprint: 0,
            last_handoff: None,
            skills: SkillCatalog::new(),
            capabilities: CapabilityManifest::new(),
            sections: ContextSectionRegistry::default_agent_sections(),
            memory_enabled: false,
            knowledge_enabled: false,
            plan_tool_enabled: false,
            compression,
            pressure,
            renewal,
        }
    }

    // ── Pressure ─────────────────────────────────────────────────────────────

    pub fn rho(&self) -> f64 {
        self.pressure.pressure(&self.partitions, &self.engine)
    }

    pub fn should_compress(&self) -> PressureAction {
        self.pressure.recommend(self.rho())
    }

    pub fn compress(&mut self, action: PressureAction) -> (u32, Option<String>, Vec<Message>) {
        if self.sections.is_partition_pinned(ContextSectionPartition::History) {
            return (0, None, vec![]);
        }
        let target = self.config.target_tokens(self.max_tokens);
        self.compression.compress(
            &mut self.partitions,
            action,
            self.max_tokens,
            target,
            &self.engine,
        )
    }

    pub fn force_compress(&mut self) -> (u32, Option<String>, Vec<Message>) {
        if self.sections.is_partition_pinned(ContextSectionPartition::History) {
            return (0, None, vec![]);
        }
        self.compression.compress(
            &mut self.partitions,
            PressureAction::AutoCompact,
            self.max_tokens,
            0,
            &self.engine,
        )
    }

    // ── Renewal ───────────────────────────────────────────────────────────────

    pub fn should_renew(&self) -> bool {
        self.renewal
            .should_renew(&self.pressure, &self.partitions, &self.engine)
    }

    pub fn renew(&mut self) {
        let goal = self.partitions.task_state.goal.clone();
        let (renewed, artifact) =
            self.renewal
                .renew(&self.partitions, &goal, self.sprint, self.max_tokens);
        self.partitions = renewed;
        self.last_handoff = Some(artifact);
        self.sprint += 1;
    }

    // ── Render ────────────────────────────────────────────────────────────────

    pub fn render(&self) -> RenderedContext {
        super::renderer::render(&self.partitions, self.max_tokens, &self.engine)
    }

    pub fn snapshot_hint(&self) -> ContextSnapshotHint {
        ContextSnapshotHint::from_parts(&self.sections, &self.capabilities)
    }

    // ── History / Memory ──────────────────────────────────────────────────────

    pub fn push_history(&mut self, msg: Message, tokens: u32) {
        self.partitions.history.push(msg, tokens);
    }

    pub fn push_memory(&mut self, msg: Message, tokens: u32) {
        self.partitions.memory.push(msg, tokens);
    }

    // ── Task state (Phase B) ─────────────────────────────────────────────────

    /// Initialise task state at run start. Should be called once after
    /// `run_started` is processed.
    pub fn init_task(&mut self, goal: String, criteria: Vec<String>) {
        self.partitions.task_state = TaskState {
            goal,
            criteria,
            ..Default::default()
        };
    }

    /// Apply a partial task state update. Called by the SDK after tool
    /// completion or in response to an `update_plan` meta-tool call.
    pub fn update_task(&mut self, update: TaskUpdate) {
        self.partitions.task_state.apply(update);
    }

    // ── Section pinning ──────────────────────────────────────────────────────

    /// Pin a section so its partition is exempt from GC even under token pressure.
    /// Returns true if the section id was found in the registry.
    pub fn pin_section(&mut self, id: &str) -> bool {
        self.sections.pin(id)
    }

    /// Unpin a section, allowing its partition to be compressed again.
    /// Returns true if the section id was found in the registry.
    pub fn unpin_section(&mut self, id: &str) -> bool {
        self.sections.unpin(id)
    }

    // ── Skills ────────────────────────────────────────────────────────────────

    pub fn set_available_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.capabilities.remove_kind(CapabilityKind::Skill);
        for skill in &skills {
            self.capabilities.add_skill(skill.clone());
        }
        self.skills.set_available(skills);
    }

    pub fn skill_tool_schema(&self) -> Option<ToolSchema> {
        self.skills.build_tool_schema()
    }

    // ── Meta-tools ────────────────────────────────────────────────────────────

    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Memory,
                MEMORY_TOOL_NAME,
                "Search long-term memory through the memory meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Memory, MEMORY_TOOL_NAME);
        }
    }

    pub fn set_knowledge_enabled(&mut self, enabled: bool) {
        self.knowledge_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Knowledge,
                KNOWLEDGE_TOOL_NAME,
                "Search external knowledge through the knowledge meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Knowledge, KNOWLEDGE_TOOL_NAME);
        }
    }

    pub fn set_plan_tool_enabled(&mut self, enabled: bool) {
        self.plan_tool_enabled = enabled;
        if enabled {
            self.capabilities.add_marker(
                CapabilityKind::Tool,
                "update_plan",
                "Update task plan and progress through the planning meta-tool.",
            );
        } else {
            self.capabilities
                .remove(CapabilityKind::Tool, "update_plan");
        }
    }

    pub fn capability_inventory(&self) -> String {
        self.capabilities.format_inventory()
    }

    pub fn meta_tool_schemas(&self) -> Vec<ToolSchema> {
        let mut tools = Vec::new();
        if let Some(skill_tool) = self.skill_tool_schema() {
            tools.push(skill_tool);
        }
        if let Some(memory_tool) = self.memory_tool_schema() {
            tools.push(memory_tool);
        }
        if let Some(knowledge_tool) = self.knowledge_tool_schema() {
            tools.push(knowledge_tool);
        }
        if let Some(plan_tool) = self.plan_tool_schema() {
            tools.push(plan_tool);
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    pub fn plan_tool_schema(&self) -> Option<ToolSchema> {
        if !self.plan_tool_enabled {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new("update_plan"),
            description: "Update your task plan and progress. Call this after completing a step or when the plan changes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": { "type": "array", "items": { "type": "string" }, "description": "The updated list of plan steps." },
                    "current_step": { "type": "integer", "description": "The 0-based index of the step currently executing." },
                    "progress": { "type": "string", "description": "Free-text summary of progress made so far." },
                    "blocked_on": { "type": "array", "items": { "type": "string" }, "description": "Reasons why the task is blocked." }
                }
            }),
        })
    }

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
    use crate::context::task_state::PlanStep;
    use crate::types::message::Message;
    use crate::types::skill::SkillMetadata;

    #[test]
    fn manager_renew_uses_task_state_goal() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("test goal".to_string(), vec![]);
        mgr.partitions.system.push(Message::system("rules"), 10);
        for i in 0..10 {
            mgr.push_history(Message::user(format!("msg {i}")), 50);
        }
        mgr.renew();
        let artifact = mgr.last_handoff.as_ref().unwrap();
        assert_eq!(artifact.goal, "test goal");
        assert_eq!(mgr.sprint, 1);
    }

    #[test]
    fn compress_only_touches_history() {
        let mut mgr = ContextManager::new(1_000);
        mgr.partitions.memory.push(Message::user("memory"), 100);
        for _ in 0..30 {
            mgr.push_history(Message::user("history msg"), 50);
        }
        let memory_before = mgr.partitions.memory.token_count;
        let history_before = mgr.partitions.history.token_count;
        mgr.compress(PressureAction::AutoCompact);
        assert_eq!(mgr.partitions.memory.token_count, memory_before);
        assert!(mgr.partitions.history.token_count < history_before);
    }

    #[test]
    fn init_task_sets_goal_and_criteria() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("analyse data".to_string(), vec!["criterion A".to_string()]);
        assert_eq!(mgr.partitions.task_state.goal, "analyse data");
        assert_eq!(mgr.partitions.task_state.criteria, ["criterion A"]);
    }

    #[test]
    fn update_task_applies_plan() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("g".to_string(), vec![]);
        mgr.update_task(TaskUpdate {
            plan: Some(vec!["step 1".to_string(), "step 2".to_string()]),
            current_step: Some(0),
            ..Default::default()
        });
        assert_eq!(mgr.partitions.task_state.plan.len(), 2);
        assert_eq!(mgr.partitions.task_state.current_step, Some(0));
    }

    #[test]
    fn task_state_survives_autocompact() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("survive compression".to_string(), vec![]);
        mgr.update_task(TaskUpdate {
            plan: Some(vec!["fetch data".to_string(), "analyse".to_string()]),
            ..Default::default()
        });
        for _ in 0..10 {
            mgr.push_history(Message::user("filler"), 50);
        }
        mgr.compress(PressureAction::AutoCompact);
        // task_state must be intact after history is wiped
        assert_eq!(mgr.partitions.task_state.goal, "survive compression");
        assert_eq!(mgr.partitions.task_state.plan.len(), 2);
    }

    #[test]
    fn render_includes_task_state_in_system_text() {
        let mut mgr = ContextManager::new(10_000);
        mgr.init_task("find anomalies".to_string(), vec![]);
        let rc = mgr.render();
        assert!(rc.system_text.contains("[TASK STATE] goal: find anomalies"));
    }

    #[test]
    fn renewal_open_tasks_from_task_state() {
        let mut mgr = ContextManager::new(1_000);
        mgr.init_task("g".to_string(), vec![]);
        mgr.partitions.task_state.plan = vec![
            PlanStep {
                label: "done".to_string(),
                done: true,
            },
            PlanStep {
                label: "pending".to_string(),
                done: false,
            },
        ];
        mgr.renew();
        let artifact = mgr.last_handoff.as_ref().unwrap();
        assert_eq!(artifact.open_tasks, vec!["pending"]);
    }

    #[test]
    fn pinned_history_section_skips_compression() {
        let mut mgr = ContextManager::new(1_000);
        // Fill history well past the pressure threshold
        for _ in 0..30 {
            mgr.push_history(Message::user("filler message for pinning test"), 50);
        }
        let tokens_before = mgr.partitions.history.token_count;
        mgr.pin_section("history.rolling");
        // compress() should be a no-op while the section is pinned
        let (saved, _, _) = mgr.compress(PressureAction::AutoCompact);
        assert_eq!(saved, 0, "compression must be skipped when history is pinned");
        assert_eq!(mgr.partitions.history.token_count, tokens_before);
    }

    #[test]
    fn unpinned_history_section_allows_compression() {
        let mut mgr = ContextManager::new(1_000);
        for _ in 0..30 {
            mgr.push_history(Message::user("filler"), 50);
        }
        mgr.pin_section("history.rolling");
        mgr.unpin_section("history.rolling");
        let (saved, _, _) = mgr.compress(PressureAction::AutoCompact);
        assert!(saved > 0, "compression should proceed after unpin");
    }

    #[test]
    fn force_compress_also_skips_when_history_pinned() {
        let mut mgr = ContextManager::new(1_000);
        for _ in 0..10 {
            mgr.push_history(Message::user("filler"), 50);
        }
        mgr.pin_section("history.rolling");
        let (saved, _, _) = mgr.force_compress();
        assert_eq!(saved, 0);
    }

    #[test]
    fn skill_tool_schema_empty_when_no_skills() {
        let mgr = ContextManager::new(10_000);
        assert!(mgr.skill_tool_schema().is_none());
    }

    #[test]
    fn skill_tool_schema_present_when_registered() {
        let mut mgr = ContextManager::new(10_000);
        mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
        assert!(
            mgr.skill_tool_schema()
                .unwrap()
                .description
                .contains("debug")
        );
    }

    #[test]
    fn available_skills_are_reflected_in_capability_manifest() {
        let mut mgr = ContextManager::new(1_000);
        mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);

        let inventory = mgr.capability_inventory();

        assert!(inventory.contains("debug"));
        assert!(inventory.contains("Debug helper"));
    }

    #[test]
    fn toggled_meta_tools_are_reflected_in_capability_manifest() {
        let mut mgr = ContextManager::new(1_000);

        mgr.set_memory_enabled(true);
        assert!(mgr.capability_inventory().contains(MEMORY_TOOL_NAME));

        mgr.set_memory_enabled(false);
        assert!(!mgr.capability_inventory().contains(MEMORY_TOOL_NAME));
    }

    #[test]
    fn meta_tool_schemas_are_sorted() {
        let mut mgr = ContextManager::new(1_000);
        mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
        mgr.set_memory_enabled(true);
        mgr.set_knowledge_enabled(true);

        let names = mgr
            .meta_tool_schemas()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect::<Vec<_>>();

        assert_eq!(names, ["knowledge", "memory", "skill"]);
    }

    #[test]
    fn section_registry_is_available_on_manager() {
        let mgr = ContextManager::new(1_000);

        assert!(mgr.sections.get("capabilities.inventory").is_some());
    }

    #[test]
    fn snapshot_hint_changes_when_capabilities_change() {
        let mut mgr = ContextManager::new(1_000);
        let before = mgr.snapshot_hint();

        mgr.set_memory_enabled(true);
        let after = mgr.snapshot_hint();

        assert_ne!(
            before.capability_manifest_hash,
            after.capability_manifest_hash
        );
    }
}
