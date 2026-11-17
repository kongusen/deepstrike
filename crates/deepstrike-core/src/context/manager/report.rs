//! The end-of-run lifecycle report.

use super::*;

impl ContextManager {
    /// The run's context lifecycle metrics (plan task 4.1).
    pub fn lifecycle_metrics(&self) -> LifecycleMetrics {
        let knowledge = &self.partitions.knowledge;
        let referenced = knowledge.entries.iter().filter(|e| e.value.ref_count > 0).count() as u32;
        let resident_waste: u64 = knowledge
            .entries
            .iter()
            .filter(|e| e.value.ref_count == 0)
            .map(|e| u64::from(e.tokens))
            .sum();
        let items = knowledge.item_records();
        let recalls = items.iter().filter(|item| item.origin == ItemOrigin::MemoryRecall);
        let (recalled, recall_expanded) = recalls.fold((0, 0), |(all, expanded), item| {
            (all + 1, expanded + u32::from(item.value.expanded))
        });
        let mut history = self.history_tally.clone();
        let partition = &self.partitions.history;
        for index in 0..partition.len() {
            let tag = partition.tag(index);
            if let Some(tally) = history.get_mut(&tag.origin) {
                tally.resident_items += 1;
                tally.resident_tokens += u64::from(partition.measured_tokens(index, &self.engine));
            }
        }
        if !self.dependency_bodies.is_empty() {
            let tally = history.entry(ItemOrigin::DependencyOutput).or_insert(OriginTally {
                origin: ItemOrigin::DependencyOutput,
                items: 0,
                tokens: 0,
                resident_items: 0,
                resident_tokens: 0,
                read_back: 0,
            });
            tally.items = self.dependency_bodies.len() as u32;
            tally.tokens = self
                .dependency_bodies
                .values()
                .map(|body| u64::from(self.engine.count(body)))
                .sum();
        }
        let history_len = self.partitions.history.len();
        let resident = (0..history_len).map(|index| {
            HistoryItemRecord::of(
                self.partitions.history.tag(index),
                self.partitions.history.measured_tokens(index, &self.engine),
                self.history_fidelity(index),
                None,
            )
        });
        let mut history_items: Vec<HistoryItemRecord> =
            self.retired_history.iter().copied().chain(resident).collect();
        let overflow = history_items.len().saturating_sub(HISTORY_ITEM_DETAIL_CAP);
        history_items.drain(..overflow);
        let waste_items = items
            .iter()
            .filter(|item| {
                item.id != 0
                    && item.value.ref_count == 0
                    && self.graph.in_edges(item.id).next().is_none()
            })
            .map(|item| item.id)
            .collect();
        let used: std::collections::BTreeSet<ItemId> = items
            .iter()
            .filter(|item| item.value.ref_count > 0 || self.graph.in_edges(item.id).next().is_some())
            .map(|item| item.id)
            .collect();
        let parent_items_used = self
            .graph
            .external()
            .iter()
            .filter(|edge| edge.relation == Relation::InheritedFrom && used.contains(&edge.from))
            .map(|edge| edge.to.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        LifecycleMetrics {
            external_edges: self.graph.external().to_vec(),
            parent_items_used,
            edges: self.graph.edges().to_vec(),
            edges_omitted: self.graph.omitted(),
            waste_items,
            history_items,
            history_items_omitted: self.retired_history_omitted + overflow as u32,
            history: history.into_values().collect(),
            knowledge_entries: knowledge.entries.len() as u32,
            knowledge_referenced: referenced,
            regret: knowledge.regret,
            waste_tokens: knowledge.wasted_tokens + resident_waste,
            cache_loss_tokens: self.cache_ledger.lost_tokens,
            prefix_breaks: self.cache_ledger.breaks.iter().copied().collect(),
            prefix_breaks_omitted: self.cache_ledger.breaks_omitted,
            cache_tail_tokens: self.cache_ledger.tail_tokens,
            notes_written: self.partitions.task_state.notes_written,
            notes_active: self.partitions.task_state.notes.len() as u32,
            kernel_text_tokens: self.kernel_text_tokens,
            recalled,
            recall_expanded,
            items,
        }
    }
}
