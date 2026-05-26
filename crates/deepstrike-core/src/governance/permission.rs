use compact_str::CompactString;

use crate::types::message::ToolCall;
use crate::types::policy::{CallerContext, GovernanceVerdict};

/// Permission action for a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAction {
    Allow,
    Deny,
    AskUser,
}

/// A permission rule matching tool names by glob pattern.
#[derive(Debug, Clone)]
pub struct PermissionRule {
    pub tool_pattern: CompactString,
    pub action: PermissionAction,
}

impl PermissionRule {
    fn matches(&self, tool_name: &str) -> bool {
        let p = self.tool_pattern.as_str();
        if p == "*" {
            return true;
        }
        if let Some(prefix) = p.strip_suffix('*') {
            return tool_name.starts_with(prefix);
        }
        if let Some(suffix) = p.strip_prefix('*') {
            return tool_name.ends_with(suffix);
        }
        p == tool_name
    }
}

/// Permission manager — evaluates rules in order, first match wins.
pub struct PermissionManager {
    rules: Vec<PermissionRule>,
    default: PermissionAction,
}

impl PermissionManager {
    pub fn new(default: PermissionAction) -> Self {
        Self {
            rules: Vec::new(),
            default,
        }
    }

    pub fn add_rule(&mut self, rule: PermissionRule) {
        self.rules.push(rule);
    }

    pub fn check(&self, call: &ToolCall, _caller: &CallerContext) -> Option<GovernanceVerdict> {
        for rule in &self.rules {
            if rule.matches(&call.name) {
                return match rule.action {
                    PermissionAction::Allow => None,
                    PermissionAction::Deny => Some(GovernanceVerdict::Deny {
                        stage: "permission",
                        reason: format!(
                            "tool '{}' denied by rule '{}'",
                            call.name, rule.tool_pattern
                        ),
                    }),
                    PermissionAction::AskUser => Some(GovernanceVerdict::AskUser {
                        reason: format!("tool '{}' requires user approval", call.name),
                    }),
                };
            }
        }
        match self.default {
            PermissionAction::Allow => None,
            PermissionAction::AskUser => Some(GovernanceVerdict::AskUser {
                reason: format!("tool '{}' requires user approval", call.name),
            }),
            PermissionAction::Deny => Some(GovernanceVerdict::Deny {
                stage: "permission",
                reason: format!("tool '{}' denied by default policy", call.name),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;

    fn test_call(name: &str) -> ToolCall {
        ToolCall {
            id: CompactString::new("call-1"),
            name: CompactString::new(name),
            arguments: serde_json::Value::Null,
        }
    }

    fn test_caller() -> CallerContext {
        CallerContext {
            agent_id: "test".into(),
            session_id: "s1".into(),
            is_sub_agent: false,
        }
    }

    #[test]
    fn allow_by_default() {
        let pm = PermissionManager::new(PermissionAction::Allow);
        assert!(pm.check(&test_call("anything"), &test_caller()).is_none());
    }

    #[test]
    fn deny_by_pattern() {
        let mut pm = PermissionManager::new(PermissionAction::Allow);
        pm.add_rule(PermissionRule {
            tool_pattern: "db.*".into(),
            action: PermissionAction::Deny,
        });
        assert!(pm.check(&test_call("db.drop"), &test_caller()).is_some());
        assert!(pm.check(&test_call("file.read"), &test_caller()).is_none());
    }
}
