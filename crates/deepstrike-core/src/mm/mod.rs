//! Memory-management paging decisions (Phase 4) and long-term memory management (Phase 7).
//!
//! The kernel decides **when** to page working context out/in; SDKs perform **how**
//! (durable store, embedding search, idle pipeline). No I/O in this module.
//!
//! Phase 7 extends this module with memory classification and validation rules.

use crate::context::pressure::PressureAction;
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

/// Parsed arguments for a page-in meta-tool call. Retained as the payload type for
/// [`crate::syscall::Syscall::PageIn`]; the request-extraction helper that used to build these
/// from live tool calls was removed when the automatic memory/knowledge tool-call page-in path
/// was retired (that content now flows through the normal tool-result → history path instead —
/// see `apply_page_in`'s doc comment).
#[derive(Debug, Clone, Default)]
pub struct PageInRequest {
    pub call_id: String,
    pub tool: String,
    pub query: String,
    pub top_k: u32,
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

}
