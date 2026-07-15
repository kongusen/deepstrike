use std::collections::BTreeMap;

use deepstrike_core::context::policy::{ContextPolicyV1, PressureThresholdsPpm};
use deepstrike_core::runtime::{KernelInput, KernelInputEvent, KernelRuntime, KernelSnapshot};
use deepstrike_core::scheduler::policy::SchedulerBudget;
use deepstrike_core::types::message::{Content, ContentPart, Message, ToolCall, ToolResult};
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

// ── Compaction golden (W2-S2 A/B pre-gate for the compression pipeline) ─────────────
//
// `fixture_snapshot` above renders once at a cold ~4% rho, so its golden pins the
// no-pressure path — determinism and structural invariants, but `compression_count: 0`.
// The fixture below drives a real tool-call loop over a preloaded history heavy enough
// that the turn-boundary eviction checkpoint fires a compaction before the second
// provider turn. Its golden pins the *compression pipeline* itself (tier, prefix
// invalidation, post-compaction fact recall), so any P1 change to compression bytes
// moves the golden and must ship a justification — the regression environment the spec
// requires to precede compression-algorithm work.

fn history_event(role: &str, content: &str, tokens: u32) -> KernelInputEvent {
    let message = if role == "user" {
        Message::user(content)
    } else {
        Message::assistant(content)
    };
    KernelInputEvent::AddHistoryMessage {
        message,
        tokens: Some(tokens),
    }
}

fn compaction_fixture_snapshot() -> KernelSnapshot {
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_tokens: 1_000,
        max_turns: 20,
        max_total_tokens: 1_000_000,
        max_wall_ms: None,
    });
    let mut seq = 1u64;
    let drive = |runtime: &mut KernelRuntime, seq: &mut u64, event: KernelInputEvent| {
        let step = runtime.step(input(*seq, event));
        assert!(step.faults.is_empty(), "seq {}: {:?}", *seq, step.faults);
        *seq += 1;
        step
    };

    drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::ConfigureRun {
            config: deepstrike_core::runtime::kernel::RunConfig {
                context_policy: Some(policy(1)),
                ..Default::default()
            },
        },
    );
    drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::AddSystemMessage {
            content: "SYSTEM_ANCHOR".into(),
            tokens: 8,
        },
    );
    drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::AddKnowledgeMessage {
            content: "pinned anchor note".into(),
            tokens: 8,
            key: Some("anchor".into()),
            pinned: true,
        },
    );
    // Old history — the fact under test (`ORCHID`) is introduced in transaction 4, well before
    // the compaction, so the T2 probe genuinely measures post-compaction recall (not preservation
    // of a recent turn).
    drive(
        &mut runtime,
        &mut seq,
        history_event("user", "recall the project codename ORCHID and its release", 220),
    );
    drive(
        &mut runtime,
        &mut seq,
        history_event(
            "assistant",
            "Understood; codename ORCHID release plan noted with retry decision",
            220,
        ),
    );
    drive(
        &mut runtime,
        &mut seq,
        history_event("user", "routine chatter number one about unrelated things", 200),
    );
    drive(
        &mut runtime,
        &mut seq,
        history_event("assistant", "acknowledged the routine chatter number one", 120),
    );

    let started = drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("what is the codename"),
            run_spec: None,
        },
    );
    let effect1 = started.actions[0].effect_id.clone();

    // Turn 1 answers with a tool call so the loop advances to a second provider turn — the
    // turn-boundary eviction checkpoint runs between them and compacts the pressured history.
    let mut assistant = Message::assistant("let me look it up");
    assistant.tool_calls.push(ToolCall {
        id: "call-1".into(),
        name: "search".into(),
        arguments: serde_json::json!({ "q": "codename" }),
    });
    let tool_step = drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::ProviderResult {
            effect_id: effect1,
            message: assistant,
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    );
    let tool_effect = tool_step.actions[0].effect_id.clone();
    let render2 = drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::ToolResults {
            effect_id: tool_effect,
            results: vec![ToolResult {
                call_id: "call-1".into(),
                output: Content::Text("ORCHID".into()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: Some(4),
            }],
        },
    );
    let effect2 = render2.actions[0].effect_id.clone();
    drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::ProviderResult {
            effect_id: effect2,
            message: Message::assistant("The codename is ORCHID."),
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    );
    runtime.snapshot().unwrap()
}

