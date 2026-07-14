use super::runtime::EVENT_REPLAY_WINDOW_CAPACITY;
use super::*;

fn correlated_input(
    operation_id: &str,
    event_id: &str,
    observed_at_ms: u64,
    event: KernelInputEvent,
) -> KernelInput {
    KernelInput::correlated(operation_id, event_id, observed_at_ms, event)
}

#[test]
fn abi_v2_envelope_correlates_input_step_and_effect() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    ));

    assert_eq!(KERNEL_ABI_VERSION, 2);
    assert_eq!(step.version, 2);
    assert_eq!(step.operation_id, "op-1");
    assert_eq!(step.input_event_id, "event-1");
    assert_eq!(step.step_seq, 1);
    assert!(step.faults.is_empty());
    assert_eq!(step.actions.len(), 1);
    assert_eq!(step.actions[0].causation_id, "event-1");
    assert_eq!(step.actions[0].effect_id, "op-1:step:1:effect:0");
}

#[test]
fn abi_v1_is_rejected_with_a_structured_fault() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let mut input = correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    );
    input.version = 1;

    let step = runtime.step(input);

    assert!(step.actions.is_empty());
    assert!(step.observations.is_empty());
    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::VersionMismatch,
            ..
        }]
    ));
}

#[test]
fn exact_event_replay_returns_the_original_step() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let input = correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    );

    let first = runtime.step(input.clone());
    let replay = runtime.step(input);

    assert_eq!(
        serde_json::to_value(replay).unwrap(),
        serde_json::to_value(first).unwrap()
    );
}

#[test]
fn duplicate_event_id_with_different_payload_is_rejected() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::SetMemoryEnabled { enabled: true },
    ));

    let step = runtime.step(correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::SetMemoryEnabled { enabled: false },
    ));

    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::DuplicateEventConflict,
            ..
        }]
    ));
}

#[test]
fn cross_operation_input_is_rejected() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::SetMemoryEnabled { enabled: true },
    ));

    let step = runtime.step(correlated_input(
        "op-2",
        "event-2",
        43,
        KernelInputEvent::SetMemoryEnabled { enabled: false },
    ));

    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::OperationMismatch,
            ..
        }]
    ));
}

#[test]
fn unknown_effect_result_is_rejected() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(correlated_input(
        "op-1",
        "event-0",
        41,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    ));
    let step = runtime.step(correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::ProviderResult {
            effect_id: "missing-effect".to_string(),
            message: Message::assistant("unexpected"),
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    ));

    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::UnexpectedEffectResult,
            effect_id: Some(effect_id),
            ..
        }] if effect_id == "missing-effect"
    ));
}

#[test]
fn rejected_effect_result_does_not_bind_the_operation() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(correlated_input(
        "bad-op",
        "bad-event",
        42,
        KernelInputEvent::ProviderResult {
            effect_id: "missing-effect".to_string(),
            message: Message::assistant("unexpected"),
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    ));

    let step = runtime.step(correlated_input(
        "good-op",
        "good-event",
        43,
        KernelInputEvent::SetMemoryEnabled { enabled: true },
    ));

    assert!(step.faults.is_empty());
    assert_eq!(step.operation_id, "good-op");
}

#[test]
fn event_replay_dedupe_has_a_fixed_capacity() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    for index in 0..(EVENT_REPLAY_WINDOW_CAPACITY + 1) {
        runtime.step(correlated_input(
            "op-1",
            &format!("event-{index}"),
            index as u64,
            KernelInputEvent::SetMemoryEnabled {
                enabled: index % 2 == 0,
            },
        ));
    }

    assert_eq!(runtime.recorded_event_count(), EVENT_REPLAY_WINDOW_CAPACITY);
}

#[test]
fn provider_result_before_start_is_an_invalid_lifecycle_fault() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::ProviderResult {
            effect_id: "not-yet-issued".to_string(),
            message: Message::assistant("too early"),
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    ));

    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidLifecycle,
            ..
        }]
    ));
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Created);
}

#[test]
fn configure_run_validates_before_applying_any_field() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::ConfigureRun {
            config: RunConfig {
                memory_enabled: Some(true),
                knowledge_budget_ratio: Some(1.5),
                ..RunConfig::default()
            },
        },
    ));

    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidConfig,
            ..
        }]
    ));
    assert!(!runtime.state_machine().ctx.memory_enabled);
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Created);
}

#[test]
fn configure_then_start_advances_the_explicit_lifecycle() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Created);

    runtime.step(correlated_input(
        "op-1",
        "event-1",
        42,
        KernelInputEvent::ConfigureRun {
            config: RunConfig::default(),
        },
    ));
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Configured);

    runtime.step(correlated_input(
        "op-1",
        "event-2",
        43,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    ));
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Running);
}

#[test]
fn terminal_lifecycle_rejects_business_mutation() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
        enabled: true,
    }));
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("test"),
        run_spec: None,
    }));
    runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: runtime.pending_provider_effect_id(),
        message: Message::assistant("done"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        now_ms: None,
        stop_reason: None,
    }));
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Completed);

    let step = runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
        enabled: false,
    }));

    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidLifecycle,
            ..
        }]
    ));
    assert!(runtime.state_machine().ctx.memory_enabled);
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Completed);
}

#[test]
fn start_run_returns_versioned_provider_action() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("ship it"),
        run_spec: None,
    }));

    assert_eq!(step.version, KERNEL_ABI_VERSION);
    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::CallProvider { .. },
            ..
        }]
    ));
}

#[test]
fn provider_text_response_returns_done() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("ship it"),
        run_spec: None,
    }));
    let step = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: runtime.pending_provider_effect_id(),
        message: Message::assistant("done"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        stop_reason: None,
        now_ms: None,
    }));

    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::Done { .. },
            ..
        }]
    ));
}

