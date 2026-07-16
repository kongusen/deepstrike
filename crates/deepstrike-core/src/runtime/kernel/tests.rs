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
fn prepared_step_is_committed_only_after_matching_token() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let prepared = runtime.prepare_step(correlated_input(
        "op-prepare",
        "event-start",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    ));

    assert_eq!(prepared.status, KernelPreparationStatus::Prepared);
    assert_eq!(prepared.base_generation, 0);
    assert_eq!(prepared.step.step_seq, 1);
    assert_eq!(
        prepared.step.actions[0].effect_id,
        "op-prepare:step:1:effect:0"
    );
    let token = prepared.prepare_token.as_deref().expect("prepared token");

    let committed = runtime
        .commit_prepared(token)
        .expect("matching token commits the candidate");
    assert_eq!(
        serde_json::to_value(committed).unwrap(),
        serde_json::to_value(prepared.step).unwrap(),
    );
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Running);
    assert_eq!(runtime.diagnostics().next_step_seq, 2);

    let next = runtime.prepare_step(correlated_input(
        "op-prepare",
        "event-memory",
        43,
        KernelInputEvent::SetMemoryEnabled { enabled: true },
    ));
    assert_eq!(next.base_generation, 1);
    runtime
        .abort_prepared(next.prepare_token.as_deref().unwrap())
        .expect("follow-up candidate aborts");
}

#[test]
fn mismatched_prepare_token_invalidates_the_runtime() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let prepared = runtime.prepare_step(correlated_input(
        "op-token-mismatch",
        "event-start",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    ));
    let token = prepared.prepare_token.as_deref().expect("prepared token");

    let mismatch = runtime
        .commit_prepared("wrong-token")
        .expect_err("mismatched token must fail closed");
    assert_eq!(mismatch.code, KernelFaultCode::TransactionConflict);

    let poisoned = runtime
        .commit_prepared(token)
        .expect_err("invalidated runtime cannot be reused");
    assert_eq!(poisoned.code, KernelFaultCode::TransactionConflict);
    let snapshot_fault = runtime
        .snapshot()
        .expect_err("invalidated runtime cannot checkpoint speculative state");
    assert_eq!(snapshot_fault.code, KernelFaultCode::TransactionConflict);
}

#[test]
fn snapshot_rejects_an_outstanding_prepared_transition_without_panicking() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let prepared = runtime.prepare_step(correlated_input(
        "op-prepare-snapshot",
        "evt-prepare-snapshot",
        42,
        KernelInputEvent::ConfigureRun {
            config: RunConfig::default(),
        },
    ));
    assert_eq!(prepared.status, KernelPreparationStatus::Prepared);

    let fault = runtime
        .snapshot()
        .expect_err("uncommitted candidate must not enter a checkpoint");
    assert_eq!(fault.code, KernelFaultCode::TransactionConflict);

    runtime
        .abort_prepared(prepared.prepare_token.as_deref().unwrap())
        .expect("prepared transition remains abortable after the rejected snapshot");
}

#[test]
fn configured_prompt_reservations_are_journaled_and_fail_closed() {
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_tokens: 100,
        ..SchedulerBudget::default()
    });
    let configured = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            prompt_budget: Some(crate::context::config::PromptBudgetConfig {
                prompt_overhead_tokens: 40,
                output_reserve_tokens: 40,
                safety_margin_tokens: 10,
            }),
            ..RunConfig::default()
        },
    }));
    assert!(configured.faults.is_empty());

    let started = runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("fixed context cannot fit the ten-token input allowance"),
        run_spec: None,
    }));
    assert!(
        started
            .actions
            .iter()
            .all(|action| !matches!(action.effect, KernelEffect::CallProvider { .. }))
    );
    assert!(started.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::ContextBudgetExceeded { max_tokens: 10, .. }
    )));
}

#[test]
fn prompt_reservations_cannot_consume_the_entire_context_window() {
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_tokens: 100,
        ..SchedulerBudget::default()
    });
    let rejected = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            prompt_budget: Some(crate::context::config::PromptBudgetConfig {
                prompt_overhead_tokens: 50,
                output_reserve_tokens: 50,
                safety_margin_tokens: 0,
            }),
            memory_enabled: Some(true),
            ..RunConfig::default()
        },
    }));

    assert!(matches!(
        rejected.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidConfig,
            ..
        }]
    ));
}

#[test]
fn aborting_prepared_step_restores_the_exact_committed_runtime() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(correlated_input(
        "op-abort",
        "event-configure",
        40,
        KernelInputEvent::ConfigureRun {
            config: RunConfig {
                memory_enabled: Some(true),
                ..Default::default()
            },
        },
    ));
    let before = runtime.snapshot_json().expect("committed snapshot");
    let input = correlated_input(
        "op-abort",
        "event-start",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    );

    let prepared = runtime.prepare_step(input.clone());
    let token = prepared.prepare_token.as_deref().expect("prepared token");
    runtime
        .abort_prepared(token)
        .expect("abort rebuilds the committed prefix");

    assert_eq!(runtime.snapshot_json().expect("restored snapshot"), before);
    let retried = runtime.step(input);
    assert_eq!(retried.step_seq, prepared.step.step_seq);
    assert_eq!(
        retried.actions[0].effect_id,
        prepared.step.actions[0].effect_id
    );
}

#[test]
fn second_prepare_is_rejected_without_discarding_the_first_candidate() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let first = runtime.prepare_step(correlated_input(
        "op-pending",
        "event-start",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    ));
    let first_token = first.prepare_token.as_deref().expect("first token");

    let second = runtime.prepare_step(correlated_input(
        "op-pending",
        "event-configure",
        43,
        KernelInputEvent::ConfigureRun {
            config: RunConfig::default(),
        },
    ));

    assert_eq!(second.status, KernelPreparationStatus::Rejected);
    assert!(matches!(
        second.step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::TransactionConflict,
            ..
        }]
    ));
    runtime
        .commit_prepared(first_token)
        .expect("first candidate remains committable");
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Running);
}

#[test]
fn exact_event_replay_requires_no_new_durable_commit() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let input = correlated_input(
        "op-replay-prepare",
        "event-start",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    );
    let first = runtime.step(input.clone());

    let replay = runtime.prepare_step(input);

    assert_eq!(replay.status, KernelPreparationStatus::Replayed);
    assert!(replay.prepare_token.is_none());
    assert_eq!(
        serde_json::to_value(replay.step).unwrap(),
        serde_json::to_value(first).unwrap(),
    );
    assert_eq!(runtime.diagnostics().accepted_input_count, 1);
}

#[test]
fn rejected_prepare_has_no_token_and_does_not_mutate_runtime() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let mut invalid = correlated_input(
        "op-rejected-prepare",
        "event-start",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    );
    invalid.version = 1;

    let rejected = runtime.prepare_step(invalid);

    assert_eq!(rejected.status, KernelPreparationStatus::Rejected);
    assert!(rejected.prepare_token.is_none());
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Created);
    assert_eq!(runtime.diagnostics().next_step_seq, 1);
    assert_eq!(runtime.diagnostics().accepted_input_count, 0);
}

#[test]
fn prepared_step_json_round_trips_the_normalized_input_and_status() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let input = correlated_input(
        "op-json-prepare",
        "event-start",
        42,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    );

    let prepared = runtime
        .prepare_step_json(&serde_json::to_string(&input).unwrap())
        .expect("wire input stages");

    assert_eq!(prepared.status, KernelPreparationStatus::Prepared);
    assert_eq!(prepared.input.operation_id, "op-json-prepare");
    assert_eq!(prepared.input.event_id, "event-start");
    let token = prepared.prepare_token.as_deref().expect("wire token");
    runtime
        .commit_prepared(token)
        .expect("wire candidate commits");
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Running);
}

fn accept_workflow_spawn(runtime: &mut KernelRuntime, step: KernelStep) -> KernelStep {
    let Some(KernelAction {
        effect_id,
        effect: KernelEffect::SpawnWorkflow { nodes, .. },
        ..
    }) = step.actions.first()
    else {
        return step;
    };
    runtime.step(KernelInput::new(KernelInputEvent::WorkflowSpawnResult {
        effect_id: effect_id.clone(),
        started_agent_ids: nodes.iter().map(|node| node.agent_id.clone()).collect(),
        failures: Vec::new(),
        error: None,
    }))
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
fn duplicate_effect_result_with_new_event_id_returns_the_original_step() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let start = runtime.step(correlated_input(
        "op-effect-replay",
        "event-start",
        41,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    ));
    let effect_id = start.actions[0].effect_id.clone();
    let result = KernelInputEvent::ProviderResult {
        effect_id: effect_id.clone(),
        message: Message::assistant("done"),
        observed_input_tokens: Some(10),
        observed_output_tokens: Some(2),
        now_ms: Some(42),
        stop_reason: None,
    };

    let first = runtime.step(correlated_input(
        "op-effect-replay",
        "event-result-1",
        42,
        result.clone(),
    ));
    let duplicate = runtime.step(correlated_input(
        "op-effect-replay",
        "event-result-2",
        43,
        result,
    ));

    assert_eq!(
        serde_json::to_value(duplicate).unwrap(),
        serde_json::to_value(first).unwrap(),
    );
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Completed);
}

