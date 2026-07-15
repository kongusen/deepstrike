//! Deterministic value-aware selection over indivisible context units.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ops::Range;

use super::token_engine::ContextTokenEngine;
use super::units::unit_boundaries;
use crate::types::message::{Content, ContentPart, Message};

pub struct UtilitySelectionContext<'a> {
    pub goal: &'a str,
    pub criteria: &'a [String],
    pub preserved_refs: &'a [String],
    pub active_directives: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtilityUnitScore {
    pub range: Range<usize>,
    pub tokens: u32,
    pub mandatory: bool,
    pub goal_overlap: u32,
    pub has_unresolved: bool,
    pub referenced_later: bool,
    pub is_error_or_decision: bool,
    pub recency: u32,
    pub token_cost: u32,
    pub prefix_invalidation_cost: u32,
    pub utility: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UtilityArchivePlan {
    pub archived_ranges: Vec<Range<usize>>,
    pub retained_ranges: Vec<Range<usize>>,
    pub archived_tokens: u32,
    pub retained_tokens: u32,
    pub scores: Vec<UtilityUnitScore>,
}

/// Select complete units to retain under `target_tokens`.
///
/// Mandatory dependencies are retained even when they alone exceed the target;
/// callers can then escalate pressure honestly instead of silently deleting the
/// evidence required to continue the task.
pub fn plan_utility_archive(
    messages: &[Message],
    total_tokens: u32,
    target_tokens: u32,
    preserve_recent_units: usize,
    engine: &ContextTokenEngine,
    context: &UtilitySelectionContext<'_>,
) -> UtilityArchivePlan {
    let ranges = unit_boundaries(messages);
    if ranges.is_empty() {
        return UtilityArchivePlan::default();
    }
    let unit_texts = ranges
        .iter()
        .map(|range| unit_text(&messages[range.clone()]))
        .collect::<Vec<_>>();
    let goal_terms = terms(
        std::iter::once(context.goal)
            .chain(context.criteria.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
            .as_str(),
    );
    let recent_start = ranges.len().saturating_sub(preserve_recent_units);
    let denominator = total_tokens.max(1);
    let unit_count = ranges.len().max(1) as u32;
    let mut scores = Vec::with_capacity(ranges.len());

    for (index, range) in ranges.iter().enumerate() {
        let slice = &messages[range.clone()];
        let text = &unit_texts[index];
        let tokens = slice
            .iter()
            .map(|message| {
                message
                    .token_count
                    .unwrap_or_else(|| engine.count_message(message))
            })
            .sum::<u32>();
        let goal_overlap = overlap_count(&terms(text), &goal_terms);
        let has_unresolved = has_unresolved(slice, text);
        let referenced_later = unit_referenced_later(slice, text, &unit_texts[index + 1..]);
        let is_error_or_decision = is_error_or_decision(slice, text);
        let dependency = context
            .preserved_refs
            .iter()
            .any(|reference| contains_folded(text, reference))
            || context
                .active_directives
                .iter()
                .any(|directive| directive_dependency(text, directive));
        let mandatory = index >= recent_start || has_unresolved || dependency;
        let recency = ((index as u64 + 1) * 1_000 / u64::from(unit_count)) as u32;
        let token_cost = (u64::from(tokens) * 1_000 / u64::from(denominator)) as u32;
        let prefix_invalidation_cost =
            ((ranges.len() - index) as u64 * 1_000 / u64::from(unit_count)) as u32;
        let utility = i64::from(goal_overlap) * 4_000
            + if has_unresolved { 20_000 } else { 0 }
            + if referenced_later { 5_000 } else { 0 }
            + if is_error_or_decision { 6_000 } else { 0 }
            + i64::from(recency) * 2
            - i64::from(token_cost) * 2
            - i64::from(prefix_invalidation_cost);
        scores.push(UtilityUnitScore {
            range: range.clone(),
            tokens,
            mandatory,
            goal_overlap,
            has_unresolved,
            referenced_later,
            is_error_or_decision,
            recency,
            token_cost,
            prefix_invalidation_cost,
            utility,
        });
    }

    if total_tokens <= target_tokens {
        return UtilityArchivePlan {
            archived_ranges: Vec::new(),
            retained_ranges: ranges,
            archived_tokens: 0,
            retained_tokens: scores.iter().map(|score| score.tokens).sum(),
            scores,
        };
    }

    let mut retained = scores
        .iter()
        .enumerate()
        .filter_map(|(index, score)| score.mandatory.then_some(index))
        .collect::<BTreeSet<_>>();
    let mut retained_tokens = retained
        .iter()
        .map(|index| scores[*index].tokens)
        .sum::<u32>();
    let mut optional = scores
        .iter()
        .enumerate()
        .filter_map(|(index, score)| (!score.mandatory).then_some(index))
        .collect::<Vec<_>>();
    optional.sort_by(|left, right| compare_density(&scores[*right], &scores[*left]));
    for index in optional {
        let tokens = scores[index].tokens;
        if retained_tokens.saturating_add(tokens) <= target_tokens {
            retained.insert(index);
            retained_tokens = retained_tokens.saturating_add(tokens);
        }
    }

    let retained_ranges = ranges
        .iter()
        .enumerate()
        .filter_map(|(index, range)| retained.contains(&index).then_some(range.clone()))
        .collect::<Vec<_>>();
    let archived_ranges = ranges
        .iter()
        .enumerate()
        .filter_map(|(index, range)| (!retained.contains(&index)).then_some(range.clone()))
        .collect::<Vec<_>>();
    let archived_tokens = scores
        .iter()
        .enumerate()
        .filter_map(|(index, score)| (!retained.contains(&index)).then_some(score.tokens))
        .sum();
    UtilityArchivePlan {
        archived_ranges,
        retained_ranges,
        archived_tokens,
        retained_tokens,
        scores,
    }
}

fn compare_density(left: &UtilityUnitScore, right: &UtilityUnitScore) -> Ordering {
    let left_density = i128::from(left.utility) * i128::from(right.tokens.max(1));
    let right_density = i128::from(right.utility) * i128::from(left.tokens.max(1));
    left_density
        .cmp(&right_density)
        .then_with(|| left.utility.cmp(&right.utility))
        .then_with(|| left.range.start.cmp(&right.range.start))
}

fn unit_text(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    for message in messages {
        match &message.content {
            Content::Text(text) => parts.push(text.clone()),
            Content::Parts(content_parts) => {
                for part in content_parts {
                    match part {
                        ContentPart::Text { text } => parts.push(text.clone()),
                        ContentPart::ToolResult {
                            call_id, output, ..
                        } => parts.push(format!("{call_id} {output}")),
                        ContentPart::Image { url, .. } => {
                            parts.push(url.clone().unwrap_or_default())
                        }
                        ContentPart::Audio { .. } => parts.push("audio".into()),
                    }
                }
            }
        }
        for call in &message.tool_calls {
            parts.push(format!("{} {} {}", call.id, call.name, call.arguments));
        }
    }
    parts.join("\n")
}

fn terms(text: &str) -> BTreeSet<String> {
    let mut output = BTreeSet::new();
    let mut ascii = String::new();
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '/' | '.' | ':') {
            ascii.push(character);
            continue;
        }
        if !ascii.is_empty() {
            if ascii.chars().count() > 1 {
                output.insert(std::mem::take(&mut ascii));
            } else {
                ascii.clear();
            }
        }
        if !character.is_whitespace() && !character.is_ascii_punctuation() {
            output.insert(character.to_string());
        }
    }
    if ascii.chars().count() > 1 {
        output.insert(ascii);
    }
    output
}

