//! The item store: history admission, item ids and the item graph, recall and item leases.

use super::*;

impl ContextManager {
    // ── History / Knowledge ───────────────────────────────────────────────────

    /// Plan task 5.1 · the provider summarized history through item `through` server-side. The
    /// covered items leave as compacted (extended to the end of a tool transaction, never split),
    /// and one `provider_summary` item heads history in their place, rendering `replay` (the
    /// provider's block) — so pressure, renewal and the next render see what the model sees.
    /// Returns whether `through` named a history item.
    pub fn absorb_provider_compaction(
        &mut self,
        through: ItemId,
        replay: serde_json::Value,
        summary_tokens: u32,
    ) -> bool {
        let history = &self.partitions.history;
        let Some(position) = history.tags().iter().position(|tag| tag.id == through) else {
            return false;
        };
        let end = crate::context::units::unit_boundaries(history.messages())
            .into_iter()
            .find(|unit| unit.contains(&position))
            .map_or(position + 1, |unit| unit.end);
        let covered_tokens: u32 = (0..end)
            .map(|index| history.measured_tokens(index, &self.engine))
            .sum();
        let retained = history.token_count().saturating_sub(covered_tokens);
        let covered: std::collections::BTreeSet<usize> = (0..end).collect();
        let rows = self.partitions.history.take_rows(&covered, retained);
        for row in &rows {
            let record = HistoryItemRecord::of(
                row.tag,
                row.measurement.tokens,
                row.tag.fidelity,
                Some(ItemEnd::Compacted),
            );
            self.retire_history_item(record);
        }
        let tag = ItemTag::new(
            self.alloc_item_id(),
            ItemOrigin::ProviderSummary,
            self.partitions.knowledge.turn,
        );
        for row in &rows {
            self.add_edge(tag.id, row.tag.id, Relation::Summarizes);
        }
        let message = CoreMessage::assistant("[earlier conversation summarized by the provider]");
        let tally = self.tally(ItemOrigin::ProviderSummary);
        tally.items += 1;
        tally.tokens += u64::from(summary_tokens);
        self.partitions.history.prepend_row(crate::context::partitions::PartitionRow {
            tag,
            measurement: crate::context::measurement::TokenMeasurement::for_message(&message, summary_tokens),
            message,
        });
        self.keep_item_replay(tag.id, replay);
        self.cache_ledger.note_cause(BreakCause::Provider);
        self.prune_orphaned_handles();
        self.prune_item_replays();
        self.reset_collapse_generation();
        self.sweep_knowledge_at_boundary();
        self.frozen_history_len = self.partitions.history.len();
        true
    }