fn compaction_probe() -> FactProbe {
    FactProbe {
        id: "codename".into(),
        introduced_at: TracePoint::Transaction(4),
        required_at: TracePoint::ProviderTurn(2),
        canonical_value: "ORCHID".into(),
        aliases: vec![],
        acceptable_handles: vec![],
    }
}

#[test]
fn compaction_report_is_byte_deterministic() {
    let snapshot = compaction_fixture_snapshot();
    let trace =
        export_snapshot_trace(&snapshot, BTreeMap::new(), vec![compaction_probe()]).unwrap();
    let options = ReplayOptions {
        context_policy: Some(policy(1)),
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
        include_str!("goldens/compaction-report.json")
            .trim_end()
            .as_bytes()
    );
    assert!(!left.contains("/tmp/"));
    assert!(!left.contains("localhost"));

    // The golden must actually exercise the compression pipeline (else it is the cold-start
    // golden again). Pin the load-bearing facts here so a fixture edit that silently stops
    // compressing fails loudly, not just as an opaque byte diff.
    let report = replay_fork(&trace, &options).unwrap();
    assert!(report.comparable);
    assert_eq!(report.t1.compression_count, 1);
    assert_eq!(report.t1.prefix_invalidation_count, 1);
    assert!(report.t2.fact_probes[0].retained, "fact recall survives compaction");
    assert!(report.t2.invariants.iter().all(|i| i.passed));
}

#[test]
fn policy_ab_produces_comparable_but_divergent_reports() {
    // The A/B contract: replaying ONE trace under two context policies that keep the same
    // provider-demand structure yields two *comparable* reports whose mechanical T1 metrics
    // diverge with the policy. This is what makes the lab a pre-gate for compression tuning —
    // a knob change is observable as a report delta, not a silent behavior drift.
    let snapshot = compaction_fixture_snapshot();
    let trace =
        export_snapshot_trace(&snapshot, BTreeMap::new(), vec![compaction_probe()]).unwrap();

    let tight = replay_fork(
        &trace,
        &ReplayOptions {
            context_policy: Some(policy(1)),
            lab_overrides: LabContextOverrides::default(),
        },
    )
    .unwrap();
    let loose = replay_fork(
        &trace,
        &ReplayOptions {
            context_policy: Some(policy(3)),
            lab_overrides: LabContextOverrides::default(),
        },
    )
    .unwrap();

    // Both fork the same recorded provider demand — neither is quarantined as not-comparable.
    assert!(tight.comparable && loose.comparable);
    // Both still compress and still recall the fact — the divergence is in *how much* context
    // the post-compaction render carries, not in correctness.
    assert_eq!(tight.t1.compression_count, 1);
    assert_eq!(loose.t1.compression_count, 1);
    assert!(tight.t2.fact_probes[0].retained && loose.t2.fact_probes[0].retained);
    // Preserving more recent turns changes the post-compaction render size — the observable A/B.
    let tight_turn2 = tight.t1.provider_turns[1].render_tokens;
    let loose_turn2 = loose.t1.provider_turns[1].render_tokens;
    assert_ne!(
        tight_turn2, loose_turn2,
        "preserve_recent_turns must move the post-compaction render size"
    );
    assert_ne!(
        tight.normalized_json().unwrap(),
        loose.normalized_json().unwrap()
    );
}