#[test]
fn duplicate_effect_result_with_conflicting_payload_is_rejected() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let start = runtime.step(correlated_input(
        "op-effect-conflict",
        "event-start",
        41,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        },
    ));
    let effect_id = start.actions[0].effect_id.clone();
    runtime.step(correlated_input(
        "op-effect-conflict",
        "event-result-1",
        42,
        KernelInputEvent::ProviderResult {
            effect_id: effect_id.clone(),
            message: Message::assistant("done"),
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    ));

    let conflict = runtime.step(correlated_input(
        "op-effect-conflict",
        "event-result-2",
        43,
        KernelInputEvent::ProviderResult {
            effect_id: effect_id.clone(),
            message: Message::assistant("different"),
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    ));

    assert!(matches!(
        conflict.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::UnexpectedEffectResult,
            effect_id: Some(conflicting_effect_id),
            ..
        }] if conflicting_effect_id == &effect_id
    ));
}

#[test]
fn deterministic_replay_preserves_the_next_effect_identity() {
    fn drive_to_tool_effect() -> KernelStep {
        let mut runtime = KernelRuntime::new(SchedulerBudget::default());
        let start = runtime.step(correlated_input(
            "op-crash-replay",
            "event-start",
            41,
            KernelInputEvent::StartRun {
                task: RuntimeTask::new("use a tool"),
                run_spec: None,
            },
        ));
        runtime.step(correlated_input(
            "op-crash-replay",
            "event-provider-result",
            42,
            KernelInputEvent::ProviderResult {
                effect_id: start.actions[0].effect_id.clone(),
                message: assistant_calling("fetch"),
                observed_input_tokens: None,
                observed_output_tokens: None,
                now_ms: None,
                stop_reason: None,
            },
        ))
    }

    let before_crash = drive_to_tool_effect();
    let after_replay = drive_to_tool_effect();

    assert_eq!(
        before_crash.actions[0].effect_id,
        after_replay.actions[0].effect_id
    );
    assert_eq!(
        serde_json::to_value(&before_crash.actions[0]).unwrap(),
        serde_json::to_value(&after_replay.actions[0]).unwrap(),
    );
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
fn reliability_config_bounds_replay_windows_from_the_sdk_boundary() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let configured = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            reliability: Some(KernelReliabilityConfig {
                event_replay_capacity: Some(2),
                completed_effect_replay_capacity: Some(2),
                provider_recovery_attempts: Some(0),
                output_recovery_attempts: Some(1),
                host_effect_retry_attempts: Some(2),
                spool_threshold_bytes: Some(4096),
                spool_preview_bytes: Some(512),
                snapshot_input_limit: Some(32),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));
    assert!(configured.faults.is_empty());

    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
        enabled: true,
    }));
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
        enabled: false,
    }));
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
        enabled: true,
    }));

    assert_eq!(runtime.recorded_event_count(), 2);
    assert_eq!(runtime.state_machine().provider_recovery_attempt_limit, 0);
    assert_eq!(runtime.state_machine().output_recovery_attempt_limit, 1);
    assert_eq!(runtime.state_machine().host_effect_retry_attempt_limit, 2);
    assert_eq!(
        runtime.state_machine().ctx.config.spool_threshold_bytes,
        4096
    );
    assert_eq!(runtime.state_machine().ctx.config.spool_preview_bytes, 512);
}

#[test]
fn reliability_limits_reject_oversized_inputs_before_state_changes() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let configured = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            reliability: Some(KernelReliabilityConfig {
                max_input_bytes: Some(512),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));
    assert!(configured.faults.is_empty());
    let before = runtime.diagnostics();

    let rejected = runtime.step(KernelInput::new(KernelInputEvent::AddSystemMessage {
        content: "x".repeat(2_048),
        tokens: 512,
    }));

    assert!(matches!(
        rejected.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::ResourceLimitExceeded,
            ..
        }]
    ));
    let after = runtime.diagnostics();
    assert_eq!(after.accepted_input_count, before.accepted_input_count);
    assert_eq!(after.accepted_input_bytes, before.accepted_input_bytes);
}

#[test]
fn snapshot_journal_is_bounded_by_bytes_and_restores_its_watermark() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let configured = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            reliability: Some(KernelReliabilityConfig {
                max_input_bytes: Some(4_096),
                snapshot_journal_bytes_limit: Some(1_024),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));
    assert!(configured.faults.is_empty());

    let before_overflow = runtime.snapshot().expect("journal initially fits");
    let restored =
        KernelRuntime::restore_snapshot(before_overflow).expect("restore byte watermark");
    assert_eq!(
        restored.diagnostics().accepted_input_bytes,
        runtime.diagnostics().accepted_input_bytes
    );
    assert_eq!(restored.diagnostics().snapshot_journal_bytes_limit, 1_024);

    for index in 0..16 {
        runtime.step(KernelInput::new(KernelInputEvent::AddSystemMessage {
            content: format!("entry-{index}-{}", "x".repeat(160)),
            tokens: 48,
        }));
        if runtime.diagnostics().snapshot_overflowed {
            break;
        }
    }

    let diagnostics = runtime.diagnostics();
    assert!(diagnostics.snapshot_overflowed);
    assert!(diagnostics.accepted_input_bytes <= diagnostics.snapshot_journal_bytes_limit);
    assert!(matches!(
        runtime.snapshot(),
        Err(KernelFault {
            code: KernelFaultCode::SnapshotIncompatible,
            ..
        })
    ));
}

#[test]
fn snapshot_replay_accepts_historical_inputs_larger_than_the_final_live_limit() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            reliability: Some(KernelReliabilityConfig {
                max_input_bytes: Some(4_096),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));
    runtime.step(KernelInput::new(KernelInputEvent::AddSystemMessage {
        content: "historical".repeat(240),
        tokens: 720,
    }));
    let lowered = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            reliability: Some(KernelReliabilityConfig {
                max_input_bytes: Some(512),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));
    assert!(lowered.faults.is_empty());

    let restored = KernelRuntime::restore_snapshot(runtime.snapshot().expect("bounded snapshot"))
        .expect("historical accepted input remains replayable");
    assert_eq!(restored.diagnostics().max_input_bytes, 512);
}

#[test]
fn reliability_config_rejects_unsafe_ranges_atomically() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            memory_enabled: Some(true),
            reliability: Some(KernelReliabilityConfig {
                event_replay_capacity: Some(0),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));

    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidConfig,
            ..
        }]
    ));
    assert!(!runtime.state_machine().ctx.memory_enabled);
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
fn preloaded_history_resumes_from_configured_to_running() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(correlated_input(
        "op-recovery",
        "event-preload",
        42,
        KernelInputEvent::PreloadHistory {
            messages: vec![Message::user("continue")],
        },
    ));
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Configured);

    let step = runtime.step(correlated_input(
        "op-recovery",
        "event-resume",
        43,
        KernelInputEvent::Resume,
    ));

    assert!(step.faults.is_empty());
    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::CallProvider { .. },
            ..
        }]
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
    runtime.clear_test_observations();

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
fn critical_signal_preemption_is_committed_only_after_correlated_result() {
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
    use crate::types::signal::Urgency;

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
        spec: AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Implement,
            "do work",
        ),
        parent_session_id: "parent-session".to_string(),
    }));

    let requested = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-preempt".into(),
        attempt: 1,
        signal: signal(Urgency::Critical, "stop child"),
    }));
    let effect_id = match &requested.actions[0] {
        KernelAction {
            effect_id,
            effect: KernelEffect::PreemptSubAgents { agent_ids, .. },
            ..
        } if agent_ids == &vec!["worker".to_string()] => effect_id.clone(),
        other => panic!("expected preempt_sub_agents action, got {other:?}"),
    };
    assert!(
        !requested
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::AgentPreempted { .. }))
    );

    let committed = runtime.step(KernelInput::new(KernelInputEvent::PreemptResult {
        effect_id,
        error: None,
    }));
    assert!(matches!(
        committed.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::CallProvider { .. },
            ..
        }]
    ));
    assert!(committed.observations.iter().any(|o| matches!(
        o,
        KernelObservation::AgentPreempted { agent_ids, .. }
            if agent_ids == &vec!["worker".to_string()]
    )));
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
    runtime.clear_test_observations();

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
fn budget_grant_enforces_local_token_cap() {
    use crate::types::message::{Content, Message, ToolCall, ToolResult};

    fn run_one_turn(granted_tokens: Option<u64>) -> KernelStep {
        let mut runtime = KernelRuntime::new(SchedulerBudget {
            max_total_tokens: 100,
            ..SchedulerBudget::default()
        });
        runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
            config: RunConfig {
                budget_grant: granted_tokens.map(|tokens| BudgetGrant {
                    reservation_id: "reservation-token".into(),
                    tokens: Some(tokens),
                    subagents: None,
                    rounds: None,
                }),
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

    // This vehicle received five tokens and spends ten, so its reservation cap fires.
    assert!(
        exceeded(&run_one_turn(Some(5))),
        "token grant must bound local usage"
    );
    // N=1 / no group (base 0): local 10 is far under the cap → pre-L1 behavior unchanged.
    assert!(
        !exceeded(&run_one_turn(None)),
        "no group seed ⇒ per-vehicle budget, well under cap"
    );
}

#[test]
fn budget_grant_enforces_local_spawn_cap() {
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

    // The reservation grants no sub-agent capacity, so the first spawn is denied.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            budget_grant: Some(BudgetGrant {
                reservation_id: "reservation-spawn".into(),
                tokens: None,
                subagents: Some(0),
                rounds: None,
            }),
            ..RunConfig::default()
        },
    }));
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("task"),
        run_spec: None,
    }));
    runtime.clear_test_observations();

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
fn budget_grant_reports_correlated_terminal_usage_once() {
    use crate::types::message::Message;
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::correlated(
        "operation-budget",
        "configure-budget",
        1,
        KernelInputEvent::ConfigureRun {
            config: RunConfig {
                budget_grant: Some(BudgetGrant {
                    reservation_id: "reservation-1".into(),
                    tokens: Some(100),
                    subagents: Some(2),
                    rounds: Some(1),
                }),
                ..RunConfig::default()
            },
        },
    ));
    runtime.step(KernelInput::correlated(
        "operation-budget",
        "start-budget",
        2,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("task"),
            run_spec: None,
        },
    ));
    let effect_id = runtime.pending_provider_effect_id();
    let terminal_input = KernelInput::correlated(
        "operation-budget",
        "provider-budget",
        3,
        KernelInputEvent::ProviderResult {
            effect_id,
            message: Message::assistant("done"),
            observed_input_tokens: Some(7),
            observed_output_tokens: Some(3),
            now_ms: None,
            stop_reason: None,
        },
    );
    let terminal = runtime.step(terminal_input.clone());
    let reports = terminal
        .observations
        .iter()
        .filter_map(|observation| match observation {
            KernelObservation::BudgetUsageReported {
                operation_id,
                reservation_id,
                tokens,
                subagents,
                rounds,
            } if operation_id == "operation-budget" && reservation_id == "reservation-1" => {
                Some((*tokens, *subagents, *rounds))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reports, vec![(1, 0, 0)]);
    assert_eq!(
        serde_json::to_value(runtime.step(terminal_input).observations).unwrap(),
        serde_json::to_value(terminal.observations).unwrap(),
    );
}

#[test]
fn complete_run_commits_host_driven_terminal_usage() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            budget_grant: Some(BudgetGrant {
                reservation_id: "workflow-reservation".into(),
                tokens: None,
                subagents: Some(3),
                rounds: None,
            }),
            ..RunConfig::default()
        },
    }));
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("host workflow"),
        run_spec: None,
    }));

    let terminal = runtime.step(KernelInput::correlated(
        "local-operation",
        "workflow-complete",
        3,
        KernelInputEvent::CompleteRun,
    ));

    assert!(
        matches!(
            terminal.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::Done { result },
                ..
            }] if result.termination == crate::types::result::TerminationReason::Completed
        ),
        "{terminal:?}"
    );
    assert!(terminal.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::BudgetUsageReported {
            operation_id,
            reservation_id,
            ..
        } if operation_id == "local-operation" && reservation_id == "workflow-reservation"
    )));
    assert!(runtime.is_terminal());
}

