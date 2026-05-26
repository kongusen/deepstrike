#![allow(deprecated)]

// Phase 6 — Milestone Contracts
// G6 gate: verifier 驱动 phase advance；blocked retry 可控；unlock capabilities 带 provenance

use compact_str::CompactString;
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::scheduler::state_machine::*;
use deepstrike_core::types::message::*;
use deepstrike_core::types::milestone::*;
use deepstrike_core::types::result::TerminationReason;
use deepstrike_core::types::task::RuntimeTask;

fn default_sm() -> LoopStateMachine {
    LoopStateMachine::new(LoopPolicy {
        max_tokens: 128_000,
        ..LoopPolicy::default()
    })
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

fn tool_response(call_id: &str) -> LoopEvent {
    LoopEvent::LLMResponse {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            tool_calls: vec![ToolCall {
                id: CompactString::new(call_id),
                name: CompactString::new("some_tool"),
                arguments: serde_json::json!({}),
            }],
            token_count: None,
        },
    }
}

// ─── G6 gate: verifier carried in EvaluateMilestone ────────────────────────

#[test]
fn evaluate_milestone_carries_verifier_type() {
    let mut sm = default_sm();
    sm.load_milestone_contract(
        MilestoneContract::new().phase(
            MilestonePhase::new("verify")
                .with_criterion("output is correct")
                .with_verifier(MilestoneVerifier::MachineCheck),
        ),
    );
    sm.start(RuntimeTask::new("test"));
    let action = sm.feed(text_response());

    if let LoopAction::EvaluateMilestone { verifier, phase_id, .. } = action {
        assert_eq!(phase_id, "verify");
        assert_eq!(verifier, Some(MilestoneVerifier::MachineCheck));
    } else {
        panic!("expected EvaluateMilestone, got: {action:?}");
    }
}

#[test]
fn evaluate_milestone_carries_required_evidence() {
    let mut sm = default_sm();
    sm.load_milestone_contract(
        MilestoneContract::new().phase(
            MilestonePhase::new("test")
                .requiring_evidence("test_suite_pass")
                .requiring_evidence("coverage_80pct"),
        ),
    );
    sm.start(RuntimeTask::new("test"));
    let action = sm.feed(text_response());

    if let LoopAction::EvaluateMilestone { required_evidence, .. } = action {
        assert_eq!(required_evidence, vec!["test_suite_pass", "coverage_80pct"]);
    } else {
        panic!("expected EvaluateMilestone");
    }
}

#[test]
fn evaluate_milestone_defaults_to_no_verifier_when_unset() {
    let mut sm = default_sm();
    sm.load_milestone_contract(
        MilestoneContract::new().phase(MilestonePhase::new("plan")),
    );
    sm.start(RuntimeTask::new("test"));
    let action = sm.feed(text_response());

    if let LoopAction::EvaluateMilestone { verifier, .. } = action {
        assert_eq!(verifier, None, "verifier should be None when not configured");
    } else {
        panic!("expected EvaluateMilestone");
    }
}

// ─── G6 gate: blocked retry controllable ───────────────────────────────────

#[test]
fn retry_policy_terminate_on_exceed() {
    let mut sm = default_sm();
    // max_attempts=2: first fail is normal block, second fail exceeds budget
    sm.load_milestone_contract(
        MilestoneContract::new().phase(
            MilestonePhase::new("plan")
                .with_retry_policy(RetryPolicy::max(2))
                .with_rollback_policy(MilestoneRollbackPolicy::Terminate),
        ),
    );
    sm.start(RuntimeTask::new("test"));
    sm.feed(text_response()); // → EvaluateMilestone
    sm.take_observations(); // drain start/llm obs

    // First fail — blocked_count=1, not exceeded (1 < 2)
    let action1 = sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::fail("plan", "not done yet"),
    });
    let obs1 = sm.take_observations();
    assert!(
        matches!(action1, LoopAction::CallLLM { .. }),
        "first fail should continue (CallLLM), not terminate"
    );
    assert!(
        obs1.iter().any(|o| matches!(o, LoopObservation::MilestoneBlocked { .. })),
        "first fail should emit MilestoneBlocked, got: {obs1:?}"
    );

    sm.feed(text_response()); // → EvaluateMilestone (second attempt)
    sm.take_observations();

    // Second fail — blocked_count=2, exceeded (2 >= 2) → Terminate
    let action2 = sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::fail("plan", "still wrong"),
    });
    assert!(
        matches!(action2, LoopAction::Done { ref result } if result.termination == TerminationReason::MilestoneExceeded),
        "second fail should terminate with MilestoneExceeded, got: {action2:?}"
    );
}

#[test]
fn retry_policy_zero_means_unlimited() {
    let mut sm = default_sm();
    sm.load_milestone_contract(
        MilestoneContract::new().phase(
            MilestonePhase::new("plan")
                .with_retry_policy(RetryPolicy::max(0)), // 0 = unlimited
        ),
    );
    sm.start(RuntimeTask::new("test"));

    // Block many times — should never terminate
    for _ in 0..10 {
        sm.feed(text_response());
        sm.feed(LoopEvent::MilestoneResult {
            result: MilestoneCheckResult::fail("plan", "nope"),
        });
    }
    assert!(!sm.is_terminal(), "unlimited retry should not terminate");
}