#[test]
fn config_inputs_mutate_runtime_without_actions() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(KernelInput::new(KernelInputEvent::SetTools {
        tools: vec![ToolSchema {
            name: "echo".into(),
            description: "Echo input".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }],
    }));

    assert!(step.actions.is_empty());
    assert_eq!(runtime.state_machine().tools.len(), 1);
}

#[test]
fn skill_activated_input_records_active_skill() {
    // P1-B B1: the SkillActivated event (serde `skill_activated`) records the active skill and,
    // via the catalog's declared tools, yields a narrowing filter — without itself acting.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let mut debug = SkillMetadata::new("debug", "Debug helper");
    debug.allowed_tools = vec!["read".into(), "grep".into()];
    runtime.step(KernelInput::new(KernelInputEvent::SetAvailableSkills {
        skills: vec![debug],
    }));

    let step = runtime.step(KernelInput::new(KernelInputEvent::SkillActivated {
        name: "debug".to_string(),
        lease_turns: None,
    }));

    assert!(
        step.actions.is_empty(),
        "activation is config, not an action"
    );
    assert!(
        runtime
            .state_machine()
            .ctx
            .active_skills
            .contains_key("debug")
    );
    let filter = runtime
        .state_machine()
        .ctx
        .active_skill_tool_filter()
        .unwrap();
    assert_eq!(filter.len(), 2);
}

#[test]
fn skill_deactivated_rewidens_toolset_and_unpins_knowledge() {
    // K3: deactivation removes the skill from the active set (filter back to None ⇒ no
    // narrowing) and marks its `skill:<name>` knowledge pin for the next boundary sweep.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let mut debug = SkillMetadata::new("debug", "Debug helper");
    debug.allowed_tools = vec!["read".into(), "grep".into()];
    runtime.step(KernelInput::new(KernelInputEvent::SetAvailableSkills {
        skills: vec![debug],
    }));
    runtime.step(KernelInput::new(KernelInputEvent::SkillActivated {
        name: "debug".to_string(),
        lease_turns: None,
    }));
    runtime.step(KernelInput::new(KernelInputEvent::AddKnowledgeMessage {
        content: "debug skill content".to_string(),
        tokens: 5,
        key: Some("skill:debug".to_string()),
        pinned: false,
    }));
    assert!(
        runtime
            .state_machine()
            .ctx
            .active_skill_tool_filter()
            .is_some()
    );

    runtime.step(KernelInput::new(KernelInputEvent::SkillDeactivated {
        name: "debug".to_string(),
    }));
    let sm = runtime.state_machine();
    assert!(!sm.ctx.active_skills.contains_key("debug"));
    assert!(
        sm.ctx.active_skill_tool_filter().is_none(),
        "toolset re-widens"
    );
    assert!(
        sm.ctx.partitions.knowledge.entries[0].evict_at_boundary,
        "knowledge pin marked for the boundary sweep"
    );
}

#[test]
fn skill_lease_expires_after_turns_and_reactivation_renarrows() {
    // K3: `lease_turns: 1` expires once the turn counter passes activation+1 — the head-of-
    // event sweep deactivates it exactly like an explicit SkillDeactivated. A later
    // re-activation re-narrows.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let mut debug = SkillMetadata::new("debug", "Debug helper");
    debug.allowed_tools = vec!["read".into(), "grep".into()];
    runtime.step(KernelInput::new(KernelInputEvent::SetAvailableSkills {
        skills: vec![debug],
    }));
    runtime.step(KernelInput::new(KernelInputEvent::SkillActivated {
        name: "debug".to_string(),
        lease_turns: Some(1),
    }));
    assert!(
        runtime
            .state_machine()
            .ctx
            .active_skill_tool_filter()
            .is_some()
    );

    // One full tool round advances the turn; the sweep runs at the HEAD of the next loop
    // event, so the following provider turn is what actually expires the lease.
    run_with_tool_call(&mut runtime, "read");
    runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: vec![ToolResult {
            call_id: "call-1".into(),
            output: crate::types::message::Content::Text("ok".into()),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    }));
    runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: runtime.pending_provider_effect_id(),
        message: assistant_calling("read"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        stop_reason: None,
        now_ms: None,
    }));
    assert!(
        !runtime
            .state_machine()
            .ctx
            .active_skills
            .contains_key("debug"),
        "lease expired after the turn advanced"
    );
    assert!(
        runtime
            .state_machine()
            .ctx
            .active_skill_tool_filter()
            .is_none()
    );

    // Re-activation works and re-narrows.
    runtime.step(KernelInput::new(KernelInputEvent::SkillActivated {
        name: "debug".to_string(),
        lease_turns: None,
    }));
    assert!(
        runtime
            .state_machine()
            .ctx
            .active_skill_tool_filter()
            .is_some()
    );
}

#[test]
fn update_task_input_mutates_task_state() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(KernelInput::new(KernelInputEvent::UpdateTask {
        update: TaskUpdate {
            progress: Some("tools executed".to_string()),
            ..Default::default()
        },
    }));

    assert!(step.actions.is_empty());
    assert_eq!(
        runtime.state_machine().ctx.partitions.task_state.progress,
        "tools executed"
    );
}

#[test]
fn add_knowledge_message_enters_knowledge_partition() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(KernelInput::new(KernelInputEvent::AddKnowledgeMessage {
        content: "skill: debug".to_string(),
        tokens: 10,
        key: None,
        pinned: false,
    }));

    assert!(step.actions.is_empty());
    assert_eq!(runtime.state_machine().ctx.partitions.knowledge.len(), 1);
}

