use super::audit::AuditLog;
use super::constraint::ConstraintValidator;
use super::permission::{PermissionAction, PermissionManager};
use super::rate_limit::RateLimiter;
use super::veto::VetoAuthority;
use crate::types::message::ToolCall;
use crate::types::policy::{CallerContext, GovernanceVerdict};

/// Full governance pipeline: Permission → Veto → RateLimit → Constraint
/// Any stage can deny; all must pass for Allow.
pub struct GovernancePipeline {
    pub permission: PermissionManager,
    pub veto: VetoAuthority,
    pub rate_limiter: RateLimiter,
    pub constraints: ConstraintValidator,
    pub audit: AuditLog,
}

impl GovernancePipeline {
    pub fn new(default_action: PermissionAction) -> Self {
        Self {
            permission: PermissionManager::new(default_action),
            veto: VetoAuthority::new(),
            rate_limiter: RateLimiter::default(),
            constraints: ConstraintValidator::new(),
            audit: AuditLog::new(),
        }
    }

    /// Set the current timestamp for rate limiting and audit.
    pub fn set_time(&mut self, now_ms: u64) {
        self.rate_limiter.set_time(now_ms);
        self.audit.set_time(now_ms);
    }

    /// Evaluate a tool call through the full pipeline.
    pub fn evaluate(&mut self, call: &ToolCall, caller: &CallerContext) -> GovernanceVerdict {
        // 1. Permission
        if let Some(verdict) = self.permission.check(call, caller) {
            self.audit.record_deny(call, &verdict);
            return verdict;
        }

        // 2. Veto
        if let Some(verdict) = self.veto.check(call, caller) {
            self.audit.record_deny(call, &verdict);
            return verdict;
        }

        // 3. Rate limit
        if let Some(verdict) = self.rate_limiter.check(call) {
            self.audit.record_deny(call, &verdict);
            return verdict;
        }

        // 4. Constraint validation
        if let Some(verdict) = self.constraints.validate(call) {
            self.audit.record_deny(call, &verdict);
            return verdict;
        }

        self.audit.record_allow(call);
        GovernanceVerdict::Allow
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
        assert!(matches!(v, GovernanceVerdict::Deny { stage: "permission", .. }));
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
        assert!(matches!(v, GovernanceVerdict::Deny { stage: "permission", .. }));
    }
}
