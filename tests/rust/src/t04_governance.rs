use compact_str::CompactString;
use deepstrike_core::AgentIdentity;
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::types::message::ToolCall;
use deepstrike_core::types::policy::{CallerContext, GovernanceVerdict, VetoCheck};

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

#[test]
fn default_allow_pipeline_records_audit() {
    let mut pipeline = GovernancePipeline::default();
    pipeline.set_time(1000);
    pipeline.evaluate(&call("test"), &caller());
    assert_eq!(pipeline.audit.len(), 1);
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

#[test]
fn veto_closure_check() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.veto.add_check(|c: &ToolCall, _: &CallerContext| {
        if c.name.as_str().starts_with("danger_") {
            Some(format!("blocked: {}", c.name))
        } else {
            None
        }
    });
    pipeline.set_time(1000);

    let v = pipeline.evaluate(&call("danger_exec"), &caller());
    assert!(matches!(v, GovernanceVerdict::Deny { stage: "veto", .. }));
    assert!(matches!(
        pipeline.evaluate(&call("safe_read"), &caller()),
        GovernanceVerdict::Allow
    ));
}

#[test]
fn veto_trait_impl_check() {
    struct BlockNet;
    impl VetoCheck for BlockNet {
        fn check(&self, call: &ToolCall, _caller: &CallerContext) -> Option<String> {
            if call.name.as_str().contains("net") {
                Some("network access vetoed".into())
            } else {
                None
            }
        }
    }

    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.veto.add_check(BlockNet);
    pipeline.set_time(1000);

    assert!(matches!(
        pipeline.evaluate(&call("http_net_get"), &caller()),
        GovernanceVerdict::Deny { stage: "veto", .. }
    ));
    assert!(matches!(
        pipeline.evaluate(&call("read_file"), &caller()),
        GovernanceVerdict::Allow
    ));
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

#[test]
fn audit_records_multiple_evaluations() {
    let mut pipeline = GovernancePipeline::default();
    pipeline.set_time(1000);
    pipeline.evaluate(&call("a"), &caller());
    pipeline.evaluate(&call("b"), &caller());
    pipeline.evaluate(&call("c"), &caller());
    assert_eq!(pipeline.audit.len(), 3);
}

#[test]
fn audit_records_denials() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Deny);
    pipeline.set_time(1000);
    pipeline.evaluate(&call("blocked"), &caller());
    assert_eq!(pipeline.audit.len(), 1);
}

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

#[test]
fn sandbox_policy_enforces_directories() {
    use deepstrike_core::governance::sandbox::SandboxProfile;

    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.set_sandbox_profile(SandboxProfile {
        allow_network: true,
        allow_fs_read: vec!["/tmp/safe".to_string()],
        allow_fs_write: vec!["/tmp/safe/write".to_string()],
    });
    pipeline.set_time(1000);

    // 1. Safe read
    let call_read_safe = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("read_file"),
        arguments: serde_json::json!({ "path": "/tmp/safe/file.txt" }),
    };
    assert!(matches!(
        pipeline.evaluate(&call_read_safe, &caller()),
        GovernanceVerdict::Allow
    ));

    // 2. Unsafe read
    let call_read_unsafe = ToolCall {
        id: CompactString::new("c2"),
        name: CompactString::new("read_file"),
        arguments: serde_json::json!({ "path": "/etc/passwd" }),
    };
    let v_read = pipeline.evaluate(&call_read_unsafe, &caller());
    assert!(
        matches!(v_read, GovernanceVerdict::Deny { stage: "sandbox_policy", .. }),
        "Expected sandbox_policy deny for /etc/passwd: got {:?}", v_read
    );

    // 3. Safe write
    let call_write_safe = ToolCall {
        id: CompactString::new("c3"),
        name: CompactString::new("write_file"),
        arguments: serde_json::json!({ "path": "/tmp/safe/write/output.txt" }),
    };
    assert!(matches!(
        pipeline.evaluate(&call_write_safe, &caller()),
        GovernanceVerdict::Allow
    ));

    // 4. Unsafe write
    let call_write_unsafe = ToolCall {
        id: CompactString::new("c4"),
        name: CompactString::new("write_file"),
        arguments: serde_json::json!({ "path": "/tmp/safe/file.txt" }),
    };
    let v_write = pipeline.evaluate(&call_write_unsafe, &caller());
    assert!(
        matches!(v_write, GovernanceVerdict::Deny { stage: "sandbox_policy", .. }),
        "Expected sandbox_policy deny for write to /tmp/safe/file.txt: got {:?}", v_write
    );
}

#[test]
fn sandbox_policy_blocks_network() {
    use deepstrike_core::governance::sandbox::SandboxProfile;

    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.set_sandbox_profile(SandboxProfile {
        allow_network: false,
        allow_fs_read: Vec::new(),
        allow_fs_write: Vec::new(),
    });
    pipeline.set_time(1000);

    let call_net = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("http_net_get"),
        arguments: serde_json::json!({ "url": "https://google.com" }),
    };
    let v = pipeline.evaluate(&call_net, &caller());
    assert!(
        matches!(v, GovernanceVerdict::Deny { stage: "sandbox_policy", .. }),
        "Expected sandbox_policy deny for network call: got {:?}", v
    );
}

