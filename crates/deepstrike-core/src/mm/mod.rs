//! Memory-management paging decisions (Phase 4) and long-term memory management (Phase 7).
//!
//! The kernel decides **when** to page working context out/in; SDKs perform **how**
//! (durable store, embedding search, idle pipeline). No I/O in this module.
//!
//! Phase 7 extends this module with memory classification and validation rules.

use crate::context::manager::{KNOWLEDGE_TOOL_NAME, MEMORY_TOOL_NAME};
use crate::context::pressure::PressureAction;
use crate::types::message::ToolCall;
use serde::{Deserialize, Serialize};

pub mod handle;
pub mod memory;

pub use handle::{
    plan_eviction, plan_spool, EvictionOp, EvictionPlan, Handle, HandleId, HandleKind, HandleTable,
    Residency, SpoolDecision,
};

/// Long-term tier hint for a page-out event (SDK maps to durable vs semantic store).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTierHint {
    /// History compression archive — default durable/session store.
    Durable,
    /// Sprint renewal / handoff — semantic long-term pipeline.
    Semantic,
}

impl MemoryTierHint {
    pub fn label(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::Semantic => "semantic",
        }
    }
}

/// Map a pressure-driven compression action to the recommended long-term tier.
pub fn tier_hint_for_compress(action: PressureAction) -> MemoryTierHint {
    match action {
        PressureAction::ContextCollapse | PressureAction::AutoCompact => MemoryTierHint::Semantic,
        _ => MemoryTierHint::Durable,
    }
}

/// Whether a tool name triggers an explicit page-in request (memory / knowledge meta-tools).
pub fn is_page_in_tool(name: &str) -> bool {
    name == MEMORY_TOOL_NAME || name == KNOWLEDGE_TOOL_NAME
}

/// Parsed arguments for a page-in meta-tool call.
#[derive(Debug, Clone, Default)]
pub struct PageInRequest {
    pub call_id: String,
    pub tool: String,
    pub query: String,
    pub top_k: u32,
}

/// Extract page-in requests from proposed tool calls (pure parse, no I/O).
pub fn page_in_requests_from_calls(calls: &[ToolCall]) -> Vec<PageInRequest> {
    let mut out = Vec::new();
    for call in calls {
        if !is_page_in_tool(call.name.as_str()) {
            continue;
        }
        let args = &call.arguments;
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let top_k = args
            .get("top_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as u32;
        out.push(PageInRequest {
            call_id: call.id.to_string(),
            tool: call.name.to_string(),
            query,
            top_k: top_k.max(1),
        });
    }
    out
}

/// One knowledge entry supplied by the SDK after a long-term fetch (page-in).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageInEntry {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;

    #[test]
    fn tier_hint_maps_auto_compact_to_semantic() {
        assert_eq!(
            tier_hint_for_compress(PressureAction::AutoCompact),
            MemoryTierHint::Semantic
        );
        assert_eq!(
            tier_hint_for_compress(PressureAction::SnipCompact),
            MemoryTierHint::Durable
        );
    }

    #[test]
    fn page_in_requests_from_memory_call() {
        use crate::types::message::ToolCall;
        let calls = vec![ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("memory"),
            arguments: serde_json::json!({"query": "past bugs", "top_k": 3}),
        }];
        let reqs = page_in_requests_from_calls(&calls);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].query, "past bugs");
        assert_eq!(reqs[0].top_k, 3);
    }
}
