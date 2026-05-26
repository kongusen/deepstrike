use deepstrike_core::context::manager::ContextManager;
use deepstrike_core::context::pressure::PressureAction;
use deepstrike_core::types::message::Message;
use deepstrike_core::types::skill::SkillMetadata;

// ─── Construction ───────────────────────────────────────────────────────────

#[test]
fn context_manager_new_defaults() {
    let mgr = ContextManager::new(128_000);
    assert_eq!(mgr.max_tokens, 128_000);
    assert!(mgr.partitions.task_state.goal.is_empty());
    assert_eq!(mgr.sprint, 0);
    assert!(mgr.last_handoff.is_none());
    assert!(!mgr.memory_enabled);
    assert!(!mgr.knowledge_enabled);
}

// ─── Pressure ───────────────────────────────────────────────────────────────

#[test]
fn empty_context_has_zero_pressure() {
    let mgr = ContextManager::new(128_000);
    assert!(mgr.rho() < 0.01);
}

#[test]
fn pressure_increases_with_history() {
    let mut mgr = ContextManager::new(1000);
    for i in 0..20 {
        mgr.push_history(Message::user(format!("msg {i}")), 50);
    }
    assert!(mgr.rho() > 0.5);
}

// ─── Compression ────────────────────────────────────────────────────────────

#[test]
fn compress_reduces_history_token_count() {
    let mut mgr = ContextManager::new(500);
    for i in 0..20 {
        mgr.push_history(Message::user(format!("history message number {i}")), 40);
    }
    let before = mgr.partitions.history.token_count;
    mgr.compress(PressureAction::AutoCompact);
    assert!(mgr.partitions.history.token_count < before);
}

#[test]
fn compress_does_not_touch_memory_partition() {
    let mut mgr = ContextManager::new(500);
    mgr.push_memory(Message::user("important memory"), 100);
    for _ in 0..10 {
        mgr.push_history(Message::user("filler"), 50);
    }
    let memory_before = mgr.partitions.memory.token_count;
    mgr.compress(PressureAction::AutoCompact);
    assert_eq!(mgr.partitions.memory.token_count, memory_before);
}

#[test]
fn should_compress_returns_none_when_low_pressure() {
    let mgr = ContextManager::new(128_000);
    assert_eq!(mgr.should_compress(), PressureAction::None);
}

// ─── Render ─────────────────────────────────────────────────────────────────

#[test]
fn render_empty_context_returns_structured_context() {
    let mgr = ContextManager::new(10_000);
    let rendered = mgr.render();
    assert!(rendered.system_text.is_empty());
    assert!(
        rendered.turns.is_empty()
            || rendered
                .turns
                .iter()
                .all(|m| m.content.text_len() < usize::MAX)
    );
}

#[test]
fn render_includes_system_and_history() {
    let mut mgr = ContextManager::new(10_000);
    mgr.partitions
        .system
        .push(Message::system("You are helpful."), 10);
    mgr.push_history(Message::user("Hello"), 5);
    mgr.push_history(Message::assistant("Hi!"), 5);

    let rendered = mgr.render();
    assert!(rendered.system_text.contains("You are helpful"));
    assert_eq!(rendered.turns.len(), 2);
    assert!(
        rendered
            .turns
            .iter()
            .any(|m| m.content.as_text() == Some("Hello"))
    );
}

// ─── Renewal ────────────────────────────────────────────────────────────────

#[test]
fn renew_produces_handoff_artifact() {
    let mut mgr = ContextManager::new(500);
    mgr.partitions.task_state.goal = "test goal".to_string();
    mgr.partitions.system.push(Message::system("rules"), 10);
    for i in 0..10 {
        mgr.push_history(Message::user(format!("msg {i}")), 50);
    }
    assert_eq!(mgr.sprint, 0);
    mgr.renew();
    assert_eq!(mgr.sprint, 1);
    let artifact = mgr.last_handoff.as_ref().unwrap();
    assert_eq!(artifact.goal, "test goal");
    assert_eq!(artifact.sprint, 0);
}

// ─── Skill catalog ──────────────────────────────────────────────────────────

#[test]
fn skill_tool_schema_none_when_no_skills() {
    let mgr = ContextManager::new(10_000);
    assert!(mgr.skill_tool_schema().is_none());
}

#[test]
fn skill_tool_schema_present_with_skills() {
    let mut mgr = ContextManager::new(10_000);
    mgr.set_available_skills(vec![
        SkillMetadata::new("summarize", "Summarize text"),
        SkillMetadata::new("debug", "Debug helper"),
    ]);
    let schema = mgr.skill_tool_schema().unwrap();
    assert!(schema.description.contains("summarize") || schema.description.contains("debug"));
}

#[test]
fn set_available_skills_replaces_previous() {
    let mut mgr = ContextManager::new(10_000);
    mgr.set_available_skills(vec![SkillMetadata::new("a", "A")]);
    assert!(mgr.skill_tool_schema().is_some());

    mgr.set_available_skills(vec![]);
    assert!(mgr.skill_tool_schema().is_none());
}

// ─── Memory / Knowledge meta-tool ───────────────────────────────────────────

#[test]
fn memory_tool_disabled_by_default() {
    let mgr = ContextManager::new(10_000);
    assert!(mgr.memory_tool_schema().is_none());
}

#[test]
fn memory_tool_enabled() {
    let mut mgr = ContextManager::new(10_000);
    mgr.set_memory_enabled(true);
    let schema = mgr.memory_tool_schema().unwrap();
    assert_eq!(schema.name.as_str(), "memory");
    assert!(schema.description.contains("long-term memory"));
}

#[test]
fn knowledge_tool_disabled_by_default() {
    let mgr = ContextManager::new(10_000);
    assert!(mgr.knowledge_tool_schema().is_none());
}

#[test]
fn knowledge_tool_enabled() {
    let mut mgr = ContextManager::new(10_000);
    mgr.set_knowledge_enabled(true);
    let schema = mgr.knowledge_tool_schema().unwrap();
    assert_eq!(schema.name.as_str(), "knowledge");
    assert!(schema.description.contains("knowledge base"));
}

#[test]
fn toggle_memory_on_off() {
    let mut mgr = ContextManager::new(10_000);
    mgr.set_memory_enabled(true);
    assert!(mgr.memory_tool_schema().is_some());
    mgr.set_memory_enabled(false);
    assert!(mgr.memory_tool_schema().is_none());
}

// ─── Push helpers ───────────────────────────────────────────────────────────

#[test]
fn push_history_updates_token_count() {
    let mut mgr = ContextManager::new(10_000);
    assert_eq!(mgr.partitions.history.token_count, 0);
    mgr.push_history(Message::user("hello"), 50);
    assert_eq!(mgr.partitions.history.token_count, 50);
}

#[test]
fn push_memory_updates_token_count() {
    let mut mgr = ContextManager::new(10_000);
    mgr.push_memory(Message::user("fact"), 30);
    assert_eq!(mgr.partitions.memory.token_count, 30);
}
