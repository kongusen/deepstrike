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

/// One knowledge entry supplied by the SDK after a long-term fetch (page-in).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageInEntry {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// K1: entry identity — a keyed page-in upserts instead of appending a duplicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// K1: pinned entries are exempt from the K2 budget sweep.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
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
