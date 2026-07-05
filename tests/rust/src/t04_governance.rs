use compact_str::CompactString;
use deepstrike_core::AgentIdentity;
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::types::message::ToolCall;
use deepstrike_core::types::policy::{CallerContext, GovernanceVerdict};

fn call(name: &str) -> ToolCall {
    ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new(name),
        arguments: serde_json::Value::Null,
    }
}

fn caller() -> CallerContext {
    AgentIdentity::new("agent-1", "session-1")
}

// ─── Default pipeline ───────────────────────────────────────────────────────

#[test]
fn default_allow_pipeline_allows_all() {
    let mut pipeline = GovernancePipeline::default();
    pipeline.set_time(1000);
    let v = pipeline.evaluate(&call("anything"), &caller());
    assert!(matches!(v, GovernanceVerdict::Allow));
}

// ─── Permission rules ──────────────────────────────────────────────────────

#[test]
fn deny_rule_blocks_matching_tool() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "danger.*".into(),
        action: PermissionAction::Deny,
    });
    pipeline.set_time(1000);

    let v = pipeline.evaluate(&call("danger.delete"), &caller());
    assert!(matches!(
        v,
        GovernanceVerdict::Deny {
            stage: "permission",
            ..
        }
    ));
}

#[test]
fn deny_rule_does_not_block_non_matching() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "danger.*".into(),
        action: PermissionAction::Deny,
    });
    pipeline.set_time(1000);

    let v = pipeline.evaluate(&call("safe_read"), &caller());
    assert!(matches!(v, GovernanceVerdict::Allow));
}

#[test]
fn deny_default_blocks_all_tools() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Deny);
    pipeline.set_time(1000);
    let v = pipeline.evaluate(&call("anything"), &caller());
    assert!(matches!(
        v,
        GovernanceVerdict::Deny {
            stage: "permission",
            ..
        }
    ));
}

#[test]
fn ask_user_rule_produces_ask_user_verdict() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "sensitive.*".into(),
        action: PermissionAction::AskUser,
    });
    pipeline.set_time(1000);

    let v = pipeline.evaluate(&call("sensitive.delete"), &caller());
    assert!(matches!(v, GovernanceVerdict::AskUser { .. }));
}

// ─── Veto authority ─────────────────────────────────────────────────────────

#[test]
fn veto_blocks_tool() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.veto.block_tool("nuke");
    pipeline.set_time(1000);

    let v = pipeline.evaluate(&call("nuke"), &caller());
    assert!(matches!(v, GovernanceVerdict::Deny { stage: "veto", .. }));
}

// ─── Rate limiting ──────────────────────────────────────────────────────────

#[test]
fn rate_limiter_allows_within_limit() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.rate_limiter.set_limit(
        "api_call",
        RateLimit {
            max_calls: 2,
            window_ms: 1000,
        },
    );
    pipeline.set_time(100);

    assert!(matches!(
        pipeline.evaluate(&call("api_call"), &caller()),
        GovernanceVerdict::Allow
    ));
    assert!(matches!(
        pipeline.evaluate(&call("api_call"), &caller()),
        GovernanceVerdict::Allow
    ));
    assert!(matches!(
        pipeline.evaluate(&call("api_call"), &caller()),
        GovernanceVerdict::RateLimited { .. }
    ));
}

#[test]
fn rate_limiter_window_expires() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.rate_limiter.set_limit(
        "api",
        RateLimit {
            max_calls: 1,
            window_ms: 100,
        },
    );

    pipeline.set_time(0);
    assert!(matches!(
        pipeline.evaluate(&call("api"), &caller()),
        GovernanceVerdict::Allow
    ));
    assert!(matches!(
        pipeline.evaluate(&call("api"), &caller()),
        GovernanceVerdict::RateLimited { .. }
    ));

    pipeline.set_time(200);
    assert!(matches!(
        pipeline.evaluate(&call("api"), &caller()),
        GovernanceVerdict::Allow
    ));
}

// ─── Pipeline order: permission → veto → rate_limit ─────────────────────────

#[test]
fn permission_deny_stops_before_veto() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "blocked".into(),
        action: PermissionAction::Deny,
    });
    pipeline.veto.block_tool("blocked");
    pipeline.set_time(1000);

    let v = pipeline.evaluate(&call("blocked"), &caller());
    match v {
        GovernanceVerdict::Deny { stage, .. } => assert_eq!(stage, "permission"),
        _ => panic!("expected permission deny"),
    }
}