#[test]
fn knowledge_budget_exceeded_observed_in_live_loop() {
    // K2: the per-turn budget check runs in the LLMResponse path; an over-budget knowledge
    // partition (40 tokens > 100 × 0.25 default) surfaces as a KnowledgeBudgetExceeded
    // observation. Raising the ratio via SetKnowledgeBudget silences it.
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_tokens: 100,
        ..SchedulerBudget::default()
    });
    runtime.step(KernelInput::new(KernelInputEvent::AddKnowledgeMessage {
        content: "reference".to_string(),
        tokens: 40,
        key: None,
        pinned: false,
    }));
    run_with_tool_call(&mut runtime, "search");
    // The check runs at the turn boundary (ToolResults handling), not on the proposal.
    let step = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: vec![ToolResult {
            call_id: "call-1".into(),
            output: crate::types::message::Content::Text("ok".into()),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    }));
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::KnowledgeBudgetExceeded {
            used: 40,
            budget: 25,
            ..
        }
    )));

    // Runtime knob: a generous ratio ⇒ under budget ⇒ no warning on the next turn.
    let mut runtime2 = KernelRuntime::new(SchedulerBudget {
        max_tokens: 100,
        ..SchedulerBudget::default()
    });
    runtime2.step(KernelInput::new(KernelInputEvent::SetKnowledgeBudget {
        ratio: 0.9,
    }));
    runtime2.step(KernelInput::new(KernelInputEvent::AddKnowledgeMessage {
        content: "reference".to_string(),
        tokens: 40,
        key: None,
        pinned: false,
    }));
    run_with_tool_call(&mut runtime2, "search");
    let step2 = runtime2.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime2.pending_tool_effect_id(),
        results: vec![ToolResult {
            call_id: "call-1".into(),
            output: crate::types::message::Content::Text("ok".into()),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    }));
    assert!(
        !step2
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::KnowledgeBudgetExceeded { .. }))
    );
}

#[test]
fn keyed_add_knowledge_dedupes_and_remove_marks() {
    // K1 event-level: a same-key AddKnowledgeMessage stages an upsert (one entry, original
    // bytes rendered mid-generation); RemoveKnowledge marks for the boundary sweep. Both
    // decoded from the serde wire shape SDKs feed.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    for content in ["v1", "v2"] {
        let ev: KernelInputEvent = serde_json::from_value(serde_json::json!({
            "kind": "add_knowledge_message",
            "content": content,
            "tokens": 5,
            "key": "skill:debug",
        }))
        .unwrap();
        runtime.step(KernelInput::new(ev));
    }
    let knowledge = &runtime.state_machine().ctx.partitions.knowledge;
    assert_eq!(knowledge.len(), 1);
    assert_eq!(
        knowledge
            .messages()
            .next()
            .and_then(|m| m.content.as_text()),
        Some("v1"),
        "upsert deferred to boundary — original bytes still rendered"
    );

    let ev: KernelInputEvent = serde_json::from_value(serde_json::json!({
        "kind": "remove_knowledge",
        "key": "skill:debug",
    }))
    .unwrap();
    runtime.step(KernelInput::new(ev));
    assert!(runtime.state_machine().ctx.partitions.knowledge.entries[0].evict_at_boundary);
}

#[test]
fn capability_mount_emits_observation() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(KernelInput::new(KernelInputEvent::MountCapability {
        capability: CapabilityDescriptor::marker(
            CapabilityKind::McpServer,
            "docs",
            "Documentation server",
        ),
    }));

    assert!(step.actions.is_empty());
    assert!(matches!(
        step.observations.as_slice(),
        [KernelObservation::CapabilityChanged { .. }]
    ));
}

#[test]
fn spawn_sub_agent_input_registers_process() {
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("worker", "worker-session"),
        AgentRole::Implement,
        "do work",
    );
    let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
        spec,
        parent_session_id: "parent-session".to_string(),
    }));

    assert!(step.actions.is_empty());
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::AgentProcessChanged {
            agent_id,
            parent_session_id,
            state,
            ..
        } if agent_id == "worker" && parent_session_id == "parent-session" && state == "running"
    )));
    assert_eq!(
        runtime
            .state_machine()
            .agent_process("worker")
            .expect("process")
            .parent_session_id
            .as_str(),
        "parent-session"
    );
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::Suspended { reason, .. } if reason == "sub_agent_await"
    )));
    assert!(runtime.state_machine().is_suspended());
    assert!(matches!(
        runtime.state_machine().wait_reason(),
        Some(crate::scheduler::tcb::WaitReason::SubAgentJoin(_))
    ));
}

#[test]
fn set_resource_quota_input_denies_spawn_over_quota() {
    use crate::governance::quota::ResourceQuota;
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    // Quota flows in through the same versioned JSON event ABI as governance/scheduler config.
    let step = runtime.step(KernelInput::new(KernelInputEvent::SetResourceQuota {
        quota: ResourceQuota {
            max_spawn_depth: Some(0),
            ..ResourceQuota::default()
        },
    }));
    assert!(step.actions.is_empty(), "config input yields no actions");

    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("worker", "worker-session"),
        AgentRole::Implement,
        "do work",
    );
    let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
        spec,
        parent_session_id: "parent-session".to_string(),
    }));

    // Denied spawn rolls the turn back to another reasoning pass — no process registered,
    // not suspended on a sub-agent join.
    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::CallProvider { .. },
            ..
        }]
    ));
    assert!(!step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::AgentProcessChanged { agent_id, .. } if agent_id == "worker"
    )));
    assert!(runtime.state_machine().agent_process("worker").is_none());
    assert!(!runtime.state_machine().is_suspended());
}

