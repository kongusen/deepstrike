//! Plan task 7.1 · the item graph: typed, directed edges between the items of one operation.
//!
//! The item table says what each item is; the graph says how items relate: which call a tool result
//! answers, which history item referenced which knowledge item and by what evidence, what a read
//! brought back, what a provider summary covers. Edges are recorded from committed input only (D1),
//! so a replay rebuilds the same graph, and they travel in the checkpoint.
//!
//! The graph is bounded. An edge whose two ends are both still resident is always kept; of the
//! edges that touch an item which has left, only the latest [`RETIRED_EDGE_CAP`] are kept and the
//! rest are counted in [`ItemGraph::omitted`]. In 0.2.81 the graph only reports: no decision reads it.

use std::collections::BTreeSet;

use crate::context::item::{HISTORY_ITEM_DETAIL_CAP, ItemEdge, ItemId};

/// Edges touching a retired item that the graph keeps, newest last.
pub const RETIRED_EDGE_CAP: usize = HISTORY_ITEM_DETAIL_CAP;

/// Edges the graph holds before it prunes against residency.
pub const EDGE_PRUNE_AT: usize = 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemGraph {
    edges: Vec<ItemEdge>,
    omitted: u32,
}

impl ItemGraph {
    /// Record `edge` unless it is degenerate (an untracked end, a self-loop) or already recorded.
    /// Returns whether it was added.
    pub fn add(&mut self, edge: ItemEdge) -> bool {
        if edge.from == 0 || edge.to == 0 || edge.from == edge.to {
            return false;
        }
        if self
            .edges
            .iter()
            .any(|e| e.from == edge.from && e.to == edge.to && e.relation == edge.relation)
        {
            return false;
        }
        self.edges.push(edge);
        true
    }

    /// Whether the graph has grown past [`EDGE_PRUNE_AT`] and should be pruned.
    pub fn over_limit(&self) -> bool {
        self.edges.len() > EDGE_PRUNE_AT
    }

    /// Keep every edge whose ends are both in `resident`, and the latest [`RETIRED_EDGE_CAP`] of
    /// the others; count what is dropped. If resident edges alone still exceed
    /// [`EDGE_PRUNE_AT`], the oldest go too.
    pub fn prune(&mut self, resident: &BTreeSet<ItemId>) {
        let live = |edge: &ItemEdge| resident.contains(&edge.from) && resident.contains(&edge.to);
        let retired = self.edges.iter().filter(|edge| !live(edge)).count();
        let mut drop_retired = retired.saturating_sub(RETIRED_EDGE_CAP);
        let before = self.edges.len();
        self.edges.retain(|edge| {
            if live(edge) || drop_retired == 0 {
                return true;
            }
            drop_retired -= 1;
            false
        });
        let overflow = self.edges.len().saturating_sub(EDGE_PRUNE_AT);
        self.edges.drain(..overflow);
        self.omitted = self.omitted.saturating_add((before - self.edges.len()) as u32);
    }

    pub fn edges(&self) -> &[ItemEdge] {
        &self.edges
    }

    /// Edges dropped by pruning over the run.
    pub fn omitted(&self) -> u32 {
        self.omitted
    }

    /// The recorded edges into `to`.
    pub fn in_edges(&self, to: ItemId) -> impl Iterator<Item = &ItemEdge> {
        self.edges.iter().filter(move |edge| edge.to == to)
    }

    /// Rebuild from a checkpoint.
    pub fn restore(&mut self, edges: Vec<ItemEdge>, omitted: u32) {
        self.edges = edges;
        self.omitted = omitted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::item::Relation;

    fn edge(from: ItemId, to: ItemId) -> ItemEdge {
        ItemEdge { from, to, relation: Relation::ReferencesLexical, turn: 0 }
    }

    #[test]
    fn degenerate_and_repeated_edges_are_not_recorded() {
        let mut graph = ItemGraph::default();
        assert!(!graph.add(edge(0, 3)));
        assert!(!graph.add(edge(3, 3)));
        assert!(graph.add(edge(4, 3)));
        assert!(!graph.add(edge(4, 3)));
        assert!(graph.add(ItemEdge { relation: Relation::ReferencesStrong, ..edge(4, 3) }));
        assert_eq!(graph.in_edges(3).count(), 2);
    }

    #[test]
    fn pruning_keeps_resident_edges_and_the_latest_retired_ones() {
        let mut graph = ItemGraph::default();
        let total = RETIRED_EDGE_CAP as u64 + 5;
        for from in 1..=total {
            graph.add(edge(from + 1000, 1));
        }
        graph.add(edge(2, 3));
        let resident = BTreeSet::from([2, 3]);
        graph.prune(&resident);
        assert_eq!(graph.omitted(), 5);
        assert_eq!(graph.edges().len(), RETIRED_EDGE_CAP + 1);
        assert_eq!(graph.edges()[0].from, 1006, "the oldest retired edges go first");
        assert!(graph.edges().contains(&edge(2, 3)));
    }
}
