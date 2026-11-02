//! Plan task 1.1 · the context item: one unit of model-visible context with an origin, a scope, a
//! fidelity, a lease and an authority.
//!
//! Every item gets its scope, fidelity, lease and authority from one table keyed by origin
//! ([`ItemOrigin::defaults`]); a host cannot set them per item. Knowledge-tier items (system[1] and
//! the staged state-turn copy) are held as [`ContextItem`]s today; history messages and handles
//! join the same table in task 1.1b.

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::types::message::{CoreMessage, Role};

/// Turns a recalled memory index line stays resident after it was recalled or last used.
pub const RECALL_LEASE_TURNS: u32 = 8;

/// An item's identity within one operation: allocated in arrival order, never reused, carried
/// through compaction, renewal and checkpoints. `0` marks an item the kernel did not admit itself
/// (a message restored from a checkpoint that predates item ids).
pub type ItemId = u64;

/// The identity a history message carries as an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemTag {
    pub id: ItemId,
    pub origin: ItemOrigin,
    #[serde(default)]
    pub injected_turn: u32,
}

impl ItemTag {
    /// The tag of a message with no recorded identity: id `0`, origin from its role.
    pub fn untracked(message: &CoreMessage) -> Self {
        Self { id: 0, origin: ItemOrigin::of_message(message), injected_turn: 0 }
    }
}

/// How many history items of one origin the run admitted, how many are still in history, and how
/// often the model read one back with `read_result` (plan task 4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginTally {
    pub origin: ItemOrigin,
    pub items: u32,
    pub tokens: u64,
    /// Filled in the end-of-run report only.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub resident_items: u32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub resident_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub read_back: u32,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// Where an item came from. It decides the item's defaults ([`Self::defaults`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOrigin {
    /// Written by the kernel itself (the governance note on denied tools).
    System,
    /// A skill body the host staged under `skill:<name>`.
    Skill,
    /// A memory index line the kernel recalled (`memory:<record_id>`).
    MemoryRecall,
    /// Reference material the host seeded or pinned.
    Reference,
    ToolResult,
    DependencyOutput,
    User,
    Assistant,
    TaskState,
    Signal,
    ProviderSummary,
}

impl ItemOrigin {
    /// The origin of a history message, from its role.
    pub fn of_message(message: &CoreMessage) -> Self {
        match message.role {
            Role::System => Self::System,
            Role::User => Self::User,
            Role::Assistant => Self::Assistant,
            Role::Tool => Self::ToolResult,
        }
    }

    /// The origin of knowledge a host pushes under `key`: a `skill:` key is a skill body, anything
    /// else is reference material.
    pub fn for_host_key(key: Option<&str>) -> Self {
        match key {
            Some(key) if key.starts_with("skill:") => Self::Skill,
            _ => Self::Reference,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Skill => "skill",
            Self::MemoryRecall => "memory_recall",
            Self::Reference => "reference",
            Self::ToolResult => "tool_result",
            Self::DependencyOutput => "dependency_output",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::TaskState => "task_state",
            Self::Signal => "signal",
            Self::ProviderSummary => "provider_summary",
        }
    }

