use std::collections::BTreeMap;

use deepstrike_core::context::policy::{ContextPolicyV1, PressureThresholdsPpm};
use deepstrike_core::runtime::{KernelInput, KernelInputEvent, KernelRuntime, KernelSnapshot};
use deepstrike_core::scheduler::policy::SchedulerBudget;
use deepstrike_core::types::message::Message;
use deepstrike_core::types::task::RuntimeTask;
use deepstrike_lab::{
    FactProbe, LabContextOverrides, ReplayError, ReplayOptions, TracePoint, export_snapshot_trace,
    replay_fork,
};

fn input(seq: u64, event: KernelInputEvent) -> KernelInput {
    KernelInput::correlated("op", format!("event-{seq}"), seq, event)
}

fn policy(preserve_recent_turns: u32) -> ContextPolicyV1 {
    ContextPolicyV1 {
        version: 1,
        pressure_thresholds_ppm: PressureThresholdsPpm {
            snip: 700_000,
            micro: 800_000,
            collapse: 900_000,
            auto: 950_000,
            renewal: 980_000,
        },
        target_after_compress_ppm: 650_000,
        preserve_recent_turns,
        renewal_carryover_ppm: 50_000,
        collapse_old_assistant_narration: true,
        idle_micro_compact_minutes: 60,
    }
}

fn fixture_snapshot() -> KernelSnapshot {
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_tokens: 1_024,
        max_turns: 10,
        max_total_tokens: 10_000,
        max_wall_ms: None,
    });
    let mut seq = 1;
    for event in [
        KernelInputEvent::ConfigureRun {
            config: deepstrike_core::runtime::kernel::RunConfig {
                context_policy: Some(policy(2)),
                ..Default::default()
            },
        },
        KernelInputEvent::AddSystemMessage {
            content: "SYSTEM_ANCHOR".into(),
            tokens: 3,
        },
        KernelInputEvent::AddKnowledgeMessage {
            content: "KNOWLEDGE_ANCHOR".into(),
            tokens: 3,
            key: Some("anchor".into()),
            pinned: true,
        },
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("remember project codename ORCHID"),
            run_spec: None,
        },
    ] {
        let step = runtime.step(input(seq, event));
        assert!(step.faults.is_empty(), "{:?}", step.faults);
        seq += 1;
    }
    runtime.snapshot().unwrap()
}

fn completed_fixture_snapshot() -> KernelSnapshot {
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_tokens: 1_024,
        max_turns: 10,
        max_total_tokens: 10_000,
        max_wall_ms: None,
    });
    let configured = runtime.step(input(
        1,
        KernelInputEvent::ConfigureRun {
            config: deepstrike_core::runtime::kernel::RunConfig {
                context_policy: Some(policy(2)),
                ..Default::default()
            },
        },
    ));
    assert!(configured.faults.is_empty());
    let started = runtime.step(input(
        2,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("answer once"),
            run_spec: None,
        },
    ));
    let effect_id = started.actions[0].effect_id.clone();
    let completed = runtime.step(input(
        3,
        KernelInputEvent::ProviderResult {
            effect_id,
            message: Message::assistant("final answer"),
            observed_input_tokens: Some(10),
            observed_output_tokens: Some(3),
            now_ms: None,
            stop_reason: None,
        },
    ));
    assert!(completed.faults.is_empty());
    runtime.snapshot().unwrap()
}

#[test]
fn normalized_report_is_byte_deterministic() {
    let snapshot = fixture_snapshot();
    let probes = vec![FactProbe {
        id: "codename".into(),
        introduced_at: TracePoint::Transaction(4),
        required_at: TracePoint::ProviderTurn(1),
        canonical_value: "ORCHID".into(),
        aliases: vec!["orchid".into()],
        acceptable_handles: vec![],
    }];
    let trace = export_snapshot_trace(&snapshot, BTreeMap::new(), probes).unwrap();
    let options = ReplayOptions {
        context_policy: Some(policy(2)),
        lab_overrides: LabContextOverrides::default(),
    };

    let left = replay_fork(&trace, &options)
        .unwrap()
        .normalized_json()
        .unwrap();
    let right = replay_fork(&trace, &options)
        .unwrap()
        .normalized_json()
        .unwrap();

    assert_eq!(left.as_bytes(), right.as_bytes());
    assert_eq!(
        left.as_bytes(),
        include_str!("goldens/basic-report.json")
            .trim_end()
            .as_bytes()
    );
    assert!(!left.contains("/tmp/"));
    assert!(!left.contains("localhost"));
}

