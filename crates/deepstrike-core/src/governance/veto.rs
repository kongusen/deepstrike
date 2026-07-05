use std::collections::HashSet;

use compact_str::CompactString;

use crate::types::message::ToolCall;
use crate::types::policy::GovernanceVerdict;

/// Veto authority — hard security boundary that cannot be overridden.
pub struct VetoAuthority {
    /// Tool names that are always blocked (e.g., "rm", "exec_shell")
    blocked_tools: HashSet<CompactString>,
}

impl VetoAuthority {
    pub fn new() -> Self {
        Self {
            blocked_tools: HashSet::new(),
        }
    }

    pub fn block_tool(&mut self, name: impl Into<CompactString>) {
        self.blocked_tools.insert(name.into());
    }

    pub fn check(&self, call: &ToolCall) -> Option<GovernanceVerdict> {
        if self.blocked_tools.contains(call.name.as_str()) {
            return Some(GovernanceVerdict::Deny {
                stage: "veto",
                reason: format!("tool '{}' is vetoed", call.name),
            });
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

        assert!(veto.check(&call).is_some());
    }

    #[test]
    fn passes_unblocked_tools() {
        let veto = VetoAuthority::new();
        let call = ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("read_file"),
            arguments: serde_json::Value::Null,
        };
        assert!(veto.check(&call).is_none());
    }
}