#[test]
fn group_budget_base_enforces_shared_token_cap() {
    use crate::types::message::{Content, Message, ToolCall, ToolResult};

    // Drive one tool-calling turn under a 100-token run cap, with `group_base` already spent by
    // other members of the governance domain. The token-budget axis is checked after the tool
    // results, against `group_base + local`.
    fn run_one_turn(group_base: Option<u64>) -> KernelStep {
        let mut runtime = KernelRuntime::new(SchedulerBudget {
            max_total_tokens: 100,
            ..SchedulerBudget::default()
        });
        runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
            config: RunConfig {
                group_tokens_base: group_base,
                ..RunConfig::default()
            },
        }));
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("task"),
            run_spec: None,
        }));
        let mut msg = Message::assistant("");
        msg.token_count = Some(10); // this vehicle's local spend this turn
        msg.tool_calls.push(ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({}),
        });
        runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
            effect_id: runtime.pending_provider_effect_id(),
            message: msg,
            observed_input_tokens: None,
            observed_output_tokens: None,
            stop_reason: None,
            now_ms: None,
        }));
        runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
            effect_id: runtime.pending_tool_effect_id(),
            results: vec![ToolResult {
                call_id: "c1".into(),
                output: Content::Text("ok".into()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            }],
        }))
    }

    let exceeded = |step: &KernelStep| {
        step.observations.iter().any(|o| {
                matches!(o, KernelObservation::BudgetExceeded { budget, .. } if budget == "token_budget")
            })
    };

    // Group already spent 95; this vehicle's 10 pushes the domain to 105 > 100 → shared cap fires.
    assert!(
        exceeded(&run_one_turn(Some(95))),
        "group token budget must span the whole domain"
    );
    // N=1 / no group (base 0): local 10 is far under the cap → pre-L1 behavior unchanged.
    assert!(
        !exceeded(&run_one_turn(None)),
        "no group seed ⇒ per-vehicle budget, well under cap"
    );
}

#[test]
fn group_spawns_base_enforces_cumulative_spawn_cap() {
    use crate::governance::quota::ResourceQuota;
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

    // Cumulative cap of 2 sub-agents across the domain. Other members already spawned 2 (seeded),
    // so this vehicle's very first spawn is denied — the cap spans the whole group.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            resource_quota: Some(ResourceQuota {
                max_total_subagents: Some(2),
                ..ResourceQuota::default()
            }),
            group_spawns_base: Some(2),
            ..RunConfig::default()
        },
    }));
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("worker", "worker-session"),
        AgentRole::Implement,
        "do work",
    );
    let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
        spec,
        parent_session_id: "parent-session".to_string(),
    }));

    // Denied: domain already at the cumulative cap → rolled back, no process registered.
    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::CallProvider { .. },
            ..
        }]
    ));
    assert!(runtime.state_machine().agent_process("worker").is_none());
    assert_eq!(runtime.local_subagents_spawned(), 0);
}

#[test]
fn default_runtime_leaves_spawn_unquota_ed() {
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

    // No SetResourceQuota event => pre-M2 behavior: spawn is unconditionally admitted.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("worker", "worker-session"),
        AgentRole::Implement,
        "do work",
    );
    runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
        spec,
        parent_session_id: "parent-session".to_string(),
    }));
    assert!(runtime.state_machine().agent_process("worker").is_some());
    assert!(runtime.state_machine().is_suspended());
}

/// Wire-format lock for `agent_process_changed` multi-word enum values. The kernel stringifies
/// `isolation`/`context_inheritance` as debug-lowercase (`readonly`/`systemonly`), which is NOT
/// the same as serde snake_case (`read_only`/`system_only`) — and no golden fixture covers these
/// variants. This pins the current bytes so the observation refactor cannot silently change them.
#[test]
fn agent_process_changed_locks_multiword_wire_form() {
    use crate::types::agent::{AgentIdentity, AgentIsolation, AgentRole, AgentRunSpec};

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    // Verify role → SystemOnly inheritance; explicit ReadOnly isolation. Both are multi-word.
    let spec = AgentRunSpec::new(
        AgentIdentity::sub_agent("worker", "worker-session"),
        AgentRole::Verify,
        "do work",
    )
    .with_isolation(AgentIsolation::ReadOnly);
    let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
        spec,
        parent_session_id: "parent-session".to_string(),
    }));

    let obs = step
        .observations
        .iter()
        .find(|o| matches!(o, KernelObservation::AgentProcessChanged { .. }))
        .expect("agent_process_changed observation");
    let json = serde_json::to_value(obs).unwrap();
    assert_eq!(
        json["isolation"], "readonly",
        "isolation must stay debug-lowercase"
    );
    assert_eq!(
        json["context_inheritance"], "systemonly",
        "context_inheritance must stay debug-lowercase"
    );
    assert_eq!(json["role"], "verify");
    assert_eq!(json["state"], "running");
}

// ── M-memory-policy: set_memory_policy is enforced at the WriteMemory / QueryMemory traps ──

fn write_memory(runtime: &mut KernelRuntime, name: &str, content: &str) -> KernelStep {
    use crate::mm::memory::{MemoryMetadata, MemoryWriteRequest};
    runtime.step(KernelInput::new(KernelInputEvent::WriteMemory {
        memory: MemoryWriteRequest {
            metadata: MemoryMetadata {
                name: name.to_string(),
                description: "desc".to_string(),
                ..Default::default()
            },
            content: content.to_string(),
        },
    }))
}

#[test]
fn memory_policy_validation_disabled_admits_forbidden_write() {
    // "代码模式:" is a forbidden pattern under default validation; disabling validation admits it.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryPolicy {
        memory_path: String::new(),
        stale_warning_days: 2,
        retrieval_top_k: 5,
        validation_enabled: false,
        max_content_bytes: None,
        max_name_length: None,
    }));
    let step = write_memory(&mut runtime, "note", "代码模式: foo");
    assert!(
        step.observations
            .iter()
            .any(|o| matches!(o, KernelObservation::MemoryWritten { .. }))
    );
    assert!(
        !step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::MemoryValidationFailed { .. }))
    );
}

#[test]
fn default_runtime_accepts_content_hosts_have_not_forbidden() {
    // P13: no baked-in forbidden patterns — with no policy installed, a structurally
    // valid write passes; content judgment belongs to hosts/models, not the kernel.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = write_memory(&mut runtime, "note", "代码模式: foo");
    assert!(
        step.observations
            .iter()
            .any(|o| matches!(o, KernelObservation::MemoryWritten { .. }))
    );
}

