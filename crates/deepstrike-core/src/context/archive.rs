//! Which history units a compaction archives: the oldest first.
//!
//! Kernel-distillation plan, stage 2 (DEC-13): the utility-scored selection that used to live here
//! (goal-term overlap, "unresolved" and "error/decision" markers, later references) was measured
//! against plain oldest-first archiving and gave no significant gain in any scenario; it is gone.
//! What remains is mechanics: the most recent `preserve_recent_units` units always stay, and of
//! the rest the newest that fit under the target stay with them. A unit is indivisible (a tool
//! call stays with its results), and nothing is chosen by reading content.

use std::ops::Range;

use super::measurement::TokenMeasurement;
use super::token_engine::ContextTokenEngine;
use super::units::unit_boundaries;
use crate::types::message::CoreMessage;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchivePlan {
    pub archived_ranges: Vec<Range<usize>>,
    pub retained_ranges: Vec<Range<usize>>,
    pub archived_tokens: u32,
    pub retained_tokens: u32,
}

/// Plan which units to archive so the retained history fits `target_tokens`. `measurements` is
/// aligned with `messages`; a missing entry is counted with `engine`. The recent units stay even
/// when they alone exceed the target, so the caller can escalate pressure instead of losing them.
pub fn plan_archive(
    messages: &[CoreMessage],
    measurements: &[TokenMeasurement],
    total_tokens: u32,
    target_tokens: u32,
    preserve_recent_units: usize,
    engine: &ContextTokenEngine,
) -> ArchivePlan {
    let ranges = unit_boundaries(messages);
    if ranges.is_empty() {
        return ArchivePlan::default();
    }
    let tokens: Vec<u32> = ranges
        .iter()
        .map(|range| {
            range
                .clone()
                .map(|index| {
                    measurements
                        .get(index)
                        .map(|measurement| measurement.tokens)
                        .unwrap_or_else(|| engine.count_message(&messages[index]))
                })
                .sum()
        })
        .collect();
    if total_tokens <= target_tokens {
        return ArchivePlan {
            retained_tokens: tokens.iter().sum(),
            retained_ranges: ranges,
            ..ArchivePlan::default()
        };
    }
    // The recent units, then the newest older units that still fit; the first that does not fit
    // and everything older is archived.
    let recent_start = ranges.len().saturating_sub(preserve_recent_units);
    let mut keep_from = recent_start;
    let mut retained_tokens: u32 = tokens[recent_start..].iter().sum();
    while keep_from > 0 {
        let next = retained_tokens.saturating_add(tokens[keep_from - 1]);
        if next > target_tokens {
            break;
        }
        retained_tokens = next;
        keep_from -= 1;
    }
    ArchivePlan {
        archived_ranges: ranges[..keep_from].to_vec(),
        retained_ranges: ranges[keep_from..].to_vec(),
        archived_tokens: tokens[..keep_from].iter().sum(),
        retained_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(n: usize) -> Vec<CoreMessage> {
        (0..n).map(|i| CoreMessage::user(format!("message {i} ...."))).collect()
    }

    #[test]
    fn the_oldest_units_are_archived_first() {
        let engine = ContextTokenEngine::char_approx();
        let messages = history(6);
        let per = engine.count_message(&messages[0]);
        let plan = plan_archive(&messages, &[], per * 6, per * 3, 1, &engine);
        assert_eq!(plan.retained_ranges, vec![3..4, 4..5, 5..6]);
        assert_eq!(plan.archived_ranges, vec![0..1, 1..2, 2..3]);
        assert_eq!((plan.retained_tokens, plan.archived_tokens), (per * 3, per * 3));
    }

    #[test]
    fn recent_units_stay_even_over_the_target() {
        let engine = ContextTokenEngine::char_approx();
        let messages = history(4);
        let per = engine.count_message(&messages[0]);
        let plan = plan_archive(&messages, &[], per * 4, per, 2, &engine);
        assert_eq!(plan.retained_ranges, vec![2..3, 3..4]);
        assert_eq!(plan.retained_tokens, per * 2);
    }

    #[test]
    fn nothing_is_archived_under_the_target() {
        let engine = ContextTokenEngine::char_approx();
        let messages = history(3);
        let plan = plan_archive(&messages, &[], 10, 1_000, 1, &engine);
        assert!(plan.archived_ranges.is_empty());
        assert_eq!(plan.retained_ranges.len(), 3);
    }
}
