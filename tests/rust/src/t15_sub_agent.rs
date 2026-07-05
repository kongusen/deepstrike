#![allow(deprecated)]

// Phase 7 — Sub-Agent Isolation
// G7 gate: sub-agent isolation + lineage replay

use compact_str::CompactString;
use deepstrike_core::scheduler::policy::SchedulerBudget;
use deepstrike_core::scheduler::state_machine::*;
use deepstrike_core::scheduler::tcb::{TaskLifecycle, WaitReason};
use deepstrike_core::proc::ProcessState;
use deepstrike_core::types::agent::{
    AgentCapabilityFilter, AgentIdentity, AgentIsolation, AgentRole, AgentRunSpec,
    ContextInheritance, IsolationManifest,
};
use deepstrike_core::types::capability::{CapabilityKind, CapabilityManifest};
use deepstrike_core::types::message::*;
use deepstrike_core::types::result::{LoopResult, SubAgentResult, TerminationReason};
use deepstrike_core::types::task::RuntimeTask;

fn default_sm() -> LoopStateMachine {
    LoopStateMachine::new(SchedulerBudget { max_tokens: 128_000, ..SchedulerBudget::default() })
}

fn text_response() -> LoopEvent {
    LoopEvent::LLMResponse {
        message: Message {
            role: Role::Assistant,
            content: Content::Text("done".into()),
            tool_calls: vec![],
            token_count: None,
        },
    }
}

fn simple_manifest_with_tools() -> CapabilityManifest {
    let mut manifest = CapabilityManifest::new();
    manifest.add_tool(ToolSchema {
        name: CompactString::new("read_file"),
        description: "read a file".into(),
        parameters: serde_json::json!({"type": "object"}),
    });
    manifest.add_tool(ToolSchema {
        name: CompactString::new("write_file"),
        description: "write a file".into(),
        parameters: serde_json::json!({"type": "object"}),
    });
    manifest.add_marker(CapabilityKind::Skill, "search", "semantic search");
    manifest
}

// ─── G7 gate: IsolationManifest generation ─────────────────────────────────

#[test]
fn isolation_manifest_from_spec_applies_capability_filter() {
    let available = simple_manifest_with_tools();

    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("explore-1", "session-abc"),
        AgentRole::Explore,
        "read only exploration",
    )
    .with_capability_filter(AgentCapabilityFilter {
        allowed_kinds: vec![CapabilityKind::Skill],
        allowed_ids: vec![],
    });

    let manifest = IsolationManifest::from_spec(&spec, "parent-session-001", &available);

    assert_eq!(manifest.permitted_capability_ids.len(), 1);
    assert_eq!(manifest.permitted_capability_ids[0].as_str(), "search");
}

#[test]
fn isolation_manifest_role_defaults_context_inheritance() {
    let available = CapabilityManifest::new();

    let explore_spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("e", "s"),
        AgentRole::Explore,
        "explore",
    );
    let explore = IsolationManifest::from_spec(&explore_spec, "parent", &available);
    assert_eq!(explore.context_inheritance, ContextInheritance::SystemOnly);

    let implement_spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("i", "s"),
        AgentRole::Implement,
        "implement",
    );
    let implement = IsolationManifest::from_spec(&implement_spec, "parent", &available);
    assert_eq!(implement.context_inheritance, ContextInheritance::Full);

    let verify_spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("v", "s"),
        AgentRole::Verify,
        "verify",
    );
    let verify = IsolationManifest::from_spec(&verify_spec, "parent", &available);
    assert_eq!(verify.context_inheritance, ContextInheritance::SystemOnly);

    let plan_spec =
        AgentRunSpec::new(AgentIdentity::sub_agent("p", "s"), AgentRole::Plan, "plan");
    let plan = IsolationManifest::from_spec(&plan_spec, "parent", &available);
    assert_eq!(plan.context_inheritance, ContextInheritance::Full);
}

// ─── G7 gate: parent-child lineage ─────────────────────────────────────────

#[test]
fn sub_agent_identity_carries_parent_session_id() {
    let identity = AgentIdentity::sub_agent("child-agent", "child-session")
        .with_parent("parent-session-xyz");

    assert!(identity.is_sub_agent);
    assert_eq!(identity.parent_session_id.as_deref(), Some("parent-session-xyz"));
}

#[test]
fn spawn_sub_agent_emits_process_observation() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.take_observations(); // drain start observations

    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("worker", "worker-session"),
        AgentRole::Implement,
        "do work",
    );
    let action = sm.spawn_sub_agent(spec, "parent-session-001");
    assert!(matches!(action, LoopAction::AwaitingResume));
    assert_eq!(sm.lifecycle(), TaskLifecycle::Suspended);
    assert!(matches!(sm.wait_reason(), Some(WaitReason::SubAgentJoin(_))));

    let obs = sm.take_observations();
    assert!(obs.iter().any(|o| matches!(
        o,
        KernelObservation::AgentProcessChanged {
            agent_id,
            parent_session_id,
            role,
            state,
            ..
        } if agent_id == "worker"
            && parent_session_id == "parent-session-001"
            && role == "implement"
            && state == "running"
    )));
}