#[test]
fn memory_policy_size_override_rejects_oversized_write() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryPolicy {
        memory_path: String::new(),
        stale_warning_days: 2,
        retrieval_top_k: 5,
        validation_enabled: true,
        max_content_bytes: Some(8),
        max_name_length: None,
    }));
    let step = write_memory(
        &mut runtime,
        "note",
        "this content is well over eight bytes",
    );
    let failed = step.observations.iter().find_map(|o| match o {
        KernelObservation::MemoryValidationFailed { error, .. } => Some(error.clone()),
        _ => None,
    });
    assert!(failed.is_some_and(|e| e.contains("too large")));
}

#[test]
fn memory_policy_clamps_retrieval_top_k() {
    use crate::mm::memory::MemoryQuery;
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryPolicy {
        memory_path: String::new(),
        stale_warning_days: 2,
        retrieval_top_k: 3,
        validation_enabled: true,
        max_content_bytes: None,
        max_name_length: None,
    }));
    let step = runtime.step(KernelInput::new(KernelInputEvent::QueryMemory {
        query: MemoryQuery {
            top_k: 50,
            ..Default::default()
        },
    }));
    let requested = step.observations.iter().find_map(|o| match o {
        KernelObservation::MemoryQueried { requested_k, .. } => Some(*requested_k),
        _ => None,
    });
    assert_eq!(requested, Some(3));
}

#[test]
fn default_runtime_uses_requested_top_k_verbatim() {
    use crate::mm::memory::MemoryQuery;
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(KernelInput::new(KernelInputEvent::QueryMemory {
        query: MemoryQuery {
            top_k: 50,
            ..Default::default()
        },
    }));
    let requested = step.observations.iter().find_map(|o| match o {
        KernelObservation::MemoryQueried { requested_k, .. } => Some(*requested_k),
        _ => None,
    });
    assert_eq!(requested, Some(50));
}

#[test]
fn provider_result_now_ms_drives_wall_time_budget() {
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_wall_ms: Some(10),
        ..SchedulerBudget::default()
    });
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("ship it"),
        run_spec: None,
    }));
    let mut msg = Message::assistant("");
    msg.tool_calls.push(ToolCall {
        id: "call-1".into(),
        name: "echo".into(),
        arguments: serde_json::json!({}),
    });
    runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: runtime.pending_provider_effect_id(),
        message: msg,
        observed_input_tokens: None,
        observed_output_tokens: None,
        stop_reason: None,
        now_ms: Some(100),
    }));
    let step = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: vec![ToolResult {
            call_id: "call-1".into(),
            output: crate::types::message::Content::Text("ok".into()),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    }));

    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction { effect: KernelEffect::CallProvider { tools, .. }, .. }] if tools.is_empty()
    ));
}

// ─── Governance gate ───────────────────────────────────────────────────

fn assistant_calling(tool: &str) -> Message {
    let mut msg = Message::assistant("");
    msg.tool_calls.push(ToolCall {
        id: "call-1".into(),
        name: tool.into(),
        arguments: serde_json::json!({}),
    });
    msg
}

/// Feed a tool-calling response and return the resulting step.
fn run_with_tool_call(runtime: &mut KernelRuntime, tool: &str) -> KernelStep {
    run_with_tool_call_named(runtime, tool, "call-1")
}

fn run_with_tool_call_named(runtime: &mut KernelRuntime, tool: &str, _call_id: &str) -> KernelStep {
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("do the thing"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();
    runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: runtime.pending_provider_effect_id(),
        message: assistant_calling(tool),
        observed_input_tokens: None,
        observed_output_tokens: None,
        stop_reason: None,
        now_ms: None,
    }))
}

#[test]
fn governance_deny_blocks_tool_and_reprompts() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
        default_action: Some(PolicyAction::Allow),
        rules: vec![PolicyRule {
            tool_pattern: "danger.*".to_string(),
            action: PolicyAction::Deny,
        }],
        vetoed_tools: vec![],
        rate_limits: vec![],
        constraints: vec![],
    }));

    let step = run_with_tool_call(&mut runtime, "danger.delete");

    // Denied call must NOT reach ExecuteTool; the turn rolls back and re-prompts.
    assert!(
        matches!(
            step.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::CallProvider { .. },
                ..
            }]
        ),
        "denied tool should roll back and re-call provider, got {:?}",
        step.actions
    );
    assert!(
        step.observations
            .iter()
            .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
        "expected a Rollbacked observation for the denied turn",
    );
}

#[test]
fn configure_run_bundle_applies_governance_equivalently_to_load_governance_policy() {
    // K2: the consolidated `ConfigureRun` bundle must apply governance identically to the granular
    // `LoadGovernancePolicy` event — a deny rule blocks the matching tool and re-prompts.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            tools: Some(vec![]),
            governance: Some(GovernanceConfig {
                default_action: Some(PolicyAction::Allow),
                rules: vec![PolicyRule {
                    tool_pattern: "danger.*".to_string(),
                    action: PolicyAction::Deny,
                }],
                ..GovernanceConfig::default()
            }),
            attention_max_queue_size: Some(32),
            ..RunConfig::default()
        },
    }));

    let step = run_with_tool_call(&mut runtime, "danger.delete");

    assert!(
        matches!(
            step.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::CallProvider { .. },
                ..
            }]
        ),
        "bundle-configured deny should roll back and re-call provider, got {:?}",
        step.actions
    );
    assert!(
        step.observations
            .iter()
            .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
        "expected a Rollbacked observation for the bundle-denied turn",
    );
}

#[test]
fn configure_run_entropy_watch_flows_through_the_abi() {
    // The `entropy_watch` bundle field arms the watch; a completed tool round then
    // surfaces BOTH the unconditional sample and (threshold 0 + all-error results) the alert.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            entropy_watch: Some(crate::scheduler::entropy::EntropyWatchConfig {
                enabled: true,
                threshold: 0.1,
                hysteresis: 0.05,
                cooldown_turns: 0,
                notify_model: false,
            }),
            ..RunConfig::default()
        },
    }));
    run_with_tool_call(&mut runtime, "step");
    let step = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: vec![ToolResult {
            call_id: "call-1".into(),
            output: crate::types::message::Content::Text("boom".into()),
            is_error: true,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    }));
    assert!(
            step.observations
                .iter()
                .any(|o| matches!(o, KernelObservation::EntropySample { failure_rate, .. } if *failure_rate > 0.9)),
            "completed boundary must carry the entropy sample: {:?}",
            step.observations
        );
    assert!(
            step.observations
                .iter()
                .any(|o| matches!(o, KernelObservation::EntropyAlert { threshold, .. } if (*threshold - 0.1).abs() < 1e-9)),
            "armed watch + errored turn must alert: {:?}",
            step.observations
        );
}

