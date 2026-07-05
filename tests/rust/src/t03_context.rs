#![allow(deprecated)]

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
fn compress_does_not_touch_knowledge_partition() {
    let mut mgr = ContextManager::new(500);
    mgr.push_knowledge(Message::user("important knowledge"), 100);
    for _ in 0..10 {
        mgr.push_history(Message::user("filler"), 50);
    }
    let knowledge_before = mgr.partitions.knowledge.token_count;
    mgr.compress(PressureAction::AutoCompact);
    assert_eq!(mgr.partitions.knowledge.token_count, knowledge_before);
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
fn renew_advances_sprint_and_preserves_goal() {
    let mut mgr = ContextManager::new(500);
    mgr.partitions.task_state.goal = "test goal".to_string();
    mgr.partitions.system.push(Message::system("rules"), 10);
    for i in 0..10 {
        mgr.push_history(Message::user(format!("msg {i}")), 50);
    }
    assert_eq!(mgr.sprint, 0);
    mgr.renew();
    assert_eq!(mgr.sprint, 1);
    assert_eq!(mgr.partitions.task_state.goal, "test goal");
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
fn push_knowledge_updates_token_count() {
    let mut mgr = ContextManager::new(10_000);
    mgr.push_knowledge(Message::user("fact"), 30);
    assert_eq!(mgr.partitions.knowledge.token_count, 30);
}

// ─── Virtual Context Memory & Replay (Phase 2) ───────────────────────────────

#[test]
fn context_fault_serialization_roundtrip() {
    use deepstrike_core::context::fault::ContextFault;
    let fault = ContextFault::MissingArchive {
        session_id: "session-123".to_string(),
        seq: 42,
    };
    let json_str = serde_json::to_string(&fault).unwrap();
    let parsed: ContextFault = serde_json::from_str(&json_str).unwrap();
    match parsed {
        ContextFault::MissingArchive { session_id, seq } => {
            assert_eq!(session_id, "session-123");
            assert_eq!(seq, 42);
        }
        _ => panic!("Expected MissingArchive variant"),
    }
}

#[test]
fn reconstruct_messages_with_fallback_success_and_degrade() {
    use deepstrike_core::runtime::session::SessionEvent;
    use deepstrike_core::runtime::reconstruct_messages_with_fallback;
    use deepstrike_core::context::fault::ContextFault;

    let events = vec![
        SessionEvent::RunStarted {
            run_id: "r1".to_string(),
            goal: "Task Goal".to_string(),
            criteria: vec![],
            agent_id: None,
            system_prompt: None,
        },
        SessionEvent::Compressed {
            turn: 1,
            archived_seq_range: (0, 1),
            category: None,
            primitive: None,
            action: Some("auto_compact".to_string()),
            summary: Some("Compressed turn 1 summary".to_string()),
            summary_tokens: Some(10),
            archive_ref: Some("archive/success.jsonl".to_string()),
            preserved_refs: vec![],
        },
        SessionEvent::Compressed {
            turn: 2,
            archived_seq_range: (2, 3),
            category: None,
            primitive: None,
            action: Some("auto_compact".to_string()),
            summary: Some("Compressed turn 2 summary".to_string()),
            summary_tokens: Some(10),
            archive_ref: Some("archive/missing.jsonl".to_string()),
            preserved_refs: vec![],
        },
    ];

    let messages = reconstruct_messages_with_fallback(&events, "s1", 1000, |ref_str| {
        if ref_str == "archive/success.jsonl" {
            Ok(vec![Message::user("Inside archive message")])
        } else {
            Err(ContextFault::MissingArchive {
                session_id: "s1".to_string(),
                seq: 2,
            })
        }
    });

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].content.as_text().unwrap(), "Task Goal");
    assert_eq!(messages[1].content.as_text().unwrap(), "Inside archive message");
    assert!(messages[2].content.as_text().unwrap().contains("Compressed turn 2 summary"));
}

// ─── Capability Bus & Lease & Agent Run Spec (Phase 3) ───────────────────────