#[test]
fn spawn_sub_agent_registers_kernel_process() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.take_observations();

    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("worker", "worker-session"),
        AgentRole::Implement,
        "do work",
    );
    sm.spawn_sub_agent(spec, "parent-session-001");
    let obs = sm.take_observations();

    let process = sm.agent_process("worker").expect("process should be registered");
    assert_eq!(process.agent_id.as_str(), "worker");
    assert_eq!(process.parent_session_id.as_str(), "parent-session-001");
    assert_eq!(process.role, AgentRole::Implement);
    assert_eq!(process.context_inheritance, ContextInheritance::Full);
    assert_eq!(process.state, ProcessState::Running);
    assert!(process.permitted_capability_ids.is_empty());
    assert!(obs.iter().any(|o| {
        matches!(
            o,
            KernelObservation::AgentProcessChanged {
                agent_id,
                state,
                ..
            } if agent_id == "worker" && state == "running"
        )
    }));
    assert!(obs.iter().any(|o| matches!(
        o,
        KernelObservation::Suspended { reason, .. } if reason == "sub_agent_await"
    )));
}

#[test]
fn spawn_sub_agent_manifest_permits_filtered_capabilities() {
    let mut sm = default_sm();
    // Mount a tool into the state machine's capability set
    sm.mount_capability(
        deepstrike_core::types::capability::CapabilityDescriptor::tool(ToolSchema {
            name: CompactString::new("deploy"),
            description: "deploy tool".into(),
            parameters: serde_json::json!({"type": "object"}),
        }),
        None,
        None,
    );
    sm.start(RuntimeTask::new("test"));
    sm.take_observations();

    // Spec with no filter — should see all capabilities
    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("full-agent", "s"),
        AgentRole::Implement,
        "full access",
    );
    let _action = sm.spawn_sub_agent(spec, "parent");
    sm.take_observations();
    let process = sm.agent_process("full-agent").expect("process");
    assert!(
        process
            .permitted_capability_ids
            .contains(&CompactString::new("deploy")),
        "unfiltered sub-agent should see deploy capability"
    );
}

// ─── G7 gate: replay — sub-agent completed resumes loop ──────────────────────

#[test]
fn sub_agent_completed_resumes_loop_with_call_llm() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.feed(text_response()); // drive through one turn
    sm.take_observations();

    let result = SubAgentResult {
        agent_id: CompactString::new("worker"),
        result: LoopResult {
            termination: TerminationReason::Completed,
            final_message: Some(Message::assistant("task complete")),
            turns_used: 3,
            total_tokens_used: 500,
            loop_continue: None,
            classify_branch: None,
            tournament_winner: None,
        },
    };

    let action = sm.feed(LoopEvent::SubAgentCompleted { result });
    assert!(
        matches!(action, LoopAction::CallLLM { .. }),
        "SubAgentCompleted should resume loop with CallLLM, got: {action:?}"
    );
}

#[test]
fn sub_agent_completed_updates_kernel_process() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.take_observations();

    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("worker", "worker-session"),
        AgentRole::Implement,
        "do work",
    );
    sm.spawn_sub_agent(spec, "parent-session-001");
    sm.take_observations();

    assert_eq!(sm.lifecycle(), TaskLifecycle::Suspended);
    assert!(matches!(sm.wait_reason(), Some(WaitReason::SubAgentJoin(_))));

    let result = SubAgentResult {
        agent_id: CompactString::new("worker"),
        result: LoopResult {
            termination: TerminationReason::Completed,
            final_message: Some(Message::assistant("task complete")),
            turns_used: 3,
            total_tokens_used: 500,
            loop_continue: None,
            classify_branch: None,
            tournament_winner: None,
        },
    };

    sm.feed(LoopEvent::SubAgentCompleted { result });

    let process = sm.agent_process("worker").expect("process should remain registered");
    assert_eq!(process.state, ProcessState::Joined);
    assert!(process.result.is_some());
    assert!(sm.take_observations().iter().any(|o| {
        matches!(
            o,
            KernelObservation::AgentProcessChanged {
                agent_id,
                state,
                result_termination,
                ..
            } if agent_id == "worker"
                && state == "joined"
                && result_termination.as_deref() == Some("completed")
        )
    }));
}

// ─── G7 gate: serialization ─────────────────────────────────────────────────

#[test]
fn isolation_manifest_serializes_round_trip() {
    let available = simple_manifest_with_tools();
    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("agent-x", "session-y")
            .with_parent("session-parent"),
        AgentRole::Verify,
        "verify output",
    )
    .with_isolation(AgentIsolation::ReadOnly);

    let manifest = IsolationManifest::from_spec(&spec, "session-parent", &available);
    let json = serde_json::to_string(&manifest).expect("serialize");
    let restored: IsolationManifest = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(restored.agent_id, manifest.agent_id);
    assert_eq!(restored.parent_session_id, manifest.parent_session_id);
    assert_eq!(restored.role, manifest.role);
    assert_eq!(restored.isolation, manifest.isolation);
    assert_eq!(restored.context_inheritance, manifest.context_inheritance);
    assert_eq!(restored.permitted_capability_ids, manifest.permitted_capability_ids);
}