#[test]
fn set_entropy_watch_event_parses_from_json_and_applies_partially() {
    // The granular event round-trips inside the required v2 envelope while absent
    // event fields keep their current values (mirrors SetRepeatFuse).
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let input: KernelInput = serde_json::from_str(
        r#"{"version":2,"operation_id":"op-entropy","event_id":"event-entropy-1","observed_at_ms":42,"event":{"kind":"set_entropy_watch","enabled":true,"threshold":0.4}}"#,
    )
    .expect("granular event must deserialize");
    runtime.step(input);
    let cfg = runtime.state_machine().entropy_watch_config();
    assert!(cfg.enabled);
    assert!((cfg.threshold - 0.4).abs() < 1e-9);
    assert!(
        (cfg.hysteresis - 0.1).abs() < 1e-9,
        "absent field keeps the default"
    );
    assert_eq!(cfg.cooldown_turns, 4, "absent field keeps the default");
}

#[test]
fn configure_run_round_trips_over_the_abi() {
    // The bundle must survive the versioned JSON ABI (replayable / session-loggable) like every
    // other host event.
    let event = KernelInputEvent::ConfigureRun {
        config: RunConfig {
            resource_quota: Some(crate::governance::quota::ResourceQuota {
                max_concurrent_subagents: Some(2),
                ..Default::default()
            }),
            scheduler_max_wall_ms: Some(60_000),
            plan_tool_enabled: Some(true),
            ..RunConfig::default()
        },
    };
    let json = serde_json::to_string(&event).expect("serialize");
    let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(parsed, KernelInputEvent::ConfigureRun { .. }));
}

#[test]
fn governance_ask_user_suspends_until_resume() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
        default_action: Some(PolicyAction::Allow),
        rules: vec![PolicyRule {
            tool_pattern: "sensitive.*".to_string(),
            action: PolicyAction::AskUser,
        }],
        vetoed_tools: vec![],
        rate_limits: vec![],
        constraints: vec![],
    }));

    let step = run_with_tool_call(&mut runtime, "sensitive.read");

    assert!(
        step.actions.is_empty(),
        "AskUser should suspend without ExecuteTool, got {:?}",
        step.actions
    );
    assert!(
        step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::ToolGated { tool, .. } if tool == "sensitive.read"
        )),
        "expected a ToolGated observation for the AskUser call",
    );
    assert!(
        step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::Suspended { reason, .. } if reason == "ask_user"
        )),
        "expected a Suspended observation",
    );
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Suspended);

    let resumed = runtime.step(KernelInput::new(KernelInputEvent::Resume {
        approved_calls: vec!["call-1".to_string()],
        denied_calls: vec![],
    }));
    assert!(
        matches!(
            resumed.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::ExecuteTool { .. },
                ..
            }]
        ),
        "resume with approval should emit ExecuteTool, got {:?}",
        resumed.actions
    );
    assert!(resumed.observations.iter().any(|o| matches!(
        o,
        KernelObservation::Resumed { approved, denied, .. }
        if approved == &["call-1"] && denied.is_empty()
    )),);
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Running);
}

#[test]
fn governance_ask_user_resume_all_denied_feeds_tool_results() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
        default_action: Some(PolicyAction::Allow),
        rules: vec![PolicyRule {
            tool_pattern: "sensitive.*".to_string(),
            action: PolicyAction::AskUser,
        }],
        vetoed_tools: vec![],
        rate_limits: vec![],
        constraints: vec![],
    }));
    run_with_tool_call(&mut runtime, "sensitive.read");
    runtime.state_machine_mut().take_observations();

    let step = runtime.step(KernelInput::new(KernelInputEvent::Resume {
        approved_calls: vec![],
        denied_calls: vec!["call-1".to_string()],
    }));
    assert!(
        matches!(
            step.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::CallProvider { .. },
                ..
            }]
        ),
        "all denied should re-prompt provider, got {:?}",
        step.actions
    );
}

#[test]
fn no_governance_policy_executes_all_tools() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = run_with_tool_call(&mut runtime, "danger.delete");

    // Without a policy the gate is a no-op — behavior is unchanged.
    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::ExecuteTool { .. },
            ..
        }]
    ));
    assert!(
        !step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::ToolGated { .. })),
    );
}

fn tool_ok(call_id: &str) -> ToolResult {
    ToolResult {
        call_id: call_id.into(),
        output: crate::types::message::Content::Text("ok".to_string()),
        is_error: false,
        is_fatal: false,
        error_kind: None,
        token_count: None,
    }
}

#[test]
fn governance_rate_limit_blocks_second_call() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
        default_action: Some(PolicyAction::Allow),
        rules: vec![],
        vetoed_tools: vec![],
        rate_limits: vec![RateLimitSpec {
            tool: "fetch".to_string(),
            max_calls: 1,
            window_ms: 60_000,
        }],
        constraints: vec![],
    }));
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("fetch twice"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    // First call within the window — allowed.
    let s1 = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: runtime.pending_provider_effect_id(),
        message: assistant_calling("fetch"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        stop_reason: None,
        now_ms: Some(1_000),
    }));
    assert!(
        matches!(
            s1.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::ExecuteTool { .. },
                ..
            }]
        ),
        "first call should execute, got {:?}",
        s1.actions
    );

    // Close the turn so the kernel re-prompts the provider.
    runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: vec![tool_ok("call-1")],
    }));
    runtime.state_machine_mut().take_observations();

    // Second call to the same tool within the window — rate limited → rollback.
    let s2 = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: runtime.pending_provider_effect_id(),
        message: assistant_calling("fetch"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        stop_reason: None,
        now_ms: Some(1_001),
    }));
    assert!(
        matches!(
            s2.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::CallProvider { .. },
                ..
            }]
        ),
        "rate-limited call should roll back and re-call provider, got {:?}",
        s2.actions
    );
    assert!(
        s2.observations
            .iter()
            .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
        "expected a Rollbacked observation for the rate-limited turn",
    );
}