fn cancel_local_operation(
    runtime: &mut KernelRuntime,
    reason: CancellationReason,
    pending_call_ids: Vec<String>,
) -> KernelStep {
    runtime.step(KernelInput::new(KernelInputEvent::CancelOperation {
        operation_id: "local-operation".into(),
        reason,
        pending_call_ids,
    }))
}

#[test]
fn cancellation_is_terminal_correlated_and_idempotent() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            budget_grant: Some(BudgetGrant {
                reservation_id: "cancel-reservation".into(),
                tokens: Some(100),
                subagents: None,
                rounds: None,
            }),
            ..RunConfig::default()
        },
    }));
    let started = runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("cancel me"),
        run_spec: None,
    }));
    let pending_provider = started.actions[0].effect_id.clone();

    let cancelled = cancel_local_operation(
        &mut runtime,
        CancellationReason::LeaseLost,
        vec!["provider-call".into(), "provider-call".into()],
    );

    assert!(matches!(
        cancelled.actions.as_slice(),
        [KernelAction { effect: KernelEffect::Done { result }, .. }]
            if result.termination == crate::types::result::TerminationReason::UserAbort
    ));
    assert!(cancelled.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::OperationCancelled {
            operation_id,
            reason: CancellationReason::LeaseLost,
            pending_call_ids,
            ..
        } if operation_id == "local-operation" && pending_call_ids == &["provider-call"]
    )));
    assert_eq!(
        cancelled
            .observations
            .iter()
            .filter(|observation| matches!(
                observation,
                KernelObservation::BudgetUsageReported { .. }
            ))
            .count(),
        1,
    );
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Cancelled);

    let repeated = cancel_local_operation(
        &mut runtime,
        CancellationReason::LeaseLost,
        vec!["provider-call".into()],
    );
    assert_eq!(repeated.input_event_id, cancelled.input_event_id);
    assert_eq!(repeated.step_seq, cancelled.step_seq);

    let conflicting = cancel_local_operation(
        &mut runtime,
        CancellationReason::HostShutdown,
        vec!["provider-call".into()],
    );
    assert!(matches!(
        conflicting.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::DuplicateEventConflict,
            ..
        }]
    ));

    let late_provider = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: pending_provider,
        message: crate::types::message::Message::assistant("too late"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        now_ms: None,
        stop_reason: None,
    }));
    assert!(matches!(
        late_provider.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidLifecycle,
            ..
        }]
    ));
}

#[test]
fn cancellation_cleans_tool_and_subagent_wait_states() {
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
    use crate::types::result::TerminationReason;

    let mut tool_runtime = KernelRuntime::new(SchedulerBudget::default());
    let tool_step = run_with_tool_call(&mut tool_runtime, "echo");
    assert!(matches!(
        tool_step.actions[0].effect,
        KernelEffect::ExecuteTool { .. }
    ));
    let cancelled = cancel_local_operation(
        &mut tool_runtime,
        CancellationReason::Deadline,
        vec!["call-1".into()],
    );
    assert!(matches!(
        cancelled.actions[0].effect,
        KernelEffect::Done { .. }
    ));

    let mut child_runtime = KernelRuntime::new(SchedulerBudget::default());
    child_runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent"),
        run_spec: None,
    }));
    child_runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
        spec: AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Implement,
            "work",
        ),
        parent_session_id: "parent-session".into(),
    }));
    let cancelled = cancel_local_operation(
        &mut child_runtime,
        CancellationReason::User,
        vec!["worker".into()],
    );
    assert!(matches!(
        cancelled.actions[0].effect,
        KernelEffect::Done { .. }
    ));
    assert!(matches!(
        child_runtime
            .state_machine()
            .task_table()
            .get("worker")
            .map(|task| task.state),
        Some(crate::scheduler::tcb::TaskLifecycle::Done(
            TerminationReason::UserAbort
        ))
    ));
}

#[test]
fn cancellation_cleans_pending_workflow_spawn() {
    use crate::orchestration::workflow::fanout_synthesize;
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("workflow"),
        run_spec: None,
    }));
    let spawning = runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
        spec: fanout_synthesize(
            vec![RuntimeTask::new("a"), RuntimeTask::new("b")],
            RuntimeTask::new("merge"),
        ),
        parent_session_id: "parent".into(),
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_outcomes: Vec::new(),
    }));
    let spawn_effect = spawning.actions[0].effect_id.clone();

    let cancelled = cancel_local_operation(
        &mut runtime,
        CancellationReason::HostShutdown,
        vec!["wf-node0".into(), "wf-node1".into()],
    );
    assert!(matches!(
        cancelled.actions[0].effect,
        KernelEffect::Done { .. }
    ));

    let late_spawn = runtime.step(KernelInput::new(KernelInputEvent::WorkflowSpawnResult {
        effect_id: spawn_effect,
        started_agent_ids: vec!["wf-node0".into(), "wf-node1".into()],
        failures: Vec::new(),
        error: None,
    }));
    assert!(matches!(
        late_spawn.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidLifecycle,
            ..
        }]
    ));
}

#[test]
fn cancellation_rejects_an_inner_operation_mismatch() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("task"),
        run_spec: None,
    }));
    let rejected = runtime.step(KernelInput::new(KernelInputEvent::CancelOperation {
        operation_id: "different-operation".into(),
        reason: CancellationReason::User,
        pending_call_ids: Vec::new(),
    }));
    assert!(matches!(
        rejected.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::OperationMismatch,
            ..
        }]
    ));
    assert!(!runtime.is_terminal());
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
    runtime.clear_test_observations();

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
    runtime.clear_test_observations();

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

fn memory_record(record_id: &str, name: &str, content: &str) -> crate::mm::memory::MemoryRecord {
    use crate::mm::memory::{
        MemoryAuthor, MemoryKind, MemoryProvenance, MemoryRecord, MemoryScope, MemoryTrustLevel,
    };
    MemoryRecord {
        record_id: record_id.into(),
        scope: MemoryScope::new("tenant-test", "kernel-tests"),
        name: name.into(),
        kind: MemoryKind::Project,
        content: content.into(),
        description: "desc".into(),
        provenance: MemoryProvenance {
            session_id: Some("session-test".into()),
            author: MemoryAuthor::Host,
            trust: MemoryTrustLevel::HostVerified,
            evidence_refs: Vec::new(),
        },
        created_at: 1,
        updated_at: 1,
        last_recalled_at: None,
        recall_count: 0,
        confidence: 1.0,
        links: Vec::new(),
        pinned: false,
        ttl_days: None,
    }
}

fn write_memory(runtime: &mut KernelRuntime, name: &str, content: &str) -> KernelStep {
    let requested = runtime.step(KernelInput::new(KernelInputEvent::WriteMemory {
        memory: memory_record(&format!("record-{name}"), name, content),
    }));
    if let Some(KernelAction {
        effect_id,
        effect: KernelEffect::PersistMemory { .. },
        ..
    }) = requested.actions.first()
    {
        assert!(
            !requested
                .observations
                .iter()
                .any(|observation| matches!(observation, KernelObservation::MemoryWritten { .. }))
        );
        return runtime.step(KernelInput::new(KernelInputEvent::MemoryPersistResult {
            effect_id: effect_id.clone(),
            error: None,
        }));
    }
    requested
}

