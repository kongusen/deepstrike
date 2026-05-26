use serde::{Deserialize, Serialize};

use super::audit::AuditLog;
use super::constraint::ConstraintValidator;
use super::permission::{PermissionAction, PermissionManager};
use super::rate_limit::RateLimiter;
use super::sandbox::{SandboxPolicy, SandboxProfile};
use super::tool_decision::{ToolDecision, ToolDecisionPipeline, ToolDecisionStage};
use super::veto::VetoAuthority;
use crate::types::capability::CapabilityDescriptor;
use crate::types::message::ToolCall;
use crate::types::policy::{CallerContext, GovernanceVerdict};
use crate::AgentRunSpec;

/// Security policy snapshot for auditing/inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityPolicySnapshot {
    pub default_permission: String,
    pub rule_count: usize,
    pub veto_count: usize,
    pub rate_limit_count: usize,
    pub constraint_count: usize,
    pub has_sandbox_profile: bool,
}

/// Full governance pipeline: CapabilityCheck -> ConstraintCheck -> PermissionCheck -> VetoCheck -> RateLimit -> SandboxPolicy
/// Any stage can deny; all must pass for Allow.
pub struct GovernancePipeline {
    pub permission: PermissionManager,
    pub veto: VetoAuthority,
    pub rate_limiter: RateLimiter,
    pub constraints: ConstraintValidator,
    pub audit: AuditLog,
    pub sandbox: SandboxPolicy,
    pub capabilities: Option<Vec<CapabilityDescriptor>>,
    pub run_spec: Option<AgentRunSpec>,
}

impl GovernancePipeline {
    pub fn new(default_action: PermissionAction) -> Self {
        Self {
            permission: PermissionManager::new(default_action),
            veto: VetoAuthority::new(),
            rate_limiter: RateLimiter::default(),
            constraints: ConstraintValidator::new(),
            audit: AuditLog::new(),
            sandbox: SandboxPolicy::new(),
            capabilities: None,
            run_spec: None,
        }
    }

    /// Set sandbox profile
    pub fn set_sandbox_profile(&mut self, profile: SandboxProfile) {
        self.sandbox.profile = Some(profile);
    }

    /// Set capabilities list
    pub fn set_capabilities(&mut self, capabilities: Vec<CapabilityDescriptor>) {
        self.capabilities = Some(capabilities);
    }

    /// Set agent run spec
    pub fn set_run_spec(&mut self, run_spec: AgentRunSpec) {
        self.run_spec = Some(run_spec);
    }

    /// Expose current policy snapshot
    pub fn take_policy_snapshot(&self) -> SecurityPolicySnapshot {
        let default_perm = match self.permission.default_action() {
            PermissionAction::Allow => "allow",
            PermissionAction::Deny => "deny",
            PermissionAction::AskUser => "ask_user",
        };
        SecurityPolicySnapshot {
            default_permission: default_perm.to_string(),
            rule_count: self.permission.rule_count(),
            veto_count: self.veto.blocked_count() + self.veto.custom_count(),
            rate_limit_count: self.rate_limiter.limit_count(),
            constraint_count: self.constraints.constraint_count(),
            has_sandbox_profile: self.sandbox.profile.is_some(),
        }
    }

    /// Set the current timestamp for rate limiting and audit.
    pub fn set_time(&mut self, now_ms: u64) {
        self.rate_limiter.set_time(now_ms);
        self.audit.set_time(now_ms);
    }

    /// Evaluate a tool call through the full pipeline.
    pub fn evaluate(&mut self, call: &ToolCall, caller: &CallerContext) -> GovernanceVerdict {
        let mut decisions = Vec::new();

        // 1. Classifier
        decisions.push(ToolDecision::allow(ToolDecisionStage::Classifier));

        // 2. CapabilityCheck
        let capability_verdict = self.check_capability(call);
        decisions.push(match capability_verdict {
            Some(v) => ToolDecision {
                stage: ToolDecisionStage::CapabilityCheck,
                verdict: v,
            },
            None => ToolDecision::allow(ToolDecisionStage::CapabilityCheck),
        });

        // 3. ConstraintCheck
        let constraint_verdict = self.constraints.validate(call);
        decisions.push(match constraint_verdict {
            Some(v) => ToolDecision {
                stage: ToolDecisionStage::ConstraintCheck,
                verdict: v,
            },
            None => ToolDecision::allow(ToolDecisionStage::ConstraintCheck),
        });

        // 4. PermissionCheck
        let permission_verdict = self.permission.check(call, caller);
        decisions.push(match permission_verdict {
            Some(v) => ToolDecision {
                stage: ToolDecisionStage::PermissionCheck,
                verdict: v,
            },
            None => ToolDecision::allow(ToolDecisionStage::PermissionCheck),
        });

        // 5. VetoCheck
        let veto_verdict = self.veto.check(call, caller);
        decisions.push(match veto_verdict {
            Some(v) => ToolDecision {
                stage: ToolDecisionStage::VetoCheck,
                verdict: v,
            },
            None => ToolDecision::allow(ToolDecisionStage::VetoCheck),
        });

        // 6. RateLimit
        let rate_verdict = self.rate_limiter.check(call);
        decisions.push(match rate_verdict {
            Some(v) => ToolDecision {
                stage: ToolDecisionStage::RateLimit,
                verdict: v,
            },
            None => ToolDecision::allow(ToolDecisionStage::RateLimit),
        });

        // 7. SandboxPolicy
        let sandbox_verdict = self.sandbox.check(call);
        decisions.push(match sandbox_verdict {
            Some(v) => ToolDecision {
                stage: ToolDecisionStage::SandboxPolicy,
                verdict: v,
            },
            None => ToolDecision::allow(ToolDecisionStage::SandboxPolicy),
        });

        // 8. Audit (we reduce first and then audit the final verdict)
        let final_verdict = ToolDecisionPipeline::reduce(&decisions);

        match final_verdict {
            GovernanceVerdict::Allow => {
                self.audit.record_allow(call);
            }
            ref other => {
                self.audit.record_deny(call, other);
            }
        }

        final_verdict
    }

    fn check_capability(&self, call: &ToolCall) -> Option<GovernanceVerdict> {
        // If capabilities manifest is mounted, check it
        if let Some(ref caps) = self.capabilities {
            let found = caps.iter().any(|c| {
                c.kind == crate::types::capability::CapabilityKind::Tool && c.id == call.name.as_str()
            });
            if !found {
                return Some(GovernanceVerdict::Deny {
                    stage: "capability_check",
                    reason: format!(
                        "tool '{}' is not mounted in the current capabilities manifest",
                        call.name
                    ),
                });
            }
        }

        // If run_spec is set, check run_spec filter
        if let Some(ref spec) = self.run_spec {
            let desc = crate::types::capability::CapabilityDescriptor::marker(
                crate::types::capability::CapabilityKind::Tool,
                call.name.clone(),
                "",
            );
            if !spec.capability_filter.allows(&desc) {
                return Some(GovernanceVerdict::Deny {
                    stage: "capability_check",
                    reason: format!(
                        "tool '{}' is blocked by agent run specification capability filter",
                        call.name
                    ),
                });
            }
        }

        None
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
        assert_eq!(pipeline.audit.len(), 1);
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
        assert!(matches!(deny, GovernanceVerdict::Deny { stage: "veto", .. }));
    }
}