#[test]
fn governance_constraint_required_param_denies() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
        default_action: Some(PolicyAction::Allow),
        rules: vec![],
        vetoed_tools: vec![],
        rate_limits: vec![],
        constraints: vec![ConstraintSpec::Required {
            tool: "write".to_string(),
            path: "path".to_string(),
        }],
    }));

    // assistant_calling emits empty args `{}` → required "path" is missing → deny.
    let step = run_with_tool_call(&mut runtime, "write");
    assert!(
        matches!(
            step.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::CallProvider { .. },
                ..
            }]
        ),
        "missing required param should roll back, got {:?}",
        step.actions
    );
    assert!(
        step.observations
            .iter()
            .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
        "expected a Rollbacked observation for the constraint violation",
    );
}

// ─── In-kernel signal routing (attention policy) ────────────────────────

fn signal(
    urgency: crate::types::signal::Urgency,
    summary: &str,
) -> crate::types::signal::RuntimeSignal {
    use crate::types::signal::{RuntimeSignal, SignalSource, SignalType};
    RuntimeSignal::new(SignalSource::Gateway, SignalType::Alert, urgency, summary)
}

fn started_runtime_with_attention(max_queue: u32) -> KernelRuntime {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetAttentionPolicy {
        max_queue_size: max_queue,
    }));
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("watch for signals"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();
    runtime
}

#[test]
fn attention_policy_critical_signal_interrupts() {
    use crate::types::signal::Urgency;
    let mut runtime = started_runtime_with_attention(8);
    let step = runtime.step(KernelInput::new(KernelInputEvent::Signal {
        signal: signal(Urgency::Critical, "fire"),
    }));
    assert!(
        matches!(
            step.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::CallProvider { .. },
                ..
            }]
        ),
        "critical signal should drive a provider call, got {:?}",
        step.actions
    );
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::SignalDisposed { disposition, .. } if disposition == "interrupt_now"
    )));
}

#[test]
fn attention_policy_normal_signal_queues_without_action() {
    use crate::types::signal::Urgency;
    let mut runtime = started_runtime_with_attention(8);
    let step = runtime.step(KernelInput::new(KernelInputEvent::Signal {
        signal: signal(Urgency::Normal, "job"),
    }));
    assert!(
        step.actions.is_empty(),
        "normal signal should queue without a provider call, got {:?}",
        step.actions
    );
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::SignalDisposed { disposition, queue_depth, .. }
        if disposition == "queue" && *queue_depth == 1
    )));
}

#[test]
fn attention_policy_full_queue_drops() {
    use crate::types::signal::Urgency;
    let mut runtime = started_runtime_with_attention(1);
    runtime.step(KernelInput::new(KernelInputEvent::Signal {
        signal: signal(Urgency::Normal, "first"),
    }));
    let step = runtime.step(KernelInput::new(KernelInputEvent::Signal {
        signal: signal(Urgency::Normal, "second"),
    }));
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::SignalDisposed { disposition, .. } if disposition == "dropped"
    )));
}

#[test]
fn page_in_populates_knowledge_partition() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
        enabled: true,
    }));
    let before = runtime.state_machine().ctx.partitions.knowledge.len();
    runtime.step(KernelInput::new(KernelInputEvent::PageIn {
        entries: vec![crate::mm::PageInEntry {
            content: "[memory] prior fix".to_string(),
            tokens: Some(10),
            source: Some("memory".to_string()),
            key: None,
            pinned: false,
        }],
    }));
    let after = runtime.state_machine().ctx.partitions.knowledge.len();
    assert!(after > before, "page-in should add knowledge messages");
}

#[test]
fn memory_tool_does_not_emit_page_in_requested() {
    // The automatic PageInRequested producer for live memory/knowledge tool calls was retired:
    // a memory-tool result now flows to `history` via the normal tool-result path only, so it
    // decays with the compression pyramid instead of living forever in `knowledge`.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
        enabled: true,
    }));

    let _step = run_with_tool_call(&mut runtime, "memory");
    // (the PageInRequested observation itself was deleted with its retired producer)
}

#[test]
fn load_workflow_input_drives_dag_to_completion() {
    use crate::orchestration::workflow::fanout_synthesize;
    use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    // Exercise the full serde round-trip of LoadWorkflow + WorkflowSpec over the ABI.
    let spec = fanout_synthesize(
        vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
        RuntimeTask::new("synth"),
    );
    let event = KernelInputEvent::LoadWorkflow {
        spec,
        parent_session_id: "sess".to_string(),
        resumed_completed: Vec::new(),
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_results: Vec::new(),
    };
    let json = serde_json::to_string(&event).expect("serialize");
    let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");

    let step = runtime.step(KernelInput::new(parsed));
    // First batch carries both workers' goals so the SDK can run them.
    let batch = step
        .observations
        .iter()
        .find_map(|o| match o {
            KernelObservation::WorkflowBatchSpawned { nodes, .. } => Some(nodes.clone()),
            _ => None,
        })
        .expect("workflow_batch_spawned");
    assert_eq!(batch.len(), 2);
    let goals: Vec<&str> = batch.iter().map(|n| n.goal.as_str()).collect();
    assert!(goals.contains(&"w0") && goals.contains(&"w1"));
    assert_eq!(batch[0].agent_id, "wf-node0");
    assert_eq!(batch[0].isolation, "read_only"); // fanout workers are Explore → read_only

    let complete = |runtime: &mut KernelRuntime, id: &str| {
        runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
            result: SubAgentResult {
                agent_id: compact_str::CompactString::new(id),
                result: LoopResult {
                    termination: TerminationReason::Completed,
                    final_message: None,
                    turns_used: 1,
                    total_tokens_used: 1,
                    loop_continue: None,
                    classify_branch: None,
                    tournament_winner: None,
                    pace_decision: None,
                },
            },
        }))
    };

    complete(&mut runtime, "wf-node0");
    // After both workers, synth becomes the next batch.
    let step = complete(&mut runtime, "wf-node1");
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowBatchSpawned { nodes, .. }
            if nodes.len() == 1 && nodes[0].agent_id == "wf-node2"
    )));

    // Synth completes → workflow finishes.
    let step = complete(&mut runtime, "wf-node2");
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowCompleted { completed, .. } if completed.len() == 3
    )));
}