#[test]
fn memory_scoped_upsert_is_canonical_and_snapshot_replay_is_exact() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());

    let first = runtime.step(KernelInput::new(KernelInputEvent::WriteMemory {
        memory: memory_record("stable-id", "build", "cargo build"),
    }));
    let first_effect = first.actions[0].effect_id.clone();
    runtime.step(KernelInput::new(KernelInputEvent::MemoryPersistResult {
        effect_id: first_effect,
        error: None,
    }));

    let mut replacement = memory_record("incoming-id", "build", "cargo nextest");
    replacement.updated_at = 2;
    let second = runtime.step(KernelInput::new(KernelInputEvent::WriteMemory {
        memory: replacement,
    }));
    let (second_effect, canonical) = match &second.actions[0] {
        KernelAction {
            effect_id,
            effect: KernelEffect::PersistMemory { memory },
            ..
        } => (effect_id.clone(), memory),
        other => panic!("expected persist_memory, got {other:?}"),
    };
    assert_eq!(canonical.record_id, "stable-id");
    assert_eq!(canonical.created_at, 1);
    assert_eq!(canonical.updated_at, 2);
    assert_eq!(canonical.content, "cargo nextest");

    let committed = runtime.step(KernelInput::new(KernelInputEvent::MemoryPersistResult {
        effect_id: second_effect,
        error: None,
    }));
    assert!(committed.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::MemoryWritten { record_id, name, .. }
            if record_id == "stable-id" && name == "build"
    )));

    let encoded = runtime.snapshot_json().expect("memory journal snapshot");
    let restored = KernelRuntime::restore_snapshot_json(&encoded).expect("replay memory journal");
    assert_eq!(restored.snapshot_json().expect("re-encode"), encoded);
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
        promotion_recall_threshold: None,
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
        promotion_recall_threshold: None,
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
    use crate::mm::memory::{MemoryQuery, MemoryScope};
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryPolicy {
        memory_path: String::new(),
        stale_warning_days: 2,
        retrieval_top_k: 3,
        validation_enabled: true,
        max_content_bytes: None,
        max_name_length: None,
        promotion_recall_threshold: None,
    }));
    let step = runtime.step(KernelInput::new(KernelInputEvent::QueryMemory {
        query: MemoryQuery {
            scope: MemoryScope::new("tenant-test", "kernel-tests"),
            query: "build settings".into(),
            top_k: 50,
            ..Default::default()
        },
    }));
    let (effect_id, requested_k) = match &step.actions[0] {
        KernelAction {
            effect_id,
            effect: KernelEffect::QueryMemory { requested_k, .. },
            ..
        } => (effect_id.clone(), *requested_k),
        other => panic!("expected query_memory action, got {other:?}"),
    };
    assert_eq!(requested_k, 3);
    assert!(
        !step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::MemoryQueried { .. }))
    );
    let completed = runtime.step(KernelInput::new(KernelInputEvent::MemoryQueryResult {
        effect_id,
        hits: Vec::new(),
        error: None,
    }));
    assert!(
        completed
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::MemoryQueried { requested_k: 3, .. }))
    );
}

#[test]
fn memory_query_result_enforces_scope_and_replays_recalled_record() {
    use crate::mm::memory::{MemoryQuery, MemoryRecall, MemoryScope};

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let query = MemoryQuery {
        scope: MemoryScope::new("tenant-test", "kernel-tests"),
        query: "which command builds the project".into(),
        top_k: 2,
        kinds: vec![crate::mm::memory::MemoryKind::Project],
        min_score: Some(0.5),
    };
    let requested = runtime.step(KernelInput::new(KernelInputEvent::QueryMemory {
        query: query.clone(),
    }));
    let effect_id = requested.actions[0].effect_id.clone();

    let mut escaped = memory_record("other-scope", "build", "npm run build");
    escaped.scope = MemoryScope::new("other-tenant", "kernel-tests");
    let rejected = runtime.step(KernelInput::new(KernelInputEvent::MemoryQueryResult {
        effect_id: effect_id.clone(),
        hits: vec![MemoryRecall {
            record: escaped,
            score: 0.9,
            why: "lexical overlap".into(),
        }],
        error: None,
    }));
    assert!(matches!(
        rejected.faults.first().map(|fault| fault.code),
        Some(KernelFaultCode::UnexpectedEffectResult)
    ));

    let accepted = runtime.step(KernelInput::new(KernelInputEvent::MemoryQueryResult {
        effect_id,
        hits: vec![MemoryRecall {
            record: memory_record("record-build", "build", "cargo build"),
            score: 0.9,
            why: "lexical overlap".into(),
        }],
        error: None,
    }));
    assert!(accepted.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::MemoryQueried { scope, query: observed_query, .. }
            if scope == &query.scope && observed_query == &query.query
    )));
    assert!(matches!(
        accepted.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::CallProvider { context, .. },
            ..
        }] if context.turns.iter().any(|message| message.content.as_text().is_some_and(|text|
            text.contains("record-build") && text.contains("cargo build")))
    ));
    assert!(
        runtime
            .state_machine()
            .ctx
            .partitions
            .history
            .messages
            .iter()
            .any(|message| message
                .content
                .as_text()
                .is_some_and(|text| text.contains("record-build") && text.contains("cargo build")))
    );

    let encoded = runtime.snapshot_json().expect("recall journal snapshot");
    let restored = KernelRuntime::restore_snapshot_json(&encoded).expect("replay recall journal");
    assert_eq!(restored.snapshot_json().expect("re-encode"), encoded);
}

/// Drive a recall whose single hit carries `recall_count`, and return the completing step. The hit
/// is the host's current record state; the kernel journals the incremented lifecycle statelessly.
fn recall_hit(runtime: &mut KernelRuntime, record_id: &str, recall_count: u64) -> KernelStep {
    use crate::mm::memory::{MemoryQuery, MemoryRecall, MemoryScope};
    let requested = runtime.step(KernelInput::new(KernelInputEvent::QueryMemory {
        query: MemoryQuery {
            scope: MemoryScope::new("tenant-test", "kernel-tests"),
            query: "which command builds the project".into(),
            top_k: 5,
            kinds: vec![crate::mm::memory::MemoryKind::Project],
            min_score: None,
        },
    }));
    let effect_id = requested.actions[0].effect_id.clone();
    let mut record = memory_record(record_id, "build", "cargo build");
    record.recall_count = recall_count;
    runtime.step(KernelInput::new(KernelInputEvent::MemoryQueryResult {
        effect_id,
        hits: vec![MemoryRecall {
            record,
            score: 0.9,
            why: "lexical overlap".into(),
        }],
        error: None,
    }))
}

#[test]
fn m3_recall_journals_incremented_lifecycle_from_the_hit() {
    // Recall lifecycle is derived from the routed hit (host-authoritative store), not a kernel
    // ledger: a hit at count N journals N+1, stamped with the current turn.
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = recall_hit(&mut runtime, "record-build", 4);
    let recalls = step
        .observations
        .iter()
        .find_map(|o| match o {
            KernelObservation::MemoryRecalled { recalls, .. } => Some(recalls.clone()),
            _ => None,
        })
        .expect("recall journals a MemoryRecalled observation");
    assert_eq!(recalls.len(), 1);
    assert_eq!(recalls[0].record_id, "record-build");
    assert_eq!(recalls[0].recall_count, 5, "hit count 4 → journaled 5");
}

#[test]
fn m4_recall_crossing_threshold_suggests_promotion_only_on_the_edge() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryPolicy {
        memory_path: String::new(),
        stale_warning_days: 2,
        retrieval_top_k: 5,
        validation_enabled: true,
        max_content_bytes: None,
        max_name_length: None,
        promotion_recall_threshold: Some(2),
    }));

    // Hit at count 0 → 1: still below the threshold, no suggestion.
    let below = recall_hit(&mut runtime, "record-build", 0);
    assert!(
        !below
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::PromotionSuggested { .. }))
    );

    // Hit at count 1 → 2: crosses the threshold, suggestion fires with the new count.
    let crossing = recall_hit(&mut runtime, "record-build", 1);
    let suggested = crossing.observations.iter().find_map(|o| match o {
        KernelObservation::PromotionSuggested { record_id, recall_count, .. } => {
            Some((record_id.clone(), *recall_count))
        }
        _ => None,
    });
    assert_eq!(suggested, Some(("record-build".into(), 2)));

    // Hit already at/above the threshold (2 → 3): edge already passed, no repeat nag.
    let above = recall_hit(&mut runtime, "record-build", 2);
    assert!(
        !above
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::PromotionSuggested { .. }))
    );
}

#[test]
fn default_runtime_uses_requested_top_k_verbatim() {
    use crate::mm::memory::{MemoryQuery, MemoryScope};
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step(KernelInput::new(KernelInputEvent::QueryMemory {
        query: MemoryQuery {
            scope: MemoryScope::new("tenant-test", "kernel-tests"),
            query: "build settings".into(),
            top_k: 50,
            ..Default::default()
        },
    }));
    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::QueryMemory {
                requested_k: 50,
                ..
            },
            ..
        }]
    ));
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

#[test]
fn large_result_spool_is_a_correlated_effect_before_provider_continues() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    run_with_tool_call(&mut runtime, "echo");
    let requested = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: vec![ToolResult {
            call_id: "call-1".into(),
            output: crate::types::message::Content::Text("Z".repeat(60 * 1024)),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    }));
    let effect_id = match requested.actions.as_slice() {
        [
            KernelAction {
                effect_id,
                effect:
                    KernelEffect::SpoolLargeResult {
                        call_id,
                        tool,
                        output,
                        ..
                    },
                ..
            },
        ] => {
            assert_eq!(call_id, "call-1");
            assert_eq!(tool, "echo");
            assert_eq!(output.len(), 60 * 1024);
            effect_id.clone()
        }
        other => panic!("expected spool effect, got {other:?}"),
    };
    assert!(
        !requested
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::LargeResultSpooled { .. }))
    );
    let committed = runtime.step(KernelInput::new(KernelInputEvent::LargeResultSpoolResult {
        effect_id,
        spool_ref: Some("spool://call-1".to_string()),
        error: None,
    }));
    assert!(matches!(
        committed.actions.as_slice(),
        [KernelAction { effect: KernelEffect::CallProvider { tools, .. }, .. }]
            if tools.iter().any(|tool| tool.name == "read_result")
    ));
    assert!(committed.observations.iter().any(|o| matches!(
        o, KernelObservation::LargeResultSpooled { call_id, spool_ref: Some(spool_ref), .. }
            if call_id == "call-1" && spool_ref == "spool://call-1"
    )));
}