#[test]
fn capability_check_stage() {
    use deepstrike_core::types::capability::{CapabilityDescriptor, CapabilityKind};
    use deepstrike_core::types::agent::{AgentRunSpec, AgentIdentity, AgentRole, AgentCapabilityFilter};

    let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
    pipeline.set_time(1000);

    // Mounted capabilities
    pipeline.set_capabilities(vec![
        CapabilityDescriptor::marker(CapabilityKind::Tool, "allowed_tool", "an allowed tool")
    ]);

    // 1. Mounted tool allowed
    let call_ok = ToolCall {
        id: CompactString::new("c1"),
        name: CompactString::new("allowed_tool"),
        arguments: serde_json::Value::Null,
    };
    assert!(matches!(
        pipeline.evaluate(&call_ok, &caller()),
        GovernanceVerdict::Allow
    ));

    // 2. Unmounted tool denied
    let call_not_mounted = ToolCall {
        id: CompactString::new("c2"),
        name: CompactString::new("unmounted_tool"),
        arguments: serde_json::Value::Null,
    };
    let v_mount = pipeline.evaluate(&call_not_mounted, &caller());
    assert!(
        matches!(v_mount, GovernanceVerdict::Deny { stage: "capability_check", .. }),
        "Expected capability_check deny: got {:?}", v_mount
    );

    // 3. Blocked by agent run spec filter
    let spec = AgentRunSpec::new(
        AgentIdentity::new("sub", "session-1"),
        AgentRole::Implement,
        "test goal"
    ).with_capability_filter(AgentCapabilityFilter {
        allowed_kinds: vec![CapabilityKind::Tool],
        allowed_ids: vec![compact_str::CompactString::new("some_other_tool")],
    });
    pipeline.set_run_spec(spec);
    let v_spec = pipeline.evaluate(&call_ok, &caller());
    assert!(
        matches!(v_spec, GovernanceVerdict::Deny { stage: "capability_check", .. }),
        "Expected capability_check spec deny: got {:?}", v_spec
    );
}

#[test]
fn policy_snapshot_creation() {
    let mut pipeline = GovernancePipeline::new(PermissionAction::Deny);
    pipeline.permission.add_rule(PermissionRule {
        tool_pattern: "read_*".into(),
        action: PermissionAction::Allow,
    });
    pipeline.veto.block_tool("unsafe_tool");
    pipeline.rate_limiter.set_limit("read_file", RateLimit { max_calls: 10, window_ms: 1000 });
    pipeline.constraints.add(deepstrike_core::governance::constraint::ParamConstraint {
        tool_name: "write_file".to_string(),
        param_path: "path".to_string(),
        rule: deepstrike_core::governance::constraint::ConstraintRule::Required,
    });

    let snapshot = pipeline.take_policy_snapshot();
    assert_eq!(snapshot.default_permission, "deny");
    assert_eq!(snapshot.rule_count, 1);
    assert_eq!(snapshot.veto_count, 1);
    assert_eq!(snapshot.rate_limit_count, 1);
    assert_eq!(snapshot.constraint_count, 1);
    assert!(!snapshot.has_sandbox_profile);

    pipeline.set_sandbox_profile(deepstrike_core::governance::sandbox::SandboxProfile {
        allow_network: false,
        allow_fs_read: vec![],
        allow_fs_write: vec![],
    });
    let snapshot2 = pipeline.take_policy_snapshot();
    assert!(snapshot2.has_sandbox_profile);
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
fn ask_user_is_not_denied_by_reduce_alone() {
    use deepstrike_core::governance::tool_decision::{ToolDecision, ToolDecisionPipeline, ToolDecisionStage};

    let decisions = vec![
        ToolDecision::allow(ToolDecisionStage::Classifier),
        ToolDecision::ask_user(ToolDecisionStage::PermissionCheck, "needs approval"),
    ];
    let v = ToolDecisionPipeline::reduce(&decisions);
    assert!(
        matches!(v, GovernanceVerdict::AskUser { .. }),
        "AskUser must survive reduction when no Deny present",
    );
}

#[test]
fn deny_overrides_ask_user_in_reduce() {
    use deepstrike_core::governance::tool_decision::{ToolDecision, ToolDecisionPipeline, ToolDecisionStage};

    let decisions = vec![
        ToolDecision::deny(ToolDecisionStage::VetoCheck, "hard veto"),
        ToolDecision::ask_user(ToolDecisionStage::PermissionCheck, "wants approval"),
    ];
    let v = ToolDecisionPipeline::reduce(&decisions);
    assert!(
        matches!(v, GovernanceVerdict::Deny { stage: "veto", .. }),
        "Deny must override AskUser in reduction",
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
