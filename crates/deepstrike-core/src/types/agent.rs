use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// Unified agent identity — shared across scheduler, memory, and governance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent_id: CompactString,
    pub session_id: CompactString,
    pub is_sub_agent: bool,
}

impl AgentIdentity {
    pub fn new(agent_id: impl Into<CompactString>, session_id: impl Into<CompactString>) -> Self {
        Self { agent_id: agent_id.into(), session_id: session_id.into(), is_sub_agent: false }
    }

    pub fn sub_agent(agent_id: impl Into<CompactString>, session_id: impl Into<CompactString>) -> Self {
        Self { agent_id: agent_id.into(), session_id: session_id.into(), is_sub_agent: true }
    }
}
