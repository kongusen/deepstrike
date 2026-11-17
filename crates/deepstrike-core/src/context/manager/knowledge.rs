//! Knowledge (system[1]): placement against the cached prefix, host writes, the budget, sweeps.

use super::*;

impl ContextManager {
    /// Place a just-appended knowledge entry. Before the prefix is sent it stays in system[1].
    /// After, it is staged in the state turn: re-sending it uncached each turn costs less than
    /// re-caching the history behind system[1] until the staged tokens reach a quarter of the
    /// history, at which point every staged entry is promoted at once.
    pub(super) fn place_appended_knowledge(&mut self, entries_before: usize) {
        if self.partitions.knowledge.entries.len() == entries_before {
            return;
        }
        let id = self.alloc_item_id();
        let knowledge = &mut self.partitions.knowledge;
        if let Some(entry) = knowledge.entries.last_mut() {
            entry.id = id;
        }
        if !self.prefix_sent {
            return;
        }
        if let Some(entry) = knowledge.entries.last_mut() {
            entry.staged = true;
        }
        let history = self.partitions.history.token_count();
        if self.partitions.knowledge.staged_tokens().saturating_mul(4) >= history {
            self.partitions.knowledge.promote_staged();
            self.cache_ledger.note_cause(BreakCause::Promotion);
            self.cache_invalidated_tokens += u64::from(history);
            self.prefix_sent = false;
        }
    }

    pub fn restore_frozen_history_len(&mut self, len: usize) -> bool {
        if len > self.partitions.history.len() {
            return false;
        }
        self.frozen_history_len = len;
        true
    }

    /// Push content into the Knowledge slot (memory retrievals, skill defs, artifacts).
    pub fn push_knowledge(&mut self, msg: CoreMessage, tokens: u32) {
        let before = self.partitions.knowledge.entries.len();
        self.partitions.knowledge.push(msg, tokens);
        self.place_appended_knowledge(before);
    }

    /// K1: keyed knowledge push — a fresh key appends (placed by
    /// [`Self::place_appended_knowledge`]), an existing key stages a boundary-deferred upsert.
    /// `pinned` entries are exempt from the K2 budget sweep.
    pub fn push_knowledge_entry(
        &mut self,
        key: Option<CompactString>,
        msg: CoreMessage,
        tokens: u32,
        pinned: bool,
    ) {
        let origin = ItemOrigin::for_host_key(key.as_deref());
        self.push_knowledge_item(origin, key, msg, tokens, pinned);
    }

    /// [`Self::push_knowledge_entry`] for an item whose origin the caller knows.
    pub fn push_knowledge_item(
        &mut self,
        origin: ItemOrigin,
        key: Option<CompactString>,
        msg: CoreMessage,
        tokens: u32,
        pinned: bool,
    ) {
        let before = self.partitions.knowledge.entries.len();
        self.partitions
            .knowledge
            .push_item(origin, key, msg, tokens, pinned);
        self.place_appended_knowledge(before);
    }

    /// K1: mark a keyed knowledge entry for removal at the next compaction/renewal boundary.
    /// Errs-open: unknown key is a no-op (returns false).
    /// Plan task 1.1 · scope: the parent's knowledge a child spawned with `inheritance` starts with,
    /// as `(key, content, tokens, pinned, item id)`. Besides what the scope allows, a child never inherits
    /// an item whose life is tied to the parent's own state: one leased until a parent event (an
    /// active skill's body) or one the kernel derived for the parent (the child's kernel derives
    /// its own).
    pub fn inheritable_knowledge(
        &self,
        inheritance: crate::types::agent::ContextInheritance,
    ) -> Vec<(Option<String>, String, u32, bool, ItemId)> {
        self.partitions
            .knowledge
            .entries
            .iter()
            .filter(|entry| !entry.is_leaving())
            .filter(|entry| entry.defaults().scope.inherited_by(inheritance))
            .filter(|entry| !matches!(entry.lease, crate::context::item::Lease::Until(_)))
            .filter(|entry| entry.defaults().authority != Authority::Kernel)
            .filter_map(|entry| {
                let text = entry.message.content.as_text()?.to_string();
                Some((
                    entry.key.as_ref().map(ToString::to_string),
                    text,
                    entry.tokens,
                    entry.pinned,
                    entry.id,
                ))
            })
            .collect()
    }