#[test]
fn policy_that_reshapes_provider_demand_fails_closed() {
    // A policy aggressive enough to change the causal structure (heavy compaction defers the
    // second provider turn behind a host page-out, or renewal restarts the sprint) must be
    // rejected as `trace_not_comparable`, never silently compared against the recorded trace.
    // This is the guard that keeps an A/B honest: only same-shape forks are diffed.
    let snapshot = compaction_fixture_snapshot();
    let trace =
        export_snapshot_trace(&snapshot, BTreeMap::new(), vec![compaction_probe()]).unwrap();

    let aggressive = ContextPolicyV1 {
        version: 1,
        pressure_thresholds_ppm: PressureThresholdsPpm {
            snip: 100_000,
            micro: 200_000,
            collapse: 300_000,
            auto: 400_000,
            renewal: 980_000,
        },
        target_after_compress_ppm: 80_000,
        preserve_recent_turns: 1,
        renewal_carryover_ppm: 40_000,
        collapse_old_assistant_narration: true,
        idle_micro_compact_minutes: 60,
    };

    let error = replay_fork(
        &trace,
        &ReplayOptions {
            context_policy: Some(aggressive),
            lab_overrides: LabContextOverrides::default(),
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), "trace_not_comparable");
}

// ── Image-message compaction golden ───────────────────────────────────────────
//
// Pins that Image ContentParts contribute real token weight to pressure (ρ) via
// ContentPart::estimate_tokens — not the historical `count_part → 1` blind spot.
// Without the modality heuristic, high-detail images would under-count and skip
// compaction; this golden fails loudly if that regression returns.

fn image_history_event(parts: Vec<ContentPart>) -> KernelInputEvent {
    KernelInputEvent::AddHistoryMessage {
        message: Message::user_multimodal(parts),
        // None → engine.count_message → estimate_tokens for Image parts.
        tokens: None,
    }
}