#[test]
fn veto_stops_before_rate_limit() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.veto.block_tool("vetoed");
    pipeline.rate_limiter.set_limit(
        "vetoed",
        RateLimit {
            max_calls: 0,
            window_ms: 1000,
        },
    );
    pipeline.set_time(1000);

    let v = pipeline.evaluate(&call("vetoed"), &caller());
    match v {
        GovernanceVerdict::Deny { stage, .. } => assert_eq!(stage, "veto"),
        _ => panic!("expected veto deny"),
    }
}

// ─── Audit ──────────────────────────────────────────────────────────────────

// ─── Wildcard patterns ──────────────────────────────────────────────────────

#[test]
fn wildcard_star_matches_all() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "*".into(),
        action: PermissionAction::Deny,
    });
    pipeline.set_time(1000);

    assert!(matches!(
        pipeline.evaluate(&call("anything"), &caller()),
        GovernanceVerdict::Deny { .. }
    ));
}

#[test]
fn suffix_wildcard_matches() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "db.*".into(),
        action: PermissionAction::Deny,
    });
    pipeline.set_time(1000);

    assert!(matches!(
        pipeline.evaluate(&call("db.drop"), &caller()),
        GovernanceVerdict::Deny { .. }
    ));
    assert!(matches!(
        pipeline.evaluate(&call("db.query"), &caller()),
        GovernanceVerdict::Deny { .. }
    ));
    assert!(matches!(
        pipeline.evaluate(&call("file.read"), &caller()),
        GovernanceVerdict::Allow
    ));
}

#[test]
fn prefix_wildcard_matches() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "*.delete".into(),
        action: PermissionAction::Deny,
    });
    pipeline.set_time(1000);

    assert!(matches!(
        pipeline.evaluate(&call("fs.delete"), &caller()),
        GovernanceVerdict::Deny { .. }
    ));
    assert!(matches!(
        pipeline.evaluate(&call("fs.read"), &caller()),
        GovernanceVerdict::Allow
    ));
}

// ─── ask_user verdict ────────────────────────────────────────────────────────

#[test]
fn ask_user_verdict_is_distinct_from_deny() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "sensitive.*".into(),
        action: PermissionAction::AskUser,
    });
    pipeline.set_time(1000);

    let v_ask = pipeline.evaluate(&call("sensitive.delete"), &caller());
    let v_deny = pipeline.evaluate(&call("nuke_everything"), &caller());

    assert!(
        matches!(v_ask, GovernanceVerdict::AskUser { .. }),
        "ask_user rule must produce AskUser verdict, got {v_ask:?}",
    );
    assert!(
        matches!(v_deny, GovernanceVerdict::Allow),
        "non-matching tool must be allowed, got {v_deny:?}",
    );
}

#[test]
fn ask_user_is_not_downgraded_by_passing_stages() {
    // AskUser must survive evaluation when no Deny is present (severity fold keeps it).
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.set_time(1000);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "sensitive_op".into(),
        action: PermissionAction::AskUser,
    });
    let v = pipeline.evaluate(&call("sensitive_op"), &caller());
    assert!(
        matches!(v, GovernanceVerdict::AskUser { .. }),
        "AskUser must survive when no Deny present",
    );
}

#[test]
fn deny_overrides_ask_user() {
    // A veto Deny must override a permission AskUser on the same call.
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.set_time(1000);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "sensitive_op".into(),
        action: PermissionAction::AskUser,
    });
    pipeline.veto.block_tool("sensitive_op");
    let v = pipeline.evaluate(&call("sensitive_op"), &caller());
    assert!(
        matches!(v, GovernanceVerdict::Deny { stage: "veto", .. }),
        "Deny must override AskUser",
    );
}

// ─── ToolDenied schema consistency ───────────────────────────────────────────
// Verify the GovernancePipeline produces deny/ask_user verdicts that all SDKs
// can serialize consistently (field names, kinds).

#[test]
fn governance_verdict_kinds_are_stable() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.veto.block_tool("nuke");
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "approval_needed".into(),
        action: PermissionAction::AskUser,
    });
    pipeline.set_time(1000);

    let deny = pipeline.evaluate(&call("nuke"), &caller());
    let ask = pipeline.evaluate(&call("approval_needed"), &caller());
    let allow = pipeline.evaluate(&call("safe_read"), &caller());

    assert!(matches!(deny, GovernanceVerdict::Deny { stage: "veto", .. }));
    assert!(matches!(ask, GovernanceVerdict::AskUser { .. }));
    assert!(matches!(allow, GovernanceVerdict::Allow));
}
