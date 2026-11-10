//! Plan task 8.0 · the cache ledger: where each provider request broke the previous request's
//! cacheable prefix, how many tokens that cost, and why.
//!
//! A provider caches a request as one prefix: tool schemas, then system[0], system[1], then the
//! history turns. The volatile state turn is the uncached tail and is left out. Every time a
//! request goes out the kernel fingerprints that prefix segment by segment and compares it with the
//! previous request. The first segment that differs is a **prefix break**: every token the previous
//! request held after it has to be sent uncached again. The ledger records the break (turn, cause,
//! tokens kept before it, tokens lost after it) and sums the losses into the lifecycle report's
//! `cache_loss_tokens`.
//!
//! The cause comes from the change that made the break. Compaction, renewal, provider rewrites,
//! tool-result collapse and knowledge promotion announce themselves ([`CacheLedger::note_cause`]);
//! an unannounced break is classified from the render itself: a dropped item is a budget drop, an
//! item re-rendered as the narration stub is narration collapse, and a different toolset is a
//! toolset change.
//!
//! The state turn after the history is rebuilt every request, so the previous request's state turn
//! is never reusable: that is not a break, but a provider's count of uncached input includes it.
//! The ledger sums it separately as `tail_tokens`, so measured cache loss can be compared with the
//! kernel's.
//!
//! Everything here is derived from the rendered request, so a replay rebuilds the same ledger; it
//! travels in the checkpoint so a restored run keeps comparing against the request it last sent.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::context::fault::stable_hash;
use crate::context::item::{HISTORY_ITEM_DETAIL_CAP, ItemId};

/// Prefix breaks the ledger lists one by one; older ones are only counted.
pub const PREFIX_BREAK_DETAIL_CAP: usize = HISTORY_ITEM_DETAIL_CAP;

/// Why a request's cacheable prefix differs from the previous request's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakCause {
    /// The tool schemas changed (a skill narrowed or widened the toolset, a meta tool appeared).
    Toolset,
    /// system[0] changed.
    System,
    /// system[1] changed outside a compaction: staged knowledge promoted, or a boundary sweep.
    Promotion,
    /// Older tool results collapsed to previews under pressure.
    Collapse,
    /// An item re-rendered in place without an announced cause (a page-in, a page-out).
    Reprojection,
    /// Old assistant narration left the protected window and rendered as the narration stub.
    Narration,
    /// The renderer dropped the oldest history over the input budget.
    BudgetDrop,
    Snip,
    Micro,
    Auto,
    Renewal,
    /// The provider compacted or cleared context server-side.
    Provider,
    /// History rolled back, or anything else.
    Other,
}

impl BreakCause {
    /// The cause a compaction tier announces.
    pub fn of_compaction(action: crate::context::pressure::PressureAction) -> Self {
        use crate::context::pressure::PressureAction;
        match action {
            PressureAction::SnipCompact => Self::Snip,
            PressureAction::MicroCompact => Self::Micro,
            PressureAction::ContextCollapse => Self::Collapse,
            PressureAction::AutoCompact => Self::Auto,
            PressureAction::None => Self::Other,
        }
    }
}

/// One prefix break, attributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefixBreak {
    /// The turn of the request that broke the prefix.
    pub turn: u32,
    pub cause: BreakCause,
    /// Tokens of the previous request's prefix still byte-identical (still cacheable).
    pub kept_tokens: u32,
    /// Tokens of the previous request's prefix after the break: sent uncached again.
    pub lost_tokens: u32,
}

/// The fingerprint and size of one prefix segment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// FNV-1a 64 of the segment's wire material, as 16 hex digits (a string, so a JSON host that
    /// reads numbers as doubles cannot round it).
    pub hash: String,
    pub tokens: u32,
}

impl Segment {
    pub fn of(material: &[u8], tokens: u32) -> Self {
        Self {
            hash: format!("{:016x}", stable_hash(material)),
            tokens,
        }
    }
}

/// One history turn of a sent request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentTurn {
    #[serde(flatten)]
    pub segment: Segment,
    /// The history item the turn rendered; 0 for a synthetic turn.
    #[serde(default)]
    pub item: ItemId,
    /// The turn rendered as the narration stub.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stub: bool,
}

/// The cacheable prefix of one sent request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentPrefix {
    pub tools: Segment,
    pub system: Segment,
    pub knowledge: Segment,
    #[serde(default)]
    pub turns: Vec<SentTurn>,
    /// The uncached state turn after the turns.
    #[serde(default, skip_serializing_if = "crate::context::item::is_zero_u32")]
    pub tail: u32,
}

impl SentPrefix {
    fn header_tokens(&self) -> u32 {
        self.tools
            .tokens
            .saturating_add(self.system.tokens)
            .saturating_add(self.knowledge.tokens)
    }

    fn total_tokens(&self) -> u32 {
        self.turns
            .iter()
            .fold(self.header_tokens(), |sum, turn| sum.saturating_add(turn.segment.tokens))
    }

