use compact_str::CompactString;
use deepstrike_core::governance::constraint::{ConstraintRule, ParamConstraint};
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::pipeline::GovernancePipeline;
use deepstrike_core::governance::rate_limit::RateLimit;
use deepstrike_core::types::agent::AgentIdentity;
use deepstrike_core::types::message::ToolCall;
use deepstrike_core::types::policy::GovernanceVerdict as CoreVerdict;

/// SDK-facing governance verdict (aligned with Node/Python `kind` field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceVerdict {
    pub kind: String,
    pub reason: Option<String>,
    pub retry_after_ms: Option<u64>,
}

/// Facade over the kernel `GovernancePipeline`.
pub struct Governance {
    inner: GovernancePipeline,
    agent_id: String,
    session_id: String,
}

impl Governance {
    pub fn new(default_action: PermissionAction) -> Self {
        Self {
            inner: GovernancePipeline::new(default_action),
            agent_id: "anonymous".into(),
            session_id: String::new(),
        }
    }

    pub fn allow() -> Self {
        Self::new(PermissionAction::Allow)
    }

    pub fn set_identity(&mut self, agent_id: impl Into<String>, session_id: impl Into<String>) {
        self.agent_id = agent_id.into();
        self.session_id = session_id.into();
    }

    pub fn add_permission_rule(&mut self, pattern: impl Into<String>, action: PermissionAction) {
        self.inner.permission.add_rule(PermissionRule {
            tool_pattern: CompactString::new(pattern.into()),
            action,
        });
    }

    pub fn block_tool(&mut self, name: impl Into<String>) {
        self.inner.veto.block_tool(name.into());
    }

    pub fn set_rate_limit(&mut self, tool_name: impl Into<String>, max_calls: u32, window_ms: u64) {
        self.inner.rate_limiter.set_limit(
            tool_name.into(),
            RateLimit {
                max_calls,
                window_ms,
            },
        );
    }

    pub fn require_param(&mut self, tool_name: impl Into<String>, param_path: impl Into<String>) {
        self.inner.constraints.add(ParamConstraint {
            tool_name: tool_name.into(),
            param_path: param_path.into(),
            rule: ConstraintRule::Required,
        });
    }

    pub fn allow_param_values(
        &mut self,
        tool_name: impl Into<String>,
        param_path: impl Into<String>,
        allowed_values: Vec<String>,
    ) {
        self.inner.constraints.add(ParamConstraint {
            tool_name: tool_name.into(),
            param_path: param_path.into(),
            rule: ConstraintRule::Enum(allowed_values),
        });
    }

    pub fn limit_param_range(
        &mut self,
        tool_name: impl Into<String>,
        param_path: impl Into<String>,
        min: Option<f64>,
        max: Option<f64>,
    ) {
        self.inner.constraints.add(ParamConstraint {
            tool_name: tool_name.into(),
            param_path: param_path.into(),
            rule: ConstraintRule::Range { min, max },
        });
    }

    pub fn set_time(&mut self, now_ms: u64) {
        self.inner.set_time(now_ms);
    }

    pub fn evaluate(&mut self, tool_name: &str, args_json: &str) -> GovernanceVerdict {
        let args: serde_json::Value =
            serde_json::from_str(args_json).unwrap_or(serde_json::Value::Null);
        let call = ToolCall {
            id: CompactString::new("gov"),
            name: CompactString::new(tool_name),
            arguments: args,
        };
        let caller = AgentIdentity::new(&self.agent_id, &self.session_id);
        from_core(self.inner.evaluate(&call, &caller))
    }
}

fn from_core(v: CoreVerdict) -> GovernanceVerdict {
    match v {
        CoreVerdict::Allow => GovernanceVerdict {
            kind: "allow".into(),
            reason: None,
            retry_after_ms: None,
        },
        CoreVerdict::Deny { reason, .. } => GovernanceVerdict {
            kind: "deny".into(),
            reason: Some(reason),
            retry_after_ms: None,
        },
        CoreVerdict::RateLimited { retry_after_ms } => GovernanceVerdict {
            kind: "rate_limited".into(),
            reason: None,
            retry_after_ms: Some(retry_after_ms),
        },
        CoreVerdict::AskUser { reason } => GovernanceVerdict {
            kind: "ask_user".into(),
            reason: Some(reason),
            retry_after_ms: None,
        },
    }
}