#[test]
fn failed_large_result_spool_is_observed_and_retried_without_success_fact() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    run_with_tool_call(&mut runtime, "echo");
    let requested = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: vec![ToolResult {
            call_id: "call-1".into(),
            output: crate::types::message::Content::Text("Z".repeat(60 * 1024)),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    }));
    let failed = runtime.step(KernelInput::new(KernelInputEvent::LargeResultSpoolResult {
        effect_id: requested.actions[0].effect_id.clone(),
        spool_ref: None,
        error: Some("disk full".to_string()),
    }));
    assert!(matches!(
        failed.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::SpoolLargeResult { .. },
            ..
        }]
    ));
    assert!(failed.observations.iter().any(|o| matches!(o, KernelObservation::LargeResultSpoolFailed { error, .. } if error == "disk full")));
    assert!(
        !failed
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::LargeResultSpooled { .. }))
    );
}

#[test]
fn host_effect_retry_limit_terminates_instead_of_spinning() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            reliability: Some(KernelReliabilityConfig {
                host_effect_retry_attempts: Some(0),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));
    run_with_tool_call(&mut runtime, "echo");
    let requested = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: vec![ToolResult {
            call_id: "call-1".into(),
            output: crate::types::message::Content::Text("Z".repeat(60 * 1024)),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    }));

    let exhausted = runtime.step(KernelInput::new(KernelInputEvent::LargeResultSpoolResult {
        effect_id: requested.actions[0].effect_id.clone(),
        spool_ref: None,
        error: Some("disk full".to_string()),
    }));

    assert!(
        matches!(exhausted.actions.as_slice(), [KernelAction { effect: KernelEffect::Done { result }, .. }] if result.termination == crate::types::result::TerminationReason::Error)
    );
    assert!(exhausted.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::LargeResultSpoolFailed { .. }
    )));
}

#[test]
fn multiple_large_results_commit_in_order_before_provider_continues() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    run_with_tool_call(&mut runtime, "echo");
    let requested = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
        effect_id: runtime.pending_tool_effect_id(),
        results: ["call-1", "call-2"]
            .into_iter()
            .map(|call_id| ToolResult {
                call_id: call_id.into(),
                output: crate::types::message::Content::Text("Z".repeat(60 * 1024)),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            })
            .collect(),
    }));
    let first_effect_id = requested.actions[0].effect_id.clone();
    assert!(
        matches!(&requested.actions[0].effect, KernelEffect::SpoolLargeResult { call_id, .. } if call_id == "call-1")
    );
    let second = runtime.step(KernelInput::new(KernelInputEvent::LargeResultSpoolResult {
        effect_id: first_effect_id,
        spool_ref: Some("spool://call-1".to_string()),
        error: None,
    }));
    let second_effect_id = second.actions[0].effect_id.clone();
    assert!(
        matches!(&second.actions[0].effect, KernelEffect::SpoolLargeResult { call_id, .. } if call_id == "call-2")
    );
    let completed = runtime.step(KernelInput::new(KernelInputEvent::LargeResultSpoolResult {
        effect_id: second_effect_id,
        spool_ref: Some("spool://call-2".to_string()),
        error: None,
    }));
    assert!(matches!(
        completed.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::CallProvider { .. },
            ..
        }]
    ));
}

fn runtime_with_page_out() -> KernelRuntime {
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_tokens: 100,
        ..SchedulerBudget::default()
    });
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("compact"),
        run_spec: None,
    }));
    for index in 0..10 {
        runtime.push_test_history(Message::user(format!("filler {index}")), 50);
    }
    runtime
}

#[test]
fn page_out_archive_is_a_correlated_effect_with_no_pre_result_success_fact() {
    let mut runtime = runtime_with_page_out();
    let requested = runtime.step(KernelInput::new(KernelInputEvent::ForceCompact));
    let effect_id = match requested.actions.as_slice() {
        [
            KernelAction {
                effect_id,
                effect: KernelEffect::ArchivePageOut { archived, tier, .. },
                ..
            },
        ] => {
            assert!(!archived.is_empty());
            assert_eq!(tier, "semantic");
            effect_id.clone()
        }
        other => panic!("expected page-out archive effect, got {other:?}"),
    };
    assert!(
        !requested
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::PageOutArchived { .. }))
    );
    let committed = runtime.step(KernelInput::new(KernelInputEvent::PageOutArchiveResult {
        effect_id,
        archive_ref: Some("archive://batch-1".to_string()),
        error: None,
    }));
    assert!(committed.actions.is_empty());
    assert!(committed.observations.iter().any(|o| matches!(
        o, KernelObservation::PageOutArchived { archive_ref: Some(archive_ref), message_count, .. }
            if archive_ref == "archive://batch-1" && *message_count > 0
    )));
}

#[test]
fn failed_page_out_archive_is_observed_and_retried() {
    let mut runtime = runtime_with_page_out();
    let requested = runtime.step(KernelInput::new(KernelInputEvent::ForceCompact));
    let failed = runtime.step(KernelInput::new(KernelInputEvent::PageOutArchiveResult {
        effect_id: requested.actions[0].effect_id.clone(),
        archive_ref: None,
        error: Some("archive unavailable".to_string()),
    }));
    assert!(matches!(
        failed.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::ArchivePageOut { .. },
            ..
        }]
    ));
    assert!(failed.observations.iter().any(|o| matches!(o, KernelObservation::PageOutArchiveFailed { error, .. } if error == "archive unavailable")));
    assert!(
        !failed
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::PageOutArchived { .. }))
    );
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
    runtime.clear_test_observations();
    runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: runtime.pending_provider_effect_id(),
        message: assistant_calling(tool),
        observed_input_tokens: None,
        observed_output_tokens: None,
        stop_reason: None,
        now_ms: None,
    }))
}

fn step_has_permission_denied_result(step: &KernelStep) -> bool {
    match step.actions.as_slice() {
        [
            KernelAction {
                effect: KernelEffect::CallProvider { context, .. },
                ..
            },
        ] => context.turns.iter().any(|message| match &message.content {
            crate::types::message::Content::Parts(parts) => parts.iter().any(|part| {
                matches!(
                    part,
                    crate::types::message::ContentPart::ToolResult { output, is_error: true, .. }
                        if output.contains("permission denied")
                )
            }),
            _ => false,
        }),
        _ => false,
    }
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

    // Denied call must NOT reach ExecuteTool; the denial commits as an error tool result and the
    // loop re-prompts without rollback.
    assert!(
        matches!(
            step.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::CallProvider { .. },
                ..
            }]
        ),
        "denied tool should commit the denial and re-call provider, got {:?}",
        step.actions
    );
    assert!(
        !step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
        "governance denial must not roll the turn back",
    );
    assert!(
        step_has_permission_denied_result(&step),
        "the denial must be visible to the model as an error tool result"
    );
}

