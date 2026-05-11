use compact_str::CompactString;

use crate::types::message::ToolCall;
use crate::types::policy::GovernanceVerdict;

/// A single audit log entry.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub tool_name: CompactString,
    pub call_id: CompactString,
    pub verdict: &'static str,
    pub stage: Option<&'static str>,
    pub reason: Option<String>,
    pub timestamp_ms: u64,
}

/// In-memory audit log for governance decisions.
pub struct AuditLog {
    entries: Vec<AuditEntry>,
    current_time_ms: u64,
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            current_time_ms: 0,
        }
    }

    pub fn set_time(&mut self, now_ms: u64) {
        self.current_time_ms = now_ms;
    }

    pub fn record_allow(&mut self, call: &ToolCall) {
        self.entries.push(AuditEntry {
            tool_name: call.name.clone(),
            call_id: call.id.clone(),
            verdict: "allow",
            stage: None,
            reason: None,
            timestamp_ms: self.current_time_ms,
        });
    }

    pub fn record_deny(&mut self, call: &ToolCall, verdict: &GovernanceVerdict) {
        let (stage, reason) = match verdict {
            GovernanceVerdict::Deny { stage, reason } => (Some(*stage), Some(reason.clone())),
            GovernanceVerdict::RateLimited { retry_after_ms } => {
                (Some("rate_limit"), Some(format!("retry after {}ms", retry_after_ms)))
            }
            GovernanceVerdict::AskUser { reason } => (Some("permission"), Some(reason.clone())),
            GovernanceVerdict::Allow => (None, None),
        };
        self.entries.push(AuditEntry {
            tool_name: call.name.clone(),
            call_id: call.id.clone(),
            verdict: "deny",
            stage,
            reason,
            timestamp_ms: self.current_time_ms,
        });
    }

    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}