fn overlap_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> u32 {
    left.intersection(right).count() as u32
}

fn contains_folded(text: &str, pattern: &str) -> bool {
    !pattern.trim().is_empty() && text.to_lowercase().contains(&pattern.to_lowercase())
}

fn directive_dependency(text: &str, directive: &str) -> bool {
    if contains_folded(text, directive) {
        return true;
    }
    let directive_terms = terms(directive);
    if directive_terms.is_empty() {
        return false;
    }
    let threshold = directive_terms.len().min(2);
    terms(text).intersection(&directive_terms).count() >= threshold
}

fn has_unresolved(messages: &[Message], text: &str) -> bool {
    let mut opened = BTreeSet::new();
    let mut resolved = BTreeSet::new();
    for message in messages {
        for call in &message.tool_calls {
            opened.insert(call.id.to_string());
        }
        if let Content::Parts(parts) = &message.content {
            for part in parts {
                if let ContentPart::ToolResult {
                    call_id, is_error, ..
                } = part
                {
                    if *is_error {
                        return true;
                    }
                    resolved.insert(call_id.to_string());
                }
            }
        }
    }
    opened.iter().any(|call_id| !resolved.contains(call_id))
        || marker(
            text,
            &[
                "unresolved",
                "open question",
                "retry",
                "blocked",
                "待确认",
                "未解决",
                "重试",
                "阻塞",
            ],
        )
}