#[test]
fn legacy_deny_mode_field_cannot_restore_governance_rollback() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let event: KernelInputEvent = serde_json::from_value(serde_json::json!({
        "kind": "load_governance_policy",
        "default_action": "allow",
        "rules": [{ "tool_pattern": "danger.*", "action": "deny" }],
        "vetoed_tools": [],
        "rate_limits": [],
        "constraints": [],
        "deny_mode": "rollback"
    }))
    .expect("legacy development event must remain readable");
    runtime.step(KernelInput::new(event));

    let step = run_with_tool_call(&mut runtime, "danger.delete");

    assert!(
        !step
            .observations
            .iter()
            .any(|observation| matches!(observation, KernelObservation::Rollbacked { .. })),
        "a stale deny_mode field must not restore the removed rollback behavior",
    );
    assert!(
        step_has_permission_denied_result(&step),
        "the denial must remain visible to the model"
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
            signal_policy: Some(SignalPolicyConfig {
                version: SIGNAL_POLICY_VERSION,
                queue_max: 32,
                ttl_ms: None,
                deadline_escalation: None,
            }),
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
        "bundle-configured deny should commit the denial and re-call provider, got {:?}",
        step.actions
    );
    assert!(
        !step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
        "bundle deny must behave like the granular event: visible result, no rollback",
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
            scheduler_policy: Some(crate::scheduler::policy::SchedulerPolicyConfig {
                critical_path_weight: 42,
                ..crate::scheduler::policy::SchedulerPolicyConfig::default()
            }),
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

    assert!(matches!(
        step.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::RequestApproval { requests },
            ..
        }] if requests.len() == 1 && requests[0].tool == "sensitive.read"
    ));
    assert!(
        !step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::ToolGated { .. }))
    );
    assert!(
        step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::Suspended { reason, .. } if reason == "ask_user"
        )),
        "expected a Suspended observation",
    );
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Suspended);

    let resumed = runtime.step(KernelInput::new(KernelInputEvent::ApprovalResult {
        effect_id: step.actions[0].effect_id.clone(),
        approved_calls: vec!["call-1".to_string()],
        denied_calls: vec![],
        error: None,
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
fn approval_host_failure_reissues_effect_without_success_observation() {
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
    let approval = run_with_tool_call(&mut runtime, "sensitive.read");

    let failed = runtime.step(KernelInput::new(KernelInputEvent::ApprovalResult {
        effect_id: approval.actions[0].effect_id.clone(),
        approved_calls: vec![],
        denied_calls: vec![],
        error: Some("approval service unavailable".to_string()),
    }));

    assert!(matches!(
        failed.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::RequestApproval { .. },
            ..
        }]
    ));
    assert_ne!(failed.actions[0].effect_id, approval.actions[0].effect_id);
    assert!(failed.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::ApprovalResolutionFailed { error, .. }
            if error == "approval service unavailable"
    )));
    assert!(!failed.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::Resumed { .. } | KernelObservation::ToolGated { .. }
    )));
    assert_eq!(runtime.lifecycle(), KernelLifecycle::Suspended);
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
    let approval = run_with_tool_call(&mut runtime, "sensitive.read");
    runtime.clear_test_observations();

    let step = runtime.step(KernelInput::new(KernelInputEvent::ApprovalResult {
        effect_id: approval.actions[0].effect_id.clone(),
        approved_calls: vec![],
        denied_calls: vec!["call-1".to_string()],
        error: None,
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
    runtime.clear_test_observations();

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
    runtime.clear_test_observations();

    // Second call to the same tool within the window — rate limited → visible error result.
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
        "rate-limited call should commit the denial and re-call provider, got {:?}",
        s2.actions
    );
    assert!(
        !s2.observations
            .iter()
            .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
        "rate-limited calls must not roll back",
    );
    assert!(
        step_has_permission_denied_result(&s2),
        "the rate-limit denial must be visible to the model"
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

    // assistant_calling emits empty args `{}` → required "path" is missing → deny. The violation
    // commits as an error result — a dynamic denial the model must see to correct its arguments.
    let step = run_with_tool_call(&mut runtime, "write");
    assert!(
        matches!(
            step.actions.as_slice(),
            [KernelAction {
                effect: KernelEffect::CallProvider { .. },
                ..
            }]
        ),
        "missing required param should commit the denial and re-prompt, got {:?}",
        step.actions
    );
    assert!(
        !step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
        "constraint violation commits as a visible error result, not a rollback",
    );
    assert!(
        step_has_permission_denied_result(&step),
        "the constraint denial must be visible to the model"
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
    runtime.step(KernelInput::new(KernelInputEvent::SetSignalPolicy {
        policy: SignalPolicyConfig {
            version: SIGNAL_POLICY_VERSION,
            queue_max: max_queue,
            ttl_ms: None,
            deadline_escalation: None,
        },
    }));
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("watch for signals"),
        run_spec: None,
    }));
    runtime.clear_test_observations();
    runtime
}

#[test]
fn signal_policy_event_serde_uses_the_single_versioned_contract() {
    let event: KernelInputEvent = serde_json::from_value(serde_json::json!({
        "kind": "set_signal_policy",
        "policy": {
            "version": 1,
            "queue_max": 4,
            "ttl_ms": 250,
            "deadline_escalation": true
        }
    }))
    .expect("signal policy parses");

    let encoded = serde_json::to_value(&event).expect("signal policy serializes");
    assert_eq!(encoded["kind"], "set_signal_policy");
    assert_eq!(encoded["policy"]["version"], 1);
    assert_eq!(encoded["policy"]["queue_max"], 4);
    assert_eq!(encoded["policy"]["ttl_ms"], 250);
    assert_eq!(encoded["policy"]["deadline_escalation"], true);

    match event {
        KernelInputEvent::SetSignalPolicy { policy } => {
            assert_eq!(policy.version, SIGNAL_POLICY_VERSION);
            assert_eq!(policy.queue_max, 4);
            assert_eq!(policy.ttl_ms, Some(250));
            assert_eq!(policy.deadline_escalation, Some(true));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn legacy_attention_policy_shape_is_rejected_instead_of_silently_ignored() {
    let parsed = serde_json::from_value::<KernelInputEvent>(serde_json::json!({
        "kind": "configure_run",
        "config": {
            "attention_max_queue_size": 4
        }
    }));
    let legacy_event = serde_json::from_value::<KernelInputEvent>(serde_json::json!({
        "kind": "set_attention_policy",
        "max_queue_size": 4
    }));

    assert!(parsed.is_err());
    assert!(legacy_event.is_err());
}

#[test]
fn signal_policy_rejects_invalid_values_atomically() {
    let invalid = [
        SignalPolicyConfig {
            version: SIGNAL_POLICY_VERSION,
            queue_max: 0,
            ttl_ms: None,
            deadline_escalation: None,
        },
        SignalPolicyConfig {
            version: SIGNAL_POLICY_VERSION,
            queue_max: 4,
            ttl_ms: Some(0),
            deadline_escalation: None,
        },
        SignalPolicyConfig {
            version: SIGNAL_POLICY_VERSION + 1,
            queue_max: 4,
            ttl_ms: Some(10),
            deadline_escalation: None,
        },
    ];

    for policy in invalid {
        let mut runtime = KernelRuntime::new(SchedulerBudget::default());
        let rejected = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
            config: RunConfig {
                memory_enabled: Some(true),
                signal_policy: Some(policy),
                ..RunConfig::default()
            },
        }));

        assert!(matches!(
            rejected.faults.as_slice(),
            [KernelFault {
                code: KernelFaultCode::InvalidConfig,
                ..
            }]
        ));
        assert!(!runtime.state_machine().ctx.memory_enabled);
    }
}

#[test]
fn granular_signal_policy_rejects_invalid_ttl_before_mutation() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let rejected = runtime.step(KernelInput::new(KernelInputEvent::SetSignalPolicy {
        policy: SignalPolicyConfig {
            version: SIGNAL_POLICY_VERSION,
            queue_max: 4,
            ttl_ms: Some(0),
            deadline_escalation: None,
        },
    }));

    assert!(matches!(
        rejected.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidConfig,
            ..
        }]
    ));
}

#[test]
fn granular_signal_policy_is_rejected_after_start() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("policy is frozen at start"),
        run_spec: None,
    }));

    let rejected = runtime.step(KernelInput::new(KernelInputEvent::SetSignalPolicy {
        policy: SignalPolicyConfig {
            version: SIGNAL_POLICY_VERSION,
            queue_max: 4,
            ttl_ms: Some(10),
            deadline_escalation: None,
        },
    }));

    assert!(matches!(
        rejected.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidLifecycle,
            ..
        }]
    ));
}

#[test]
fn configured_signal_policy_applies_queue_ttl_in_runtime() {
    use crate::types::signal::Urgency;

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let configured = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            signal_policy: Some(SignalPolicyConfig {
                version: SIGNAL_POLICY_VERSION,
                queue_max: 1,
                ttl_ms: Some(10),
                deadline_escalation: Some(false),
            }),
            ..RunConfig::default()
        },
    }));
    assert!(configured.faults.is_empty());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("ttl queue"),
        run_spec: None,
    }));

    let first = runtime.step(KernelInput::correlated(
        "local-operation",
        "signal-policy-first",
        10,
        KernelInputEvent::DeliverSignal {
            delivery_id: "first".into(),
            attempt: 1,
            signal: signal(Urgency::Normal, "stale").with_timestamp(10),
        },
    ));
    assert!(first.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::SignalDeliveryDisposed { disposition, .. }
            if disposition == "queue"
    )));

    let second = runtime.step(KernelInput::correlated(
        "local-operation",
        "signal-policy-second",
        30,
        KernelInputEvent::DeliverSignal {
            delivery_id: "second".into(),
            attempt: 1,
            signal: signal(Urgency::Normal, "fresh").with_timestamp(30),
        },
    ));
    assert!(
        second
            .observations
            .iter()
            .any(|observation| matches!(observation, KernelObservation::SignalExpired { .. }))
    );
    assert!(second.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::SignalDeliveryDisposed { disposition, queue_depth: 1, .. }
            if disposition == "queue"
    )));
}

#[test]
fn configured_deadline_escalation_changes_runtime_disposition() {
    use crate::types::signal::Urgency;

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let configured = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            signal_policy: Some(SignalPolicyConfig {
                version: SIGNAL_POLICY_VERSION,
                queue_max: 4,
                ttl_ms: None,
                deadline_escalation: Some(true),
            }),
            ..RunConfig::default()
        },
    }));
    assert!(configured.faults.is_empty());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("deadline queue"),
        run_spec: None,
    }));

    let delivered = runtime.step(KernelInput::correlated(
        "local-operation",
        "deadline-due",
        100,
        KernelInputEvent::DeliverSignal {
            delivery_id: "deadline-due".into(),
            attempt: 1,
            signal: signal(Urgency::Normal, "due now")
                .with_timestamp(90)
                .with_deadline(100),
        },
    ));

    assert!(delivered.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::SignalDeliveryDisposed { disposition, .. }
            if disposition == "interrupt"
    )));
}

#[test]
fn attention_policy_critical_signal_interrupts() {
    use crate::types::signal::Urgency;
    let mut runtime = started_runtime_with_attention(8);
    let step = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-critical".into(),
        attempt: 1,
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
        KernelObservation::SignalDeliveryDisposed {
            delivery_id,
            attempt: 1,
            disposition,
            ..
        } if delivery_id == "delivery-critical" && disposition == "interrupt_now"
    )));
}

