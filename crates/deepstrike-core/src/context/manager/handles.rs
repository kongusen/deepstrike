//! Where each tool result's body lives (the handle table) and what a history item renders as.

use super::*;

impl ContextManager {
    // ── Layer 4: read-time projection (handle residency) ────────────────────

    /// Start a fresh collapse generation: un-collapse every `Collapsed` handle back to `Resident`.
    /// Called only at compaction/renewal boundaries — the sole points where un-collapsing is
    /// cache-free, since the rendered prefix is rewritten there regardless. Between boundaries
    /// [`Self::recompute_handle_residency`] keeps collapse strictly one-way (P0-C). Spooled/paged-out
    /// handles are untouched (they leave the Resident↔Collapsed cycle deliberately).
    pub fn reset_collapse_generation(&mut self) {
        for handle in self.handles.all_mut() {
            if matches!(handle.residency, Residency::Collapsed) {
                handle.residency = Residency::Resident;
            }
        }
    }

    /// Drop handles whose anchored source message no longer lives in `partitions.history` — i.e.
    /// archived by a compaction or dropped on renewal. Without this the handle table grows with
    /// total session length (a handle per tool result, never removed), which also inflates the
    /// per-turn `recompute_handle_residency` scan. Called at compaction/renewal boundaries, so the
    /// table tracks the working set, not the whole session. Handles with no `source` anchor (future
    /// non-tool-result kinds) are always kept — they can't be orphaned by this check.
    ///
    /// §25.9 · a handle whose body lives **outside** core is also always kept, whatever happened to
    /// the message it was anchored to. The handle table is the kernel's only record of an external
    /// payload: pruning one does not free the body, it only makes the body permanently unreachable
    /// — and page-in is exactly the operation that outlives the preview's stay in working context.
    pub fn prune_orphaned_handles(&mut self) {
        let live_items: std::collections::HashSet<ItemId> =
            self.partitions.history.tags().iter().map(|tag| tag.id).collect();
        // A handle restored from a checkpoint without item ids is matched by its call_id.
        let live: std::collections::HashSet<CompactString> = self
            .partitions
            .history
            .messages()
            .iter()
            .flat_map(|m| match &m.content {
                Content::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::ToolResult { call_id, .. } => Some(call_id.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        self.handles.retain(|h| {
            h.residency.payload_ref().is_some()
                || if h.item != 0 {
                    live_items.contains(&h.item)
                } else {
                    h.source.as_ref().is_none_or(|s| live.contains(s))
                }
        });
    }

    /// Give every handle restored without an item id the id of the history item holding its body.
    pub(crate) fn backfill_handle_items(&mut self) {
        let missing: Vec<(HandleId, CompactString)> = self
            .handles
            .all()
            .iter()
            .filter(|h| h.item == 0)
            .filter_map(|h| Some((h.id, h.source.clone()?)))
            .collect();
        for (id, source) in missing {
            if let Some(item) = self.history_item_holding(&source) {
                if let Some(handle) = self.handles.get_mut(id) {
                    handle.item = item;
                }
            }
        }
    }

    /// The id of the history item holding the tool result `call_id`, if one does.
    pub(super) fn history_item_holding(&self, call_id: &str) -> Option<ItemId> {
        let history = &self.partitions.history;
        history.messages().iter().position(|message| match &message.content {
            Content::Parts(parts) => parts.iter().any(|part| {
                matches!(part, ContentPart::ToolResult { call_id: id, .. } if id.as_str() == call_id)
            }),
            _ => false,
        })
        .map(|index| history.tag(index).id)
        .filter(|id| *id != 0)
    }

    /// What history item `index` renders: a handle while one of its bodies is collapsed or paged
    /// out, otherwise its stored form (full, or the excerpt compaction left).
    pub fn history_fidelity(&self, index: usize) -> crate::context::item::Fidelity {
        let tag = self.partitions.history.tag(index);
        if tag.id != 0 && self.handles.projected_items().contains(&tag.id) {
            crate::context::item::Fidelity::Handle
        } else {
            tag.fidelity
        }
    }

    /// §7.10 / §25.9 · move the payload residency of the handle addressed by `source`, minting the
    /// handle when the kernel is hearing about this body for the first time (a page-out archive has
    /// no anchored message: its handle *is* the address). Returns the residency it replaced, or
    /// `None` when the handle was minted here.
    ///
    /// This is the **single** writer of the external/paged-out axis, which is what makes "the P3
    /// handle table is the only kernel fact about where a body lives" checkable rather than
    /// aspirational: every caller goes through here, and every caller therefore has a residency
    /// transition to observe.
    ///
    /// `tokens` is the weight this handle contributes to working context, so it is `0` for every
    /// body that lives outside core — an external result's anchored message holds the *preview*,
    /// not the body, so unlike a `Collapsed` handle there is no over-count to discount — and the
    /// body's real weight only when a page-in brought it home.
    pub fn set_payload_residency(
        &mut self,
        source: &str,
        kind: HandleKind,
        tokens: u32,
        residency: Residency,
    ) -> Option<Residency> {
        if let Some(handle) = self
            .handles
            .all_mut()
            .iter_mut()
            .find(|h| h.source.as_deref() == Some(source))
        {
            let previous = std::mem::replace(&mut handle.residency, residency);
            handle.tokens = tokens;
            return Some(previous);
        }
        let id = self.alloc_handle_id();
        let item = self.history_item_holding(source).unwrap_or(0);
        self.handles.insert(Handle {
            id,
            kind,
            residency,
            tokens,
            source: Some(source.into()),
            item,
        });
        None
    }

    /// The payload residency of the handle addressed by `source`. The read half of
    /// [`Self::set_payload_residency`] — a page-in asks this before it can address anything.
    ///
    /// `source` **is** the wire address of a handle: the tool `call_id` for a result, which is
    /// exactly what the truncation marker tells the model to pass back to `read_result`, and the
    /// kernel-minted archive id for a page-out.
    pub fn payload_residency(&self, source: &str) -> Option<&Residency> {
        self.handles.residency_for_source(source)
    }
}
