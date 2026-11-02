use super::config::ContextConfig;
use super::item::{ContextItem, ItemEnd, ItemOrigin, ItemRecord, ItemTag, Lease};
use super::measurement::TokenMeasurement;
use super::task_state::TaskState;
use super::token_engine::ContextTokenEngine;
use crate::mm::value::{RetentionFeatures, RetentionKind, deterministic_retention_score};
use crate::types::message::CoreMessage;

/// A single context partition — a named bucket of messages with a token counter.
#[derive(Debug, Clone)]
pub struct Partition {
    pub messages: Vec<CoreMessage>,
    /// Host-side measurements aligned with `messages`; not part of canonical message state.
    pub measurements: Vec<TokenMeasurement>,
    /// Plan task 1.1b · each message's item identity, aligned with `messages`. A message pushed
    /// without one reads as [`ItemTag::untracked`].
    pub items: Vec<ItemTag>,
    pub token_count: u32,
}

impl Partition {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            measurements: Vec::new(),
            items: Vec::new(),
            token_count: 0,
        }
    }

    pub fn push(&mut self, msg: CoreMessage, token_count: u32) {
        let tag = ItemTag::untracked(&msg);
        self.push_tagged(msg, token_count, tag);
    }

    pub fn push_tagged(&mut self, msg: CoreMessage, token_count: u32, tag: ItemTag) {
        self.align_items();
        let measurement = TokenMeasurement::for_message(&msg, token_count);
        self.token_count += token_count;
        self.messages.push(msg);
        self.measurements.push(measurement);
        self.items.push(tag);
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.measurements.clear();
        self.items.clear();
        self.token_count = 0;
    }

    /// The item identity of message `index`.
    pub fn tag(&self, index: usize) -> ItemTag {
        self.items.get(index).copied().unwrap_or_else(|| {
            self.messages
                .get(index)
                .map(ItemTag::untracked)
                .unwrap_or(ItemTag { id: 0, origin: ItemOrigin::User, injected_turn: 0 })
        })
    }

    /// Pad `items` to `messages` (a message written straight into `messages` has no tag yet).
    pub fn align_items(&mut self) {
        while self.items.len() < self.messages.len() {
            let tag = ItemTag::untracked(&self.messages[self.items.len()]);
            self.items.push(tag);
        }
        self.items.truncate(self.messages.len());
    }

    pub fn measured_tokens(&self, index: usize, engine: &ContextTokenEngine) -> u32 {
        self.measurements
            .get(index)
            .filter(|measurement| {
                self.messages
                    .get(index)
                    .is_some_and(|message| measurement.matches_message(message))
            })
            .map(|m| m.tokens)
            .unwrap_or_else(|| {
                self.messages
                    .get(index)
                    .map(|m| engine.count_message(m))
                    .unwrap_or(0)
            })
    }

    pub fn set_measured_tokens(&mut self, index: usize, tokens: u32) {
        if let Some(message) = self.messages.get(index) {
            let measurement = TokenMeasurement::for_message(message, tokens);
            if let Some(slot) = self.measurements.get_mut(index) {
                *slot = measurement;
            } else {
                self.measurements.push(measurement);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

impl Default for Partition {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of one boundary sweep, for the `KnowledgeSwept` kernel observation.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeSweep {
    pub removed_keys: Vec<String>,
    pub tokens_freed: u32,
    /// True when the sweep changed anything (removal OR applied upsert).
    pub changed: bool,
}

/// The knowledge tier of the item table: identity-bearing [`ContextItem`]s rendered into
/// system[1].
///
/// system[1] renders before the history turns, so ANY change to it — an append included —
/// invalidates the cached history behind it. Appends are therefore immediate only while the
/// current prefix has not been sent; afterwards a fresh item is `staged` (rendered in the state
/// turn) until the next compaction/renewal boundary. Mutation and removal of existing items are
/// boundary-deferred the same way (see [`ContextItem::leaving`]).
#[derive(Debug, Clone, Default)]
pub struct KnowledgePartition {
    pub entries: Vec<ContextItem>,
    pub token_count: u32,
    /// The kernel turn new items are stamped with and leases are counted against.
    pub turn: u32,
    /// Keys a boundary sweep removed. A later push of one of them is regret.
    pub evicted_keys: std::collections::BTreeSet<String>,
    /// Evicted keys that came back.
    pub regret: u32,
    /// Tokens of items removed without ever being referenced.
    pub wasted_tokens: u64,
    /// Items that left, in the order they left (plan task 4.1).
    pub retired: Vec<ItemRecord>,
}

impl KnowledgePartition {
    pub fn new() -> Self {
        Self::default()
    }

    /// Unkeyed immediate append — exactly `push_entry(None, msg, tokens, false)`.
    pub fn push(&mut self, msg: CoreMessage, token_count: u32) {
        self.push_entry(None, msg, token_count, false);
    }

    /// Host-supplied knowledge: its origin follows from the key ([`ItemOrigin::for_host_key`]).
    pub fn push_entry(
        &mut self,
        key: Option<compact_str::CompactString>,
        msg: CoreMessage,
        tokens: u32,
        pinned: bool,
    ) {
        let origin = ItemOrigin::for_host_key(key.as_deref());
        self.push_item(origin, key, msg, tokens, pinned);
    }

    /// Keyed push: a fresh key (or `None`) appends immediately; an existing key stages a
    /// boundary-deferred upsert, renews the item's lease and clears any pending removal (the item
    /// is wanted again). `pinned` takes effect immediately in both cases (it is bookkeeping, not
    /// rendered bytes).
    pub fn push_item(
        &mut self,
        origin: ItemOrigin,
        key: Option<compact_str::CompactString>,
        msg: CoreMessage,
        tokens: u32,
        pinned: bool,
    ) {
        let turn = self.turn;
        if let Some(ref k) = key {
            if let Some(entry) = self.entries.iter_mut().find(|e| e.key.as_ref() == Some(k)) {
                entry.pending = Some(Box::new((msg, tokens)));
                entry.leaving = None;
                entry.pinned = pinned;
                entry.lease = if pinned { Lease::Permanent } else { entry.origin.defaults().lease };
                entry.value.injected_turn = turn;
                return;
            }
        }
        let mut item = ContextItem::new(origin, key, msg, tokens, pinned, turn);
        if let Some(ref k) = item.key {
            if self.evicted_keys.remove(k.as_str()) {
                self.regret += 1;
                item.value.paged_in_after_evict = true;
            }
        }
        self.token_count += tokens;
        self.entries.push(item);
    }

    /// Renew a resident item's lease from `turn`, as a fresh push of the same content would.
    /// Returns whether a live item holds `key`.
    pub fn renew(&mut self, key: &str) -> bool {
        let turn = self.turn;
        match self
            .entries
            .iter_mut()
            .find(|entry| !entry.is_leaving() && entry.key.as_deref() == Some(key))
        {
            Some(entry) => {
                entry.value.injected_turn = turn;
                true
            }
            None => false,
        }
    }

    /// The model asked for the full form behind `key` (for a recall, `memory(record_id)`): the
    /// strongest reference there is. A leaving item is wanted again.
    pub fn note_expanded(&mut self, key: &str) -> bool {
        let turn = self.turn;
        match self.entries.iter_mut().find(|entry| entry.key.as_deref() == Some(key)) {
            Some(entry) => {
                entry.value.expanded = true;
                entry.value.ref_count = entry.value.ref_count.saturating_add(1);
                entry.value.last_referenced_turn = Some(turn);
                if entry.leaving == Some(ItemEnd::Expired) {
                    entry.leaving = None;
                }
                true
            }
            None => false,
        }
    }

    /// Mark every item whose lease ran out at the current turn to leave at the next boundary.
    /// Returns the keys newly marked.
    pub fn expire_leases(&mut self) -> Vec<String> {
        let turn = self.turn;
        let mut expired = Vec::new();
        for entry in self.entries.iter_mut().filter(|entry| !entry.is_leaving()) {
            if entry.lease_expired(turn) {
                entry.leaving = Some(ItemEnd::Expired);
                if let Some(key) = &entry.key {
                    expired.push(key.to_string());
                }
            }
        }
        expired
    }

    /// Promote every staged entry into system[1]. Returns the promoted tokens.
    pub fn promote_staged(&mut self) -> u32 {
        let mut promoted = 0;
        for entry in self.entries.iter_mut().filter(|entry| entry.staged) {
            entry.staged = false;
            promoted += entry.tokens;
        }
        promoted
    }

    /// Tokens currently staged outside system[1].
    pub fn staged_tokens(&self) -> u32 {
        self.entries
            .iter()
            .filter(|entry| entry.staged)
            .map(|entry| entry.tokens)
            .sum()
    }

    /// The system[1] messages, in entry order.
    pub fn resident_messages(&self) -> impl Iterator<Item = &CoreMessage> {
        self.entries
            .iter()
            .filter(|entry| !entry.staged)
            .map(|entry| &entry.message)
    }

    /// The staged messages, in entry order (rendered in the state turn).
    pub fn staged_messages(&self) -> impl Iterator<Item = &CoreMessage> {
        self.entries
            .iter()
            .filter(|entry| entry.staged)
            .map(|entry| &entry.message)
    }

    /// Record references from a journal-derived history message. This is deliberately driven by
    /// committed input, never by render-time wall clocks. Exact keyed references always count;
    /// otherwise two shared content terms (one for a one-term entry) are required.
    pub fn observe_references(&mut self, message: &CoreMessage, step: u64) {
        let text = searchable_message_text(message);
        let input_terms = lexical_terms(&text);
        if text.is_empty() || input_terms.is_empty() {
            return;
        }
        let folded = text.to_lowercase();
        let turn = self.turn;
        for entry in &mut self.entries {
            if entry.is_leaving() {
                continue;
            }
            let exact_key = entry
                .key
                .as_deref()
                .filter(|key| key.len() >= 3)
                .is_some_and(|key| folded.contains(&key.to_lowercase()));
            let entry_text = searchable_message_text(&entry.message);
            let entry_terms = lexical_terms(&entry_text);
            let overlap = entry_terms.intersection(&input_terms).count();
            let lexical_hit =
                !entry_terms.is_empty() && overlap >= if entry_terms.len() == 1 { 1 } else { 2 };
            if exact_key || lexical_hit {
                entry.value.ref_count = entry.value.ref_count.saturating_add(1);
                entry.value.last_referenced_step = Some(step);
                entry.value.last_referenced_turn = Some(turn);
            }
        }
    }

    pub fn retention_score(&self, index: usize, current_step: u64) -> Option<i64> {
        let entry = self.entries.get(index)?;
        Some(deterministic_retention_score(RetentionFeatures {
            pinned: entry.pinned,
            use_count: entry.value.ref_count,
            last_used_step: entry.value.last_referenced_step,
            current_step,
            lease_remaining_steps: entry.lease_remaining_turns(self.turn).map(u64::from),
            kind: retention_kind(entry.key.as_deref()),
            tokens: entry.tokens,
            confidence_ppm: 0,
            stale_discount_ppm: 0,
        }))
    }

    /// Withdraw content staged under `key` for something the kernel then refused (a skill whose
    /// activation was rejected). A freshly appended entry has never been rendered, so it is dropped
    /// outright; an existing entry keeps its rendered bytes, loses the staged upsert, and leaves at
    /// the next boundary. Returns whether anything was withdrawn.
    pub fn retract_staged(&mut self, key: &str) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.key.as_deref() == Some(key))
        else {
            return false;
        };
        let entry = &mut self.entries[index];
        if entry.pending.take().is_some() {
            entry.leaving = Some(ItemEnd::Removed);
            return true;
        }
        let entry = self.entries.remove(index);
        self.token_count = self.token_count.saturating_sub(entry.tokens);
        self.retired.push(entry.record(Some(ItemEnd::Removed)));
        true
    }

    /// A live entry holds `key` (one not already marked for removal).
    pub fn contains_key(&self, key: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| !entry.is_leaving() && entry.key.as_deref() == Some(key))
    }

    /// Mark the keyed entry for removal at the next boundary. Errs-open: unknown key is a no-op.
    /// Returns whether a matching entry was marked.
    pub fn remove(&mut self, key: &str) -> bool {
        match self
            .entries
            .iter_mut()
            .find(|e| e.key.as_deref() == Some(key))
        {
            Some(entry) => {
                entry.leaving = Some(ItemEnd::Removed);
                entry.pending = None;
                true
            }
            None => false,
        }
    }

    /// Apply pending upserts and drop marked entries (and every `until_boundary` one). Call ONLY at
    /// compaction/renewal boundaries — this is the one place existing system[1] bytes may be
    /// rewritten.
    pub fn sweep_at_boundary(&mut self) -> KnowledgeSweep {
        let mut sweep = KnowledgeSweep::default();
        if self.promote_staged() > 0 {
            sweep.changed = true;
        }
        for entry in &mut self.entries {
            if let Some(replacement) = entry.pending.take() {
                let (msg, tokens) = *replacement;
                self.token_count = self.token_count - entry.tokens + tokens;
                entry.message = msg;
                entry.tokens = tokens;
                sweep.changed = true;
            }
            if entry.lease == Lease::UntilBoundary && !entry.is_leaving() {
                entry.leaving = Some(ItemEnd::Expired);
            }
        }
        let before = self.entries.len();
        let (evicted_keys, wasted_tokens, retired) =
            (&mut self.evicted_keys, &mut self.wasted_tokens, &mut self.retired);
        self.entries.retain(|e| {
            let Some(end) = e.leaving else {
                return true;
            };
            if let Some(ref k) = e.key {
                sweep.removed_keys.push(k.to_string());
                evicted_keys.insert(k.to_string());
            }
            if e.value.ref_count == 0 {
                *wasted_tokens += u64::from(e.tokens);
            }
            sweep.tokens_freed += e.tokens;
            retired.push(e.record(Some(end)));
            false
        });
        if self.entries.len() != before {
            self.token_count = self.token_count.saturating_sub(sweep.tokens_freed);
            sweep.changed = true;
        }
        sweep
    }

    /// Every item of the run, retired ones first (in the order they left), then the resident ones.
    pub fn item_records(&self) -> Vec<ItemRecord> {
        self.retired
            .iter()
            .cloned()
            .chain(self.entries.iter().map(|entry| entry.record(None)))
            .collect()
    }

    /// The rendered messages, in entry order (renderer / snapshot surface).
    pub fn messages(&self) -> impl Iterator<Item = &CoreMessage> {
        self.entries.iter().map(|e| &e.message)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn retention_kind(key: Option<&str>) -> RetentionKind {
    match key
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
    {
        "user" => RetentionKind::User,
        "feedback" => RetentionKind::Feedback,
        "project" | "memory" => RetentionKind::Project,
        "reference" | "ref" => RetentionKind::Reference,
        "skill" => RetentionKind::Skill,
        "artifact" => RetentionKind::Artifact,
        _ => RetentionKind::Other,
    }
}

fn searchable_message_text(message: &CoreMessage) -> String {
    let mut values = Vec::new();
    match &message.content {
        crate::types::message::Content::Text(text) => values.push(text.clone()),
        crate::types::message::Content::Parts(parts) => {
            for part in parts {
                match part {
                    crate::types::message::ContentPart::Text { text } => values.push(text.clone()),
                    crate::types::message::ContentPart::ToolResult { output, .. } => {
                        values.push(output.clone())
                    }
                    crate::types::message::ContentPart::Image {
                        source: crate::types::durable_content::DurableSource::Url { url },
                        ..
                    } => values.push(url.clone()),
                    _ => {}
                }
            }
        }
    }
    for call in &message.tool_calls {
        values.push(call.name.to_string());
        values.push(call.arguments.to_string());
    }
    values.join(" ")
}

fn lexical_terms(text: &str) -> std::collections::BTreeSet<String> {
    let mut terms = std::collections::BTreeSet::new();
    let mut segment = String::new();
    let flush = |segment: &mut String, terms: &mut std::collections::BTreeSet<String>| {
        if segment.is_empty() {
            return;
        }
        let folded = segment.to_lowercase();
        terms.insert(folded.clone());
        let chars = folded.chars().collect::<Vec<_>>();
        if chars.iter().any(|character| is_han(*character)) {
            for pair in chars.windows(2) {
                terms.insert(pair.iter().collect());
            }
        }
        segment.clear();
    };
    for character in text.chars() {
        if character.is_alphanumeric() {
            segment.push(character);
        } else {
            flush(&mut segment, &mut terms);
        }
    }
    flush(&mut segment, &mut terms);
    terms
}

fn is_han(character: char) -> bool {
    matches!(character as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
}

/// Four-slot context model aligned with LLM API slots (five fields — slot 3 spans
/// `task_state` + `signals`):
///
///   Slot 1 — Identity  (system):    who the agent is; role, rules, constraints.
///                                    Maps to: Anthropic system[0] cache_control, OpenAI system role.
///                                    Never changes within a run.
///
///   Slot 2 — Knowledge (knowledge): what the agent knows; memory retrievals, skill
///                                    definitions, artifacts. Low-frequency changes.
///                                    Maps to: Anthropic system[1] cache_control.
///
///   Slot 3 — State     (task_state + signals): what the agent is doing right now.
///                                    task_state = goal/plan/progress (structured).
///                                    signals = runtime events (rollback notes, interrupts).
///                                    Maps to: messages[0] user turn, rebuilt every call.
///
///   Slot 4 — History   (history):   what the agent has done; conversation turns,
///                                    tool calls and results. Compression pipeline target.
///                                    Maps to: messages[1..N].
pub struct ContextPartitions {
    pub system: Partition,
    pub knowledge: KnowledgePartition,
    pub task_state: TaskState,
    /// Runtime signals injected into the current turn (rollback notes, interrupts).
    /// Rendering is read-only. The prefix delivered to a provider request is consumed only when
    /// that request's correlated `ProviderResult` commits; later arrivals remain for the next turn.
    pub signals: Vec<String>,
    pub history: Partition,
}

impl ContextPartitions {
    pub fn new(_config: &ContextConfig) -> Self {
        Self {
            system: Partition::new(),
            knowledge: KnowledgePartition::new(),
            task_state: TaskState::default(),
            signals: Vec::new(),
            history: Partition::new(),
        }
    }

    /// Total token count across all slots.
    /// task_state tokens are measured from its rendered compact form.
    pub fn total_tokens(&self, engine: &ContextTokenEngine) -> u32 {
        // An empty task_state renders to nothing and must cost zero tokens; the engine's
        // per-message floor (`.max(1)`) would otherwise charge a phantom token for the empty
        // string and inflate the fixed-context deduction the utility selector budgets against.
        let task_state = self.task_state.format_compact();
        let task_state_tokens = if task_state.is_empty() {
            0
        } else {
            engine.count(&task_state)
        };
        self.system.token_count
            + self.knowledge.token_count
            + task_state_tokens
            + self.history.token_count
    }
}

impl Default for ContextPartitions {
    fn default() -> Self {
        Self::new(&ContextConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::token_engine::ContextTokenEngine;
    use crate::types::message::CoreMessage;

    fn engine() -> ContextTokenEngine {
        ContextTokenEngine::char_approx()
    }

    #[test]
    fn stale_measurement_does_not_count_a_replaced_message() {
        let mut partition = Partition::new();
        partition.push(CoreMessage::user("old"), 1);
        partition.messages[0] = CoreMessage::user("new evidence ".repeat(100));
        let expected = engine().count_message(&partition.messages[0]);
        assert!(expected > 1);
        assert_eq!(partition.measured_tokens(0, &engine()), expected);
    }

    #[test]
    fn push_updates_token_count() {
        let mut ctx = ContextPartitions::new(&ContextConfig::default());
        let base = ctx.total_tokens(&engine());
        ctx.system.push(CoreMessage::system("rules"), 10);
        ctx.history.push(CoreMessage::user("hello"), 5);
        assert_eq!(ctx.total_tokens(&engine()), base + 15);
    }

    #[test]
    fn task_state_tokens_included_in_total() {
        use crate::context::task_state::TaskState;
        let mut ctx = ContextPartitions::new(&ContextConfig::default());
        let before = ctx.total_tokens(&engine());
        ctx.task_state = TaskState {
            goal: "do something important".to_string(),
            ..Default::default()
        };
        let after = ctx.total_tokens(&engine());
        assert!(
            after > before,
            "task_state should contribute to total_tokens"
        );
    }

    #[test]
    fn knowledge_tokens_included_in_total() {
        let mut ctx = ContextPartitions::new(&ContextConfig::default());
        let before = ctx.total_tokens(&engine());
        ctx.knowledge.push(CoreMessage::system("skill: debug"), 20);
        assert_eq!(ctx.total_tokens(&engine()), before + 20);
    }

    // ── K1: keyed knowledge entries ──────────────────────────────────────────

    fn text_of(p: &KnowledgePartition) -> Vec<String> {
        p.messages()
            .filter_map(|m| m.content.as_text().map(str::to_string))
            .collect()
    }

    #[test]
    fn keyed_upsert_defers_to_boundary() {
        let mut p = KnowledgePartition::new();
        p.push_entry(
            Some("ref".into()),
            CoreMessage::system("original"),
            10,
            false,
        );
        p.push_entry(
            Some("ref".into()),
            CoreMessage::system("updated"),
            12,
            false,
        );
        // Mid-generation: still ONE entry rendering the ORIGINAL bytes (system[1] untouched).
        assert_eq!(p.len(), 1);
        assert_eq!(text_of(&p), vec!["original"]);
        assert_eq!(p.token_count, 10);

        let sweep = p.sweep_at_boundary();
        assert!(sweep.changed);
        assert!(
            sweep.removed_keys.is_empty(),
            "upsert-only sweep removes nothing"
        );
        assert_eq!(text_of(&p), vec!["updated"]);
        assert_eq!(p.token_count, 12);
    }

    #[test]
    fn remove_marks_then_sweep_drops() {
        let mut p = KnowledgePartition::new();
        p.push_entry(
            Some("ref".into()),
            CoreMessage::system("pinned ref"),
            8,
            false,
        );
        assert!(p.remove("ref"));
        // Still rendered until the boundary (no mid-generation byte rewrite).
        assert_eq!(p.len(), 1);
        assert_eq!(text_of(&p), vec!["pinned ref"]);

        let sweep = p.sweep_at_boundary();
        assert!(sweep.changed);
        assert_eq!(sweep.removed_keys, vec!["ref".to_string()]);
        assert_eq!(sweep.tokens_freed, 8);
        assert!(p.is_empty());
        assert_eq!(p.token_count, 0);
    }

    #[test]
    fn remove_unknown_key_errs_open() {
        let mut p = KnowledgePartition::new();
        p.push(CoreMessage::system("unkeyed"), 5);
        assert!(!p.remove("missing"));
        assert!(!p.sweep_at_boundary().changed);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn same_key_push_after_remove_revives_entry() {
        let mut p = KnowledgePartition::new();
        p.push_entry(Some("ref".into()), CoreMessage::system("older"), 5, false);
        p.remove("ref");
        // Re-pushing the key means the entry is wanted again — the eviction mark clears and the
        // fresh content lands as a deferred upsert.
        p.push_entry(Some("ref".into()), CoreMessage::system("newer"), 6, false);
        let sweep = p.sweep_at_boundary();
        assert!(sweep.removed_keys.is_empty());
        assert_eq!(text_of(&p), vec!["newer"]);
    }

    #[test]
    fn fresh_keys_and_unkeyed_append_immediately() {
        let mut p = KnowledgePartition::new();
        p.push(CoreMessage::system("existing"), 3);
        p.push_entry(Some("a".into()), CoreMessage::system("fresh"), 4, true);
        // Appends land in the partition right away; the manager stages one that arrives after
        // the prefix was sent, so the cached history stays valid (plan task 1.2).
        assert_eq!(text_of(&p), vec!["existing", "fresh"]);
        assert_eq!(p.token_count, 7);
        assert!(p.entries[1].pinned);
    }

    #[test]
    fn committed_message_updates_reference_usage_without_a_clock() {
        let mut p = KnowledgePartition::new();
        p.push_entry(
            Some("project:orchid".into()),
            CoreMessage::system("Atlas storage engine for ORCHID"),
            8,
            false,
        );
        p.push_entry(
            Some("reference:unrelated".into()),
            CoreMessage::system("Mercury deployment guide"),
            8,
            false,
        );

        p.observe_references(&CoreMessage::assistant("Use project:orchid and Atlas"), 7);

        assert_eq!(p.entries[0].value.ref_count, 1);
        assert_eq!(p.entries[0].value.last_referenced_step, Some(7));
        assert_eq!(p.entries[1].value.ref_count, 0);
        assert!(p.retention_score(0, 9) > p.retention_score(1, 9));
    }
}