    /// Plan task 1.1 · authority: whether the host may upsert `upserts` and remove `removals`. A host
    /// command may not touch an item the kernel or the provider wrote, and may not remove a skill
    /// body while the model's activation holds it (deactivating the skill removes it). Returns the
    /// first refused key and why.
    pub fn check_host_knowledge_write<'a>(
        &self,
        upserts: impl IntoIterator<Item = &'a str>,
        removals: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), String> {
        let live = |key: &str| {
            self.partitions
                .knowledge
                .entries
                .iter()
                .find(|entry| !entry.is_leaving() && entry.key.as_deref() == Some(key))
        };
        use crate::context::authority::{admit_write, WriteOp};
        for key in upserts {
            if let Some(entry) = live(key) {
                admit_write(entry.origin, Authority::Host, WriteOp::Replace)
                    .map_err(|why| format!("knowledge {key:?}: {why}"))?;
            }
        }
        for key in removals {
            let Some(entry) = live(key) else { continue };
            admit_write(entry.origin, Authority::Host, WriteOp::Remove)
                .map_err(|why| format!("knowledge {key:?}: {why}"))?;
            // Not a table cell: whether the body may go depends on whether its skill is active now.
            match entry.defaults().authority {
                Authority::Model
                    if entry.origin == ItemOrigin::Skill
                        && key
                            .strip_prefix("skill:")
                            .is_some_and(|name| self.active_skills.contains_key(name)) =>
                {
                    return Err(crate::context::voice::refusal::active_skill_body(key));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn remove_knowledge(&mut self, key: &str) -> bool {
        let removed = self.partitions.knowledge.remove(key);
        if removed {
            }
        removed
    }

    /// K1: run the boundary sweep (apply pending upserts, drop marked entries) and stash the
    /// result for the state machine to drain into a `KnowledgeSwept` observation. Called only
    /// from the compaction/renewal boundary blocks — the one place system[1] bytes may change.
    pub(super) fn sweep_knowledge_at_boundary(&mut self) {
        let sweep = self.partitions.knowledge.sweep_at_boundary();
        if sweep.changed {
                // P9: the model must not have knowledge silently vanish under it. The boundary
            // already broke the prompt-cache prefix, so a one-line ephemeral tail note is
            // cache-free; keyed removals name what left and how to get it back.
            if !sweep.removed_keys.is_empty() {
                self.partitions
                    .signals
                    .push(SignalItem::new(crate::context::voice::knowledge_removed(&sweep.removed_keys)));
            }
            self.pending_knowledge_sweeps.push(sweep);
        }
        // K2: a boundary starts a fresh cache generation — the budget warning may fire again.
        self.knowledge_budget_warned = false;
        self.prefix_sent = false;
        self.prune_item_graph();
    }

    /// K2: knowledge-budget check, run per turn before render. Over budget ⇒ mark the LOWEST-VALUE
    /// unpinned, non-skill entries for eviction at the next boundary until the projected usage
    /// (used − already-marked) fits, and return `Some((used, budget))` ONCE per cache generation
    /// for the `KnowledgeBudgetExceeded` observation (marking itself is idempotent and repeats
    /// harmlessly). Skill pins are exempt — deactivation/lease governs them, the budget never
    /// silently unloads a skill the model believes is active. If marking every eligible entry
    /// still exceeds the budget, the warning stands and the overweight remainder is the host's
    /// explicit choice (errs-open). `knowledge_budget_ratio <= 0.0` disables.
    pub fn enforce_knowledge_budget(&mut self) -> Option<(u32, u32)> {
        let ratio = self.config.knowledge_budget_ratio;
        if ratio <= 0.0 {
            return None;
        }
        let budget = (self.max_tokens as f64 * ratio) as u32;
        let used = self.partitions.knowledge.token_count;
        if used <= budget {
            return None;
        }
        let marked: u32 = self
            .partitions
            .knowledge
            .entries
            .iter()
            .filter(|e| e.is_leaving())
            .map(|e| e.tokens)
            .sum();
        let mut projected = used.saturating_sub(marked);
        let held = self.held_by_resident_items();
        let mut candidates = self
            .partitions
            .knowledge
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                !entry.is_leaving()
                    && !held.contains(&entry.id)
                    && !entry.pinned
                    && !entry
                        .key
                        .as_deref()
                        .is_some_and(|key| key.starts_with("skill:"))
            })
            .map(|(index, _)| {
                let score = self
                    .partitions
                    .knowledge
                    .retention_score(index, self.knowledge_reference_step)
                    .unwrap_or(i64::MIN);
                (score, index)
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        for (_, index) in candidates {
            if projected <= budget {
                break;
            }
            let entry = &mut self.partitions.knowledge.entries[index];
            entry.leaving = Some(ItemEnd::Evicted);
            projected = projected.saturating_sub(entry.tokens);
        }
        if self.knowledge_budget_warned {
            return None;
        }
        self.knowledge_budget_warned = true;
        Some((used, budget))
    }

    /// K1: drain boundary-sweep results (state-machine side turns these into observations).
    pub fn take_knowledge_sweeps(&mut self) -> Vec<crate::context::partitions::KnowledgeSweep> {
        std::mem::take(&mut self.pending_knowledge_sweeps)
    }
}