    /// The origin table: every item of this origin starts with these.
    pub fn defaults(self) -> ItemDefaults {
        use Authority as A;
        use Fidelity as F;
        use ItemScope as S;
        use Lease as L;
        let (scope, fidelity, lease, authority) = match self {
            Self::System => (S::Operation, F::Full, L::Permanent, A::Kernel),
            // A skill leaves when it is deactivated or its activation lease runs out
            // (`ContextManager::deactivate_skill`); the item itself does not expire.
            Self::Skill => (S::Task, F::Full, L::Permanent, A::Model),
            Self::MemoryRecall => (S::Session, F::Handle, L::Turns(RECALL_LEASE_TURNS), A::Host),
            Self::Reference => (S::Operation, F::Full, L::Permanent, A::Host),
            Self::ToolResult => (S::Task, F::Full, L::Permanent, A::Host),
            Self::DependencyOutput => (S::Edge, F::Handle, L::Permanent, A::Kernel),
            Self::User => (S::Operation, F::Full, L::Permanent, A::Host),
            Self::Assistant => (S::Operation, F::Full, L::Permanent, A::Model),
            Self::TaskState => (S::Task, F::Structured, L::Permanent, A::Kernel),
            Self::Signal => (S::Operation, F::Full, L::SingleUse, A::Host),
            Self::ProviderSummary => (S::Operation, F::Full, L::Permanent, A::Provider),
        };
        ItemDefaults { scope, fidelity, lease, authority }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemScope {
    Operation,
    Task,
    Edge,
    Session,
    Agent,
}

/// How much of the source the item renders: all of it, a structured form, or a handle line the
/// model expands on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    Full,
    Structured,
    Handle,
}

/// How long an item stays. An expired item leaves at the next boundary, like any other removal
/// from system[1].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lease {
    Permanent,
    /// Leaves at the next compaction or renewal boundary.
    UntilBoundary,
    /// Leaves once this many turns pass without the item being recalled or referenced.
    Turns(u32),
    /// Leaves once it has been referenced.
    SingleUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Host,
    Kernel,
    Model,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemDefaults {
    pub scope: ItemScope,
    pub fidelity: Fidelity,
    pub lease: Lease,
    pub authority: Authority,
}

/// Why an item left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemEnd {
    /// The knowledge budget chose it.
    Evicted,
    /// Its lease ran out.
    Expired,
    /// Removed by name (a host mutation, a deactivated skill, a withdrawn note).
    Removed,
}

/// One knowledge-tier item.
#[derive(Debug, Clone)]
pub struct ContextItem {
    /// `None` appends an unkeyed item. Keyed items upsert.
    pub key: Option<CompactString>,
    pub origin: ItemOrigin,
    pub lease: Lease,
    pub message: CoreMessage,
    pub tokens: u32,
    /// Host-pinned ⇒ never budget-evicted and never expires. Skill items are not host-pinned.
    pub pinned: bool,
    /// Marked for removal at the next compaction/renewal boundary, for this reason. Knowledge
    /// renders into the cached system[1] block, so existing bytes are only rewritten where the
    /// prompt-cache prefix is being rebuilt anyway.
    pub leaving: Option<ItemEnd>,
    /// Deferred upsert: a same-key push mid-generation stages its replacement here instead of
    /// rewriting rendered bytes; applied at the next boundary.
    pub pending: Option<Box<(CoreMessage, u32)>>,
    /// Arrived after the current cache prefix was sent: rendered in the state turn instead of
    /// system[1] until the next boundary promotes it.
    pub staged: bool,
    pub value: ItemValue,
}

/// What the kernel has observed about an item's use. Driven by committed input only, never by a
/// wall clock, so it replays byte-for-byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemValue {
    /// Turn the item arrived (or was last renewed by a fresh recall or upsert).
    #[serde(default)]
    pub injected_turn: u32,
    /// Committed history messages that referenced it.
    #[serde(default)]
    pub ref_count: u64,
    /// Reference step (history position) of the last reference; the retention score's recency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_referenced_step: Option<u64>,
    /// Turn of the last reference; a turn lease counts from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_referenced_turn: Option<u32>,
    /// The model explicitly asked for the item's full form (`memory(record_id)` for a recall).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expanded: bool,
    /// Its key had been removed earlier in the run and came back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paged_in_after_evict: bool,
}

impl ContextItem {
    /// A new item of `origin`, with the origin's lease (permanent when pinned).
    pub fn new(
        origin: ItemOrigin,
        key: Option<CompactString>,
        message: CoreMessage,
        tokens: u32,
        pinned: bool,
        turn: u32,
    ) -> Self {
        let lease = if pinned { Lease::Permanent } else { origin.defaults().lease };
        Self {
            key,
            origin,
            lease,
            message,
            tokens,
            pinned,
            leaving: None,
            pending: None,
            staged: false,
            value: ItemValue { injected_turn: turn, ..ItemValue::default() },
        }
    }

    pub fn defaults(&self) -> ItemDefaults {
        self.origin.defaults()
    }