#[test]
fn inserted_provider_demand_is_trace_not_comparable() {
    let snapshot = fixture_snapshot();
    let mut trace = export_snapshot_trace(&snapshot, BTreeMap::new(), vec![]).unwrap();
    let start = trace
        .transactions
        .iter_mut()
        .find(|tx| matches!(tx.input.event, KernelInputEvent::StartRun { .. }))
        .unwrap();
    start.effects.clear();

    let error = replay_fork(&trace, &ReplayOptions::default()).unwrap_err();
    assert!(matches!(error, ReplayError::TraceNotComparable { .. }));
    assert_eq!(error.code(), "trace_not_comparable");
}

#[test]
fn logical_effect_key_rebinds_a_result_without_using_stale_effect_id() {
    let snapshot = completed_fixture_snapshot();
    let mut trace = export_snapshot_trace(&snapshot, BTreeMap::new(), vec![]).unwrap();
    let result = trace
        .transactions
        .iter_mut()
        .find(|transaction| {
            matches!(
                transaction.input.event,
                KernelInputEvent::ProviderResult { .. }
            )
        })
        .unwrap();
    let KernelInputEvent::ProviderResult { effect_id, .. } = &mut result.input.event else {
        unreachable!();
    };
    *effect_id = "stale-step-ordinal-effect-id".into();

    let report = replay_fork(&trace, &ReplayOptions::default()).unwrap();
    assert!(report.comparable);
}

#[test]
fn deleted_provider_demand_is_trace_not_comparable() {
    let snapshot = fixture_snapshot();
    let mut trace = export_snapshot_trace(&snapshot, BTreeMap::new(), vec![]).unwrap();
    let provider = trace
        .transactions
        .iter()
        .flat_map(|transaction| transaction.effects.iter())
        .find(|effect| effect.kind == "provider")
        .unwrap()
        .clone();
    trace.transactions[0]
        .effects
        .push(deepstrike_lab::TraceEffectRecord {
            kind: "provider".into(),
            logical_effect_key: deepstrike_lab::LogicalEffectKey(format!(
                "{}:deleted",
                provider.logical_effect_key.0
            )),
        });

    let error = replay_fork(&trace, &ReplayOptions::default()).unwrap_err();
    assert_eq!(error.code(), "trace_not_comparable");
}

#[test]
fn duplicate_logical_effect_key_fails_closed() {
    let snapshot = fixture_snapshot();
    let mut trace = export_snapshot_trace(&snapshot, BTreeMap::new(), vec![]).unwrap();
    let provider = trace
        .transactions
        .iter()
        .flat_map(|transaction| transaction.effects.iter())
        .find(|effect| effect.kind == "provider")
        .unwrap()
        .clone();
    trace.transactions[0].effects.push(provider);

    let error = replay_fork(&trace, &ReplayOptions::default()).unwrap_err();
    assert!(matches!(error, ReplayError::TraceNotComparable { .. }));
}

#[test]
fn fact_probe_cannot_be_required_before_introduction() {
    let snapshot = fixture_snapshot();
    let probe = FactProbe {
        id: "time-travel".into(),
        introduced_at: TracePoint::Transaction(4),
        required_at: TracePoint::Transaction(3),
        canonical_value: "ORCHID".into(),
        aliases: vec![],
        acceptable_handles: vec![],
    };

    let error = export_snapshot_trace(&snapshot, BTreeMap::new(), vec![probe]).unwrap_err();
    assert_eq!(error.code(), "invalid_trace");
}

#[test]
fn typed_probe_and_structural_invariants_are_reported() {
    let snapshot = fixture_snapshot();
    let probes = vec![FactProbe {
        id: "missing".into(),
        introduced_at: TracePoint::Transaction(4),
        required_at: TracePoint::ProviderTurn(1),
        canonical_value: "NEVER_PRESENT".into(),
        aliases: vec![],
        acceptable_handles: vec!["lab://known-handle".into()],
    }];
    let trace = export_snapshot_trace(&snapshot, BTreeMap::new(), probes).unwrap();
    let report = replay_fork(&trace, &ReplayOptions::default()).unwrap();

    assert_eq!(report.t2.fact_probes.len(), 1);
    assert!(!report.t2.fact_probes[0].retained);
    assert!(
        report
            .t2
            .invariants
            .iter()
            .all(|invariant| !invariant.name.is_empty())
    );
    assert!(
        report
            .t1
            .provider_turns
            .iter()
            .all(|turn| turn.render_tokens <= 1_024)
    );
}