#[test]
fn load_workflow_without_start_run_is_rejected() {
    use crate::orchestration::workflow::fanout_synthesize;

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let spec = fanout_synthesize(
        vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
        RuntimeTask::new("synth"),
    );
    let step = runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
        spec,
        parent_session_id: "sess".to_string(),
        resumed_completed: Vec::new(),
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_results: Vec::new(),
    }));

    assert!(step.actions.is_empty());
    assert!(step.observations.is_empty());
    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidLifecycle,
            ..
        }]
    ));
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Created);
}

#[test]
fn submit_workflow_nodes_input_appends_a_node_over_the_abi() {
    // R3-1: exercise the full serde round-trip of SubmitWorkflowNodes + WorkflowNode over the
    // ABI, and confirm the appended node spawns as a workflow batch mid-run.
    use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
    use crate::types::agent::AgentRole;
    use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    // A single-node workflow: wf-node0 spawns first.
    let spec = WorkflowSpec::new(vec![WorkflowNode::new(
        RuntimeTask::new("root"),
        AgentRole::Implement,
    )]);
    runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
        spec,
        parent_session_id: "sess".to_string(),
        resumed_completed: Vec::new(),
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_results: Vec::new(),
    }));
    runtime.state_machine_mut().take_observations();

    // Submit a node over the ABI while wf-node0 runs (full serde round-trip).
    let event = KernelInputEvent::SubmitWorkflowNodes {
        nodes: vec![WorkflowNode::new(
            RuntimeTask::new("more"),
            AgentRole::Implement,
        )],
        submitter_agent_id: None,
    };
    let json = serde_json::to_string(&event).expect("serialize");
    let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");
    let step = runtime.step(KernelInput::new(parsed));
    // The appended node spawns as wf-node1 in a workflow batch.
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowBatchSpawned { nodes, .. }
            if nodes.len() == 1 && nodes[0].agent_id == "wf-node1" && nodes[0].goal == "more"
    )));

    let complete = |runtime: &mut KernelRuntime, id: &str| {
        runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
            result: SubAgentResult {
                agent_id: compact_str::CompactString::new(id),
                result: LoopResult {
                    termination: TerminationReason::Completed,
                    final_message: None,
                    turns_used: 1,
                    total_tokens_used: 1,
                    loop_continue: None,
                    classify_branch: None,
                    tournament_winner: None,
                    pace_decision: None,
                },
            },
        }))
    };
    complete(&mut runtime, "wf-node0");
    // The workflow finishes only after the submitted node also completes (2 nodes total).
    let step = complete(&mut runtime, "wf-node1");
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowCompleted { completed, .. } if completed.len() == 2
    )));
}

#[test]
fn submit_workflow_input_bootstraps_a_dag_over_the_abi() {
    // M5/G1: a top-level agent authors a whole spec over the ABI (full serde round-trip of
    // SubmitWorkflow + WorkflowSpec) with no workflow active → the kernel bootstraps and drives it.
    use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
    use crate::types::agent::AgentRole;
    use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    // No LoadWorkflow first — the agent itself authors the spec.
    let spec = WorkflowSpec::new(vec![WorkflowNode::new(
        RuntimeTask::new("authored root"),
        AgentRole::Implement,
    )]);
    let event = KernelInputEvent::SubmitWorkflow {
        spec,
        parent_session_id: "sess".to_string(),
        submitter_agent_id: None,
    };
    let json = serde_json::to_string(&event).expect("serialize");
    let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");
    let step = runtime.step(KernelInput::new(parsed));
    // The authored node bootstraps as wf-node0 in a workflow batch.
    assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowBatchSpawned { nodes, .. }
                if nodes.len() == 1 && nodes[0].agent_id == "wf-node0" && nodes[0].goal == "authored root"
        )));

    let step = runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
        result: SubAgentResult {
            agent_id: compact_str::CompactString::new("wf-node0"),
            result: LoopResult {
                termination: TerminationReason::Completed,
                final_message: None,
                turns_used: 1,
                total_tokens_used: 1,
                loop_continue: None,
                classify_branch: None,
                tournament_winner: None,
                pace_decision: None,
            },
        },
    }));
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowCompleted { completed, .. } if completed.len() == 1
    )));
}

#[test]
fn load_workflow_resumes_from_completed_nodes() {
    use crate::orchestration::workflow::fanout_synthesize;

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    // Resume a 2-worker fanout where worker 0 already completed before the interruption.
    let spec = fanout_synthesize(
        vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
        RuntimeTask::new("synth"),
    );
    let step = runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
        spec,
        parent_session_id: "sess".to_string(),
        resumed_completed: vec!["wf-node0".to_string()],
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_results: Vec::new(),
    }));

    // Only the remaining worker is re-spawned (node 0 is not re-run).
    let batch = step
        .observations
        .iter()
        .find_map(|o| match o {
            KernelObservation::WorkflowBatchSpawned { nodes, .. } => Some(nodes.clone()),
            _ => None,
        })
        .expect("workflow_batch_spawned");
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].agent_id, "wf-node1");
}