    /// Whether it is marked to leave at the next boundary.
    pub fn is_leaving(&self) -> bool {
        self.leaving.is_some()
    }

    /// Turns left on a turn lease, counted from the later of arrival and last reference. `None` for
    /// a lease that is not counted in turns.
    pub fn lease_remaining_turns(&self, current_turn: u32) -> Option<u32> {
        let Lease::Turns(turns) = self.lease else {
            return None;
        };
        let since = self
            .value
            .last_referenced_turn
            .map_or(self.value.injected_turn, |used| used.max(self.value.injected_turn));
        Some((since.saturating_add(turns)).saturating_sub(current_turn))
    }

    /// Whether the lease has run out at `current_turn`.
    pub fn lease_expired(&self, current_turn: u32) -> bool {
        match self.lease {
            Lease::Permanent | Lease::UntilBoundary => false,
            Lease::Turns(_) => self.lease_remaining_turns(current_turn) == Some(0),
            Lease::SingleUse => self.value.ref_count > 0,
        }
    }

    pub fn record(&self, end: Option<ItemEnd>) -> ItemRecord {
        ItemRecord {
            key: self.key.as_ref().map(ToString::to_string),
            origin: self.origin,
            tokens: self.tokens,
            value: self.value,
            end,
        }
    }
}

/// One item's lifecycle for the end-of-run report (plan task 4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub origin: ItemOrigin,
    pub tokens: u32,
    pub value: ItemValue,
    /// Why it left; `None` while still resident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<ItemEnd>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(origin: ItemOrigin, pinned: bool) -> ContextItem {
        ContextItem::new(origin, Some("k".into()), CoreMessage::system("x"), 1, pinned, 3)
    }

    #[test]
    fn the_origin_table_gives_a_recall_a_handle_with_a_turn_lease() {
        let recall = ItemOrigin::MemoryRecall.defaults();
        assert_eq!(recall.scope, ItemScope::Session);
        assert_eq!(recall.fidelity, Fidelity::Handle);
        assert_eq!(recall.lease, Lease::Turns(RECALL_LEASE_TURNS));
        assert_eq!(recall.authority, Authority::Host);
        assert_eq!(ItemOrigin::for_host_key(Some("skill:x")), ItemOrigin::Skill);
        assert_eq!(ItemOrigin::for_host_key(Some("memory:x")), ItemOrigin::Reference);
        assert_eq!(ItemOrigin::for_host_key(None), ItemOrigin::Reference);
    }

    #[test]
    fn a_turn_lease_counts_from_the_last_reference() {
        let mut recall = item(ItemOrigin::MemoryRecall, false);
        assert_eq!(recall.lease_remaining_turns(3), Some(RECALL_LEASE_TURNS));
        assert!(!recall.lease_expired(3 + RECALL_LEASE_TURNS - 1));
        assert!(recall.lease_expired(3 + RECALL_LEASE_TURNS));
        recall.value.last_referenced_turn = Some(6);
        assert!(!recall.lease_expired(3 + RECALL_LEASE_TURNS));
        assert!(recall.lease_expired(6 + RECALL_LEASE_TURNS));
    }

    #[test]
    fn a_pinned_item_never_expires() {
        let pinned = item(ItemOrigin::MemoryRecall, true);
        assert_eq!(pinned.lease, Lease::Permanent);
        assert!(!pinned.lease_expired(u32::MAX));
        assert_eq!(pinned.lease_remaining_turns(9), None);
    }

    #[test]
    fn a_single_use_item_expires_once_referenced() {
        let mut signal = item(ItemOrigin::Signal, false);
        assert!(!signal.lease_expired(100));
        signal.value.ref_count = 1;
        assert!(signal.lease_expired(3));
    }

    #[test]
    fn permanent_and_until_boundary_leases_do_not_expire_by_turn() {
        assert!(!item(ItemOrigin::Reference, false).lease_expired(u32::MAX));
        let mut boundary = item(ItemOrigin::Reference, false);
        boundary.lease = Lease::UntilBoundary;
        assert!(!boundary.lease_expired(u32::MAX));
    }
}