#[test]
fn attention_policy_normal_signal_queues_without_action() {
    use crate::types::signal::Urgency;
    let mut runtime = started_runtime_with_attention(8);
    let step = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-normal".into(),
        attempt: 1,
        signal: signal(Urgency::Normal, "job"),
    }));
    assert!(
        step.actions.is_empty(),
        "normal signal should queue without a provider call, got {:?}",
        step.actions
    );
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::SignalDeliveryDisposed { disposition, queue_depth, .. }
        if disposition == "queue" && *queue_depth == 1
    )));
}

#[test]
fn attention_policy_full_queue_drops() {
    use crate::types::signal::Urgency;
    let mut runtime = started_runtime_with_attention(1);
    runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-first".into(),
        attempt: 1,
        signal: signal(Urgency::Normal, "first"),
    }));
    let step = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-second".into(),
        attempt: 1,
        signal: signal(Urgency::Normal, "second"),
    }));
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::SignalDeliveryDisposed { disposition, .. } if disposition == "dropped"
    )));
}

#[test]
fn signal_redelivery_attempts_are_correlated_and_distinct() {
    use crate::types::signal::Urgency;
    let mut runtime = started_runtime_with_attention(8);
    let signal = signal(Urgency::Low, "leased").with_dedupe("logical-signal");

    let first = runtime.step(KernelInput::correlated(
        "local-operation",
        "delivery-event-1",
        1,
        KernelInputEvent::DeliverSignal {
            delivery_id: "delivery-1".into(),
            attempt: 1,
            signal: signal.clone(),
        },
    ));
    let second = runtime.step(KernelInput::correlated(
        "local-operation",
        "delivery-event-2",
        2,
        KernelInputEvent::DeliverSignal {
            delivery_id: "delivery-1".into(),
            attempt: 2,
            signal,
        },
    ));

    assert!(first.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::SignalDeliveryDisposed {
            delivery_id,
            attempt: 1,
            disposition,
            ..
        } if delivery_id == "delivery-1" && disposition == "observe"
    )));
    assert!(second.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::SignalDeliveryDisposed {
            delivery_id,
            attempt: 2,
            disposition,
            ..
        } if delivery_id == "delivery-1" && disposition == "ignore"
    )));
}

#[test]
fn signal_delivery_rejects_missing_identity_or_zero_attempt() {
    use crate::types::signal::Urgency;
    let mut runtime = started_runtime_with_attention(8);
    let step = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: String::new(),
        attempt: 0,
        signal: signal(Urgency::Low, "invalid"),
    }));

    assert!(matches!(
        step.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidConfig,
            ..
        }]
    ));
}

#[test]
fn delivered_signal_is_consumed_only_by_its_correlated_provider_result() {
    use crate::types::signal::Urgency;

    let mut runtime = started_runtime_with_attention(8);
    let requested = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-consume".into(),
        attempt: 1,
        signal: signal(Urgency::Critical, "consume once"),
    }));
    let effect_id = match requested.actions.as_slice() {
        [
            KernelAction {
                effect_id,
                effect: KernelEffect::CallProvider { .. },
                ..
            },
        ] => effect_id.clone(),
        other => panic!("expected provider request, got {other:?}"),
    };
    assert_eq!(runtime.state_machine().ctx.partitions.signals.len(), 1);

    let rejected = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: "wrong-effect".into(),
        message: Message::assistant("wrong"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        now_ms: None,
        stop_reason: None,
    }));
    assert!(matches!(
        rejected.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::UnexpectedEffectResult,
            ..
        }]
    ));
    assert_eq!(runtime.state_machine().ctx.partitions.signals.len(), 1);

    runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id,
        message: Message::assistant("handled"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        now_ms: None,
        stop_reason: None,
    }));
    assert!(runtime.state_machine().ctx.partitions.signals.is_empty());
}

#[test]
fn provider_error_does_not_consume_delivered_signal() {
    use crate::types::signal::Urgency;

    let mut runtime = started_runtime_with_attention(8);
    let requested = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-provider-error".into(),
        attempt: 1,
        signal: signal(Urgency::Critical, "survive provider failure"),
    }));
    let effect_id = requested.actions[0].effect_id.clone();

    runtime.step(KernelInput::new(KernelInputEvent::ProviderError {
        effect_id,
        message: "provider failed".into(),
    }));

    assert_eq!(runtime.state_machine().ctx.partitions.signals.len(), 1);
}

#[test]
fn signal_arriving_during_provider_call_gets_a_follow_up_turn_before_completion() {
    use crate::types::signal::Urgency;

    let mut runtime = started_runtime_with_attention(8);
    let in_flight_effect_id = runtime.pending_provider_effect_id();
    let disposition = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-in-flight".into(),
        attempt: 1,
        signal: signal(Urgency::High, "handle after current response"),
    }));
    assert!(disposition.actions.is_empty());

    let boundary = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
        effect_id: in_flight_effect_id,
        message: Message::assistant("current response"),
        observed_input_tokens: None,
        observed_output_tokens: None,
        now_ms: None,
        stop_reason: None,
    }));

    assert!(matches!(
        boundary.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::CallProvider { .. },
            ..
        }]
    ));
    assert_eq!(runtime.state_machine().ctx.partitions.signals.len(), 1);
}

#[test]
fn terminal_runtime_accepts_signal_and_reports_pending_depth() {
    use crate::types::signal::Urgency;

    let mut runtime = started_runtime_with_attention(8);
    let terminal = runtime.step(KernelInput::new(KernelInputEvent::CompleteRun));
    assert!(matches!(
        terminal.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::Done { .. },
            ..
        }]
    ));

    let pending = runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "delivery-after-terminal".into(),
        attempt: 1,
        signal: signal(Urgency::Normal, "late work"),
    }));

    assert!(pending.faults.is_empty());
    assert!(pending.observations.iter().any(|observation| matches!(
        observation,
        KernelObservation::SignalsPending { depth: 1, .. }
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
    runtime.clear_test_observations();

    // Exercise the full serde round-trip of LoadWorkflow + WorkflowSpec over the ABI.
    let spec = fanout_synthesize(
        vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
        RuntimeTask::new("synth"),
    );
    let event = KernelInputEvent::LoadWorkflow {
        spec,
        parent_session_id: "sess".to_string(),
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_outcomes: Vec::new(),
    };
    let json = serde_json::to_string(&event).expect("serialize");
    let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");

    let step = runtime.step(KernelInput::new(parsed));
    assert!(
        !step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::WorkflowBatchSpawned { .. }))
    );
    // First batch is an effect request; it is not a completed fact until the host result arrives.
    let (spawn_effect_id, batch) = match &step.actions[0] {
        KernelAction {
            effect_id,
            effect: KernelEffect::SpawnWorkflow { nodes, .. },
            ..
        } => (effect_id.clone(), nodes.clone()),
        other => panic!("expected spawn_workflow action, got {other:?}"),
    };
    assert_eq!(batch.len(), 2);
    let goals: Vec<&str> = batch.iter().map(|n| n.goal.as_str()).collect();
    assert!(goals.contains(&"w0") && goals.contains(&"w1"));
    assert_eq!(batch[0].agent_id, "wf-node0");
    assert_eq!(batch[0].isolation, "read_only"); // fanout workers are Explore → read_only

    let started = runtime.step(KernelInput::new(KernelInputEvent::WorkflowSpawnResult {
        effect_id: spawn_effect_id,
        started_agent_ids: batch.iter().map(|node| node.agent_id.clone()).collect(),
        failures: Vec::new(),
        error: None,
    }));
    assert!(started.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowBatchSpawned { nodes, .. } if nodes.len() == 2
    )));

    let complete = |runtime: &mut KernelRuntime, id: &str| {
        let step = runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
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
        }));
        accept_workflow_spawn(runtime, step)
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
        KernelObservation::WorkflowCompleted { node_outcomes, .. }
            if node_outcomes.iter().filter(|outcome| {
                outcome.status == crate::orchestration::workflow::WorkflowNodeStatus::Completed
            }).count() == 3
    )));
}

#[test]
fn workflow_spawn_host_failure_reissues_effect_without_success_observation() {
    use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
    use crate::types::agent::AgentRole;

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    let first = runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
        spec: WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("worker"),
            AgentRole::Implement,
        )]),
        parent_session_id: "sess".to_string(),
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_outcomes: Vec::new(),
    }));
    let effect_id = first.actions[0].effect_id.clone();

    let failed = runtime.step(KernelInput::new(KernelInputEvent::WorkflowSpawnResult {
        effect_id,
        started_agent_ids: Vec::new(),
        failures: Vec::new(),
        error: Some("orchestrator unavailable".to_string()),
    }));

    assert!(matches!(
        failed.actions.as_slice(),
        [KernelAction {
            effect: KernelEffect::SpawnWorkflow { .. },
            ..
        }]
    ));
    assert!(
        !failed
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::WorkflowBatchSpawned { .. }))
    );
    assert!(failed.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowSpawnFailed { error, .. }
            if error == "orchestrator unavailable"
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
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_outcomes: Vec::new(),
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
    runtime.clear_test_observations();

    // A single-node workflow: wf-node0 spawns first.
    let spec = WorkflowSpec::new(vec![WorkflowNode::new(
        RuntimeTask::new("root"),
        AgentRole::Implement,
    )]);
    let initial = runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
        spec,
        parent_session_id: "sess".to_string(),
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
        resumed_outcomes: Vec::new(),
    }));
    accept_workflow_spawn(&mut runtime, initial);
    runtime.clear_test_observations();

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
    let step = accept_workflow_spawn(&mut runtime, step);
    // The appended node spawns as wf-node1 in a workflow batch.
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowBatchSpawned { nodes, .. }
            if nodes.len() == 1 && nodes[0].agent_id == "wf-node1" && nodes[0].goal == "more"
    )));

    let complete = |runtime: &mut KernelRuntime, id: &str| {
        let step = runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
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
        }));
        accept_workflow_spawn(runtime, step)
    };
    complete(&mut runtime, "wf-node0");
    // The workflow finishes only after the submitted node also completes (2 nodes total).
    let step = complete(&mut runtime, "wf-node1");
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::WorkflowCompleted { node_outcomes, .. }
            if node_outcomes.iter().filter(|outcome| {
                outcome.status == crate::orchestration::workflow::WorkflowNodeStatus::Completed
            }).count() == 2
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
    runtime.clear_test_observations();

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
    let step = accept_workflow_spawn(&mut runtime, step);
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
        KernelObservation::WorkflowCompleted { node_outcomes, .. }
            if node_outcomes.iter().filter(|outcome| {
                outcome.status == crate::orchestration::workflow::WorkflowNodeStatus::Completed
            }).count() == 1
    )));
}

