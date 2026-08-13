//! Deterministic merge point for every kind of local runnable work.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalRunnableKind {
    WorkflowNode,
    NestedTask,
    TimerWaiter,
    MessageWaiter,
    EventWaiter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalRunnable {
    pub id: String,
    pub kind: LocalRunnableKind,
    /// Rank assigned by the source scheduler. Sources share one merge/tie-break path.
    pub source_rank: u64,
}

impl LocalRunnable {
    pub fn workflow(id: impl Into<String>, source_rank: u64) -> Self {
        Self {
            id: id.into(),
            kind: LocalRunnableKind::WorkflowNode,
            source_rank,
        }
    }
}

/// Stable total order: source preference first, canonical id second, kind last.
pub fn order_runnables(mut candidates: Vec<LocalRunnable>) -> Vec<LocalRunnable> {
    candidates.sort_by(|left, right| {
        left.source_rank
            .cmp(&right.source_rank)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    candidates
}