#[test]
fn retry_policy_rollback_on_exceed() {
    let mut sm = default_sm();
    sm.load_milestone_contract(
        MilestoneContract::new().phase(
            MilestonePhase::new("plan")
                .with_retry_policy(RetryPolicy::max(1))
                .with_rollback_policy(MilestoneRollbackPolicy::Rollback),
        ),
    );
    sm.start(RuntimeTask::new("test"));
    sm.feed(text_response());

    // First block — budget exhausted immediately (max=1)
    sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::fail("plan", "bad"),
    });
    sm.feed(text_response());
    sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::fail("plan", "bad again"),
    });
    let obs = sm.take_observations();
    assert!(
        obs.iter().any(|o| matches!(o, LoopObservation::Rollbacked { .. })),
        "should rollback when MilestoneRollbackPolicy::Rollback and budget exceeded"
    );
}

#[test]
fn no_retry_policy_means_unlimited_retries() {
    let mut sm = default_sm();
    sm.load_milestone_contract(
        MilestoneContract::new().phase(MilestonePhase::new("plan")),
    );
    sm.start(RuntimeTask::new("test"));

    for _ in 0..5 {
        sm.feed(text_response());
        sm.feed(LoopEvent::MilestoneResult {
            result: MilestoneCheckResult::fail("plan", "nope"),
        });
    }
    assert!(!sm.is_terminal(), "no retry_policy → unlimited retries");
}

// ─── G6 gate: capability unlock carries milestone provenance ────────────────

#[test]
fn capability_unlock_has_milestone_provenance() {
    use deepstrike_core::types::capability::{CapabilityDescriptor, CapabilityKind};

    let schema = ToolSchema {
        name: CompactString::new("deploy"),
        description: "deploy tool".into(),
        parameters: serde_json::json!({"type":"object"}),
    };
    let cap = CapabilityDescriptor::tool(schema);

    let mut sm = default_sm();
    sm.load_milestone_contract(
        MilestoneContract::new().phase(
            MilestonePhase::new("plan").unlocking(cap),
        ),
    );
    sm.start(RuntimeTask::new("test"));
    sm.feed(text_response());
    sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::pass("plan"),
    });

    let obs = sm.take_observations();
    let capability_changed = obs.iter().find(|o| {
        matches!(o, LoopObservation::CapabilityChanged { mounted_by: Some(mb), .. } if mb.starts_with("milestone:"))
    });
    assert!(
        capability_changed.is_some(),
        "capability unlock should carry mounted_by = 'milestone:{{phase_id}}'"
    );

    if let Some(LoopObservation::CapabilityChanged { mounted_by, mount_reason, .. }) = capability_changed {
        assert_eq!(mounted_by.as_deref(), Some("milestone:plan"));
        assert_eq!(mount_reason.as_deref(), Some("phase_advance"));
    }
}

#[test]
fn milestone_advance_resets_blocked_count() {
    let mut sm = default_sm();
    sm.load_milestone_contract(
        MilestoneContract::new()
            .phase(
                MilestonePhase::new("plan")
                    .with_retry_policy(RetryPolicy::max(2))
                    .with_rollback_policy(MilestoneRollbackPolicy::Terminate),
            )
            .phase(MilestonePhase::new("implement")),
    );
    sm.start(RuntimeTask::new("test"));
    sm.feed(text_response());

    // Block once on phase "plan"
    sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::fail("plan", "not done"),
    });
    sm.feed(text_response());

    // Advance phase "plan"
    sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::pass("plan"),
    });
    sm.feed(text_response());

    // Block on "implement" — blocked count should have reset, so 2 more blocks allowed
    sm.feed(LoopEvent::MilestoneResult {
        result: MilestoneCheckResult::fail("implement", "not done"),
    });
    assert!(
        !sm.is_terminal(),
        "blocked count should reset on phase advance; first block on new phase should not terminate"
    );
}

// ─── MilestoneVerifier builder ──────────────────────────────────────────────

#[test]
fn external_command_verifier_serializes() {
    let v = MilestoneVerifier::ExternalCommand { cmd: "make test".into() };
    let json = serde_json::to_string(&v).unwrap();
    assert!(json.contains("external_command"));
    assert!(json.contains("make test"));
}

#[test]
fn milestone_phase_builder_chains() {
    let phase = MilestonePhase::new("verify")
        .with_verifier(MilestoneVerifier::HumanApproval)
        .with_retry_policy(RetryPolicy::max(3))
        .with_rollback_policy(MilestoneRollbackPolicy::Rollback)
        .requiring_evidence("sign_off");

    assert_eq!(phase.verifier, Some(MilestoneVerifier::HumanApproval));
    assert_eq!(phase.retry_policy.unwrap().max_attempts, 3);
    assert_eq!(phase.rollback_policy, MilestoneRollbackPolicy::Rollback);
    assert_eq!(phase.required_evidence, vec!["sign_off"]);
}