fn assert_same_kernel_step(left: &KernelStep, right: &KernelStep) {
    assert_eq!(
        serde_json::to_value(left).expect("serialize left step"),
        serde_json::to_value(right).expect("serialize right step"),
    );
}

#[test]
fn snapshot_v2_restores_pending_effect_identity_and_dedupe_window() {
    let policy = SchedulerBudget::default();
    let mut uninterrupted = KernelRuntime::new(policy);
    let start = correlated_input(
        "snapshot-op",
        "snapshot-start",
        10,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("resume exactly"),
            run_spec: None,
        },
    );
    let started = uninterrupted.step(start.clone());
    let provider_effect_id = started.actions[0].effect_id.clone();

    let snapshot = uninterrupted.snapshot().expect("snapshot pending provider");
    assert_eq!(snapshot.snapshot_version, KERNEL_SNAPSHOT_VERSION);
    assert_eq!(snapshot.lifecycle, KernelLifecycle::Running);
    let mut restored = KernelRuntime::restore_snapshot(snapshot).expect("restore snapshot");

    assert_same_kernel_step(&started, &restored.step(start));
    let provider_result = correlated_input(
        "snapshot-op",
        "snapshot-provider-result",
        20,
        KernelInputEvent::ProviderResult {
            effect_id: provider_effect_id,
            message: crate::types::message::Message::assistant("finished"),
            observed_input_tokens: Some(12),
            observed_output_tokens: Some(3),
            now_ms: Some(20),
            stop_reason: None,
        },
    );
    let uninterrupted_done = uninterrupted.step(provider_result.clone());
    let restored_done = restored.step(provider_result);
    assert_same_kernel_step(&uninterrupted_done, &restored_done);
    assert_eq!(restored.lifecycle(), KernelLifecycle::Completed);
}

#[test]
fn snapshot_v2_restores_workflow_budget_and_terminal_cancellation() {
    use crate::orchestration::workflow::fanout_synthesize;

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(correlated_input(
        "workflow-snapshot-op",
        "workflow-config",
        1,
        KernelInputEvent::ConfigureRun {
            config: RunConfig {
                budget_grant: Some(BudgetGrant {
                    reservation_id: "workflow-snapshot-reservation".into(),
                    tokens: Some(100),
                    subagents: Some(2),
                    rounds: Some(1),
                }),
                ..RunConfig::default()
            },
        },
    ));
    runtime.step(correlated_input(
        "workflow-snapshot-op",
        "workflow-start",
        2,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent"),
            run_spec: None,
        },
    ));
    let workflow = runtime.step(correlated_input(
        "workflow-snapshot-op",
        "workflow-load",
        3,
        KernelInputEvent::LoadWorkflow {
            spec: fanout_synthesize(
                vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
                RuntimeTask::new("synth"),
            ),
            parent_session_id: "parent-session".into(),
            resumed_submissions: Vec::new(),
            resumed_submission_bases: Vec::new(),
            resumed_outcomes: Vec::new(),
        },
    ));
    let pending_ids = match &workflow.actions[0] {
        KernelAction {
            effect: KernelEffect::SpawnWorkflow { nodes, .. },
            ..
        } => nodes
            .iter()
            .map(|node| node.agent_id.clone())
            .collect::<Vec<_>>(),
        other => panic!("expected workflow spawn, got {other:?}"),
    };

    let snapshot_json = runtime.snapshot_json().expect("encode workflow snapshot");
    let mut restored =
        KernelRuntime::restore_snapshot_json(&snapshot_json).expect("restore workflow");
    let cancel = correlated_input(
        "workflow-snapshot-op",
        "workflow-cancel",
        4,
        KernelInputEvent::CancelOperation {
            operation_id: "workflow-snapshot-op".into(),
            reason: CancellationReason::HostShutdown,
            pending_call_ids: pending_ids,
        },
    );
    let original_cancel = runtime.step(cancel.clone());
    let restored_cancel = restored.step(cancel);
    assert_same_kernel_step(&original_cancel, &restored_cancel);
    assert!(
        restored_cancel
            .observations
            .iter()
            .any(|observation| matches!(
                observation,
                KernelObservation::BudgetUsageReported { reservation_id, .. }
                    if reservation_id == "workflow-snapshot-reservation"
            ))
    );
    assert_eq!(restored.lifecycle(), KernelLifecycle::Cancelled);
}

#[test]
fn snapshot_v2_rejects_incompatible_or_over_limit_checkpoints() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            reliability: Some(KernelReliabilityConfig {
                snapshot_input_limit: Some(2),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));
    let mut incompatible = runtime.snapshot().expect("bounded snapshot");
    incompatible.snapshot_version = 1;
    assert!(matches!(
        KernelRuntime::restore_snapshot(incompatible),
        Err(KernelFault {
            code: KernelFaultCode::SnapshotIncompatible,
            ..
        })
    ));

    let mut inconsistent_limit = runtime.snapshot().expect("bounded snapshot");
    inconsistent_limit.snapshot_input_limit = 100_001;
    assert!(matches!(
        KernelRuntime::restore_snapshot(inconsistent_limit),
        Err(KernelFault {
            code: KernelFaultCode::SnapshotIncompatible,
            ..
        })
    ));

    let mut inconsistent_journal = runtime.snapshot().expect("bounded snapshot");
    inconsistent_journal
        .accepted_inputs
        .push(inconsistent_journal.accepted_inputs[0].clone());
    assert!(matches!(
        KernelRuntime::restore_snapshot(inconsistent_journal),
        Err(KernelFault {
            code: KernelFaultCode::SnapshotIncompatible,
            ..
        })
    ));

    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("fills the configured journal"),
        run_spec: None,
    }));
    runtime.step(KernelInput::new(KernelInputEvent::DeliverSignal {
        delivery_id: "third-input".into(),
        attempt: 1,
        signal: signal(crate::types::signal::Urgency::Normal, "queued"),
    }));
    assert!(matches!(
        runtime.snapshot(),
        Err(KernelFault {
            code: KernelFaultCode::SnapshotIncompatible,
            ..
        })
    ));
    assert_eq!(
        runtime.accepted_snapshot_input_count(),
        2,
        "snapshot journal stays memory-bounded"
    );
}

#[test]
fn snapshot_v2_rejects_a_limit_below_the_existing_journal() {
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
        enabled: true,
    }));
    let rejected = runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
        config: RunConfig {
            reliability: Some(KernelReliabilityConfig {
                snapshot_input_limit: Some(1),
                ..KernelReliabilityConfig::default()
            }),
            ..RunConfig::default()
        },
    }));
    assert!(matches!(
        rejected.faults.as_slice(),
        [KernelFault {
            code: KernelFaultCode::InvalidConfig,
            ..
        }]
    ));
    assert_eq!(
        runtime
            .snapshot()
            .expect("original journal remains valid")
            .accepted_inputs
            .len(),
        1
    );
}

#[test]
fn snapshot_v2_preserves_u64_policy_across_json_hosts() {
    let policy = SchedulerBudget {
        max_tokens: 4096,
        max_turns: 10,
        max_total_tokens: u64::MAX,
        max_wall_ms: Some(u64::MAX - 1),
    };
    let runtime = KernelRuntime::new(policy);
    let encoded = runtime.snapshot_json().expect("encode large policy");
    let value: serde_json::Value = serde_json::from_str(&encoded).expect("snapshot JSON");
    assert_eq!(
        value["initial_policy"]["max_total_tokens"],
        u64::MAX.to_string()
    );
    assert_eq!(
        value["initial_policy"]["max_wall_ms"],
        (u64::MAX - 1).to_string()
    );

    let restored = KernelRuntime::restore_snapshot_json(&encoded).expect("restore large policy");
    assert_eq!(restored.snapshot_json().expect("re-encode"), encoded);

    let mut noncanonical = runtime.snapshot().expect("typed snapshot");
    noncanonical.initial_policy.max_total_tokens = "01".into();
    assert!(matches!(
        KernelRuntime::restore_snapshot(noncanonical),
        Err(KernelFault {
            code: KernelFaultCode::SnapshotIncompatible,
            ..
        })
    ));
}

#[test]
fn load_workflow_resumes_from_completed_nodes() {
    use crate::orchestration::workflow::fanout_synthesize;

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent task"),
        run_spec: None,
    }));
    runtime.clear_test_observations();

    // Resume a 2-worker fanout where worker 0 already completed before the interruption.
    let spec = fanout_synthesize(
        vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
        RuntimeTask::new("synth"),
    );
    let step = runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
        spec,
        parent_session_id: "sess".to_string(),
        resumed_outcomes: vec![
            crate::orchestration::workflow::ResumedNodeOutcome::completed("wf-node0"),
        ],
        resumed_submissions: Vec::new(),
        resumed_submission_bases: Vec::new(),
    }));
    let step = accept_workflow_spawn(&mut runtime, step);

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
