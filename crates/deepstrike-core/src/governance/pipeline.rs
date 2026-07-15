use super::constraint::ConstraintValidator;
use super::permission::{PermissionAction, PermissionManager};
use super::rate_limit::RateLimiter;
use super::veto::VetoAuthority;
use crate::types::message::ToolCall;
use crate::types::policy::{CallerContext, GovernanceVerdict};

/// Governance pipeline: Constraint -> Permission -> Veto -> RateLimit.
/// Every stage runs; the most severe verdict wins (Deny > AskUser > RateLimited > Allow),
/// with the earliest stage winning ties — so a veto Deny can never be softened by a
/// later or earlier Allow, and an AskUser survives any number of passing stages.
pub struct GovernancePipeline {
    pub permission: PermissionManager,
    pub veto: VetoAuthority,
    pub rate_limiter: RateLimiter,
    pub constraints: ConstraintValidator,
}

fn severity(v: &GovernanceVerdict) -> u8 {
    match v {
        GovernanceVerdict::Deny { .. } => 3,
        GovernanceVerdict::AskUser { .. } => 2,
        GovernanceVerdict::RateLimited { .. } => 1,
        GovernanceVerdict::Allow => 0,
    }
}

impl GovernancePipeline {
    pub fn new(default_action: PermissionAction) -> Self {
        Self {
            permission: PermissionManager::new(default_action),
            veto: VetoAuthority::new(),
            rate_limiter: RateLimiter::default(),
            constraints: ConstraintValidator::new(),
        }
    }

    /// Set the current timestamp for rate limiting.
    pub fn set_time(&mut self, now_ms: u64) {
        self.rate_limiter.set_time(now_ms);
    }

    /// Evaluate a tool call through the full pipeline.
    pub fn evaluate(&mut self, call: &ToolCall, _caller: &CallerContext) -> GovernanceVerdict {
        let mut worst = GovernanceVerdict::Allow;
        let verdicts = [
            self.constraints.validate(call),
            self.permission.check(call),
            self.veto.check(call),
            self.rate_limiter.check(call),
        ];
        for verdict in verdicts.into_iter().flatten() {
            if severity(&verdict) > severity(&worst) {
                worst = verdict;
            }
        }
        worst
    }
}

impl Default for GovernancePipeline {
    fn default() -> Self {
        Self::new(PermissionAction::Allow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::permission::PermissionRule;
    use compact_str::CompactString;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new(name),
            arguments: serde_json::Value::Null,
        }
    }

    fn caller() -> CallerContext {
        CallerContext {
            agent_id: "a".into(),
            session_id: "s".into(),
            is_sub_agent: false,
            parent_session_id: None,
        }
    }

    #[test]
    fn full_pipeline_allow() {
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.set_time(1000);
        let v = pipeline.evaluate(&call("read_file"), &caller());
        assert!(matches!(v, GovernanceVerdict::Allow));
    }

    #[test]
    fn permission_deny_stops_pipeline() {
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.permission.add_rule(PermissionRule {
            tool_pattern: "danger.*".into(),
            action: PermissionAction::Deny,
        });

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
    fn veto_overrides_permission() {
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.veto.block_tool("nuke");

        let v = pipeline.evaluate(&call("nuke"), &caller());
        assert!(matches!(v, GovernanceVerdict::Deny { stage: "veto", .. }));
    }

    #[test]
    fn ask_user_verdict() {
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.set_time(1000);
        pipeline.permission.add_rule(PermissionRule {
            tool_pattern: "sensitive.*".into(),
            action: PermissionAction::AskUser,
        });

        let v = pipeline.evaluate(&call("sensitive.delete"), &caller());
        assert!(matches!(v, GovernanceVerdict::AskUser { .. }));
    }

    #[test]
    fn deny_default_blocks_all() {
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

    // ─── Verdict-severity fold invariants (ported from the ToolDecision reduce tests) ────────

    #[test]
    fn deny_beats_ask_user_regardless_of_stage_order() {
        // Permission says AskUser (earlier stage), veto says Deny (later stage):
        // the more severe Deny must win even though it arrives later.
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.permission.add_rule(PermissionRule {
            tool_pattern: "nuke".into(),
            action: PermissionAction::AskUser,
        });
        pipeline.veto.block_tool("nuke");

        let v = pipeline.evaluate(&call("nuke"), &caller());
        assert!(matches!(v, GovernanceVerdict::Deny { stage: "veto", .. }));
    }

    #[test]
    fn ask_user_survives_later_passing_stages() {
        // AskUser from permission must not be downgraded by veto/rate-limit passing (None).
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.set_time(1000);
        pipeline.permission.add_rule(PermissionRule {
            tool_pattern: "sensitive".into(),
            action: PermissionAction::AskUser,
        });

        let v = pipeline.evaluate(&call("sensitive"), &caller());
        assert!(matches!(v, GovernanceVerdict::AskUser { .. }));
    }

    // ─── Monotonic Veto invariant ──────────────────────────────────────────
    // Once VetoAuthority issues Deny, no other stage can flip it to Allow.

    #[test]
    fn veto_deny_overrides_explicit_permission_allow() {
        // Permission has an explicit Allow rule for the tool (returns None = pass-through).
        // Veto hard-blocks the same tool.
        // The veto must win — permission Allow cannot suppress a veto Deny.
        let mut pipeline = GovernancePipeline::new(PermissionAction::Deny);
        pipeline.permission.add_rule(PermissionRule {
            tool_pattern: "rm_rf".into(),
            action: PermissionAction::Allow,
        });
        pipeline.veto.block_tool("rm_rf");

        let v = pipeline.evaluate(&call("rm_rf"), &caller());
        assert!(
            matches!(v, GovernanceVerdict::Deny { stage: "veto", .. }),
            "Veto must override explicit permission Allow: got {v:?}",
        );
    }

    #[test]
    fn veto_deny_is_monotonic_across_repeated_evaluations() {
        // After a Veto Deny is issued, subsequent evaluations on the same pipeline
        // continue to return Deny — the veto is not a one-shot effect.
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.veto.block_tool("nuke");

        for _ in 0..3 {
            let v = pipeline.evaluate(&call("nuke"), &caller());
            assert!(
                matches!(v, GovernanceVerdict::Deny { stage: "veto", .. }),
                "Veto deny must persist monotonically: got {v:?}",
            );
        }
    }

    #[test]
    fn veto_deny_blocks_even_when_default_is_allow() {
        // Sanity: the overall pipeline default of Allow does not prevent Veto.
        // This ensures no Allow shortcut fires before the Veto stage.
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.veto.block_tool("exec_shell");

        // Allowed tool passes through
        let pass = pipeline.evaluate(&call("read_file"), &caller());
        assert!(matches!(pass, GovernanceVerdict::Allow));

        // Vetoed tool is denied regardless of the Allow default
        let deny = pipeline.evaluate(&call("exec_shell"), &caller());
        assert!(matches!(
            deny,
            GovernanceVerdict::Deny { stage: "veto", .. }
        ));
    }
}