    /// Plan task 5.1 · the provider cleared these tool results server-side: render them as
    /// handles, as the model now sees them.
    pub fn absorb_cleared_tool_results<'a>(&mut self, call_ids: impl IntoIterator<Item = &'a str>) {
        let cleared: std::collections::HashSet<&str> = call_ids.into_iter().collect();
        for handle in self.handles.tool_result_handles_mut() {
            if handle.source.as_deref().is_some_and(|source| cleared.contains(source))
                && matches!(handle.residency, Residency::Resident)
            {
                handle.residency = Residency::Collapsed;
                self.cache_ledger.note_cause(BreakCause::Provider);
            }
        }
    }

    /// Keep `replay` with item `id`, counting what it adds to a request.
    pub(crate) fn keep_item_replay(&mut self, id: ItemId, replay: serde_json::Value) {
        let tokens = self.engine.count(&replay.to_string());
        self.item_replays.insert(id, (replay, tokens));
    }

    /// Tokens the replay of every item still in history adds to a request.
    pub(super) fn replay_tokens(&self) -> u32 {
        self.item_replays.values().map(|(_, tokens)| *tokens).sum()
    }

    /// Drop replay state whose item left history.
    pub(super) fn prune_item_replays(&mut self) {
        if self.item_replays.is_empty() {
            return;
        }
        let live: std::collections::HashSet<ItemId> =
            self.partitions.history.tags().iter().map(|tag| tag.id).collect();
        self.item_replays.retain(|id, _| live.contains(id));
    }

    /// Append `msg` to history after the tool results the last assistant turn still waits for.
    /// Used for content an effect delivers mid-batch (a memory hit that resolves before a sibling
    /// tool): placed now, it would split the batch's results, which a provider rejects.
    pub fn push_history_after_tool_results(&mut self, msg: CoreMessage, tokens: u32) {
        if self.awaits_tool_results() {
            self.held_history.push((msg, tokens));
        } else {
            self.push_history(msg, tokens);
        }
    }

    /// Append whatever is still held, whether or not the batch completed. Called before a provider
    /// call, so held content is never lost to a batch whose results never all arrived.
    pub fn release_held_history(&mut self) {
        for (held, tokens) in std::mem::take(&mut self.held_history) {
            self.admit_history(held, tokens);
        }
    }

    pub fn push_history(&mut self, msg: CoreMessage, tokens: u32) {
        let is_tool = msg.role == crate::types::message::Role::Tool;
        self.admit_history(msg, tokens);
        if is_tool && !self.held_history.is_empty() && !self.awaits_tool_results() {
            for (held, tokens) in std::mem::take(&mut self.held_history) {
                self.admit_history(held, tokens);
            }
        }
    }

    /// Whether the last assistant message in history asked for a call whose result is not in
    /// history yet.
    pub(super) fn awaits_tool_results(&self) -> bool {
        let mut answered = std::collections::HashSet::new();
        for message in self.partitions.history.messages().iter().rev() {
            match message.role {
                crate::types::message::Role::Tool => {
                    if let Content::Parts(parts) = &message.content {
                        for part in parts {
                            if let ContentPart::ToolResult { call_id, .. } = part {
                                answered.insert(call_id.as_str());
                            }
                        }
                    }
                }
                crate::types::message::Role::Assistant => {
                    return message
                        .tool_calls
                        .iter()
                        .any(|call| !answered.contains(call.id.as_str()));
                }
                _ => return false,
            }
        }
        false
    }

    pub(super) fn admit_history(&mut self, msg: CoreMessage, tokens: u32) {
        self.knowledge_reference_step = self.knowledge_reference_step.saturating_add(1);
        let item = self.alloc_item_id();
        let evidence = Self::evidence_of(&msg);
        let references = self
            .partitions
            .knowledge
            .observe_references(&msg, self.knowledge_reference_step, evidence);
        for (to, strong) in references {
            let relation = if strong { Relation::ReferencesStrong } else { Relation::ReferencesLexical };
            self.add_edge(item, to, relation);
        }
        if let Content::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolResult { call_id, .. } = part
                    && let Some(call) = self.item_of_call(call_id)
                {
                    self.add_edge(item, call, Relation::Answers);
                }
            }
        }
        if evidence == crate::context::partitions::Evidence::Any {
            self.credit_expansions(item, &msg);
        }
        if let Some(record) = Self::memory_hit_record(&msg) {
            let key = format!("memory:{record}");
            let recalled = self
                .partitions
                .knowledge
                .entries
                .iter()
                .find(|entry| entry.key.as_deref() == Some(key.as_str()))
                .map(|entry| entry.id);
            if let Some(recalled) = recalled {
                self.add_edge(item, recalled, Relation::Expands);
            }
        }
        // P3 (3a): index each tool result entering working context as a handle on its item,
        // anchored to its call_id. The handle's residency is the item's: a collapsed or paged-out
        // body makes the item render as a handle, without mutating the stored message.
        if let Content::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolResult {
                    call_id, output, ..
                } = part
                {
                    let id = self.alloc_handle_id();
                    let tok = self.engine.count(output).max(1);
                    self.handles.insert(Handle {
                        item,
                        ..Handle::resident_for(id, HandleKind::ToolResult, tok, call_id.clone())
                    });
                }
            }
        }
        let tag = ItemTag::new(
            item,
            ItemOrigin::of_message(&msg),
            self.partitions.knowledge.turn,
        );
        let tally = self.tally(tag.origin);
        tally.items += 1;
        tally.tokens += u64::from(tokens);
        if tag.origin == ItemOrigin::Assistant {
            if let Some(replay) = self.pending_assistant_replay.take() {
                self.keep_item_replay(tag.id, replay);
            }
        }
        self.partitions.history.push_tagged(msg, tokens, tag);
    }

    /// Drop history from `len` on (a rolled-back turn); its items leave as removed.
    pub fn truncate_history(&mut self, len: usize) {
        // Held messages belong to the turn being rolled back.
        self.held_history.clear();
        for index in len..self.partitions.history.len() {
            let tag = self.partitions.history.tag(index);
            let tokens = self.partitions.history.measured_tokens(index, &self.engine);
            let fidelity = self.history_fidelity(index);
            self.retire_history_item(HistoryItemRecord::of(
                tag,
                tokens,
                fidelity,
                Some(ItemEnd::Removed),
            ));
        }
        self.partitions.history.truncate(len);
        self.prune_item_replays();
        self.prune_item_graph();
    }

    pub(super) fn retire_history_item(&mut self, record: HistoryItemRecord) {
        self.retired_history.push_back(record);
        if self.retired_history.len() > HISTORY_ITEM_DETAIL_CAP {
            self.retired_history.pop_front();
            self.retired_history_omitted += 1;
        }
    }

    pub(super) fn tally(&mut self, origin: ItemOrigin) -> &mut OriginTally {
        self.history_tally.entry(origin).or_insert(OriginTally {
            origin,
            items: 0,
            tokens: 0,
            resident_items: 0,
            resident_tokens: 0,
            read_back: 0,
        })
    }

    /// The model asked `read_result` for `address`: a dependency output (`dep:<agent_id>`) or an
    /// earlier tool result by call id. Either is an item it is reading back in full.
    pub fn note_read_back(&mut self, address: &str) {
        let origin = if address.starts_with("dep:") {
            ItemOrigin::DependencyOutput
        } else {
            ItemOrigin::ToolResult
        };
        self.tally(origin).read_back += 1;
        if let Some(agent) = address.strip_prefix("dep:")
            && let Some(reader) = self.last_assistant_item()
        {
            let turn = self.partitions.knowledge.turn;
            self.graph.add_external(ExternalEdge {
                from: reader,
                to: ExternalItem { agent: Some(agent.to_string()), ..ExternalItem::default() },
                relation: Relation::DependsOn,
                turn,
            });
        }
        let target = self
            .handles
            .all()
            .iter()
            .find(|handle| {
                handle.source.as_deref() == Some(address) || handle.id.to_string() == address
            })
            .map(|handle| handle.item);
        if let (Some(reader), Some(target)) = (self.last_assistant_item(), target) {
            self.add_edge(reader, target, Relation::ReadsBack);
        }
    }

    /// Plan task 7.1 · the model fetched record `record_id` with `memory(record_id)`: if the kernel
    /// recalled it, the call read that recall back. Report only.
    pub fn note_memory_read(&mut self, record_id: &str) {
        let key = format!("memory:{record_id}");
        let target = self
            .partitions
            .knowledge
            .entries
            .iter()
            .find(|entry| entry.key.as_deref() == Some(key.as_str()))
            .map(|entry| entry.id);
        if let (Some(reader), Some(target)) = (self.last_assistant_item(), target) {
            self.add_edge(reader, target, Relation::ReadsBack);
        }
    }

    /// Plan task 7.4 · the knowledge entry just seeded under `key` (the last entry when unkeyed) is
    /// a copy of `source`, a parent run's item. Report only.
    pub fn note_inherited_from(&mut self, key: Option<&str>, source: ExternalItem) {
        let entries = &self.partitions.knowledge.entries;
        let copy = match key {
            Some(key) => entries.iter().find(|entry| entry.key.as_deref() == Some(key)),
            None => entries.last(),
        };
        if let Some(from) = copy.map(|entry| entry.id) {
            let turn = self.partitions.knowledge.turn;
            self.graph.add_external(ExternalEdge {
                from,
                to: source,
                relation: Relation::InheritedFrom,
                turn,
            });
        }
    }

    pub(crate) fn alloc_item_id(&mut self) -> ItemId {
        self.last_item_id += 1;
        self.last_item_id
    }

    /// Plan task 9.1 · an edge from a note (crate-visible for `context::notes`).
    pub(crate) fn add_graph_edge(&mut self, from: ItemId, to: ItemId, relation: Relation) {
        self.add_edge(from, to, relation);
    }

    #[cfg(test)]
    pub(crate) fn resident_item_ids_for_test(&self) -> std::collections::BTreeSet<ItemId> {
        self.resident_item_ids()
    }

    /// Record an edge of the item graph, pruning it against residency when it has grown large.
    pub(super) fn add_edge(&mut self, from: ItemId, to: ItemId, relation: Relation) {
        let turn = self.partitions.knowledge.turn;
        if self.graph.add(ItemEdge { from, to, relation, turn }) && self.graph.over_limit() {
            self.prune_item_graph();
        }
    }

    /// Ids of every item still in context: history rows and knowledge entries.
    pub(super) fn resident_item_ids(&self) -> std::collections::BTreeSet<ItemId> {
        let history = &self.partitions.history;
        (0..history.len())
            .map(|index| history.tag(index).id)
            .chain(self.partitions.knowledge.entries.iter().map(|entry| entry.id))
            // Plan task 9.1 · notes are resident for as long as they exist.
            .chain(self.partitions.task_state.notes.iter().map(|note| note.item))
            .filter(|id| *id != 0)
            .collect()
    }

    pub(crate) fn prune_item_graph(&mut self) {
        let resident = self.resident_item_ids();
        self.graph.prune(&resident);
    }

    /// The assistant history item that made tool call `call_id`.
    pub(super) fn item_of_call(&self, call_id: &str) -> Option<ItemId> {
        let history = &self.partitions.history;
        (0..history.len()).rev().find_map(|index| {
            let message = &history.messages()[index];
            (message.role == crate::types::message::Role::Assistant
                && message.tool_calls.iter().any(|call| call.id.as_str() == call_id))
            .then(|| history.tag(index).id)
            .filter(|id| *id != 0)
        })
    }

    /// The latest assistant history item: the one whose tool calls are being answered.
    pub(super) fn last_assistant_item(&self) -> Option<ItemId> {
        let history = &self.partitions.history;
        (0..history.len()).rev().find_map(|index| {
            (history.tag(index).origin == ItemOrigin::Assistant)
                .then(|| history.tag(index).id)
                .filter(|id| *id != 0)
        })
    }

    pub(super) fn alloc_handle_id(&mut self) -> HandleId {
        let id = self.next_handle_id;
        self.next_handle_id = self.next_handle_id.wrapping_add(1);
        id
    }

    /// The next handle id this allocator will hand out.
    ///
    /// Read by the §12.1 checkpoint projection: a restored kernel that restarted the allocator
    /// would re-issue an id an outstanding `LoadPayload` effect still addresses.
    pub fn next_handle_id(&self) -> HandleId {
        self.next_handle_id
    }

    /// §12.2 · reinstall the allocator a checkpoint recorded.
    ///
    /// The mirror of [`Self::next_handle_id`], and the reason handle identity survives a restore:
    /// the handle *table* is repopulated by id, and this is what stops the next allocation from
    /// colliding with one of them.
    pub fn restore_next_handle_id(&mut self, next: HandleId) {
        self.next_handle_id = next;
    }

    pub fn frozen_history_len(&self) -> usize {
        self.frozen_history_len
    }

    pub(crate) fn knowledge_checkpoint_state(&self) -> (u64, bool) {
        (self.knowledge_reference_step, self.knowledge_budget_warned)
    }

    pub(crate) fn restore_knowledge_checkpoint_state(&mut self, reference_step: u64, warned: bool) {
        self.knowledge_reference_step = reference_step;
        self.knowledge_budget_warned = warned;
    }

    pub(crate) fn cache_checkpoint_state(&self) -> (bool, u64) {
        (self.prefix_sent, self.cache_invalidated_tokens)
    }

    pub(crate) fn restore_cache_checkpoint_state(&mut self, prefix_sent: bool, invalidated: u64) {
        self.prefix_sent = prefix_sent;
        self.cache_invalidated_tokens = invalidated;
    }

    /// Turn on kernel-owned memory recall for this operation, `top_k` hits per recall.
    pub fn enable_recall(&mut self, top_k: u32) {
        self.recall_top_k = Some(top_k.max(1));
    }

    /// Take the owed recall, if any: the query text and width the kernel asks the host for.
    pub fn take_due_recall(&mut self) -> Option<(String, u32)> {
        let top_k = self.recall_top_k?;
        if !std::mem::take(&mut self.recall_due) {
            return None;
        }
        let goal = self.partitions.task_state.goal.trim();
        (!goal.is_empty()).then(|| (goal.to_string(), top_k))
    }

    /// The index line one recalled record becomes. The header names the record, its name, kind and
    /// trust; the body is its one-line description (its name when it has none). The model reads
    /// the record in full with `memory(record_id)`.
    pub fn recall_index_line(record_id: &str, kind: &str, trust: &str, description: &str, name: &str) -> String {
        crate::context::voice::recall_index_line(record_id, kind, trust, description, name)
    }

    pub fn memory_name_field(name: &str) -> String {
        crate::context::voice::memory_name_field(name)
    }

    /// Land recalled records as keyed `memory_recall` items `memory:<record_id>`. The key dedupes a
    /// record across recalls; a record already resident keeps its bytes and has its lease renewed.
    pub fn push_recalled(&mut self, lines: Vec<(String, String)>) {
        for (record_id, line) in lines {
            let key = CompactString::from(format!("memory:{record_id}"));
            if self.partitions.knowledge.renew(&key) {
                continue;
            }
            let tokens = self.engine.count(&line).max(1);
            self.push_knowledge_item(
                ItemOrigin::MemoryRecall,
                Some(key),
                CoreMessage::system(line),
                tokens,
                false,
            );
        }
    }

    /// The model read memory record `record_id` in full: its index line was used.
    pub fn note_memory_expanded(&mut self, record_id: &str) -> bool {
        self.partitions
            .knowledge
            .note_expanded(&format!("memory:{record_id}"))
    }

    /// Advance the item table to `turn` and mark every item whose lease ran out to leave at the
    /// next boundary. Runs at the head of every event, like the skill lease sweep.
    pub fn sweep_expired_item_leases(&mut self, turn: u32) {
        self.partitions.knowledge.turn = turn;
        let held = self.held_by_resident_items();
        self.partitions.knowledge.expire_leases(&held);
    }

    /// Plan task 7.3 · knowledge items a resident item still holds by a strong reference or a
    /// read-back: they neither expire nor yield to the knowledge budget.
    pub(super) fn held_by_resident_items(&self) -> std::collections::BTreeSet<ItemId> {
        let history: std::collections::BTreeSet<ItemId> = (0..self.partitions.history.len())
            .map(|index| self.partitions.history.tag(index).id)
            .collect();
        self.graph
            .edges()
            .iter()
            .filter(|edge| {
                matches!(edge.relation, Relation::ReferencesStrong | Relation::ReadsBack)
                    && history.contains(&edge.from)
            })
            .map(|edge| edge.to)
            .collect()
    }

    /// Plan task 7.3 · how a history message's mentions of knowledge count: the model's words and
    /// the user's by any evidence, host output only by name, a memory hit (the record's own text,
    /// written into history by the kernel) never.
    pub(super) fn evidence_of(message: &CoreMessage) -> crate::context::partitions::Evidence {
        use crate::context::partitions::Evidence;
        match message.role {
            crate::types::message::Role::Assistant => Evidence::Any,
            crate::types::message::Role::User if Self::memory_hit_record(message).is_some() => {
                Evidence::None
            }
            crate::types::message::Role::User => Evidence::Any,
            _ => Evidence::StrongOnly,
        }
    }

    /// The record a kernel-written memory hit (`[MEMORY record_ref=<id> name=… kind=…] <content>`) carries.
    pub(super) fn memory_hit_record(message: &CoreMessage) -> Option<&str> {
        if message.role != crate::types::message::Role::User {
            return None;
        }
        let text = message.content.as_text()?;
        let rest = text.strip_prefix("[MEMORY record_ref=")?;
        let end = rest.find([' ', ']'])?;
        Some(&rest[..end]).filter(|id| !id.is_empty())
    }

    /// Plan task 7.3 · one hop: `item` (counted evidence) used the content of a resident memory hit
    /// that expands a recalled item; the use reaches the recalled item along the `expands` edge.
    pub(super) fn credit_expansions(&mut self, item: ItemId, message: &CoreMessage) {
        let history = &self.partitions.history;
        let carriers: Vec<(ItemId, ItemId, usize)> = self
            .graph
            .edges()
            .iter()
            .filter(|edge| edge.relation == Relation::Expands)
            .filter_map(|edge| {
                (0..history.len())
                    .find(|index| history.tag(*index).id == edge.from)
                    .map(|index| (edge.from, edge.to, index))
            })
            .collect();
        for (carrier, recalled, index) in carriers {
            let carried = &self.partitions.history.messages()[index];
            if !crate::context::partitions::mentions(message, carried) {
                continue;
            }
            self.add_edge(item, carrier, Relation::ReferencesLexical);
            self.partitions
                .knowledge
                .credit_lexical(recalled, self.knowledge_reference_step);
        }
    }

    /// Push a runtime signal into the current turn's State slot.
    /// Rendering does not consume signals. The state machine clears only the prefix acknowledged by
    /// a correlated provider result, so provider failures and retries see the same signal payload.
    /// Plan task 1.7 · count text the kernel wrote for the model (a result it answered a call
    /// with); signals are counted by [`Self::commit_delivered_signals`].
    pub fn note_kernel_text(&mut self, text: &str) {
        self.kernel_text_tokens = self.kernel_text_tokens.saturating_add(u64::from(self.engine.count(text)));
    }

    /// Count the results the kernel answered calls with.
    pub fn note_kernel_results(&mut self, results: &[crate::types::message::ToolResult]) {
        for result in results {
            if let Some(text) = result.output.as_text() {
                self.note_kernel_text(text);
            }
        }
    }

    /// The request that carried the delivered signals committed: count them, then let them go.
    pub fn commit_delivered_signals(&mut self) {
        let tokens: u64 = self
            .partitions
            .signals
            .iter()
            .filter(|signal| signal.delivered)
            .map(|signal| u64::from(self.engine.count(&signal.text)))
            .sum();
        self.kernel_text_tokens = self.kernel_text_tokens.saturating_add(tokens);
        self.partitions.drop_delivered_signals();
    }

    pub fn push_signal(&mut self, text: String) {
        self.partitions.signals.push(SignalItem::new(text));
    }

    /// Record a durable user directive in the (non-compressible, renewal-carried) task_state, so a
    /// mid-task user command keeps its salience across compaction/renewal — unlike the ephemeral
    /// signal channel, which is cleared on renewal.
    pub fn record_directive(&mut self, text: impl Into<String>) {
        self.partitions.task_state.record_directive(text);
    }
}
