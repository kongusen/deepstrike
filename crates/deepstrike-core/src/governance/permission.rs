use compact_str::CompactString;

use crate::types::message::ToolCall;
use crate::types::policy::GovernanceVerdict;

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

    pub fn default_action(&self) -> &PermissionAction {
        &self.default
    }

    /// spc_004-04: the attenuation-invariant check for spawn-time capability delegation — a
    /// second, additive layer alongside [`Self::check`]'s tool-name glob, not a replacement for
    /// it. `None` ⇒ every requested child capability legally narrows some parent capability
    /// (`caps_subset` succeeded). `Some(Deny)` ⇒ at least one did not; the reason names the
    /// offending capability ids so the rejection is diagnosable, not just "no".
    ///
    /// Not yet called from the live spawn path: `IsolationManifest`/`Tcb` carry capability grants
    /// as `Vec<CompactString>` ids today, not the richer `Capability` (resource/actions/
    /// constraints) shape this needs. Wiring that through is a larger structural change than this
    /// card — this method is the reusable governance primitive a future card calls once that data
    /// exists, using the same `Disposition`/`GovernanceVerdict` semantics `gate.rs` already
    /// understands.
    pub fn check_delegation(
        &self,
        requested_child_caps: &[crate::types::capability::Capability],
        parent_caps: &[crate::types::capability::Capability],
    ) -> Option<GovernanceVerdict> {
        match crate::types::capability::caps_subset(requested_child_caps, parent_caps) {
            Ok(()) => None,
            Err(violations) => Some(GovernanceVerdict::Deny {
                stage: "capability_delegation",
                reason: format!(
                    "capability delegation would widen authority beyond the parent's: {}",
                    violations
                        .iter()
                        .map(|cap| cap.id.0.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        }
    }

    pub fn check(&self, call: &ToolCall) -> Option<GovernanceVerdict> {
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

    #[test]
    fn allow_by_default() {
        let pm = PermissionManager::new(PermissionAction::Allow);
        assert!(pm.check(&test_call("anything")).is_none());
    }

    #[test]
    fn deny_by_pattern() {
        let mut pm = PermissionManager::new(PermissionAction::Allow);
        pm.add_rule(PermissionRule {
            tool_pattern: "db.*".into(),
            action: PermissionAction::Deny,
        });
        assert!(pm.check(&test_call("db.drop")).is_some());
        assert!(pm.check(&test_call("file.read")).is_none());
    }

    fn cap(resource: &str, actions: &[&str]) -> crate::types::capability::Capability {
        use crate::types::capability::*;
        Capability {
            id: CapabilityId("cap".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector(resource.into()),
            actions: ActionSet(actions.iter().map(|a| (*a).into()).collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("issuer".into()),
        }
    }

    #[test]
    fn check_delegation_denies_a_child_requesting_wider_scope_than_the_parent_holds() {
        let pm = PermissionManager::new(PermissionAction::Allow);
        let parent_caps = vec![cap("/repo/src/**", &["read"])];
        let requested_child_caps = vec![cap("/repo/**", &["read"])];

        let verdict = pm.check_delegation(&requested_child_caps, &parent_caps);
        assert!(matches!(verdict, Some(GovernanceVerdict::Deny { .. })));
    }

    #[test]
    fn check_delegation_allows_a_legal_narrowing() {
        let pm = PermissionManager::new(PermissionAction::Allow);
        let parent_caps = vec![cap("/repo/src/**", &["read"])];
        let requested_child_caps = vec![cap("/repo/src/utils/**", &["read"])];

        assert!(pm.check_delegation(&requested_child_caps, &parent_caps).is_none());
    }

    #[test]
    fn check_delegation_does_not_affect_plain_tool_name_glob_checks() {
        // spc_004-04's own scope fence: `check_delegation` is an additive second layer,
        // `check` (tool-name glob) must behave exactly as before.
        let mut pm = PermissionManager::new(PermissionAction::Allow);
        pm.add_rule(PermissionRule {
            tool_pattern: "read_*".into(),
            action: PermissionAction::Allow,
        });
        pm.add_rule(PermissionRule {
            tool_pattern: "*".into(),
            action: PermissionAction::Deny,
        });
        assert!(pm.check(&test_call("read_file")).is_none());
        assert!(pm.check(&test_call("write_file")).is_some());
    }
}
