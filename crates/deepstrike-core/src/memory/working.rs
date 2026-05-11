use std::collections::HashMap;

use compact_str::CompactString;

use crate::context::dashboard::Dashboard;
use crate::types::signal::RuntimeSignal;

/// Working memory: ephemeral state for the current run.
#[derive(Debug, Default)]
pub struct WorkingMemory {
    pub pending_signals: Vec<RuntimeSignal>,
    pub tool_cache: HashMap<CompactString, CompactString>,
    pub scratch: HashMap<String, serde_json::Value>,
    /// Structured dashboard state shared with context layer.
    pub dashboard: Dashboard,
}

impl WorkingMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cache_tool_result(&mut self, call_id: CompactString, result: CompactString) {
        self.tool_cache.insert(call_id, result);
    }

    pub fn get_cached(&self, call_id: &str) -> Option<&CompactString> {
        self.tool_cache.get(call_id)
    }

    pub fn add_signal(&mut self, signal: RuntimeSignal) {
        self.pending_signals.push(signal);
    }

    pub fn drain_signals(&mut self) -> Vec<RuntimeSignal> {
        std::mem::take(&mut self.pending_signals)
    }

    pub fn clear(&mut self) {
        self.pending_signals.clear();
        self.tool_cache.clear();
        self.scratch.clear();
    }
}