    /// The tokens a turn with this hash had in this request, to avoid recounting it.
    pub fn tokens_of(&self, hash: &str) -> Option<u32> {
        self.turns
            .iter()
            .find(|turn| turn.segment.hash == hash)
            .map(|turn| turn.segment.tokens)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheLedger {
    /// The prefix of the last request sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent: Option<SentPrefix>,
    /// The announced cause of a rewrite since the last request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<BreakCause>,
    /// The latest breaks, oldest first.
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    pub breaks: VecDeque<PrefixBreak>,
    #[serde(default, skip_serializing_if = "crate::context::item::is_zero_u32")]
    pub breaks_omitted: u32,
    /// Lost tokens summed over every break of the run.
    #[serde(default, skip_serializing_if = "crate::context::item::is_zero_u64")]
    pub lost_tokens: u64,
    /// The state-turn tokens of every request a later request followed: never reusable, so not a
    /// break, but part of what a provider counts as uncached.
    #[serde(default, skip_serializing_if = "crate::context::item::is_zero_u64")]
    pub tail_tokens: u64,
    /// Plan task 8.3 · `read_result` has been offered to the model; it stays in the toolset from
    /// then on, so the toolset (the head of the prefix) does not change again when residency does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_result_offered: bool,
}

impl CacheLedger {
    /// A rewrite with `cause` happened; the next request's break, if it has one, is attributed to
    /// it. The first cause announced since the last request is kept: later rewrites in the same
    /// interval (a boundary sweep a compaction triggered) follow from it.
    pub fn note_cause(&mut self, cause: BreakCause) {
        self.pending.get_or_insert(cause);
    }

    /// The request with prefix `next` is being sent on `turn`. Compares it with the last request,
    /// records the break if there is one, and returns it.
    pub fn observe(&mut self, turn: u32, next: SentPrefix) -> Option<PrefixBreak> {
        let pending = self.pending.take();
        self.tail_tokens += self.sent.as_ref().map_or(0, |prev| u64::from(prev.tail));
        let found = self.sent.as_ref().and_then(|prev| find_break(prev, &next, pending));
        self.sent = Some(next);
        let found = found.map(|(cause, kept_tokens, lost_tokens)| PrefixBreak {
            turn,
            cause,
            kept_tokens,
            lost_tokens,
        })?;
        self.lost_tokens += u64::from(found.lost_tokens);
        self.breaks.push_back(found);
        if self.breaks.len() > PREFIX_BREAK_DETAIL_CAP {
            self.breaks.pop_front();
            self.breaks_omitted += 1;
        }
        Some(found)
    }
}

/// Where `next` stops reusing `prev`, why, and the (kept, lost) tokens of `prev`'s prefix.
fn find_break(
    prev: &SentPrefix,
    next: &SentPrefix,
    pending: Option<BreakCause>,
) -> Option<(BreakCause, u32, u32)> {
    let total = prev.total_tokens();
    let lost = |kept: u32| total.saturating_sub(kept);
    if prev.tools.hash != next.tools.hash {
        return Some((BreakCause::Toolset, 0, total)).filter(|_| total > 0);
    }
    if prev.system.hash != next.system.hash {
        let kept = prev.tools.tokens;
        return Some((pending.unwrap_or(BreakCause::System), kept, lost(kept)));
    }
    if prev.knowledge.hash != next.knowledge.hash {
        let kept = prev.tools.tokens.saturating_add(prev.system.tokens);
        return Some((pending.unwrap_or(BreakCause::Promotion), kept, lost(kept)));
    }
    let same = prev
        .turns
        .iter()
        .zip(&next.turns)
        .take_while(|(a, b)| a.segment.hash == b.segment.hash)
        .count();
    if same == prev.turns.len() {
        return None;
    }
    let kept = prev.turns[..same]
        .iter()
        .fold(prev.header_tokens(), |sum, turn| sum.saturating_add(turn.segment.tokens));
    let lost = lost(kept);
    if lost == 0 {
        return None;
    }
    let cause = pending.unwrap_or_else(|| classify(&prev.turns[same], next, same));
    Some((cause, kept, lost))
}

/// An unannounced turn break: what happened to the first turn that no longer matches.
fn classify(prev: &SentTurn, next: &SentPrefix, at: usize) -> BreakCause {
    if prev.item != 0 && !next.turns.iter().any(|turn| turn.item == prev.item) {
        return BreakCause::BudgetDrop;
    }
    match next.turns.get(at) {
        Some(turn) if turn.item == prev.item && turn.stub && !prev.stub => BreakCause::Narration,
        Some(turn) if turn.item == prev.item && turn.item != 0 => BreakCause::Reprojection,
        _ => BreakCause::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(name: &str, tokens: u32) -> Segment {
        Segment::of(name.as_bytes(), tokens)
    }

    fn turn(name: &str, item: ItemId, tokens: u32) -> SentTurn {
        SentTurn {
            segment: seg(name, tokens),
            item,
            stub: false,
        }
    }

    fn prefix(turns: Vec<SentTurn>) -> SentPrefix {
        SentPrefix {
            tools: seg("tools", 100),
            system: seg("system", 50),
            knowledge: seg("knowledge", 20),
            turns,
            tail: 7,
        }
    }

    #[test]
    fn an_appended_request_breaks_nothing() {
        let mut ledger = CacheLedger::default();
        assert_eq!(ledger.observe(1, prefix(vec![turn("a", 1, 10)])), None);
        let next = prefix(vec![turn("a", 1, 10), turn("b", 2, 30)]);
        assert_eq!(ledger.observe(2, next), None);
        assert_eq!(ledger.lost_tokens, 0);
        assert_eq!(ledger.tail_tokens, 7, "the first request's state turn was not reusable");
    }

    #[test]
    fn a_dropped_oldest_turn_is_a_budget_drop() {
        let mut ledger = CacheLedger::default();
        ledger.observe(1, prefix(vec![turn("a", 1, 10), turn("b", 2, 30)]));
        let found = ledger.observe(2, prefix(vec![turn("b", 2, 30), turn("c", 3, 5)]));
        assert_eq!(
            found,
            Some(PrefixBreak {
                turn: 2,
                cause: BreakCause::BudgetDrop,
                kept_tokens: 170,
                lost_tokens: 40,
            })
        );
        assert_eq!(ledger.lost_tokens, 40);
    }

    #[test]
    fn a_turn_rendered_as_the_stub_is_narration() {
        let mut ledger = CacheLedger::default();
        ledger.observe(1, prefix(vec![turn("a", 1, 10), turn("b", 2, 30)]));
        let mut stub = turn("stub", 1, 4);
        stub.stub = true;
        let found = ledger.observe(2, prefix(vec![stub, turn("b", 2, 30)])).unwrap();
        assert_eq!(found.cause, BreakCause::Narration);
        assert_eq!((found.kept_tokens, found.lost_tokens), (170, 40));
    }

    #[test]
    fn an_announced_cause_wins_and_is_spent_by_one_request() {
        let mut ledger = CacheLedger::default();
        ledger.observe(1, prefix(vec![turn("a", 1, 10)]));
        ledger.note_cause(BreakCause::Auto);
        ledger.note_cause(BreakCause::Promotion);
        let found = ledger.observe(2, prefix(vec![turn("summary", 5, 3)])).unwrap();
        assert_eq!(found.cause, BreakCause::Auto);
        // A compaction that broke nothing leaves no cause behind for a later break.
        ledger.note_cause(BreakCause::Snip);
        assert_eq!(ledger.observe(3, prefix(vec![turn("summary", 5, 3)])), None);
        assert_eq!(ledger.pending, None);
    }

    #[test]
    fn a_new_toolset_loses_the_whole_prefix() {
        let mut ledger = CacheLedger::default();
        ledger.observe(1, prefix(vec![turn("a", 1, 10)]));
        let mut next = prefix(vec![turn("a", 1, 10)]);
        next.tools = seg("narrowed", 60);
        let found = ledger.observe(2, next).unwrap();
        assert_eq!(found.cause, BreakCause::Toolset);
        assert_eq!((found.kept_tokens, found.lost_tokens), (0, 180));
    }

    #[test]
    fn changed_knowledge_is_a_promotion_unless_announced() {
        let mut ledger = CacheLedger::default();
        ledger.observe(1, prefix(vec![turn("a", 1, 10)]));
        let mut next = prefix(vec![turn("a", 1, 10)]);
        next.knowledge = seg("promoted", 40);
        let found = ledger.observe(2, next).unwrap();
        assert_eq!(found.cause, BreakCause::Promotion);
        assert_eq!((found.kept_tokens, found.lost_tokens), (150, 30));
    }

    #[test]
    fn the_detail_is_capped_but_the_total_is_not() {
        let mut ledger = CacheLedger::default();
        for round in 0..(PREFIX_BREAK_DETAIL_CAP as u32 + 3) {
            let name = format!("t{round}");
            ledger.observe(round, prefix(vec![turn(&name, u64::from(round) + 1, 10)]));
        }
        assert_eq!(ledger.breaks.len(), PREFIX_BREAK_DETAIL_CAP);
        assert_eq!(ledger.breaks_omitted, 2);
        assert_eq!(ledger.lost_tokens, 10 * (PREFIX_BREAK_DETAIL_CAP as u64 + 2));
    }

    #[test]
    fn the_ledger_round_trips_through_json() {
        let mut ledger = CacheLedger::default();
        ledger.observe(1, prefix(vec![turn("a", 1, 10)]));
        ledger.observe(2, prefix(vec![turn("b", 2, 10)]));
        ledger.note_cause(BreakCause::Renewal);
        let json = serde_json::to_string(&ledger).unwrap();
        assert_eq!(serde_json::from_str::<CacheLedger>(&json).unwrap(), ledger);
    }
}