fn image_compaction_fixture_snapshot() -> KernelSnapshot {
    let mut runtime = KernelRuntime::new(SchedulerBudget {
        max_tokens: 1_000,
        max_turns: 20,
        max_total_tokens: 1_000_000,
        max_wall_ms: None,
    });
    let mut seq = 1u64;
    let drive = |runtime: &mut KernelRuntime, seq: &mut u64, event: KernelInputEvent| {
        let step = runtime.step(input(*seq, event));
        assert!(step.faults.is_empty(), "seq {}: {:?}", *seq, step.faults);
        *seq += 1;
        step
    };

    drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::ConfigureRun {
            config: deepstrike_core::runtime::kernel::RunConfig {
                context_policy: Some(policy(1)),
                ..Default::default()
            },
        },
    );
    drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::AddSystemMessage {
            content: "SYSTEM_ANCHOR".into(),
            tokens: 8,
        },
    );
    drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::AddKnowledgeMessage {
            content: "pinned anchor note".into(),
            tokens: 8,
            key: Some("anchor".into()),
            pinned: true,
        },
    );
    // Fact + auto-detail image. tokens: None so ledger uses estimate_tokens (255 for image).
    // The image's real token weight lands this in the snipcompact band (ρ≈0.73), not AutoCompact
    // page-out — keeps the provider-demand shape comparable for lab replay.
    drive(
        &mut runtime,
        &mut seq,
        image_history_event(vec![
            ContentPart::text(
                "recall the project codename ORCHID and its release from the attached board photo",
            ),
            ContentPart::image_base64("Qk08AAAAAAAA", "image/png"),
        ]),
    );
    drive(
        &mut runtime,
        &mut seq,
        history_event(
            "assistant",
            "Understood; codename ORCHID release plan noted with retry decision",
            220,
        ),
    );
    drive(
        &mut runtime,
        &mut seq,
        image_history_event(vec![
            ContentPart::text("routine chatter number one about unrelated things and filler"),
            ContentPart::image_base64_with_detail("Qk08AAAAAAAA", "image/png", "low"),
        ]),
    );
    drive(
        &mut runtime,
        &mut seq,
        history_event("assistant", "acknowledged the routine chatter number one", 120),
    );

    let started = drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("what is the codename"),
            run_spec: None,
        },
    );
    let effect1 = started.actions[0].effect_id.clone();

    let mut assistant = Message::assistant("let me look it up");
    assistant.tool_calls.push(ToolCall {
        id: "call-1".into(),
        name: "search".into(),
        arguments: serde_json::json!({ "q": "codename" }),
    });
    let tool_step = drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::ProviderResult {
            effect_id: effect1,
            message: assistant,
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    );
    let tool_effect = tool_step.actions[0].effect_id.clone();
    let after_tools = drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::ToolResults {
            effect_id: tool_effect,
            results: vec![ToolResult {
                call_id: "call-1".into(),
                output: Content::Text("ORCHID".into()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: Some(4),
            }],
        },
    );
    // Microcompact returns CallProvider; AutoCompact may emit ArchivePageOut first.
    let mut pending = after_tools;
    loop {
        match pending.actions.first().map(|a| &a.effect) {
            Some(deepstrike_core::runtime::KernelEffect::ArchivePageOut { .. }) => {
                let effect_id = pending.actions[0].effect_id.clone();
                pending = drive(
                    &mut runtime,
                    &mut seq,
                    KernelInputEvent::PageOutArchiveResult {
                        effect_id,
                        archive_ref: Some("lab-archive:image-compaction".into()),
                        error: None,
                    },
                );
            }
            Some(deepstrike_core::runtime::KernelEffect::CallProvider { .. }) => break,
            other => panic!("expected CallProvider or ArchivePageOut after tools, got {other:?}"),
        }
    }
    let effect2 = pending.actions[0].effect_id.clone();
    drive(
        &mut runtime,
        &mut seq,
        KernelInputEvent::ProviderResult {
            effect_id: effect2,
            message: Message::assistant("The codename is ORCHID."),
            observed_input_tokens: None,
            observed_output_tokens: None,
            now_ms: None,
            stop_reason: None,
        },
    );
    runtime.snapshot().unwrap()
}

fn image_compaction_probe() -> FactProbe {
    FactProbe {
        id: "codename".into(),
        // Configure(1) + System(2) + Knowledge(3) + ORCHID image user(4)
        introduced_at: TracePoint::Transaction(4),
        required_at: TracePoint::ProviderTurn(2),
        canonical_value: "ORCHID".into(),
        aliases: vec![],
        acceptable_handles: vec![],
    }
}

#[test]
fn image_compaction_report_is_byte_deterministic() {
    // Pin that Image parts land on the engine ledger via estimate_tokens before the run.
    let sample = Message::user_multimodal(vec![
        ContentPart::text("x"),
        ContentPart::image_base64("Qk08AAAAAAAA", "image/png"),
    ]);
    let counted =
        deepstrike_core::context::token_engine::ContextTokenEngine::char_approx().count_message(&sample);
    assert!(
        counted >= 255,
        "image auto detail must contribute ≥255 tokens, got {counted}"
    );

    let snapshot = image_compaction_fixture_snapshot();
    let trace =
        export_snapshot_trace(&snapshot, BTreeMap::new(), vec![image_compaction_probe()]).unwrap();
    let options = ReplayOptions {
        context_policy: Some(policy(1)),
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
        include_str!("goldens/image-compaction-report.json")
            .trim_end()
            .as_bytes()
    );

    let report = replay_fork(&trace, &options).unwrap();
    assert!(report.comparable);
    assert!(
        report.t1.compression_count >= 1,
        "image-weighted history must engage a compression tier; got compression_count={}",
        report.t1.compression_count
    );
    assert!(report.t2.fact_probes[0].retained, "fact recall survives image compaction");
    assert!(report.t2.invariants.iter().all(|i| i.passed));
}
