use std::collections::HashSet;

use compact_str::CompactString;

use crate::types::message::ToolCall;
use crate::types::policy::{CallerContext, GovernanceVerdict, VetoCheck};

/// Veto authority — hard security boundary that cannot be overridden.
pub struct VetoAuthority {
    /// Tool names that are always blocked (e.g., "rm", "exec_shell")
    blocked_tools: HashSet<CompactString>,
    /// Custom veto checks from SDK layer (trait objects → FFI-friendly).
    custom_checks: Vec<Box<dyn VetoCheck>>,
}

impl VetoAuthority {
    pub fn new() -> Self {
        Self {
            blocked_tools: HashSet::new(),
            custom_checks: Vec::new(),
        }
    }

    pub fn block_tool(&mut self, name: impl Into<CompactString>) {
        self.blocked_tools.insert(name.into());
    }

    pub fn blocked_count(&self) -> usize {
        self.blocked_tools.len()
    }

    pub fn custom_count(&self) -> usize {
        self.custom_checks.len()
    }

    /// Register a check via the `VetoCheck` trait.
    /// Plain closures work too thanks to the blanket impl in `types::policy`.
    pub fn add_check<C>(&mut self, check: C)
    where
        C: VetoCheck + 'static,
    {
        self.custom_checks.push(Box::new(check));
    }

    pub fn check(&self, call: &ToolCall, caller: &CallerContext) -> Option<GovernanceVerdict> {
        // Hard block list
        if self.blocked_tools.contains(call.name.as_str()) {
            return Some(GovernanceVerdict::Deny {
                stage: "veto",
                reason: format!("tool '{}' is vetoed", call.name),
            });
        }
        // Custom checks
        for check in &self.custom_checks {
            if let Some(reason) = check.check(call, caller) {
                return Some(GovernanceVerdict::Deny {
                    stage: "veto",
                    reason,
                });
            }
        }
        None
    }
}

impl Default for VetoAuthority {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;

    #[test]
    fn blocks_vetoed_tools() {
        let mut veto = VetoAuthority::new();
        veto.block_tool("rm_rf");

        let call = ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("rm_rf"),
            arguments: serde_json::Value::Null,
        };
        let caller = CallerContext {
            agent_id: "a".into(),
            session_id: "s".into(),
            is_sub_agent: false,
            parent_session_id: None,
        };

        assert!(veto.check(&call, &caller).is_some());
    }

    #[test]
    fn closure_check_via_blanket_impl() {
        let mut veto = VetoAuthority::new();
        veto.add_check(|call: &ToolCall, _caller: &CallerContext| {
            if call.name.as_str().starts_with("danger_") {
                Some(format!("blocked dangerous tool: {}", call.name))
            } else {
                None
            }
        });

        let call = ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("danger_eval"),
            arguments: serde_json::Value::Null,
        };
        let caller = CallerContext {
            agent_id: "a".into(),
            session_id: "s".into(),
            is_sub_agent: false,
            parent_session_id: None,
        };
        assert!(veto.check(&call, &caller).is_some());
    }

    #[test]
    fn trait_impl_check() {
        struct BlockNet;
        impl VetoCheck for BlockNet {
            fn check(&self, call: &ToolCall, _caller: &CallerContext) -> Option<String> {
                if call.name.as_str().contains("net") {
                    Some("network access vetoed".to_string())
                } else {
                    None
                }
            }
        }

        let mut veto = VetoAuthority::new();
        veto.add_check(BlockNet);

        let call = ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("http_net_get"),
            arguments: serde_json::Value::Null,
        };
        let caller = CallerContext {
            agent_id: "a".into(),
            session_id: "s".into(),
            is_sub_agent: false,
            parent_session_id: None,
        };
        assert!(veto.check(&call, &caller).is_some());
    }
}