fn is_error_or_decision(messages: &[Message], text: &str) -> bool {
    messages.iter().any(|message| {
        matches!(&message.content, Content::Parts(parts) if parts.iter().any(|part| matches!(part, ContentPart::ToolResult { is_error: true, .. })))
    }) || marker(
        text,
        &[
            "error", "failed", "failure", "exception", "decision", "decided", "must", "should",
            "错误", "失败", "异常", "决定", "选择", "必须", "应当",
        ],
    )
}

fn marker(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| contains_folded(text, marker))
}

fn unit_referenced_later(messages: &[Message], text: &str, later: &[String]) -> bool {
    let mut references = messages
        .iter()
        .flat_map(|message| message.tool_calls.iter().map(|call| call.id.to_string()))
        .collect::<BTreeSet<_>>();
    references.extend(
        text.split_whitespace()
            .map(|token| token.trim_matches(|character: char| character.is_ascii_punctuation()))
            .filter(|token| token.contains('/') || token.contains("://"))
            .filter(|token| token.len() > 3)
            .map(str::to_string),
    );
    references.iter().any(|reference| {
        later
            .iter()
            .any(|later_text| contains_folded(later_text, reference))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{ContentPart, ToolCall};

    #[test]
    fn unresolved_tool_unit_is_mandatory() {
        let mut call = Message::assistant("working");
        call.tool_calls.push(ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "/work/a"}),
        });
        call.token_count = Some(20);
        let mut recent = Message::user("recent");
        recent.token_count = Some(20);
        let messages = vec![call, recent];
        let plan = plan_utility_archive(
            &messages,
            40,
            20,
            1,
            &ContextTokenEngine::char_approx(),
            &UtilitySelectionContext {
                goal: "",
                criteria: &[],
                preserved_refs: &[],
                active_directives: &[],
            },
        );
        assert!(plan.scores[0].mandatory);
        assert!(plan.scores[0].has_unresolved);
        assert_eq!(plan.retained_tokens, 40);
    }

    #[test]
    fn preserved_ref_keeps_complete_tool_unit() {
        let mut call = Message::assistant("read artifact");
        call.tool_calls.push(ToolCall {
            id: "call-keep".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        });
        call.token_count = Some(20);
        let mut result = Message::tool(vec![ContentPart::ToolResult {
            call_id: "call-keep".into(),
            output: "artifact".into(),
            is_error: false,
        }]);
        result.token_count = Some(20);
        let messages = vec![call, result];
        let plan = plan_utility_archive(
            &messages,
            40,
            0,
            0,
            &ContextTokenEngine::char_approx(),
            &UtilitySelectionContext {
                goal: "",
                criteria: &[],
                preserved_refs: &["call-keep".into()],
                active_directives: &[],
            },
        );
        assert!(plan.scores[0].mandatory);
        assert_eq!(plan.archived_ranges, Vec::<Range<usize>>::new());
    }
}