#[test]
fn execute_capability_command_mount_unmount_replace_pin() {
    use deepstrike_core::scheduler::state_machine::LoopStateMachine;
    use deepstrike_core::scheduler::policy::SchedulerBudget;
    use deepstrike_core::types::capability::{CapabilityCommand, CapabilityDescriptor, CapabilityKind};

    let mut sm = LoopStateMachine::new(SchedulerBudget::default());

    // 1. Mount capability
    let desc = CapabilityDescriptor::marker(CapabilityKind::Command, "doctor", "system doctor")
        .with_version("1.0.0");
    sm.execute_capability_command(CapabilityCommand::Mount {
        capability: desc.clone(),
        mounted_by: None,
        mount_reason: None,
    });

    assert_eq!(sm.ctx.capabilities.len(), 1);
    let obs = sm.take_observations();
    assert_eq!(obs.len(), 1);
    if let deepstrike_core::KernelObservation::CapabilityChanged {
        change_kind,
        capability_id,
        version,
        ..
    } = &obs[0] {
        assert_eq!(change_kind.as_deref(), Some("mount"));
        assert_eq!(capability_id.as_deref(), Some("doctor"));
        assert_eq!(version.as_deref(), Some("1.0.0"));
    } else {
        panic!("Expected CapabilityChanged observation");
    }

    // 2. Pin capability
    sm.execute_capability_command(CapabilityCommand::Pin {
        kind: CapabilityKind::Command,
        id: "doctor".to_string(),
    });
    assert!(sm.ctx.capabilities.capabilities()[0].is_pinned);
    let obs = sm.take_observations();
    assert_eq!(obs.len(), 1);
    if let deepstrike_core::KernelObservation::CapabilityChanged {
        change_kind,
        ..
    } = &obs[0] {
        assert_eq!(change_kind.as_deref(), Some("pin"));
    }

    // 3. Replace capability
    let new_desc = CapabilityDescriptor::marker(CapabilityKind::Command, "doctor", "new system doctor")
        .with_version("2.0.0");
    sm.execute_capability_command(CapabilityCommand::Replace {
        old_kind: CapabilityKind::Command,
        old_id: "doctor".to_string(),
        new_capability: new_desc,
    });
    assert_eq!(sm.ctx.capabilities.capabilities()[0].description, "new system doctor");
    let obs = sm.take_observations();
    assert_eq!(obs.len(), 1);
    if let deepstrike_core::KernelObservation::CapabilityChanged {
        change_kind,
        capability_id,
        version,
        ..
    } = &obs[0] {
        assert_eq!(change_kind.as_deref(), Some("replace"));
        assert_eq!(capability_id.as_deref(), Some("doctor"));
        assert_eq!(version.as_deref(), Some("2.0.0"));
    }

    // 4. Unmount capability
    sm.execute_capability_command(CapabilityCommand::Unmount {
        kind: CapabilityKind::Command,
        id: "doctor".to_string(),
    });
    assert_eq!(sm.ctx.capabilities.len(), 0);
}

#[test]
fn capability_lease_auto_revokes() {
    use deepstrike_core::scheduler::state_machine::{LoopStateMachine, LoopEvent};
    use deepstrike_core::scheduler::policy::SchedulerBudget;
    use deepstrike_core::types::capability::{CapabilityCommand, CapabilityDescriptor, CapabilityKind, CapabilityLease};

    let mut sm = LoopStateMachine::new(SchedulerBudget::default());
    let lease = CapabilityLease { expires_at_turn: 2 };
    let desc = CapabilityDescriptor::marker(CapabilityKind::McpServer, "mcp1", "mcp 1 server")
        .with_lease(lease);

    sm.execute_capability_command(CapabilityCommand::Mount {
        capability: desc,
        mounted_by: None,
        mount_reason: None,
    });
    assert_eq!(sm.ctx.capabilities.len(), 1);
    sm.take_observations();

    // Turn = 0: not expired
    sm.feed(LoopEvent::ToolResults { results: vec![] });
    assert_eq!(sm.ctx.capabilities.len(), 1);
    assert_eq!(sm.turn, 1);
    
    // Turn = 1: not expired
    sm.feed(LoopEvent::ToolResults { results: vec![] });
    assert_eq!(sm.ctx.capabilities.len(), 1);
    assert_eq!(sm.turn, 2);

    // Turn = 2: expired, should auto revoke on feed
    sm.feed(LoopEvent::ToolResults { results: vec![] });
    assert_eq!(sm.ctx.capabilities.len(), 0);
    let obs = sm.take_observations();
    assert!(obs.iter().any(|o| {
        if let deepstrike_core::KernelObservation::CapabilityChanged {
            change_kind,
            capability_id,
            ..
        } = o {
            change_kind.as_deref() == Some("unmount") && capability_id.as_deref() == Some("mcp1")
        } else {
            false
        }
    }));
}

#[test]
fn agent_run_spec_capability_filter_enforcement() {
    use deepstrike_core::scheduler::state_machine::LoopStateMachine;
    use deepstrike_core::scheduler::policy::SchedulerBudget;
    use deepstrike_core::types::agent::{AgentRunSpec, AgentIdentity, AgentRole, AgentCapabilityFilter};
    use deepstrike_core::types::capability::{CapabilityDescriptor, CapabilityKind};
    use deepstrike_core::types::message::ToolSchema;
    use compact_str::CompactString;

    let mut sm = LoopStateMachine::new(SchedulerBudget::default());
    sm.tools = vec![
        ToolSchema {
            name: CompactString::new("read_file"),
            description: "read".to_string(),
            parameters: serde_json::json!({}),
        },
        ToolSchema {
            name: CompactString::new("write_file"),
            description: "write".to_string(),
            parameters: serde_json::json!({}),
        },
    ];

    let filter = AgentCapabilityFilter {
        allowed_kinds: vec![CapabilityKind::Tool],
        allowed_ids: vec![CompactString::new("read_file")],
    };
    let spec = AgentRunSpec::new(
        AgentIdentity::new("a1", "s1"),
        AgentRole::Custom,
        "goal",
    ).with_capability_filter(filter);

    sm.run_spec = Some(spec);

    let action = sm.start(deepstrike_core::types::task::RuntimeTask::new("goal"));

    if let deepstrike_core::scheduler::state_machine::LoopAction::CallLLM { tools, .. } = action {
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name.as_str(), "read_file");
    } else {
        panic!("Expected CallLLM action");
    }
}
